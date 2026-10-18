use std::{
    collections::VecDeque,
    future::Future,
    panic::AssertUnwindSafe,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{FutureExt, StreamExt, stream, stream::FuturesUnordered};
use serde_json::json;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    crypto::sha256_hex,
    git::canonical_worktree_path_key,
    orchestration::{OrchestrationCommand, OrchestrationEngine, engine::ActivityInput},
    persistence::Repositories,
    production::{
        provider_runtime::{ProviderRuntimeError, ProviderRuntimeSupervisor},
        server_terminal::ServerTerminalServices,
        worktree_catalog_rpc::{
            WorktreeRemovalCleanupAdmission, WorktreeRemovalCleanupAdmissionError,
            WorktreeRemovalCleanupAdmissionFuture, WorktreeRemovalQuiesceFuture,
            WorktreeRemovalQuiesceLease, WorktreeRemovalQuiesceRequest, WorktreeRemovalQuiescer,
        },
    },
    worktree_catalog::{
        AdoptedWorktreeAvailability, CatalogFuture, CatalogWorkspaceLossObserver, RemovalGuard,
        WorkspaceAvailabilityRegistry, WorkspaceCleanupOwnership, WorkspaceLossTransition,
        WorkspaceRemovalIdentity,
    },
};

const PRODUCTION_REAPER_CAPACITY: usize = 64;
const PRODUCTION_MAX_PARALLEL_QUIESCES: usize = 16;
const PRODUCTION_GRACEFUL_TIMEOUT: Duration = Duration::from_secs(5);
const PRODUCTION_REMOVAL_RETRY_BACKOFF: Duration = Duration::from_secs(1);

pub(crate) type WorktreeRuntimeFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub(crate) trait WorktreeRuntimeActions: Send + Sync + 'static {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        Box::pin(async { Err("removal provider liveness lookup is unavailable".to_owned()) })
    }

    fn affected_thread_ids(
        &self,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        Box::pin(async move { Ok(vec![transition.thread_id]) })
    }
    fn stop_provider(
        &self,
        thread_id: String,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<(), String>>;
    fn close_terminals(
        &self,
        thread_id: String,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<(), String>>;
    fn append_warning(
        &self,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<(), String>>;

    fn affected_thread_ids_for_removal(
        &self,
        request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        Box::pin(async move { Ok(vec![request.identity().thread_id().to_owned()]) })
    }

    fn stop_provider_for_removal(
        &self,
        _thread_id: String,
        _identity: WorkspaceRemovalIdentity,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        Box::pin(async { Err("removal provider quiesce is unavailable".to_owned()) })
    }

    fn close_terminals_for_removal(
        &self,
        _thread_id: String,
        _identity: WorkspaceRemovalIdentity,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        Box::pin(async { Err("removal terminal quiesce is unavailable".to_owned()) })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct WorktreeRuntimeOptions {
    graceful_timeout: Duration,
    reaper_capacity: usize,
    max_parallel_quiesces: usize,
}

impl Default for WorktreeRuntimeOptions {
    fn default() -> Self {
        Self {
            graceful_timeout: PRODUCTION_GRACEFUL_TIMEOUT,
            reaper_capacity: PRODUCTION_REAPER_CAPACITY,
            max_parallel_quiesces: PRODUCTION_MAX_PARALLEL_QUIESCES,
        }
    }
}

#[derive(Clone)]
pub(crate) struct WorktreeRuntime {
    inner: Arc<Inner>,
}

struct Inner {
    actions: Arc<dyn WorktreeRuntimeActions>,
    registry: WorkspaceAvailabilityRegistry,
    options: WorktreeRuntimeOptions,
    reaper_sender: mpsc::Sender<ReaperJob>,
    removal_reaper_sender: mpsc::Sender<RemovalReaperJob>,
    removal_cleanup_slots: Arc<Semaphore>,
    quiesce_permits: Arc<Semaphore>,
    active_reaper_jobs: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    reaper: Mutex<Option<JoinHandle<()>>>,
}

enum ReaperJob {
    Loss(LossReaperJob),
    Removal(RemovalReaperJob),
}

struct LossReaperJob {
    transition: WorkspaceLossTransition,
    actions: Arc<dyn WorktreeRuntimeActions>,
    max_parallel: usize,
    ownership: WorkspaceCleanupOwnership,
}

struct RemovalReaperJob {
    _admission: WorktreeRemovalCleanupAdmission,
    request: WorktreeRemovalQuiesceRequest,
    _guard: RemovalGuard,
    actions: Arc<dyn WorktreeRuntimeActions>,
    affected_thread_ids: Vec<String>,
    max_parallel: usize,
    cancellation: CancellationToken,
}

type CleanupFuture = WorktreeRuntimeFuture<Result<CleanupResult, String>>;

struct CleanupResult {
    provider: Result<(), String>,
    terminals: Result<(), String>,
}

impl WorktreeRuntime {
    pub(crate) fn start(
        orchestration: OrchestrationEngine,
        provider: Arc<ProviderRuntimeSupervisor>,
        terminals: ServerTerminalServices,
        registry: WorkspaceAvailabilityRegistry,
    ) -> Self {
        Self::start_inner(
            Arc::new(ProductionWorktreeRuntimeActions {
                orchestration,
                provider,
                terminals,
                registry: registry.clone(),
            }),
            registry,
            WorktreeRuntimeOptions::default(),
        )
    }

    #[cfg(test)]
    fn start_for_test(
        actions: Arc<dyn WorktreeRuntimeActions>,
        registry: WorkspaceAvailabilityRegistry,
        options: WorktreeRuntimeOptions,
    ) -> Self {
        Self::start_inner(actions, registry, options)
    }

    fn start_inner(
        actions: Arc<dyn WorktreeRuntimeActions>,
        registry: WorkspaceAvailabilityRegistry,
        options: WorktreeRuntimeOptions,
    ) -> Self {
        let (reaper_sender, reaper_receiver) = mpsc::channel(options.reaper_capacity.max(1));
        // The lifetime slots make `try_send` capacity exact: every queued job owns one slot.
        let (removal_reaper_sender, removal_reaper_receiver) =
            mpsc::channel(options.reaper_capacity.max(1));
        let shutdown = CancellationToken::new();
        let reaper_shutdown = shutdown.clone();
        let reaper_registry = registry.clone();
        let quiesce_permits = Arc::new(Semaphore::new(options.max_parallel_quiesces.max(1)));
        let removal_cleanup_slots = Arc::new(Semaphore::new(options.reaper_capacity.max(1)));
        let reaper_permits = quiesce_permits.clone();
        let active_reaper_jobs = Arc::new(AtomicUsize::new(0));
        let reaper_active_jobs = active_reaper_jobs.clone();
        let reaper = tokio::spawn(async move {
            run_reaper(
                reaper_receiver,
                removal_reaper_receiver,
                reaper_registry,
                reaper_shutdown,
                reaper_permits,
                reaper_active_jobs,
                options,
            )
            .await;
        });
        Self {
            inner: Arc::new(Inner {
                actions,
                registry,
                options,
                reaper_sender,
                removal_reaper_sender,
                removal_cleanup_slots,
                quiesce_permits,
                active_reaper_jobs,
                shutdown,
                reaper: Mutex::new(Some(reaper)),
            }),
        }
    }

    pub(crate) async fn observe(&self, transitions: Vec<WorkspaceLossTransition>) {
        stream::iter(transitions)
            .for_each_concurrent(
                self.inner.options.max_parallel_quiesces.max(1),
                |transition| async move {
                    let Ok(_permit) = self.inner.quiesce_permits.acquire().await else {
                        return;
                    };
                    self.quiesce(transition).await;
                },
            )
            .await;
    }

    async fn quiesce(&self, transition: WorkspaceLossTransition) {
        let thread_id = transition.thread_id.clone();
        let deadline = tokio::time::Instant::now() + self.inner.options.graceful_timeout;
        let max_parallel = self.inner.options.max_parallel_quiesces.max(1);
        let canonical_cleanup = tokio::time::timeout_at(
            deadline,
            cleanup_attempt(
                self.inner.actions.clone(),
                vec![thread_id.clone()],
                max_parallel,
                transition.clone(),
            ),
        );
        let alias_cleanup = async {
            let mut affected_thread_ids = match tokio::time::timeout_at(
                deadline,
                self.inner.actions.affected_thread_ids(transition.clone()),
            )
            .await
            {
                Ok(Ok(thread_ids)) => thread_ids,
                Ok(Err(error)) => {
                    tracing::warn!(%thread_id, %error, "failed to resolve workspace thread aliases");
                    return (false, Ok(empty_cleanup_result()));
                }
                Err(_) => {
                    tracing::warn!(%thread_id, "workspace thread alias resolution timed out");
                    return (false, Ok(empty_cleanup_result()));
                }
            };
            affected_thread_ids.sort();
            affected_thread_ids.dedup();
            affected_thread_ids.retain(|affected_thread_id| affected_thread_id != &thread_id);
            if affected_thread_ids.is_empty() {
                return (true, Ok(empty_cleanup_result()));
            }
            let cleanup = cleanup_attempt(
                self.inner.actions.clone(),
                affected_thread_ids,
                max_parallel,
                transition.clone(),
            );
            match tokio::time::timeout_at(deadline, cleanup).await {
                Ok(result) => (true, result),
                Err(_) => (true, Err("workspace alias cleanup timed out".to_owned())),
            }
        };

        let (warning_result, canonical_cleanup_result, (aliases_resolved, alias_cleanup_result)) = tokio::join!(
            tokio::time::timeout_at(
                deadline,
                self.inner.actions.append_warning(transition.clone()),
            ),
            canonical_cleanup,
            alias_cleanup,
        );
        let canonical_cleanup_result = canonical_cleanup_result
            .unwrap_or_else(|_| Err("canonical workspace cleanup timed out".to_owned()));

        match warning_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%thread_id, %error, "failed to append workspace-unavailable activity");
            }
            Err(_) => {
                tracing::warn!(%thread_id, "workspace-unavailable activity persistence timed out");
            }
        }
        log_cleanup_outcome(&thread_id, &canonical_cleanup_result);
        log_cleanup_outcome(&thread_id, &alias_cleanup_result);

        if aliases_resolved
            && cleanup_outcome_succeeded(&canonical_cleanup_result)
            && cleanup_outcome_succeeded(&alias_cleanup_result)
        {
            if self.inner.registry.has_transition_admissions(&transition)
                && tokio::time::timeout_at(
                    deadline,
                    self.inner
                        .registry
                        .wait_for_transition_admissions(&transition),
                )
                .await
                .is_err()
            {
                self.enqueue_cleanup_retry(transition, max_parallel).await;
            }
        } else {
            self.enqueue_cleanup_retry(transition, max_parallel).await;
        }
    }

    async fn enqueue_cleanup_retry(
        &self,
        transition: WorkspaceLossTransition,
        max_parallel: usize,
    ) {
        let thread_id = transition.thread_id.clone();
        let Some(ownership) = self.inner.registry.begin_orphan_cleanup(&transition) else {
            return;
        };
        let job = ReaperJob::Loss(LossReaperJob {
            transition,
            actions: self.inner.actions.clone(),
            max_parallel,
            ownership,
        });
        if let Err(error) = self.inner.reaper_sender.try_send(job) {
            tracing::warn!(
                %thread_id,
                error = %error,
                capacity = self.inner.options.reaper_capacity,
                "workspace cleanup reaper is saturated; queued cleanup cancelled"
            );
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        let reaper = self
            .inner
            .reaper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(reaper) = reaper
            && let Err(error) = reaper.await
        {
            tracing::warn!(%error, "workspace cleanup reaper task failed during shutdown");
        }
        debug_assert_eq!(self.inner.active_reaper_jobs.load(Ordering::SeqCst), 0);
    }

    #[cfg(test)]
    fn active_reaper_jobs(&self) -> usize {
        self.inner.active_reaper_jobs.load(Ordering::SeqCst)
    }
}

impl CatalogWorkspaceLossObserver for WorktreeRuntime {
    fn observe(&self, transitions: Vec<WorkspaceLossTransition>) -> CatalogFuture<()> {
        let runtime = self.clone();
        Box::pin(async move {
            runtime.observe(transitions).await;
        })
    }
}

impl WorktreeRemovalQuiescer for WorktreeRuntime {
    fn live_session_thread_ids(
        &self,
        thread_ids: Vec<String>,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        self.inner.actions.live_session_thread_ids(thread_ids)
    }

    fn admit_cleanup(&self) -> WorktreeRemovalCleanupAdmissionFuture {
        let slots = self.inner.removal_cleanup_slots.clone();
        Box::pin(async move {
            slots
                .try_acquire_owned()
                .map(WorktreeRemovalCleanupAdmission::retaining)
                .map_err(|_| WorktreeRemovalCleanupAdmissionError::Capacity)
        })
    }

    fn quiesce(
        &self,
        admission: WorktreeRemovalCleanupAdmission,
        request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        let runtime = self.clone();
        Box::pin(async move { runtime.quiesce_removal(admission, request).await })
    }
}

impl WorktreeRuntime {
    async fn quiesce_removal(
        &self,
        admission: WorktreeRemovalCleanupAdmission,
        request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceLease {
        let identity = request.identity().clone();
        let deadline = tokio::time::Instant::now() + self.inner.options.graceful_timeout;
        let permit =
            match tokio::time::timeout_at(deadline, self.inner.quiesce_permits.acquire()).await {
                Ok(Ok(permit)) => permit,
                _ => {
                    return self
                        .enqueue_removal_retry(admission, request, Vec::new())
                        .await;
                }
            };
        if !self.inner.registry.removal_is_current(&identity) {
            return WorktreeRemovalQuiesceLease::complete();
        }
        let (mut affected, aliases_resolved) = match tokio::time::timeout_at(
            deadline,
            self.inner
                .actions
                .affected_thread_ids_for_removal(request.clone()),
        )
        .await
        {
            Ok(Ok(mut affected)) => {
                affected.push(identity.thread_id().to_owned());
                affected.sort();
                affected.dedup();
                (affected, true)
            }
            _ => (vec![identity.thread_id().to_owned()], false),
        };
        affected.sort();
        affected.dedup();
        let cleanup = cleanup_removal_attempt(
            self.inner.actions.clone(),
            affected.clone(),
            self.inner.options.max_parallel_quiesces.max(1),
            identity.clone(),
        );
        let succeeded = matches!(
            tokio::time::timeout_at(deadline, cleanup).await,
            Ok(Ok(result)) if cleanup_succeeded(&result)
        );
        let admissions_drained =
            if succeeded && self.inner.registry.has_removal_admissions(&identity) {
                tokio::time::timeout_at(
                    deadline,
                    self.inner.registry.wait_for_removal_admissions(&identity),
                )
                .await
                .is_ok()
            } else {
                true
            };
        drop(permit);
        if aliases_resolved && succeeded && admissions_drained {
            WorktreeRemovalQuiesceLease::complete()
        } else {
            self.enqueue_removal_retry(admission, request, affected)
                .await
        }
    }

    async fn enqueue_removal_retry(
        &self,
        admission: WorktreeRemovalCleanupAdmission,
        mut request: WorktreeRemovalQuiesceRequest,
        mut affected_thread_ids: Vec<String>,
    ) -> WorktreeRemovalQuiesceLease {
        let identity = request.identity().clone();
        affected_thread_ids.extend(request.known_thread_ids().iter().cloned());
        if affected_thread_ids.is_empty() {
            affected_thread_ids.push(identity.thread_id().to_owned());
        }
        affected_thread_ids.sort();
        affected_thread_ids.dedup();
        let Some(guard) = self.inner.registry.retain_removal(&identity) else {
            return WorktreeRemovalQuiesceLease::complete();
        };
        let retry_identity = guard.identity();
        request.replace_identity(retry_identity);
        let cancellation = CancellationToken::new();
        let job = RemovalReaperJob {
            _admission: admission,
            request,
            _guard: guard,
            actions: self.inner.actions.clone(),
            affected_thread_ids,
            max_parallel: self.inner.options.max_parallel_quiesces.max(1),
            cancellation: cancellation.clone(),
        };
        if self.inner.shutdown.is_cancelled() {
            return WorktreeRemovalQuiesceLease::complete();
        }
        match self.inner.removal_reaper_sender.try_send(job) {
            Ok(()) => WorktreeRemovalQuiesceLease::pending(cancellation),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    capacity = self.inner.options.reaper_capacity,
                    "workspace removal cleanup reaper is unavailable"
                );
                WorktreeRemovalQuiesceLease::complete()
            }
        }
    }
}

async fn run_reaper(
    mut receiver: mpsc::Receiver<ReaperJob>,
    mut removal_receiver: mpsc::Receiver<RemovalReaperJob>,
    registry: WorkspaceAvailabilityRegistry,
    shutdown: CancellationToken,
    permits: Arc<Semaphore>,
    active_jobs: Arc<AtomicUsize>,
    options: WorktreeRuntimeOptions,
) {
    let capacity = options.reaper_capacity.max(1);
    let attempt_timeout = options.graceful_timeout;
    let mut running = FuturesUnordered::new();
    let mut retries = VecDeque::new();
    let mut next_source = 0_usize;
    loop {
        while running.len() < capacity {
            let next = next_reaper_job(
                &mut next_source,
                &mut retries,
                &mut receiver,
                &mut removal_receiver,
            );
            let Some(job) = next else {
                break;
            };
            running.push(run_reaper_job(
                job,
                registry.clone(),
                shutdown.clone(),
                permits.clone(),
                active_jobs.clone(),
                attempt_timeout,
                PRODUCTION_REMOVAL_RETRY_BACKOFF,
            ));
        }
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                receiver.close();
                while receiver.try_recv().is_ok() {}
                removal_receiver.close();
                while removal_receiver.try_recv().is_ok() {}
                drop(running);
                return;
            }
            completed = running.next(), if !running.is_empty() => {
                if let Some(Some(job)) = completed {
                    retries.push_back(job);
                }
            }
            job = receiver.recv(), if running.len() < capacity && retries.is_empty() => match job {
                Some(job) => running.push(run_reaper_job(
                    job,
                    registry.clone(),
                    shutdown.clone(),
                    permits.clone(),
                    active_jobs.clone(),
                    attempt_timeout,
                    PRODUCTION_REMOVAL_RETRY_BACKOFF,
                )),
                None if running.is_empty() => return,
                None => {}
            },
            job = removal_receiver.recv(), if running.len() < capacity && retries.is_empty() => match job {
                Some(job) => running.push(run_reaper_job(
                    ReaperJob::Removal(job),
                    registry.clone(),
                    shutdown.clone(),
                    permits.clone(),
                    active_jobs.clone(),
                    attempt_timeout,
                    PRODUCTION_REMOVAL_RETRY_BACKOFF,
                )),
                None if running.is_empty() => return,
                None => {}
            },
        }
    }
}

