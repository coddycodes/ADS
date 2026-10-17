use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{Mutex, Notify, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{
    orchestration::{
        OrchestrationCommand, OrchestrationEngine, ProviderTurnDelivery, TurnDeliveryFailureReason,
        TurnDeliveryMode, TurnDeliveryState, TurnDeliveryTransition, engine::OptionalNullable,
    },
    production::provider_runtime::{
        ProviderDeliveryOutcome, ProviderReconciliationOutcome, ProviderRuntimeError,
        ProviderRuntimeSupervisor, STEER_TARGET_GONE_DETAIL, deliver_durable_orchestration_turn,
        delivery_detail, finalize_delivery_route_cwd, provider_display_name,
        provider_instance_label, reconcile_orchestration_turn,
    },
    worktree_catalog::WorkspaceAvailabilityRegistry,
};

use super::workspace_availability::{WorkspaceAdmissionController, WorkspaceAdmissionError};

const DELETED_THREAD_DETAIL: &str =
    "This thread was deleted before the message could be delivered.";
const MAX_CONCURRENT_THREADS: usize = 4;
const RETRY_BACKOFF_MIN: Duration = Duration::from_millis(50);
const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(1);
/// The longest wait between attempts at one delivery that keeps failing. A provider that cannot
/// start fails every attempt the same way, and each attempt costs a launch; capped at a second, it
/// retried every second for as long as the turn stayed pending.
const DELIVERY_RETRY_BACKOFF_MAX: Duration = Duration::from_secs(30);
const SHUTDOWN_TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const FORCE_CANCEL_TASK_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const RECONCILIATION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
type DeliveryRouteFuture =
    Pin<Box<dyn Future<Output = Result<(), ProviderRuntimeError>> + Send + 'static>>;
#[cfg(test)]
pub(super) type DeliveryRouter =
    Arc<dyn Fn(OrchestrationCommand) -> DeliveryRouteFuture + Send + Sync>;
type ProviderDeliveryRouteFuture =
    Pin<Box<dyn Future<Output = ProviderDeliveryOutcome> + Send + 'static>>;
type ProviderDeliveryRouter =
    Arc<dyn Fn(OrchestrationCommand, String) -> ProviderDeliveryRouteFuture + Send + Sync>;
type DeliveryReconciliationFuture =
    Pin<Box<dyn Future<Output = ProviderReconciliationOutcome> + Send + 'static>>;
type DeliveryReconciler =
    Arc<dyn Fn(ProviderTurnDelivery) -> DeliveryReconciliationFuture + Send + Sync>;

pub struct TurnDeliveryService {
    /// Stops new durable claims without cancelling already-admitted provider work.
    shutdown: CancellationToken,
    force_cancel: CancellationToken,
    wake: Arc<Notify>,
    worker: Mutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    retry_probe: Arc<DeliveryRetryProbe>,
}

#[cfg(test)]
#[derive(Default)]
struct DeliveryRetryProbe {
    scheduled: std::sync::atomic::AtomicUsize,
    changed: Notify,
}

#[cfg(test)]
impl DeliveryRetryProbe {
    fn record(&self) {
        self.scheduled
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_one();
    }

    async fn wait_for(&self, expected: usize) {
        loop {
            let changed = self.changed.notified();
            if self.scheduled.load(std::sync::atomic::Ordering::SeqCst) >= expected {
                return;
            }
            changed.await;
        }
    }
}

impl TurnDeliveryService {
    pub fn start(
        engine: OrchestrationEngine,
        provider: Arc<ProviderRuntimeSupervisor>,
        settings_root: PathBuf,
    ) -> Self {
        let route_engine = engine.clone();
        let route_provider = provider.clone();
        let route_settings_root = settings_root.clone();
        let router: ProviderDeliveryRouter = Arc::new(move |command, delivery_key| {
            let engine = route_engine.clone();
            let provider = route_provider.clone();
            let settings_root = route_settings_root.clone();
            Box::pin(async move {
                deliver_durable_orchestration_turn(
                    &provider,
                    &engine,
                    &settings_root,
                    command,
                    delivery_key,
                )
                .await
            })
        });
        let reconciliation_engine = engine.clone();
        let recovery_settings_root = settings_root.clone();
        let reconciler: DeliveryReconciler = Arc::new(move |row| {
            let engine = reconciliation_engine.clone();
            let provider = provider.clone();
            let settings_root = settings_root.clone();
            Box::pin(async move {
                reconcile_orchestration_turn(&provider, &engine, &settings_root, row).await
            })
        });
        Self::start_worker(
            engine,
            MAX_CONCURRENT_THREADS,
            MAX_CONCURRENT_THREADS,
            router,
            reconciler,
            Some(recovery_settings_root),
        )
    }

    pub(crate) fn start_with_availability(
        engine: OrchestrationEngine,
        provider: Arc<ProviderRuntimeSupervisor>,
        settings_root: PathBuf,
        availability: WorkspaceAvailabilityRegistry,
    ) -> Self {
        let route_engine = engine.clone();
        let route_provider = provider.clone();
        let route_settings_root = settings_root.clone();
        let admission =
            WorkspaceAdmissionController::new(availability.clone(), engine.repositories());
        let provider_router: ProviderDeliveryRouter = Arc::new(move |command, delivery_key| {
            let engine = route_engine.clone();
            let provider = route_provider.clone();
            let settings_root = route_settings_root.clone();
            Box::pin(async move {
                deliver_durable_orchestration_turn(
                    &provider,
                    &engine,
                    &settings_root,
                    command,
                    delivery_key,
                )
                .await
            })
        });
        let router = guard_delivery_router(admission, provider_router);
        let reconciliation_engine = engine.clone();
        let reconciliation_admission =
            WorkspaceAdmissionController::new(availability, engine.repositories());
        let recovery_settings_root = settings_root.clone();
        let provider_reconciler: DeliveryReconciler = Arc::new(move |row| {
            let engine = reconciliation_engine.clone();
            let provider = provider.clone();
            let settings_root = settings_root.clone();
            Box::pin(async move {
                reconcile_orchestration_turn(&provider, &engine, &settings_root, row).await
            })
        });
        let reconciler = guard_delivery_reconciler(reconciliation_admission, provider_reconciler);
        Self::start_worker(
            engine,
            MAX_CONCURRENT_THREADS,
            MAX_CONCURRENT_THREADS,
            router,
            reconciler,
            Some(recovery_settings_root),
        )
    }

    #[cfg(test)]
    pub(super) fn start_with_router(
        engine: OrchestrationEngine,
        max_concurrent_threads: usize,
        router: DeliveryRouter,
    ) -> Self {
        let router = legacy_delivery_router(router);
        Self::start_worker(
            engine,
            max_concurrent_threads,
            max_concurrent_threads,
            router,
            unavailable_reconciler(),
            None,
        )
    }

    #[cfg(test)]
    fn start_with_router_and_capacity(
        engine: OrchestrationEngine,
        max_concurrent_threads: usize,
        capacity: usize,
        router: DeliveryRouter,
    ) -> Self {
        Self::start_worker(
            engine,
            max_concurrent_threads,
            capacity,
            legacy_delivery_router(router),
            unavailable_reconciler(),
            None,
        )
    }

    #[cfg(test)]
    fn start_with_delivery_router(
        engine: OrchestrationEngine,
        max_concurrent_threads: usize,
        router: ProviderDeliveryRouter,
        reconciler: DeliveryReconciler,
    ) -> Self {
        Self::start_worker(
            engine,
            max_concurrent_threads,
            max_concurrent_threads,
            router,
            reconciler,
            None,
        )
    }

    /// `settings_root` is where recovery reads the instance labels its details name; without
    /// one they name the provider's driver.
    fn start_worker(
        engine: OrchestrationEngine,
        max_concurrent_threads: usize,
        capacity: usize,
        router: ProviderDeliveryRouter,
        reconciler: DeliveryReconciler,
        settings_root: Option<PathBuf>,
    ) -> Self {
        Self::start_worker_with_shutdown_grace(
            engine,
            max_concurrent_threads,
            capacity,
            router,
            reconciler,
            settings_root,
            SHUTDOWN_TASK_DRAIN_TIMEOUT,
        )
    }

    fn start_worker_with_shutdown_grace(
        engine: OrchestrationEngine,
        max_concurrent_threads: usize,
        capacity: usize,
        router: ProviderDeliveryRouter,
        reconciler: DeliveryReconciler,
        settings_root: Option<PathBuf>,
        shutdown_grace: Duration,
    ) -> Self {
        let shutdown = CancellationToken::new();
        let force_cancel = CancellationToken::new();
        let wake = Arc::new(Notify::new());
        engine.set_turn_delivery_waker(wake.clone());
        let permits = Arc::new(Semaphore::new(capacity));
        #[cfg(test)]
        let retry_probe = Arc::new(DeliveryRetryProbe::default());
        let worker = tokio::spawn(run(
            engine,
            router,
            reconciler,
            settings_root,
            max_concurrent_threads,
            permits,
            shutdown.clone(),
            force_cancel.clone(),
            shutdown_grace,
            wake.clone(),
            #[cfg(test)]
            retry_probe.clone(),
        ));
        Self {
            shutdown,
            force_cancel,
            wake,
            worker: Mutex::new(Some(worker)),
            #[cfg(test)]
            retry_probe,
        }
    }

    #[cfg(test)]
    async fn wait_for_retries_scheduled(&self, expected: usize) {
        self.retry_probe.wait_for(expected).await;
    }

    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.wake.notify_waiters();
        if let Some(worker) = self.worker.lock().await.take() {
            let _ = worker.await;
        }
        debug_assert!(
            self.force_cancel.is_cancelled() || self.worker.lock().await.is_none(),
            "delivery worker must finish or enter force cancellation"
        );
    }
}

pub(crate) fn workspace_admission_detail(error: WorkspaceAdmissionError) -> String {
    match error {
        WorkspaceAdmissionError::Unavailable(error) => error.message,
        WorkspaceAdmissionError::Identity(error) => error.message,
        WorkspaceAdmissionError::Resolution(error) => error,
    }
}

fn guard_delivery_router(
    admission: WorkspaceAdmissionController,
    router: ProviderDeliveryRouter,
) -> ProviderDeliveryRouter {
    Arc::new(move |command, delivery_key| {
        let admission = admission.clone();
        let router = router.clone();
        Box::pin(async move {
            let thread_id = match &command {
                OrchestrationCommand::ThreadTurnStart { thread_id, .. } => thread_id,
                _ => {
                    return ProviderDeliveryOutcome::Rejected {
                        detail: "durable provider delivery requires a turn start".to_owned(),
                    };
                }
            };
            let workspace_admission = match admission
                .acquire_thread(thread_id, std::iter::empty())
                .await
            {
                Ok(admission) => admission,
                Err(error) => {
                    return ProviderDeliveryOutcome::DefinitelyNotSent {
                        detail: workspace_admission_detail(error),
                    };
                }
            };
            let loss = workspace_admission.loss_cancellation();
            let route = router(command, delivery_key);
            tokio::pin!(route);
            // Once routing can run, it may already have written to the provider.
            // Cancelling its future cannot prove that the message was not sent;
            // preserve the uncertain-delivery confirmation before any retry.
            tokio::select! {
                biased;
                () = loss.cancelled() => ProviderDeliveryOutcome::Ambiguous {
                    detail: loss
                        .unavailable()
                        .map_or_else(|| "workspace became unavailable".to_owned(), |error| error.message),
                },
                outcome = &mut route => outcome,
            }
        })
    })
}

fn guard_delivery_reconciler(
    admission: WorkspaceAdmissionController,
    reconciler: DeliveryReconciler,
) -> DeliveryReconciler {
    Arc::new(move |row| {
        let admission = admission.clone();
        let reconciler = reconciler.clone();
        Box::pin(async move {
            let workspace_admission = match admission
                .acquire_thread(&row.thread_id, std::iter::empty())
                .await
            {
                Ok(admission) => admission,
                Err(error) => {
                    return ProviderReconciliationOutcome::Unavailable {
                        detail: workspace_admission_detail(error),
                    };
                }
            };
            let loss = workspace_admission.loss_cancellation();
            let reconcile = reconciler(row);
            tokio::pin!(reconcile);
            tokio::select! {
                biased;
                () = loss.cancelled() => ProviderReconciliationOutcome::Unavailable {
                    detail: loss
                        .unavailable()
                        .map_or_else(|| "workspace became unavailable".to_owned(), |error| error.message),
                },
                outcome = &mut reconcile => outcome,
            }
        })
    })
}

#[cfg(test)]
fn legacy_delivery_router(router: DeliveryRouter) -> ProviderDeliveryRouter {
    Arc::new(move |command, _delivery_key| {
        let future = router(command);
        Box::pin(async move {
            match future.await {
                Ok(()) => ProviderDeliveryOutcome::Accepted { turn_id: None },
                Err(error) => ProviderDeliveryOutcome::Rejected {
                    detail: error.to_string(),
                },
            }
        })
    })
}

#[cfg(test)]
fn unavailable_reconciler() -> DeliveryReconciler {
    Arc::new(|_| {
        Box::pin(async {
            ProviderReconciliationOutcome::Unavailable {
                detail: "provider reconciliation is not configured for this test".to_owned(),
            }
        })
    })
}

