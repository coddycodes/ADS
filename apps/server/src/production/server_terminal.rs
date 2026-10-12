use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Weak},
};

use serde::Deserialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    cloud::{RelayClientInstallEvent, RelayClientService, RelayClientStatus},
    diagnostics::{
        AttributedProcess, AttributionConfidence, AttributionKind, AttributionScope, BucketMetric,
        CurrentProcessDiagnostics, DiagnosticsMonitor, NativeProcessSampler, NativeResourceSampler,
        ProcessAttributionRegistry, ProcessAttributionTotals, ProcessIdentity,
        ProcessResourceHistory, ProcessResourceTotals, ProcessRow, ProcessSignal,
        RuntimeProcessOwnership, SplitMetric, UiCoverage, UiCoverageStatus,
        bound_diagnostic_string, process_tree_metadata,
        profiler::{client::ingest_client_batch, runtime::ProfilerRuntime},
    },
    persistence::{OrchestrationEvent, Repositories},
    production::orchestration_effects::SetupScriptLaunch,
    provider_usage::{
        CodexRateLimitResetOutcome, ConsumeCodexRateLimitResetResult, ProviderUsageCommandError,
        ProviderUsageProvider, ProviderUsageResult, ProviderUsageService, ProviderUsageSnapshot,
        ProviderUsageStatus, ProviderUsageWindow, RateLimitResetCredits,
    },
    rpc::{RpcRegistry, RpcResult, RpcStreamChunk},
    terminal::{
        InputReadmissionScope, TerminalAttachInput, TerminalError, TerminalInputAdmission,
        TerminalLaunchCommand, TerminalManager, TerminalMetadataEvent, TerminalOpenInput,
        TerminalSessionIdentity,
    },
    worktree_catalog::{
        WorkspaceAdmissionCancellation, WorkspaceAdmissionLease, WorkspaceAvailabilityRegistry,
    },
};

use super::{
    local_servers,
    workspace_availability::{WorkspaceAdmissionController, WorkspaceAdmissionError},
};

const PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS: usize = 160;

pub type JsonFuture = Pin<Box<dyn Future<Output = RpcResult> + Send + 'static>>;
pub type JsonStream = mpsc::Receiver<RpcStreamChunk>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessTreeCleanup {
    StandaloneServer,
    EmbeddedHost,
}