fn next_reaper_job(
    next_source: &mut usize,
    retries: &mut VecDeque<ReaperJob>,
    receiver: &mut mpsc::Receiver<ReaperJob>,
    removal_receiver: &mut mpsc::Receiver<RemovalReaperJob>,
) -> Option<ReaperJob> {
    for _ in 0..3 {
        let source = *next_source;
        *next_source = (*next_source + 1) % 3;
        let job = match source {
            0 => retries.pop_front(),
            1 => receiver.try_recv().ok(),
            _ => removal_receiver.try_recv().ok().map(ReaperJob::Removal),
        };
        if job.is_some() {
            return job;
        }
    }
    None
}

fn run_reaper_job(
    job: ReaperJob,
    registry: WorkspaceAvailabilityRegistry,
    shutdown: CancellationToken,
    permits: Arc<Semaphore>,
    active_jobs: Arc<AtomicUsize>,
    attempt_timeout: Duration,
    removal_retry_backoff: Duration,
) -> WorktreeRuntimeFuture<Option<ReaperJob>> {
    match job {
        ReaperJob::Loss(job) => Box::pin(async move {
            run_loss_reaper_job(
                job,
                registry,
                shutdown,
                permits,
                active_jobs,
                attempt_timeout,
            )
            .await;
            None
        }),
        ReaperJob::Removal(job) => run_removal_reaper_job(
            job,
            registry,
            shutdown,
            permits,
            active_jobs,
            attempt_timeout,
            removal_retry_backoff,
        ),
    }
}

fn run_loss_reaper_job(
    job: LossReaperJob,
    registry: WorkspaceAvailabilityRegistry,
    shutdown: CancellationToken,
    permits: Arc<Semaphore>,
    active_jobs: Arc<AtomicUsize>,
    attempt_timeout: Duration,
) -> WorktreeRuntimeFuture<()> {
    Box::pin(async move {
        let _active = ActiveReaperJob::new(active_jobs);
        if !registry.cleanup_is_current(&job.ownership) {
            return;
        }
        let _permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            () = job.ownership.cancelled() => return,
            permit = permits.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return,
            },
        };
        let deadline = tokio::time::Instant::now() + attempt_timeout;
        let attempt = async {
            let mut affected_thread_ids = job
                .actions
                .affected_thread_ids(job.transition.clone())
                .await?;
            affected_thread_ids.sort();
            affected_thread_ids.dedup();
            cleanup_attempt(
                job.actions.clone(),
                affected_thread_ids,
                job.max_parallel,
                job.transition.clone(),
            )
            .await
        };
        let result = tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            () = job.ownership.cancelled() => return,
            result = tokio::time::timeout_at(deadline, attempt) => result,
        };
        match result {
            Ok(Ok(cleanup))
                if cleanup_succeeded(&cleanup)
                    && !registry.has_transition_admissions(&job.transition)
                    && registry.cleanup_is_current(&job.ownership) =>
            {
                log_cleanup_result(&job.transition.thread_id, &cleanup);
                registry.complete_orphan_cleanup(&job.ownership);
            }
            Ok(Ok(cleanup)) => log_cleanup_result(&job.transition.thread_id, &cleanup),
            Ok(Err(error)) => tracing::warn!(
                thread_id = %job.transition.thread_id,
                %error,
                "reaped workspace cleanup task failed"
            ),
            Err(_) => tracing::warn!(
                thread_id = %job.transition.thread_id,
                timeout_ms = attempt_timeout.as_millis(),
                "reaped workspace cleanup attempt timed out"
            ),
        }
    })
}

fn run_removal_reaper_job(
    mut job: RemovalReaperJob,
    registry: WorkspaceAvailabilityRegistry,
    shutdown: CancellationToken,
    permits: Arc<Semaphore>,
    active_jobs: Arc<AtomicUsize>,
    attempt_timeout: Duration,
    retry_backoff: Duration,
) -> WorktreeRuntimeFuture<Option<ReaperJob>> {
    Box::pin(async move {
        let _active = ActiveReaperJob::new(active_jobs);
        let _permit = tokio::select! {
            biased;
            () = shutdown.cancelled() => return None,
            () = job.cancellation.cancelled() => return None,
            permit = permits.acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => return None,
            },
        };
        let attempt = async {
            let mut resolved = job
                .actions
                .affected_thread_ids_for_removal(job.request.clone())
                .await?;
            resolved.extend(job.affected_thread_ids.iter().cloned());
            resolved.push(job.request.identity().thread_id().to_owned());
            resolved.sort();
            resolved.dedup();
            job.affected_thread_ids = resolved;
            cleanup_removal_attempt(
                job.actions.clone(),
                job.affected_thread_ids.clone(),
                job.max_parallel,
                job.request.identity().clone(),
            )
            .await
        };
        let succeeded = match tokio::select! {
            biased;
            () = shutdown.cancelled() => return None,
            () = job.cancellation.cancelled() => return None,
            result = tokio::time::timeout(attempt_timeout, attempt) => result,
        } {
            Ok(Ok(result)) if cleanup_succeeded(&result) => {
                log_cleanup_result(job.request.identity().thread_id(), &result);
                true
            }
            Ok(Ok(result)) => {
                log_cleanup_result(job.request.identity().thread_id(), &result);
                false
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    thread_id = job.request.identity().thread_id(),
                    %error,
                    "reaped workspace removal cleanup failed"
                );
                false
            }
            Err(_) => {
                tracing::warn!(
                    thread_id = job.request.identity().thread_id(),
                    timeout_ms = attempt_timeout.as_millis(),
                    "reaped workspace removal cleanup timed out"
                );
                false
            }
        };
        let admissions_drained =
            if succeeded && registry.has_removal_admissions(job.request.identity()) {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return None,
                    () = job.cancellation.cancelled() => return None,
                    result = tokio::time::timeout(
                        attempt_timeout,
                        registry.wait_for_removal_admissions(job.request.identity()),
                    ) => result.is_ok(),
                }
            } else {
                succeeded
            };
        if succeeded && admissions_drained {
            None
        } else {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => None,
                () = job.cancellation.cancelled() => None,
                () = tokio::time::sleep(retry_backoff) => Some(ReaperJob::Removal(job)),
            }
        }
    })
}

struct ActiveReaperJob {
    active_jobs: Arc<AtomicUsize>,
}

impl ActiveReaperJob {
    fn new(active_jobs: Arc<AtomicUsize>) -> Self {
        active_jobs.fetch_add(1, Ordering::SeqCst);
        Self { active_jobs }
    }
}

impl Drop for ActiveReaperJob {
    fn drop(&mut self) {
        self.active_jobs.fetch_sub(1, Ordering::SeqCst);
    }
}

fn cleanup_attempt(
    actions: Arc<dyn WorktreeRuntimeActions>,
    affected_thread_ids: Vec<String>,
    max_parallel: usize,
    transition: WorkspaceLossTransition,
) -> CleanupFuture {
    Box::pin(
        AssertUnwindSafe(async move {
            let results = stream::iter(affected_thread_ids)
                .map(|affected_thread_id| {
                    let actions = actions.clone();
                    let transition = transition.clone();
                    async move {
                        let (provider, terminals) = tokio::join!(
                            actions.stop_provider(affected_thread_id.clone(), transition.clone()),
                            actions.close_terminals(affected_thread_id.clone(), transition),
                        );
                        (affected_thread_id, provider, terminals)
                    }
                })
                .buffer_unordered(max_parallel.max(1))
                .collect::<Vec<_>>()
                .await;
            let mut provider_errors = Vec::new();
            let mut terminal_errors = Vec::new();
            for (affected_thread_id, provider, terminals) in results {
                if let Err(error) = provider {
                    provider_errors.push(format!("{affected_thread_id}: {error}"));
                }
                if let Err(error) = terminals {
                    terminal_errors.push(format!("{affected_thread_id}: {error}"));
                }
            }
            CleanupResult {
                provider: aggregate_cleanup_errors(provider_errors),
                terminals: aggregate_cleanup_errors(terminal_errors),
            }
        })
        .catch_unwind()
        .map(|result| result.map_err(panic_message)),
    )
}