#[allow(clippy::too_many_arguments)]
async fn run(
    engine: OrchestrationEngine,
    router: ProviderDeliveryRouter,
    reconciler: DeliveryReconciler,
    settings_root: Option<PathBuf>,
    max_concurrent_threads: usize,
    permits: Arc<Semaphore>,
    stop_claiming: CancellationToken,
    force_cancel: CancellationToken,
    shutdown_grace: Duration,
    wake: Arc<Notify>,
    #[cfg(test)] retry_probe: Arc<DeliveryRetryProbe>,
) {
    let mut tasks = JoinSet::new();
    let mut recovery_task = None;
    let mut in_flight_commands = HashSet::new();
    let mut in_flight_threads = HashSet::new();
    let mut retry_backoffs = HashMap::new();
    let mut backoff = RETRY_BACKOFF_MIN;
    let mut reconciliation_backoff = RETRY_BACKOFF_MIN;
    let mut reconciliation_ready_at = None;
    let repositories = engine.repositories();
    let mut cohort_backoff = RETRY_BACKOFF_MIN;
    let mut recovery_cohort: HashSet<String> = loop {
        let result = tokio::select! {
            () = stop_claiming.cancelled() => return,
            rows = repositories.list_provider_turn_deliveries(vec![TurnDeliveryState::Sending]) => rows,
        };
        match result {
            Ok(rows) => break rows.into_iter().map(|row| row.command_id).collect(),
            Err(error) => {
                tracing::warn!(%error, "provider delivery recovery cohort could not be loaded; retrying");
                tokio::select! {
                    () = stop_claiming.cancelled() => return,
                    () = sleep(cohort_backoff) => {}
                }
                cohort_backoff = next_backoff(cohort_backoff);
            }
        }
    };
    if !recovery_cohort.is_empty() {
        reconciliation_ready_at = Some(Instant::now());
    }
    loop {
        if recovery_task.is_none()
            && !recovery_cohort.is_empty()
            && reconciliation_ready_at.is_some_and(|ready_at| ready_at <= Instant::now())
        {
            let engine = engine.clone();
            let reconciler = reconciler.clone();
            let settings_root = settings_root.clone();
            let mut cohort = recovery_cohort.clone();
            let in_flight_commands = in_flight_commands.clone();
            recovery_task = Some(tokio::spawn(async move {
                let result = recover_sending(
                    &engine,
                    &reconciler,
                    settings_root.as_deref(),
                    &mut cohort,
                    &in_flight_commands,
                )
                .await;
                RecoveryCompletion { cohort, result }
            }));
            reconciliation_ready_at = None;
        }
        let retry_delay = match fill_available_slots(
            &engine,
            &router,
            max_concurrent_threads,
            &mut tasks,
            &mut in_flight_commands,
            &mut in_flight_threads,
            &mut retry_backoffs,
            &stop_claiming,
            &force_cancel,
            &permits,
        )
        .await
        {
            Ok(result) => {
                backoff = RETRY_BACKOFF_MIN;
                min_delay(
                    result.retry_delay,
                    reconciliation_ready_at
                        .map(|ready_at| ready_at.saturating_duration_since(Instant::now())),
                )
            }
            Err(error) => {
                tracing::warn!(%error, "provider delivery state unavailable; retrying");
                let delay = backoff;
                backoff = next_backoff(backoff);
                Some(delay)
            }
        };

        match wait_for_dispatch_signal(
            &mut tasks,
            &mut recovery_task,
            &stop_claiming,
            &wake,
            retry_delay,
        )
        .await
        {
            DispatchWait::Continue => {}
            DispatchWait::Completion(completion) => {
                if let Some(completion) =
                    remove_completion(completion, &mut in_flight_commands, &mut in_flight_threads)
                {
                    let command_id = completion.command_id.clone();
                    match completion.result {
                        Ok(DeliveryTaskOutcome::Finished) => {
                            retry_backoffs.remove(&command_id);
                        }
                        Ok(DeliveryTaskOutcome::DefinitelyNotSent) => {
                            schedule_retry(&mut retry_backoffs, command_id);
                            #[cfg(test)]
                            retry_probe.record();
                        }
                        Err(error) => {
                            tracing::warn!(%error, "provider delivery transition failed");
                            schedule_retry(&mut retry_backoffs, command_id);
                            #[cfg(test)]
                            retry_probe.record();
                        }
                    }
                }
            }
            DispatchWait::Recovery(completion) => {
                recovery_task = None;
                match completion {
                    Ok(completion) => {
                        recovery_cohort = completion.cohort;
                        match completion.result {
                            Ok(has_unavailable) if has_unavailable => {
                                reconciliation_ready_at =
                                    Some(Instant::now() + reconciliation_backoff);
                                reconciliation_backoff = next_backoff(reconciliation_backoff);
                            }
                            Ok(_) => {
                                reconciliation_ready_at = None;
                                reconciliation_backoff = RETRY_BACKOFF_MIN;
                            }
                            Err(error) => {
                                tracing::warn!(%error, "provider delivery recovery failed; retrying");
                                reconciliation_ready_at =
                                    Some(Instant::now() + reconciliation_backoff);
                                reconciliation_backoff = next_backoff(reconciliation_backoff);
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "provider delivery recovery task failed; retrying");
                        reconciliation_ready_at = Some(Instant::now() + reconciliation_backoff);
                        reconciliation_backoff = next_backoff(reconciliation_backoff);
                    }
                }
            }
            DispatchWait::Shutdown => {
                if let Some(task) = recovery_task.take() {
                    task.abort();
                    let _ = task.await;
                }
                stop_tasks(
                    &mut tasks,
                    &force_cancel,
                    shutdown_grace,
                    FORCE_CANCEL_TASK_DRAIN_TIMEOUT,
                )
                .await;
                return;
            }
        }
    }
}

fn min_delay(left: Option<Duration>, right: Option<Duration>) -> Option<Duration> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(delay), None) | (None, Some(delay)) => Some(delay),
        (None, None) => None,
    }
}

fn schedule_retry(retry_backoffs: &mut HashMap<String, DeliveryRetry>, command_id: String) {
    let delay = retry_backoffs
        .get(&command_id)
        .map(|retry| retry.next_delay)
        .unwrap_or(RETRY_BACKOFF_MIN);
    retry_backoffs.insert(
        command_id,
        DeliveryRetry {
            ready_at: Instant::now() + delay,
            next_delay: next_delivery_backoff(delay),
        },
    );
}

enum DispatchWait {
    Continue,
    Completion(Option<Result<DeliveryCompletion, tokio::task::JoinError>>),
    Recovery(Result<RecoveryCompletion, tokio::task::JoinError>),
    Shutdown,
}

struct RecoveryCompletion {
    cohort: HashSet<String>,
    result: Result<bool, String>,
}