/// Required bridge to Rust domains whose registries are assembled by the production runtime.
/// Implementations return contract-encoded JSON from the in-process native runtime.
pub trait ProductionServerControl: std::fmt::Debug + Send + Sync + 'static {
    fn call(
        &self,
        method: &'static str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> JsonFuture;

    fn subscribe(&self, method: &'static str, cancellation: CancellationToken) -> JsonStream;
}

#[derive(Clone)]
pub struct ServerTerminalServices {
    terminal: TerminalManager,
    process_attribution: ProcessAttributionRegistry,
    process_sampler: Arc<NativeProcessSampler>,
    process_tree_cleanup: ProcessTreeCleanup,
    resource_sampler: Arc<NativeResourceSampler>,
    process_monitor: Arc<DiagnosticsMonitor<NativeResourceSampler>>,
    provider_usage: ProviderUsageService,
    relay: RelayClientService,
    control: Arc<dyn ProductionServerControl>,
    workspace_admission: Option<WorkspaceAdmissionController>,
    /// Stops the [`Self::watch_workspace_changes`] task when terminals shut down.
    workspace_watch: CancellationToken,
    /// `None` when the host runs no profiler; its status RPC then reports it off. Weak: the
    /// production runtime alone owns the profiler, so dropping it after a failed start tears
    /// the profiler down even while detached tasks still hold clones of these services.
    profiler: Option<Weak<ProfilerRuntime>>,
    /// Tags client profiler reports; empty when the host sets none.
    environment_id: Arc<str>,
}

impl ServerTerminalServices {
    pub async fn live_terminal_count(&self) -> usize {
        self.terminal.live_session_count().await
    }

    #[cfg(test)]
    pub(crate) fn terminal_manager_for_test(&self) -> TerminalManager {
        self.terminal.clone()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        terminal: TerminalManager,
        process_sampler: Arc<NativeProcessSampler>,
        resource_sampler: Arc<NativeResourceSampler>,
        process_monitor: Arc<DiagnosticsMonitor<NativeResourceSampler>>,
        provider_usage: ProviderUsageService,
        relay: RelayClientService,
        control: Arc<dyn ProductionServerControl>,
    ) -> Self {
        Self::new_with_process_tree_cleanup(
            terminal,
            process_sampler,
            resource_sampler,
            process_monitor,
            provider_usage,
            relay,
            control,
            ProcessTreeCleanup::EmbeddedHost,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_process_tree_cleanup(
        terminal: TerminalManager,
        process_sampler: Arc<NativeProcessSampler>,
        resource_sampler: Arc<NativeResourceSampler>,
        process_monitor: Arc<DiagnosticsMonitor<NativeResourceSampler>>,
        provider_usage: ProviderUsageService,
        relay: RelayClientService,
        control: Arc<dyn ProductionServerControl>,
        process_tree_cleanup: ProcessTreeCleanup,
    ) -> Self {
        let process_attribution = terminal.process_attribution_registry();
        Self {
            terminal,
            process_attribution,
            process_sampler,
            process_tree_cleanup,
            resource_sampler,
            process_monitor,
            provider_usage,
            relay,
            control,
            workspace_admission: None,
            workspace_watch: CancellationToken::new(),
            profiler: None,
            environment_id: Arc::from(""),
        }
    }

    #[must_use]
    pub(crate) fn with_profiler(mut self, profiler: &Arc<ProfilerRuntime>) -> Self {
        self.profiler = Some(Arc::downgrade(profiler));
        self
    }

    #[must_use]
    pub(crate) fn with_environment_id(mut self, environment_id: &str) -> Self {
        self.environment_id = Arc::from(environment_id);
        self
    }

    #[must_use]
    pub fn with_availability_registry(
        mut self,
        availability_registry: WorkspaceAvailabilityRegistry,
    ) -> Self {
        self.workspace_admission = Some(WorkspaceAdmissionController::registry_only(
            availability_registry,
        ));
        self
    }

    #[must_use]
    pub(crate) fn with_workspace_admission(
        mut self,
        availability_registry: WorkspaceAvailabilityRegistry,
        repositories: Repositories,
    ) -> Self {
        self.workspace_admission = Some(WorkspaceAdmissionController::new(
            availability_registry,
            repositories,
        ));
        self
    }

    pub(crate) async fn freeze_process_ownership(&self) -> RuntimeProcessOwnership {
        let roots = self.process_attribution.freeze_and_snapshot_identities();
        match self
            .process_sampler
            .capture_runtime_process_ownership(roots.clone())
            .await
        {
            Ok(ownership) => ownership,
            Err(error) => {
                let error = bound_diagnostic_string(
                    &error.to_string(),
                    PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
                );
                tracing::warn!(
                    %error,
                    root_count = roots.len(),
                    "failed to capture the initial runtime-owned process closure"
                );
                RuntimeProcessOwnership::roots_only(roots)
            }
        }
    }

    pub(crate) async fn shutdown_with_process_ownership(&self, ownership: RuntimeProcessOwnership) {
        self.workspace_watch.cancel();
        self.terminal.shutdown().await;
        match self
            .process_sampler
            .cleanup_runtime_process_ownership(ownership)
            .await
        {
            Ok(report) if report.failure_count > 0 => {
                tracing::warn!(
                    attempted = report.attempted,
                    succeeded = report.succeeded,
                    failed = report.failure_count,
                    failures = ?report.failures,
                    "runtime-owned process cleanup completed with failures"
                );
            }
            Ok(report) if report.attempted > 0 => {
                tracing::debug!(
                    attempted = report.attempted,
                    succeeded = report.succeeded,
                    "runtime-owned process cleanup completed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                let error = bound_diagnostic_string(
                    &error.to_string(),
                    PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
                );
                tracing::warn!(%error, "failed to inspect remaining runtime-owned processes during shutdown");
            }
        }
        match cleanup_server_descendants(self.process_tree_cleanup, &self.process_sampler).await {
            Ok(Some(report)) if report.failure_count > 0 => {
                tracing::warn!(
                    attempted = report.attempted,
                    succeeded = report.succeeded,
                    failed = report.failure_count,
                    failures = ?report.failures,
                    "standalone descendant cleanup completed with failures"
                );
            }
            Ok(Some(report)) if report.attempted > 0 => {
                tracing::debug!(
                    attempted = report.attempted,
                    succeeded = report.succeeded,
                    "standalone descendant cleanup completed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                let error = bound_diagnostic_string(
                    &error.to_string(),
                    PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
                );
                tracing::warn!(%error, "failed to inspect standalone descendants during shutdown");
            }
        }
    }

    pub async fn shutdown(&self) {
        let ownership = self.freeze_process_ownership().await;
        self.shutdown_with_process_ownership(ownership).await;
    }

    pub async fn close_thread_terminals(&self, thread_id: &str) {
        let _ = self.terminal.close(thread_id, None).await;
    }

    /// Re-admits the ordered-input sessions whose thread's persisted workspace may have
    /// changed, without interrupting their input: each session otherwise keeps the admission it
    /// resolved when it began. A session whose new workspace cannot be admitted is sealed.
    /// Runs until terminals shut down.
    pub fn watch_workspace_changes(&self, mut events: broadcast::Receiver<OrchestrationEvent>) {
        let terminal = self.terminal.clone();
        let availability = self.workspace_admission.clone();
        let shutdown = self.workspace_watch.clone();
        tokio::spawn(async move {
            // One resolution in flight per session, each independent, while later events keep
            // marking sessions pending; a session changed again meanwhile resolves once more,
            // at its latest round. Dropping the set at shutdown aborts the resolutions.
            let mut resolving = tokio::task::JoinSet::<(String, u64)>::new();
            let mut latest = HashMap::<String, (String, u64)>::new();
            let spawn = |resolving: &mut tokio::task::JoinSet<(String, u64)>,
                         input_id: String,
                         thread_id: String,
                         revision: u64| {
                let terminal = terminal.clone();
                let availability = availability.clone();
                resolving.spawn(async move {
                    if let Some(availability) = availability {
                        let _ = readmit_input(
                            &terminal,
                            &availability,
                            &thread_id,
                            &input_id,
                            revision,
                        )
                        .await;
                    }
                    (input_id, revision)
                });
            };
            loop {
                let event = tokio::select! {
                    () = shutdown.cancelled() => return,
                    Some(resolved) = resolving.join_next(), if !resolving.is_empty() => {
                        let Ok((input_id, revision)) = resolved else { continue };
                        match latest.remove(&input_id) {
                            Some((thread_id, wanted)) if wanted > revision => {
                                latest.insert(input_id.clone(), (thread_id.clone(), wanted));
                                spawn(&mut resolving, input_id, thread_id, wanted);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    event = events.recv() => event,
                };
                let scope = match &event {
                    Ok(event) => {
                        let payload = &event.event.payload;
                        match event.event.event_type.as_str() {
                            "thread.created" => {
                                Some(InputReadmissionScope::Thread(&event.event.aggregate_id))
                            }
                            "thread.meta-updated" if payload.get("worktreePath").is_some() => {
                                Some(InputReadmissionScope::Thread(&event.event.aggregate_id))
                            }
                            // Moves the workspace of the project's threads without a worktree.
                            "project.meta-updated" if payload.get("workspaceRoot").is_some() => {
                                Some(InputReadmissionScope::InheritedRootOf(
                                    &event.event.aggregate_id,
                                ))
                            }
                            _ => None,
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            skipped,
                            "terminal input missed orchestration events; re-admitting every input session"
                        );
                        Some(InputReadmissionScope::All)
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let (Some(scope), Some(_)) = (scope, availability.as_ref()) else {
                    continue;
                };
                let (revision, sessions) = terminal.start_input_readmission(scope);
                for (input_id, thread_id) in sessions {
                    let resolving_now = latest.contains_key(&input_id);
                    latest.insert(input_id.clone(), (thread_id.clone(), revision));
                    if !resolving_now {
                        spawn(&mut resolving, input_id, thread_id, revision);
                    }
                }
            }
        });
    }

    pub async fn quiesce_thread_terminals_for_workspace_loss(
        &self,
        thread_id: &str,
    ) -> Result<(), String> {
        self.terminal
            .quiesce_thread_preserving_history(thread_id)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn capture_thread_terminal_identities(
        &self,
        thread_id: &str,
    ) -> Vec<TerminalSessionIdentity> {
        self.terminal
            .capture_thread_session_identities(thread_id)
            .await
    }

    pub(crate) async fn quiesce_terminal_identities_for_workspace_loss(
        &self,
        identities: Vec<TerminalSessionIdentity>,
    ) -> Result<(), String> {
        self.terminal
            .quiesce_sessions_preserving_history_if_current(identities)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn terminal_exists(&self, thread_id: &str, terminal_id: &str) -> bool {
        self.terminal
            .subscribe_metadata()
            .await
            .initial
            .iter()
            .any(|terminal| terminal.thread_id == thread_id && terminal.terminal_id == terminal_id)
    }

    pub async fn launch_setup_script(&self, input: SetupScriptLaunch) -> Result<(), String> {
        let workspace_admission = acquire_terminal_start(
            self.workspace_admission.as_ref(),
            &input.thread_id,
            &input.cwd.to_string_lossy(),
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut terminal_input = TerminalOpenInput::new(
            input.thread_id.clone(),
            input.terminal_id.clone(),
            input.cwd,
            120,
            30,
        );
        terminal_input.worktree_path = Some(input.worktree_path);
        terminal_input.env = input.env;
        let publication_cancellation = workspace_admission
            .as_ref()
            .map_or_else(CancellationToken::new, |admission| {
                admission.loss_cancellation().cancellation_token()
            });
        await_terminal_publication(
            workspace_admission,
            self.terminal
                .open_with_initial_input_and_publication_cancellation(
                    terminal_input,
                    format!("{}\r", input.command),
                    publication_cancellation,
                ),
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

async fn cleanup_server_descendants(
    process_tree_cleanup: ProcessTreeCleanup,
    process_sampler: &NativeProcessSampler,
) -> Result<Option<crate::process::ProcessCleanupReport>, crate::diagnostics::SignalError> {
    match process_tree_cleanup {
        ProcessTreeCleanup::StandaloneServer => process_sampler
            .cleanup_descendants(std::process::id())
            .await
            .map(Some),
        ProcessTreeCleanup::EmbeddedHost => Ok(None),
    }
}

pub fn register_server_terminal_rpc(registry: &mut RpcRegistry, services: ServerTerminalServices) {
    register_control_rpcs(registry, &services);
    register_diagnostics_rpcs(registry, &services);
    register_provider_usage_rpcs(registry, &services);
    register_cloud_rpcs(registry, &services);
    register_terminal_rpcs(registry, &services);
}

fn register_control_rpcs(registry: &mut RpcRegistry, services: &ServerTerminalServices) {
    for method in [
        "server.getConfig",
        "server.getProviderCapabilities",
        "server.getSettings",
        "server.getTraceDiagnostics",
        "server.refreshProviders",
        "server.removeKeybinding",
        "server.updateProvider",
        "server.updateSettings",
        "server.upsertKeybinding",
    ] {
        let control = services.control.clone();
        registry.register_unary(method, move |request, cancellation| {
            control.call(method, request.payload, cancellation)
        });
    }
    for method in ["subscribeServerConfig", "subscribeServerLifecycle"] {
        let control = services.control.clone();
        registry.register_stream(method, move |_request, cancellation| {
            control.subscribe(method, cancellation)
        });
    }
    {
        let terminal = services.terminal.clone();
        registry.register_stream(
            "subscribeDiscoveredLocalServers",
            move |_request, cancellation| {
                discovered_local_servers_stream(terminal.clone(), cancellation)
            },
        );
    }
}

fn discovered_local_servers_stream(
    terminal: TerminalManager,
    cancellation: CancellationToken,
) -> JsonStream {
    spawn_stream(cancellation, move |sender, cancellation| async move {
        let mut discovery = local_servers::Discovery::default();
        loop {
            let terminals = terminal.live_session_pids().await;
            let servers = discovery.scan(&cancellation, terminals).await;
            if cancellation.is_cancelled()
                || sender
                    .send(Ok(vec![json!({
                        "servers": servers,
                        "scannedAt": format_time(OffsetDateTime::now_utc()),
                    })]))
                    .await
                    .is_err()
            {
                return;
            }
            tokio::select! {
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(local_servers::SCAN_INTERVAL) => {}
            }
        }
    })
}

fn register_diagnostics_rpcs(registry: &mut RpcRegistry, services: &ServerTerminalServices) {
    let monitor = services.process_monitor.clone();
    registry.register_unary(
        "server.getProcessDiagnostics",
        move |_request, _cancellation| {
            let monitor = monitor.clone();
            async move { Ok(process_diagnostics_to_wire(monitor.read_current().await)) }
        },
    );

    let monitor = services.process_monitor.clone();
    registry.register_unary(
        "server.getProcessResourceHistory",
        move |request, _cancellation| {
            let monitor = monitor.clone();
            async move {
                let input: ResourceHistoryInput = decode_payload(&request.payload)?;
                Ok(resource_history_to_wire(
                    monitor.read_history(input.window_ms, input.bucket_ms).await,
                ))
            }
        },
    );

    let profiler = services.profiler.clone();
    registry.register_unary(
        "server.getProfilerStatus",
        move |_request, _cancellation| {
            let profiler = profiler.as_ref().and_then(Weak::upgrade);
            async move {
                let Some(profiler) = profiler else {
                    return Ok(json!({
                        "enabled": false,
                        "recordingSince": null,
                        "diskBytes": 0,
                        "eventCount": 0,
                        "droppedEvents": 0,
                        "lastCrashAt": null,
                        "capabilities": {
                            "nativeDumps": "unavailable",
                            "osCrashRecords": "unavailable",
                            "storage": "ok",
                        },
                        "findings": [],
                    }));
                };
                // Status reads the profiler directory, which must not block an async worker. A
                // panicking handler would fail the whole connection; this fails one request.
                tokio::task::spawn_blocking(move || profiler.status())
                    .await
                    .map_err(|_| {
                        json!({
                            "_tag": "ProfilerStatusError",
                            "message": "The profiler status could not be read.",
                        })
                    })
            }
        },
    );
    let environment_id = services.environment_id.clone();
    registry.register_unary(
        "server.reportProfilerClientBatch",
        move |request, _cancellation| {
            let environment_id = environment_id.clone();
            async move { ingest_client_batch(request.payload, &environment_id) }
        },
    );

    let sampler = services.resource_sampler.clone();
    registry.register_unary("server.signalProcess", move |request, _cancellation| {
        let sampler = sampler.clone();
        async move {
            let input: SignalProcessInput = decode_payload(&request.payload)?;
            let expected_identity = decode_process_identity(input.pid, &input.process_key)?;
            let signal = decode_process_signal(&input.signal)?;
            match sampler
                .signal_external_descendant(expected_identity, signal)
                .await
            {
                Ok(()) => Ok(json!({
                    "pid": input.pid,
                    "signal": input.signal,
                    "signaled": true,
                    "message": effect_none(),
                })),
                Err(error) => Ok(json!({
                    "pid": input.pid,
                    "signal": input.signal,
                    "signaled": false,
                    "message": effect_some(Value::String(bound_diagnostic_string(
                        &error.to_string(),
                        PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
                    ))),
                })),
            }
        }
    });
}

fn register_provider_usage_rpcs(registry: &mut RpcRegistry, services: &ServerTerminalServices) {
    let usage = services.provider_usage.clone();
    registry.register_unary("server.getProviderUsage", move |_request, _cancellation| {
        let usage = usage.clone();
        async move { Ok(provider_usage_to_wire(usage.read().await)) }
    });

    let usage = services.provider_usage.clone();
    registry.register_unary(
        "server.refreshProviderUsage",
        move |request, _cancellation| {
            let usage = usage.clone();
            async move {
                let input: RefreshProviderUsageInput = decode_payload(&request.payload)?;
                let providers = input
                    .providers
                    .map(|providers| {
                        providers
                            .into_iter()
                            .map(|provider| match provider.as_str() {
                                "claude" => Ok(ProviderUsageProvider::Claude),
                                "codex" => Ok(ProviderUsageProvider::Codex),
                                _ => Err(invalid_request("provider must be claude or codex")),
                            })
                            .collect::<Result<Vec<_>, Value>>()
                    })
                    .transpose()?;
                let refreshed = if input.force {
                    usage.refresh_forced(providers).await
                } else {
                    usage.refresh(providers).await
                };
                Ok(provider_usage_to_wire(refreshed))
            }
        },
    );

    let usage = services.provider_usage.clone();
    registry.register_unary(
        "server.consumeCodexRateLimitReset",
        move |request, _cancellation| {
            let usage = usage.clone();
            async move {
                let input: ConsumeCodexRateLimitResetInput = decode_payload(&request.payload)?;
                usage
                    .consume_codex_rate_limit_reset(&input.request_id)
                    .await
                    .map(consume_codex_rate_limit_reset_to_wire)
                    .map_err(provider_usage_reset_error_to_wire)
            }
        },
    );
}

fn register_cloud_rpcs(registry: &mut RpcRegistry, services: &ServerTerminalServices) {
    let relay = services.relay.clone();
    registry.register_unary(
        "cloud.getRelayClientStatus",
        move |_request, _cancellation| {
            let relay = relay.clone();
            async move { Ok(relay_status_to_wire(relay.resolve().await)) }
        },
    );

    let relay = services.relay.clone();
    registry.register_stream("cloud.installRelayClient", move |_request, cancellation| {
        let relay = relay.clone();
        spawn_stream(cancellation, move |sender, cancellation| async move {
            let result = tokio::select! {
                () = cancellation.cancelled() => return,
                result = relay.install() => result,
            };
            match result {
                Ok(events) => {
                    for event in events {
                        if sender
                            .send(Ok(vec![relay_event_to_wire(event)]))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(message) => {
                    let _ = sender
                        .send(Err(json!({
                            "_tag": "RelayClientInstallFailedError",
                            "reason": "download_failed",
                            "message": message,
                        })))
                        .await;
                }
            }
        })
    });
}

fn register_terminal_rpcs(registry: &mut RpcRegistry, services: &ServerTerminalServices) {
    let terminal = services.terminal.clone();
    let availability = services.workspace_admission.clone();
    registry.register_unary("terminal.open", move |request, _cancellation| {
        let terminal = terminal.clone();
        let availability = availability.clone();
        async move {
            let input: TerminalStartPayload = decode_payload(&request.payload)?;
            let workspace_admission =
                acquire_terminal_start(availability.as_ref(), &input.thread_id, &input.cwd).await?;
            let publication_cancellation = workspace_admission
                .as_ref()
                .map_or_else(CancellationToken::new, |admission| {
                    admission.loss_cancellation().cancellation_token()
                });
            await_terminal_publication(
                workspace_admission,
                terminal.open_with_publication_cancellation(
                    input.into_open(false)?,
                    publication_cancellation,
                ),
            )
            .await
            .map(|snapshot| serde_json::to_value(snapshot).expect("terminal snapshot serializes"))
        }
    });

    let terminal = services.terminal.clone();
    let availability = services.workspace_admission.clone();
    registry.register_stream("terminal.attach", move |request, cancellation| {
        let terminal = terminal.clone();
        let availability = availability.clone();
        spawn_stream(cancellation, move |sender, cancellation| async move {
            let input: TerminalAttachPayload = match decode_payload(&request.payload) {
                Ok(input) => input,
                Err(error) => { let _ = sender.send(Err(error)).await; return; }
            };
            let workspace_admission = if input.restart_if_not_running {
                match acquire_terminal_attach(availability.as_ref(), &input).await {
                    Ok(admission) => admission,
                    Err(error) => { let _ = sender.send(Err(error)).await; return; }
                }
            } else { None };
            let input = match input.into_attach(request.payload.get("sizeClaim")) {
                Ok(input) => input,
                Err(error) => { let _ = sender.send(Err(error)).await; return; }
            };
            let publication_cancellation = workspace_admission
                .as_ref()
                .map_or_else(CancellationToken::new, |admission| {
                    admission.loss_cancellation().cancellation_token()
                });
            let mut attachment = match await_terminal_publication(
                workspace_admission,
                terminal.attach_with_publication_cancellation(
                    input,
                    publication_cancellation,
                ),
            ).await {
                Ok(attachment) => attachment,
                Err(error) => { let _ = sender.send(Err(error)).await; return; }
            };
            if sender.send(Ok(vec![json!({
                "type": "snapshot",
                "snapshot": attachment.initial,
            })])).await.is_err() {
                return;
            }
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    event = attachment.recv() => match event {
                        Some(event) => if sender.send(Ok(vec![serde_json::to_value(event).expect("terminal event serializes")])).await.is_err() { return; },
                        None => return,
                    }
                }
            }
        })
    });

    let terminal = services.terminal.clone();
    let availability = services.workspace_admission.clone();
    registry.register_unary("terminal.write", move |request, _cancellation| {
        let terminal = terminal.clone();
        let availability = availability.clone();
        async move {
            let input: TerminalWritePayload = decode_payload(&request.payload)?;
            let _workspace_admission =
                acquire_terminal_thread(availability.as_ref(), &input.thread_id).await?;
            terminal
                .write(&input.thread_id, &input.terminal_id, &input.data)
                .await
                .map_err(terminal_error)?;
            Ok(Value::Null)
        }
    });
    register_ordered_input_rpcs(registry, services);
    register_terminal_unary(
        registry,
        "terminal.resize",
        services.terminal.clone(),
        |terminal, payload| {
            Box::pin(async move {
                let input: TerminalResizePayload = decode_payload(&payload)?;
                terminal
                    .resize(
                        &input.thread_id,
                        &input.terminal_id,
                        input.cols,
                        input.rows,
                        validate_terminal_size_claim(payload.get("sizeClaim"))?,
                    )
                    .await
                    .map_err(terminal_error)?;
                Ok(Value::Null)
            })
        },
    );
    register_terminal_unary(
        registry,
        "terminal.clear",
        services.terminal.clone(),
        |terminal, payload| {
            Box::pin(async move {
                let input: TerminalSessionPayload = decode_payload(&payload)?;
                terminal
                    .clear(&input.thread_id, &input.terminal_id)
                    .await
                    .map_err(terminal_error)?;
                Ok(Value::Null)
            })
        },
    );
    let terminal = services.terminal.clone();
    let availability = services.workspace_admission.clone();
    registry.register_unary("terminal.restart", move |request, _cancellation| {
        let terminal = terminal.clone();
        let availability = availability.clone();
        async move {
            let input: TerminalStartPayload = decode_payload(&request.payload)?;
            let workspace_admission =
                acquire_terminal_start(availability.as_ref(), &input.thread_id, &input.cwd).await?;
            let publication_cancellation = workspace_admission
                .as_ref()
                .map_or_else(CancellationToken::new, |admission| {
                    admission.loss_cancellation().cancellation_token()
                });
            await_terminal_publication(
                workspace_admission,
                terminal.restart_with_publication_cancellation(
                    input.into_open(true)?,
                    publication_cancellation,
                ),
            )
            .await
            .map(|snapshot| serde_json::to_value(snapshot).expect("terminal snapshot serializes"))
        }
    });
    register_terminal_unary(
        registry,
        "terminal.close",
        services.terminal.clone(),
        |terminal, payload| {
            Box::pin(async move {
                let input: TerminalClosePayload = decode_payload(&payload)?;
                terminal
                    .close(&input.thread_id, input.terminal_id.as_deref())
                    .await
                    .map_err(terminal_error)?;
                Ok(Value::Null)
            })
        },
    );

    let terminal = services.terminal.clone();
    registry.register_stream("subscribeTerminalEvents", move |_request, cancellation| {
        let mut events = terminal.subscribe_events();
        spawn_stream(cancellation, move |sender, cancellation| async move {
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    event = events.recv() => match event {
                        Ok(event) => if sender.send(Ok(vec![serde_json::to_value(event).expect("terminal event serializes")])).await.is_err() { return; },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            }
        })
    });

    let terminal = services.terminal.clone();
    registry.register_stream("subscribeTerminalMetadata", move |_request, cancellation| {
        let terminal = terminal.clone();
        spawn_stream(cancellation, move |sender, cancellation| async move {
            let mut attachment = terminal.subscribe_metadata().await;
            if sender.send(Ok(vec![json!({ "type": "snapshot", "terminals": attachment.initial })])).await.is_err() {
                return;
            }
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    event = attachment.recv() => match event {
                        Some(event) => if sender.send(Ok(vec![terminal_metadata_to_wire(event)])).await.is_err() { return; },
                        None => return,
                    }
                }
            }
        })
    });
}

async fn acquire_terminal_start(
    admission: Option<&WorkspaceAdmissionController>,
    thread_id: &str,
    cwd: &str,
) -> Result<Option<WorkspaceAdmissionLease>, Value> {
    let Some(admission) = admission else {
        return Ok(None);
    };
    admission
        .acquire_thread(thread_id, [std::path::Path::new(cwd)])
        .await
        .map(Some)
        .map_err(workspace_admission_error)
}

async fn acquire_terminal_attach(
    admission: Option<&WorkspaceAdmissionController>,
    input: &TerminalAttachPayload,
) -> Result<Option<WorkspaceAdmissionLease>, Value> {
    let Some(admission) = admission else {
        return Ok(None);
    };
    if let Some(cwd) = input.cwd.as_deref() {
        return admission
            .acquire_thread(&input.thread_id, [std::path::Path::new(cwd)])
            .await
            .map(Some)
            .map_err(workspace_admission_error);
    }
    acquire_terminal_thread(Some(admission), &input.thread_id).await
}

fn register_ordered_input_rpcs(registry: &mut RpcRegistry, services: &ServerTerminalServices) {
    let terminal = services.terminal.clone();
    let availability = services.workspace_admission.clone();
    registry.register_unary_with_context(
        "terminal.beginInput",
        move |request, context, _cancellation| {
            let terminal = terminal.clone();
            let availability = availability.clone();
            async move {
                let input: TerminalInputBeginPayload = decode_payload(&request.payload)?;
                // The input session's workspace resolution: its lease keeps this admission until
                // it ends, workspace loss seals it, and a workspace change re-admits it (#82).
                let epoch = terminal.input_readmission_epoch();
                let admission =
                    acquire_terminal_input(availability.as_ref(), &input.thread_id).await?;
                let loss = admission
                    .as_ref()
                    .map(|(admission, _)| admission.loss_cancellation());
                let session_admission = admission.map(input_admission);
                let lease = await_admission_loss(
                    loss,
                    terminal.begin_input(
                        context.connection_id(),
                        context.connection_closed(),
                        &input.thread_id,
                        &input.terminal_id,
                        input.attachment_sequence,
                        session_admission,
                    ),
                )
                .await?;
                // A change that committed while this resolved re-admitted the sessions it found
                // before this one registered; admit the current workspace here instead, and
                // report it if it cannot be admitted rather than return a sealed session.
                if terminal.input_readmission_epoch() != epoch
                    && let Some(availability) = availability.as_ref()
                {
                    // The client never learns this lease if the request ends here.
                    let unclaimed = UnclaimedInputLease {
                        terminal: &terminal,
                        connection_id: context.connection_id(),
                        thread_id: &input.thread_id,
                        terminal_id: &input.terminal_id,
                        input_id: Some(&lease.input_id),
                    };
                    // A round of its own, so later rounds see it pending and cover or supersede it.
                    let (revision, _) = terminal
                        .start_input_readmission(InputReadmissionScope::Session(&lease.input_id));
                    let readmitted = readmit_input(
                        &terminal,
                        availability,
                        &input.thread_id,
                        &lease.input_id,
                        revision,
                    )
                    .await;
                    // A concurrent round may have sealed it even when this result was ignored.
                    if let Err(closed) = terminal.check_input_open(&lease.input_id) {
                        return Err(readmitted.err().unwrap_or_else(|| terminal_error(closed)));
                    }
                    unclaimed.disarm();
                }
                Ok(serde_json::to_value(lease).expect("input lease serializes"))
            }
        },
    );

    let terminal = services.terminal.clone();
    registry.register_unary_with_context(
        "terminal.writeInput",
        move |request, context, _cancellation| {
            let terminal = terminal.clone();
            async move {
                if let Some(data) = request.payload.get("data").and_then(Value::as_str)
                    && (data.is_empty() || data.len() > 16 * 1024)
                {
                    if let (Some(thread_id), Some(terminal_id), Some(input_id)) = (
                        request.payload.get("threadId").and_then(Value::as_str),
                        request.payload.get("terminalId").and_then(Value::as_str),
                        request.payload.get("inputId").and_then(Value::as_str),
                    ) {
                        let _ = terminal.cancel_input(context.connection_id(), thread_id, terminal_id, input_id);
                    }
                    return Err(json!({"_tag":"TerminalInputError", "code":"capacity", "message":"Terminal input exceeded its frame limit. Reconnect input before typing again."}));
                }
                let input: TerminalInputFramePayload = decode_payload(&request.payload)?;
                // In memory only: the session's lease already holds the workspace admission
                // `terminal.beginInput` took, so a keystroke never waits for the database.
                let frame = terminal.prepare_input_write(context.connection_id(), &input.thread_id, &input.terminal_id, &input.input_id, input.sequence, &input.data).map_err(terminal_error)?;
                // Loss of the session's current admission seals it, which fails this frame
                // wherever it waits; report that loss rather than the sealed session.
                let seal = frame.seal();
                let result = frame.write().await.map_err(|error| {
                    seal.lost_admission()
                        .as_deref()
                        .and_then(|admission| admission.downcast_ref::<WorkspaceAdmissionLease>())
                        .and_then(|admission| admission.loss_cancellation().unavailable())
                        .map_or_else(
                            || terminal_error(error),
                            |unavailable| {
                                serde_json::to_value(unavailable)
                                    .expect("workspace unavailable error serializes")
                            },
                        )
                });
                if result.is_err() {
                    let _ = terminal.cancel_input(
                        context.connection_id(),
                        &input.thread_id,
                        &input.terminal_id,
                        &input.input_id,
                    );
                }
                result
                    .map(|ack| serde_json::to_value(ack).expect("input acknowledgement serializes"))
            }
        },
    );

    let terminal = services.terminal.clone();
    registry.register_unary_with_context(
        "terminal.cancelInput",
        move |request, context, _cancellation| {
            let terminal = terminal.clone();
            async move {
                let input: TerminalInputCancelPayload = decode_payload(&request.payload)?;
                terminal
                    .cancel_input(
                        context.connection_id(),
                        &input.thread_id,
                        &input.terminal_id,
                        &input.input_id,
                    )
                    .map_err(terminal_error)?;
                Ok(Value::Null)
            }
        },
    );
}

async fn acquire_terminal_thread(
    admission: Option<&WorkspaceAdmissionController>,
    thread_id: &str,
) -> Result<Option<WorkspaceAdmissionLease>, Value> {
    let Some(admission) = admission else {
        return Ok(None);
    };
    admission
        .acquire_thread(thread_id, std::iter::empty())
        .await
        .map(Some)
        .map_err(workspace_admission_error)
}

/// Cancels an input lease whose `inputId` never reached the client, when the begin request
/// ends before it returns.
struct UnclaimedInputLease<'a> {
    terminal: &'a TerminalManager,
    connection_id: uuid::Uuid,
    thread_id: &'a str,
    terminal_id: &'a str,
    input_id: Option<&'a str>,
}

impl UnclaimedInputLease<'_> {
    fn disarm(mut self) {
        self.input_id = None;
    }
}

impl Drop for UnclaimedInputLease<'_> {
    fn drop(&mut self) {
        if let Some(input_id) = self.input_id {
            let _ = self.terminal.cancel_input(
                self.connection_id,
                self.thread_id,
                self.terminal_id,
                input_id,
            );
        }
    }
}

/// Admits the thread's persisted workspace for an input session, with the project whose root
/// it is when the thread has no worktree of its own.
async fn acquire_terminal_input(
    admission: Option<&WorkspaceAdmissionController>,
    thread_id: &str,
) -> Result<Option<(WorkspaceAdmissionLease, Option<String>)>, Value> {
    let Some(admission) = admission else {
        return Ok(None);
    };
    admission
        .acquire_thread_with_origin(thread_id, std::iter::empty())
        .await
        .map(Some)
        .map_err(workspace_admission_error)
}

fn input_admission(
    (admission, inherited_root_of): (WorkspaceAdmissionLease, Option<String>),
) -> TerminalInputAdmission {
    TerminalInputAdmission::new(
        admission.loss_cancellation().cancellation_token(),
        admission,
        inherited_root_of,
    )
}

/// Gives a live input session the thread's current workspace admission as the result of
/// re-admission round `revision`, without interrupting its input. A workspace that cannot be
/// admitted (unavailable, or unresolvable) seals the session and returns why, unless a newer
/// round superseded this one.
async fn readmit_input(
    terminal: &TerminalManager,
    availability: &WorkspaceAdmissionController,
    thread_id: &str,
    input_id: &str,
    revision: u64,
) -> Result<(), Value> {
    match acquire_terminal_input(Some(availability), thread_id).await {
        Ok(admission) => {
            terminal.readmit_input(input_id, revision, admission.map(input_admission));
            Ok(())
        }
        // A newer round superseded this one; its result decides.
        Err(_) if !terminal.readmit_input(input_id, revision, None) => Ok(()),
        Err(error) => {
            tracing::warn!(
                thread_id,
                "terminal input could not admit the thread's changed workspace; sealed the session"
            );
            Err(error)
        }
    }
}

async fn await_terminal_publication<T, F>(
    admission: Option<WorkspaceAdmissionLease>,
    operation: F,
) -> Result<T, Value>
where
    F: Future<Output = Result<T, TerminalError>>,
{
    let loss = admission
        .as_ref()
        .map(WorkspaceAdmissionLease::loss_cancellation);
    let result = await_admission_loss(loss, operation).await;
    drop(admission);
    result
}

/// Runs `operation` unless the workspace admission behind `loss` is lost first, in which case
/// the structured loss is the result.
async fn await_admission_loss<T, F>(
    loss: Option<WorkspaceAdmissionCancellation>,
    operation: F,
) -> Result<T, Value>
where
    F: Future<Output = Result<T, TerminalError>>,
{
    let result = if let Some(loss) = &loss {
        tokio::pin!(operation);
        tokio::select! {
            biased;
            () = loss.cancelled() => {
                let unavailable = loss
                    .unavailable()
                    .expect("workspace loss cancellation retains its error");
                return Err(serde_json::to_value(unavailable)
                    .expect("workspace unavailable error serializes"));
            }
            result = &mut operation => result,
        }
    } else {
        operation.await
    };
    if matches!(result, Err(TerminalError::PublicationCancelled))
        && let Some(unavailable) = loss.and_then(|loss| loss.unavailable())
    {
        return Err(
            serde_json::to_value(unavailable).expect("workspace unavailable error serializes")
        );
    }
    result.map_err(terminal_error)
}

fn workspace_admission_error(error: WorkspaceAdmissionError) -> Value {
    match error {
        WorkspaceAdmissionError::Unavailable(error) => {
            serde_json::to_value(error).expect("workspace unavailable error serializes")
        }
        WorkspaceAdmissionError::Identity(error) => {
            serde_json::to_value(error).expect("workspace identity error serializes")
        }
        WorkspaceAdmissionError::Resolution(error) => terminal_error(TerminalError::Io(error)),
    }
}

type TerminalUnary = fn(TerminalManager, Value) -> JsonFuture;

fn register_terminal_unary(
    registry: &mut RpcRegistry,
    method: &'static str,
    terminal: TerminalManager,
    handler: TerminalUnary,
) {
    registry.register_unary(method, move |request, _cancellation| {
        handler(terminal.clone(), request.payload)
    });
}

fn spawn_stream<F, Fut>(cancellation: CancellationToken, task: F) -> JsonStream
where
    F: FnOnce(mpsc::Sender<RpcStreamChunk>, CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(task(sender, cancellation));
    receiver
}

const TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH: usize = 4_096;
const TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH: usize = 8_192;
const TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT: usize = 64;
const TERMINAL_LAUNCH_LABEL_MAX_LENGTH: usize = 128;
const TERMINAL_SIZE_CLAIM_MAX_LENGTH: usize = 128;
const PROVIDER_TERMINAL_ACTIVITY_SLUG_MAX_LENGTH: usize = 64;

fn is_ecmascript_trim_character(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000a}'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn trim_ecmascript(value: &str) -> &str {
    value.trim_matches(is_ecmascript_trim_character)
}

fn is_provider_terminal_activity_slug(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn validate_terminal_size_claim(value: Option<&Value>) -> Result<Option<String>, Value> {
    let Some(value) = value else {
        return Ok(None);
    };
    let claim = value
        .as_str()
        .ok_or_else(|| invalid_request("sizeClaim must be a string"))?;
    let claim = trim_ecmascript(claim);
    if claim.is_empty() {
        return Err(invalid_request("sizeClaim must not be empty"));
    }
    if claim.encode_utf16().count() > TERMINAL_SIZE_CLAIM_MAX_LENGTH {
        return Err(invalid_request("sizeClaim is too long"));
    }
    Ok(Some(claim.to_owned()))
}

fn validate_terminal_launch_command(
    command: Option<TerminalLaunchCommand>,
) -> Result<Option<TerminalLaunchCommand>, Value> {
    let Some(mut command) = command else {
        return Ok(None);
    };

    command.executable = trim_ecmascript(&command.executable).to_owned();
    if command.executable.is_empty() {
        return Err(invalid_request("command executable must not be empty"));
    }
    if command.executable.encode_utf16().count() > TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH {
        return Err(invalid_request("command executable is too long"));
    }
    if command.args.len() > TERMINAL_LAUNCH_ARGUMENT_MAX_COUNT {
        return Err(invalid_request("command has too many arguments"));
    }
    if command
        .args
        .iter()
        .any(|argument| argument.encode_utf16().count() > TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH)
    {
        return Err(invalid_request("command argument is too long"));
    }
    if let Some(label) = command.label.as_mut() {
        *label = trim_ecmascript(label).to_owned();
        if label.is_empty() {
            return Err(invalid_request("command label must not be empty"));
        }
        if label.encode_utf16().count() > TERMINAL_LAUNCH_LABEL_MAX_LENGTH {
            return Err(invalid_request("command label is too long"));
        }
    }
    if let Some(activity) = command.activity.as_mut() {
        activity.driver_kind = trim_ecmascript(&activity.driver_kind).to_owned();
        if activity.driver_kind.encode_utf16().count() > PROVIDER_TERMINAL_ACTIVITY_SLUG_MAX_LENGTH
            || !is_provider_terminal_activity_slug(&activity.driver_kind)
        {
            return Err(invalid_request("command activity driver kind is invalid"));
        }
        activity.provider_instance_id = trim_ecmascript(&activity.provider_instance_id).to_owned();
        if activity.provider_instance_id.encode_utf16().count()
            > PROVIDER_TERMINAL_ACTIVITY_SLUG_MAX_LENGTH
            || !is_provider_terminal_activity_slug(&activity.provider_instance_id)
        {
            return Err(invalid_request(
                "command activity provider instance ID is invalid",
            ));
        }
    }
    // Mirrors the contract's TerminalEnvSchema: this env is broadcast in
    // terminal summaries, and one invalid entry would fail every client's decode.
    if let Some(env) = command.env.as_ref() {
        if env.len() > TERMINAL_COMMAND_ENV_MAX_ENTRIES {
            return Err(invalid_request("command env has too many entries"));
        }
        if env.iter().any(|(key, value)| {
            !is_terminal_env_key(key)
                || value.encode_utf16().count() > TERMINAL_COMMAND_ENV_VALUE_MAX_LENGTH
        }) {
            return Err(invalid_request("command env is invalid"));
        }
    }

    Ok(Some(command))
}

const TERMINAL_COMMAND_ENV_MAX_ENTRIES: usize = 128;
const TERMINAL_COMMAND_ENV_KEY_MAX_LENGTH: usize = 128;
const TERMINAL_COMMAND_ENV_VALUE_MAX_LENGTH: usize = 8_192;

/// `^[A-Za-z_][A-Za-z0-9_]*$`, at most 128 characters.
fn is_terminal_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= TERMINAL_COMMAND_ENV_KEY_MAX_LENGTH
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|rest| rest.is_ascii_alphanumeric() || rest == '_')
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalStartPayload {
    thread_id: String,
    terminal_id: String,
    cwd: String,
    worktree_path: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    command: Option<TerminalLaunchCommand>,
    #[serde(default)]
    center_panel: bool,
}

impl TerminalStartPayload {
    fn into_open(self, dimensions_required: bool) -> Result<TerminalOpenInput, Value> {
        let cols = self
            .cols
            .or((!dimensions_required).then_some(120))
            .ok_or_else(|| invalid_request("cols is required"))?;
        let rows = self
            .rows
            .or((!dimensions_required).then_some(30))
            .ok_or_else(|| invalid_request("rows is required"))?;
        Ok(TerminalOpenInput {
            thread_id: self.thread_id,
            terminal_id: self.terminal_id,
            cwd: PathBuf::from(self.cwd),
            worktree_path: self.worktree_path.map(PathBuf::from),
            cols,
            rows,
            env: self.env,
            command: validate_terminal_launch_command(self.command)?,
            center_panel: self.center_panel,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalAttachPayload {
    thread_id: String,
    terminal_id: String,
    cwd: Option<String>,
    worktree_path: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    restart_if_not_running: bool,
    command: Option<TerminalLaunchCommand>,
    #[serde(default)]
    center_panel: bool,
}

impl TerminalAttachPayload {
    fn into_attach(self, size_claim: Option<&Value>) -> Result<TerminalAttachInput, Value> {
        Ok(TerminalAttachInput {
            size_claim: validate_terminal_size_claim(size_claim)?,
            thread_id: self.thread_id,
            terminal_id: self.terminal_id,
            cwd: self.cwd.map(PathBuf::from),
            worktree_path: self.worktree_path.map(PathBuf::from),
            cols: self.cols,
            rows: self.rows,
            env: self.env,
            restart_if_not_running: self.restart_if_not_running,
            command: validate_terminal_launch_command(self.command)?,
            center_panel: self.center_panel,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalSessionPayload {
    thread_id: String,
    terminal_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalWritePayload {
    thread_id: String,
    terminal_id: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalInputBeginPayload {
    thread_id: String,
    terminal_id: String,
    attachment_sequence: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalInputFramePayload {
    thread_id: String,
    terminal_id: String,
    input_id: String,
    sequence: u64,
    data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalInputCancelPayload {
    thread_id: String,
    terminal_id: String,
    input_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalResizePayload {
    thread_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalClosePayload {
    thread_id: String,
    terminal_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceHistoryInput {
    window_ms: u64,
    bucket_ms: u64,
}
#[derive(Deserialize)]
struct SignalProcessInput {
    pid: u32,
    #[serde(rename = "processKey")]
    process_key: String,
    signal: String,
}
#[derive(Deserialize)]
struct RefreshProviderUsageInput {
    providers: Option<Vec<String>>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsumeCodexRateLimitResetInput {
    request_id: String,
}

fn decode_payload<T: for<'de> Deserialize<'de>>(payload: &Value) -> Result<T, Value> {
    serde_json::from_value(payload.clone()).map_err(|error| invalid_request(&error.to_string()))
}

fn decode_process_identity(pid: u32, process_key: &str) -> Result<ProcessIdentity, Value> {
    let (key_pid, started_at) = process_key
        .split_once(':')
        .ok_or_else(|| invalid_request("processKey must contain pid and start identity"))?;
    let key_pid = key_pid
        .parse::<u32>()
        .map_err(|_| invalid_request("processKey contains an invalid pid"))?;
    let started_at = started_at
        .parse::<u64>()
        .map_err(|_| invalid_request("processKey contains an invalid start identity"))?;
    if key_pid != pid {
        return Err(invalid_request("processKey pid does not match pid"));
    }
    Ok(ProcessIdentity { pid, started_at })
}

fn decode_process_signal(signal: &str) -> Result<ProcessSignal, Value> {
    match signal {
        "SIGINT" => Ok(ProcessSignal::Interrupt),
        "SIGKILL" => Ok(ProcessSignal::Kill),
        _ => Err(invalid_request("signal must be SIGINT or SIGKILL")),
    }
}

fn provider_usage_to_wire(result: ProviderUsageResult) -> Value {
    json!({
        "readAt": format_time(result.read_at),
        "isFetching": result.is_fetching,
        "providers": result.providers.into_iter().map(provider_usage_snapshot_to_wire).collect::<Vec<_>>(),
    })
}

fn provider_usage_snapshot_to_wire(snapshot: ProviderUsageSnapshot) -> Value {
    json!({
        "provider": match snapshot.provider { ProviderUsageProvider::Claude => "claude", ProviderUsageProvider::Codex => "codex" },
        "status": match snapshot.status {
            ProviderUsageStatus::Idle => "idle", ProviderUsageStatus::Fetching => "fetching",
            ProviderUsageStatus::Ok => "ok", ProviderUsageStatus::Error => "error",
            ProviderUsageStatus::Unavailable => "unavailable",
        },
        "session": snapshot.session.map(provider_usage_window_to_wire),
        "weekly": snapshot.weekly.map(provider_usage_window_to_wire),
        "fableWeekly": snapshot.fable_weekly.map(provider_usage_window_to_wire),
        "planType": snapshot.plan_type,
        "rateLimitResetCredits": snapshot.rate_limit_reset_credits.map(rate_limit_reset_credits_to_wire),
        "updatedAt": format_time(snapshot.updated_at),
        "error": snapshot.error,
        "metadata": snapshot.metadata,
    })
}

fn rate_limit_reset_credits_to_wire(credits: RateLimitResetCredits) -> Value {
    json!({
        "availableCount": credits.available_count,
        "totalEarnedCount": credits.total_earned_count,
        "nextExpiresAt": credits.next_expires_at.map(format_time),
    })
}

fn consume_codex_rate_limit_reset_to_wire(result: ConsumeCodexRateLimitResetResult) -> Value {
    json!({
        "outcome": match result.outcome {
            CodexRateLimitResetOutcome::Reset => "reset",
            CodexRateLimitResetOutcome::NothingToReset => "nothingToReset",
            CodexRateLimitResetOutcome::NoCredit => "noCredit",
            CodexRateLimitResetOutcome::AlreadyRedeemed => "alreadyRedeemed",
        },
        "usage": provider_usage_to_wire(result.usage),
    })
}

fn provider_usage_reset_error_to_wire(error: ProviderUsageCommandError) -> Value {
    json!({
        "_tag": "ServerProviderUsageResetError",
        "message": error.message,
    })
}

fn provider_usage_window_to_wire(window: ProviderUsageWindow) -> Value {
    json!({
        "usedPercent": window.used_percent,
        "windowMinutes": window.window_minutes,
        "resetsAt": window.resets_at.map(format_time),
        "resetDescription": window.reset_description,
    })
}

fn process_diagnostics_to_wire(current: CurrentProcessDiagnostics) -> Value {
    let read_at_ms = current.snapshot.as_ref().map_or_else(
        || OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
        |snapshot| snapshot.sampled_at_ms,
    );
    let error = current.error.map_or_else(effect_none, |message| {
        effect_some(json!({
            "message": bound_diagnostic_string(
                &message,
                PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
            ),
        }))
    });
    let Some(snapshot) = current.snapshot else {
        return json!({
            "serverPid": std::process::id(),
            "readAt": format_epoch_ms(read_at_ms),
            "totals": attribution_totals_to_wire(ProcessAttributionTotals::default()),
            "uiCoverage": ui_coverage_to_wire(&UiCoverage::default()),
            "processes": [],
            "error": error,
        });
    };
    let server_pid = snapshot.server_identity.pid;

    json!({
        "serverPid": server_pid,
        "readAt": format_epoch_ms(read_at_ms),
        "totals": attribution_totals_to_wire(snapshot.totals),
        "uiCoverage": ui_coverage_to_wire(&snapshot.ui_coverage),
        "processes": attributed_processes_to_wire(&snapshot.processes, &snapshot.native_rows),
        "error": error,
    })
}

fn resource_history_to_wire(history: ProcessResourceHistory) -> Value {
    json!({
        "readAt": format_epoch_ms(history.read_at_ms),
        "windowMs": history.window_ms,
        "bucketMs": history.bucket_ms,
        "sampleIntervalMs": history.sample_interval_ms,
        "retainedSampleCount": history.retained_sample_count,
        "cpuSecondsApprox": split_f64_to_wire(history.total_cpu_seconds_approx),
        "uiCoverage": ui_coverage_to_wire(&history.ui_coverage),
        "buckets": history.buckets.into_iter().map(|bucket| json!({
            "startedAt": format_epoch_ms(bucket.started_at_ms),
            "endedAt": format_epoch_ms(bucket.ended_at_ms),
            "cpuPercent": bucket_f64_to_wire(bucket.cpu_percent),
            "rssBytes": bucket_u64_to_wire(bucket.rss_bytes),
            "maxProcessCount": split_usize_to_wire(bucket.max_process_count),
        })).collect::<Vec<_>>(),
        "processes": history.processes.into_iter().map(|process| json!({
            "processKey": process.process_key, "pid": process.pid,
            "ppid": process.ppid,
            "command": process.command,
            "depth": process.depth,
            "scope": attribution_scope_to_wire(process.scope),
            "kind": attribution_kind_to_wire(process.kind),
            "label": process.label,
            "confidence": attribution_confidence_to_wire(process.confidence),
            "firstSeenAt": format_epoch_ms(process.first_seen_at_ms), "lastSeenAt": format_epoch_ms(process.last_seen_at_ms),
            "currentCpuPercent": process.current_cpu_percent, "avgCpuPercent": process.avg_cpu_percent,
            "maxCpuPercent": process.max_cpu_percent, "cpuSecondsApprox": process.cpu_seconds_approx,
            "currentRssBytes": process.current_rss_bytes, "maxRssBytes": process.max_rss_bytes,
            "sampleCount": process.sample_count,
        })).collect::<Vec<_>>(),
        "error": history.error.map_or_else(effect_none, |message| effect_some(json!({
            "failureTag": "ProcessDiagnosticsQueryFailedError",
            "message": bound_diagnostic_string(
                &message,
                PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
            ),
        }))),
    })
}

fn attributed_processes_to_wire(
    processes: &[AttributedProcess],
    native_rows: &[ProcessRow],
) -> Vec<Value> {
    let metadata = process_tree_metadata(
        native_rows,
        processes.iter().map(|process| process.identity),
    );

    processes
        .iter()
        .filter_map(|process| {
            let metadata = metadata.get(&process.identity)?;
            Some(json!({
                "pid": process.identity.pid,
                "ppid": metadata.ppid,
                "pgid": metadata.pgid.map_or_else(effect_none, |pgid| effect_some(json!(pgid))),
                "status": metadata.status,
                "cpuPercent": process.cpu_percent,
                "rssBytes": process.rss_bytes,
                "elapsed": metadata.elapsed,
                "command": metadata.command,
                "depth": metadata.depth,
                "childPids": metadata.child_pids,
                "processKey": process.process_key,
                "scope": attribution_scope_to_wire(process.scope),
                "kind": attribution_kind_to_wire(process.kind),
                "label": process.label,
                "confidence": attribution_confidence_to_wire(process.confidence),
            }))
        })
        .collect()
}

fn attribution_totals_to_wire(totals: ProcessAttributionTotals) -> Value {
    json!({
        "combined": resource_totals_to_wire(totals.combined),
        "core": resource_totals_to_wire(totals.core),
        "external": resource_totals_to_wire(totals.external),
    })
}

fn resource_totals_to_wire(totals: ProcessResourceTotals) -> Value {
    json!({
        "cpuPercent": totals.cpu_percent,
        "rssBytes": totals.rss_bytes,
        "processCount": totals.process_count,
    })
}

fn attribution_scope_to_wire(scope: AttributionScope) -> &'static str {
    match scope {
        AttributionScope::Core => "core",
        AttributionScope::External => "external",
    }
}

fn attribution_kind_to_wire(kind: AttributionKind) -> &'static str {
    match kind {
        AttributionKind::Server => "server",
        AttributionKind::Ui => "ui",
        AttributionKind::Provider => "provider",
        AttributionKind::Terminal => "terminal",
        AttributionKind::Helper => "helper",
        AttributionKind::Unknown => "unknown",
    }
}

fn attribution_confidence_to_wire(confidence: AttributionConfidence) -> &'static str {
    match confidence {
        AttributionConfidence::Exact => "exact",
        AttributionConfidence::Inherited => "inherited",
        AttributionConfidence::Fallback => "fallback",
    }
}

fn ui_coverage_to_wire(coverage: &UiCoverage) -> Value {
    json!({
        "status": match coverage.status {
            UiCoverageStatus::Available => "available",
            UiCoverageStatus::Partial => "partial",
            UiCoverageStatus::Unavailable => "unavailable",
            UiCoverageStatus::NotApplicable => "notApplicable",
        },
        "message": coverage.message.as_ref().map_or_else(effect_none, |message| {
            effect_some(json!(bound_diagnostic_string(
                message,
                PROCESS_DIAGNOSTIC_MESSAGE_MAX_SCALARS,
            )))
        }),
    })
}

fn split_f64_to_wire(metric: SplitMetric<f64>) -> Value {
    json!({
        "combined": metric.combined,
        "core": metric.core,
        "external": metric.external,
    })
}

fn split_u64_to_wire(metric: SplitMetric<u64>) -> Value {
    json!({
        "combined": metric.combined,
        "core": metric.core,
        "external": metric.external,
    })
}

fn split_usize_to_wire(metric: SplitMetric<usize>) -> Value {
    json!({
        "combined": metric.combined,
        "core": metric.core,
        "external": metric.external,
    })
}

fn bucket_f64_to_wire(metric: BucketMetric<f64>) -> Value {
    json!({
        "average": split_f64_to_wire(metric.average),
        "peak": split_f64_to_wire(metric.peak),
    })
}

fn bucket_u64_to_wire(metric: BucketMetric<u64>) -> Value {
    json!({
        "average": split_u64_to_wire(metric.average),
        "peak": split_u64_to_wire(metric.peak),
    })
}

fn relay_status_to_wire(status: RelayClientStatus) -> Value {
    match status {
        RelayClientStatus::Available {
            executable_path,
            source,
            version,
        } => json!({
            "status": "available", "executablePath": executable_path, "source": source, "version": version,
        }),
        RelayClientStatus::Missing { version } => {
            json!({ "status": "missing", "version": version })
        }
        RelayClientStatus::Unsupported {
            platform,
            arch,
            version,
        } => json!({
            "status": "unsupported", "platform": platform, "arch": arch, "version": version,
        }),
    }
}

fn relay_event_to_wire(event: RelayClientInstallEvent) -> Value {
    match event {
        RelayClientInstallEvent::Progress { stage } => {
            json!({ "type": "progress", "stage": stage })
        }
        RelayClientInstallEvent::Complete { status } => {
            json!({ "type": "complete", "status": relay_status_to_wire(status) })
        }
    }
}

fn terminal_metadata_to_wire(event: TerminalMetadataEvent) -> Value {
    serde_json::to_value(event).expect("terminal metadata event serializes")
}

fn terminal_error(error: TerminalError) -> Value {
    match error {
        TerminalError::Shutdown | TerminalError::PublicationCancelled => json!({
            "_tag": "TerminalSpawnError",
            "reason": "Terminal manager is shut down.",
        }),
        TerminalError::CwdNotFound(cwd) => {
            json!({ "_tag": "TerminalCwdNotFoundError", "cwd": cwd })
        }
        TerminalError::CwdNotDirectory(cwd) => {
            json!({ "_tag": "TerminalCwdNotDirectoryError", "cwd": cwd })
        }
        TerminalError::NotFound {
            thread_id,
            terminal_id,
        } => json!({
            "_tag": "TerminalSessionLookupError",
            "threadId": thread_id,
            "terminalId": terminal_id,
        }),
        TerminalError::NotRunning {
            thread_id,
            terminal_id,
        } => json!({
            "_tag": "TerminalNotRunningError",
            "threadId": thread_id,
            "terminalId": terminal_id,
        }),
        TerminalError::Spawn { .. } => json!({
            "_tag": "TerminalSpawnError",
            "reason": "Terminal process could not be started.",
        }),
        TerminalError::Io(message) => json!({
            "_tag": "TerminalCwdStatError",
            "cwd": "",
            "cause": message,
        }),
        TerminalError::Input(error) => json!({
            "_tag": "TerminalInputError",
            "code": error.code,
            "message": error.message,
        }),
        TerminalError::Close => json!({
            "_tag": "TerminalCloseError",
            "reason": "Terminal processes did not exit before cleanup timed out.",
        }),
        TerminalError::WorktreeRemovalInProgress { .. } => json!({
            "_tag": "TerminalSpawnError",
            "reason": "The worktree is being removed.",
        }),
    }
}

fn invalid_request(message: &str) -> Value {
    json!({ "_tag": "RpcRequestInvalid", "message": message })
}

fn effect_none() -> Value {
    json!({ "_tag": "None" })
}
fn effect_some(value: Value) -> Value {
    json!({ "_tag": "Some", "value": value })
}

fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| value.unix_timestamp().to_string())
}

fn format_epoch_ms(value: i128) -> String {
    let nanos = value.saturating_mul(1_000_000);
    i64::try_from(nanos)
        .ok()
        .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos)).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::{future::Future, pin::Pin, process::Stdio, time::Duration};

    use futures_util::FutureExt as _;

    use crate::{
        ServerConfig,
        diagnostics::{
            AttributedProcess, AttributedProcessSnapshot, AttributionConfidence, AttributionKind,
            AttributionScope, BucketMetric, NativeResourceSampler, NotApplicableUiProcessObserver,
            ProcessAttributionRegistry, ProcessAttributionTotals, ProcessIdentity,
            ProcessResourceBucket, ProcessResourceSummary, ProcessResourceTotals, ProcessRow,
            ResourceSampler, SamplingError, SplitMetric, UiCoverage, UiCoverageStatus,
        },
        production::control::NativeServerControl,
        terminal::{
            PortablePtyBackend, TerminalEvent, TerminalLaunchCommand, TerminalManagerOptions,
        },
    };

    use super::*;

    #[tokio::test]
    async fn embedded_host_cleanup_preserves_unmanaged_host_descendants() {
        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("C:\\Windows\\System32\\ping.exe");
            command.args(["-n", "30", "127.0.0.1"]);
            command
        } else {
            let mut command = tokio::process::Command::new("sleep");
            command.arg("30");
            command
        };
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let mut sentinel = command.spawn().expect("sentinel process starts");

        let cleanup = cleanup_server_descendants(
            ProcessTreeCleanup::EmbeddedHost,
            &NativeProcessSampler::default(),
        )
        .await
        .expect("embedded cleanup policy is evaluated");
        let survived = sentinel
            .try_wait()
            .expect("sentinel status is readable")
            .is_none();
        if survived {
            let _ = sentinel.start_kill();
        }
        let _ = sentinel.wait().await;

        assert_eq!(cleanup, None);
        assert!(
            survived,
            "embedded cleanup must preserve host-owned children"
        );
    }

    fn terminal_start_payload(command: Value) -> TerminalStartPayload {
        decode_payload(&json!({
            "threadId": "thread-validation",
            "terminalId": "term-validation",
            "cwd": "/tmp",
            "cols": 120,
            "rows": 30,
            "command": command,
        }))
        .expect("terminal start payload decodes")
    }

    fn terminal_attach_payload(command: Value) -> TerminalAttachPayload {
        decode_payload(&json!({
            "threadId": "thread-validation",
            "terminalId": "term-validation",
            "command": command,
        }))
        .expect("terminal attach payload decodes")
    }

    async fn real_terminal_services(root: &std::path::Path) -> ServerTerminalServices {
        let attribution = ProcessAttributionRegistry::new();
        let terminal = TerminalManager::with_process_attribution(
            Arc::new(PortablePtyBackend),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::from_millis(20),
                ..TerminalManagerOptions::default()
            },
            attribution.clone(),
        );
        let sampler = Arc::new(NativeProcessSampler::default());
        let resource_sampler = Arc::new(NativeResourceSampler::new(
            sampler.clone(),
            attribution,
            Arc::new(NotApplicableUiProcessObserver),
        ));
        let monitor = Arc::new(DiagnosticsMonitor::new(
            resource_sampler.clone(),
            Duration::from_secs(60),
        ));
        let usage = ProviderUsageService::new(Vec::new(), Arc::new(OffsetDateTime::now_utc));
        let relay = RelayClientService::new(
            || async {
                RelayClientStatus::Missing {
                    version: "1.0.0".to_owned(),
                }
            },
            |_report| async {
                Ok(RelayClientStatus::Missing {
                    version: "1.0.0".to_owned(),
                })
            },
        );
        let config = ServerConfig::new(root);
        crate::test_support::hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({}),
        );
        let control = Arc::new(NativeServerControl::new(config, json!({"policy":"test"})).await);
        ServerTerminalServices::new(
            terminal,
            sampler,
            resource_sampler,
            monitor,
            usage,
            relay,
            control,
        )
    }

    fn write_probe_command(value: &str) -> String {
        if cfg!(windows) {
            format!("echo.\recho {value}\r")
        } else {
            format!("printf '\\n%s\\n' '{value}'\n")
        }
    }

    fn strip_terminal_control_sequences(input: &str) -> String {
        #[derive(Clone, Copy)]
        enum State {
            Text,
            Escape,
            Csi,
            Osc,
            OscEscape,
        }

        let mut state = State::Text;
        let mut stripped = String::with_capacity(input.len());
        for character in input.chars() {
            state = match state {
                State::Text if character == '\u{1b}' => State::Escape,
                State::Text => {
                    stripped.push(character);
                    State::Text
                }
                State::Escape if character == '[' => State::Csi,
                State::Escape if character == ']' => State::Osc,
                State::Escape => State::Text,
                State::Csi if ('@'..='~').contains(&character) => State::Text,
                State::Csi => State::Csi,
                State::Osc if character == '\u{7}' => State::Text,
                State::Osc if character == '\u{1b}' => State::OscEscape,
                State::Osc => State::Osc,
                State::OscEscape if character == '\\' => State::Text,
                State::OscEscape if character == '\u{1b}' => State::OscEscape,
                State::OscEscape => State::Osc,
            };
        }
        stripped
    }

    fn probe_output_contains_exact_line(output: &str, value: &str) -> bool {
        strip_terminal_control_sequences(output)
            .lines()
            .any(|line| line.trim() == value)
    }

    #[test]
    fn terminal_probe_ignores_windows_vt_sequences_without_accepting_the_echoed_command() {
        let observed = concat!(
            "\u{1b}[?25lecho still-alive-b\n",
            "\u{1b}[?25lstill-alive-b\u{1b}[8;1H \u{1b}[?25h",
        );

        assert!(probe_output_contains_exact_line(observed, "still-alive-b"));
        assert!(!probe_output_contains_exact_line(
            "\u{1b}[?25lecho still-alive-b\u{1b}[?25h",
            "still-alive-b",
        ));
    }

    fn real_terminal_input(
        thread_id: &str,
        terminal_id: &str,
        cwd: &std::path::Path,
    ) -> TerminalOpenInput {
        let mut input = TerminalOpenInput::new(thread_id, terminal_id, cwd.to_path_buf(), 80, 24);
        if cfg!(windows) {
            input.command = Some(TerminalLaunchCommand {
                executable: "cmd.exe".to_owned(),
                args: vec![
                    "/d".to_owned(),
                    "/q".to_owned(),
                    "/k".to_owned(),
                    "prompt $H".to_owned(),
                ],
                label: None,
                activity: None,
                env: None,
            });
        } else {
            input.command = Some(TerminalLaunchCommand {
                executable: "/bin/sh".to_owned(),
                args: Vec::new(),
                label: None,
                activity: None,
                env: None,
            });
        }
        input
    }

    async fn wait_for_probe(
        events: &mut crate::terminal::TerminalEventReceiver,
        thread_id: &str,
        terminal_id: &str,
        value: &str,
    ) -> Result<(), String> {
        let mut output = String::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match events.recv().await {
                    Ok(TerminalEvent::Output {
                        thread_id: event_thread,
                        terminal_id: event_terminal,
                        data,
                        ..
                    }) if event_thread == thread_id && event_terminal == terminal_id => {
                        output.push_str(&data.replace('\r', ""));
                        if probe_output_contains_exact_line(&output, value) {
                            return Ok(());
                        }
                        if output.len() > 16 * 1024 {
                            return Err(format!(
                                "terminal {terminal_id} exceeded its bounded probe output"
                            ));
                        }
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err(format!(
                            "terminal {terminal_id} event stream closed before probe"
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            format!(
                "timed out waiting for terminal {terminal_id} probe; observed output: {output:?}"
            )
        })?
    }

    fn exact_process_exited(identity: ProcessIdentity) -> Result<(), String> {
        if matches!(
            NativeProcessSampler::process_identity(identity.pid),
            Ok(current) if current == identity
        ) {
            return Err(format!(
                "runtime-owned process {} remained alive after shutdown",
                identity.pid
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn discovered_local_servers_stream_rescans_and_ends_on_cancellation() {
        async fn next_snapshot(stream: &mut JsonStream) -> Value {
            let mut values = tokio::time::timeout(Duration::from_secs(5), stream.recv())
                .await
                .expect("discovery snapshot timeout")
                .expect("discovery stream open")
                .expect("discovery snapshot succeeds");
            assert_eq!(values.len(), 1);
            values.remove(0)
        }

        let root = tempfile::tempdir().expect("services root");
        let services = real_terminal_services(root.path()).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local listener");
        let port = listener.local_addr().expect("listener address").port();
        let cancellation = CancellationToken::new();
        let mut discovery =
            discovered_local_servers_stream(services.terminal.clone(), cancellation.clone());

        let discovered = next_snapshot(&mut discovery).await;
        assert!(discovered["scannedAt"].is_string());
        let server = discovered["servers"]
            .as_array()
            .expect("servers array")
            .iter()
            .find(|server| server["port"] == port)
            .expect("bound listener is discovered")
            .clone();
        assert_eq!(server["host"], "127.0.0.1");
        assert_eq!(server["url"], format!("http://127.0.0.1:{port}/"));
        assert_eq!(server["terminal"], Value::Null);

        drop(listener);
        let rescanned = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = next_snapshot(&mut discovery).await;
                if snapshot["servers"]
                    .as_array()
                    .is_some_and(|servers| servers.iter().all(|server| server["port"] != port))
                {
                    break snapshot;
                }
            }
        })
        .await
        .expect("periodic discovery removes closed listener");
        assert!(rescanned["scannedAt"].is_string());

        cancellation.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), discovery.recv())
                .await
                .expect("discovery cancellation timeout")
                .is_none()
        );
        services.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutting_down_one_runtime_reaps_only_its_real_terminal_child() {
        let root_a = tempfile::tempdir().expect("runtime A root");
        let root_b = tempfile::tempdir().expect("runtime B root");
        let (services_a, services_b) = tokio::join!(
            real_terminal_services(root_a.path()),
            real_terminal_services(root_b.path())
        );
        let mut events_b = services_b.terminal.subscribe_events();
        let outcome = AssertUnwindSafe(async {
            let (opened_a, opened_b) = tokio::join!(
                services_a.terminal.open(real_terminal_input(
                    "thread-a",
                    "terminal-a",
                    root_a.path()
                )),
                services_b.terminal.open(real_terminal_input(
                    "thread-b",
                    "terminal-b",
                    root_b.path()
                ))
            );
            let pid_a = opened_a
                .map_err(|error| error.to_string())?
                .pid
                .ok_or("A pid")?;
            let pid_b = opened_b
                .map_err(|error| error.to_string())?
                .pid
                .ok_or("B pid")?;
            let identity_a = NativeProcessSampler::process_identity(pid_a)
                .map_err(|error| format!("A identity: {error}"))?;
            let identity_b = NativeProcessSampler::process_identity(pid_b)
                .map_err(|error| format!("B identity: {error}"))?;
            services_a.shutdown().await;
            exact_process_exited(identity_a)?;
            let current_b = NativeProcessSampler::process_identity(pid_b)
                .map_err(|error| format!("runtime B remains alive: {error}"))?;
            if current_b != identity_b {
                return Err("runtime B identity changed during runtime A shutdown".to_owned());
            }
            services_b
                .terminal
                .write(
                    "thread-b",
                    "terminal-b",
                    &write_probe_command("still-alive-b"),
                )
                .await
                .map_err(|error| error.to_string())?;
            wait_for_probe(&mut events_b, "thread-b", "terminal-b", "still-alive-b").await?;
            Ok::<_, String>((identity_a, identity_b))
        })
        .catch_unwind()
        .await;

        tokio::join!(services_a.shutdown(), services_b.shutdown());
        match outcome {
            Ok(Ok((identity_a, identity_b))) => {
                exact_process_exited(identity_a).expect("runtime A exact cleanup");
                exact_process_exited(identity_b).expect("runtime B exact cleanup");
            }
            Ok(Err(error)) => panic!("two-runtime terminal assertion failed: {error}"),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn assert_invalid_terminal_launch_command(command: Value) {
        for dimensions_required in [false, true] {
            let error = terminal_start_payload(command.clone())
                .into_open(dimensions_required)
                .expect_err("start and restart must reject invalid commands");
            assert_eq!(error["_tag"], "RpcRequestInvalid");
        }

        let error = terminal_attach_payload(command)
            .into_attach(None)
            .expect_err("attach must reject invalid commands");
        assert_eq!(error["_tag"], "RpcRequestInvalid");
    }

    #[test]
    fn terminal_spawn_errors_are_explicit_bounded_and_redacted() {
        let error = terminal_error(TerminalError::Spawn {
            attempted: vec!["/secret/provider".to_owned()],
            message: "token=secret".to_owned(),
        });

        assert_eq!(
            error,
            json!({
                "_tag": "TerminalSpawnError",
                "reason": "Terminal process could not be started.",
            })
        );
        let encoded = error.to_string();
        assert!(!encoded.contains("/secret/provider"));
        assert!(!encoded.contains("token=secret"));
        assert!(error["reason"].as_str().unwrap().encode_utf16().count() <= 512);
    }

    #[test]
    fn terminal_close_errors_are_explicit() {
        assert_eq!(
            terminal_error(TerminalError::Close),
            json!({
                "_tag": "TerminalCloseError",
                "reason": "Terminal processes did not exit before cleanup timed out.",
            })
        );
    }

    #[test]
    fn terminal_payload_conversions_reject_commands_outside_contract_bounds() {
        for command in [
            json!({ "executable": " \t ", "args": [] }),
            json!({ "executable": "x".repeat(4_097), "args": [] }),
            json!({ "executable": "codex", "args": vec!["x"; 65] }),
            json!({ "executable": "codex", "args": ["x".repeat(8_193)] }),
            json!({ "executable": "codex", "args": [], "label": " \n " }),
            json!({
                "executable": "codex",
                "args": [],
                "label": "x".repeat(129),
            }),
        ] {
            assert_invalid_terminal_launch_command(command);
        }
    }

    #[test]
    fn terminal_payload_conversions_trim_command_names_without_trimming_arguments() {
        let command = json!({
            "executable": "  /opt/codex  ",
            "args": ["  --dangerously-bypass-approvals-and-sandbox  "],
            "label": "  Codex Terminal  ",
        });

        for dimensions_required in [false, true] {
            let open = terminal_start_payload(command.clone())
                .into_open(dimensions_required)
                .expect("valid start and restart commands");
            assert_eq!(
                open.command,
                Some(TerminalLaunchCommand {
                    executable: "/opt/codex".to_owned(),
                    args: vec!["  --dangerously-bypass-approvals-and-sandbox  ".to_owned()],
                    label: Some("Codex Terminal".to_owned()),
                    activity: None,
                    env: None,
                })
            );
        }

        let attach = terminal_attach_payload(command)
            .into_attach(None)
            .expect("valid attach command");
        assert_eq!(
            attach.command,
            Some(TerminalLaunchCommand {
                executable: "/opt/codex".to_owned(),
                args: vec!["  --dangerously-bypass-approvals-and-sandbox  ".to_owned()],
                label: Some("Codex Terminal".to_owned()),
                activity: None,
                env: None,
            })
        );
    }

    #[test]
    fn terminal_payload_conversions_measure_command_bounds_in_utf16_code_units() {
        let astral = "😀";
        let at_boundary = json!({
            "executable": astral.repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH / 2),
            "args": [astral.repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH / 2)],
            "label": astral.repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH / 2),
        });
        assert!(
            terminal_start_payload(at_boundary).into_open(false).is_ok(),
            "astral strings at the UTF-16 contract limits must be accepted"
        );

        for command in [
            json!({
                "executable": astral.repeat(TERMINAL_LAUNCH_EXECUTABLE_MAX_LENGTH / 2 + 1),
                "args": [],
            }),
            json!({
                "executable": "codex",
                "args": [astral.repeat(TERMINAL_LAUNCH_ARGUMENT_MAX_LENGTH / 2 + 1)],
            }),
            json!({
                "executable": "codex",
                "args": [],
                "label": astral.repeat(TERMINAL_LAUNCH_LABEL_MAX_LENGTH / 2 + 1),
            }),
        ] {
            assert_invalid_terminal_launch_command(command);
        }
    }

    #[test]
    fn terminal_payload_conversions_use_ecmascript_trim_semantics() {
        let command = json!({
            "executable": "\u{feff}codex\u{feff}",
            "args": ["\u{feff}--model\u{feff}"],
            "label": "\u{feff}Codex Terminal\u{feff}",
        });
        let open = terminal_start_payload(command)
            .into_open(false)
            .expect("ECMAScript whitespace is trimmed from command names");
        assert_eq!(
            open.command,
            Some(TerminalLaunchCommand {
                executable: "codex".to_owned(),
                args: vec!["\u{feff}--model\u{feff}".to_owned()],
                label: Some("Codex Terminal".to_owned()),
                activity: None,
                env: None,
            })
        );

        let non_ecmascript_whitespace = json!({
            "executable": "\u{85}codex\u{85}",
            "args": [],
            "label": "\u{85}Codex Terminal\u{85}",
        });
        let open = terminal_start_payload(non_ecmascript_whitespace)
            .into_open(false)
            .expect("non-ECMAScript whitespace is retained");
        assert_eq!(
            open.command,
            Some(TerminalLaunchCommand {
                executable: "\u{85}codex\u{85}".to_owned(),
                args: Vec::new(),
                label: Some("\u{85}Codex Terminal\u{85}".to_owned()),
                activity: None,
                env: None,
            })
        );

        assert_invalid_terminal_launch_command(json!({
            "executable": "\u{feff}",
            "args": [],
        }));
    }

    #[derive(Debug)]
    struct StaticResourceSampler {
        snapshot: AttributedProcessSnapshot,
    }

    impl ResourceSampler for StaticResourceSampler {
        fn sample(
            &self,
        ) -> Pin<
            Box<dyn Future<Output = Result<AttributedProcessSnapshot, SamplingError>> + Send + '_>,
        > {
            Box::pin(async { Ok(self.snapshot.clone()) })
        }
    }

    #[test]
    fn attributed_current_wire_maps_every_variant_and_bounds_failures() {
        let kinds = [
            AttributionKind::Server,
            AttributionKind::Ui,
            AttributionKind::Provider,
            AttributionKind::Terminal,
            AttributionKind::Helper,
            AttributionKind::Unknown,
        ];
        let scopes = [
            AttributionScope::Core,
            AttributionScope::External,
            AttributionScope::External,
            AttributionScope::External,
            AttributionScope::External,
            AttributionScope::External,
        ];
        let confidences = [
            AttributionConfidence::Exact,
            AttributionConfidence::Inherited,
            AttributionConfidence::Fallback,
            AttributionConfidence::Exact,
            AttributionConfidence::Inherited,
            AttributionConfidence::Fallback,
        ];
        let processes = kinds
            .into_iter()
            .zip(scopes)
            .zip(confidences)
            .enumerate()
            .map(|(index, ((kind, scope), confidence))| {
                let pid = u32::try_from(index).expect("fixture index") + 10;
                let identity = ProcessIdentity {
                    pid,
                    started_at: u64::from(pid) * 10,
                };
                AttributedProcess {
                    identity,
                    process_key: identity.key(),
                    scope,
                    kind,
                    label: format!("process-{pid}"),
                    confidence,
                    cpu_percent: f64::from(pid),
                    rss_bytes: u64::from(pid) * 100,
                }
            })
            .collect::<Vec<_>>();
        let rows = processes
            .iter()
            .map(|process| {
                let mut row = ProcessRow::fixture(
                    process.identity.pid,
                    if process.identity.pid == 10 { 1 } else { 10 },
                    format!("command-{}", process.identity.pid),
                );
                row.started_at = process.identity.started_at;
                row
            })
            .collect::<Vec<_>>();
        let totals = ProcessAttributionTotals {
            combined: ProcessResourceTotals {
                cpu_percent: 75.0,
                rss_bytes: 7_500,
                process_count: 6,
            },
            core: ProcessResourceTotals {
                cpu_percent: 10.0,
                rss_bytes: 1_000,
                process_count: 1,
            },
            external: ProcessResourceTotals {
                cpu_percent: 65.0,
                rss_bytes: 6_500,
                process_count: 5,
            },
        };

        for (status, expected_status, coverage_message) in [
            (UiCoverageStatus::Available, "available", None),
            (UiCoverageStatus::Partial, "partial", Some("界".repeat(500))),
            (UiCoverageStatus::Unavailable, "unavailable", None),
            (UiCoverageStatus::NotApplicable, "notApplicable", None),
        ] {
            let wire = process_diagnostics_to_wire(CurrentProcessDiagnostics {
                snapshot: Some(Arc::new(AttributedProcessSnapshot {
                    sampled_at_ms: 1_000,
                    server_identity: ProcessIdentity {
                        pid: 10,
                        started_at: 100,
                    },
                    native_rows: rows.clone().into(),
                    processes: processes.clone(),
                    totals,
                    ui_coverage: UiCoverage {
                        status,
                        message: coverage_message,
                    },
                })),
                error: Some("é".repeat(500)),
            });

            assert_eq!(wire["totals"]["combined"]["cpuPercent"], 75.0);
            assert_eq!(
                wire["totals"]["combined"]["cpuPercent"]
                    .as_f64()
                    .expect("combined CPU percent"),
                wire["totals"]["core"]["cpuPercent"]
                    .as_f64()
                    .expect("core CPU percent")
                    + wire["totals"]["external"]["cpuPercent"]
                        .as_f64()
                        .expect("external CPU percent")
            );
            assert_eq!(
                wire["totals"]["combined"]["rssBytes"]
                    .as_u64()
                    .expect("combined RSS"),
                wire["totals"]["core"]["rssBytes"]
                    .as_u64()
                    .expect("core RSS")
                    + wire["totals"]["external"]["rssBytes"]
                        .as_u64()
                        .expect("external RSS")
            );
            assert_eq!(
                wire["totals"]["combined"]["processCount"]
                    .as_u64()
                    .expect("combined process count"),
                wire["totals"]["core"]["processCount"]
                    .as_u64()
                    .expect("core process count")
                    + wire["totals"]["external"]["processCount"]
                        .as_u64()
                        .expect("external process count")
            );
            assert_eq!(wire["uiCoverage"]["status"], expected_status);
            if status == UiCoverageStatus::Partial {
                assert_eq!(wire["uiCoverage"]["message"]["_tag"], "Some");
                assert_eq!(
                    wire["uiCoverage"]["message"]["value"]
                        .as_str()
                        .expect("bounded Unicode coverage message")
                        .chars()
                        .count(),
                    160
                );
            } else {
                assert_eq!(wire["uiCoverage"]["message"]["_tag"], "None");
            }
            assert!(
                wire["error"]["value"]["message"]
                    .as_str()
                    .expect("bounded failure message")
                    .chars()
                    .count()
                    <= 160
            );
            assert_eq!(
                wire["processes"]
                    .as_array()
                    .expect("attributed processes")
                    .iter()
                    .map(|process| process["kind"].as_str().expect("kind"))
                    .collect::<Vec<_>>(),
                ["server", "ui", "provider", "terminal", "helper", "unknown"]
            );
            assert_eq!(
                wire["processes"][0]["scope"].as_str().expect("scope"),
                "core"
            );
            assert_eq!(
                wire["processes"][1]["scope"].as_str().expect("scope"),
                "external"
            );
            assert_eq!(
                wire["processes"]
                    .as_array()
                    .expect("attributed processes")
                    .iter()
                    .map(|process| process["confidence"].as_str().expect("confidence"))
                    .collect::<Vec<_>>(),
                [
                    "exact",
                    "inherited",
                    "fallback",
                    "exact",
                    "inherited",
                    "fallback"
                ]
            );
        }
    }

    #[test]
    fn typed_payload_decoders_reject_non_object_wire_values() {
        let invalid = json!("not-an-object");
        assert!(decode_payload::<RefreshProviderUsageInput>(&invalid).is_err());
        assert!(decode_payload::<ResourceHistoryInput>(&invalid).is_err());
        assert!(decode_payload::<SignalProcessInput>(&invalid).is_err());
        assert!(
            decode_payload::<SignalProcessInput>(&json!({
                "pid": 42,
                "signal": "SIGINT",
            }))
            .is_err()
        );
        let signal_input: SignalProcessInput = decode_payload(&json!({
            "pid": 42,
            "processKey": "42:100",
            "signal": "SIGINT",
        }))
        .expect("identity-bound signal input");
        assert_eq!(
            decode_process_identity(signal_input.pid, &signal_input.process_key)
                .expect("process identity"),
            ProcessIdentity {
                pid: 42,
                started_at: 100,
            }
        );
        assert!(decode_process_identity(42, "43:100").is_err());
        assert!(decode_process_identity(42, "invalid").is_err());
        assert!(matches!(
            decode_process_signal(&signal_input.signal),
            Ok(ProcessSignal::Interrupt)
        ));
        assert!(decode_process_signal("SIGHUP").is_err());
        assert!(decode_payload::<TerminalAttachPayload>(&invalid).is_err());
        assert!(decode_payload::<TerminalClosePayload>(&invalid).is_err());
        assert!(decode_payload::<TerminalResizePayload>(&invalid).is_err());
        assert!(decode_payload::<TerminalSessionPayload>(&invalid).is_err());
        assert!(decode_payload::<TerminalWritePayload>(&invalid).is_err());
        assert_eq!(format_epoch_ms(i128::MAX), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn terminal_resize_claim_decoding_accepts_legacy_and_bounds_new_claims() {
        assert!(validate_terminal_size_claim(None).unwrap().is_none());
        assert_eq!(
            validate_terminal_size_claim(Some(&json!(" \u{feff}renderer\u{feff} ")))
                .unwrap()
                .as_deref(),
            Some("renderer")
        );
        for invalid in [
            Value::Null,
            json!(""),
            json!(" "),
            json!("x".repeat(TERMINAL_SIZE_CLAIM_MAX_LENGTH + 1)),
            json!(false),
        ] {
            assert_eq!(
                validate_terminal_size_claim(Some(&invalid)).unwrap_err()["_tag"],
                "RpcRequestInvalid"
            );
        }
    }

    #[test]
    fn attributed_history_wire_preserves_the_complete_process_order() {
        fn summary(
            process_key: &str,
            last_seen_at_ms: i128,
            cpu_seconds_approx: f64,
        ) -> ProcessResourceSummary {
            ProcessResourceSummary {
                process_key: process_key.to_owned(),
                pid: 1,
                ppid: 0,
                command: process_key.to_owned(),
                depth: 0,
                scope: AttributionScope::Core,
                kind: AttributionKind::Provider,
                label: process_key.to_owned(),
                confidence: AttributionConfidence::Exact,
                first_seen_at_ms: 0,
                last_seen_at_ms,
                current_cpu_percent: 1.0,
                avg_cpu_percent: 1.0,
                max_cpu_percent: 1.0,
                cpu_seconds_approx,
                current_rss_bytes: 1,
                max_rss_bytes: 1,
                sample_count: 1,
            }
        }
        let wire = resource_history_to_wire(ProcessResourceHistory {
            read_at_ms: 2_000,
            window_ms: 60_000,
            bucket_ms: 1_000,
            sample_interval_ms: 500,
            retained_sample_count: 2,
            total_cpu_seconds_approx: SplitMetric::default(),
            ui_coverage: UiCoverage::default(),
            buckets: Vec::new(),
            processes: vec![
                summary("exited:high-cpu", 1_000, 100.0),
                summary("active:z", 2_000, 2.0),
                summary("active:b", 2_000, 4.0),
                summary("active:a", 2_000, 4.0),
            ],
            error: None,
        });

        assert_eq!(
            wire["processes"]
                .as_array()
                .expect("processes")
                .iter()
                .map(|process| process["processKey"].as_str().expect("process key"))
                .collect::<Vec<_>>(),
            ["exited:high-cpu", "active:z", "active:b", "active:a"]
        );
        assert_eq!(wire["retainedSampleCount"], 2);
    }

    #[tokio::test]
    async fn attributed_history_wire_includes_independent_claimed_roots() {
        let sampled_at_ms = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
        let mut server = ProcessRow::fixture(10, 1, "/native/bibcode --serve");
        server.started_at = 100;
        server.cpu_percent = 10.0;
        server.cpu_core_percent = Some(5.0);
        server.rss_bytes = 100;
        let mut child = ProcessRow::fixture(11, 10, "/native/codex --model gpt");
        child.started_at = 110;
        child.cpu_percent = 20.0;
        child.cpu_core_percent = Some(10.0);
        child.rss_bytes = 200;
        let mut claimed_root = ProcessRow::fixture(20, 1, "/native/claimed-provider");
        claimed_root.started_at = 200;
        claimed_root.cpu_percent = 100.0;
        claimed_root.cpu_core_percent = Some(50.0);
        claimed_root.rss_bytes = 10_000;
        let server_identity = ProcessIdentity {
            pid: 10,
            started_at: 100,
        };
        let child_identity = ProcessIdentity {
            pid: 11,
            started_at: 110,
        };
        let claimed_identity = ProcessIdentity {
            pid: 20,
            started_at: 200,
        };
        let attributed = |identity: ProcessIdentity,
                          scope: AttributionScope,
                          kind: AttributionKind,
                          label: &str,
                          cpu_percent: f64,
                          rss_bytes: u64| AttributedProcess {
            identity,
            process_key: identity.key(),
            scope,
            kind,
            label: label.to_owned(),
            confidence: AttributionConfidence::Exact,
            cpu_percent,
            rss_bytes,
        };
        let monitor = DiagnosticsMonitor::new(
            Arc::new(StaticResourceSampler {
                snapshot: AttributedProcessSnapshot {
                    sampled_at_ms,
                    server_identity,
                    native_rows: Arc::from([server, child, claimed_root]),
                    processes: vec![
                        attributed(
                            server_identity,
                            AttributionScope::Core,
                            AttributionKind::Server,
                            "attributed/server-label",
                            10.0,
                            100,
                        ),
                        attributed(
                            child_identity,
                            AttributionScope::Core,
                            AttributionKind::Provider,
                            "attributed/child-label",
                            20.0,
                            200,
                        ),
                        attributed(
                            claimed_identity,
                            AttributionScope::External,
                            AttributionKind::Provider,
                            "attributed/claimed-label",
                            100.0,
                            10_000,
                        ),
                    ],
                    totals: ProcessAttributionTotals {
                        combined: ProcessResourceTotals {
                            cpu_percent: 130.0,
                            rss_bytes: 10_300,
                            process_count: 3,
                        },
                        core: ProcessResourceTotals {
                            cpu_percent: 30.0,
                            rss_bytes: 300,
                            process_count: 2,
                        },
                        external: ProcessResourceTotals {
                            cpu_percent: 100.0,
                            rss_bytes: 10_000,
                            process_count: 1,
                        },
                    },
                    ui_coverage: UiCoverage::default(),
                },
            }),
            Duration::from_secs(2),
        );

        let history = monitor.read_history(1_000, 1_000).await;
        assert_eq!(
            history
                .processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>(),
            [10, 11, 20]
        );
        let wire = resource_history_to_wire(history);
        let bucket = &wire["buckets"][0];
        let processes = wire["processes"].as_array().expect("processes");

        assert!(
            (wire["cpuSecondsApprox"]["combined"]
                .as_f64()
                .expect("total CPU seconds")
                - 2.6)
                .abs()
                < 1e-9
        );
        assert_eq!(wire["retainedSampleCount"], 1);
        assert_eq!(bucket["cpuPercent"]["average"]["combined"], 130.0);
        assert_eq!(bucket["cpuPercent"]["peak"]["combined"], 130.0);
        assert_eq!(bucket["rssBytes"]["peak"]["combined"], 10_300);
        assert_eq!(bucket["maxProcessCount"]["combined"], 3);
        assert_eq!(
            processes
                .iter()
                .map(|process| process["pid"].as_u64().expect("pid"))
                .collect::<Vec<_>>(),
            [10, 11, 20]
        );
        assert_eq!(processes[0]["scope"], "core");
        assert_eq!(processes[1]["label"], "attributed/child-label");
        assert_eq!(processes[2]["scope"], "external");
        assert_eq!(processes[2]["command"], "/native/claimed-provider");
    }

    #[test]
    fn attributed_current_wire_reproduces_native_metadata() {
        let mut server = ProcessRow::fixture(10, 1, "/opt/bibcode server");
        server.started_at = 100;
        server.pgid = Some(10);
        server.status = "Sleep".to_owned();
        server.cpu_percent = 1.5;
        server.rss_bytes = 100;
        server.elapsed = "01:02:03".to_owned();
        let mut child = ProcessRow::fixture(11, 10, "codex --model gpt");
        child.started_at = 110;
        child.pgid = Some(10);
        child.status = "Run".to_owned();
        child.cpu_percent = 2.5;
        child.rss_bytes = 200;
        child.elapsed = "00:02:00".to_owned();
        let mut grandchild = ProcessRow::fixture(12, 11, "git status");
        grandchild.started_at = 120;
        grandchild.pgid = Some(12);
        grandchild.status = "Stop".to_owned();
        grandchild.cpu_percent = 3.5;
        grandchild.rss_bytes = 300;
        grandchild.elapsed = "00:00:05".to_owned();
        let server_identity = ProcessIdentity {
            pid: 10,
            started_at: 100,
        };
        let child_identity = ProcessIdentity {
            pid: 11,
            started_at: 110,
        };
        let grandchild_identity = ProcessIdentity {
            pid: 12,
            started_at: 120,
        };

        let wire = process_diagnostics_to_wire(CurrentProcessDiagnostics {
            snapshot: Some(Arc::new(AttributedProcessSnapshot {
                sampled_at_ms: 1_000,
                server_identity,
                native_rows: Arc::from([server, child, grandchild]),
                processes: [
                    (
                        &server_identity,
                        AttributionKind::Server,
                        AttributionConfidence::Exact,
                    ),
                    (
                        &child_identity,
                        AttributionKind::Unknown,
                        AttributionConfidence::Fallback,
                    ),
                    (
                        &grandchild_identity,
                        AttributionKind::Unknown,
                        AttributionConfidence::Fallback,
                    ),
                ]
                .into_iter()
                .map(|(identity, kind, confidence)| AttributedProcess {
                    identity: *identity,
                    process_key: identity.key(),
                    scope: if kind == AttributionKind::Server {
                        AttributionScope::Core
                    } else {
                        AttributionScope::External
                    },
                    kind,
                    label: format!("process-{}", identity.pid),
                    confidence,
                    cpu_percent: match identity.pid {
                        10 => 1.5,
                        11 => 2.5,
                        _ => 3.5,
                    },
                    rss_bytes: match identity.pid {
                        10 => 100,
                        11 => 200,
                        _ => 300,
                    },
                })
                .collect(),
                totals: ProcessAttributionTotals {
                    combined: ProcessResourceTotals {
                        cpu_percent: 7.5,
                        rss_bytes: 600,
                        process_count: 3,
                    },
                    core: ProcessResourceTotals {
                        cpu_percent: 1.5,
                        rss_bytes: 100,
                        process_count: 1,
                    },
                    external: ProcessResourceTotals {
                        cpu_percent: 6.0,
                        rss_bytes: 500,
                        process_count: 2,
                    },
                },
                ui_coverage: UiCoverage::default(),
            })),
            error: None,
        });
        let processes = wire["processes"].as_array().expect("processes");

        assert_eq!(wire["totals"]["combined"]["processCount"], 3);
        assert_eq!(wire["totals"]["combined"]["rssBytes"], 600);
        assert_eq!(wire["totals"]["combined"]["cpuPercent"], 7.5);
        assert_eq!(
            processes
                .iter()
                .map(|process| process["pid"].as_u64().expect("pid"))
                .collect::<Vec<_>>(),
            [10, 11, 12]
        );
        assert_eq!(processes[0]["ppid"], 1);
        assert_eq!(processes[0]["pgid"]["value"], 10);
        assert_eq!(processes[0]["status"], "Sleep");
        assert_eq!(processes[0]["elapsed"], "01:02:03");
        assert_eq!(processes[0]["command"], "/opt/bibcode server");
        assert_eq!(processes[0]["depth"], 0);
        assert_eq!(processes[0]["childPids"], json!([11]));
        assert_eq!(processes[1]["depth"], 1);
        assert_eq!(processes[1]["childPids"], json!([12]));
        assert_eq!(processes[2]["depth"], 2);
    }

    #[test]
    fn attributed_current_wire_includes_independently_claimed_roots() {
        let server = ProcessRow::fixture(10, 1, "bibcode");
        let server_child = ProcessRow::fixture(11, 10, "server child");
        let claimed_root = ProcessRow::fixture(20, 1, "claimed provider");
        let claimed_child = ProcessRow::fixture(21, 20, "claimed provider child");
        let claimed_identity = ProcessIdentity {
            pid: 20,
            started_at: 0,
        };

        let wire = process_diagnostics_to_wire(CurrentProcessDiagnostics {
            snapshot: Some(Arc::new(AttributedProcessSnapshot {
                sampled_at_ms: 1_000,
                server_identity: ProcessIdentity {
                    pid: 10,
                    started_at: 0,
                },
                native_rows: Arc::from([server, server_child, claimed_root, claimed_child]),
                processes: vec![AttributedProcess {
                    identity: claimed_identity,
                    process_key: claimed_identity.key(),
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "claimed/provider".to_owned(),
                    confidence: AttributionConfidence::Exact,
                    cpu_percent: 99.0,
                    rss_bytes: 9_999,
                }],
                totals: ProcessAttributionTotals {
                    combined: ProcessResourceTotals {
                        cpu_percent: 99.0,
                        rss_bytes: 9_999,
                        process_count: 1,
                    },
                    core: ProcessResourceTotals::default(),
                    external: ProcessResourceTotals {
                        cpu_percent: 99.0,
                        rss_bytes: 9_999,
                        process_count: 1,
                    },
                },
                ui_coverage: UiCoverage::default(),
            })),
            error: None,
        });

        assert_eq!(
            wire["processes"]
                .as_array()
                .expect("processes")
                .iter()
                .map(|process| process["pid"].as_u64().expect("pid"))
                .collect::<Vec<_>>(),
            [20]
        );
        assert_eq!(wire["totals"]["combined"]["processCount"], 1);
    }

    #[tokio::test]
    async fn unit_build_covers_server_terminal_callbacks_payloads_and_wire_adapters() {
        let temp = tempfile::tempdir().expect("terminal workspace");
        let terminal = TerminalManager::new(
            Arc::new(PortablePtyBackend),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::from_millis(20),
                ..TerminalManagerOptions::default()
            },
        );
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
        let usage = ProviderUsageService::new(Vec::new(), Arc::new(OffsetDateTime::now_utc));
        let relay = RelayClientService::new(
            || async {
                RelayClientStatus::Missing {
                    version: "1.0.0".to_owned(),
                }
            },
            |report| async move {
                report(RelayClientInstallEvent::Progress {
                    stage: "checking".to_owned(),
                })
                .await?;
                Ok(RelayClientStatus::Missing {
                    version: "1.0.0".to_owned(),
                })
            },
        );
        let config = ServerConfig::new(temp.path());
        crate::test_support::hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({}),
        );
        let control = Arc::new(NativeServerControl::new(config, json!({"policy":"test"})).await);
        let services = ServerTerminalServices::new(
            terminal,
            sampler,
            resource_sampler,
            monitor,
            usage,
            relay,
            control,
        );

        services
            .launch_setup_script(SetupScriptLaunch {
                thread_id: "thread-1".to_owned(),
                terminal_id: "setup-1".to_owned(),
                script_id: "script-1".to_owned(),
                script_name: "Setup".to_owned(),
                command: if cfg!(windows) {
                    "Write-Output setup".to_owned()
                } else {
                    "printf setup".to_owned()
                },
                cwd: temp.path().to_path_buf(),
                worktree_path: temp.path().to_path_buf(),
                env: BTreeMap::new(),
            })
            .await
            .expect("setup script launches");
        services.close_thread_terminals("thread-1").await;

        let start: TerminalStartPayload = decode_payload(&json!({
            "threadId":"thread-2",
            "terminalId":"terminal-2",
            "cwd":temp.path(),
            "env":{},
            "centerPanel":true,
            "command": {
                "executable": "/opt/codex",
                "args": ["--dangerously-bypass-approvals-and-sandbox"],
                "label": "Codex Terminal"
            }
        }))
        .expect("terminal start payload");
        let open = start.into_open(false).expect("default dimensions");
        assert_eq!((open.cols, open.rows), (120, 30));
        assert!(open.center_panel);
        assert_eq!(
            open.command,
            Some(TerminalLaunchCommand {
                executable: "/opt/codex".to_owned(),
                args: vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()],
                label: Some("Codex Terminal".to_owned()),
                activity: None,
                env: None,
            })
        );
        let missing_dimensions: TerminalStartPayload = decode_payload(&json!({
            "threadId":"thread-2",
            "terminalId":"terminal-2",
            "cwd":temp.path(),
            "env":{}
        }))
        .expect("terminal restart payload");
        assert!(
            !missing_dimensions.center_panel,
            "centerPanel defaults to false"
        );
        assert!(missing_dimensions.into_open(true).is_err());
        let attach: TerminalAttachPayload = decode_payload(&json!({
            "threadId":"thread-2",
            "terminalId":"terminal-2",
            "cwd":temp.path(),
            "cols":80,
            "rows":24,
            "env":{"UNIT":"1"},
            "restartIfNotRunning":true,
            "centerPanel":true,
            "command": {
                "executable": "/opt/codex",
                "args": ["--dangerously-bypass-approvals-and-sandbox"],
                "label": "Codex Terminal"
            }
        }))
        .expect("terminal attach payload");
        let attach = attach
            .into_attach(None)
            .expect("valid terminal attach payload");
        assert_eq!(attach.cols, Some(80));
        assert!(attach.restart_if_not_running);
        assert!(attach.center_panel);
        assert_eq!(
            attach.command,
            Some(TerminalLaunchCommand {
                executable: "/opt/codex".to_owned(),
                args: vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()],
                label: Some("Codex Terminal".to_owned()),
                activity: None,
                env: None,
            })
        );
        assert!(decode_payload::<TerminalStartPayload>(&json!({})).is_err());

        let now = OffsetDateTime::UNIX_EPOCH;
        let usage_value = provider_usage_to_wire(ProviderUsageResult {
            read_at: now,
            is_fetching: true,
            providers: vec![
                ProviderUsageSnapshot {
                    provider: ProviderUsageProvider::Claude,
                    status: ProviderUsageStatus::Ok,
                    session: Some(ProviderUsageWindow {
                        used_percent: 25,
                        window_minutes: 300,
                        resets_at: Some(now),
                        reset_description: Some("soon".to_owned()),
                    }),
                    weekly: None,
                    fable_weekly: None,
                    plan_type: None,
                    rate_limit_reset_credits: None,
                    updated_at: now,
                    error: None,
                    metadata: BTreeMap::new(),
                },
                ProviderUsageSnapshot {
                    provider: ProviderUsageProvider::Codex,
                    status: ProviderUsageStatus::Error,
                    session: None,
                    weekly: None,
                    fable_weekly: None,
                    plan_type: None,
                    rate_limit_reset_credits: None,
                    updated_at: now,
                    error: Some("unavailable".to_owned()),
                    metadata: BTreeMap::from([("source".to_owned(), "test".to_owned())]),
                },
            ],
        });
        assert_eq!(usage_value["providers"][0]["provider"], "claude");
        assert_eq!(usage_value["providers"][1]["status"], "error");

        let history = resource_history_to_wire(ProcessResourceHistory {
            read_at_ms: 0,
            window_ms: 60_000,
            bucket_ms: 1_000,
            sample_interval_ms: 500,
            retained_sample_count: 2,
            total_cpu_seconds_approx: SplitMetric {
                combined: 1.5,
                core: 1.0,
                external: 0.5,
            },
            ui_coverage: UiCoverage::default(),
            buckets: vec![ProcessResourceBucket {
                started_at_ms: 0,
                ended_at_ms: 1_000,
                cpu_percent: BucketMetric {
                    average: SplitMetric {
                        combined: 5.0,
                        core: 3.0,
                        external: 2.0,
                    },
                    peak: SplitMetric {
                        combined: 10.0,
                        core: 6.0,
                        external: 4.0,
                    },
                },
                rss_bytes: BucketMetric {
                    average: SplitMetric {
                        combined: 768,
                        core: 512,
                        external: 256,
                    },
                    peak: SplitMetric {
                        combined: 1024,
                        core: 640,
                        external: 384,
                    },
                },
                max_process_count: SplitMetric {
                    combined: 2,
                    core: 1,
                    external: 1,
                },
            }],
            processes: vec![ProcessResourceSummary {
                process_key: "1:server".to_owned(),
                pid: 1,
                ppid: 0,
                command: "server".to_owned(),
                depth: 0,
                scope: AttributionScope::Core,
                kind: AttributionKind::Server,
                label: "server".to_owned(),
                confidence: AttributionConfidence::Exact,
                first_seen_at_ms: 0,
                last_seen_at_ms: 1_000,
                current_cpu_percent: 2.0,
                avg_cpu_percent: 3.0,
                max_cpu_percent: 4.0,
                cpu_seconds_approx: 1.0,
                current_rss_bytes: 512,
                max_rss_bytes: 1024,
                sample_count: 2,
            }],
            error: Some("sample failed".to_owned()),
        });
        assert_eq!(
            history["buckets"][0]["maxProcessCount"],
            json!({ "combined": 2, "core": 1, "external": 1 })
        );
        assert_eq!(history["error"]["_tag"], "Some");

        assert_eq!(
            relay_status_to_wire(RelayClientStatus::Available {
                executable_path: "/tmp/cloudflared".to_owned(),
                source: "managed".to_owned(),
                version: "1".to_owned(),
            })["status"],
            "available"
        );
        assert_eq!(
            relay_status_to_wire(RelayClientStatus::Unsupported {
                platform: "plan9".to_owned(),
                arch: "mips".to_owned(),
                version: "1".to_owned(),
            })["status"],
            "unsupported"
        );
        assert_eq!(
            relay_event_to_wire(RelayClientInstallEvent::Progress {
                stage: "download".to_owned()
            })["type"],
            "progress"
        );
        assert_eq!(
            relay_event_to_wire(RelayClientInstallEvent::Complete {
                status: RelayClientStatus::Missing {
                    version: "1".to_owned()
                }
            })["type"],
            "complete"
        );
        assert_eq!(
            terminal_metadata_to_wire(TerminalMetadataEvent::Remove {
                thread_id: "thread".to_owned(),
                terminal_id: "terminal".to_owned(),
                reason: crate::terminal::TerminalRemovalReason::Closed,
            })["reason"],
            "closed"
        );
        assert_eq!(effect_none()["_tag"], "None");
        assert_eq!(effect_some(json!(1))["value"], 1);
        assert_eq!(invalid_request("bad")["_tag"], "RpcRequestInvalid");
        assert_eq!(format_epoch_ms(i128::MAX), "1970-01-01T00:00:00Z");
        assert!(format_time(now).contains("1970-01-01"));

        assert!(matches!(
            services.relay.resolve().await,
            RelayClientStatus::Missing { .. }
        ));
        assert!(
            !services
                .relay
                .install()
                .await
                .expect("relay install events")
                .is_empty()
        );
        services.shutdown().await;
    }

    #[test]
    fn codex_reset_wire_serializes_outcomes_complete_snapshots_and_typed_errors() {
        let now = OffsetDateTime::UNIX_EPOCH;
        let usage = ProviderUsageResult {
            read_at: now,
            is_fetching: false,
            providers: vec![
                ProviderUsageSnapshot {
                    provider: ProviderUsageProvider::Claude,
                    status: ProviderUsageStatus::Unavailable,
                    session: None,
                    weekly: None,
                    fable_weekly: None,
                    plan_type: None,
                    rate_limit_reset_credits: None,
                    updated_at: now,
                    error: None,
                    metadata: BTreeMap::new(),
                },
                ProviderUsageSnapshot {
                    provider: ProviderUsageProvider::Codex,
                    status: ProviderUsageStatus::Ok,
                    session: None,
                    weekly: None,
                    fable_weekly: Some(ProviderUsageWindow {
                        used_percent: 40,
                        window_minutes: 10_080,
                        resets_at: None,
                        reset_description: None,
                    }),
                    plan_type: Some("pro".to_owned()),
                    rate_limit_reset_credits: Some(RateLimitResetCredits {
                        available_count: 2,
                        total_earned_count: None,
                        next_expires_at: None,
                    }),
                    updated_at: now,
                    error: None,
                    metadata: BTreeMap::new(),
                },
            ],
        };

        for (outcome, expected) in [
            (CodexRateLimitResetOutcome::Reset, "reset"),
            (CodexRateLimitResetOutcome::NothingToReset, "nothingToReset"),
            (CodexRateLimitResetOutcome::NoCredit, "noCredit"),
            (
                CodexRateLimitResetOutcome::AlreadyRedeemed,
                "alreadyRedeemed",
            ),
        ] {
            let wire = consume_codex_rate_limit_reset_to_wire(ConsumeCodexRateLimitResetResult {
                outcome,
                usage: usage.clone(),
            });
            assert_eq!(wire["outcome"], expected);
            let absent = &wire["usage"]["providers"][0];
            assert_eq!(absent["fableWeekly"], Value::Null);
            assert_eq!(absent["planType"], Value::Null);
            assert_eq!(absent["rateLimitResetCredits"], Value::Null);
            let codex = &wire["usage"]["providers"][1];
            assert_eq!(codex["fableWeekly"]["usedPercent"], 40);
            assert_eq!(codex["planType"], "pro");
            assert_eq!(codex["rateLimitResetCredits"]["availableCount"], 2);
            assert_eq!(
                codex["rateLimitResetCredits"]["totalEarnedCount"],
                Value::Null
            );
            assert_eq!(codex["rateLimitResetCredits"]["nextExpiresAt"], Value::Null);
        }

        assert_eq!(
            provider_usage_reset_error_to_wire(ProviderUsageCommandError::new(
                "Codex reset request failed."
            )),
            json!({
                "_tag": "ServerProviderUsageResetError",
                "message": "Codex reset request failed.",
            })
        );
    }

    #[test]
    fn launch_command_env_is_validated_before_it_is_broadcast() {
        let command = |env: Value| {
            serde_json::from_value::<TerminalLaunchCommand>(
                json!({"executable": "opencode", "args": [], "env": env}),
            )
            .expect("command shape")
        };
        let validate = |env: Value| validate_terminal_launch_command(Some(command(env)));
        assert!(validate(json!({"OPENCODE_CONFIG_CONTENT": "{\"theme\":\"system\"}"})).is_ok());
        assert!(validate(json!({"_UNDER_1": "v"})).is_ok());
        assert!(validate(json!({"BAD-KEY": "v"})).is_err());
        assert!(validate(json!({"1BAD": "v"})).is_err());
        assert!(validate(json!({"": "v"})).is_err());
        assert!(validate(json!({"K".repeat(129): "v"})).is_err());
        assert!(validate(json!({"BIG": "x".repeat(8_193)})).is_err());
        let many = (0..129)
            .map(|index| (format!("K{index}"), json!("v")))
            .collect::<serde_json::Map<_, _>>();
        assert!(validate(Value::Object(many)).is_err());
    }
}

#[cfg(test)]
#[path = "server_terminal_input_tests.rs"]
mod input_admission_tests;