fn cleanup_removal_attempt(
    actions: Arc<dyn WorktreeRuntimeActions>,
    affected_thread_ids: Vec<String>,
    max_parallel: usize,
    identity: WorkspaceRemovalIdentity,
) -> CleanupFuture {
    Box::pin(
        AssertUnwindSafe(async move {
            let results = stream::iter(affected_thread_ids)
                .map(|affected_thread_id| {
                    let actions = actions.clone();
                    let identity = identity.clone();
                    async move {
                        let (provider, terminals) = tokio::join!(
                            actions.stop_provider_for_removal(
                                affected_thread_id.clone(),
                                identity.clone(),
                            ),
                            actions
                                .close_terminals_for_removal(affected_thread_id.clone(), identity,),
                        );
                        (affected_thread_id, provider, terminals)
                    }
                })
                .buffer_unordered(max_parallel.max(1))
                .collect::<Vec<_>>()
                .await;
            let mut provider_errors = Vec::new();
            let mut terminal_errors = Vec::new();
            for (thread_id, provider, terminals) in results {
                if let Err(error) = provider {
                    provider_errors.push(format!("{thread_id}: {error}"));
                }
                if let Err(error) = terminals {
                    terminal_errors.push(format!("{thread_id}: {error}"));
                }
            }
            CleanupResult {
                provider: aggregate_cleanup_errors(provider_errors),
                terminals: aggregate_cleanup_errors(terminal_errors),
            }
        })
        .catch_unwind()
        .map(|result| result.map_err(panic_message)),
    )
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("workspace cleanup panicked")
        .to_owned()
}

fn cleanup_succeeded(result: &CleanupResult) -> bool {
    result.provider.is_ok() && result.terminals.is_ok()
}

fn empty_cleanup_result() -> CleanupResult {
    CleanupResult {
        provider: Ok(()),
        terminals: Ok(()),
    }
}

fn cleanup_outcome_succeeded(result: &Result<CleanupResult, String>) -> bool {
    result.as_ref().is_ok_and(cleanup_succeeded)
}

fn log_cleanup_outcome(thread_id: &str, result: &Result<CleanupResult, String>) {
    match result {
        Ok(result) => log_cleanup_result(thread_id, result),
        Err(error) => {
            tracing::warn!(%thread_id, %error, "workspace cleanup task failed");
        }
    }
}

fn log_cleanup_result(thread_id: &str, result: &CleanupResult) {
    if let Err(error) = &result.provider {
        tracing::warn!(%thread_id, %error, "provider cleanup failed for unavailable workspace");
    }
    if let Err(error) = &result.terminals {
        tracing::warn!(%thread_id, %error, "terminal cleanup failed for unavailable workspace");
    }
}

fn aggregate_cleanup_errors(errors: Vec<String>) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

struct ProductionWorktreeRuntimeActions {
    orchestration: OrchestrationEngine,
    provider: Arc<ProviderRuntimeSupervisor>,
    terminals: ServerTerminalServices,
    registry: WorkspaceAvailabilityRegistry,
}