async fn wait_for_dispatch_signal(
    tasks: &mut JoinSet<DeliveryCompletion>,
    recovery_task: &mut Option<JoinHandle<RecoveryCompletion>>,
    shutdown: &CancellationToken,
    wake: &Notify,
    retry_delay: Option<Duration>,
) -> DispatchWait {
    let retry_wait = async {
        match retry_delay {
            Some(delay) => sleep(delay).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(retry_wait);
    let recovery_wait = async {
        match recovery_task.as_mut() {
            Some(task) => task.await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(recovery_wait);
    tokio::select! {
        () = shutdown.cancelled() => DispatchWait::Shutdown,
        () = wake.notified() => DispatchWait::Continue,
        completion = tasks.join_next(), if !tasks.is_empty() => DispatchWait::Completion(completion),
        completion = &mut recovery_wait => DispatchWait::Recovery(completion),
        () = &mut retry_wait => DispatchWait::Continue,
    }
}

struct DeliveryRetry {
    ready_at: Instant,
    next_delay: Duration,
}

struct FillResult {
    retry_delay: Option<Duration>,
}

async fn recover_sending(
    engine: &OrchestrationEngine,
    reconciler: &DeliveryReconciler,
    settings_root: Option<&Path>,
    recovery_cohort: &mut HashSet<String>,
    in_flight_commands: &HashSet<String>,
) -> Result<bool, String> {
    let rows = engine
        .repositories()
        .list_provider_turn_deliveries(vec![TurnDeliveryState::Sending])
        .await
        .map_err(|error| error.to_string())?;
    let sending_ids = rows
        .iter()
        .map(|row| row.command_id.clone())
        .collect::<HashSet<_>>();
    recovery_cohort.retain(|command_id| sending_ids.contains(command_id));
    let recovery_rows = rows
        .into_iter()
        .filter(|row| {
            recovery_cohort.contains(&row.command_id)
                && !in_flight_commands.contains(&row.command_id)
        })
        .collect::<Vec<_>>();
    let mut labels = HashMap::new();
    for row in recovery_rows {
        let outcome = match row.provider_kind.as_str() {
            "codex" | "opencode" => {
                match timeout(RECONCILIATION_ATTEMPT_TIMEOUT, reconciler(row.clone())).await {
                    Ok(outcome) => outcome,
                    Err(_) => ProviderReconciliationOutcome::Unavailable {
                        detail: "provider delivery reconciliation timed out".to_owned(),
                    },
                }
            }
            // This provider cannot look up a delivery, so the row becomes uncertain and its
            // detail is what the undelivered turn shows, naming the instance as the user does.
            provider => {
                if !labels.contains_key(&row.provider_instance_id) {
                    let label = match settings_root {
                        Some(settings_root) => {
                            provider_instance_label(
                                settings_root,
                                &row.provider_instance_id,
                                provider,
                            )
                            .await
                        }
                        None => provider_display_name(provider).to_owned(),
                    };
                    labels.insert(row.provider_instance_id.clone(), label);
                }
                let label = &labels[&row.provider_instance_id];
                ProviderReconciliationOutcome::Unavailable {
                    detail: format!(
                        "BiBCode can't check whether {label} received this message after a restart."
                    ),
                }
            }
        };
        let (next_state, detail) = match outcome {
            ProviderReconciliationOutcome::Found => (TurnDeliveryState::Delivered, None),
            ProviderReconciliationOutcome::Absent => (TurnDeliveryState::Pending, None),
            ProviderReconciliationOutcome::ConversationLost { detail } => {
                (TurnDeliveryState::Uncertain, Some(detail))
            }
            ProviderReconciliationOutcome::Unavailable { detail }
                if matches!(row.provider_kind.as_str(), "codex" | "opencode") =>
            {
                tracing::warn!(
                    command_id = %row.command_id,
                    provider = %row.provider_kind,
                    %detail,
                    "provider delivery reconciliation unavailable; preserving sending state"
                );
                continue;
            }
            ProviderReconciliationOutcome::Unavailable { detail } => {
                (TurnDeliveryState::Uncertain, Some(detail))
            }
        };
        let command_id = row.command_id.clone();
        let transitioned = engine
            .transition_turn_delivery(TurnDeliveryTransition {
                turn_id: None,
                command_id: row.command_id,
                expected_states: vec![TurnDeliveryState::Sending],
                expected_attempt: row.attempts,
                next_state,
                detail,
                updated_at: now(),
            })
            .await
            .map_err(|error| error.to_string())?;
        if transitioned {
            recovery_cohort.remove(&command_id);
        }
    }
    Ok(!recovery_cohort.is_empty())
}

#[allow(clippy::too_many_arguments)]
async fn fill_available_slots(
    engine: &OrchestrationEngine,
    router: &ProviderDeliveryRouter,
    max_concurrent_threads: usize,
    tasks: &mut JoinSet<DeliveryCompletion>,
    in_flight_commands: &mut HashSet<String>,
    in_flight_threads: &mut HashSet<String>,
    retry_backoffs: &mut HashMap<String, DeliveryRetry>,
    stop_claiming: &CancellationToken,
    force_cancel: &CancellationToken,
    permits: &Arc<Semaphore>,
) -> Result<FillResult, String> {
    if stop_claiming.is_cancelled() {
        return Ok(FillResult { retry_delay: None });
    }
    promote_settled_threads(engine, stop_claiming).await?;
    let available = max_concurrent_threads.saturating_sub(tasks.len());
    if available == 0 {
        return Ok(FillResult { retry_delay: None });
    }
    let rows = engine
        .repositories()
        .list_provider_turn_deliveries(vec![
            TurnDeliveryState::Pending,
            TurnDeliveryState::Sending,
            TurnDeliveryState::Uncertain,
            TurnDeliveryState::Failed,
        ])
        .await
        .map_err(|error| error.to_string())?;
    let rows = dismiss_deliveries_for_deleted_threads(engine, rows).await?;
    let active_commands = rows
        .iter()
        .map(|row| row.command_id.as_str())
        .collect::<HashSet<_>>();
    retry_backoffs.retain(|command_id, _| active_commands.contains(command_id.as_str()));
    let now = Instant::now();
    for row in rows
        .iter()
        .filter(|row| row.state == TurnDeliveryState::Pending && row.attempts > 0)
    {
        retry_backoffs
            .entry(row.command_id.clone())
            .or_insert_with(|| persisted_delivery_retry(row, now));
    }
    // There is only one candidate per thread, so each session is read once per fill.
    let mut candidates = Vec::new();
    for row in claimable_oldest_per_thread(rows) {
        if row.mode == TurnDeliveryMode::Start
            && engine
                .repositories()
                .get_thread_session(row.thread_id.clone())
                .await
                .map_err(|error| error.to_string())?
                .is_some_and(|session| matches!(session.status.as_str(), "running" | "starting"))
        {
            continue;
        }
        // A closed (archived) chat panel starts nothing until it is reopened; unarchive wakes
        // delivery. The claim repeats this check.
        if engine
            .repositories()
            .get_thread(row.thread_id.clone())
            .await
            .map_err(|error| error.to_string())?
            .is_some_and(|thread| thread.kind == "panel" && thread.archived_at.is_some())
        {
            continue;
        }
        candidates.push(row);
    }
    let retry_delay = candidates
        .iter()
        .filter(|row| {
            !in_flight_commands.contains(&row.command_id)
                && !in_flight_threads.contains(&row.thread_id)
        })
        .filter_map(|row| retry_backoffs.get(&row.command_id))
        .filter(|retry| retry.ready_at > now)
        .map(|retry| retry.ready_at.saturating_duration_since(now))
        .min();
    let selected = candidates
        .into_iter()
        .filter(|row| {
            !in_flight_commands.contains(&row.command_id)
                && !in_flight_threads.contains(&row.thread_id)
        })
        .filter(|row| {
            retry_backoffs
                .get(&row.command_id)
                .is_none_or(|retry| retry.ready_at <= now)
        })
        .take(available)
        .collect::<Vec<_>>();
    for row in selected {
        if stop_claiming.is_cancelled() {
            break;
        }
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            break;
        };
        if stop_claiming.is_cancelled() {
            break;
        }
        in_flight_commands.insert(row.command_id.clone());
        in_flight_threads.insert(row.thread_id.clone());
        let engine = engine.clone();
        let router = router.clone();
        let stop_claiming = stop_claiming.clone();
        let force_cancel = force_cancel.clone();
        if stop_claiming.is_cancelled() {
            in_flight_commands.remove(&row.command_id);
            in_flight_threads.remove(&row.thread_id);
            break;
        }
        tasks.spawn(async move {
            let _permit = permit;
            prepare_claim_and_deliver(engine, router, row, stop_claiming, force_cancel).await
        });
    }
    Ok(FillResult { retry_delay })
}

/// A delivery whose thread was deleted can never be delivered: the provider would launch a
/// session the orchestration engine then refuses to record. Settling it as `Dismissed` ends the
/// retry loop, including for rows stranded before this rule existed. Returns the rows that remain.
async fn dismiss_deliveries_for_deleted_threads(
    engine: &OrchestrationEngine,
    rows: Vec<ProviderTurnDelivery>,
) -> Result<Vec<ProviderTurnDelivery>, String> {
    let mut deleted = HashMap::new();
    let mut remaining = Vec::with_capacity(rows.len());
    for row in rows {
        if row.state != TurnDeliveryState::Pending {
            remaining.push(row);
            continue;
        }
        let is_deleted = match deleted.get(&row.thread_id) {
            Some(is_deleted) => *is_deleted,
            None => {
                let is_deleted = engine
                    .repositories()
                    .get_thread(row.thread_id.clone())
                    .await
                    .map_err(|error| error.to_string())?
                    .is_some_and(|thread| thread.deleted_at.is_some());
                deleted.insert(row.thread_id.clone(), is_deleted);
                is_deleted
            }
        };
        if !is_deleted {
            remaining.push(row);
            continue;
        }
        // `false` means the row changed state concurrently; the next fill re-reads it.
        engine
            .transition_turn_delivery(TurnDeliveryTransition {
                turn_id: None,
                command_id: row.command_id,
                expected_states: vec![TurnDeliveryState::Pending],
                expected_attempt: row.attempts,
                next_state: TurnDeliveryState::Dismissed,
                detail: Some(DELETED_THREAD_DETAIL.to_owned()),
                updated_at: now(),
            })
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(remaining)
}

async fn promote_settled_threads(
    engine: &OrchestrationEngine,
    shutdown: &CancellationToken,
) -> Result<(), String> {
    let repositories = engine.repositories();
    let heads = repositories
        .list_queued_provider_turn_heads()
        .await
        .map_err(|error| error.to_string())?;
    for head in heads {
        if shutdown.is_cancelled() {
            break;
        }
        if head.held
            || !repositories
                .can_promote_queued_provider_turn(
                    head.thread_id.clone(),
                    head.message_id.clone(),
                    true,
                )
                .await
                .map_err(|error| error.to_string())?
        {
            continue;
        }
        match engine
            .dispatch(OrchestrationCommand::ThreadTurnPromote {
                command_id: format!("server:turn-promote:{}", uuid::Uuid::new_v4()),
                thread_id: head.thread_id,
                message_id: head.message_id,
                created_at: now(),
            })
            .await
        {
            Ok(_) | Err(crate::orchestration::OrchestrationError::Invariant { .. }) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

struct DeliveryCompletion {
    command_id: String,
    thread_id: String,
    result: Result<DeliveryTaskOutcome, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryTaskOutcome {
    Finished,
    DefinitelyNotSent,
}

fn remove_completion(
    completion: Option<Result<DeliveryCompletion, tokio::task::JoinError>>,
    in_flight_commands: &mut HashSet<String>,
    in_flight_threads: &mut HashSet<String>,
) -> Option<DeliveryCompletion> {
    let completion = completion?;
    let Ok(completion) = completion else {
        tracing::warn!("provider delivery task failed; keeping its in-flight guard");
        return None;
    };
    in_flight_commands.remove(&completion.command_id);
    in_flight_threads.remove(&completion.thread_id);
    Some(completion)
}

async fn stop_tasks(
    tasks: &mut JoinSet<DeliveryCompletion>,
    force_cancel: &CancellationToken,
    graceful_timeout: Duration,
    force_timeout: Duration,
) {
    if timeout(graceful_timeout, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_ok()
    {
        return;
    }
    force_cancel.cancel();
    if timeout(force_timeout, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_ok()
    {
        return;
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(RETRY_BACKOFF_MAX)
}

fn next_delivery_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(DELIVERY_RETRY_BACKOFF_MAX)
}

fn persisted_delivery_retry(row: &ProviderTurnDelivery, now: Instant) -> DeliveryRetry {
    let delay = retry_delay_for_attempts(row.attempts);
    let elapsed = OffsetDateTime::parse(&row.updated_at, &Rfc3339)
        .ok()
        .and_then(|updated_at| {
            let elapsed = OffsetDateTime::now_utc() - updated_at;
            (!elapsed.is_negative())
                .then(|| std::time::Duration::try_from(elapsed).ok())
                .flatten()
        })
        .unwrap_or(Duration::ZERO);
    let remaining = delay.saturating_sub(elapsed);
    DeliveryRetry {
        ready_at: now + remaining,
        next_delay: next_delivery_backoff(delay),
    }
}

fn retry_delay_for_attempts(attempts: i64) -> Duration {
    let mut delay = RETRY_BACKOFF_MIN;
    for _ in 1..attempts.max(1) {
        delay = next_delivery_backoff(delay);
    }
    delay
}

fn oldest_per_thread(rows: Vec<ProviderTurnDelivery>) -> Vec<ProviderTurnDelivery> {
    let mut threads = HashSet::new();
    rows.into_iter()
        .filter(|row| threads.insert(row.thread_id.clone()))
        .collect()
}

fn claimable_oldest_per_thread(rows: Vec<ProviderTurnDelivery>) -> Vec<ProviderTurnDelivery> {
    oldest_per_thread(rows)
        .into_iter()
        .filter(|row| row.state == TurnDeliveryState::Pending)
        .collect()
}

async fn prepare_claim_and_deliver(
    engine: OrchestrationEngine,
    router: ProviderDeliveryRouter,
    row: ProviderTurnDelivery,
    stop_claiming: CancellationToken,
    force_cancel: CancellationToken,
) -> DeliveryCompletion {
    let command_id = row.command_id.clone();
    let thread_id = row.thread_id.clone();
    let result = async {
        if row.mode == TurnDeliveryMode::Start {
            execute_bootstrap_prerequisites(&engine, &row, &force_cancel).await?;
        }
        if stop_claiming.is_cancelled() {
            return Err("provider delivery claim cancelled".to_owned());
        }
        let claimed = engine
            .repositories()
            .claim_provider_turn(row.command_id, now())
            .await
            .map_err(|error| error.to_string())?;
        let Some(claimed) = claimed else {
            return Ok(DeliveryTaskOutcome::Finished);
        };
        if stop_claiming.is_cancelled() || force_cancel.is_cancelled() {
            return Err("provider delivery route cancelled after claim".to_owned());
        }
        deliver_claimed(&engine, &router, claimed, &force_cancel).await
    }
    .await;
    DeliveryCompletion {
        command_id,
        thread_id,
        result,
    }
}

async fn execute_bootstrap_prerequisites(
    engine: &OrchestrationEngine,
    row: &ProviderTurnDelivery,
    shutdown: &CancellationToken,
) -> Result<(), String> {
    let Ok(command) = serde_json::from_value::<OrchestrationCommand>(row.payload.clone()) else {
        return Ok(());
    };
    let OrchestrationCommand::ThreadTurnStart {
        thread_id,
        bootstrap: Some(bootstrap),
        ..
    } = command
    else {
        return Ok(());
    };
    let project_id = bootstrap
        .create_thread
        .as_ref()
        .map(|create| create.project_id.clone());
    let project_cwd = bootstrap
        .prepare_worktree
        .as_ref()
        .map(|prepare| prepare.project_cwd.clone());
    let mut worktree_path = bootstrap
        .create_thread
        .as_ref()
        .and_then(|create| create.worktree_path.clone());
    if let Some(prepare) = bootstrap.prepare_worktree {
        let effects = engine
            .bootstrap_effects()
            .ok_or_else(|| "production bootstrap effects are not registered".to_owned())?;
        let worktree = effects.prepare_worktree(prepare, shutdown).await?;
        worktree_path = Some(worktree.path.clone());
        let update = OrchestrationCommand::ThreadMetaUpdate {
            command_id: format!("server:bootstrap:{}:thread-meta", row.command_id),
            thread_id: thread_id.clone(),
            title: None,
            model_selection: None,
            branch: OptionalNullable::Present(Some(worktree.branch)),
            worktree_path: OptionalNullable::Present(Some(worktree.path)),
        };
        tokio::select! {
            () = shutdown.cancelled() => return Err("bootstrap metadata update cancelled".to_owned()),
            result = engine.dispatch(update) => result.map_err(|error| error.to_string())?,
        };
    }
    let mut finalized_payload = row.payload.clone();
    if finalize_delivery_route_cwd(
        &mut finalized_payload,
        worktree_path.as_deref().map(Path::new),
    )
    .map_err(|error| error.to_string())?
    {
        let finalized = engine
            .repositories()
            .replace_pending_provider_turn_payload(
                row.command_id.clone(),
                row.attempts,
                finalized_payload,
                now(),
            )
            .await
            .map_err(|error| error.to_string())?;
        if finalized.is_none() {
            return Err(
                "durable provider route changed state before its worktree cwd was finalized"
                    .to_owned(),
            );
        }
    }
    if bootstrap.run_setup_script == Some(true)
        && let Some(worktree_path) = worktree_path
    {
        tokio::select! {
            () = shutdown.cancelled() => return Err("bootstrap setup script cancelled".to_owned()),
            result = engine.run_bootstrap_setup(
                &row.command_id,
                &thread_id,
                project_id,
                project_cwd,
                worktree_path,
                row.created_at.clone(),
            ) => result.map_err(|error| error.to_string())?,
        };
    }
    Ok(())
}

async fn deliver_claimed(
    engine: &OrchestrationEngine,
    router: &ProviderDeliveryRouter,
    row: ProviderTurnDelivery,
    shutdown: &CancellationToken,
) -> Result<DeliveryTaskOutcome, String> {
    let is_steer = row.mode == TurnDeliveryMode::Steer;
    let active_turn_id = if is_steer {
        engine
            .repositories()
            .get_running_provider_steer_target(row.thread_id.clone(), row.message_id.clone())
            .await
            .map_err(|error| error.to_string())
    } else {
        Ok(None)
    };
    let route_result = match &active_turn_id {
        Err(detail) => ProviderDeliveryOutcome::DefinitelyNotSent {
            detail: delivery_detail(&ProviderRuntimeError::Persistence(detail.clone()), None),
        },
        Ok(None) if is_steer => ProviderDeliveryOutcome::Rejected {
            detail: STEER_TARGET_GONE_DETAIL.to_owned(),
        },
        _ => match serde_json::from_value::<OrchestrationCommand>(row.payload.clone()) {
            Ok(command) => router(command, row.delivery_key.clone()).await,
            Err(error) => ProviderDeliveryOutcome::Rejected {
                detail: format!("durable turn payload is invalid: {error}"),
            },
        },
    };
    let turn_id = match &route_result {
        ProviderDeliveryOutcome::Accepted { turn_id } if is_steer => {
            turn_id.clone().or(active_turn_id.ok().flatten())
        }
        _ => None,
    };
    let (next_state, detail, reason, task_outcome) = match route_result {
        ProviderDeliveryOutcome::Rejected { detail }
        | ProviderDeliveryOutcome::Refused { detail }
            if is_steer =>
        {
            (
                TurnDeliveryState::Queued,
                Some(detail),
                None,
                DeliveryTaskOutcome::Finished,
            )
        }
        outcome => provider_delivery_outcome(outcome),
    };
    let transition = TurnDeliveryTransition {
        turn_id,
        command_id: row.command_id.clone(),
        expected_states: vec![TurnDeliveryState::Sending],
        expected_attempt: row.attempts,
        next_state,
        detail,
        updated_at: now(),
    };
    persist_delivery_outcome(engine, shutdown, transition, reason)
        .await
        .map(|()| task_outcome)
}

async fn persist_delivery_outcome(
    engine: &OrchestrationEngine,
    shutdown: &CancellationToken,
    transition: TurnDeliveryTransition,
    reason: Option<TurnDeliveryFailureReason>,
) -> Result<(), String> {
    let mut transition = transition;
    let mut backoff = RETRY_BACKOFF_MIN;
    loop {
        let result = tokio::select! {
            () = shutdown.cancelled() => return Err("provider delivery transition cancelled".to_owned()),
            result = engine.transition_turn_delivery_with_reason(transition.clone(), reason) => result,
        };
        match result {
            Ok(true) => return Ok(()),
            Ok(false) => {
                let current = engine
                    .repositories()
                    .get_provider_turn_delivery(transition.command_id.clone())
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "provider delivery {} disappeared during outcome persistence",
                            transition.command_id
                        )
                    })?;
                if current.state == transition.next_state
                    || current.state == TurnDeliveryState::Dismissed
                {
                    return Ok(());
                }
                if transition.next_state == TurnDeliveryState::Delivered
                    && current.state == TurnDeliveryState::Pending
                    && current.attempts == transition.expected_attempt
                {
                    transition.expected_states = vec![TurnDeliveryState::Pending];
                    transition.expected_attempt = current.attempts;
                    transition.updated_at = now();
                    continue;
                }
                return Err(format!(
                    "provider delivery {} outcome conflicted with durable state {:?} at attempt {}",
                    transition.command_id, current.state, current.attempts
                ));
            }
            Err(error) => {
                tracing::warn!(%error, "provider delivery transition failed; retrying");
                tokio::select! {
                    () = shutdown.cancelled() => {
                        return Err("provider delivery transition cancelled".to_owned());
                    }
                    () = sleep(backoff) => {}
                }
                backoff = next_backoff(backoff);
            }
        }
    }
}

#[cfg(test)]
fn delivery_outcome(
    provider_kind: &str,
    route_result: Result<(), ProviderRuntimeError>,
) -> (TurnDeliveryState, Option<String>) {
    match (provider_kind, route_result) {
        ("codex" | "opencode", Ok(())) => (TurnDeliveryState::Delivered, None),
        ("claudeAgent" | "cursor", Ok(())) => (TurnDeliveryState::Uncertain, None),
        (_, Ok(())) => (TurnDeliveryState::Delivered, None),
        (_, Err(error)) => (TurnDeliveryState::Failed, Some(error.to_string())),
    }
}

fn provider_delivery_outcome(
    outcome: ProviderDeliveryOutcome,
) -> (
    TurnDeliveryState,
    Option<String>,
    Option<TurnDeliveryFailureReason>,
    DeliveryTaskOutcome,
) {
    match outcome {
        ProviderDeliveryOutcome::Accepted { .. } => (
            TurnDeliveryState::Delivered,
            None,
            None,
            DeliveryTaskOutcome::Finished,
        ),
        ProviderDeliveryOutcome::AcceptedInNewConversation { .. } => (
            TurnDeliveryState::Delivered,
            None,
            Some(TurnDeliveryFailureReason::StartedNewConversation),
            DeliveryTaskOutcome::Finished,
        ),
        ProviderDeliveryOutcome::DefinitelyNotSent { detail } => (
            TurnDeliveryState::Pending,
            Some(detail),
            None,
            DeliveryTaskOutcome::DefinitelyNotSent,
        ),
        ProviderDeliveryOutcome::Ambiguous { detail } => (
            TurnDeliveryState::Uncertain,
            Some(detail),
            None,
            DeliveryTaskOutcome::Finished,
        ),
        ProviderDeliveryOutcome::Rejected { detail } => (
            TurnDeliveryState::Failed,
            Some(detail),
            None,
            DeliveryTaskOutcome::Finished,
        ),
        ProviderDeliveryOutcome::Refused { detail } => (
            TurnDeliveryState::Failed,
            Some(detail),
            Some(TurnDeliveryFailureReason::ModelSelectionRefused),
            DeliveryTaskOutcome::Finished,
        ),
    }
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        activity::{ActivityProjection, ActivityRepository},
        orchestration::{
            EngineOptions,
            engine::{
                BootstrapSetupInput, BootstrapSetupResult, BootstrapWorktree, BoxBootstrapFuture,
                TestHooks, ThreadTurnBootstrapEffects, ThreadTurnStartBootstrapPrepareWorktree,
            },
        },
        persistence::{Database, run_migrations},
        production::provider_runtime::{
            BoxRuntimeFuture, ProviderDriver, ProviderDriverFactory, ProviderLaunchRequest,
            SupervisorOptions,
        },
        worktree_catalog::{AdoptedWorktreeAvailability, WorkspaceLossTransition},
    };
    use std::{
        future::ready,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct NeverFactory;

    impl ProviderDriverFactory for NeverFactory {
        fn create(
            &self,
            request: ProviderLaunchRequest,
        ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
            Box::pin(async move {
                Err(ProviderRuntimeError::UnsupportedProvider {
                    provider: request.provider,
                })
            })
        }
    }

    pub(super) fn row(command_id: &str, thread_id: &str) -> ProviderTurnDelivery {
        ProviderTurnDelivery {
            mode: crate::orchestration::TurnDeliveryMode::Start,
            held: false,
            command_id: command_id.to_owned(),
            thread_id: thread_id.to_owned(),
            message_id: format!("message-{command_id}"),
            provider_instance_id: "codex".to_owned(),
            provider_kind: "codex".to_owned(),
            provider_session_id: None,
            delivery_key: format!("key-{command_id}"),
            payload: serde_json::json!({}),
            state: TurnDeliveryState::Pending,
            attempts: 0,
            last_error: None,
            created_at: "2026-08-01T00:00:00Z".to_owned(),
            updated_at: "2026-08-01T00:00:00Z".to_owned(),
        }
    }

    fn turn_payload(command_id: &str, thread_id: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "thread.turn.start",
            "commandId": command_id,
            "threadId": thread_id,
            "message": {
                "messageId": format!("message-{command_id}"),
                "role": "user",
                "text": command_id,
                "attachments": []
            },
            "createdAt": "2026-08-01T00:00:00Z"
        })
    }

    async fn wait_for_count(counter: &AtomicUsize, changed: &Notify, expected: usize) {
        loop {
            let notified = changed.notified();
            let actual = counter.load(Ordering::SeqCst);
            if actual >= expected {
                assert_eq!(actual, expected);
                return;
            }
            notified.await;
        }
    }

    #[tokio::test]
    async fn unavailable_workspace_blocks_delivery_and_reconciliation_before_provider_routing() {
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(WorkspaceLossTransition {
                    thread_id: "thread-guarded".to_owned(),
                    repository_key: "repository-a".to_owned(),
                    generation: 1,
                    path: PathBuf::from("/repo/worktrees/guarded"),
                    availability: AdoptedWorktreeAvailability::MissingRegistered,
                })
                .await
                .expect("physical identity resolves")
        );
        let admission = WorkspaceAdmissionController::registry_only(registry);
        let delivery_calls = Arc::new(AtomicUsize::new(0));
        let observed_delivery_calls = delivery_calls.clone();
        let router = guard_delivery_router(
            admission.clone(),
            Arc::new(move |_command, _delivery_key| {
                observed_delivery_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(ready(ProviderDeliveryOutcome::Accepted { turn_id: None }))
            }),
        );
        let reconciliation_calls = Arc::new(AtomicUsize::new(0));
        let observed_reconciliation_calls = reconciliation_calls.clone();
        let reconciler = guard_delivery_reconciler(
            admission,
            Arc::new(move |_row| {
                observed_reconciliation_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(ready(ProviderReconciliationOutcome::Found))
            }),
        );

        let command = serde_json::from_value(turn_payload("command-1", "thread-guarded"))
            .expect("turn command");
        assert!(matches!(
            router(command, "delivery-key".to_owned()).await,
            ProviderDeliveryOutcome::DefinitelyNotSent { .. }
        ));
        assert!(matches!(
            reconciler(row("command-1", "thread-guarded")).await,
            ProviderReconciliationOutcome::Unavailable { .. }
        ));
        assert_eq!(delivery_calls.load(Ordering::SeqCst), 0);
        assert_eq!(reconciliation_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn workspace_loss_preserves_ambiguity_after_provider_routing() {
        struct PendingRoute {
            started: Arc<Semaphore>,
            written: Arc<AtomicBool>,
            drops: Arc<AtomicUsize>,
        }

        impl Future for PendingRoute {
            type Output = ProviderDeliveryOutcome;

            fn poll(
                self: Pin<&mut Self>,
                _context: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                if !self.written.swap(true, Ordering::SeqCst) {
                    // Routing may write to the provider before waiting for its
                    // acknowledgement. Dropping the future cannot undo that.
                    self.started.add_permits(1);
                }
                std::task::Poll::Pending
            }
        }

        impl Drop for PendingRoute {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }

        let registry = WorkspaceAvailabilityRegistry::new();
        let started = Arc::new(Semaphore::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let written = Arc::new(AtomicBool::new(false));
        let router = guard_delivery_router(
            WorkspaceAdmissionController::registry_only(registry.clone()),
            Arc::new({
                let started = started.clone();
                let drops = drops.clone();
                let written = written.clone();
                move |_command, _delivery_key| {
                    Box::pin(PendingRoute {
                        started: started.clone(),
                        written: written.clone(),
                        drops: drops.clone(),
                    })
                }
            }),
        );
        let command = serde_json::from_value(turn_payload("command-loss", "thread-loss"))
            .expect("turn command");
        let route = tokio::spawn(async move { router(command, "delivery-loss".to_owned()).await });
        started
            .acquire()
            .await
            .expect("provider route starts")
            .forget();

        assert!(written.load(Ordering::SeqCst));
        assert!(
            registry
                .mark_unavailable(WorkspaceLossTransition {
                    thread_id: "thread-loss".to_owned(),
                    repository_key: "repository-a".to_owned(),
                    generation: 1,
                    path: PathBuf::from("/repo/worktrees/loss"),
                    availability: AdoptedWorktreeAvailability::MissingRegistered,
                })
                .await
                .expect("physical identity resolves")
        );
        tokio::task::yield_now().await;
        let finished = route.is_finished();
        if !finished {
            route.abort();
        }
        assert!(
            finished,
            "workspace loss must cancel the provider publication future"
        );
        assert!(matches!(
            route.await.expect("guarded route task"),
            ProviderDeliveryOutcome::Ambiguous { .. }
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    async fn seed_delivery(
        database: &Database,
        command_id: &str,
        thread_id: &str,
        ordinal: u8,
        state: TurnDeliveryState,
        attempts: i64,
    ) {
        seed_delivery_for_provider(
            database,
            command_id,
            thread_id,
            ordinal,
            state,
            attempts,
            "codex",
            &format!("key-{ordinal}"),
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_delivery_for_provider(
        database: &Database,
        command_id: &str,
        thread_id: &str,
        ordinal: u8,
        state: TurnDeliveryState,
        attempts: i64,
        provider_kind: &str,
        delivery_key: &str,
    ) {
        let command_id = command_id.to_owned();
        let thread_id = thread_id.to_owned();
        let provider_kind = provider_kind.to_owned();
        let delivery_key = delivery_key.to_owned();
        let payload = turn_payload(&command_id, &thread_id).to_string();
        let state = match state {
            TurnDeliveryState::Queued => "queued",
            TurnDeliveryState::Pending => "pending",
            TurnDeliveryState::Sending => "sending",
            TurnDeliveryState::Delivered => "delivered",
            TurnDeliveryState::Uncertain => "uncertain",
            TurnDeliveryState::Dismissed => "dismissed",
            TurnDeliveryState::Failed => "failed",
        };
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES (?, 'thread', ?, ?, 0, 'accepted', NULL, 'digest')",
                    rusqlite::params![&command_id, &thread_id, format!("2026-08-01T00:00:0{ordinal}Z")],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, NULL, ?, ?)",
                    rusqlite::params![
                        command_id,
                        thread_id,
                        format!("message-{ordinal}"),
                        provider_kind,
                        provider_kind,
                        delivery_key,
                        payload,
                        state,
                        attempts,
                        format!("2026-08-01T00:00:0{ordinal}Z"),
                        format!("2026-08-01T00:00:0{ordinal}Z"),
                    ],
                )?;
                Ok(())
            })
            .await
            .expect("seed pending delivery");
    }

    async fn seed_pending(database: &Database, command_id: &str, thread_id: &str, ordinal: u8) {
        seed_delivery(
            database,
            command_id,
            thread_id,
            ordinal,
            TurnDeliveryState::Pending,
            0,
        )
        .await;
    }

    #[test]
    fn delivery_order_keeps_only_the_oldest_row_per_thread() {
        let selected = oldest_per_thread(vec![row("a", "one"), row("b", "one"), row("c", "two")]);
        assert_eq!(
            selected
                .into_iter()
                .map(|row| row.command_id)
                .collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }

    #[test]
    fn delivery_order_blocks_pending_behind_uncertain() {
        let mut uncertain = row("uncertain", "one");
        uncertain.state = TurnDeliveryState::Uncertain;
        let selected = claimable_oldest_per_thread(vec![uncertain, row("pending", "one")]);
        assert!(selected.is_empty());
    }

    #[test]
    fn delivery_order_blocks_pending_behind_failed() {
        let mut failed = row("failed", "one");
        failed.state = TurnDeliveryState::Failed;
        let selected = claimable_oldest_per_thread(vec![failed, row("pending", "one")]);
        assert!(selected.is_empty());
    }

    #[test]
    fn delivery_success_is_conservative_by_provider_kind() {
        assert_eq!(
            delivery_outcome("codex", Ok(())).0,
            TurnDeliveryState::Delivered
        );
        assert_eq!(
            delivery_outcome("opencode", Ok(())).0,
            TurnDeliveryState::Delivered
        );
        assert_eq!(
            delivery_outcome("claudeAgent", Ok(())).0,
            TurnDeliveryState::Uncertain
        );
        assert_eq!(
            delivery_outcome("cursor", Ok(())).0,
            TurnDeliveryState::Uncertain
        );
        assert_eq!(
            delivery_outcome("codex", Err(ProviderRuntimeError::Shutdown)).0,
            TurnDeliveryState::Failed
        );
    }

    #[test]
    fn provider_delivery_outcomes_have_explicit_durable_transitions() {
        assert_eq!(
            provider_delivery_outcome(ProviderDeliveryOutcome::AcceptedInNewConversation {
                turn_id: Some("fresh".to_owned())
            }),
            (
                TurnDeliveryState::Delivered,
                None,
                Some(TurnDeliveryFailureReason::StartedNewConversation),
                DeliveryTaskOutcome::Finished
            ),
        );
        assert_eq!(
            provider_delivery_outcome(ProviderDeliveryOutcome::Accepted { turn_id: None }),
            (
                TurnDeliveryState::Delivered,
                None,
                None,
                DeliveryTaskOutcome::Finished
            )
        );
        assert_eq!(
            provider_delivery_outcome(ProviderDeliveryOutcome::DefinitelyNotSent {
                detail: "not admitted".to_owned(),
            }),
            (
                TurnDeliveryState::Pending,
                Some("not admitted".to_owned()),
                None,
                DeliveryTaskOutcome::DefinitelyNotSent
            )
        );
        assert_eq!(
            provider_delivery_outcome(ProviderDeliveryOutcome::Ambiguous {
                detail: "connection lost".to_owned(),
            })
            .0,
            TurnDeliveryState::Uncertain
        );
        assert_eq!(
            provider_delivery_outcome(ProviderDeliveryOutcome::Rejected {
                detail: "invalid request".to_owned(),
            }),
            (
                TurnDeliveryState::Failed,
                Some("invalid request".to_owned()),
                None,
                DeliveryTaskOutcome::Finished
            )
        );
        assert_eq!(
            provider_delivery_outcome(ProviderDeliveryOutcome::Refused {
                detail: "model refused".to_owned(),
            }),
            (
                TurnDeliveryState::Failed,
                Some("model refused".to_owned()),
                Some(crate::orchestration::TurnDeliveryFailureReason::ModelSelectionRefused),
                DeliveryTaskOutcome::Finished
            )
        );
    }

    #[tokio::test]
    async fn delivery_reason_persists_through_transition_retries_without_resending() {
        for (outcome, expected_state, expected_reason, expected_detail, state_label) in [
            (
                ProviderDeliveryOutcome::Refused {
                    detail: "model refused".to_owned(),
                },
                TurnDeliveryState::Failed,
                "modelSelectionRefused",
                Some("model refused"),
                "failed",
            ),
            (
                ProviderDeliveryOutcome::AcceptedInNewConversation {
                    turn_id: Some("fresh".to_owned()),
                },
                TurnDeliveryState::Delivered,
                "startedNewConversation",
                None,
                "delivered",
            ),
        ] {
            let database = Database::open_in_memory().await.expect("database");
            database
                .call(|connection| Ok(run_migrations(connection, None)?))
                .await
                .expect("migrations");
            seed_pending(&database, "refused", "thread", 1).await;
            database.call(|connection| {
            connection.execute(
                "INSERT INTO projection_thread_messages (message_id, thread_id, role, text, is_streaming, created_at, updated_at) VALUES ('message-1', 'thread', 'user', 'refused', 0, 'created', 'created')", [],
            )?;
            Ok(())
        }).await.expect("message projection");
            let hooks = TestHooks::default();
            hooks.fail_next_delivery_transitions(2);
            let engine = OrchestrationEngine::start(
                database.clone(),
                EngineOptions {
                    test_hooks: hooks.clone(),
                    ..EngineOptions::default()
                },
            )
            .await
            .expect("engine");
            let calls = Arc::new(AtomicUsize::new(0));
            let router: ProviderDeliveryRouter = Arc::new({
                let calls = calls.clone();
                move |_, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Box::pin(ready(outcome.clone()))
                }
            });
            let service = TurnDeliveryService::start_with_delivery_router(
                engine.clone(),
                1,
                router,
                Arc::new(|_| Box::pin(ready(ProviderReconciliationOutcome::Absent))),
            );
            timeout(Duration::from_secs(5), async {
                loop {
                    let row = engine
                        .repositories()
                        .get_provider_turn_delivery("refused".to_owned())
                        .await
                        .expect("outbox")
                        .expect("delivery");
                    if row.state == expected_state {
                        assert_eq!(row.attempts, 1);
                        assert_eq!(row.last_error.as_deref(), expected_detail);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("refusal persisted");
            let reason = database
                .call(|connection| {
                    Ok(connection.query_row(
                    "SELECT failure_reason FROM provider_turn_outbox WHERE command_id = 'refused'",
                    [],
                    |row| row.get::<_, Option<String>>(0),
                )?)
                })
                .await
                .expect("outbox reason");
            assert_eq!(reason.as_deref(), Some(expected_reason));
            let events = engine.read_events(0).await.expect("events");
            let refused = events
                .iter()
                .filter(|event| {
                    event.event.event_type == "thread.turn-delivery-updated"
                        && event.event.payload["state"] == state_label
                })
                .collect::<Vec<_>>();
            assert_eq!(refused.len(), 1);
            assert_eq!(
                refused[0].event.payload["delivery"]["reason"],
                expected_reason
            );
            let message = engine
                .repositories()
                .get_message("message-1".to_owned())
                .await
                .expect("message lookup")
                .expect("message");
            assert_eq!(message.delivery_reason.as_deref(), Some(expected_reason));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(hooks.delivery_transition_attempts(), 3);
            service.shutdown().await;
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn refused_steer_returns_to_queue_without_a_failure_reason() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery(
            &database,
            "refused-steer",
            "thread",
            1,
            TurnDeliveryState::Sending,
            1,
        )
        .await;
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("engine");
        database.call(|connection| {
            connection.execute_batch(
                "UPDATE provider_turn_outbox SET mode = 'steer';
                 INSERT INTO projection_thread_sessions (thread_id, status, provider_name, active_turn_id, updated_at) VALUES ('thread', 'running', 'codex', 'turn-1', 'updated');
                 INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, actor_kind, payload_json, metadata_json)
                 VALUES ('steer-event', 'thread', 'thread', 1, 'thread.turn-steer-requested', 'updated', 'server', '{\"threadId\":\"thread\",\"messageId\":\"message-1\",\"turnId\":\"turn-1\"}', '{}');
                 INSERT INTO projection_thread_messages (message_id, thread_id, role, text, is_streaming, created_at, updated_at) VALUES ('message-1', 'thread', 'user', 'steer', 0, 'created', 'created');",
            )?;
            Ok(())
        }).await.expect("running steer target");
        let row = engine
            .repositories()
            .get_provider_turn_delivery("refused-steer".to_owned())
            .await
            .expect("outbox")
            .expect("steer");
        let calls = Arc::new(AtomicUsize::new(0));
        let router: ProviderDeliveryRouter = Arc::new({
            let calls = calls.clone();
            move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(ready(ProviderDeliveryOutcome::Refused {
                    detail: "model refused".to_owned(),
                }))
            }
        });
        assert_eq!(
            deliver_claimed(&engine, &router, row, &CancellationToken::new())
                .await
                .expect("requeue"),
            DeliveryTaskOutcome::Finished
        );
        let row = engine
            .repositories()
            .get_provider_turn_delivery("refused-steer".to_owned())
            .await
            .expect("outbox")
            .expect("queued");
        assert_eq!(row.state, TurnDeliveryState::Queued);
        assert_eq!(row.mode, TurnDeliveryMode::Start);
        assert_eq!(row.attempts, 0);
        assert_eq!(row.last_error.as_deref(), Some("model refused"));
        let reason = database.call(|connection| Ok(connection.query_row(
            "SELECT failure_reason FROM provider_turn_outbox WHERE command_id = 'refused-steer'", [], |row| row.get::<_, Option<String>>(0),
        )?)).await.expect("outbox reason");
        assert_eq!(reason, None);
        let message = engine
            .repositories()
            .get_message("message-1".to_owned())
            .await
            .expect("message lookup")
            .expect("message");
        assert_eq!(message.delivery_state.as_deref(), Some("queued"));
        assert_eq!(message.delivery_reason, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn restart_recovery_marks_a_delivery_uncertain_when_its_conversation_is_lost() {
        let database = Database::open_in_memory().await.expect("database");
        database.call(|connection| {
            run_migrations(connection, None)?;
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('recover', 'thread', 'thread-1', '2026-08-01T00:00:00Z', 0, 'accepted', NULL, 'digest')",
                [],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('recover', 'thread-1', 'message-1', 'opencode', 'opencode', NULL, 'key', '{}', 'sending', 1, NULL, '2026-08-01T00:00:00Z', '2026-08-01T00:00:01Z')",
                [],
            )?;
            Ok(())
        }).await.expect("seed");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let routed = Arc::new(AtomicUsize::new(0));
        let router: ProviderDeliveryRouter = Arc::new({
            let routed = routed.clone();
            move |_, _| {
                routed.fetch_add(1, Ordering::SeqCst);
                Box::pin(ready(ProviderDeliveryOutcome::Accepted { turn_id: None }))
            }
        });
        let service = TurnDeliveryService::start_with_delivery_router(
            engine.clone(),
            1,
            router,
            Arc::new(|_| {
                Box::pin(ready(ProviderReconciliationOutcome::ConversationLost {
                    detail: "conversation lost".to_owned(),
                }))
            }),
        );

        let row = timeout(Duration::from_secs(5), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery("recover".to_owned())
                    .await
                    .expect("outbox")
                    .expect("delivery");
                if row.state != TurnDeliveryState::Sending {
                    break row;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("recovery settles the row");
        // Unknowable arrival waits for the user's explicit retry instead of resending.
        assert_eq!(row.state, TurnDeliveryState::Uncertain);
        assert_eq!(row.last_error.as_deref(), Some("conversation lost"));
        assert_eq!(routed.load(Ordering::SeqCst), 0);

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_service_preserves_sending_when_exact_reconciliation_is_unavailable() {
        let database = Database::open_in_memory().await.expect("database");
        database.call(|connection| {
            run_migrations(connection, None)?;
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('recover', 'thread', 'thread-1', '2026-08-01T00:00:00Z', 0, 'accepted', NULL, 'digest')",
                [],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('recover', 'thread-1', 'message-1', 'codex', 'codex', NULL, 'key', '{}', 'sending', 1, NULL, '2026-08-01T00:00:00Z', '2026-08-01T00:00:01Z')",
                [],
            )?;
            Ok(())
        }).await.expect("seed");
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("engine");
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(NeverFactory),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let state = tempfile::tempdir().expect("state");
        let service = TurnDeliveryService::start(
            engine.clone(),
            provider.clone(),
            state.path().to_path_buf(),
        );

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let row = engine
            .repositories()
            .get_provider_turn_delivery("recover".to_owned())
            .await
            .expect("row")
            .expect("delivery");
        assert_eq!(row.state, TurnDeliveryState::Sending);
        assert_eq!(engine.repositories().provider_turn_claims_for_test(), 0);

        service.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    /// Settles, as a restart does, a Claude row left sending, with `settings` as the server's
    /// settings file, and returns the persisted row.
    async fn restart_a_claude_sending_row(
        settings: Option<serde_json::Value>,
    ) -> ProviderTurnDelivery {
        let database = Database::open_in_memory().await.expect("database");
        database.call(|connection| {
            run_migrations(connection, None)?;
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('recover', 'thread', 'thread-1', '2026-08-01T00:00:00Z', 0, 'accepted', NULL, 'digest')",
                [],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('recover', 'thread-1', 'message-1', 'claudeAgent', 'claudeAgent', NULL, 'key', '{}', 'sending', 1, NULL, '2026-08-01T00:00:00Z', '2026-08-01T00:00:01Z')",
                [],
            )?;
            Ok(())
        }).await.expect("seed");
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("engine");
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(NeverFactory),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let state = tempfile::tempdir().expect("state");
        if let Some(settings) = settings {
            std::fs::write(
                state.path().join("settings.json"),
                serde_json::to_vec(&settings).expect("settings json"),
            )
            .expect("settings");
        }
        let service = TurnDeliveryService::start(
            engine.clone(),
            provider.clone(),
            state.path().to_path_buf(),
        );

        let row = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery("recover".to_owned())
                    .await
                    .expect("row")
                    .expect("delivery");
                if row.state != TurnDeliveryState::Sending {
                    break row;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the sending row is settled at startup");
        service.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
        row
    }

    /// Claude cannot look up a delivery after a restart, so a row left sending becomes uncertain,
    /// and its detail says so in plain words, naming the instance as the user does.
    #[tokio::test]
    async fn restart_leaves_a_claude_sending_row_uncertain_with_a_plain_detail() {
        let row = restart_a_claude_sending_row(None).await;
        assert_eq!(row.state, TurnDeliveryState::Uncertain);
        assert_eq!(
            row.last_error.as_deref(),
            Some("BiBCode can't check whether Claude received this message after a restart.")
        );

        let row = restart_a_claude_sending_row(Some(serde_json::json!({
            "providerInstances": {
                "claudeAgent": {"driver":"claudeAgent", "enabled":true, "displayName":"Work Claude"}
            }
        })))
        .await;
        assert_eq!(row.state, TurnDeliveryState::Uncertain);
        assert_eq!(
            row.last_error.as_deref(),
            Some("BiBCode can't check whether Work Claude received this message after a restart.")
        );

        // The label is read without the instance's secrets, so a missing secret file still
        // leaves the display name.
        let row = restart_a_claude_sending_row(Some(serde_json::json!({
            "providerInstances": {
                "claudeAgent": {
                    "driver":"claudeAgent", "enabled":true, "displayName":"Work Claude",
                    "environment":[{
                        "name":"ANTHROPIC_API_KEY", "value":"", "sensitive":true, "valueRedacted":true
                    }]
                }
            }
        })))
        .await;
        assert_eq!(row.state, TurnDeliveryState::Uncertain);
        assert_eq!(
            row.last_error.as_deref(),
            Some("BiBCode can't check whether Work Claude received this message after a restart.")
        );
    }

    #[tokio::test]
    async fn unavailable_reconciliation_retries_with_bounded_backoff_then_resends_after_absent() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery_for_provider(
            &database,
            "recover-bounded",
            "thread-bounded",
            1,
            TurnDeliveryState::Sending,
            1,
            "codex",
            "stable-bounded-key",
        )
        .await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let sends = Arc::new(AtomicUsize::new(0));
        let send_times = Arc::new(std::sync::Mutex::new(Vec::new()));
        let router: ProviderDeliveryRouter = Arc::new({
            let sends = sends.clone();
            let send_times = send_times.clone();
            move |_command, _delivery_key| {
                sends.fetch_add(1, Ordering::SeqCst);
                send_times.lock().unwrap().push(Instant::now());
                Box::pin(async { ProviderDeliveryOutcome::Accepted { turn_id: None } })
            }
        });
        let reconciliations = Arc::new(AtomicUsize::new(0));
        let reconciliation_times = Arc::new(std::sync::Mutex::new(Vec::new()));
        let reconciler: DeliveryReconciler = Arc::new({
            let reconciliations = reconciliations.clone();
            let reconciliation_times = reconciliation_times.clone();
            move |_row| {
                let attempt = reconciliations.fetch_add(1, Ordering::SeqCst);
                reconciliation_times.lock().unwrap().push(Instant::now());
                Box::pin(async move {
                    if attempt < 2 {
                        ProviderReconciliationOutcome::Unavailable {
                            detail: "temporarily unavailable".to_owned(),
                        }
                    } else {
                        ProviderReconciliationOutcome::Absent
                    }
                })
            }
        });
        let service =
            TurnDeliveryService::start_with_delivery_router(engine.clone(), 1, router, reconciler);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery("recover-bounded".to_owned())
                    .await
                    .expect("row")
                    .expect("delivery");
                if row.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("authoritative absent eventually permits resend");
        let attempts = reconciliations.load(Ordering::SeqCst);
        assert_eq!(attempts, 3);
        let last_reconciliation = {
            let reconciliation_times = reconciliation_times.lock().unwrap();
            assert!(
                reconciliation_times[1].duration_since(reconciliation_times[0])
                    >= Duration::from_millis(40)
            );
            assert!(
                reconciliation_times[2].duration_since(reconciliation_times[1])
                    >= Duration::from_millis(90)
            );
            reconciliation_times[2]
        };
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert!(send_times.lock().unwrap()[0] >= last_reconciliation);
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("recover-bounded".to_owned())
                .await
                .expect("row")
                .expect("delivery")
                .state,
            TurnDeliveryState::Delivered
        );

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn startup_recovery_never_reconciles_or_resends_a_live_claim() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery_for_provider(
            &database,
            "recover-unavailable",
            "thread-recover",
            1,
            TurnDeliveryState::Sending,
            1,
            "codex",
            "recover-key",
        )
        .await;
        seed_delivery_for_provider(
            &database,
            "live-send",
            "thread-live",
            2,
            TurnDeliveryState::Pending,
            0,
            "codex",
            "live-key",
        )
        .await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");

        let sends = Arc::new(AtomicUsize::new(0));
        let first_send_started = Arc::new(Notify::new());
        let release_first_send = Arc::new(Notify::new());
        let router: ProviderDeliveryRouter = Arc::new({
            let sends = sends.clone();
            let first_send_started = first_send_started.clone();
            let release_first_send = release_first_send.clone();
            move |_command, _delivery_key| {
                let attempt = sends.fetch_add(1, Ordering::SeqCst);
                let first_send_started = first_send_started.clone();
                let release_first_send = release_first_send.clone();
                Box::pin(async move {
                    if attempt == 0 {
                        first_send_started.notify_one();
                        release_first_send.notified().await;
                    }
                    ProviderDeliveryOutcome::Accepted { turn_id: None }
                })
            }
        });
        let cohort_reconciliations = Arc::new(AtomicUsize::new(0));
        let second_cohort_reconciliation = Arc::new(Notify::new());
        let live_reconciliations = Arc::new(AtomicUsize::new(0));
        let reconciler: DeliveryReconciler = Arc::new({
            let cohort_reconciliations = cohort_reconciliations.clone();
            let second_cohort_reconciliation = second_cohort_reconciliation.clone();
            let live_reconciliations = live_reconciliations.clone();
            move |row| {
                let cohort_reconciliations = cohort_reconciliations.clone();
                let second_cohort_reconciliation = second_cohort_reconciliation.clone();
                let live_reconciliations = live_reconciliations.clone();
                Box::pin(async move {
                    if row.command_id == "live-send" {
                        live_reconciliations.fetch_add(1, Ordering::SeqCst);
                        ProviderReconciliationOutcome::Absent
                    } else {
                        if cohort_reconciliations.fetch_add(1, Ordering::SeqCst) == 1 {
                            second_cohort_reconciliation.notify_one();
                        }
                        ProviderReconciliationOutcome::Unavailable {
                            detail: "keep startup recovery active".to_owned(),
                        }
                    }
                })
            }
        });
        let service =
            TurnDeliveryService::start_with_delivery_router(engine.clone(), 1, router, reconciler);

        tokio::time::timeout(Duration::from_secs(1), first_send_started.notified())
            .await
            .expect("live send started");
        tokio::time::timeout(
            Duration::from_secs(1),
            second_cohort_reconciliation.notified(),
        )
        .await
        .expect("periodic startup recovery retried its unavailable cohort");
        tokio::task::yield_now().await;
        assert_eq!(
            live_reconciliations.load(Ordering::SeqCst),
            0,
            "startup recovery must exclude newly claimed live sends"
        );
        release_first_send.notify_waiters();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let delivery = engine
                    .repositories()
                    .get_provider_turn_delivery("live-send".to_owned())
                    .await
                    .expect("row")
                    .expect("delivery");
                if delivery.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("accepted live send is durably delivered");
        assert_eq!(sends.load(Ordering::SeqCst), 1, "live turn sent once");

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn accepted_delivery_repairs_same_attempt_pending_transition_conflict() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery(
            &database,
            "accepted-conflict",
            "thread-conflict",
            1,
            TurnDeliveryState::Pending,
            1,
        )
        .await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");

        persist_delivery_outcome(
            &engine,
            &CancellationToken::new(),
            TurnDeliveryTransition {
                turn_id: None,
                command_id: "accepted-conflict".to_owned(),
                expected_states: vec![TurnDeliveryState::Sending],
                expected_attempt: 1,
                next_state: TurnDeliveryState::Delivered,
                detail: None,
                updated_at: now(),
            },
            None,
        )
        .await
        .expect("accepted outcome is persisted despite the stale expected state");

        let delivery = engine
            .repositories()
            .get_provider_turn_delivery("accepted-conflict".to_owned())
            .await
            .expect("row")
            .expect("delivery");
        assert_eq!(delivery.state, TurnDeliveryState::Delivered);
        assert_eq!(delivery.attempts, 1);

        engine.shutdown().await;
    }

    #[tokio::test]
    async fn accepted_delivery_does_not_repair_a_later_pending_attempt() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery(
            &database,
            "accepted-later-attempt",
            "thread-later-attempt",
            1,
            TurnDeliveryState::Pending,
            2,
        )
        .await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");

        let error = persist_delivery_outcome(
            &engine,
            &CancellationToken::new(),
            TurnDeliveryTransition {
                turn_id: None,
                command_id: "accepted-later-attempt".to_owned(),
                expected_states: vec![TurnDeliveryState::Sending],
                expected_attempt: 1,
                next_state: TurnDeliveryState::Delivered,
                detail: None,
                updated_at: now(),
            },
            None,
        )
        .await
        .expect_err("a stale acceptance must not deliver a later attempt");
        assert!(error.contains("conflicted with durable state Pending at attempt 2"));

        let delivery = engine
            .repositories()
            .get_provider_turn_delivery("accepted-later-attempt".to_owned())
            .await
            .expect("row")
            .expect("delivery");
        assert_eq!(delivery.state, TurnDeliveryState::Pending);
        assert_eq!(delivery.attempts, 2);

        engine.shutdown().await;
    }

    #[tokio::test]
    async fn stalled_reconciliation_does_not_block_unrelated_delivery_or_shutdown() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery(
            &database,
            "stalled-recovery",
            "thread-stalled",
            1,
            TurnDeliveryState::Sending,
            1,
        )
        .await;
        seed_pending(&database, "unrelated-pending", "thread-ready", 2).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let delivered = Arc::new(Notify::new());
        let router: ProviderDeliveryRouter = Arc::new({
            let delivered = delivered.clone();
            move |_command, _delivery_key| {
                let delivered = delivered.clone();
                Box::pin(async move {
                    delivered.notify_one();
                    ProviderDeliveryOutcome::Accepted { turn_id: None }
                })
            }
        });
        let reconciliation_started = Arc::new(Notify::new());
        let reconciler: DeliveryReconciler = Arc::new({
            let reconciliation_started = reconciliation_started.clone();
            move |_row| {
                let reconciliation_started = reconciliation_started.clone();
                Box::pin(async move {
                    reconciliation_started.notify_one();
                    std::future::pending().await
                })
            }
        });
        let service =
            TurnDeliveryService::start_with_delivery_router(engine.clone(), 1, router, reconciler);

        tokio::time::timeout(Duration::from_secs(1), reconciliation_started.notified())
            .await
            .expect("startup reconciliation began");
        tokio::time::timeout(Duration::from_millis(250), delivered.notified())
            .await
            .expect("unrelated pending delivery is not starved");
        tokio::time::timeout(Duration::from_millis(250), service.shutdown())
            .await
            .expect("shutdown cancels stalled reconciliation");

        engine.shutdown().await;
    }

    #[tokio::test]
    async fn ambiguous_post_admission_outcome_persists_uncertain_without_retry() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "ambiguous", "thread-ambiguous", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let routes = Arc::new(AtomicUsize::new(0));
        let router: ProviderDeliveryRouter = Arc::new({
            let routes = routes.clone();
            move |_command, _delivery_key| {
                routes.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    ProviderDeliveryOutcome::Ambiguous {
                        detail: "connection closed after request write".to_owned(),
                    }
                })
            }
        });
        let service = TurnDeliveryService::start_with_delivery_router(
            engine.clone(),
            1,
            router,
            unavailable_reconciler(),
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery("ambiguous".to_owned())
                    .await
                    .expect("row")
                    .expect("delivery");
                if row.state == TurnDeliveryState::Uncertain {
                    assert_eq!(
                        row.last_error.as_deref(),
                        Some("connection closed after request write")
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ambiguous outcome persists as uncertain");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(routes.load(Ordering::SeqCst), 1);

        service.shutdown().await;
        engine.shutdown().await;
    }

    async fn assert_exact_recovery(
        provider_kind: &'static str,
        reconciliation: ProviderReconciliationOutcome,
        expected_total_sends: usize,
    ) {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        let command_id = format!("recover-{provider_kind}");
        let thread_id = format!("thread-{provider_kind}");
        let delivery_key = format!("stable-key-{provider_kind}");
        seed_delivery_for_provider(
            &database,
            &command_id,
            &thread_id,
            1,
            TurnDeliveryState::Sending,
            1,
            provider_kind,
            &delivery_key,
        )
        .await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let sends = Arc::new(AtomicUsize::new(1));
        let router: ProviderDeliveryRouter = Arc::new({
            let sends = sends.clone();
            let delivery_key = delivery_key.clone();
            move |_command, observed_key| {
                assert_eq!(observed_key, delivery_key);
                sends.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { ProviderDeliveryOutcome::Accepted { turn_id: None } })
            }
        });
        let reconciler: DeliveryReconciler = Arc::new({
            let delivery_key = delivery_key.clone();
            move |row| {
                assert_eq!(row.delivery_key, delivery_key);
                assert_eq!(row.provider_kind, provider_kind);
                let reconciliation = reconciliation.clone();
                Box::pin(async move { reconciliation })
            }
        });
        let service =
            TurnDeliveryService::start_with_delivery_router(engine.clone(), 1, router, reconciler);

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery(command_id.clone())
                    .await
                    .expect("row")
                    .expect("delivery");
                if row.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("recovered delivery reaches delivered");
        assert_eq!(sends.load(Ordering::SeqCst), expected_total_sends);

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn codex_exact_found_recovery_does_not_resend() {
        assert_exact_recovery("codex", ProviderReconciliationOutcome::Found, 1).await;
    }

    #[tokio::test]
    async fn codex_exact_absent_recovery_resends_once_with_same_delivery_key() {
        assert_exact_recovery("codex", ProviderReconciliationOutcome::Absent, 2).await;
    }

    #[tokio::test]
    async fn opencode_exact_found_recovery_does_not_resend() {
        assert_exact_recovery("opencode", ProviderReconciliationOutcome::Found, 1).await;
    }

    #[tokio::test]
    async fn opencode_exact_absent_recovery_resends_once_with_same_delivery_key() {
        assert_exact_recovery("opencode", ProviderReconciliationOutcome::Absent, 2).await;
    }

    async fn assert_no_id_restart_becomes_uncertain_without_a_duplicate_send(
        provider_kind: &'static str,
    ) {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        let command_id = format!("recover-no-id-{provider_kind}");
        let thread_id = format!("thread-no-id-{provider_kind}");
        seed_delivery_for_provider(
            &database,
            &command_id,
            &thread_id,
            1,
            TurnDeliveryState::Sending,
            1,
            provider_kind,
            "unused-no-id-key",
        )
        .await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let provider_send_count = Arc::new(AtomicUsize::new(1));
        let router: ProviderDeliveryRouter = Arc::new({
            let provider_send_count = provider_send_count.clone();
            move |_command, _delivery_key| {
                provider_send_count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { ProviderDeliveryOutcome::Accepted { turn_id: None } })
            }
        });
        let service = TurnDeliveryService::start_with_delivery_router(
            engine.clone(),
            1,
            router,
            unavailable_reconciler(),
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery(command_id.clone())
                    .await
                    .expect("row")
                    .expect("delivery");
                if row.state == TurnDeliveryState::Uncertain {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("no-id restart becomes uncertain");
        assert_eq!(provider_send_count.load(Ordering::SeqCst), 1);

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn claude_no_id_restart_after_write_does_not_send_again() {
        assert_no_id_restart_becomes_uncertain_without_a_duplicate_send("claudeAgent").await;
    }

    #[tokio::test]
    async fn claude_legacy_alias_no_id_restart_after_write_does_not_send_again() {
        assert_no_id_restart_becomes_uncertain_without_a_duplicate_send("claude").await;
    }

    #[tokio::test]
    async fn cursor_no_id_restart_after_write_does_not_send_again() {
        assert_no_id_restart_becomes_uncertain_without_a_duplicate_send("cursor").await;
    }

    #[tokio::test]
    async fn delivery_refills_a_free_slot_while_an_older_route_is_still_running() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "a", "thread-a", 1).await;
        seed_pending(&database, "b", "thread-b", 2).await;
        seed_pending(&database, "c", "thread-c", 3).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let a_started = Arc::new(Notify::new());
        let a_release = Arc::new(Notify::new());
        let c_started = Arc::new(Notify::new());
        let router: DeliveryRouter = Arc::new({
            let a_started = a_started.clone();
            let a_release = a_release.clone();
            let c_started = c_started.clone();
            move |command| {
                let a_started = a_started.clone();
                let a_release = a_release.clone();
                let c_started = c_started.clone();
                Box::pin(async move {
                    match command.command_id() {
                        "a" => {
                            a_started.notify_one();
                            a_release.notified().await;
                        }
                        "c" => c_started.notify_one(),
                        _ => {}
                    }
                    Ok(())
                })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 2, router);

        tokio::time::timeout(std::time::Duration::from_secs(5), a_started.notified())
            .await
            .expect("A starts");
        tokio::time::timeout(std::time::Duration::from_secs(5), c_started.notified())
            .await
            .expect("C starts after B frees a slot while A remains blocked");
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("a".to_owned())
                .await
                .expect("A row")
                .expect("A delivery")
                .state,
            TurnDeliveryState::Sending
        );

        a_release.notify_one();
        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_state_read_failure_is_fail_closed() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "pending", "thread", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        engine
            .repositories()
            .fail_provider_turn_reads_for_test(true);
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let router: DeliveryRouter = Arc::new({
            let provider_calls = provider_calls.clone();
            move |_| {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router.clone());

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            engine.repositories().provider_turn_claims_for_test(),
            0,
            "an incomplete state read must prevent every claim"
        );

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn restart_reuses_persisted_attempt_backoff_instead_of_retrying_immediately() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_delivery(
            &database,
            "restart-backoff",
            "restart-thread",
            1,
            TurnDeliveryState::Pending,
            2,
        )
        .await;
        let updated_at = now();
        database
            .call(move |connection| {
                connection.execute(
                    "UPDATE provider_turn_outbox SET updated_at = ? WHERE command_id = 'restart-backoff'",
                    [updated_at],
                )?;
                Ok(())
            })
            .await
            .expect("persist retry transition timestamp");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let read_pause = engine
            .repositories()
            .pause_after_next_provider_turn_read_for_test();
        let routes = Arc::new(AtomicUsize::new(0));
        let routed = Arc::new(Notify::new());
        let router: ProviderDeliveryRouter = Arc::new({
            let routes = routes.clone();
            let routed = routed.clone();
            move |_command, _delivery_key| {
                routes.fetch_add(1, Ordering::SeqCst);
                routed.notify_one();
                Box::pin(async { ProviderDeliveryOutcome::Accepted { turn_id: None } })
            }
        });
        let service = TurnDeliveryService::start_with_delivery_router(
            engine.clone(),
            1,
            router,
            unavailable_reconciler(),
        );

        read_pause.wait_until_entered().await;
        read_pause.release();
        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            routes.load(Ordering::SeqCst),
            0,
            "a restarted worker must honor the durable retry deadline"
        );
        tokio::time::advance(Duration::from_millis(99)).await;
        // Delivery crosses the real SQLite worker after the virtual retry timer fires.
        // Resume real time so the completion timeout cannot outrun that I/O.
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(1), routed.notified())
            .await
            .expect("persisted backoff expires");
        assert_eq!(routes.load(Ordering::SeqCst), 1);

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn permanent_bootstrap_failure_retains_backoff_across_respawns() {
        struct FailingBootstrapEffects {
            attempts: AtomicUsize,
            attempted: Notify,
        }

        impl ThreadTurnBootstrapEffects for FailingBootstrapEffects {
            fn prepare_worktree<'a>(
                &'a self,
                _input: ThreadTurnStartBootstrapPrepareWorktree,
                _cancellation: &'a CancellationToken,
            ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                self.attempted.notify_one();
                Box::pin(async { Err("permanent worktree failure".to_owned()) })
            }

            fn run_setup_script<'a>(
                &'a self,
                _input: BootstrapSetupInput,
            ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
                Box::pin(async { Ok(BootstrapSetupResult::NoScript) })
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        let payload = serde_json::json!({
            "type":"thread.turn.start", "commandId":"backoff-bootstrap",
            "threadId":"backoff-thread",
            "message":{"messageId":"backoff-message","role":"user","text":"build","attachments":[]},
            "bootstrap":{"prepareWorktree":{"projectCwd":"C:/repo","baseBranch":"main"}},
            "createdAt":"2026-08-01T00:00:00Z"
        })
        .to_string();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('backoff-bootstrap', 'thread', 'backoff-thread', '2026-08-01T00:00:00Z', 0, 'accepted', NULL, 'digest')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('backoff-bootstrap', 'backoff-thread', 'backoff-message', 'codex', 'codex', NULL, 'backoff-key', ?, 'pending', 0, NULL, '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z')",
                    [payload],
                )?;
                Ok(())
            })
            .await
            .expect("seed bootstrap delivery");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let effects = Arc::new(FailingBootstrapEffects {
            attempts: AtomicUsize::new(0),
            attempted: Notify::new(),
        });
        engine.set_bootstrap_effects(effects.clone());
        let router: DeliveryRouter = Arc::new(|_| Box::pin(async { Ok(()) }));
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router.clone());

        wait_for_count(&effects.attempts, &effects.attempted, 1).await;
        service.wait_for_retries_scheduled(1).await;
        tokio::time::advance(RETRY_BACKOFF_MIN).await;
        wait_for_count(&effects.attempts, &effects.attempted, 2).await;
        service.wait_for_retries_scheduled(2).await;
        tokio::time::advance(RETRY_BACKOFF_MIN).await;
        assert_eq!(
            effects.attempts.load(Ordering::SeqCst),
            2,
            "the second failure must retain the 100ms delay instead of resetting to 50ms"
        );
        tokio::time::advance(RETRY_BACKOFF_MIN).await;
        wait_for_count(&effects.attempts, &effects.attempted, 3).await;

        service.shutdown().await;
        engine.shutdown().await;
    }

    /// A delivery that keeps failing backs off to half a minute between attempts; capped at a
    /// second, a provider that could not start was retried every second indefinitely.
    #[test]
    fn a_failing_delivery_backs_off_to_thirty_seconds() {
        let mut retries = HashMap::new();
        let mut delays = Vec::new();
        for _ in 0..12 {
            schedule_retry(&mut retries, "command".to_owned());
            delays.push(retries["command"].next_delay);
        }
        assert_eq!(delays[0], RETRY_BACKOFF_MIN * 2);
        assert!(delays.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(delays.last(), Some(&Duration::from_secs(30)));
        // A restarted server resumes the same schedule from the persisted attempt count.
        assert_eq!(retry_delay_for_attempts(20), Duration::from_secs(30));
    }

    /// Retries that fail the same way change nothing the thread shows, so they emit no thread
    /// update: a provider that could not start updated its thread every second indefinitely.
    #[tokio::test(start_paused = true)]
    async fn repeated_identical_failures_emit_one_update() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "transient", "thread", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let mut events = engine.subscribe_events();
        let calls = Arc::new(AtomicUsize::new(0));
        let router: ProviderDeliveryRouter = Arc::new({
            let calls = calls.clone();
            move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(ready(ProviderDeliveryOutcome::DefinitelyNotSent {
                    detail: "Codex could not start: the session is restarting".to_owned(),
                }))
            }
        });
        let service = TurnDeliveryService::start_with_delivery_router(
            engine.clone(),
            1,
            router,
            Arc::new(|_| Box::pin(ready(ProviderReconciliationOutcome::Absent))),
        );
        // Each failed attempt is persisted off the runtime, so wait for its retry to be scheduled
        // before moving the clock on to the next attempt.
        for attempt in 1..=3 {
            service.wait_for_retries_scheduled(attempt).await;
            while calls.load(Ordering::SeqCst) <= attempt {
                tokio::time::advance(RETRY_BACKOFF_MIN).await;
                tokio::task::yield_now().await;
            }
        }
        service.wait_for_retries_scheduled(4).await;
        let mut updates = 0;
        while let Ok(event) = events.try_recv() {
            if event.event.event_type == "thread.turn-delivery-updated" {
                updates += 1;
            }
        }
        assert_eq!(
            updates, 1,
            "retries that change nothing must not update the thread"
        );
        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn failing_thread_backoff_escalates_while_other_threads_succeed() {
        struct GatedFailingBootstrapEffects {
            attempts: AtomicUsize,
            attempted: Notify,
            releases: Semaphore,
        }

        impl ThreadTurnBootstrapEffects for GatedFailingBootstrapEffects {
            fn prepare_worktree<'a>(
                &'a self,
                _input: ThreadTurnStartBootstrapPrepareWorktree,
                _cancellation: &'a CancellationToken,
            ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
                Box::pin(async move {
                    self.attempts.fetch_add(1, Ordering::SeqCst);
                    self.attempted.notify_one();
                    self.releases
                        .acquire()
                        .await
                        .expect("failing attempt gate")
                        .forget();
                    Err("permanent worktree failure".to_owned())
                })
            }

            fn run_setup_script<'a>(
                &'a self,
                _input: BootstrapSetupInput,
            ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
                Box::pin(async { Ok(BootstrapSetupResult::NoScript) })
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        let payload = serde_json::json!({
            "type":"thread.turn.start", "commandId":"failing-bootstrap",
            "threadId":"failing-thread",
            "message":{"messageId":"failing-message","role":"user","text":"build","attachments":[]},
            "bootstrap":{"prepareWorktree":{"projectCwd":"C:/repo","baseBranch":"main"}},
            "createdAt":"2026-08-01T00:00:01Z"
        })
        .to_string();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('failing-bootstrap', 'thread', 'failing-thread', '2026-08-01T00:00:01Z', 0, 'accepted', NULL, 'digest')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('failing-bootstrap', 'failing-thread', 'failing-message', 'codex', 'codex', NULL, 'failing-key', ?, 'pending', 0, NULL, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z')",
                    [payload],
                )?;
                Ok(())
            })
            .await
            .expect("seed failing delivery");
        for ordinal in 2..=4 {
            seed_pending(
                &database,
                &format!("healthy-{ordinal}"),
                &format!("healthy-thread-{ordinal}"),
                ordinal,
            )
            .await;
        }
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let effects = Arc::new(GatedFailingBootstrapEffects {
            attempts: AtomicUsize::new(0),
            attempted: Notify::new(),
            releases: Semaphore::new(0),
        });
        engine.set_bootstrap_effects(effects.clone());
        let healthy_routes = Arc::new(AtomicUsize::new(0));
        let healthy_started = Arc::new(Notify::new());
        let healthy_releases = Arc::new(Semaphore::new(0));
        let router: DeliveryRouter = Arc::new({
            let healthy_routes = healthy_routes.clone();
            let healthy_started = healthy_started.clone();
            let healthy_releases = healthy_releases.clone();
            move |_| {
                let healthy_routes = healthy_routes.clone();
                let healthy_started = healthy_started.clone();
                let healthy_releases = healthy_releases.clone();
                Box::pin(async move {
                    healthy_routes.fetch_add(1, Ordering::SeqCst);
                    healthy_started.notify_one();
                    healthy_releases
                        .acquire()
                        .await
                        .expect("healthy route gate")
                        .forget();
                    Ok(())
                })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 2, router);

        wait_for_count(&effects.attempts, &effects.attempted, 1).await;
        wait_for_count(&healthy_routes, &healthy_started, 1).await;
        healthy_releases.add_permits(1);
        wait_for_count(&healthy_routes, &healthy_started, 2).await;
        healthy_releases.add_permits(1);
        wait_for_count(&healthy_routes, &healthy_started, 3).await;
        healthy_releases.add_permits(1);
        effects.releases.add_permits(1);
        service.wait_for_retries_scheduled(1).await;
        tokio::time::advance(RETRY_BACKOFF_MIN).await;
        wait_for_count(&effects.attempts, &effects.attempted, 2).await;

        effects.releases.add_permits(1);
        service.wait_for_retries_scheduled(2).await;
        tokio::time::advance(RETRY_BACKOFF_MIN).await;
        assert_eq!(
            effects.attempts.load(Ordering::SeqCst),
            2,
            "a healthy thread must not reset the failing command's next delay to 50ms"
        );

        tokio::time::advance(RETRY_BACKOFF_MIN).await;
        wait_for_count(&effects.attempts, &effects.attempted, 3).await;
        effects.releases.add_permits(8);
        healthy_releases.add_permits(8);
        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn graceful_shutdown_persists_an_acknowledged_in_flight_delivery() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "shutdown-ack", "shutdown-thread", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let router: ProviderDeliveryRouter = Arc::new({
            let entered = entered.clone();
            let release = release.clone();
            move |_command, _delivery_key| {
                let entered = entered.clone();
                let release = release.clone();
                Box::pin(async move {
                    entered.notify_one();
                    release.notified().await;
                    ProviderDeliveryOutcome::Accepted {
                        turn_id: Some("accepted-before-shutdown".to_owned()),
                    }
                })
            }
        });
        let service = Arc::new(TurnDeliveryService::start_with_delivery_router(
            engine.clone(),
            1,
            router,
            unavailable_reconciler(),
        ));
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("provider delivery is admitted before shutdown");

        let shutdown = tokio::spawn({
            let service = service.clone();
            async move { service.shutdown().await }
        });
        tokio::task::yield_now().await;
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("graceful shutdown completes")
            .expect("shutdown task");

        let delivery = engine
            .repositories()
            .get_provider_turn_delivery("shutdown-ack".to_owned())
            .await
            .expect("delivery row")
            .expect("delivery");
        assert_eq!(delivery.state, TurnDeliveryState::Delivered);
        assert_eq!(delivery.attempts, 1);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn graceful_shutdown_force_aborts_stuck_delivery_only_after_its_grace_window() {
        struct DropFlag(Arc<AtomicBool>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "shutdown-stuck", "shutdown-thread", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let entered = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let router: ProviderDeliveryRouter = Arc::new({
            let entered = entered.clone();
            let dropped = dropped.clone();
            move |_command, _delivery_key| {
                let entered = entered.clone();
                let guard = DropFlag(dropped.clone());
                Box::pin(async move {
                    let _guard = guard;
                    entered.notify_one();
                    std::future::pending().await
                })
            }
        });
        let service = Arc::new(TurnDeliveryService::start_worker_with_shutdown_grace(
            engine.clone(),
            1,
            1,
            router,
            unavailable_reconciler(),
            None,
            Duration::from_millis(50),
        ));
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("stuck provider delivery starts");

        let shutdown = tokio::spawn({
            let service = service.clone();
            async move { service.shutdown().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must not abort admitted work before the grace window"
        );
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("stuck work is force-aborted within the shutdown bound")
            .expect("shutdown task");
        assert!(dropped.load(Ordering::SeqCst));
        let delivery = engine
            .repositories()
            .get_provider_turn_delivery("shutdown-stuck".to_owned())
            .await
            .expect("delivery row")
            .expect("delivery");
        assert_eq!(delivery.state, TurnDeliveryState::Sending);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_drains_cancellation_owned_worktree_cleanup_before_aborting() {
        struct CleanupOnCancellation {
            reserved_path: PathBuf,
            entered: Notify,
            cleanup_started: Notify,
            cleanup_release: Notify,
            cleanup_finished: AtomicBool,
        }

        impl ThreadTurnBootstrapEffects for CleanupOnCancellation {
            fn prepare_worktree<'a>(
                &'a self,
                _input: ThreadTurnStartBootstrapPrepareWorktree,
                cancellation: &'a CancellationToken,
            ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
                Box::pin(async move {
                    tokio::fs::create_dir_all(&self.reserved_path)
                        .await
                        .expect("reserve implicit worktree path");
                    tokio::fs::write(self.reserved_path.join(".bibcode-owner"), "owned")
                        .await
                        .expect("write ownership marker");
                    self.entered.notify_one();
                    cancellation.cancelled().await;
                    self.cleanup_started.notify_one();
                    self.cleanup_release.notified().await;
                    tokio::fs::remove_dir_all(&self.reserved_path)
                        .await
                        .expect("cleanup reserved path");
                    self.cleanup_finished.store(true, Ordering::SeqCst);
                    Err("worktree creation cancelled after cleanup".to_owned())
                })
            }

            fn run_setup_script<'a>(
                &'a self,
                _input: BootstrapSetupInput,
            ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
                Box::pin(async { Ok(BootstrapSetupResult::NoScript) })
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        let payload = serde_json::json!({
            "type":"thread.turn.start", "commandId":"cleanup-bootstrap",
            "threadId":"cleanup-thread",
            "message":{"messageId":"cleanup-message","role":"user","text":"build","attachments":[]},
            "bootstrap":{"prepareWorktree":{"projectCwd":"C:/repo","baseBranch":"main"}},
            "createdAt":"2026-08-01T00:00:00Z"
        })
        .to_string();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('cleanup-bootstrap', 'thread', 'cleanup-thread', '2026-08-01T00:00:00Z', 0, 'accepted', NULL, 'digest')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('cleanup-bootstrap', 'cleanup-thread', 'cleanup-message', 'codex', 'codex', NULL, 'cleanup-key', ?, 'pending', 0, NULL, '2026-08-01T00:00:00Z', '2026-08-01T00:00:00Z')",
                    [payload],
                )?;
                Ok(())
            })
            .await
            .expect("seed bootstrap delivery");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let reserved_root = tempfile::tempdir().expect("reserved path root");
        let reserved_path = reserved_root.path().join("reserved-worktree");
        let effects = Arc::new(CleanupOnCancellation {
            reserved_path: reserved_path.clone(),
            entered: Notify::new(),
            cleanup_started: Notify::new(),
            cleanup_release: Notify::new(),
            cleanup_finished: AtomicBool::new(false),
        });
        engine.set_bootstrap_effects(effects.clone());
        let router: DeliveryRouter = Arc::new(|_| Box::pin(async { Ok(()) }));
        let service = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            router,
        ));

        tokio::time::timeout(Duration::from_secs(5), effects.entered.notified())
            .await
            .expect("reserved worktree path is owned before shutdown");
        assert!(reserved_path.exists());
        let shutdown = tokio::spawn({
            let service = service.clone();
            async move { service.shutdown().await }
        });
        tokio::time::timeout(Duration::from_secs(3), effects.cleanup_started.notified())
            .await
            .expect("cancellation-aware cleanup gets a drain window");
        assert!(
            !shutdown.is_finished(),
            "shutdown must wait for cancellation-owned cleanup"
        );
        effects.cleanup_release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown remains bounded")
            .expect("shutdown task");
        assert!(effects.cleanup_finished.load(Ordering::SeqCst));
        assert!(!reserved_path.exists());

        engine.shutdown().await;
    }

    #[tokio::test]
    async fn pending_delivery_waits_for_seeded_sending_uncertain_and_failed_predecessors() {
        for initial_state in [
            TurnDeliveryState::Sending,
            TurnDeliveryState::Uncertain,
            TurnDeliveryState::Failed,
        ] {
            let database = Database::open_in_memory().await.expect("database");
            database
                .call(|connection| Ok(run_migrations(connection, None)?))
                .await
                .expect("migrations");
            seed_delivery(&database, "blocker", "thread", 1, initial_state, 1).await;
            seed_pending(&database, "later", "thread", 2).await;
            let engine = OrchestrationEngine::start(database, EngineOptions::default())
                .await
                .expect("engine");
            let provider_calls = Arc::new(AtomicUsize::new(0));
            let routed = Arc::new(Notify::new());
            let router: DeliveryRouter = Arc::new({
                let provider_calls = provider_calls.clone();
                let routed = routed.clone();
                move |_| {
                    provider_calls.fetch_add(1, Ordering::SeqCst);
                    routed.notify_one();
                    Box::pin(async { Ok(()) })
                }
            });
            let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router);

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
            assert_eq!(engine.repositories().provider_turn_claims_for_test(), 0);
            assert_eq!(
                engine
                    .repositories()
                    .get_provider_turn_delivery("blocker".to_owned())
                    .await
                    .expect("blocker row")
                    .expect("blocker delivery")
                    .state,
                initial_state,
                "stable Sending remains blocked until exact reconciliation succeeds"
            );
            assert_eq!(
                engine
                    .repositories()
                    .get_provider_turn_delivery("later".to_owned())
                    .await
                    .expect("later row")
                    .expect("later delivery")
                    .state,
                TurnDeliveryState::Pending
            );

            let blocker = engine
                .repositories()
                .get_provider_turn_delivery("blocker".to_owned())
                .await
                .expect("blocker row")
                .expect("blocker delivery");
            assert!(
                engine
                    .transition_turn_delivery(TurnDeliveryTransition {
                        turn_id: None,
                        command_id: blocker.command_id,
                        expected_states: vec![blocker.state],
                        expected_attempt: blocker.attempts,
                        next_state: TurnDeliveryState::Dismissed,
                        detail: Some("explicitly resolved by test".to_owned()),
                        updated_at: now(),
                    })
                    .await
                    .expect("resolve blocker")
            );
            service.wake();
            tokio::time::timeout(std::time::Duration::from_secs(5), routed.notified())
                .await
                .expect("later delivery routes after explicit resolution");

            service.shutdown().await;
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn delivery_retries_only_the_transition_after_transient_persistence_failures() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "retry-transition", "thread", 1).await;
        let hooks = TestHooks::default();
        hooks.fail_next_delivery_transitions(2);
        let engine = OrchestrationEngine::start(
            database,
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let router: DeliveryRouter = Arc::new({
            let provider_calls = provider_calls.clone();
            move |_| {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router.clone());

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let delivery = engine
                    .repositories()
                    .get_provider_turn_delivery("retry-transition".to_owned())
                    .await
                    .expect("delivery row")
                    .expect("delivery");
                if delivery.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transition eventually persists");
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(hooks.delivery_transition_attempts(), 3);

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_transition_retry_stops_on_shutdown_without_rerouting() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "cancel-transition", "thread", 1).await;
        let hooks = TestHooks::default();
        hooks.fail_next_delivery_transitions(usize::MAX);
        let engine = OrchestrationEngine::start(
            database,
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let router: DeliveryRouter = Arc::new({
            let provider_calls = provider_calls.clone();
            move |_| {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if provider_calls.load(Ordering::SeqCst) == 1
                    && hooks.delivery_transition_attempts() > 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transition retry starts");

        tokio::time::timeout(std::time::Duration::from_secs(3), service.shutdown())
            .await
            .expect("shutdown cancels transition retry");
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        let delivery = engine
            .repositories()
            .get_provider_turn_delivery("cancel-transition".to_owned())
            .await
            .expect("delivery row")
            .expect("delivery");
        assert_eq!(delivery.state, TurnDeliveryState::Sending);
        assert_eq!(delivery.attempts, 1);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_holds_capacity_until_the_running_task_completes() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "a", "thread-a", 1).await;
        seed_pending(&database, "b", "thread-b", 2).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let a_started = Arc::new(Notify::new());
        let a_release = Arc::new(Notify::new());
        let b_started = Arc::new(Notify::new());
        let router: DeliveryRouter = Arc::new({
            let a_started = a_started.clone();
            let a_release = a_release.clone();
            let b_started = b_started.clone();
            move |command| {
                let a_started = a_started.clone();
                let a_release = a_release.clone();
                let b_started = b_started.clone();
                Box::pin(async move {
                    if command.command_id() == "a" {
                        a_started.notify_one();
                        a_release.notified().await;
                    } else {
                        b_started.notify_one();
                    }
                    Ok(())
                })
            }
        });
        let service =
            TurnDeliveryService::start_with_router_and_capacity(engine.clone(), 2, 1, router);

        tokio::time::timeout(std::time::Duration::from_secs(5), a_started.notified())
            .await
            .expect("A starts");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), b_started.notified())
                .await
                .is_err(),
            "B cannot start while A owns the only capacity permit"
        );
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("b".to_owned())
                .await
                .expect("B row")
                .expect("B delivery")
                .state,
            TurnDeliveryState::Pending,
            "B cannot be claimed without capacity"
        );

        a_release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), b_started.notified())
            .await
            .expect("B starts after A releases capacity");
        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_cancellation_after_state_read_prevents_claim_and_route() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "cancel-before-claim", "thread", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let read_pause = engine
            .repositories()
            .pause_after_next_provider_turn_read_for_test();
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let router: DeliveryRouter = Arc::new({
            let provider_calls = provider_calls.clone();
            move |_| {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router);

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_pause.wait_until_entered(),
        )
        .await
        .expect("dispatcher reaches the state-read/claim boundary");
        service.shutdown.cancel();
        read_pause.release();
        service.shutdown().await;

        assert_eq!(engine.repositories().provider_turn_claims_for_test(), 0);
        assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("cancel-before-claim".to_owned())
                .await
                .expect("delivery row")
                .expect("delivery")
                .state,
            TurnDeliveryState::Pending
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn bootstrap_cancellation_during_ensure_and_after_setup_launch_restarts_pending() {
        struct PausedBootstrapEffects {
            entered: Arc<Notify>,
            release: Arc<Notify>,
            setup_entered: Arc<Notify>,
            setup_release: Arc<Notify>,
            setup_launched: AtomicBool,
            prepares: AtomicUsize,
            setups: AtomicUsize,
        }

        impl ThreadTurnBootstrapEffects for PausedBootstrapEffects {
            fn prepare_worktree<'a>(
                &'a self,
                input: ThreadTurnStartBootstrapPrepareWorktree,
                _cancellation: &'a CancellationToken,
            ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
                Box::pin(async move {
                    if self.prepares.fetch_add(1, Ordering::SeqCst) == 0 {
                        self.entered.notify_one();
                        self.release.notified().await;
                    }
                    Ok(BootstrapWorktree {
                        repository_root: input.project_cwd,
                        branch: "bibcode/bootstrap".to_owned(),
                        path: "C:/repo/.worktrees/bootstrap".to_owned(),
                        remove_branch: true,
                    })
                })
            }

            fn run_setup_script<'a>(
                &'a self,
                _input: BootstrapSetupInput,
            ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
                Box::pin(async move {
                    if !self.setup_launched.swap(true, Ordering::SeqCst) {
                        self.setups.fetch_add(1, Ordering::SeqCst);
                        self.setup_entered.notify_one();
                        self.setup_release.notified().await;
                    }
                    Ok(BootstrapSetupResult::Started {
                        script_id: "install".to_owned(),
                        script_name: "Install".to_owned(),
                        terminal_id: "setup-install".to_owned(),
                    })
                })
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        let hooks = TestHooks::default();
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");
        for command in [
            serde_json::json!({
                "type":"project.create", "commandId":"bootstrap-project",
                "projectId":"bootstrap-project", "title":"Project",
                "workspaceRoot":"C:/repo", "createdAt":"2026-08-01T00:00:00Z"
            }),
            serde_json::json!({
                "type":"thread.create", "commandId":"bootstrap-thread-create",
                "threadId":"bootstrap-thread", "projectId":"bootstrap-project",
                "title":"Thread", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:00Z"
            }),
        ] {
            engine
                .dispatch(serde_json::from_value(command).expect("command"))
                .await
                .expect("fixture dispatch");
        }
        let payload = serde_json::json!({
            "type":"thread.turn.start", "commandId":"bootstrap-pending",
            "threadId":"bootstrap-thread",
            "_bibcodeProviderRouteFingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "_bibcodeProviderRouteCwdPending":true,
            "message":{"messageId":"bootstrap-message","role":"user","text":"build","attachments":[]},
            "bootstrap":{
                "createThread":{
                    "projectId":"bootstrap-project", "title":"Thread",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access", "interactionMode":"default",
                    "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:01Z"
                },
                "prepareWorktree":{
                    "projectCwd":"C:/repo", "baseBranch":"main",
                    "branch":"bibcode/bootstrap"
                },
                "runSetupScript":true
            },
            "createdAt":"2026-08-01T00:00:01Z"
        })
        .to_string();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('bootstrap-pending', 'thread', 'bootstrap-thread', '2026-08-01T00:00:01Z', 0, 'accepted', NULL, 'digest')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('bootstrap-pending', 'bootstrap-thread', 'bootstrap-message', 'codex', 'codex', NULL, 'bootstrap-key', ?, 'pending', 0, NULL, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z')",
                    [payload],
                )?;
                Ok(())
            })
            .await
            .expect("seed bootstrap delivery");
        let effects = Arc::new(PausedBootstrapEffects {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            setup_entered: Arc::new(Notify::new()),
            setup_release: Arc::new(Notify::new()),
            setup_launched: AtomicBool::new(false),
            prepares: AtomicUsize::new(0),
            setups: AtomicUsize::new(0),
        });
        engine.set_bootstrap_effects(effects.clone());
        let routes = Arc::new(AtomicUsize::new(0));
        let router: DeliveryRouter = Arc::new({
            let routes = routes.clone();
            move |_| {
                routes.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router.clone());

        tokio::time::timeout(Duration::from_secs(5), effects.entered.notified())
            .await
            .expect("worktree ensure starts");
        let pending = engine
            .repositories()
            .get_provider_turn_delivery("bootstrap-pending".to_owned())
            .await
            .expect("delivery")
            .expect("row");
        assert_eq!(pending.state, TurnDeliveryState::Pending);
        assert_eq!(pending.attempts, 0);
        assert_eq!(engine.repositories().provider_turn_claims_for_test(), 0);
        assert_eq!(routes.load(Ordering::SeqCst), 0);

        service.shutdown.cancel();
        effects.release.notify_one();
        service.shutdown().await;
        let cancelled = engine
            .repositories()
            .get_provider_turn_delivery("bootstrap-pending".to_owned())
            .await
            .expect("delivery")
            .expect("row");
        assert_eq!(cancelled.state, TurnDeliveryState::Pending);
        assert_eq!(cancelled.attempts, 0);
        assert_eq!(routes.load(Ordering::SeqCst), 0);

        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router.clone());
        tokio::time::timeout(Duration::from_secs(5), effects.setup_entered.notified())
            .await
            .expect("setup terminal launches");
        let setup_launched = engine
            .repositories()
            .get_provider_turn_delivery("bootstrap-pending".to_owned())
            .await
            .expect("delivery")
            .expect("row");
        assert_eq!(setup_launched.state, TurnDeliveryState::Pending);
        assert_eq!(setup_launched.attempts, 0);
        assert_eq!(effects.setups.load(Ordering::SeqCst), 1);
        assert_eq!(routes.load(Ordering::SeqCst), 0);
        service.shutdown.cancel();
        effects.setup_release.notify_one();
        service.shutdown().await;
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("bootstrap-pending".to_owned())
                .await
                .expect("delivery")
                .expect("row")
                .state,
            TurnDeliveryState::Pending
        );

        let claim_pause = engine
            .repositories()
            .pause_after_next_provider_turn_claim_for_test();
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router.clone());
        tokio::time::timeout(Duration::from_secs(5), claim_pause.wait_until_entered())
            .await
            .expect("bootstrap delivery reaches its durable claim boundary");
        let claimed = engine
            .repositories()
            .get_provider_turn_delivery("bootstrap-pending".to_owned())
            .await
            .expect("delivery")
            .expect("row");
        assert_eq!(claimed.state, TurnDeliveryState::Sending);
        assert_ne!(
            claimed
                .payload
                .get("_bibcodeProviderRouteCwdPending")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "a worktree cwd must be durably finalized before the row can become Sending"
        );
        assert_ne!(
            claimed.payload["_bibcodeProviderRouteFingerprint"],
            serde_json::Value::String(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()
            ),
            "the Sending row must contain the cwd-bound fingerprint, not its admission partial"
        );
        claim_pause.release();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery("bootstrap-pending".to_owned())
                    .await
                    .expect("delivery")
                    .expect("row");
                if row.state == TurnDeliveryState::Delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bootstrap delivery completes");
        assert_eq!(
            effects.prepares.load(Ordering::SeqCst),
            2,
            "the setup completion persisted during the graceful drain and is not prepared again"
        );
        assert_eq!(effects.setups.load(Ordering::SeqCst), 1);
        assert_eq!(routes.load(Ordering::SeqCst), 1);
        let delivered = engine
            .repositories()
            .get_provider_turn_delivery("bootstrap-pending".to_owned())
            .await
            .expect("delivery")
            .expect("row");
        execute_bootstrap_prerequisites(&engine, &delivered, &CancellationToken::new())
            .await
            .expect("a durable prerequisite replay is idempotent");
        let events = engine.read_events(0).await.expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event.event_type == "thread.meta-updated")
                .count(),
            1,
            "metadata retries replay one deterministic command"
        );
        for kind in ["setup-script.requested", "setup-script.started"] {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| {
                        event.event.event_type == "thread.activity-appended"
                            && event.event.payload["activity"]["kind"] == kind
                    })
                    .count(),
                1,
                "setup activity retries replay one deterministic event"
            );
        }
        let mut activity_failure = delivered.clone();
        activity_failure.command_id = "bootstrap-activity-failure".to_owned();
        activity_failure.payload["commandId"] =
            serde_json::Value::String(activity_failure.command_id.clone());
        hooks.fail_next_projector(
            "projection.thread-activities",
            Some("thread.activity-appended"),
        );
        let claims_before = engine.repositories().provider_turn_claims_for_test();
        let routes_before = routes.load(Ordering::SeqCst);
        let completion = prepare_claim_and_deliver(
            engine.clone(),
            legacy_delivery_router(router),
            activity_failure,
            CancellationToken::new(),
            CancellationToken::new(),
        )
        .await;
        assert!(
            completion.result.is_err(),
            "setup activity persistence failure must fail the prerequisite"
        );
        assert_eq!(
            engine.repositories().provider_turn_claims_for_test(),
            claims_before,
            "activity persistence failure must precede the provider claim"
        );
        assert_eq!(routes.load(Ordering::SeqCst), routes_before);
        let thread = engine
            .repositories()
            .get_thread("bootstrap-thread".to_owned())
            .await
            .expect("thread")
            .expect("persisted thread");
        assert_eq!(thread.branch.as_deref(), Some("bibcode/bootstrap"));
        assert_eq!(
            thread.worktree_path.as_deref(),
            Some("C:/repo/.worktrees/bootstrap")
        );

        service.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_cancellation_after_claim_leaves_sending_without_routing() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| Ok(run_migrations(connection, None)?))
            .await
            .expect("migrations");
        seed_pending(&database, "cancel-after-claim", "thread", 1).await;
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        let claim_pause = engine
            .repositories()
            .pause_after_next_provider_turn_claim_for_test();
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let router: DeliveryRouter = Arc::new({
            let provider_calls = provider_calls.clone();
            move |_| {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        });
        let service = TurnDeliveryService::start_with_router(engine.clone(), 1, router);

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            claim_pause.wait_until_entered(),
        )
        .await
        .expect("dispatcher claims before spawning");
        service.shutdown.cancel();
        claim_pause.release();
        service.shutdown().await;

        assert_eq!(engine.repositories().provider_turn_claims_for_test(), 1);
        assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
        let delivery = engine
            .repositories()
            .get_provider_turn_delivery("cancel-after-claim".to_owned())
            .await
            .expect("delivery row")
            .expect("delivery");
        assert_eq!(delivery.state, TurnDeliveryState::Sending);
        assert_eq!(delivery.attempts, 1);
        engine.shutdown().await;
    }
}

#[cfg(test)]
#[path = "turn_delivery_queue_tests.rs"]
mod queue_tests;