impl WorktreeRuntimeActions for ProductionWorktreeRuntimeActions {
    fn live_session_thread_ids(
        &self,
        thread_ids: Vec<String>,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        let provider = self.provider.clone();
        Box::pin(async move {
            let mut live_thread_ids = Vec::new();
            for thread_id in thread_ids {
                match provider.capture_session_identity(&thread_id).await {
                    Ok(Some(_)) => live_thread_ids.push(thread_id),
                    Ok(None) | Err(ProviderRuntimeError::SessionNotFound { .. }) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(live_thread_ids)
        })
    }

    fn affected_thread_ids(
        &self,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        let repositories = self.orchestration.repositories();
        Box::pin(async move { workspace_thread_ids(repositories, &transition).await })
    }

    fn stop_provider(
        &self,
        thread_id: String,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        Box::pin(async move {
            if !registry.transition_is_current(&transition) {
                return Ok(());
            }
            let identity = match provider.capture_session_identity(&thread_id).await {
                Ok(identity) => identity,
                Err(ProviderRuntimeError::SessionNotFound { .. }) => None,
                Err(error) => return Err(error.to_string()),
            };
            if !registry.transition_is_current(&transition) {
                return Ok(());
            }
            match provider
                .settle_session_after_workspace_loss(thread_id, identity)
                .await
            {
                Ok(()) | Err(ProviderRuntimeError::SessionNotFound { .. }) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn close_terminals(
        &self,
        thread_id: String,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        let terminals = self.terminals.clone();
        let registry = self.registry.clone();
        Box::pin(async move {
            quiesce_terminals_for_transition(&terminals, &registry, &thread_id, &transition).await
        })
    }

    fn append_warning(
        &self,
        transition: WorkspaceLossTransition,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        let orchestration = self.orchestration.clone();
        Box::pin(async move { append_workspace_warning(&orchestration, transition).await })
    }

    fn affected_thread_ids_for_removal(
        &self,
        request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
        let repositories = self.orchestration.repositories();
        Box::pin(async move { removal_thread_ids(repositories, &request).await })
    }

    fn stop_provider_for_removal(
        &self,
        thread_id: String,
        identity: WorkspaceRemovalIdentity,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        let provider = self.provider.clone();
        let registry = self.registry.clone();
        Box::pin(async move {
            if !registry.removal_is_current(&identity) {
                return Ok(());
            }
            let session = match provider.capture_session_identity(&thread_id).await {
                Ok(session) => session,
                Err(ProviderRuntimeError::SessionNotFound { .. }) => None,
                Err(error) => return Err(error.to_string()),
            };
            if !registry.removal_is_current(&identity) {
                return Ok(());
            }
            match session {
                Some(session) => match provider
                    .suspend_session_for_removal_if_current(session)
                    .await
                {
                    Ok(()) | Err(ProviderRuntimeError::SessionNotFound { .. }) => Ok(()),
                    Err(error) => Err(error.to_string()),
                },
                None => Ok(()),
            }
        })
    }

    fn close_terminals_for_removal(
        &self,
        thread_id: String,
        identity: WorkspaceRemovalIdentity,
    ) -> WorktreeRuntimeFuture<Result<(), String>> {
        let terminals = self.terminals.clone();
        let registry = self.registry.clone();
        Box::pin(async move {
            if !registry.removal_is_current(&identity) {
                return Ok(());
            }
            let terminals_to_close = terminals
                .capture_thread_terminal_identities(&thread_id)
                .await;
            let Some(_signal) = registry.begin_removal_terminal_signal(&identity).await else {
                return Ok(());
            };
            terminals
                .quiesce_terminal_identities_for_workspace_loss(terminals_to_close)
                .await
        })
    }
}

async fn quiesce_terminals_for_transition(
    terminals: &ServerTerminalServices,
    registry: &WorkspaceAvailabilityRegistry,
    thread_id: &str,
    transition: &WorkspaceLossTransition,
) -> Result<(), String> {
    if !registry.transition_is_current(transition) {
        return Ok(());
    }
    let identities = terminals
        .capture_thread_terminal_identities(thread_id)
        .await;
    let Some(_signal_permit) = registry.begin_terminal_signal(transition).await else {
        return Ok(());
    };
    terminals
        .quiesce_terminal_identities_for_workspace_loss(identities)
        .await
}

async fn append_workspace_warning(
    orchestration: &OrchestrationEngine,
    transition: WorkspaceLossTransition,
) -> Result<(), String> {
    let activity_id = workspace_warning_activity_id(&transition);
    let created_at = now_iso();
    orchestration
        .dispatch(OrchestrationCommand::ThreadActivityAppend {
            command_id: format!("server:{activity_id}"),
            thread_id: transition.thread_id.clone(),
            activity: ActivityInput {
                id: activity_id,
                tone: "warning".to_owned(),
                kind: "workspace-unavailable".to_owned(),
                summary: "Workspace unavailable; live provider and terminal sessions were stopped."
                    .to_owned(),
                payload: json!({
                    "availability": transition.availability,
                    "path": transition.path,
                }),
                turn_id: None,
                sequence: None,
                created_at: created_at.clone(),
            },
            created_at,
        })
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn workspace_warning_activity_id(transition: &WorkspaceLossTransition) -> String {
    let availability = availability_token(transition.availability);
    let token = sha256_hex(
        format!(
            "{}\0{}\0{}\0{}",
            transition.thread_id, transition.repository_key, transition.generation, availability,
        )
        .as_bytes(),
    );
    format!("workspace-loss:{token}")
}

fn availability_token(availability: AdoptedWorktreeAvailability) -> &'static str {
    match availability {
        AdoptedWorktreeAvailability::Present => "present",
        AdoptedWorktreeAvailability::MissingRegistered => "missing-registered",
        AdoptedWorktreeAvailability::MissingUnregistered => "missing-unregistered",
        AdoptedWorktreeAvailability::VerificationUnavailable => "verification-unavailable",
        AdoptedWorktreeAvailability::Removing => "removing",
    }
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

async fn workspace_thread_ids(
    repositories: Repositories,
    transition: &WorkspaceLossTransition,
) -> Result<Vec<String>, String> {
    let Some(owner) = repositories
        .get_thread(transition.thread_id.clone())
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(vec![transition.thread_id.clone()]);
    };
    let project = repositories
        .get_project(owner.project_id.clone())
        .await
        .map_err(|error| error.to_string())?;
    if project
        .as_ref()
        .and_then(|project| project.worktree_repository_key.as_deref())
        .is_some_and(|repository_key| repository_key != transition.repository_key)
    {
        return Err(
            "workspace-loss repository identity no longer matches the persisted project".to_owned(),
        );
    }
    let project_root = project.map(|project| project.workspace_root);
    let transition_path = canonical_worktree_path_key(&transition.path)
        .await
        .map_err(|error| error.to_string())?;
    let project_root_key = match project_root.as_deref() {
        Some(path) => Some(
            canonical_worktree_path_key(Path::new(path))
                .await
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    let candidates = repositories
        .list_threads_by_project(owner.project_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut thread_ids = Vec::new();
    for thread in candidates
        .into_iter()
        .filter(|thread| thread.deleted_at.is_none())
    {
        let key = match thread.worktree_path.as_deref() {
            Some(path) => canonical_worktree_path_key(Path::new(path))
                .await
                .map_err(|error| error.to_string())?,
            None => match project_root_key.as_ref() {
                Some(key) => key.clone(),
                None => continue,
            },
        };
        if key == transition_path {
            thread_ids.push(thread.thread_id);
        }
    }
    if !thread_ids.contains(&transition.thread_id) {
        thread_ids.push(transition.thread_id.clone());
    }
    thread_ids.sort();
    thread_ids.dedup();
    Ok(thread_ids)
}

pub(crate) async fn removal_thread_ids(
    repositories: Repositories,
    request: &WorktreeRemovalQuiesceRequest,
) -> Result<Vec<String>, String> {
    let projects = repositories
        .list_projects()
        .await
        .map_err(|error| error.to_string())?;
    let mut thread_ids = request.known_thread_ids().to_vec();
    for project in projects.into_iter().filter(|project| {
        project.deleted_at.is_none()
            && if let Some(repository_key) = request.repository_key() {
                project.worktree_repository_key.as_deref() == Some(repository_key)
            } else {
                project.project_id == request.project_id()
            }
    }) {
        let project_root = canonical_worktree_path_key(Path::new(&project.workspace_root))
            .await
            .map_err(|error| error.to_string())?;
        let candidates = repositories
            .list_threads_by_project(project.project_id)
            .await
            .map_err(|error| error.to_string())?;
        for thread in candidates
            .into_iter()
            .filter(|thread| thread.deleted_at.is_none())
        {
            let path_key = match thread.worktree_path.as_deref() {
                Some(path) => canonical_worktree_path_key(Path::new(path))
                    .await
                    .map_err(|error| error.to_string())?,
                None => project_root.clone(),
            };
            if path_key == request.identity().path_key() {
                thread_ids.push(thread.thread_id);
            }
        }
    }
    if !thread_ids
        .iter()
        .any(|thread_id| thread_id == request.identity().thread_id())
    {
        thread_ids.push(request.identity().thread_id().to_owned());
    }
    thread_ids.sort();
    thread_ids.dedup();
    Ok(thread_ids)
}

#[cfg(test)]
mod tests {
    use std::{
        future::{Future, pending, ready},
        path::{Path, PathBuf},
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        },
        task::{Context, Poll},
        time::Duration,
    };

    use serde_json::{Value, json};
    use tokio::sync::{Semaphore, broadcast, mpsc, watch};
    use tokio_util::sync::CancellationToken;

    use super::{
        WorktreeRuntime, WorktreeRuntimeActions, WorktreeRuntimeFuture, WorktreeRuntimeOptions,
        append_workspace_warning, quiesce_terminals_for_transition, workspace_thread_ids,
        workspace_warning_activity_id,
    };
    use crate::worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
        WorkspaceRemovalIdentity,
    };
    use crate::{
        activity::{ActivityProjection, ActivityRepository},
        cloud::{RelayClientInstallEvent, RelayClientService, RelayClientStatus},
        diagnostics::{
            DiagnosticsMonitor, NativeProcessSampler, NativeResourceSampler,
            NotApplicableUiProcessObserver, ProcessAttributionRegistry, ProcessIdentity,
        },
        orchestration::{EngineOptions, OrchestrationCommand, OrchestrationEngine, load_snapshot},
        persistence::{Database, Repositories, run_migrations},
        production::provider_runtime::{
            BoxRuntimeFuture, ProviderDriver, ProviderDriverFactory, ProviderEvent,
            ProviderLaunchRequest, ProviderRuntimeError, ProviderRuntimeSupervisor, StartedSession,
            SupervisorOptions,
        },
        production::server_terminal::{
            JsonFuture, JsonStream, ProductionServerControl, ServerTerminalServices,
        },
        production::worktree_catalog_rpc::{
            WorktreeRemovalCleanupAdmissionError, WorktreeRemovalQuiesceLease,
            WorktreeRemovalQuiesceRequest, WorktreeRemovalQuiescer,
        },
        provider_usage::ProviderUsageService,
        rpc::{RpcResult, RpcStreamChunk},
        terminal::{
            PtyBackend, PtyExit, PtyProcess, PtySpawnInput, TerminalAttachInput, TerminalManager,
            TerminalManagerOptions, TerminalOpenInput, TerminalRestartInput, TerminalStatus,
        },
    };

    #[derive(Default)]
    struct FakeActions {
        provider_calls: AtomicUsize,
        terminal_calls: AtomicUsize,
        warnings: Mutex<Vec<WorkspaceLossTransition>>,
        affected_threads: Mutex<Option<Vec<String>>>,
        provider_threads: Mutex<Vec<String>>,
        terminal_threads: Mutex<Vec<String>>,
        cleanup_never_finishes: bool,
        warning_fails: bool,
        warning_never_finishes: bool,
        removal_failures_remaining: AtomicUsize,
        removal_resolution_failures_remaining: AtomicUsize,
        removal_resolution_calls: AtomicUsize,
    }

    impl FakeActions {
        fn pending() -> Self {
            Self {
                cleanup_never_finishes: true,
                ..Self::default()
            }
        }

        fn failing_warning() -> Self {
            Self {
                warning_fails: true,
                ..Self::default()
            }
        }

        fn pending_warning() -> Self {
            Self {
                warning_never_finishes: true,
                ..Self::default()
            }
        }

        fn removal_retry() -> Self {
            Self {
                removal_failures_remaining: AtomicUsize::new(2),
                ..Self::default()
            }
        }

        fn removal_resolution_retry(threads: Vec<String>) -> Self {
            Self {
                affected_threads: Mutex::new(Some(threads)),
                removal_resolution_failures_remaining: AtomicUsize::new(1),
                ..Self::default()
            }
        }

        fn removal_always_fails() -> Self {
            Self {
                removal_failures_remaining: AtomicUsize::new(usize::MAX),
                ..Self::default()
            }
        }
    }

    impl WorktreeRuntimeActions for FakeActions {
        fn affected_thread_ids(
            &self,
            transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
            let threads = self
                .affected_threads
                .lock()
                .expect("affected thread lock")
                .clone()
                .unwrap_or_else(|| vec![transition.thread_id]);
            Box::pin(ready(Ok(threads)))
        }

        fn stop_provider(
            &self,
            thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.provider_calls.fetch_add(1, Ordering::SeqCst);
            self.provider_threads
                .lock()
                .expect("provider thread lock")
                .push(thread_id);
            if self.cleanup_never_finishes {
                Box::pin(pending())
            } else {
                Box::pin(ready(Ok(())))
            }
        }

        fn close_terminals(
            &self,
            thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.terminal_calls.fetch_add(1, Ordering::SeqCst);
            self.terminal_threads
                .lock()
                .expect("terminal thread lock")
                .push(thread_id);
            if self.cleanup_never_finishes {
                Box::pin(pending())
            } else {
                Box::pin(ready(Ok(())))
            }
        }

        fn append_warning(
            &self,
            transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.warnings.lock().expect("warning lock").push(transition);
            if self.warning_never_finishes {
                Box::pin(pending())
            } else if self.warning_fails {
                Box::pin(ready(Err("warning persistence failed".to_owned())))
            } else {
                Box::pin(ready(Ok(())))
            }
        }

        fn affected_thread_ids_for_removal(
            &self,
            request: WorktreeRemovalQuiesceRequest,
        ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
            self.removal_resolution_calls.fetch_add(1, Ordering::SeqCst);
            if self
                .removal_resolution_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Box::pin(ready(Err(
                    "injected removal alias resolution failure".to_owned()
                )));
            }
            let threads = self
                .affected_threads
                .lock()
                .expect("affected thread lock")
                .clone()
                .unwrap_or_else(|| vec![request.identity().thread_id().to_owned()]);
            Box::pin(ready(Ok(threads)))
        }

        fn stop_provider_for_removal(
            &self,
            thread_id: String,
            _identity: WorkspaceRemovalIdentity,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.provider_calls.fetch_add(1, Ordering::SeqCst);
            self.provider_threads
                .lock()
                .expect("provider thread lock")
                .push(thread_id);
            if self.cleanup_never_finishes {
                return Box::pin(pending());
            }
            let fails = self
                .removal_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            Box::pin(ready(if fails {
                Err("injected removal provider failure".to_owned())
            } else {
                Ok(())
            }))
        }

        fn close_terminals_for_removal(
            &self,
            thread_id: String,
            _identity: WorkspaceRemovalIdentity,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.terminal_calls.fetch_add(1, Ordering::SeqCst);
            self.terminal_threads
                .lock()
                .expect("terminal thread lock")
                .push(thread_id);
            if self.cleanup_never_finishes {
                return Box::pin(pending());
            }
            let fails = self
                .removal_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            Box::pin(ready(if fails {
                Err("injected removal terminal failure".to_owned())
            } else {
                Ok(())
            }))
        }
    }

    struct RuntimeTestProvider;

    impl ProviderDriverFactory for RuntimeTestProvider {
        fn create(
            &self,
            _request: ProviderLaunchRequest,
        ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
            Box::pin(async { Ok(Arc::new(Self) as Arc<dyn ProviderDriver>) })
        }
    }

    impl ProviderDriver for RuntimeTestProvider {
        fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
            Box::pin(ready(Ok(StartedSession::default())))
        }

        fn send(
            &self,
            _text: String,
            _attachments: Vec<Value>,
            _interaction_mode: String,
        ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
            Box::pin(ready(Ok(None)))
        }

        fn interrupt(
            &self,
            _turn_id: Option<String>,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn approve(
            &self,
            _request_id: String,
            _decision: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn answer(
            &self,
            _request_id: String,
            _answers: Value,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn set_mode(
            &self,
            _mode: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn set_model(
            &self,
            _model: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn set_options(
            &self,
            _options: Vec<Value>,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn rollback(
            &self,
            _turn_count: i64,
            _rewind_key: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }

        fn next_event(&self) -> BoxRuntimeFuture<'_, Option<ProviderEvent>> {
            Box::pin(pending())
        }

        fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(ready(Ok(())))
        }
    }

    #[tokio::test]
    async fn production_liveness_reports_only_captured_provider_sessions() {
        let root = tempfile::tempdir().expect("provider root");
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let activity = ActivityProjection::new(ActivityRepository::new(database.clone()));
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");
        for command in [
            json!({
                "type":"project.create", "commandId":"project", "projectId":"project-1",
                "title":"Project", "workspaceRoot":root.path(),
                "createdAt":"2026-09-27T00:00:00Z"
            }),
            json!({
                "type":"thread.create", "commandId":"thread", "threadId":"live-thread",
                "projectId":"project-1", "title":"Thread", "kind":"workspace",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":"2026-09-27T00:00:00Z"
            }),
        ] {
            engine
                .dispatch(serde_json::from_value(command).expect("command"))
                .await
                .expect("fixture created");
        }
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(RuntimeTestProvider),
            activity,
            SupervisorOptions::default(),
        ));
        let terminals = TerminalManager::new(
            Arc::new(RuntimeTestPtyBackend::default()),
            TerminalManagerOptions::default(),
        );
        let runtime = WorktreeRuntime::start(
            engine.clone(),
            provider.clone(),
            runtime_terminal_services(terminals.clone()),
            WorkspaceAvailabilityRegistry::new(),
        );
        provider
            .launch(ProviderLaunchRequest {
                thread_id: "live-thread".to_owned(),
                activity_causal_revision: 0,
                provider: "codex".to_owned(),
                provider_label: "Codex".to_owned(),
                provider_instance_id: Some("codex".to_owned()),
                binary_path: "in-memory-test-provider".to_owned(),
                cwd: root.path().to_path_buf(),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                model: Some("gpt-5".to_owned()),
                options: Vec::new(),
                custom_models: Vec::new(),
                service_tier: None,
                effort: None,
                agent: None,
                resume_cursor: None,
                environment: Default::default(),
                endpoint: None,
                server_password: None,
                mcp: None,
                codex_home: None,
                open_url: None,
            })
            .await
            .expect("provider launches");
        let thread_ids = vec!["absent-thread".to_owned(), "live-thread".to_owned()];
        assert_eq!(
            runtime
                .live_session_thread_ids(thread_ids.clone())
                .await
                .expect("capture live sessions"),
            ["live-thread"]
        );
        assert!(
            runtime
                .live_session_thread_ids(vec!["absent-thread".to_owned()])
                .await
                .expect("only requested threads")
                .is_empty()
        );
        let identity = provider
            .capture_session_identity("live-thread")
            .await
            .expect("session capture")
            .expect("live session");
        provider
            .stop_session_if_current(identity)
            .await
            .expect("provider stops");
        assert!(
            runtime
                .live_session_thread_ids(thread_ids.clone())
                .await
                .expect("stopped sessions are not live")
                .is_empty()
        );
        provider.shutdown().await.expect("provider shutdown");
        assert!(runtime.live_session_thread_ids(thread_ids).await.is_err());
        runtime.shutdown().await;
        terminals.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn production_workspace_loss_settles_without_a_live_provider() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open_in_memory().await.unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap();
        let activity = ActivityProjection::new(ActivityRepository::new(database.clone()));
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .unwrap();
        for command in [
            json!({"type":"project.create", "commandId":"project", "projectId":"p1", "title":"Project", "workspaceRoot":root.path(), "createdAt":"2026-01-01T00:00:00Z"}),
            json!({"type":"thread.create", "commandId":"thread", "threadId":"thread-1", "projectId":"p1", "title":"Thread", "kind":"workspace", "modelSelection":{"instanceId":"codex","model":"gpt-5"}, "runtimeMode":"full-access", "interactionMode":"default", "branch":null, "worktreePath":null, "createdAt":"2026-01-01T00:00:00Z"}),
            json!({"type":"thread.session.set", "commandId":"session", "threadId":"thread-1", "session":{"threadId":"thread-1", "status":"running", "providerName":"codex", "activeTurnId":"turn-1", "lastError":null, "updatedAt":"2026-01-01T00:00:00Z"}, "createdAt":"2026-01-01T00:00:00Z"}),
        ] {
            engine
                .dispatch(serde_json::from_value(command).unwrap())
                .await
                .unwrap();
        }
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(RuntimeTestProvider),
            activity,
            SupervisorOptions::default(),
        ));
        assert!(
            provider
                .capture_session_identity("thread-1")
                .await
                .unwrap()
                .is_none()
        );
        let terminals = TerminalManager::new(
            Arc::new(RuntimeTestPtyBackend::default()),
            TerminalManagerOptions::default(),
        );
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = WorkspaceLossTransition {
            // Registry admission captures physical identity; /var aliases on
            // macOS must not make this fixture look like a stale transition.
            path: std::fs::canonicalize(root.path()).unwrap().join("missing"),
            ..transition(1)
        };
        registry.mark_unavailable(loss.clone()).await.unwrap();
        let actions = super::ProductionWorktreeRuntimeActions {
            orchestration: engine.clone(),
            provider: provider.clone(),
            terminals: runtime_terminal_services(terminals.clone()),
            registry,
        };

        actions
            .stop_provider("thread-1".into(), loss)
            .await
            .unwrap();

        let session = engine
            .repositories()
            .get_thread_session("thread-1".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "error");
        assert_eq!(session.active_turn_id, None);
        assert_eq!(session.last_error_class.as_deref(), Some("session_stopped"));
        assert!(
            session
                .last_error
                .unwrap()
                .contains("workspace became unavailable")
        );
        provider.shutdown().await.unwrap();
        terminals.shutdown().await;
        engine.shutdown().await;
    }

    #[derive(Debug)]
    struct RuntimeTestPty {
        pid: u32,
        identity: ProcessIdentity,
        killed: AtomicBool,
        writes: Mutex<Vec<String>>,
        output: broadcast::Sender<String>,
        exit: watch::Sender<Option<PtyExit>>,
    }

    impl RuntimeTestPty {
        fn new(pid: u32) -> Self {
            static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
            assert_ne!(pid, 0, "synthetic PTY PID must be nonzero");
            let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
            assert_ne!(generation, 0, "synthetic PTY generation wrapped to zero");
            let (output, _) = broadcast::channel(16);
            let (exit, _) = watch::channel(None);
            Self {
                pid,
                identity: ProcessIdentity {
                    pid,
                    started_at: generation,
                },
                killed: AtomicBool::new(false),
                writes: Mutex::new(Vec::new()),
                output,
                exit,
            }
        }

        fn emit(&self, value: &str) {
            self.output
                .send(value.to_owned())
                .expect("terminal output receiver");
        }

        fn is_killed(&self) -> bool {
            self.killed.load(Ordering::Acquire)
        }
    }

    impl PtyProcess for RuntimeTestPty {
        fn pid(&self) -> u32 {
            self.pid
        }

        fn process_identity(&self) -> Option<ProcessIdentity> {
            Some(self.identity)
        }

        fn write(&self, data: &str) -> Result<(), String> {
            if self.is_killed() {
                return Err("terminal process is killed".to_owned());
            }
            self.writes
                .lock()
                .expect("terminal writes lock")
                .push(data.to_owned());
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<(), String> {
            Ok(())
        }

        fn kill(&self) -> Result<(), String> {
            self.killed.store(true, Ordering::Release);
            self.exit.send_replace(Some(PtyExit {
                exit_code: None,
                signal: None,
            }));
            Ok(())
        }

        fn wait_for_process_tree_exit(&self, _timeout: Duration) -> Result<Option<bool>, String> {
            Ok(Some(true))
        }

        fn subscribe_output(&self) -> broadcast::Receiver<String> {
            self.output.subscribe()
        }

        fn subscribe_exit(&self) -> watch::Receiver<Option<PtyExit>> {
            self.exit.subscribe()
        }
    }

    #[test]
    fn runtime_test_ptys_distinguish_same_pid_generations() {
        let first = RuntimeTestPty::new(1);
        let replacement = RuntimeTestPty::new(1);
        assert_ne!(first.identity.started_at, 0);
        assert!(replacement.identity.started_at > first.identity.started_at);
        assert_ne!(first.identity, replacement.identity);
    }

    #[derive(Debug, Default)]
    pub(super) struct RuntimeTestPtyBackend {
        processes: Mutex<Vec<Arc<RuntimeTestPty>>>,
    }

    impl RuntimeTestPtyBackend {
        fn processes(&self) -> Vec<Arc<RuntimeTestPty>> {
            self.processes.lock().expect("terminal processes").clone()
        }
    }

    impl PtyBackend for RuntimeTestPtyBackend {
        fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
            let mut processes = self.processes.lock().expect("terminal processes");
            let process = Arc::new(RuntimeTestPty::new(processes.len() as u32 + 1));
            processes.push(process.clone());
            Ok(process)
        }
    }

    #[derive(Debug)]
    struct RuntimeTestControl;

    impl ProductionServerControl for RuntimeTestControl {
        fn call(
            &self,
            _method: &'static str,
            _payload: Value,
            _cancellation: CancellationToken,
        ) -> JsonFuture {
            Box::pin(async { Ok(Value::Null) as RpcResult })
        }

        fn subscribe(&self, _method: &'static str, _cancellation: CancellationToken) -> JsonStream {
            let (_sender, receiver) = mpsc::channel::<RpcStreamChunk>(1);
            receiver
        }
    }

    pub(super) fn runtime_terminal_services(manager: TerminalManager) -> ServerTerminalServices {
        let sampler = Arc::new(NativeProcessSampler::default());
        let resource_sampler = Arc::new(NativeResourceSampler::new(
            sampler.clone(),
            ProcessAttributionRegistry::new(),
            Arc::new(NotApplicableUiProcessObserver),
        ));
        let monitor = Arc::new(DiagnosticsMonitor::new(
            resource_sampler.clone(),
            Duration::from_secs(60),
        ));
        let provider_usage =
            ProviderUsageService::new(Vec::new(), Arc::new(time::OffsetDateTime::now_utc));
        let relay = RelayClientService::new(
            || async {
                RelayClientStatus::Missing {
                    version: "1.0.0".to_owned(),
                }
            },
            |_report: Arc<dyn Fn(RelayClientInstallEvent) -> _ + Send + Sync>| async {
                Ok(RelayClientStatus::Missing {
                    version: "1.0.0".to_owned(),
                })
            },
        );
        ServerTerminalServices::new(
            manager,
            sampler,
            resource_sampler,
            monitor,
            provider_usage,
            relay,
            Arc::new(RuntimeTestControl),
        )
    }

    struct RuntimeTerminalActions {
        registry: WorkspaceAvailabilityRegistry,
        terminals: ServerTerminalServices,
        terminal_attempts: AtomicUsize,
        fail_first_terminal_attempt: bool,
    }

    impl WorktreeRuntimeActions for RuntimeTerminalActions {
        fn stop_provider(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }

        fn close_terminals(
            &self,
            thread_id: String,
            transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            let attempt = self.terminal_attempts.fetch_add(1, Ordering::SeqCst);
            if self.fail_first_terminal_attempt && attempt == 0 {
                return Box::pin(ready(Err("initial terminal cleanup failed".to_owned())));
            }
            let terminals = self.terminals.clone();
            let registry = self.registry.clone();
            Box::pin(async move {
                quiesce_terminals_for_transition(&terminals, &registry, &thread_id, &transition)
                    .await
            })
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    async fn wait_for_terminal_history(
        manager: &TerminalManager,
        thread_id: &str,
        terminal_id: &str,
        expected: &str,
    ) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let attachment = manager
                    .attach(TerminalAttachInput::existing(thread_id, terminal_id))
                    .await
                    .expect("terminal attaches while waiting for history");
                if attachment.initial.history == expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal history is published");
    }

    struct ConcurrencyActions {
        calls: AtomicUsize,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        started: Arc<Semaphore>,
        release: Arc<Semaphore>,
    }

    impl WorktreeRuntimeActions for ConcurrencyActions {
        fn stop_provider(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            self.started.add_permits(1);
            let release = self.release.clone();
            let active_count = self.active.clone();
            Box::pin(async move {
                let permit = release.acquire().await.expect("release semaphore");
                permit.forget();
                active_count.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        }

        fn close_terminals(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    struct RetryActions {
        provider_calls: AtomicUsize,
        retry_started: Arc<Semaphore>,
        retry_release: Arc<Semaphore>,
        panic_first: bool,
    }

    struct ResolverRetryActions {
        resolver_calls: AtomicUsize,
        provider_calls: AtomicUsize,
        provider_threads: Mutex<Vec<String>>,
        retry_started: Arc<Semaphore>,
        retry_release: Arc<Semaphore>,
    }

    struct RecoveryRaceActions {
        resolver_calls: AtomicUsize,
        provider_calls: AtomicUsize,
        terminal_calls: AtomicUsize,
        retry_resolver_started: Arc<Semaphore>,
        retry_resolver_release: Arc<Semaphore>,
    }

    #[derive(Default)]
    struct HangingAliasActions {
        provider_calls: AtomicUsize,
        terminal_calls: AtomicUsize,
    }

    impl WorktreeRuntimeActions for HangingAliasActions {
        fn affected_thread_ids(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
            Box::pin(pending())
        }

        fn stop_provider(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.provider_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(ready(Ok(())))
        }

        fn close_terminals(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.terminal_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    impl WorktreeRuntimeActions for RecoveryRaceActions {
        fn affected_thread_ids(
            &self,
            transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
            let attempt = self.resolver_calls.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Box::pin(ready(Ok(vec![transition.thread_id])))
            } else {
                self.retry_resolver_started.add_permits(1);
                let release = self.retry_resolver_release.clone();
                Box::pin(async move {
                    release.acquire().await.expect("resolver release").forget();
                    Ok(vec![transition.thread_id])
                })
            }
        }

        fn stop_provider(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            let attempt = self.provider_calls.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Box::pin(ready(Err("initial cleanup failed".to_owned())))
            } else {
                Box::pin(ready(Ok(())))
            }
        }

        fn close_terminals(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.terminal_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    impl WorktreeRuntimeActions for ResolverRetryActions {
        fn affected_thread_ids(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
            let attempt = self.resolver_calls.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Box::pin(ready(Err("alias query failed".to_owned())))
            } else {
                Box::pin(ready(Ok(vec!["thread-1".to_owned(), "panel-1".to_owned()])))
            }
        }

        fn stop_provider(
            &self,
            thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.provider_threads
                .lock()
                .expect("provider thread lock")
                .push(thread_id);
            if self.provider_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Box::pin(ready(Ok(())))
            } else {
                self.retry_started.add_permits(1);
                let release = self.retry_release.clone();
                Box::pin(async move {
                    release.acquire().await.expect("retry release").forget();
                    Ok(())
                })
            }
        }

        fn close_terminals(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    impl WorktreeRuntimeActions for RetryActions {
        fn stop_provider(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            let attempt = self.provider_calls.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                if self.panic_first {
                    return Box::pin(async { panic!("first provider cleanup panicked") });
                }
                return Box::pin(ready(Err("first provider cleanup failed".to_owned())));
            }
            self.retry_started.add_permits(1);
            let release = self.retry_release.clone();
            Box::pin(async move {
                let permit = release.acquire().await.expect("retry release");
                permit.forget();
                Ok(())
            })
        }

        fn close_terminals(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    struct PendingDrop {
        drops: Arc<AtomicUsize>,
    }

    impl Future for PendingDrop {
        type Output = Result<(), String>;

        fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    impl Drop for PendingDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct ShutdownActions {
        drops: Arc<AtomicUsize>,
    }

    impl WorktreeRuntimeActions for ShutdownActions {
        fn stop_provider(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(PendingDrop {
                drops: self.drops.clone(),
            })
        }

        fn close_terminals(
            &self,
            _thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    struct PersistedAliasActions {
        repositories: crate::persistence::Repositories,
        provider_threads: Arc<Mutex<Vec<String>>>,
        terminal_threads: Arc<Mutex<Vec<String>>>,
    }

    impl WorktreeRuntimeActions for PersistedAliasActions {
        fn affected_thread_ids(
            &self,
            transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<Vec<String>, String>> {
            let repositories = self.repositories.clone();
            Box::pin(async move { workspace_thread_ids(repositories, &transition).await })
        }

        fn stop_provider(
            &self,
            thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.provider_threads
                .lock()
                .expect("provider thread lock")
                .push(thread_id);
            Box::pin(ready(Ok(())))
        }

        fn close_terminals(
            &self,
            thread_id: String,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            self.terminal_threads
                .lock()
                .expect("terminal thread lock")
                .push(thread_id);
            Box::pin(ready(Ok(())))
        }

        fn append_warning(
            &self,
            _transition: WorkspaceLossTransition,
        ) -> WorktreeRuntimeFuture<Result<(), String>> {
            Box::pin(ready(Ok(())))
        }
    }

    fn transition(index: usize) -> WorkspaceLossTransition {
        WorkspaceLossTransition {
            thread_id: format!("thread-{index}"),
            repository_key: "repository-a".to_owned(),
            generation: 7,
            path: PathBuf::from(format!("/repo/worktrees/missing-{index}")),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        }
    }

    fn removal_request(identity: WorkspaceRemovalIdentity) -> WorktreeRemovalQuiesceRequest {
        let thread_id = identity.thread_id().to_owned();
        WorktreeRemovalQuiesceRequest::project(
            identity,
            "project-removal".to_owned(),
            vec![thread_id],
        )
    }

    async fn quiesce_admitted_removal(
        runtime: &WorktreeRuntime,
        request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceLease {
        let admission = WorktreeRemovalQuiescer::admit_cleanup(runtime)
            .await
            .expect("removal cleanup admission");
        WorktreeRemovalQuiescer::quiesce(runtime, admission, request).await
    }

    #[tokio::test]
    async fn removal_failure_returns_pending_and_reaper_retries_under_retained_guard() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let caller_guard = registry
            .mark_removing("thread-removal", Path::new("/repo/removal"))
            .await
            .expect("physical identity resolves");
        let identity = caller_guard.identity();
        let actions = Arc::new(FakeActions::removal_retry());
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(100),
                reaper_capacity: 4,
                max_parallel_quiesces: 2,
            },
        );

        let lease = quiesce_admitted_removal(&runtime, removal_request(identity)).await;
        assert!(lease.orphan_cleanup_pending());
        lease.commit_detached();
        drop(caller_guard);
        assert!(
            registry.guard_thread("thread-removal").await.is_err(),
            "the reaper retains the exact Removing guard while cleanup is pending"
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if actions.provider_calls.load(Ordering::SeqCst) >= 2
                    && actions.terminal_calls.load(Ordering::SeqCst) >= 2
                    && registry.guard_thread("thread-removal").await.is_ok()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("removal cleanup retry completes");
        runtime.shutdown().await;
        assert_eq!(runtime.active_reaper_jobs(), 0);
    }

    #[tokio::test]
    async fn removal_timeout_is_owned_until_shutdown_cancels_and_releases_the_reaper_guard() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let caller_guard = registry
            .mark_removing("thread-removal", Path::new("/repo/removal"))
            .await
            .expect("physical identity resolves");
        let identity = caller_guard.identity();
        let runtime = WorktreeRuntime::start_for_test(
            Arc::new(FakeActions::pending()),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(20),
                reaper_capacity: 2,
                max_parallel_quiesces: 1,
            },
        );

        let lease = quiesce_admitted_removal(&runtime, removal_request(identity)).await;
        assert!(lease.orphan_cleanup_pending());
        lease.commit_detached();
        drop(caller_guard);
        assert!(registry.guard_thread("thread-removal").await.is_err());
        runtime.shutdown().await;
        assert_eq!(registry.guard_thread("thread-removal").await, Ok(()));
        assert_eq!(runtime.active_reaper_jobs(), 0);
    }

    #[tokio::test]
    async fn removal_abandonment_cancels_queued_retry_before_it_can_touch_runtime_resources() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let caller_guard = registry
            .mark_removing("thread-removal", Path::new("/repo/removal"))
            .await
            .expect("physical identity resolves");
        let identity = caller_guard.identity();
        let actions = Arc::new(FakeActions::default());
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(20),
                reaper_capacity: 2,
                max_parallel_quiesces: 1,
            },
        );
        let blocker = runtime
            .inner
            .quiesce_permits
            .clone()
            .acquire_owned()
            .await
            .expect("block cleanup permit");
        let lease = quiesce_admitted_removal(&runtime, removal_request(identity)).await;
        assert!(lease.orphan_cleanup_pending());

        drop(lease);
        drop(caller_guard);
        drop(blocker);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if registry.guard_thread("thread-removal").await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled removal retry releases guard");
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 0);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 0);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn saturated_removal_cleanup_admission_rejects_before_removing_and_releases_on_shutdown()
    {
        let registry = WorkspaceAvailabilityRegistry::new();
        let runtime = WorktreeRuntime::start_for_test(
            Arc::new(FakeActions::pending()),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(20),
                reaper_capacity: 1,
                max_parallel_quiesces: 1,
            },
        );
        let admission = WorktreeRemovalQuiescer::admit_cleanup(&runtime)
            .await
            .expect("first cleanup slot");
        let guard = registry
            .mark_removing("saturated-removal", Path::new("/repo/saturated-removal"))
            .await
            .expect("physical identity resolves");
        let lease = WorktreeRemovalQuiescer::quiesce(
            &runtime,
            admission,
            removal_request(guard.identity()),
        )
        .await;
        assert!(lease.orphan_cleanup_pending());
        lease.commit_detached();
        drop(guard);
        assert!(registry.guard_thread("saturated-removal").await.is_err());

        assert!(
            matches!(
                WorktreeRemovalQuiescer::admit_cleanup(&runtime).await,
                Err(WorktreeRemovalCleanupAdmissionError::Capacity)
            ),
            "capacity must reject before a second Removing guard"
        );
        assert_eq!(registry.guard_thread("never-marked-removing").await, Ok(()));

        runtime.shutdown().await;
        assert_eq!(registry.guard_thread("saturated-removal").await, Ok(()));
        WorktreeRemovalQuiescer::admit_cleanup(&runtime)
            .await
            .expect("shutdown releases the lifetime cleanup slot");
    }

    #[tokio::test(start_paused = true)]
    async fn failed_removal_retry_waits_for_bounded_backoff_without_hot_looping() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let guard = registry
            .mark_removing("backoff-removal", Path::new("/repo/backoff-removal"))
            .await
            .expect("physical identity resolves");
        let actions = Arc::new(FakeActions::removal_always_fails());
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry,
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(20),
                reaper_capacity: 1,
                max_parallel_quiesces: 1,
            },
        );
        let lease = quiesce_admitted_removal(&runtime, removal_request(guard.identity())).await;
        assert!(lease.orphan_cleanup_pending());
        lease.commit_detached();
        drop(guard);

        for _ in 0..20 {
            tokio::task::yield_now().await;
            if actions.provider_calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
        }
        let calls_after_first_retry = actions.provider_calls.load(Ordering::SeqCst);
        assert_eq!(calls_after_first_retry, 2);
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            actions.provider_calls.load(Ordering::SeqCst),
            calls_after_first_retry,
            "failed cleanup must not hot-loop before retry backoff elapses"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
            if actions.provider_calls.load(Ordering::SeqCst) > calls_after_first_retry {
                break;
            }
        }
        assert_eq!(
            actions.provider_calls.load(Ordering::SeqCst),
            calls_after_first_retry + 1
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn removal_reaper_reresolves_aliases_after_initial_resolution_failure() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let caller_guard = registry
            .mark_removing("thread-removal", Path::new("/repo/removal"))
            .await
            .expect("physical identity resolves");
        let actions = Arc::new(FakeActions::removal_resolution_retry(vec![
            "thread-removal".to_owned(),
            "panel-alias".to_owned(),
        ]));
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(100),
                reaper_capacity: 2,
                max_parallel_quiesces: 2,
            },
        );

        let lease =
            quiesce_admitted_removal(&runtime, removal_request(caller_guard.identity())).await;
        assert!(lease.orphan_cleanup_pending());
        lease.commit_detached();
        drop(caller_guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if registry.guard_thread("thread-removal").await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("re-resolved removal cleanup completes");
        assert!(actions.removal_resolution_calls.load(Ordering::SeqCst) >= 2);
        assert!(
            actions
                .provider_threads
                .lock()
                .expect("provider threads")
                .iter()
                .any(|thread_id| thread_id == "panel-alias")
        );
        assert!(
            actions
                .terminal_threads
                .lock()
                .expect("terminal threads")
                .iter()
                .any(|thread_id| thread_id == "panel-alias")
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn failed_removal_reaper_attempt_retains_pending_owner_for_retry_or_shutdown() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let caller_guard = registry
            .mark_removing("thread-removal", Path::new("/repo/removal"))
            .await
            .expect("physical identity resolves");
        let actions = Arc::new(FakeActions::removal_always_fails());
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_millis(20),
                reaper_capacity: 2,
                max_parallel_quiesces: 2,
            },
        );

        let lease =
            quiesce_admitted_removal(&runtime, removal_request(caller_guard.identity())).await;
        assert!(lease.orphan_cleanup_pending());
        lease.commit_detached();
        drop(caller_guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if actions.provider_calls.load(Ordering::SeqCst) >= 2
                    && actions.terminal_calls.load(Ordering::SeqCst) >= 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first reaper cleanup attempt runs");
        assert!(registry.guard_thread("thread-removal").await.is_err());

        runtime.shutdown().await;
        assert_eq!(registry.guard_thread("thread-removal").await, Ok(()));
    }

    #[tokio::test]
    async fn repository_scoped_removal_aliases_include_pinned_peer_projects_only() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let repositories = Repositories::new(database);
        for (project_id, repository_key) in [
            ("project-a", "repository-shared"),
            ("project-b", "repository-shared"),
            ("project-c", "repository-unrelated"),
        ] {
            repositories
                .upsert_project(crate::persistence::ProjectionProject {
                    project_id: project_id.to_owned(),
                    title: project_id.to_owned(),
                    workspace_root: format!("/repo/{project_id}"),
                    default_model_selection: None,
                    scripts: json!([]),
                    worktree_discovery: json!({}),
                    worktree_repository_key: None,
                    created_at: "2026-08-09T00:00:00Z".to_owned(),
                    updated_at: "2026-08-09T00:00:00Z".to_owned(),
                    deleted_at: None,
                    repository_identity: None,
                })
                .await
                .expect("project");
            repositories
                .pin_project_worktree_repository_key(
                    project_id.to_owned(),
                    repository_key.to_owned(),
                )
                .await
                .expect("pin");
            repositories
                .upsert_thread(crate::persistence::ProjectionThread {
                    thread_id: format!("thread-{project_id}"),
                    project_id: project_id.to_owned(),
                    title: project_id.to_owned(),
                    kind: "workspace".to_owned(),
                    model_selection: json!({}),
                    runtime_mode: "full-access".to_owned(),
                    interaction_mode: "default".to_owned(),
                    branch: None,
                    worktree_path: Some("/repo/shared-worktree".to_owned()),
                    latest_turn_id: None,
                    created_at: "2026-08-09T00:00:00Z".to_owned(),
                    updated_at: "2026-08-09T00:00:00Z".to_owned(),
                    archived_at: None,
                    latest_user_message_at: None,
                    pending_approval_count: 0,
                    pending_user_input_count: 0,
                    has_actionable_proposed_plan: 0,
                    unresolved_delivery_state: None,
                    unresolved_delivery_detail: None,
                    deleted_at: None,
                    host_thread_id: None,
                })
                .await
                .expect("thread");
        }
        let registry = WorkspaceAvailabilityRegistry::new();
        let guard = registry
            .mark_removing("thread-project-a", Path::new("/repo/shared-worktree"))
            .await
            .expect("physical identity resolves");
        let request = WorktreeRemovalQuiesceRequest::repository(
            guard.identity(),
            "project-a".to_owned(),
            "repository-shared".to_owned(),
            vec!["thread-project-a".to_owned()],
        );

        let aliases = super::removal_thread_ids(repositories, &request)
            .await
            .expect("aliases");

        assert_eq!(
            aliases,
            vec!["thread-project-a".to_owned(), "thread-project-b".to_owned()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn quiesce_runs_provider_and_all_terminal_cleanup_once() {
        let actions = Arc::new(FakeActions::default());
        let registry = WorkspaceAvailabilityRegistry::new();
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![transition(1)]).await;

        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.warnings.lock().expect("warning lock").len(), 1);
        assert!(!registry.orphan_cleanup_pending("thread-1"));
        runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn loss_deadline_does_not_wait_for_an_unreleased_admission() {
        let actions = Arc::new(FakeActions::default());
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(1);
        let lease = registry
            .acquire_admission(&loss.thread_id, [loss.path.as_path()])
            .await
            .expect("workspace admission");
        let cancellation = lease.loss_cancellation();
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        assert!(cancellation.is_cancelled());
        assert!(
            registry
                .acquire_admission(&loss.thread_id, [loss.path.as_path()])
                .await
                .is_err(),
            "guard installation rejects later work immediately"
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_secs(5),
                reaper_capacity: 1,
                max_parallel_quiesces: 1,
            },
        );
        let observer = runtime.clone();
        let observe = tokio::spawn(async move {
            observer.observe(vec![loss]).await;
        });
        tokio::task::yield_now().await;

        assert_eq!(
            actions.provider_calls.load(Ordering::SeqCst),
            1,
            "visible provider cleanup starts without admission drain"
        );
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.warnings.lock().expect("warning lock").len(), 1);
        assert!(!observe.is_finished());

        tokio::time::advance(Duration::from_secs(5)).await;
        observe
            .await
            .expect("loss returns at its graceful deadline");
        assert!(
            registry.orphan_cleanup_pending(
                &lease.loss_cancellation().unavailable().unwrap().thread_id
            )
        );

        drop(lease);
        runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn hung_alias_resolution_does_not_delay_known_canonical_cleanup() {
        let actions = Arc::new(HangingAliasActions::default());
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(1);
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_secs(5),
                reaper_capacity: 1,
                max_parallel_quiesces: 1,
            },
        );

        let observer = runtime.clone();
        let observe = tokio::spawn(async move { observer.observe(vec![loss]).await });
        tokio::task::yield_now().await;

        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 1);
        assert!(!observe.is_finished());

        tokio::time::advance(Duration::from_secs(5)).await;
        observe.await.expect("loss finishes at the shared deadline");
        assert!(registry.orphan_cleanup_pending("thread-1"));
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_loss_quiesces_persisted_panel_aliases_and_preserves_histories() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"project.create","commandId":"create-project","projectId":"project-1",
                    "title":"Project","workspaceRoot":"/repo","defaultModelSelection":null,
                    "createdAt":"2026-08-10T00:00:00Z"
                }))
                .expect("project command"),
            )
            .await
            .expect("project created");
        for (thread_id, kind, path) in [
            ("workspace", "workspace", "/repo/worktrees/missing-1"),
            ("panel-a", "panel", "/repo/worktrees/missing-1/./alias/.."),
            ("panel-other", "panel", "/repo/worktrees/other"),
        ] {
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.create","commandId":format!("create-{thread_id}"),
                        "threadId":thread_id,"projectId":"project-1","title":thread_id,
                        "kind":kind,"modelSelection":{"instanceId":"codex","model":"gpt-5"},
                        "runtimeMode":"full-access","interactionMode":"default","branch":null,
                        "worktreePath":path,"createdAt":"2026-08-10T00:00:00Z"
                    }))
                    .expect("thread command"),
                )
                .await
                .expect("thread created");
        }
        for thread_id in ["workspace", "panel-a"] {
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.turn.start","commandId":format!("turn-{thread_id}"),
                        "threadId":thread_id,
                        "message":{"messageId":format!("message-{thread_id}"),"role":"user",
                            "text":format!("retained history for {thread_id}"),"attachments":[]},
                        "runtimeMode":"full-access","interactionMode":"default",
                        "createdAt":"2026-08-10T00:00:00Z"
                    }))
                    .expect("turn command"),
                )
                .await
                .expect("turn created");
        }

        let mut loss = transition(1);
        loss.thread_id = "workspace".to_owned();
        let provider_threads = Arc::new(Mutex::new(Vec::new()));
        let terminal_threads = Arc::new(Mutex::new(Vec::new()));
        let runtime = WorktreeRuntime::start_for_test(
            Arc::new(PersistedAliasActions {
                repositories: engine.repositories(),
                provider_threads: provider_threads.clone(),
                terminal_threads: terminal_threads.clone(),
            }),
            WorkspaceAvailabilityRegistry::new(),
            WorktreeRuntimeOptions::default(),
        );
        runtime.observe(vec![loss]).await;

        let mut stopped_providers = provider_threads
            .lock()
            .expect("provider thread lock")
            .clone();
        stopped_providers.sort();
        let mut stopped_terminals = terminal_threads
            .lock()
            .expect("terminal thread lock")
            .clone();
        stopped_terminals.sort();
        assert_eq!(stopped_providers, ["panel-a", "workspace"]);
        assert_eq!(stopped_terminals, ["panel-a", "workspace"]);
        let snapshot = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot");
        for thread_id in ["workspace", "panel-a"] {
            assert!(
                snapshot
                    .threads
                    .iter()
                    .any(|thread| { thread.thread_id == thread_id && thread.deleted_at.is_none() })
            );
            assert!(snapshot.messages.iter().any(|message| {
                message.message_id == format!("message-{thread_id}")
                    && message.text == format!("retained history for {thread_id}")
            }));
        }
        runtime.shutdown().await;
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn quiesce_stops_every_deduplicated_persisted_workspace_alias() {
        let actions = Arc::new(FakeActions::default());
        *actions
            .affected_threads
            .lock()
            .expect("affected thread lock") = Some(vec![
            "workspace".to_owned(),
            "panel-a".to_owned(),
            "panel-a".to_owned(),
        ]);
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            WorkspaceAvailabilityRegistry::new(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![transition(1)]).await;

        let mut providers = actions
            .provider_threads
            .lock()
            .expect("provider thread lock")
            .clone();
        providers.sort();
        let mut terminals = actions
            .terminal_threads
            .lock()
            .expect("terminal thread lock")
            .clone();
        terminals.sort();
        assert_eq!(providers, ["panel-a", "thread-1", "workspace"]);
        assert_eq!(terminals, ["panel-a", "thread-1", "workspace"]);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn warning_failure_does_not_prevent_provider_or_terminal_cleanup() {
        let actions = Arc::new(FakeActions::failing_warning());
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            WorkspaceAvailabilityRegistry::new(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![transition(1)]).await;

        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 1);
        runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_warning_does_not_delay_cleanup_or_exceed_the_graceful_bound() {
        let actions = Arc::new(FakeActions::pending_warning());
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            WorkspaceAvailabilityRegistry::new(),
            WorktreeRuntimeOptions::default(),
        );
        let observer = runtime.clone();
        let task = tokio::spawn(async move {
            observer.observe(vec![transition(1)]).await;
        });
        tokio::task::yield_now().await;

        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(5)).await;
        task.await.expect("bounded loss observer");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn admitted_transition_persists_one_deterministic_warning_and_preserves_conversation() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        for command in [
            json!({
                "type":"project.create","commandId":"create-project","projectId":"project-1",
                "title":"Project","workspaceRoot":"/repo","defaultModelSelection":null,
                "createdAt":"2026-08-10T00:00:00Z"
            }),
            json!({
                "type":"thread.create","commandId":"create-thread","threadId":"thread-1",
                "projectId":"project-1","title":"Thread","kind":"workspace",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access","interactionMode":"default","branch":"feature",
                "worktreePath":"/repo/worktrees/missing-1","createdAt":"2026-08-10T00:00:00Z"
            }),
            json!({
                "type":"thread.turn.start","commandId":"conversation-turn","threadId":"thread-1",
                "message":{"messageId":"message-1","role":"user","text":"retained conversation","attachments":[]},
                "runtimeMode":"full-access","interactionMode":"default",
                "createdAt":"2026-08-10T00:00:00Z"
            }),
        ] {
            engine
                .dispatch(serde_json::from_value::<OrchestrationCommand>(command).expect("command"))
                .await
                .expect("dispatch");
        }
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(1);
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        append_workspace_warning(&engine, loss.clone())
            .await
            .expect("warning");
        assert!(
            !registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );

        let snapshot = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot");
        assert!(
            snapshot
                .threads
                .iter()
                .any(|thread| thread.thread_id == "thread-1" && thread.deleted_at.is_none())
        );
        assert!(
            snapshot
                .messages
                .iter()
                .any(|message| message.message_id == "message-1"
                    && message.text == "retained conversation")
        );
        let warnings = snapshot
            .activities
            .iter()
            .filter(|activity| activity.kind == "workspace-unavailable")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].activity_id,
            workspace_warning_activity_id(&loss)
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn overlapping_observers_share_one_global_quiesce_bound() {
        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let actions = Arc::new(ConcurrencyActions {
            calls: AtomicUsize::new(0),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            started: started.clone(),
            release: release.clone(),
        });
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            WorkspaceAvailabilityRegistry::new(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_secs(60),
                reaper_capacity: 4,
                max_parallel_quiesces: 2,
            },
        );
        let observer_a = runtime.clone();
        let observer_b = runtime.clone();
        let task_a = tokio::spawn(async move {
            observer_a
                .observe(vec![transition(1), transition(2), transition(3)])
                .await;
        });
        let task_b = tokio::spawn(async move {
            observer_b
                .observe(vec![transition(4), transition(5), transition(6)])
                .await;
        });

        for expected in [2, 4, 6] {
            let permits = started
                .acquire_many(2)
                .await
                .expect("two cleanup attempts started");
            permits.forget();
            tokio::task::yield_now().await;
            assert_eq!(actions.calls.load(Ordering::SeqCst), expected);
            assert_eq!(actions.active.load(Ordering::SeqCst), 2);
            release.add_permits(2);
        }
        task_a.await.expect("first observer");
        task_b.await.expect("second observer");

        assert_eq!(actions.peak.load(Ordering::SeqCst), 2);
        assert_eq!(actions.active.load(Ordering::SeqCst), 0);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn cleanup_error_retains_orphan_marker_until_a_repeat_attempt_succeeds() {
        let retry_started = Arc::new(Semaphore::new(0));
        let retry_release = Arc::new(Semaphore::new(0));
        let actions = Arc::new(RetryActions {
            provider_calls: AtomicUsize::new(0),
            retry_started: retry_started.clone(),
            retry_release: retry_release.clone(),
            panic_first: false,
        });
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(1))
                .await
                .expect("physical identity resolves")
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![transition(1)]).await;
        let permit = retry_started.acquire().await.expect("retry started");
        permit.forget();
        assert!(registry.orphan_cleanup_pending("thread-1"));
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 2);

        retry_release.add_permits(1);
        while registry.orphan_cleanup_pending("thread-1") {
            tokio::task::yield_now().await;
        }
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 2);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn alias_resolution_failure_cleans_owner_then_retry_resolves_every_alias() {
        let retry_started = Arc::new(Semaphore::new(0));
        let retry_release = Arc::new(Semaphore::new(0));
        let actions = Arc::new(ResolverRetryActions {
            resolver_calls: AtomicUsize::new(0),
            provider_calls: AtomicUsize::new(0),
            provider_threads: Mutex::new(Vec::new()),
            retry_started: retry_started.clone(),
            retry_release: retry_release.clone(),
        });
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(1))
                .await
                .expect("physical identity resolves")
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![transition(1)]).await;
        assert!(
            registry.orphan_cleanup_pending("thread-1"),
            "resolver failure retains transition cleanup ownership"
        );
        retry_started
            .acquire_many(2)
            .await
            .expect("owner and panel retry start")
            .forget();
        assert_eq!(actions.resolver_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            actions
                .provider_threads
                .lock()
                .expect("provider threads")
                .as_slice(),
            ["thread-1", "panel-1", "thread-1"]
        );

        retry_release.add_permits(2);
        while registry.orphan_cleanup_pending("thread-1") {
            tokio::task::yield_now().await;
        }
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn initial_terminal_cleanup_recovery_before_signal_keeps_the_unchanged_session_live() {
        let root = tempfile::tempdir().expect("terminal root");
        let backend = Arc::new(RuntimeTestPtyBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "thread-1",
                "term-live",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("terminal opens");
        let process = backend.processes()[0].clone();
        process.emit("history-before-recovery\n");
        wait_for_terminal_history(
            &manager,
            "thread-1",
            "term-live",
            "history-before-recovery\n",
        )
        .await;

        let registry = WorkspaceAvailabilityRegistry::new();
        let mut loss = transition(1);
        loss.path = crate::git::canonical_worktree_path_key(root.path())
            .await
            .expect("physical terminal root")
            .into();
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let signal_pause = registry.pause_before_next_terminal_signal_permit();
        let actions = Arc::new(RuntimeTerminalActions {
            registry: registry.clone(),
            terminals: runtime_terminal_services(manager.clone()),
            terminal_attempts: AtomicUsize::new(0),
            fail_first_terminal_attempt: false,
        });
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );
        let observer = runtime.clone();
        let observed_loss = loss.clone();
        let observe = tokio::spawn(async move { observer.observe(vec![observed_loss]).await });
        signal_pause.wait_until_entered().await;

        registry
            .clear_recovered_in_repository(
                &loss.thread_id,
                loss.path.as_path(),
                &loss.repository_key,
            )
            .await
            .expect("physical identity resolves");
        let recovered_admission = registry
            .acquire_admission(&loss.thread_id, [loss.path.as_path()])
            .await
            .expect("recovery admits new terminal work");
        drop(recovered_admission);
        signal_pause.release();
        observe.await.expect("initial cleanup joins");

        assert_eq!(actions.terminal_attempts.load(Ordering::SeqCst), 1);
        assert!(
            !process.is_killed(),
            "stale initial cleanup must not signal"
        );
        manager
            .write("thread-1", "term-live", "write-after-recovery")
            .await
            .expect("unchanged recovered terminal remains writable");
        let attachment = manager
            .attach(TerminalAttachInput::existing("thread-1", "term-live"))
            .await
            .expect("unchanged recovered terminal remains attachable");
        assert_eq!(attachment.initial.status, TerminalStatus::Running);
        assert_eq!(attachment.initial.history, "history-before-recovery\n");

        runtime.shutdown().await;
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn reaper_terminal_cleanup_recovery_before_signal_keeps_the_unchanged_session_live() {
        let root = tempfile::tempdir().expect("terminal root");
        let backend = Arc::new(RuntimeTestPtyBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "thread-1",
                "term-reaper",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("terminal opens");
        let process = backend.processes()[0].clone();
        process.emit("reaper-history\n");
        wait_for_terminal_history(&manager, "thread-1", "term-reaper", "reaper-history\n").await;

        let registry = WorkspaceAvailabilityRegistry::new();
        let mut loss = transition(1);
        loss.path = crate::git::canonical_worktree_path_key(root.path())
            .await
            .expect("physical terminal root")
            .into();
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let signal_pause = registry.pause_before_next_terminal_signal_permit();
        let actions = Arc::new(RuntimeTerminalActions {
            registry: registry.clone(),
            terminals: runtime_terminal_services(manager.clone()),
            terminal_attempts: AtomicUsize::new(0),
            fail_first_terminal_attempt: true,
        });
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![loss.clone()]).await;
        signal_pause.wait_until_entered().await;
        assert!(registry.orphan_cleanup_pending(&loss.thread_id));
        registry
            .clear_recovered_in_repository(
                &loss.thread_id,
                loss.path.as_path(),
                &loss.repository_key,
            )
            .await
            .expect("physical identity resolves");
        signal_pause.release();
        tokio::time::timeout(Duration::from_secs(1), async {
            while runtime.active_reaper_jobs() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stale reaper cleanup finishes");

        assert_eq!(actions.terminal_attempts.load(Ordering::SeqCst), 2);
        assert!(!process.is_killed(), "stale reaper cleanup must not signal");
        manager
            .write("thread-1", "term-reaper", "write-after-reaper-recovery")
            .await
            .expect("recovered terminal remains writable after stale retry");
        let attachment = manager
            .attach(TerminalAttachInput::existing("thread-1", "term-reaper"))
            .await
            .expect("recovered terminal remains attachable after stale retry");
        assert_eq!(attachment.initial.status, TerminalStatus::Running);
        assert_eq!(attachment.initial.history, "reaper-history\n");

        runtime.shutdown().await;
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_signal_before_recovery_quiesces_all_exact_sessions_before_recovery_returns() {
        let root = tempfile::tempdir().expect("terminal root");
        let backend = Arc::new(RuntimeTestPtyBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        for terminal_id in ["term-first", "term-second"] {
            manager
                .open(TerminalOpenInput::new(
                    "thread-1",
                    terminal_id,
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .expect("terminal opens");
        }
        let old_processes = backend.processes();
        for (process, history) in old_processes
            .iter()
            .zip(["first-history\n", "second-history\n"])
        {
            process.emit(history);
        }
        wait_for_terminal_history(&manager, "thread-1", "term-first", "first-history\n").await;
        wait_for_terminal_history(&manager, "thread-1", "term-second", "second-history\n").await;

        let registry = WorkspaceAvailabilityRegistry::new();
        let mut loss = transition(1);
        loss.path = crate::git::canonical_worktree_path_key(root.path())
            .await
            .expect("physical terminal root")
            .into();
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let signal_pause = registry.pause_after_next_terminal_signal_permit();
        let actions = Arc::new(RuntimeTerminalActions {
            registry: registry.clone(),
            terminals: runtime_terminal_services(manager.clone()),
            terminal_attempts: AtomicUsize::new(0),
            fail_first_terminal_attempt: false,
        });
        let runtime = WorktreeRuntime::start_for_test(
            actions,
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );
        let observer = runtime.clone();
        let observed_loss = loss.clone();
        let observe = tokio::spawn(async move { observer.observe(vec![observed_loss]).await });
        signal_pause.wait_until_entered().await;

        let recovery_registry = registry.clone();
        let recovery_loss = loss.clone();
        let invalidation_started = registry
            .terminal_signal_invalidation_notification(&loss)
            .await
            .expect("current terminal signal gate");
        let recovery = tokio::spawn(async move {
            recovery_registry
                .clear_recovered_in_repository(
                    &recovery_loss.thread_id,
                    recovery_loss.path.as_path(),
                    &recovery_loss.repository_key,
                )
                .await
                .expect("physical identity resolves");
        });
        tokio::time::timeout(Duration::from_secs(1), invalidation_started.notified())
            .await
            .expect("recovery starts terminal signal invalidation");
        assert!(
            !recovery.is_finished(),
            "recovery waits after terminal signaling owns the transition"
        );
        signal_pause.release();
        observe.await.expect("cleanup joins");
        recovery.await.expect("recovery joins after cleanup");

        assert!(old_processes.iter().all(|process| process.is_killed()));
        for (terminal_id, history) in [
            ("term-first", "first-history\n"),
            ("term-second", "second-history\n"),
        ] {
            let attachment = manager
                .attach(TerminalAttachInput::existing("thread-1", terminal_id))
                .await
                .expect("quiesced history remains attachable");
            assert_eq!(attachment.initial.status, TerminalStatus::Exited);
            assert_eq!(attachment.initial.history, history);
        }

        manager
            .restart(TerminalRestartInput {
                center_panel: false,
                thread_id: "thread-1".to_owned(),
                terminal_id: "term-first".to_owned(),
                cwd: root.path().to_path_buf(),
                worktree_path: None,
                cols: 80,
                rows: 24,
                env: std::collections::BTreeMap::new(),
                command: None,
            })
            .await
            .expect("post-recovery terminal starts");
        let replacement = backend
            .processes()
            .last()
            .cloned()
            .expect("replacement process");
        manager
            .write("thread-1", "term-first", "post-recovery-write")
            .await
            .expect("post-recovery terminal remains writable");
        assert!(!replacement.is_killed());
        assert_eq!(
            manager
                .attach(TerminalAttachInput::existing("thread-1", "term-first"))
                .await
                .expect("replacement attaches")
                .initial
                .status,
            TerminalStatus::Running
        );

        runtime.shutdown().await;
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn exact_recovery_cancels_a_stale_active_retry_before_cleanup() {
        let resolver_started = Arc::new(Semaphore::new(0));
        let resolver_release = Arc::new(Semaphore::new(0));
        let actions = Arc::new(RecoveryRaceActions {
            resolver_calls: AtomicUsize::new(0),
            provider_calls: AtomicUsize::new(0),
            terminal_calls: AtomicUsize::new(0),
            retry_resolver_started: resolver_started.clone(),
            retry_resolver_release: resolver_release.clone(),
        });
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(1);
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![loss.clone()]).await;
        resolver_started
            .acquire()
            .await
            .expect("retry resolver starts")
            .forget();
        assert!(registry.orphan_cleanup_pending(&loss.thread_id));

        registry
            .clear_recovered_in_repository(
                &loss.thread_id,
                loss.path.as_path(),
                &loss.repository_key,
            )
            .await
            .expect("physical identity resolves");
        assert!(!registry.orphan_cleanup_pending(&loss.thread_id));
        let replacement = registry
            .acquire_admission(&loss.thread_id, [loss.path.as_path()])
            .await
            .expect("recovered workspace admits replacement work");
        resolver_release.add_permits(1);
        while runtime.active_reaper_jobs() != 0 {
            tokio::task::yield_now().await;
        }

        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 1);
        drop(replacement);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn cleanup_panic_retains_orphan_marker_until_a_repeat_attempt_succeeds() {
        let retry_started = Arc::new(Semaphore::new(0));
        let retry_release = Arc::new(Semaphore::new(0));
        let actions = Arc::new(RetryActions {
            provider_calls: AtomicUsize::new(0),
            retry_started: retry_started.clone(),
            retry_release: retry_release.clone(),
            panic_first: true,
        });
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(1))
                .await
                .expect("physical identity resolves")
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions::default(),
        );

        runtime.observe(vec![transition(1)]).await;
        let permit = retry_started.acquire().await.expect("retry started");
        permit.forget();
        assert!(registry.orphan_cleanup_pending("thread-1"));
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 2);

        retry_release.add_permits(1);
        while registry.orphan_cleanup_pending("thread-1") {
            tokio::task::yield_now().await;
        }
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 2);
        runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_cancels_and_joins_every_runtime_owned_reaper_cleanup() {
        let drops = Arc::new(AtomicUsize::new(0));
        let actions = Arc::new(ShutdownActions {
            drops: drops.clone(),
        });
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(1))
                .await
                .expect("physical identity resolves")
        );
        let runtime = WorktreeRuntime::start_for_test(
            actions,
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_secs(5),
                reaper_capacity: 1,
                max_parallel_quiesces: 1,
            },
        );
        let observer = runtime.clone();
        let task = tokio::spawn(async move {
            observer.observe(vec![transition(1)]).await;
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        task.await.expect("observer reaches reaper handoff");
        while runtime.active_reaper_jobs() == 0 {
            tokio::task::yield_now().await;
        }

        runtime.shutdown().await;

        assert_eq!(runtime.active_reaper_jobs(), 0);
        assert_eq!(
            drops.load(Ordering::SeqCst),
            2,
            "the timed-out observer future and active reaper future are both dropped"
        );
        assert!(registry.orphan_cleanup_pending("thread-1"));
    }

    #[tokio::test(start_paused = true)]
    async fn five_second_bound_reaps_late_cleanup_and_saturation_retains_guards() {
        let actions = Arc::new(FakeActions::pending());
        let registry = WorkspaceAvailabilityRegistry::new();
        for index in 1..=3 {
            assert!(
                registry
                    .mark_unavailable(transition(index))
                    .await
                    .expect("physical identity resolves")
            );
        }
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_secs(5),
                reaper_capacity: 1,
                max_parallel_quiesces: 3,
            },
        );
        let observer = runtime.clone();
        let task = tokio::spawn(async move {
            observer
                .observe(vec![transition(1), transition(2), transition(3)])
                .await;
        });
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(5)).await;
        task.await.expect("loss observer task");

        while runtime.active_reaper_jobs() == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 4);
        assert_eq!(actions.terminal_calls.load(Ordering::SeqCst), 4);
        assert_eq!(actions.warnings.lock().expect("warning lock").len(), 3);
        for index in 1..=3 {
            assert!(registry.orphan_cleanup_pending(&format!("thread-{index}")));
            assert!(
                registry
                    .guard_thread(&format!("thread-{index}"))
                    .await
                    .is_err()
            );
        }
        runtime.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn hung_reaper_attempts_release_global_permits_at_their_deadline() {
        let actions = Arc::new(FakeActions::pending());
        let registry = WorkspaceAvailabilityRegistry::new();
        for index in 1..=3 {
            assert!(
                registry
                    .mark_unavailable(transition(index))
                    .await
                    .expect("physical identity resolves")
            );
        }
        let runtime = WorktreeRuntime::start_for_test(
            actions.clone(),
            registry.clone(),
            WorktreeRuntimeOptions {
                graceful_timeout: Duration::from_secs(5),
                reaper_capacity: 4,
                max_parallel_quiesces: 2,
            },
        );
        let initial_runtime = runtime.clone();
        let initial = tokio::spawn(async move {
            initial_runtime
                .observe(vec![transition(1), transition(2)])
                .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(actions.provider_calls.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(5)).await;
        initial.await.expect("initial losses reach reaper handoff");
        while actions.provider_calls.load(Ordering::SeqCst) < 4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.active_reaper_jobs(), 2);

        let later_runtime = runtime.clone();
        let later = tokio::spawn(async move {
            later_runtime.observe(vec![transition(3)]).await;
        });
        tokio::task::yield_now().await;
        assert_eq!(
            actions.provider_calls.load(Ordering::SeqCst),
            4,
            "later cleanup waits while every global permit is owned"
        );

        tokio::time::advance(Duration::from_secs(5)).await;
        while actions.provider_calls.load(Ordering::SeqCst) < 5 {
            tokio::task::yield_now().await;
        }
        assert_eq!(runtime.active_reaper_jobs(), 0);
        assert!(!later.is_finished());
        tokio::time::advance(Duration::from_secs(5)).await;
        later.await.expect("later loss reaches its own deadline");
        for index in 1..=3 {
            assert!(registry.orphan_cleanup_pending(&format!("thread-{index}")));
        }
        runtime.shutdown().await;
    }
}

#[cfg(test)]
pub(super) mod removal_test_support {
    use super::*;
    use crate::{
        activity::{ActivityProjection, ActivityRepository},
        production::provider_runtime::{
            BoxRuntimeFuture, ProviderDriver, ProviderDriverFactory, ProviderEvent,
            ProviderLaunchRequest, StartedSession, SupervisorOptions,
        },
        terminal::{TerminalManager, TerminalManagerOptions},
    };
    use serde_json::Value;
    use std::{future::pending, path::PathBuf, sync::atomic::AtomicBool};
    use tokio::sync::Notify;

    #[derive(Clone, Default)]
    pub(crate) struct RemovalTestDriver {
        pub(crate) shutdowns: Arc<AtomicUsize>,
        pub(crate) hold_send: Arc<AtomicBool>,
        pub(crate) send_entered: Arc<Notify>,
        pub(crate) send_release: Arc<Notify>,
        pub(crate) hold_shutdown: Arc<AtomicBool>,
        pub(crate) shutdown_entered: Arc<Notify>,
        pub(crate) shutdown_release: Arc<Notify>,
    }

    impl ProviderDriverFactory for RemovalTestDriver {
        fn create(
            &self,
            _: ProviderLaunchRequest,
        ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
            Box::pin(async { Ok(Arc::new(self.clone()) as Arc<dyn ProviderDriver>) })
        }
    }

    impl ProviderDriver for RemovalTestDriver {
        fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
            Box::pin(async {
                Ok(StartedSession {
                    resume_cursor: Some(json!({"threadId":"native-removal-session"})),
                    ..StartedSession::default()
                })
            })
        }
        fn send(
            &self,
            _: String,
            _: Vec<Value>,
            _: String,
        ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
            Box::pin(async {
                self.send_entered.notify_one();
                if self.hold_send.load(Ordering::SeqCst) {
                    self.send_release.notified().await;
                }
                Ok(Some("turn-1".into()))
            })
        }
        fn interrupt(
            &self,
            _: Option<String>,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn approve(
            &self,
            _: String,
            _: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn answer(
            &self,
            _: String,
            _: Value,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn set_mode(&self, _: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn set_model(&self, _: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn set_options(
            &self,
            _: Vec<Value>,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn rollback(
            &self,
            _: i64,
            _: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
        fn next_event(&self) -> BoxRuntimeFuture<'_, Option<ProviderEvent>> {
            Box::pin(pending())
        }
        fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async {
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
                self.shutdown_entered.notify_one();
                if self.hold_shutdown.load(Ordering::SeqCst) {
                    self.shutdown_release.notified().await;
                }
                Ok(())
            })
        }
    }

    pub(crate) fn launch(thread_id: &str, cwd: PathBuf) -> ProviderLaunchRequest {
        ProviderLaunchRequest {
            thread_id: thread_id.into(),
            activity_causal_revision: 0,
            provider: "codex".into(),
            provider_label: "Codex".into(),
            provider_instance_id: Some("codex".into()),
            binary_path: "in-memory-provider".into(),
            cwd,
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
            model: Some("gpt-5".into()),
            options: Vec::new(),
            custom_models: Vec::new(),
            service_tier: None,
            effort: None,
            agent: None,
            resume_cursor: None,
            environment: Default::default(),
            endpoint: None,
            server_password: None,
            mcp: None,
            codex_home: None,
            open_url: None,
        }
    }

    pub(crate) fn supervisor(
        engine: &OrchestrationEngine,
        driver: Arc<RemovalTestDriver>,
    ) -> Arc<ProviderRuntimeSupervisor> {
        Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            driver,
            ActivityProjection::new(ActivityRepository::new(
                engine.repositories().database().clone(),
            )),
            SupervisorOptions::default(),
        ))
    }

    pub(crate) fn runtime(
        engine: &OrchestrationEngine,
        provider: Arc<ProviderRuntimeSupervisor>,
        registry: WorkspaceAvailabilityRegistry,
    ) -> (WorktreeRuntime, TerminalManager) {
        let terminals = TerminalManager::new(
            Arc::new(super::tests::RuntimeTestPtyBackend::default()),
            TerminalManagerOptions::default(),
        );
        let runtime = WorktreeRuntime::start(
            engine.clone(),
            provider,
            super::tests::runtime_terminal_services(terminals.clone()),
            registry,
        );
        (runtime, terminals)
    }
}
