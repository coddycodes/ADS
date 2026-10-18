use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
#[cfg(test)]
use tokio::sync::{Barrier, Notify};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    ServerConfig,
    activity::AgentActivitySource,
    diagnostics::{TraceDiagnosticsStore, profiler::enabled_setting},
    persistence::{read_json, write_bytes_atomically, write_json_atomically},
    production::{
        agent_activity::AgentActivitySettingsHandler,
        keybindings, provider_inventory,
        provider_maintenance::{
            ProviderMaintenance, ProviderMaintenanceTarget, ProviderUpdateLifecycleToken,
            provider_version_advanced, provider_version_regressed,
        },
        provider_runtime::resolve_provider_executable_with_environment,
        server_terminal::{JsonFuture, JsonStream, ProductionServerControl},
    },
    server_settings::{ProviderSettingsState, normalize_agent_activity_settings},
};

const MAX_KEYBINDINGS: usize = 256;
const PROVIDER_REFRESH_TTL: Duration = Duration::from_secs(5 * 60);
const PROVIDER_PROBE_TIMESTAMP_PATHS: &[&[&str]] =
    &[&["checkedAt"], &["versionAdvisory", "checkedAt"]];

enum ProviderSnapshotPublication {
    Always,
    IfChanged,
}

enum FullProviderRefreshTrigger {
    Subscription,
    SettingsChange,
    BackgroundCheck { cancellation: CancellationToken },
}

fn provider_snapshot_identity(snapshot: &Value) -> Option<(&str, &str)> {
    Some((
        snapshot.get("instanceId")?.as_str()?,
        snapshot.get("driver")?.as_str()?,
    ))
}

fn merge_provider_snapshot(
    current: Option<&Value>,
    refreshed: provider_inventory::ProviderProbeResult,
) -> Value {
    let models_authoritative = refreshed.models_authoritative;
    let mut next = refreshed.snapshot;
    if refreshed.rich_metadata == provider_inventory::RichMetadataOutcome::Succeeded {
        return next;
    }
    let Some(current) = current else {
        return next;
    };
    let Some(current_identity) = provider_snapshot_identity(current) else {
        return next;
    };
    if provider_snapshot_identity(&next) != Some(current_identity) {
        return next;
    }
    if !models_authoritative && let Some(value) = current.get("models") {
        next["models"] = value.clone();
    }
    for field in ["slashCommands", "skills", "agents"] {
        if let Some(value) = current.get(field) {
            next[field] = value.clone();
        }
    }
    next
}

fn provider_snapshot_content_eq(current: &Value, next: &Value) -> bool {
    fn equal_at_path(current: &Value, next: &Value, path: &[&str]) -> bool {
        let (Value::Object(current), Value::Object(next)) = (current, next) else {
            return current == next;
        };
        let is_ignored = |key: &str| {
            PROVIDER_PROBE_TIMESTAMP_PATHS
                .iter()
                .any(|ignored| ignored.strip_prefix(path) == Some(&[key]))
        };
        let content_len = |object: &serde_json::Map<String, Value>| {
            object.keys().filter(|key| !is_ignored(key)).count()
        };
        content_len(current) == content_len(next)
            && current.iter().all(|(key, value)| {
                if is_ignored(key) {
                    return true;
                }
                let Some(next_value) = next.get(key) else {
                    return false;
                };
                // Descend only along an ignored path; compare all other content
                // directly without cloning the inventory or allocating paths.
                let nested = PROVIDER_PROBE_TIMESTAMP_PATHS.iter().find(|ignored| {
                    ignored.starts_with(path)
                        && ignored.len() > path.len() + 1
                        && ignored[path.len()] == key
                });
                match nested {
                    Some(ignored) => equal_at_path(value, next_value, &ignored[..path.len() + 1]),
                    None => value == next_value,
                }
            })
    }
    equal_at_path(current, next, &[])
}

fn provider_update_state(
    status: &str,
    started_at: Option<&str>,
    finished_at: Option<&str>,
    message: &str,
    output: Option<&str>,
) -> Value {
    json!({
        "status": status,
        "startedAt": started_at,
        "finishedAt": finished_at,
        "message": message,
        "output": output,
    })
}

fn post_update_status(
    providers: &[Value],
    instance_id: &str,
    before_version: Option<&str>,
) -> &'static str {
    let Some(provider) = providers
        .iter()
        .find(|provider| provider["instanceId"] == instance_id)
    else {
        return "unchanged";
    };
    let driver = provider["driver"].as_str().unwrap_or_default();
    let after_version = provider["version"].as_str();
    match provider["versionAdvisory"]["status"].as_str() {
        Some("current") if provider_version_regressed(driver, before_version, after_version) => {
            "unchanged"
        }
        Some("current") => "succeeded",
        Some("behind_latest") => "unchanged",
        _ if provider_version_advanced(driver, before_version, after_version) => "succeeded",
        _ => "unchanged",
    }
}

fn provider_update_error(provider: &str, reason: impl Into<String>) -> Value {
    json!({
        "_tag": "ServerProviderUpdateError",
        "provider": provider,
        "reason": reason.into(),
    })
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct ProviderProbePause {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderUpdateVerification {
    provider: String,
    instance_id: String,
    before_version: String,
    after_version: String,
    expected_version: String,
    exit_code: i32,
    status: String,
}

#[derive(Debug)]
pub(crate) struct ProviderUpdateCheckTask {
    cancellation: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    refresh_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl ProviderUpdateCheckTask {
    pub(crate) async fn shutdown(&self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
        if let Some(task) = self.refresh_task.lock().await.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ProviderUpdateCheckTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
impl ProviderProbePause {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }

    async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    fn release(&self) {
        self.release.notify_one();
    }
}

#[derive(Clone, Default)]
struct AgentActivityHandlerSlot {
    handler: Arc<RwLock<Option<Arc<dyn AgentActivitySettingsHandler>>>>,
}

impl fmt::Debug for AgentActivityHandlerSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentActivityHandlerSlot")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct ProviderRefreshState {
    last_completed: Option<Instant>,
    manual_refreshes: usize,
}

struct ManualFullProviderRefresh {
    state: Arc<std::sync::Mutex<ProviderRefreshState>>,
}

impl ManualFullProviderRefresh {
    fn new(state: Arc<std::sync::Mutex<ProviderRefreshState>>) -> Self {
        state
            .lock()
            .expect("provider refresh state lock")
            .manual_refreshes += 1;
        Self { state }
    }
}

impl Drop for ManualFullProviderRefresh {
    fn drop(&mut self) {
        self.state
            .lock()
            .expect("provider refresh state lock")
            .manual_refreshes -= 1;
    }
}

#[derive(Clone, Debug)]
pub struct NativeServerControl {
    config: ServerConfig,
    auth_descriptor: Value,
    state_directory: PathBuf,
    settings_path: PathBuf,
    keybindings_path: PathBuf,
    settings: Arc<RwLock<Value>>,
    automatic_git_fetch_interval: watch::Sender<Duration>,
    profiler_enabled: watch::Sender<bool>,
    settings_update_lock: Arc<Mutex<()>>,
    settings_generation: Arc<AtomicU64>,
    agent_activity_handler: AgentActivityHandlerSlot,
    next_provider_probe_sequence: Arc<AtomicU64>,
    latest_committed_provider_probe_sequence: Arc<AtomicU64>,
    #[cfg(test)]
    provider_update_refresh_attempts: Arc<AtomicU64>,
    #[cfg(test)]
    latest_full_provider_refresh_generation: Arc<AtomicU64>,
    settings_load_error: Option<Value>,
    #[cfg(test)]
    settings_update_barrier: Arc<RwLock<Option<Arc<Barrier>>>>,
    #[cfg(test)]
    next_settings_persisted_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    next_provider_capabilities_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    next_provider_update_running_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    next_provider_update_after_version_probe_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    next_quick_provider_probe_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    next_full_provider_probe_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    next_full_provider_refresh_handoff_pause: Arc<Mutex<Option<ProviderProbePause>>>,
    #[cfg(test)]
    provider_update_verifications: Arc<std::sync::Mutex<Vec<ProviderUpdateVerification>>>,
    keybinding_rules: Arc<RwLock<Vec<Value>>>,
    keybinding_issues: Arc<RwLock<Vec<Value>>>,
    providers: Arc<RwLock<Vec<Value>>>,
    provider_maintenance: ProviderMaintenance,
    capability_tasks: TaskTracker,
    capability_shutdown: CancellationToken,
    capability_admission: Arc<std::sync::Mutex<()>>,
    capability_cache: Arc<std::sync::Mutex<CapabilityCache>>,
    full_provider_refresh_running: Arc<AtomicBool>,
    provider_refresh_state: Arc<std::sync::Mutex<ProviderRefreshState>>,
    activity_protocol_registered: Arc<AtomicBool>,
    config_events: broadcast::Sender<Value>,
    trace_diagnostics: TraceDiagnosticsStore,
}

impl crate::git::WorktreeBaseDirectoryProvider for NativeServerControl {
    fn worktree_base_directory<'a>(&'a self) -> crate::git::BoxWorktreeBaseDirectoryFuture<'a> {
        Box::pin(async move {
            self.settings
                .read()
                .await
                .get("worktreeBaseDirectory")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
    }
}

impl NativeServerControl {
    pub async fn new(config: ServerConfig, auth_descriptor: Value) -> Self {
        let trace_diagnostics =
            TraceDiagnosticsStore::new(config.state_dir().join("logs/server.trace.ndjson"));
        Self::with_trace_diagnostics(config, auth_descriptor, trace_diagnostics).await
    }

    pub async fn with_trace_diagnostics(
        config: ServerConfig,
        auth_descriptor: Value,
        trace_diagnostics: TraceDiagnosticsStore,
    ) -> Self {
        let state_directory = config.state_dir();
        let settings_path = state_directory.join("settings.json");
        let keybindings_path = state_directory.join("keybindings.json");
        let (mut settings, settings_load_error) = match read_json::<Value>(&settings_path).await {
            Ok(Some(settings)) => match validate_settings_document(&settings) {
                Ok(()) => (settings, None),
                Err(cause) => (
                    json!({}),
                    Some(settings_error(&settings_path, "normalize", &cause)),
                ),
            },
            Ok(None) => (json!({}), None),
            Err(error) => (
                json!({}),
                Some(settings_error(
                    &settings_path,
                    "read-file",
                    &error.to_string(),
                )),
            ),
        };
        apply_settings_defaults(&mut settings);
        redact_sensitive_environment(&mut settings);
        let (automatic_git_fetch_interval, _) =
            watch::channel(automatic_git_fetch_interval(&settings));
        let (profiler_enabled, _) = watch::channel(enabled_setting(&settings));
        let loaded_keybindings = keybindings::load(&keybindings_path).await;
        let cwd = std::env::current_dir().unwrap_or_else(|_| config.base_dir.clone());
        let provider_maintenance = ProviderMaintenance::new();
        let providers = provider_inventory::probe(&settings, None, &cwd, &provider_maintenance)
            .await
            .into_iter()
            .map(|result| result.snapshot)
            .collect();
        let (config_events, _) = broadcast::channel(32);
        Self {
            config,
            auth_descriptor,
            state_directory,
            settings_path,
            keybindings_path,
            settings: Arc::new(RwLock::new(settings.clone())),
            automatic_git_fetch_interval,
            profiler_enabled,
            settings_update_lock: Arc::new(Mutex::new(())),
            settings_generation: Arc::new(AtomicU64::new(0)),
            agent_activity_handler: AgentActivityHandlerSlot::default(),
            next_provider_probe_sequence: Arc::new(AtomicU64::new(0)),
            latest_committed_provider_probe_sequence: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            provider_update_refresh_attempts: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            latest_full_provider_refresh_generation: Arc::new(AtomicU64::new(0)),
            settings_load_error,
            #[cfg(test)]
            settings_update_barrier: Arc::new(RwLock::new(None)),
            #[cfg(test)]
            next_settings_persisted_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            next_provider_capabilities_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            next_provider_update_running_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            next_provider_update_after_version_probe_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            next_quick_provider_probe_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            next_full_provider_probe_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            next_full_provider_refresh_handoff_pause: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            provider_update_verifications: Arc::new(std::sync::Mutex::new(Vec::new())),
            keybinding_rules: Arc::new(RwLock::new(loaded_keybindings.rules)),
            keybinding_issues: Arc::new(RwLock::new(loaded_keybindings.issues)),
            providers: Arc::new(RwLock::new(providers)),
            provider_maintenance,
            capability_tasks: TaskTracker::new(),
            capability_shutdown: CancellationToken::new(),
            capability_admission: Arc::new(std::sync::Mutex::new(())),
            capability_cache: Arc::default(),
            full_provider_refresh_running: Arc::new(AtomicBool::new(false)),
            provider_refresh_state: Arc::new(
                std::sync::Mutex::new(ProviderRefreshState::default()),
            ),
            activity_protocol_registered: Arc::new(AtomicBool::new(false)),
            config_events,
            trace_diagnostics,
        }
    }

    /// Marks the structured activity protocol ready after the production registry
    /// finalization boundary has validated every required RPC.
    pub(super) fn mark_activity_protocol_registered_after_validation(&self) {
        self.activity_protocol_registered
            .store(true, Ordering::Release);
    }

    pub async fn config_snapshot(&self) -> Value {
        let settings = self.settings.read().await.clone();
        let rules = self.keybinding_rules.read().await.clone();
        let issues = self.keybinding_issues.read().await.clone();
        let providers = self.providers.read().await.clone();
        let cwd = current_directory(&self.config);
        json!({
            "environment": environment_descriptor(
                &self.config,
                self.activity_protocol_registered.load(Ordering::Acquire),
            ),
            "auth": self.auth_descriptor,
            "cwd": cwd,
            "keybindingsConfigPath": self.keybindings_path.to_string_lossy(),
            "keybindings": keybindings::resolve(&rules),
            "issues": issues,
            "providers": providers,
            "availableEditors": available_editors(),
            "observability": observability_snapshot(&self.state_directory),
            "settings": settings,
        })
    }

    pub(crate) fn automatic_git_fetch_interval_signal(&self) -> watch::Sender<Duration> {
        self.automatic_git_fetch_interval.clone()
    }

    /// The committed `enableProfiler` setting; changes as soon as a settings update commits.
    pub fn profiler_enabled_signal(&self) -> watch::Receiver<bool> {
        self.profiler_enabled.subscribe()
    }

    /// The provider inventory this control publishes, for a launch to read a model's capabilities.
    pub(crate) fn published_provider_inventory(
        &self,
    ) -> crate::production::provider_runtime::PublishedProviderInventory {
        self.providers.clone()
    }

    pub async fn attach_agent_activity_handler(
        &self,
        handler: Arc<dyn AgentActivitySettingsHandler>,
    ) {
        *self.agent_activity_handler.handler.write().await = Some(handler);
    }

    pub async fn agent_activity_enabled(&self, source: AgentActivitySource) -> bool {
        agent_activity_enabled(&*self.settings.read().await, source)
    }

    async fn update_settings(&self, payload: Value) -> Result<Value, Value> {
        let mut patch = payload.get("patch").cloned().ok_or_else(|| {
            settings_error(&self.settings_path, "normalize", "missing settings patch")
        })?;
        if !patch.is_object() {
            return Err(settings_error(
                &self.settings_path,
                "normalize",
                "settings patch must be an object",
            ));
        }
        if let Some(raw) = patch.get("worktreeBaseDirectory").and_then(Value::as_str) {
            let normalized = super::worktree_workspace::normalize_worktree_workspace(raw)
                .await
                .map_err(|error| error.to_wire())?;
            patch["worktreeBaseDirectory"] = json!(normalized);
        }

        #[cfg(test)]
        if let Some(barrier) = self.settings_update_barrier.read().await.clone() {
            barrier.wait().await;
        }
        let update_guard = Arc::clone(&self.settings_update_lock).lock_owned().await;
        if let Some(error) = &self.settings_load_error {
            return Err(error.clone());
        }
        let current = self.settings.read().await.clone();
        let previous_agent_activity = [
            (
                AgentActivitySource::Chat,
                agent_activity_enabled(&current, AgentActivitySource::Chat),
            ),
            (
                AgentActivitySource::Terminal,
                agent_activity_enabled(&current, AgentActivitySource::Terminal),
            ),
        ];
        let mut next = current;
        apply_settings_patch(&mut next, patch);
        validate_settings_document(&next)
            .map_err(|cause| settings_error(&self.settings_path, "normalize", &cause))?;
        apply_settings_defaults(&mut next);
        validate_settings_document(&next)
            .map_err(|cause| settings_error(&self.settings_path, "normalize", &cause))?;
        let commit_control = self.clone();
        let commit = tokio::spawn(async move {
            persist_sensitive_environment(&commit_control.state_directory, &mut next)
                .await
                .map_err(|message| {
                    settings_error(&commit_control.settings_path, "write-secret", &message)
                })?;
            write_json_atomically(&commit_control.settings_path, &next)
                .await
                .map_err(|error| {
                    settings_error(
                        &commit_control.settings_path,
                        "write-file",
                        &error.to_string(),
                    )
                })?;
            #[cfg(test)]
            if let Some(pause) = commit_control
                .next_settings_persisted_pause
                .lock()
                .await
                .take()
            {
                pause.entered.notify_one();
                pause.release.notified().await;
            }
            redact_sensitive_environment(&mut next);
            let generation = commit_control
                .settings_generation
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            if let Some(handler) = commit_control
                .agent_activity_handler
                .handler
                .read()
                .await
                .clone()
            {
                for (source, previous_enabled) in previous_agent_activity {
                    let next_enabled = agent_activity_enabled(&next, source);
                    if next_enabled != previous_enabled {
                        let _ = handler.transition(source, next_enabled, generation).await;
                    }
                }
            }
            commit_control
                .provider_maintenance
                .invalidate_update_lifecycles(generation, |target| {
                    provider_inventory::maintenance_target(
                        &next,
                        &target.driver,
                        Some(&target.instance_id),
                    )
                    .as_ref()
                        == Some(target)
                });
            *commit_control.settings.write().await = next.clone();
            let next_fetch_interval = automatic_git_fetch_interval(&next);
            commit_control
                .automatic_git_fetch_interval
                .send_if_modified(|current| {
                    if *current == next_fetch_interval {
                        return false;
                    }
                    *current = next_fetch_interval;
                    true
                });
            let next_profiler_enabled = enabled_setting(&next);
            commit_control.profiler_enabled.send_if_modified(|current| {
                let changed = *current != next_profiler_enabled;
                *current = next_profiler_enabled;
                changed
            });
            commit_control.publish(json!({
                "version": 1,
                "type": "settingsUpdated",
                "payload": { "settings": next.clone() },
            }));
            drop(update_guard);
            // Once settings are committed, their inventory refresh must also survive
            // the caller disconnecting, including when it supersedes a manual probe.
            let cwd =
                std::env::current_dir().unwrap_or_else(|_| commit_control.config.base_dir.clone());
            let probe_sequence = commit_control.begin_provider_probe();
            let providers = commit_control
                .probe_provider_snapshots(&next, None, &cwd)
                .await;
            commit_control
                .publish_provider_snapshots_if_current(
                    providers,
                    false,
                    generation,
                    &next,
                    probe_sequence,
                    ProviderSnapshotPublication::IfChanged,
                )
                .await;
            let _ = commit_control.start_full_provider_refresh(
                generation,
                next.clone(),
                cwd,
                FullProviderRefreshTrigger::SettingsChange,
            );
            Ok::<Value, Value>(next)
        });
        commit.await.map_err(|_| {
            settings_error(
                &self.settings_path,
                "commit",
                "persisted settings commit task stopped unexpectedly",
            )
        })?
    }

    #[cfg(test)]
    async fn install_settings_update_barrier(&self, parties: usize) {
        *self.settings_update_barrier.write().await = Some(Arc::new(Barrier::new(parties)));
    }

    #[cfg(test)]
    async fn install_next_settings_persisted_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self.next_settings_persisted_pause.lock().await = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn install_next_provider_capabilities_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self.next_provider_capabilities_pause.lock().await = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn install_next_provider_update_running_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self.next_provider_update_running_pause.lock().await = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn install_next_provider_update_after_version_probe_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self
            .next_provider_update_after_version_probe_pause
            .lock()
            .await = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn install_next_quick_provider_probe_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self.next_quick_provider_probe_pause.lock().await = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn install_next_full_provider_probe_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self.next_full_provider_probe_pause.lock().await = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn install_next_full_provider_refresh_handoff_pause(&self) -> ProviderProbePause {
        let pause = ProviderProbePause::new();
        *self.next_full_provider_refresh_handoff_pause.lock().await = Some(pause.clone());
        pause
    }

    async fn refresh_providers(&self, payload: &Value) -> Value {
        self.provider_maintenance.begin_latest_version_refresh();
        let instance_id = payload.get("instanceId").and_then(Value::as_str);
        let _manual_refresh = instance_id
            .is_none()
            .then(|| ManualFullProviderRefresh::new(self.provider_refresh_state.clone()));
        let (generation, settings) = self.settings_snapshot().await;
        let cwd = std::env::current_dir().unwrap_or_else(|_| self.config.base_dir.clone());
        let probe_sequence = self.begin_provider_probe();
        let refreshed = self
            .probe_full_provider_snapshots(&settings, instance_id, &cwd)
            .await;
        let providers = match self
            .publish_provider_snapshots_if_current(
                refreshed,
                instance_id.is_some(),
                generation,
                &settings,
                probe_sequence,
                ProviderSnapshotPublication::Always,
            )
            .await
        {
            Some(providers) => {
                if instance_id.is_none() {
                    self.record_full_provider_refresh();
                }
                providers
            }
            None => self.providers.read().await.clone(),
        };
        json!({ "providers": providers })
    }

    pub(crate) async fn shutdown_capability_discovery(&self) {
        {
            let _guard = self
                .capability_admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.capability_shutdown.cancel();
            self.capability_tasks.close();
        }
        self.capability_tasks.wait().await;
    }

    async fn provider_capabilities(
        &self,
        payload: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, Value> {
        let failure =
            |reason: String| json!({"_tag":"ServerProviderCapabilitiesError","reason":reason});
        let instance_id = payload
            .get("instanceId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| failure("Select a provider to load skills.".to_owned()))?;
        let cwd = payload
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("Select a workspace to load skills.".to_owned()))?;
        let (generation, settings) = {
            let _guard = self.settings_update_lock.lock().await;
            let settings =
                crate::server_settings::ProviderSettingsStore::new(&self.state_directory)
                    .get()
                    .await
                    .map_err(|error| failure(error.to_string()))?;
            (
                self.settings_generation.load(Ordering::Acquire),
                serde_json::to_value(settings).map_err(|error| failure(error.to_string()))?,
            )
        };
        let result = provider_inventory::discover_capabilities(
            &settings,
            instance_id,
            Path::new(cwd),
            cancellation,
        )
        .await
        .map_err(failure)?;
        if self.settings_generation.load(Ordering::Acquire) != generation {
            return Err(failure(
                "Provider settings changed during skill discovery. Retry to reload the catalog."
                    .to_owned(),
            ));
        }
        Ok(result)
    }

    async fn publish_provider_update_state(
        &self,
        target: &ProviderMaintenanceTarget,
        token: ProviderUpdateLifecycleToken,
        state: Value,
    ) -> (Vec<Value>, bool) {
        let _update_guard = self.settings_update_lock.lock().await;
        let generation = self.settings_generation.load(Ordering::Acquire);
        let configured = provider_inventory::maintenance_target(
            &*self.settings.read().await,
            &target.driver,
            Some(&target.instance_id),
        );
        let retained = configured.as_ref() == Some(target)
            && self
                .provider_maintenance
                .update_lifecycle_is_current(target, generation, token)
            && self.provider_maintenance.set_update_state_if_current(
                &target.instance_id,
                &target.driver,
                token,
                state,
            );
        if !retained {
            self.provider_maintenance
                .invalidate_update_lifecycle_if_current(&target.instance_id, &target.driver, token);
        }
        let mut providers = self.providers.write().await;
        let mut changed = false;
        for provider in providers.iter_mut() {
            let previous = provider.get("updateState").cloned();
            self.provider_maintenance.overlay_update_state(provider);
            changed |= provider.get("updateState") != previous.as_ref();
        }
        let snapshot = providers.clone();
        drop(providers);
        if changed {
            self.publish_provider_snapshots(&snapshot);
        }
        (snapshot, retained)
    }

    async fn update_provider(
        &self,
        payload: &Value,
        cancellation: CancellationToken,
    ) -> Result<Value, Value> {
        let provider = payload
            .get("provider")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                provider_update_error("unknown", "provider must be a valid provider slug")
            })?;
        validate_slug(provider, "provider")
            .map_err(|reason| provider_update_error("unknown", reason))?;
        let instance_id = match payload.get("instanceId") {
            None => None,
            Some(Value::String(instance_id)) => {
                validate_slug(instance_id, "instanceId")
                    .map_err(|reason| provider_update_error(provider, reason))?;
                Some(instance_id.as_str())
            }
            Some(_) => {
                return Err(provider_update_error(
                    provider,
                    "instanceId must be a valid provider slug",
                ));
            }
        };
        let (_, settings) = self.settings_snapshot().await;
        let target = provider_inventory::maintenance_target(&settings, provider, instance_id)
            .ok_or_else(|| {
                provider_update_error(
                    provider,
                    "The requested provider instance does not match this provider.",
                )
            })?;
        let capabilities = self.provider_maintenance.capabilities(&target).await;
        #[cfg(test)]
        if let Some(pause) = self.next_provider_capabilities_pause.lock().await.take() {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        let latest_source = capabilities.latest;
        let update = capabilities.update.ok_or_else(|| {
            provider_update_error(
                provider,
                "This provider does not expose a safe native self-update command.",
            )
        })?;
        let reservation = {
            let _update_guard = self.settings_update_lock.lock().await;
            let current_target = match provider_inventory::maintenance_target(
                &*self.settings.read().await,
                &target.driver,
                Some(&target.instance_id),
            ) {
                Some(current_target) if current_target == target => current_target,
                _ => {
                    return Err(provider_update_error(
                        provider,
                        "The requested provider instance does not match this provider.",
                    ));
                }
            };
            let settings_generation = self.settings_generation.load(Ordering::Acquire);
            self.provider_maintenance
                .reserve_target(&current_target, settings_generation)
        }
        .map_err(|reason| provider_update_error(provider, reason))?;
        let update_token = reservation.token();
        let lock = self.provider_maintenance.command_lock(update.lock_key);
        let command_guard = match lock.clone().try_lock_owned() {
            Ok(guard) => guard,
            Err(_) => {
                let _ = self
                    .publish_provider_update_state(
                        &target,
                        update_token,
                        provider_update_state(
                            "queued",
                            None,
                            None,
                            "Waiting for another provider update to finish.",
                            None,
                        ),
                    )
                    .await;
                tokio::select! {
                    guard = lock.lock_owned() => guard,
                    () = cancellation.cancelled() => {
                        let finished_at = now_iso();
                        let (providers, _) = self.publish_provider_update_state(
                            &target,
                            update_token,
                            provider_update_state(
                                "failed",
                                None,
                                Some(&finished_at),
                                "Provider update was cancelled.",
                                Some("provider.maintenance.update was cancelled"),
                            ),
                        ).await;
                        return Ok(json!({ "providers": providers }));
                    }
                }
            }
        };
        let (current_generation, current_settings) = self.settings_snapshot().await;
        let current_target = provider_inventory::maintenance_target(
            &current_settings,
            &target.driver,
            Some(&target.instance_id),
        );
        let current_update = match current_target.as_ref() {
            Some(current_target) if current_target == &target => {
                self.provider_maintenance
                    .capabilities(current_target)
                    .await
                    .update
            }
            _ => None,
        };
        if current_update.as_ref() != Some(&update)
            || !self.provider_maintenance.update_lifecycle_is_current(
                &target,
                current_generation,
                update_token,
            )
        {
            drop(command_guard);
            self.provider_maintenance
                .invalidate_update_lifecycle_if_current(
                    &target.instance_id,
                    &target.driver,
                    update_token,
                );
            let (providers, _) = self
                .publish_provider_update_state(
                    &target,
                    update_token,
                    provider_update_state(
                        "failed",
                        None,
                        Some(&now_iso()),
                        "Provider settings changed before the queued update could start.",
                        None,
                    ),
                )
                .await;
            return Ok(json!({ "providers": providers }));
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| self.config.base_dir.clone());
        let resolved_executable = resolve_provider_executable_with_environment(
            &target.binary_path,
            target
                .environment
                .iter()
                .map(|(name, value)| (name.as_os_str(), value.as_os_str())),
        );
        let before_version =
            provider_inventory::probe_maintenance_target_version(&target, &cwd).await;
        #[cfg(test)]
        if let Some(pause) = self
            .next_provider_update_after_version_probe_pause
            .lock()
            .await
            .take()
        {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        let (_, pre_run_settings) = self.settings_snapshot().await;
        let pre_run_target = provider_inventory::maintenance_target(
            &pre_run_settings,
            &target.driver,
            Some(&target.instance_id),
        );
        let pre_run_update = match pre_run_target.as_ref() {
            Some(pre_run_target) if pre_run_target == &target => {
                self.provider_maintenance
                    .capabilities(pre_run_target)
                    .await
                    .update
            }
            _ => None,
        };
        if pre_run_update.as_ref() != Some(&update) {
            drop(command_guard);
            self.provider_maintenance
                .invalidate_update_lifecycle_if_current(
                    &target.instance_id,
                    &target.driver,
                    update_token,
                );
            let providers = self.providers.read().await.clone();
            return Ok(json!({ "providers": providers }));
        }
        let started_at = now_iso();
        let (_, running_published) = self
            .publish_provider_update_state(
                &target,
                update_token,
                provider_update_state(
                    "running",
                    Some(&started_at),
                    None,
                    "Updating provider.",
                    None,
                ),
            )
            .await;
        if !running_published {
            drop(command_guard);
            let providers = self.providers.read().await.clone();
            return Ok(json!({ "providers": providers }));
        }
        #[cfg(test)]
        if let Some(pause) = self.next_provider_update_running_pause.lock().await.take() {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        // The installed major decides an OpenCode package the path did not name.
        let update = crate::production::provider_maintenance::ProviderMaintenanceCapabilities {
            latest: latest_source,
            update: Some(update),
            ..crate::production::provider_maintenance::ProviderMaintenanceCapabilities::unknown()
        }
        .for_installed_version(&target.driver, before_version.as_deref())
        .update
        .expect("the update action is kept");
        let command_result = match self
            .provider_maintenance
            .run_update_command(&target, &update, &cancellation)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                drop(command_guard);
                let finished_at = now_iso();
                let (providers, _) = self
                    .publish_provider_update_state(
                        &target,
                        update_token,
                        provider_update_state(
                            "failed",
                            Some(&started_at),
                            Some(&finished_at),
                            "Provider update failed.",
                            Some(&error),
                        ),
                    )
                    .await;
                return Ok(json!({ "providers": providers }));
            }
        };
        if command_result.exit_code != 0 {
            drop(command_guard);
            let finished_at = now_iso();
            let message = format!(
                "Update command exited with code {}.",
                command_result.exit_code
            );
            let (providers, _) = self
                .publish_provider_update_state(
                    &target,
                    update_token,
                    provider_update_state(
                        "failed",
                        Some(&started_at),
                        Some(&finished_at),
                        &message,
                        command_result.output.as_deref(),
                    ),
                )
                .await;
            return Ok(json!({ "providers": providers }));
        }
        drop(command_guard);

        self.provider_maintenance.begin_latest_version_refresh();
        let (generation, settings) = self.settings_snapshot().await;
        let probe_sequence = self.begin_provider_probe();
        let refreshed = self
            .probe_full_provider_snapshots(&settings, Some(&target.instance_id), &cwd)
            .await;
        let verification = refreshed
            .iter()
            .map(|result| result.snapshot.clone())
            .collect::<Vec<_>>();
        let verified_provider = verification
            .iter()
            .find(|provider| provider["instanceId"] == target.instance_id);
        let after_version = verified_provider
            .and_then(|provider| provider["version"].as_str())
            .map(str::to_owned);
        let expected_version = verified_provider
            .and_then(|provider| provider["versionAdvisory"]["latestVersion"].as_str())
            .map(str::to_owned);
        let executable_display = resolved_executable
            .as_deref()
            .unwrap_or_else(|| Path::new(&target.binary_path))
            .display()
            .to_string();
        let (status, message) = match self
            .publish_provider_snapshots_if_current(
                refreshed,
                true,
                generation,
                &settings,
                probe_sequence,
                ProviderSnapshotPublication::IfChanged,
            )
            .await
        {
            Some(_) => {
                let status = post_update_status(
                    &verification,
                    &target.instance_id,
                    before_version.as_deref(),
                );
                let message = match status {
                    "succeeded" => "Provider updated.".to_owned(),
                    _ if verification.iter().any(|provider| {
                        provider["instanceId"] == target.instance_id
                            && provider["versionAdvisory"]["status"] == "behind_latest"
                    }) => match (after_version.as_deref(), expected_version.as_deref()) {
                        (Some(after_version), Some(expected_version)) => format!(
                            "Update command completed, but {executable_display} still reports {after_version}; expected {expected_version}."
                        ),
                        _ => "Update command completed, but BiBCode still detects an outdated provider version."
                            .to_owned(),
                    },
                    _ => "Update command completed, but BiBCode could not verify the provider version."
                        .to_owned(),
                };
                (status, message)
            }
            None => (
                "unchanged",
                "Update command completed, but BiBCode could not verify the provider version."
                    .to_owned(),
            ),
        };
        if status == "succeeded" {
            tracing::debug!(
                operation = "provider.maintenance.update.verify",
                provider = %target.driver,
                instance_id = %target.instance_id,
                executable = %executable_display,
                before_version = %before_version.as_deref().unwrap_or("unknown"),
                after_version = %after_version.as_deref().unwrap_or("unknown"),
                expected_version = %expected_version.as_deref().unwrap_or("unknown"),
                exit_code = command_result.exit_code,
                status = %status,
                "provider update verification"
            );
        } else {
            tracing::warn!(
                operation = "provider.maintenance.update.verify",
                provider = %target.driver,
                instance_id = %target.instance_id,
                executable = %executable_display,
                before_version = %before_version.as_deref().unwrap_or("unknown"),
                after_version = %after_version.as_deref().unwrap_or("unknown"),
                expected_version = %expected_version.as_deref().unwrap_or("unknown"),
                exit_code = command_result.exit_code,
                status = %status,
                "provider update verification"
            );
        }
        #[cfg(test)]
        self.provider_update_verifications
            .lock()
            .expect("provider update verification mutex poisoned")
            .push(ProviderUpdateVerification {
                provider: target.driver.clone(),
                instance_id: target.instance_id.clone(),
                before_version: before_version.as_deref().unwrap_or("unknown").to_owned(),
                after_version: after_version.as_deref().unwrap_or("unknown").to_owned(),
                expected_version: expected_version.as_deref().unwrap_or("unknown").to_owned(),
                exit_code: command_result.exit_code,
                status: status.to_owned(),
            });
        let finished_at = now_iso();
        let (providers, _) = self
            .publish_provider_update_state(
                &target,
                update_token,
                provider_update_state(
                    status,
                    Some(&started_at),
                    Some(&finished_at),
                    &message,
                    command_result.output.as_deref(),
                ),
            )
            .await;
        Ok(json!({ "providers": providers }))
    }

    async fn settings_snapshot(&self) -> (u64, Value) {
        let _update_guard = self.settings_update_lock.lock().await;
        (
            self.settings_generation.load(Ordering::Acquire),
            self.settings.read().await.clone(),
        )
    }

    async fn request_full_provider_refresh(
        &self,
        cancellation: CancellationToken,
        refresh_task: &Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    ) {
        #[cfg(test)]
        self.provider_update_refresh_attempts
            .fetch_add(1, Ordering::AcqRel);
        let (generation, settings) = self.settings_snapshot().await;
        let cwd = std::env::current_dir().unwrap_or_else(|_| self.config.base_dir.clone());
        if cancellation.is_cancelled() {
            return;
        }
        if let Some(task) = self.start_full_provider_refresh(
            generation,
            settings,
            cwd,
            FullProviderRefreshTrigger::BackgroundCheck { cancellation },
        ) && let Some(previous) = refresh_task.lock().await.replace(task)
        {
            let _ = previous.await;
        }
    }

    pub(crate) fn start_provider_update_checks(&self) -> ProviderUpdateCheckTask {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let control = self.clone();
        let refresh_task = Arc::new(Mutex::new(None));
        let task_refresh = refresh_task.clone();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    _ = interval.tick() => {
                        if task_cancellation.is_cancelled() {
                            break;
                        }
                        control
                            .request_full_provider_refresh(task_cancellation.clone(), &task_refresh)
                            .await;
                    }
                }
            }
        });
        ProviderUpdateCheckTask {
            cancellation,
            task: Mutex::new(Some(task)),
            refresh_task,
        }
    }

    fn begin_provider_probe(&self) -> u64 {
        self.next_provider_probe_sequence
            .fetch_add(1, Ordering::AcqRel)
            + 1
    }

    async fn probe_provider_snapshots(
        &self,
        settings: &Value,
        instance_id: Option<&str>,
        cwd: &Path,
    ) -> Vec<provider_inventory::ProviderProbeResult> {
        #[cfg(test)]
        let pause = self.next_quick_provider_probe_pause.lock().await.take();
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        provider_inventory::probe(settings, instance_id, cwd, &self.provider_maintenance).await
    }

    async fn probe_full_provider_snapshots(
        &self,
        settings: &Value,
        instance_id: Option<&str>,
        cwd: &Path,
    ) -> Vec<provider_inventory::ProviderProbeResult> {
        #[cfg(test)]
        let pause = self.next_full_provider_probe_pause.lock().await.take();
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
        provider_inventory::probe_full(settings, instance_id, cwd, &self.provider_maintenance).await
    }

    #[cfg(test)]
    async fn pause_full_provider_refresh_handoff(&self) {
        if let Some(pause) = self
            .next_full_provider_refresh_handoff_pause
            .lock()
            .await
            .take()
        {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    async fn publish_provider_snapshots_if_current(
        &self,
        refreshed: Vec<provider_inventory::ProviderProbeResult>,
        partial: bool,
        generation: u64,
        expected_settings: &Value,
        probe_sequence: u64,
        publication: ProviderSnapshotPublication,
    ) -> Option<Vec<Value>> {
        let _update_guard = self.settings_update_lock.lock().await;
        let settings_are_current = self.settings.read().await.eq(expected_settings);
        if self.settings_generation.load(Ordering::Acquire) != generation || !settings_are_current {
            return None;
        }
        if probe_sequence
            <= self
                .latest_committed_provider_probe_sequence
                .load(Ordering::Acquire)
        {
            if matches!(publication, ProviderSnapshotPublication::Always) {
                // A newer probe may have stored only timestamps without publishing.
                // Acknowledge the manual refresh with that current inventory, while
                // keeping its superseded result out of the cache and completion TTL.
                self.publish_provider_snapshots(&self.providers.read().await);
            }
            return None;
        }
        let (providers, changed) = self.merge_provider_snapshots(refreshed, partial).await;
        self.latest_committed_provider_probe_sequence
            .store(probe_sequence, Ordering::Release);
        if changed || matches!(publication, ProviderSnapshotPublication::Always) {
            self.publish_provider_snapshots(&providers);
        }
        Some(providers)
    }

    async fn merge_provider_snapshots(
        &self,
        refreshed: Vec<provider_inventory::ProviderProbeResult>,
        partial: bool,
    ) -> (Vec<Value>, bool) {
        let mut current = self.providers.write().await;
        let mut merged = if partial {
            let mut merged = current.clone();
            for result in refreshed {
                let Some(id) = result
                    .snapshot
                    .get("instanceId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                else {
                    continue;
                };
                let position = merged.iter().position(|row| {
                    row.get("instanceId").and_then(Value::as_str) == Some(id.as_str())
                });
                let snapshot =
                    merge_provider_snapshot(position.map(|index| &merged[index]), result);
                if let Some(position) = position {
                    merged[position] = snapshot;
                } else {
                    merged.push(snapshot);
                }
            }
            merged
        } else {
            refreshed
                .into_iter()
                .map(|result| {
                    let id = result.snapshot.get("instanceId").and_then(Value::as_str);
                    let previous = current
                        .iter()
                        .find(|row| row.get("instanceId").and_then(Value::as_str) == id);
                    merge_provider_snapshot(previous, result)
                })
                .collect()
        };
        if !partial {
            self.provider_maintenance
                .prune_update_states(merged.iter().filter_map(provider_snapshot_identity));
        }
        for provider in merged.iter_mut() {
            self.provider_maintenance.overlay_update_state(provider);
        }
        let changed = current.len() != merged.len()
            || current
                .iter()
                .zip(&merged)
                .any(|(current, next)| !provider_snapshot_content_eq(current, next));
        *current = merged;
        (current.clone(), changed)
    }

    fn publish_provider_snapshots(&self, providers: &[Value]) {
        self.publish(json!({
            "version": 1,
            "type": "providerStatuses",
            "payload": { "providers": providers },
        }));
    }

    fn record_full_provider_refresh(&self) {
        self.provider_refresh_state
            .lock()
            .expect("provider refresh state lock")
            .last_completed = Some(Instant::now());
    }

    fn start_full_provider_refresh(
        &self,
        mut generation: u64,
        mut settings: Value,
        cwd: PathBuf,
        trigger: FullProviderRefreshTrigger,
    ) -> Option<tokio::task::JoinHandle<()>> {
        {
            let state = self
                .provider_refresh_state
                .lock()
                .expect("provider refresh state lock");
            if (matches!(trigger, FullProviderRefreshTrigger::Subscription)
                && (state.manual_refreshes > 0
                    || state
                        .last_completed
                        .is_some_and(|completed| completed.elapsed() <= PROVIDER_REFRESH_TTL)))
                || self
                    .full_provider_refresh_running
                    .swap(true, Ordering::AcqRel)
            {
                return None;
            }
        }
        let control = self.clone();
        Some(tokio::spawn(async move {
            let mut owns_refresh = true;
            'refresh: loop {
                loop {
                    let probe_sequence = control.begin_provider_probe();
                    let providers = if let FullProviderRefreshTrigger::BackgroundCheck {
                        cancellation,
                    } = &trigger
                    {
                        tokio::select! {
                            () = cancellation.cancelled() => break 'refresh,
                            providers = control.probe_full_provider_snapshots(&settings, None, &cwd) => providers,
                        }
                    } else {
                        control
                            .probe_full_provider_snapshots(&settings, None, &cwd)
                            .await
                    };
                    if control
                        .publish_provider_snapshots_if_current(
                            providers,
                            false,
                            generation,
                            &settings,
                            probe_sequence,
                            ProviderSnapshotPublication::IfChanged,
                        )
                        .await
                        .is_some()
                    {
                        control.record_full_provider_refresh();
                        #[cfg(test)]
                        control
                            .latest_full_provider_refresh_generation
                            .store(generation, Ordering::Release);
                        break;
                    }
                    (generation, settings) = control.settings_snapshot().await;
                }
                control
                    .full_provider_refresh_running
                    .store(false, Ordering::Release);
                owns_refresh = false;
                let (latest_generation, latest_settings) = control.settings_snapshot().await;
                #[cfg(test)]
                control.pause_full_provider_refresh_handoff().await;
                if latest_generation == generation && latest_settings == settings {
                    break;
                }
                if control
                    .full_provider_refresh_running
                    .swap(true, Ordering::AcqRel)
                {
                    break;
                }
                owns_refresh = true;
                (generation, settings) = (latest_generation, latest_settings);
            }
            if owns_refresh {
                control
                    .full_provider_refresh_running
                    .store(false, Ordering::Release);
            }
        }))
    }

    async fn update_keybinding(&self, method: &str, payload: Value) -> Result<Value, Value> {
        keybindings::validate(&payload, method == "server.upsertKeybinding")
            .map_err(|detail| keybindings_error(&self.keybindings_path, &detail))?;
        let mut rules = self.keybinding_rules.write().await;
        if method == "server.removeKeybinding" {
            rules.retain(|rule| !keybindings::same_rule(rule, &payload));
        } else {
            let target = payload.get("replace").unwrap_or(&payload);
            rules.retain(|rule| !keybindings::same_rule(rule, target));
            let mut rule = payload;
            rule.as_object_mut()
                .expect("validated object")
                .remove("replace");
            rules.push(rule);
            if rules.len() > MAX_KEYBINDINGS {
                let excess = rules.len() - MAX_KEYBINDINGS;
                rules.drain(..excess);
            }
        }
        write_json_atomically(&self.keybindings_path, &*rules)
            .await
            .map_err(|error| keybindings_error(&self.keybindings_path, &error.to_string()))?;
        self.keybinding_issues.write().await.clear();
        let result = json!({ "keybindings": keybindings::resolve(&rules), "issues": [] });
        self.publish(json!({
            "version": 1,
            "type": "keybindingsUpdated",
            "payload": result.clone(),
        }));
        Ok(result)
    }

    fn publish(&self, event: Value) {
        let _ = self.config_events.send(event);
    }

    fn trace_diagnostics(&self) -> Value {
        self.trace_diagnostics.read()
    }
}

fn agent_activity_enabled(settings: &Value, source: AgentActivitySource) -> bool {
    let (field, fallback) = match source {
        AgentActivitySource::Chat => ("enableChatAgentActivity", true),
        AgentActivitySource::Terminal => ("enableTerminalAgentActivity", false),
    };
    settings
        .get(field)
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

/// How long a provider's skill and command catalog is answered without re-reading it.
/// Older entries are still answered at once and refreshed in the background.
const CAPABILITY_CACHE_FRESH: Duration = Duration::from_secs(60);
/// Catalogs kept at most; the least recently stored goes first, so worktrees that are gone
/// do not stay for the server's lifetime.
const CAPABILITY_CACHE_ENTRIES: usize = 64;

type CapabilityKey = (String, String);

/// Provider skill and command catalogs by instance and workspace. Discovery starts the
/// provider CLI (about a second for Claude), so a composer opening repeatedly must not
/// wait for it each time. Entries belong to one settings generation.
#[derive(Debug, Default)]
struct CapabilityCache {
    entries: std::collections::HashMap<CapabilityKey, CachedCapabilities>,
    refreshing: std::collections::HashSet<CapabilityKey>,
}

#[derive(Debug)]
struct CachedCapabilities {
    generation: u64,
    stored_at: std::time::Instant,
    value: Value,
}

impl CapabilityCache {
    fn key(payload: &Value) -> Option<CapabilityKey> {
        let field = |name| payload.get(name).and_then(Value::as_str).map(str::to_owned);
        Some((field("instanceId")?, field("cwd")?))
    }

    /// The cached catalog for this generation, and whether it should be refreshed.
    fn cached(&self, key: &CapabilityKey, generation: u64) -> Option<(Value, bool)> {
        self.entries
            .get(key)
            .filter(|entry| entry.generation == generation)
            .map(|entry| {
                (
                    entry.value.clone(),
                    entry.stored_at.elapsed() >= CAPABILITY_CACHE_FRESH,
                )
            })
    }

    /// Keeps a complete catalog. One that reports discovery issues stays uncached, so the
    /// composer's retry discovers again.
    fn store(&mut self, key: CapabilityKey, generation: u64, value: Value) {
        // Entries from older settings can never be answered again.
        self.entries
            .retain(|_, entry| entry.generation == generation);
        if value
            .get("issues")
            .and_then(Value::as_array)
            .is_some_and(|issues| !issues.is_empty())
        {
            self.entries.remove(&key);
            return;
        }
        if !self.entries.contains_key(&key)
            && self.entries.len() >= CAPABILITY_CACHE_ENTRIES
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            key,
            CachedCapabilities {
                generation,
                stored_at: std::time::Instant::now(),
                value,
            },
        );
    }
}

impl NativeServerControl {
    /// Runs discovery as a supervised task that finishes its cleanup even if the caller
    /// stops waiting, and caches a successful catalog.
    async fn discover_provider_capabilities(
        &self,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, Value> {
        let key = CapabilityCache::key(&payload);
        let generation = self.settings_generation.load(Ordering::Acquire);
        let task = {
            let _guard = self
                .capability_admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if self.capability_shutdown.is_cancelled() {
                return Err(
                    json!({"_tag":"ServerProviderCapabilitiesError","reason":"Skill discovery is unavailable while the server is shutting down."}),
                );
            }
            let owner = self.clone();
            self.capability_tasks.spawn(async move {
                let cancellation = cancellation.child_token();
                let probe = owner.provider_capabilities(&payload, &cancellation);
                tokio::pin!(probe);
                tokio::select! {
                    biased;
                    () = owner.capability_shutdown.cancelled() => {
                        cancellation.cancel();
                        probe.await
                    }
                    result = &mut probe => result,
                }
            })
        };
        let result = match task.await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => {
                Err(json!({"_tag":"RequestCancelled","method":"server.getProviderCapabilities"}))
            }
        };
        if let (Some(key), Ok(value)) = (key, &result) {
            self.capability_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .store(key, generation, value.clone());
        }
        result
    }

    async fn provider_capabilities_cached(
        &self,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, Value> {
        let generation = self.settings_generation.load(Ordering::Acquire);
        if let Some(key) = CapabilityCache::key(&payload) {
            let mut cache = self
                .capability_cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some((value, stale)) = cache.cached(&key, generation) {
                if stale && cache.refreshing.insert(key.clone()) {
                    drop(cache);
                    let owner = self.clone();
                    tokio::spawn(async move {
                        if let Err(error) = owner
                            .discover_provider_capabilities(payload, CancellationToken::new())
                            .await
                        {
                            let reason = error.get("reason").and_then(Value::as_str);
                            tracing::warn!(
                                instance_id = %key.0,
                                reason = reason.unwrap_or_default(),
                                "background provider capability refresh failed"
                            );
                        }
                        owner
                            .capability_cache
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .refreshing
                            .remove(&key);
                    });
                }
                return Ok(value);
            }
        }
        self.discover_provider_capabilities(payload, cancellation)
            .await
    }
}

impl ProductionServerControl for NativeServerControl {
    fn call(
        &self,
        method: &'static str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> JsonFuture {
        let control = self.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(json!({ "_tag": "RequestCancelled", "method": method }));
            }
            match method {
                "server.getConfig" => match &control.settings_load_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(control.config_snapshot().await),
                },
                "server.getSettings" => match &control.settings_load_error {
                    Some(error) => Err(error.clone()),
                    None => Ok(control.settings.read().await.clone()),
                },
                "server.updateSettings" => control.update_settings(payload).await,
                "server.getProviderCapabilities" => {
                    control
                        .provider_capabilities_cached(payload, cancellation)
                        .await
                }
                "server.refreshProviders" => {
                    // The task owns the manual-refresh guard through probe, publish,
                    // and completion recording even if the RPC future is dropped.
                    match tokio::spawn(async move { control.refresh_providers(&payload).await })
                        .await
                    {
                        Ok(providers) => Ok(providers),
                        Err(error) if error.is_panic() => {
                            std::panic::resume_unwind(error.into_panic())
                        }
                        Err(_) => Err(json!({ "_tag": "RequestCancelled", "method": method })),
                    }
                }
                "server.updateProvider" => control.update_provider(&payload, cancellation).await,
                "server.upsertKeybinding" | "server.removeKeybinding" => {
                    control.update_keybinding(method, payload).await
                }
                "server.getTraceDiagnostics" => Ok(control.trace_diagnostics()),
                _ => Err(json!({
                    "_tag": "InvalidRequest",
                    "method": method,
                    "message": "Unsupported native server-control request.",
                })),
            }
        })
    }

    fn subscribe(&self, method: &'static str, cancellation: CancellationToken) -> JsonStream {
        let (sender, receiver) = mpsc::channel(8);
        let control = self.clone();
        tokio::spawn(async move {
            match method {
                "subscribeServerConfig" => {
                    if let Some(error) = &control.settings_load_error {
                        let _ = sender.send(Err(error.clone())).await;
                        return;
                    }
                    let (generation, settings) = control.settings_snapshot().await;
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| control.config.base_dir.clone());
                    let _ = control.start_full_provider_refresh(
                        generation,
                        settings,
                        cwd,
                        FullProviderRefreshTrigger::Subscription,
                    );
                    let mut updates = control.config_events.subscribe();
                    if send_event(
                        &sender,
                        json!({
                            "version": 1,
                            "type": "snapshot",
                            "config": control.config_snapshot().await,
                        }),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    loop {
                        tokio::select! {
                            () = cancellation.cancelled() => return,
                            event = updates.recv() => match event {
                                Ok(event) => {
                                    if send_event(&sender, event).await.is_err() {
                                        return;
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(broadcast::error::RecvError::Closed) => return,
                            }
                        }
                    }
                }
                "subscribeServerLifecycle" => {
                    let cwd = current_directory(&control.config);
                    let project_name = Path::new(&cwd)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or("BiBCode");
                    let activity_protocol_registered =
                        control.activity_protocol_registered.load(Ordering::Acquire);
                    let environment =
                        environment_descriptor(&control.config, activity_protocol_registered);
                    if send_event(
                        &sender,
                        json!({
                            "version": 1,
                            "sequence": 1,
                            "type": "welcome",
                            "payload": {
                                "environment": environment,
                                "cwd": cwd,
                                "projectName": project_name,
                            },
                        }),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    if send_event(
                        &sender,
                        json!({
                            "version": 1,
                            "sequence": 2,
                            "type": "ready",
                            "payload": {
                                "at": now_iso(),
                                "environment": environment_descriptor(
                                    &control.config,
                                    activity_protocol_registered,
                                ),
                            },
                        }),
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    cancellation.cancelled().await;
                }
                _ => {}
            }
        });
        receiver
    }
}

async fn send_event(
    sender: &mpsc::Sender<Result<Vec<Value>, Value>>,
    event: Value,
) -> Result<(), ()> {
    sender.send(Ok(vec![event])).await.map_err(|_| ())
}

fn validate_settings_document(settings: &Value) -> Result<(), String> {
    let object = settings
        .as_object()
        .ok_or_else(|| "settings document must be an object".to_owned())?;
    let normalized = normalize_legacy_settings_for_validation(settings);
    serde_json::from_value::<ProviderSettingsState>(normalized)
        .map_err(|error| format!("invalid known provider settings shape: {error}"))?;

    validate_optional_bool(object, "enableAssistantStreaming")?;
    validate_optional_bool(object, "enableProviderUpdateChecks")?;
    validate_optional_bool(object, "enableAgentActivity")?;
    validate_optional_bool(object, "enableChatAgentActivity")?;
    validate_optional_bool(object, "enableTerminalAgentActivity")?;
    validate_optional_bool(object, "enableProfiler")?;
    validate_optional_bool(object, "newWorktreesStartFromOrigin")?;
    validate_optional_string(object, "worktreeBaseDirectory")?;
    validate_optional_string(object, "addProjectBaseDirectory")?;
    validate_optional_duration_millis(object, "automaticGitFetchInterval")?;
    if let Some(mode) = object.get("defaultThreadEnvMode") {
        match mode.as_str() {
            Some("local" | "worktree") => {}
            _ => {
                return Err(
                    "defaultThreadEnvMode must be either \"local\" or \"worktree\"".to_owned(),
                );
            }
        }
    }
    if let Some(selection) = object.get("textGenerationModelSelection") {
        validate_model_selection(selection, "textGenerationModelSelection")?;
    }
    if let Some(default_agent) = object.get("defaultAgent") {
        validate_default_agent(default_agent)?;
    }
    if let Some(providers) = object.get("providers") {
        validate_legacy_provider_settings(providers)?;
    }
    if let Some(instances) = object.get("providerInstances") {
        validate_provider_instances(instances)?;
    }
    if let Some(defaults) = object.get("providerSessionDefaults") {
        validate_provider_session_defaults(defaults)?;
    }
    if let Some(terminal) = object.get("terminal") {
        let terminal = terminal
            .as_object()
            .ok_or_else(|| "terminal must be an object".to_owned())?;
        validate_optional_bool(terminal, "webglEnabled")?;
    }
    Ok(())
}

fn automatic_git_fetch_interval(settings: &Value) -> Duration {
    let Some(milliseconds) = settings
        .get("automaticGitFetchInterval")
        .and_then(Value::as_f64)
    else {
        return Duration::from_secs(180);
    };
    Duration::try_from_secs_f64(milliseconds / 1_000.0).unwrap_or(Duration::MAX)
}

fn normalize_legacy_settings_for_validation(settings: &Value) -> Value {
    let mut normalized = settings.clone();
    if let Some(object) = normalized.as_object_mut()
        && object.contains_key("automaticGitFetchInterval")
    {
        // The public contract accepts every nonnegative JSON number, including
        // fractional milliseconds. ProviderSettingsState predates that
        // contract and uses u64, so validate the real shape separately and
        // neutralize only the surrogate decode.
        object.insert("automaticGitFetchInterval".to_owned(), json!(0));
    }
    if let Some(instances) = normalized
        .get_mut("providerInstances")
        .and_then(Value::as_object_mut)
    {
        for instance in instances.values_mut().filter_map(Value::as_object_mut) {
            instance.entry("config").or_insert(Value::Null);
        }
    }
    if let Some(defaults) = normalized
        .get_mut("providerSessionDefaults")
        .and_then(Value::as_object_mut)
    {
        for options in defaults.values_mut().filter_map(|value| {
            value
                .as_object_mut()
                .and_then(|value| value.get_mut("options"))
        }) {
            let Some(legacy) = options.as_object() else {
                continue;
            };
            *options = Value::Array(
                legacy
                    .iter()
                    .filter_map(|(id, value)| match value {
                        Value::String(value)
                            if !id.trim().is_empty() && !value.trim().is_empty() =>
                        {
                            Some(json!({ "id": id.trim(), "value": value.trim() }))
                        }
                        Value::Bool(value) if !id.trim().is_empty() => {
                            Some(json!({ "id": id.trim(), "value": value }))
                        }
                        _ => None,
                    })
                    .collect(),
            );
        }
    }
    normalized
}

fn validate_optional_bool(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<(), String> {
    if object.get(field).is_some_and(|value| !value.is_boolean()) {
        return Err(format!("{field} must be a boolean"));
    }
    Ok(())
}

fn validate_optional_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<(), String> {
    if object.get(field).is_some_and(|value| !value.is_string()) {
        return Err(format!("{field} must be a string"));
    }
    Ok(())
}

fn validate_optional_duration_millis(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<(), String> {
    if let Some(value) = object.get(field)
        && value
            .as_f64()
            .is_none_or(|milliseconds| !milliseconds.is_finite() || milliseconds < 0.0)
    {
        return Err(format!("{field} must be a nonnegative number"));
    }
    Ok(())
}

fn validate_optional_string_array(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<(), String> {
    if let Some(value) = object.get(field)
        && !value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    {
        return Err(format!("{field} must be an array of strings"));
    }
    Ok(())
}

fn validate_non_empty_string(value: &Value, field: &str) -> Result<(), String> {
    if value.as_str().is_none_or(|value| value.trim().is_empty()) {
        return Err(format!("{field} must be a non-empty string"));
    }
    Ok(())
}

fn validate_slug(value: &str, field: &str) -> Result<(), String> {
    let value = value.trim();
    let mut characters = value.chars();
    if value.len() > 64
        || characters
            .next()
            .is_none_or(|character| !character.is_ascii_alphabetic())
        || characters.any(|character| {
            !character.is_ascii_alphanumeric() && character != '-' && character != '_'
        })
    {
        return Err(format!("{field} must be a valid provider slug"));
    }
    Ok(())
}

fn validate_model_selection(selection: &Value, field: &str) -> Result<(), String> {
    let selection = selection
        .as_object()
        .ok_or_else(|| format!("{field} must be an object"))?;
    validate_non_empty_string(
        selection
            .get("model")
            .ok_or_else(|| format!("{field}.model is required"))?,
        &format!("{field}.model"),
    )?;
    let instance_id = selection
        .get("instanceId")
        .or_else(|| selection.get("provider"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field}.instanceId is required"))?;
    validate_slug(instance_id, &format!("{field}.instanceId"))?;
    if let Some(options) = selection.get("options") {
        validate_provider_options(options, &format!("{field}.options"))?;
    }
    Ok(())
}

fn validate_default_agent(value: &Value) -> Result<(), String> {
    let selection = value
        .as_object()
        .ok_or_else(|| "defaultAgent must be an object".to_owned())?;
    match selection.get("kind").and_then(Value::as_str) {
        Some("chat" | "terminal") => {}
        _ => return Err("defaultAgent.kind must be either \"chat\" or \"terminal\"".to_owned()),
    }
    validate_slug(
        selection
            .get("instanceId")
            .and_then(Value::as_str)
            .ok_or_else(|| "defaultAgent.instanceId is required".to_owned())?,
        "defaultAgent.instanceId",
    )
}

fn validate_legacy_provider_settings(providers: &Value) -> Result<(), String> {
    let providers = providers
        .as_object()
        .ok_or_else(|| "providers must be an object".to_owned())?;
    for (driver, string_fields) in [
        ("codex", &["binaryPath", "homePath", "shadowHomePath"][..]),
        ("claudeAgent", &["binaryPath", "homePath", "launchArgs"][..]),
        ("cursor", &["binaryPath", "apiEndpoint"][..]),
        ("grok", &["binaryPath"][..]),
        (
            "opencode",
            &["binaryPath", "serverUrl", "serverPassword"][..],
        ),
    ] {
        let Some(settings) = providers.get(driver) else {
            continue;
        };
        let settings = settings
            .as_object()
            .ok_or_else(|| format!("providers.{driver} must be an object"))?;
        validate_optional_bool(settings, "enabled")?;
        for field in string_fields {
            validate_optional_string(settings, field)?;
        }
        validate_optional_string_array(settings, "customModels")?;
    }
    Ok(())
}

fn validate_provider_instances(instances: &Value) -> Result<(), String> {
    let instances = instances
        .as_object()
        .ok_or_else(|| "providerInstances must be an object".to_owned())?;
    for (instance_id, instance) in instances {
        validate_slug(instance_id, "providerInstances key")?;
        let instance = instance
            .as_object()
            .ok_or_else(|| format!("providerInstances.{instance_id} must be an object"))?;
        let driver = instance
            .get("driver")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("providerInstances.{instance_id}.driver is required"))?;
        validate_slug(driver, &format!("providerInstances.{instance_id}.driver"))?;
        validate_optional_bool(instance, "enabled")?;
        for field in ["displayName", "accentColor"] {
            if let Some(value) = instance.get(field) {
                validate_non_empty_string(
                    value,
                    &format!("providerInstances.{instance_id}.{field}"),
                )?;
            }
        }
        if let Some(environment) = instance.get("environment") {
            let environment = environment.as_array().ok_or_else(|| {
                format!("providerInstances.{instance_id}.environment must be an array")
            })?;
            let mut names: Vec<&str> = Vec::with_capacity(environment.len());
            for (index, variable) in environment.iter().enumerate() {
                let variable = variable.as_object().ok_or_else(|| {
                    format!(
                        "providerInstances.{instance_id}.environment[{index}] must be an object"
                    )
                })?;
                let name = variable
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "providerInstances.{instance_id}.environment[{index}].name is required"
                        )
                    })?;
                let mut characters = name.trim().chars();
                if name.trim().len() > 128
                    || characters.next().is_none_or(|character| {
                        !character.is_ascii_alphabetic() && character != '_'
                    })
                    || characters
                        .any(|character| !character.is_ascii_alphanumeric() && character != '_')
                {
                    return Err(format!(
                        "providerInstances.{instance_id}.environment[{index}].name is invalid"
                    ));
                }
                if let Some(existing) = names
                    .iter()
                    .find(|existing| existing.eq_ignore_ascii_case(name.trim()))
                {
                    return Err(format!(
                        "providerInstances.{instance_id}.environment[{index}].name duplicates {existing}"
                    ));
                }
                names.push(name.trim());
                validate_optional_string(variable, "value")?;
                validate_optional_bool(variable, "sensitive")?;
                validate_optional_bool(variable, "valueRedacted")?;
            }
        }
    }
    Ok(())
}

fn validate_provider_session_defaults(defaults: &Value) -> Result<(), String> {
    let defaults = defaults
        .as_object()
        .ok_or_else(|| "providerSessionDefaults must be an object".to_owned())?;
    for (driver, value) in defaults {
        validate_slug(driver, "providerSessionDefaults key")?;
        let value = value
            .as_object()
            .ok_or_else(|| format!("providerSessionDefaults.{driver} must be an object"))?;
        validate_non_empty_string(
            value
                .get("model")
                .ok_or_else(|| format!("providerSessionDefaults.{driver}.model is required"))?,
            &format!("providerSessionDefaults.{driver}.model"),
        )?;
        if let Some(options) = value.get("options") {
            validate_provider_options(
                options,
                &format!("providerSessionDefaults.{driver}.options"),
            )?;
        }
    }
    Ok(())
}

fn validate_provider_options(options: &Value, field: &str) -> Result<(), String> {
    if options.is_object() {
        return Ok(());
    }
    let options = options
        .as_array()
        .ok_or_else(|| format!("{field} must be an array or legacy object"))?;
    for (index, option) in options.iter().enumerate() {
        let option = option
            .as_object()
            .ok_or_else(|| format!("{field}[{index}] must be an object"))?;
        validate_non_empty_string(
            option
                .get("id")
                .ok_or_else(|| format!("{field}[{index}].id is required"))?,
            &format!("{field}[{index}].id"),
        )?;
        let value = option
            .get("value")
            .ok_or_else(|| format!("{field}[{index}].value is required"))?;
        match value {
            Value::String(value) if !value.trim().is_empty() => {}
            Value::Bool(_) => {}
            _ => {
                return Err(format!(
                    "{field}[{index}].value must be a non-empty string or boolean"
                ));
            }
        }
    }
    Ok(())
}

fn apply_settings_defaults(settings: &mut Value) {
    if !settings.is_object() {
        *settings = json!({});
    }
    normalize_agent_activity_settings(settings);
    settings
        .as_object_mut()
        .expect("settings object")
        .remove("observability");
    crate::server_settings::merge_missing(
        settings,
        &json!({
            "enableAssistantStreaming": false,
            "enableProviderUpdateChecks": true,
            "enableChatAgentActivity": true,
            "enableTerminalAgentActivity": false,
            "automaticGitFetchInterval": 180_000,
            "defaultThreadEnvMode": "local",
            "newWorktreesStartFromOrigin": false,
            "worktreeBaseDirectory": "",
            "addProjectBaseDirectory": "",
            "textGenerationModelSelection": {
                "instanceId": "codex",
                "model": "gpt-5.4-mini",
            },
            "providers": crate::server_settings::built_in_provider_defaults(),
            "providerInstances": {},
            "providerSessionDefaults": {},
            "terminal": { "webglEnabled": true },
        }),
    );
    settings
        .as_object_mut()
        .expect("settings object")
        .entry("defaultAgent")
        .or_insert_with(|| json!({ "kind": "chat", "instanceId": "codex" }));
}

fn apply_settings_patch(target: &mut Value, patch: Value) {
    let Some(patch) = patch.as_object() else {
        return;
    };
    let target = target.as_object_mut().expect("settings object");
    for (key, value) in patch {
        if key == "providerInstances"
            || key == "providerSessionDefaults"
            || key == "automaticGitFetchInterval"
            || key == "defaultAgent"
        {
            target.insert(key.clone(), value.clone());
            continue;
        }
        if key == "textGenerationModelSelection"
            && value.as_object().is_some_and(|selection| {
                selection.contains_key("instanceId") || selection.contains_key("model")
            })
        {
            target.insert(key.clone(), value.clone());
            continue;
        }
        match target.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                merge_patch(existing, value.clone());
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn merge_patch(target: &mut Value, patch: Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        merge_patch(existing, value);
                    }
                    _ => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

async fn persist_sensitive_environment(root: &Path, settings: &mut Value) -> Result<(), String> {
    let Some(instances) = settings
        .get_mut("providerInstances")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    for (instance_id, instance) in instances {
        let Some(environment) = instance
            .get_mut("environment")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for variable in environment {
            if variable.get("sensitive").and_then(Value::as_bool) != Some(true) {
                variable
                    .as_object_mut()
                    .map(|object| object.remove("valueRedacted"));
                continue;
            }
            let name = variable
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let value = variable
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !value.is_empty() {
                let path = secret_path(root, instance_id, name);
                write_bytes_atomically(path, value.as_bytes())
                    .await
                    .map_err(|error| error.to_string())?;
            }
            let variable = variable
                .as_object_mut()
                .expect("environment variable object");
            variable.insert("value".into(), json!(""));
            variable.insert("valueRedacted".into(), json!(true));
        }
    }
    Ok(())
}

fn redact_sensitive_environment(settings: &mut Value) {
    let Some(instances) = settings
        .get_mut("providerInstances")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for instance in instances.values_mut() {
        let Some(environment) = instance
            .get_mut("environment")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for variable in environment {
            if variable.get("sensitive").and_then(Value::as_bool) == Some(true) {
                let object = variable
                    .as_object_mut()
                    .expect("environment variable object");
                object.insert("value".into(), json!(""));
                object.insert("valueRedacted".into(), json!(true));
            } else if let Some(object) = variable.as_object_mut() {
                object.remove("valueRedacted");
            }
        }
    }
}

fn secret_path(root: &Path, instance_id: &str, name: &str) -> PathBuf {
    root.join("secrets").join(format!(
        "provider-env-{}-{}",
        URL_SAFE_NO_PAD.encode(instance_id),
        URL_SAFE_NO_PAD.encode(name),
    ))
}

fn observability_snapshot(state_directory: &Path) -> Value {
    json!({
        "logsDirectoryPath": state_directory.join("logs").to_string_lossy(),
        "localTracingEnabled": true,
        "otlpTracesEnabled": false,
        "otlpMetricsEnabled": false,
    })
}

fn settings_error(path: &Path, operation: &str, cause: &str) -> Value {
    json!({
        "_tag": "ServerSettingsError",
        "settingsPath": path.to_string_lossy(),
        "operation": operation,
        "cause": cause,
    })
}

fn keybindings_error(path: &Path, detail: &str) -> Value {
    json!({
        "_tag": "KeybindingsConfigParseError",
        "configPath": path.to_string_lossy(),
        "detail": detail,
    })
}

fn current_directory(config: &ServerConfig) -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| config.base_dir.clone())
        .to_string_lossy()
        .into_owned()
}

fn environment_descriptor(config: &ServerConfig, activity_protocol_registered: bool) -> Value {
    json!({
        "environmentId": config.environment_id,
        "label": config.environment_label,
        "platform": { "os": platform_os(), "arch": platform_arch() },
        "serverVersion": config.server_version,
        "storageInstanceId": config
            .storage_instance_id
            .expect("a running server has a prepared persistent store")
            .to_string(),
        "bootId": config.boot_id.map(|id| id.to_string()),
        "remoteUpdateSupport": config.remote_update_support,
        "remoteProtocolVersion": crate::http::REMOTE_PROTOCOL_VERSION,
        "minCompatibleRemoteProtocol": crate::http::MIN_COMPATIBLE_REMOTE_PROTOCOL,
        "capabilities": {
            "repositoryIdentity": true,
            "worktreeCatalog": true,
            "worktreeCatalogRefreshReason": true,
            "vcsStatusSummary": true,
            "gitManagerReads": true,
            "gitManagerCommitOperations": true,
            "gitManagerBranchSyncOperations": true,
            "gitManagerStashMergeOperations": true,
            "gitManagerMergeIntoOperations": true,
            "gitManagerPartialStaging": true,
            "gitManagerRewriteOperations": true,
            "gitManagerTagOperations": true,
            "gitManagerLiveSignal": true,
            "gitManagerPullRequests": true,
            "gitPullRequestBranchSelection": true,
            "pullRequestsReads": true,
            "pullRequestsMutations": true,
            "activityProtocolVersion": activity_protocol_registered.then_some(2),
            "remoteUpdateControl": true,
            "remoteUpdateProgress": true,
            "terminalOrderedInput": true,
            "terminalSizeOwnership": true,
            "terminalImagePaste": true,
            "pullRequestCreateOptions": true,
            "vcsCloneReattach": true,
            "attachmentStaging": true,
            "profiler": true,
            "previewTunnels": true,
        },
    })
}

fn available_editors() -> Vec<&'static str> {
    crate::production::editor_launch::available_editor_ids(
        &crate::production::editor_launch::EditorProbeEnv::from_process(),
    )
}

const fn platform_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

const fn platform_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "other"
    }
}

pub(crate) fn now_iso() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Mutex as StdMutex, atomic::AtomicUsize},
        time::Duration,
    };

    use axum::{Json, Router, extract::State, routing::get};
    use tokio::{net::TcpListener, sync::OnceCell};
    use url::Url;
    use uuid::Uuid;

    use crate::{
        activity::AgentActivityController,
        production::{
            agent_activity::{AgentActivitySettingsHandler, AgentActivityTransitionReport},
            server_terminal::ProductionServerControl,
        },
        test_support::hermetic_providers,
    };

    use super::*;

    const TEST_STORAGE_INSTANCE_ID: Uuid = Uuid::from_u128(0x00000000000040008000000000000004);

    fn running_test_config(path: &Path) -> ServerConfig {
        let mut config = ServerConfig::new(path);
        config.storage_instance_id = Some(crate::persistence::StorageInstanceId::from_uuid(
            TEST_STORAGE_INSTANCE_ID,
        ));
        config
    }

    #[test]
    fn post_update_verification_distinguishes_success_and_unchanged() {
        let codex = |status: &str, version: &str| {
            json!({
                "instanceId": "codex",
                "driver": "codex",
                "version": version,
                "versionAdvisory": { "status": status }
            })
        };
        let cursor = |status: &str, version: &str| {
            json!({
                "instanceId": "cursor",
                "driver": "cursor",
                "version": version,
                "versionAdvisory": { "status": status }
            })
        };

        assert_eq!(
            post_update_status(&[codex("current", "0.147.0")], "codex", Some("0.146.0")),
            "succeeded"
        );
        assert_eq!(
            post_update_status(
                &[codex("behind_latest", "0.147.0")],
                "codex",
                Some("0.146.0")
            ),
            "unchanged"
        );
        assert_eq!(
            post_update_status(&[], "codex", Some("0.146.0")),
            "unchanged"
        );
        assert_eq!(
            post_update_status(&[codex("unknown", "0.147.0")], "codex", Some("0.147.0")),
            "unchanged"
        );
        assert_eq!(
            post_update_status(
                &[cursor("unknown", "2026.08.04-aaa8809")],
                "cursor",
                Some("2026.06.19-653a7fb")
            ),
            "succeeded"
        );
        assert_eq!(
            post_update_status(
                &[cursor("unknown", "2026.08.04-bbbb")],
                "cursor",
                Some("2026.08.04-aaaa")
            ),
            "unchanged"
        );
        assert_eq!(
            post_update_status(&[codex("unknown", "0.146.0")], "codex", Some("0.147.0")),
            "unchanged"
        );
        assert_eq!(
            post_update_status(&[codex("current", "0.147.0")], "codex", Some("0.147.0")),
            "succeeded"
        );
        assert_eq!(
            post_update_status(&[codex("current", "0.147.0")], "codex", None),
            "succeeded"
        );
        assert_eq!(
            post_update_status(
                &[cursor("current", "2026.08.04-bbbb")],
                "cursor",
                Some("2026.08.04-aaaa")
            ),
            "succeeded"
        );
    }

    #[test]
    fn post_update_verification_rejects_semver_downgrades_despite_current_advisory() {
        let downgraded = json!({
            "instanceId": "codex",
            "driver": "codex",
            "version": "0.146.0",
            "versionAdvisory": { "status": "current" }
        });

        assert_eq!(
            post_update_status(&[downgraded], "codex", Some("0.147.0")),
            "unchanged"
        );
    }

    #[test]
    fn post_update_verification_rejects_cursor_downgrades_despite_current_advisory() {
        let downgraded = json!({
            "instanceId": "cursor",
            "driver": "cursor",
            "version": "2026.06.19-653a7fb",
            "versionAdvisory": { "status": "current" }
        });

        assert_eq!(
            post_update_status(&[downgraded], "cursor", Some("2026.08.04-aaa8809")),
            "unchanged"
        );
    }

    #[test]
    fn cursor_update_fixture_uses_official_binary_names() {
        assert_eq!(cursor_update_fixture_executable_name(false), "cursor-agent");
        assert_eq!(
            cursor_update_fixture_executable_name(true),
            "cursor-agent.exe"
        );
    }

    fn cursor_update_fixture_executable_name(windows: bool) -> &'static str {
        if windows {
            "cursor-agent.exe"
        } else {
            "cursor-agent"
        }
    }

    struct CompiledCursorUpdateFixture {
        _root: tempfile::TempDir,
        executable: PathBuf,
    }

    async fn compile_cursor_update_fixture(directory: &Path, version: &str) -> PathBuf {
        let release_directory =
            directory.join(".local/share/cursor-agent/versions/2026.06.19-653a7fb");
        tokio::fs::create_dir_all(&release_directory)
            .await
            .expect("create Cursor release directory");
        tokio::fs::write(release_directory.join("version-state"), version)
            .await
            .expect("write Cursor version state");
        let executable =
            release_directory.join(cursor_update_fixture_executable_name(cfg!(windows)));
        let fixture = compiled_cursor_update_fixture().await;
        crate::test_support::executable_fixture::copy_executable(&fixture.executable, &executable);
        executable
    }

    async fn write_cursor_update_fixture(directory: &Path) -> PathBuf {
        compile_cursor_update_fixture(directory, "9.8.7").await
    }

    static COMPILED_CURSOR_UPDATE_FIXTURE: OnceCell<CompiledCursorUpdateFixture> =
        OnceCell::const_new();

    async fn run_compiled_cursor_update_fixture(
        executable: &Path,
        argument: &str,
    ) -> std::io::Result<std::process::Output> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match tokio::process::Command::new(executable)
                    .arg(argument)
                    .output()
                    .await
                {
                    Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    result => return result,
                }
            }
        })
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "compiled Cursor update fixture did not become executable",
            )
        })?
    }

    async fn compiled_cursor_update_fixture() -> &'static CompiledCursorUpdateFixture {
        COMPILED_CURSOR_UPDATE_FIXTURE
            .get_or_init(|| async {
                let root = tempfile::tempdir().expect("compiled Cursor fixture directory");
                let executable = root
                    .path()
                    .join(cursor_update_fixture_executable_name(cfg!(windows)));
                let source = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/cursor_update_fixture.rs");
                let output = tokio::process::Command::new(
                    std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()),
                )
                .arg("--edition=2024")
                .arg(source)
                .arg("-o")
                .arg(&executable)
                .output()
                .await
                .expect("compile Cursor update fixture");
                assert!(
                    output.status.success(),
                    "compile Cursor update fixture: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                CompiledCursorUpdateFixture {
                    _root: root,
                    executable,
                }
            })
            .await
    }

    #[tokio::test]
    async fn compiled_cursor_update_fixture_reports_and_advances_its_version() {
        let directory = tempfile::tempdir().expect("Cursor fixture directory");
        let executable =
            compile_cursor_update_fixture(directory.path(), "2026.06.19-653a7fb").await;
        let release_directory = executable.parent().expect("Cursor release directory");
        tokio::fs::write(release_directory.join("next-version"), "2026.08.04-aaa8809")
            .await
            .expect("write next Cursor version");

        let before = run_compiled_cursor_update_fixture(&executable, "about")
            .await
            .expect("run Cursor about before update");
        assert_eq!(
            String::from_utf8_lossy(&before.stdout).trim(),
            r#"{"cliVersion":"2026.06.19-653a7fb"}"#
        );
        let update = run_compiled_cursor_update_fixture(&executable, "update")
            .await
            .expect("run Cursor update");
        assert!(update.status.success());
        let after = run_compiled_cursor_update_fixture(&executable, "about")
            .await
            .expect("run Cursor about after update");
        assert_eq!(
            String::from_utf8_lossy(&after.stdout).trim(),
            r#"{"cliVersion":"2026.08.04-aaa8809"}"#
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn compiled_cursor_update_fixture_retries_a_busy_executable() {
        let directory = tempfile::tempdir().expect("Cursor fixture directory");
        let executable =
            compile_cursor_update_fixture(directory.path(), "2026.06.19-653a7fb").await;
        let writable = std::fs::OpenOptions::new()
            .write(true)
            .open(&executable)
            .expect("hold writable Cursor fixture");
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            drop(writable);
        });

        let output = run_compiled_cursor_update_fixture(&executable, "about")
            .await
            .expect("busy Cursor fixture becomes executable");
        assert!(output.status.success());
        release.await.expect("writable fixture release");
    }

    async fn control_with_cursor_update_fixture(executable: PathBuf) -> NativeServerControl {
        control_with_cursor_update_fixture_environment(executable, json!([])).await
    }

    async fn control_with_cursor_update_fixture_environment(
        executable: PathBuf,
        environment: Value,
    ) -> NativeServerControl {
        let directory = executable.parent().expect("fixture directory");
        let state_dir = ServerConfig::new(directory).state_dir();
        hermetic_providers::write_hermetic_settings(
            &state_dir,
            json!({
                "providerInstances": {
                    "cursor-work": {
                        "driver": "cursor",
                        "enabled": true,
                        "config": { "binaryPath": executable },
                        "environment": environment
                    }
                }
            }),
        );
        NativeServerControl::new(ServerConfig::new(directory), json!({})).await
    }

    fn missing_provider_executable(temp: &tempfile::TempDir) -> String {
        temp.path()
            .join("missing-provider-executable")
            .to_string_lossy()
            .into_owned()
    }

    async fn scheduler_control(temp: &tempfile::TempDir) -> NativeServerControl {
        let config = running_test_config(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        let missing_binary = missing_provider_executable(temp);
        let missing_provider = json!({ "binaryPath": missing_binary });
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        tokio::fs::write(
            settings_path,
            serde_json::to_vec(&json!({
                "enableProviderUpdateChecks": false,
                // A settings patch replaces `providerInstances` wholesale, and the
                // inventory then falls back to these legacy entries for every
                // built-in driver left without an instance. Pinning them keeps any
                // patched full probe from launching the host's real provider CLIs,
                // whose load-dependent startup can outlast `PROBE_WAIT_GUARD`.
                "providers": {
                    "codex": missing_provider,
                    "claudeAgent": missing_provider,
                    "cursor": missing_provider,
                    "grok": missing_provider,
                    "opencode": missing_provider
                },
                "providerInstances": {
                    "codex": { "driver": "codex", "enabled": true, "config": { "binaryPath": missing_binary } },
                    "claude": { "driver": "claudeAgent", "enabled": true, "config": { "binaryPath": missing_binary } },
                    "cursor": { "driver": "cursor", "enabled": true, "config": { "binaryPath": missing_binary } },
                    "grok": { "driver": "grok", "enabled": true, "config": { "binaryPath": missing_binary } },
                    "opencode": { "driver": "opencode", "enabled": true, "config": { "binaryPath": missing_binary } }
                }
            }))
            .expect("settings JSON"),
        )
        .await
        .expect("write settings");
        NativeServerControl::new(config, json!({"policy":"test"})).await
    }

    #[test]
    fn capability_cache_skips_partial_catalogs_and_stays_bounded() {
        let key = |index: usize| ("claude".to_owned(), format!("/w/{index}"));
        let mut cache = CapabilityCache::default();
        cache.store(key(0), 1, json!({"skills":[],"issues":["Retry to load"]}));
        assert!(
            cache.cached(&key(0), 1).is_none(),
            "a partial catalog is rediscovered"
        );
        for index in 0..=CAPABILITY_CACHE_ENTRIES {
            cache.store(key(index), 1, json!({"skills":[],"issues":[]}));
        }
        assert_eq!(cache.entries.len(), CAPABILITY_CACHE_ENTRIES);
        assert!(cache.cached(&key(CAPABILITY_CACHE_ENTRIES), 1).is_some());
        assert!(
            cache.cached(&key(0), 2).is_none(),
            "another generation never answers"
        );
    }

    #[tokio::test]
    async fn capability_rpc_answers_from_cache_and_refreshes_stale_entries() {
        let temp = tempfile::tempdir().unwrap();
        let control = scheduler_control(&temp).await;
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let skill = |name: &str| workspace.join(".agents/skills").join(name);
        tokio::fs::create_dir_all(skill("first")).await.unwrap();
        tokio::fs::write(skill("first").join("SKILL.md"), "# Skill")
            .await
            .unwrap();
        tokio::fs::create_dir_all(&home).await.unwrap();
        let mut instances = control.settings.read().await["providerInstances"].clone();
        instances["cursor"]["environment"] = json!([
            {"name":"HOME","value":home},
            {"name":"USERPROFILE","value":home}
        ]);
        control
            .update_settings(json!({"patch":{"providerInstances":instances}}))
            .await
            .unwrap();
        wait_for_full_refresh_idle(&control).await;
        let payload = json!({"instanceId":"cursor","cwd":workspace});
        let names = |control: NativeServerControl| {
            let payload = payload.clone();
            async move {
                let result = control
                    .call(
                        "server.getProviderCapabilities",
                        payload,
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                result["skills"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|skill| skill["name"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(names(control.clone()).await, ["first"]);

        tokio::fs::create_dir_all(skill("second")).await.unwrap();
        tokio::fs::write(skill("second").join("SKILL.md"), "# Skill")
            .await
            .unwrap();
        // A fresh entry answers without discovering again.
        assert_eq!(names(control.clone()).await, ["first"]);

        // A stale entry still answers at once, and its refresh lands for the next call.
        for entry in control
            .capability_cache
            .lock()
            .unwrap()
            .entries
            .values_mut()
        {
            entry.stored_at = std::time::Instant::now()
                .checked_sub(CAPABILITY_CACHE_FRESH * 2)
                .unwrap();
        }
        assert_eq!(names(control.clone()).await, ["first"]);
        tokio::time::timeout(Duration::from_secs(10), async {
            while !control
                .capability_cache
                .lock()
                .unwrap()
                .refreshing
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the background refresh finishes");
        assert_eq!(names(control.clone()).await, ["first", "second"]);

        // Changed provider settings never answer from the previous catalog.
        tokio::fs::remove_dir_all(skill("second")).await.unwrap();
        instances["cursor"]["environment"][0]["value"] = json!(home.join("."));
        control
            .update_settings(json!({"patch":{"providerInstances":instances}}))
            .await
            .unwrap();
        wait_for_full_refresh_idle(&control).await;
        assert_eq!(names(control.clone()).await, ["first"]);
    }

    #[tokio::test]
    async fn capability_rpc_keeps_user_skills_and_isolates_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        let control = scheduler_control(&temp).await;
        let home = temp.path().join("home");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        for (root, name) in [
            (&home, "personal"),
            (&first, "first-repo"),
            (&second, "second-repo"),
        ] {
            let directory = root.join(".agents/skills").join(name);
            tokio::fs::create_dir_all(&directory).await.unwrap();
            tokio::fs::write(directory.join("SKILL.md"), "# Skill")
                .await
                .unwrap();
        }
        let mut instances = control.settings.read().await["providerInstances"].clone();
        instances["cursor"]["environment"] = json!([
            {"name":"HOME","value":home,"sensitive":true},
            {"name":"USERPROFILE","value":home}
        ]);
        instances["disabled"] = json!({"driver":"cursor","enabled":false,"config":{}});
        control
            .update_settings(json!({"patch":{"providerInstances":instances}}))
            .await
            .unwrap();
        wait_for_full_refresh_idle(&control).await;
        let before = control.providers.read().await.clone();
        for (cwd, expected) in [(&first, "first-repo"), (&second, "second-repo")] {
            let result = control
                .call(
                    "server.getProviderCapabilities",
                    json!({"instanceId":"cursor","cwd":cwd}),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let names = result["skills"]
                .as_array()
                .unwrap()
                .iter()
                .map(|skill| skill["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(names, [expected, "personal"]);
            assert_eq!(result["issues"], json!([]));
        }
        assert_eq!(
            *control.providers.read().await,
            before,
            "scoped reads cannot publish global inventory"
        );
        for payload in [
            json!({"instanceId":"cursor","cwd":"relative"}),
            json!({"instanceId":"cursor","cwd":temp.path().join("missing")}),
            json!({"instanceId":"unknown","cwd":first}),
            json!({"instanceId":"disabled","cwd":first}),
            json!({"instanceId":"grok","cwd":first}),
        ] {
            let error = control
                .call(
                    "server.getProviderCapabilities",
                    payload,
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert_eq!(error["_tag"], "ServerProviderCapabilitiesError");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capability_shutdown_reaps_abandoned_requests_and_closes_admission() {
        let temp = tempfile::tempdir().unwrap();
        let control = scheduler_control(&temp).await;
        let sandbox = crate::test_support::TestSandbox::new("capability-shutdown");
        let pid_file = sandbox.path("pid");
        let binary = sandbox.executable_script(
            "slow-provider",
            "printf '%s' \"$$\" > \"$BIBCODE_TEST_PID\"\nexec sleep 60",
            "",
        );
        let mut settings = control.settings.read().await.clone();
        settings["providerInstances"]["codex"]["config"]["binaryPath"] = json!(binary);
        settings["providerInstances"]["codex"]["environment"] = json!([
            {"name":"HOME","value":sandbox.root()},
            {"name":"BIBCODE_TEST_PID","value":pid_file}
        ]);
        tokio::fs::write(
            &control.settings_path,
            serde_json::to_vec(&settings).unwrap(),
        )
        .await
        .unwrap();
        let payload = json!({"instanceId":"codex","cwd":sandbox.root()});
        let request = tokio::spawn(control.call(
            "server.getProviderCapabilities",
            payload.clone(),
            CancellationToken::new(),
        ));
        let pid: libc::pid_t = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(raw) = tokio::fs::read_to_string(&pid_file).await
                    && let Ok(pid) = raw.parse()
                {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("discovery child started");
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        tokio::time::timeout(
            Duration::from_secs(5),
            control.shutdown_capability_discovery(),
        )
        .await
        .expect("shutdown drains discovery cleanup");
        // SAFETY: signal 0 only checks liveness of the fixture PID.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "discovery child was not reaped"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let error = control
            .call(
                "server.getProviderCapabilities",
                payload,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error["_tag"], "ServerProviderCapabilitiesError");
        assert!(error["reason"].as_str().unwrap().contains("shutting down"));
    }

    async fn mutable_provider_registry(
        version: &str,
    ) -> (Url, Arc<tokio::sync::RwLock<String>>, Arc<AtomicUsize>) {
        let version = Arc::new(tokio::sync::RwLock::new(version.to_owned()));
        let requests = Arc::new(AtomicUsize::new(0));
        let state = (version.clone(), requests.clone());
        let app = Router::new()
            .route(
                "/opencode-ai/latest",
                get(
                    |State((version, requests)): State<(
                        Arc<tokio::sync::RwLock<String>>,
                        Arc<AtomicUsize>,
                    )>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        Json(json!({ "version": version.read().await.clone() }))
                    },
                ),
            )
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        (
            Url::parse(&format!("http://{address}/")).expect("registry URL"),
            version,
            requests,
        )
    }

    async fn cursor_installer_registry() -> (Url, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/",
                get(|State(requests): State<Arc<AtomicUsize>>| async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    "DOWNLOAD_URL=\"https://downloads.cursor.com/lab/2026.08.04-aaa8809/${OS}/${ARCH}/agent-cli-package.tar.gz\"\nFINAL_DIR=\"$HOME/.local/share/cursor-agent/versions/2026.08.04-aaa8809\""
                }),
            )
            .with_state(requests.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Cursor installer registry listener");
        let address = listener.local_addr().expect("Cursor installer address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("registry server") });
        (
            Url::parse(&format!("http://{address}/")).expect("Cursor installer URL"),
            requests,
        )
    }

    #[tokio::test]
    async fn provider_update_invalidates_latest_version_only_after_zero_exit() {
        let directory = tempfile::tempdir().expect("state directory");
        let executable =
            compile_cursor_update_fixture(directory.path(), "2026.06.19-653a7fb").await;
        let config = ServerConfig::new(directory.path());
        hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({
                "enableProviderUpdateChecks": true,
                "providerInstances": {"cursor-work": {
                    "driver": "cursor", "enabled": true,
                    "config": {"binaryPath": executable}
                }}
            }),
        );
        let mut control = NativeServerControl::new(config, json!({})).await;
        let (installer_url, requests) = cursor_installer_registry().await;
        control.provider_maintenance =
            ProviderMaintenance::with_cursor_installer_url(installer_url);

        control
            .refresh_providers(&json!({ "instanceId": "cursor-work" }))
            .await;
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        let release_directory = executable.parent().expect("Cursor release directory");
        tokio::fs::write(release_directory.join("update-exit-code"), "7")
            .await
            .expect("write failing update exit code");
        let failed = control
            .update_provider(
                &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                CancellationToken::new(),
            )
            .await
            .expect("non-zero update returns snapshots");
        let failed_provider = failed["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("failed Cursor provider");
        assert_eq!(failed_provider["updateState"]["status"], "failed");
        let (_, settings) = control.settings_snapshot().await;
        let cwd = std::env::current_dir().unwrap_or_else(|_| control.config.base_dir.clone());
        control
            .probe_full_provider_snapshots(&settings, Some("cursor-work"), &cwd)
            .await;
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "non-zero exit must preserve the primed latest-version generation"
        );

        tokio::fs::write(release_directory.join("update-exit-code"), "0")
            .await
            .expect("write successful update exit code");
        let succeeded = control
            .update_provider(
                &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                CancellationToken::new(),
            )
            .await
            .expect("zero-exit update returns snapshots");
        let succeeded_provider = succeeded["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("successful Cursor provider");
        assert_eq!(succeeded_provider["updateState"]["status"], "unchanged");
        assert_eq!(
            succeeded_provider["updateState"]["message"],
            format!(
                "Update command completed, but {} still reports 2026.06.19-653a7fb; expected 2026.08.04-aaa8809.",
                executable.display()
            )
        );
        assert_eq!(
            control
                .provider_update_verifications
                .lock()
                .expect("provider update verification lock")
                .last(),
            Some(&ProviderUpdateVerification {
                provider: "cursor".to_owned(),
                instance_id: "cursor-work".to_owned(),
                before_version: "2026.06.19-653a7fb".to_owned(),
                after_version: "2026.06.19-653a7fb".to_owned(),
                expected_version: "2026.08.04-aaa8809".to_owned(),
                exit_code: 0,
                status: "unchanged".to_owned(),
            })
        );
        assert_eq!(
            requests.load(Ordering::SeqCst),
            2,
            "zero exit must invalidate before post-update verification"
        );
    }

    #[tokio::test]
    async fn unknown_advisory_does_not_treat_stale_published_version_as_update_progress() {
        let directory = tempfile::tempdir().expect("state directory");
        let executable =
            compile_cursor_update_fixture(directory.path(), "2026.08.04-aaa8809").await;
        let control = control_with_cursor_update_fixture(executable).await;

        control
            .refresh_providers(&json!({ "instanceId": "cursor-work" }))
            .await;
        let mut providers = control.providers.write().await;
        let cursor = providers
            .iter_mut()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("Cursor provider");
        cursor["version"] = json!("2026.06.19-653a7fb");
        drop(providers);

        let result = control
            .update_provider(
                &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                CancellationToken::new(),
            )
            .await
            .expect("no-op update returns snapshots");
        let cursor = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("updated Cursor provider");

        assert_eq!(cursor["versionAdvisory"]["status"], "unknown");
        assert_eq!(cursor["version"], "2026.08.04-aaa8809");
        assert_eq!(cursor["updateState"]["status"], "unchanged");
    }

    #[tokio::test]
    async fn failed_exact_target_probe_disables_unknown_advisory_advancement() {
        let directory = tempfile::tempdir().expect("state directory");
        let executable =
            compile_cursor_update_fixture(directory.path(), "2026.08.04-aaa8809").await;
        let control = control_with_cursor_update_fixture(executable.clone()).await;
        control
            .refresh_providers(&json!({ "instanceId": "cursor-work" }))
            .await;

        let release_directory = executable.parent().expect("Cursor release directory");
        tokio::fs::write(
            release_directory.join("failed-version-state"),
            "2026.06.19-653a7fb",
        )
        .await
        .expect("write failed probe version");
        tokio::fs::write(release_directory.join("version-exit-code"), "7")
            .await
            .expect("write version failure");
        tokio::fs::write(release_directory.join("about-exit-code"), "7")
            .await
            .expect("write about failure");
        let (_, settings) = control.settings_snapshot().await;
        let target =
            provider_inventory::maintenance_target(&settings, "cursor", Some("cursor-work"))
                .expect("Cursor maintenance target");
        let cwd = std::env::current_dir().unwrap_or_else(|_| control.config.base_dir.clone());
        let failed_probe =
            provider_inventory::probe_maintenance_target_version(&target, &cwd).await;

        let result = control
            .update_provider(
                &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                CancellationToken::new(),
            )
            .await
            .expect("no-op update returns snapshots");
        let cursor = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("updated Cursor provider");

        assert_eq!(cursor["versionAdvisory"]["status"], "unknown");
        assert_eq!(cursor["version"], "2026.08.04-aaa8809");
        assert_eq!(cursor["updateState"]["status"], "unchanged");
        assert_eq!(failed_probe, None);
    }

    #[tokio::test]
    async fn manual_provider_refresh_requires_a_new_registry_result() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut control = scheduler_control(&temp).await;
        let (registry_url, registry_version, requests) = mutable_provider_registry("1.18.11").await;
        let maintenance = ProviderMaintenance::with_registry_base_url(registry_url);
        control.provider_maintenance = maintenance.clone();
        let npm_bin = temp.path().join(".npm-global/bin");
        std::fs::create_dir_all(&npm_bin).expect("global npm bin");
        let binary = npm_bin.join(if cfg!(windows) {
            "opencode.cmd"
        } else {
            "opencode"
        });
        std::fs::write(&binary, b"fixture").expect("global npm executable");
        let target = ProviderMaintenanceTarget {
            instance_id: "opencode".to_owned(),
            driver: "opencode".to_owned(),
            binary_path: binary.to_string_lossy().into_owned(),
            environment: Vec::new(),
        };
        let mut startup = json!({
            "instanceId": "opencode",
            "driver": "opencode",
            "enabled": true,
            "installed": true,
            "version": "1.18.11",
            "checkedAt": "2026-08-01T12:00:00Z"
        });
        maintenance
            .enrich_snapshot(&target, &mut startup, true)
            .await;
        *registry_version.write().await = "1.18.15".to_owned();

        control.refresh_providers(&json!({})).await;

        let mut after_manual_refresh = startup;
        after_manual_refresh["checkedAt"] = json!("2026-08-01T12:05:00Z");
        maintenance
            .enrich_snapshot(&target, &mut after_manual_refresh, true)
            .await;
        assert_eq!(
            after_manual_refresh["versionAdvisory"]["latestVersion"],
            "1.18.15"
        );
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    /// A hang guard, not a timing assertion: a loaded host can stretch one full
    /// provider probe well past a few seconds.
    const PROBE_WAIT_GUARD: Duration = Duration::from_secs(30);

    async fn wait_for_probe_after(control: &NativeServerControl, previous: u64) -> u64 {
        tokio::time::timeout(PROBE_WAIT_GUARD, async {
            loop {
                let current = control.next_provider_probe_sequence.load(Ordering::Acquire);
                if current > previous {
                    return current;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider check did not start")
    }

    async fn wait_for_full_refresh_idle(control: &NativeServerControl) {
        tokio::time::timeout(PROBE_WAIT_GUARD, async {
            loop {
                if !control
                    .full_provider_refresh_running
                    .load(Ordering::Acquire)
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider check did not finish");
    }

    async fn wait_for_scheduler_request_after(control: &NativeServerControl, previous: u64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if control
                    .provider_update_refresh_attempts
                    .load(Ordering::Acquire)
                    > previous
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider update check did not request a refresh");
    }

    #[tokio::test(start_paused = true)]
    async fn server_config_subscriptions_refresh_only_after_completion_ttl() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let cancellation = CancellationToken::new();
        let first_probe = control.install_next_full_provider_probe_pause().await;
        let mut first = control.subscribe("subscribeServerConfig", cancellation.clone());
        assert_eq!(
            first
                .recv()
                .await
                .expect("config stream")
                .expect("snapshot")[0]["type"],
            "snapshot"
        );
        first_probe.wait_until_entered().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            1
        );

        // Even a long-running initial refresh is shared by reconnecting clients.
        tokio::time::advance(Duration::from_secs(601)).await;
        let mut during_refresh = control.subscribe("subscribeServerConfig", cancellation.clone());
        during_refresh
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            1
        );
        first_probe.release();
        wait_for_full_refresh_idle(&control).await;

        let next_probe = control.install_next_full_provider_probe_pause().await;
        for elapsed in [Duration::from_secs(299), Duration::from_secs(1)] {
            tokio::time::advance(elapsed).await;
            let mut reconnect = control.subscribe("subscribeServerConfig", cancellation.clone());
            reconnect
                .recv()
                .await
                .expect("config stream")
                .expect("snapshot");
            tokio::task::yield_now().await;
            assert_eq!(
                control.next_provider_probe_sequence.load(Ordering::Acquire),
                1,
                "a subscription within five minutes of completion must reuse the inventory"
            );
        }

        tokio::time::advance(Duration::from_nanos(1)).await;
        let mut expired = control.subscribe("subscribeServerConfig", cancellation.clone());
        expired
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        next_probe.wait_until_entered().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            2
        );
        next_probe.release();
        wait_for_full_refresh_idle(&control).await;
        cancellation.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn server_config_subscriptions_receive_manual_refresh_after_rpc_disconnect() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let manual_probe = control.install_next_full_provider_probe_pause().await;
        let manual_control = control.clone();
        let manual = tokio::spawn(async move {
            manual_control
                .call(
                    "server.refreshProviders",
                    json!({}),
                    CancellationToken::new(),
                )
                .await
        });
        manual_probe.wait_until_entered().await;

        let cancellation = CancellationToken::new();
        let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
        stream
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            1,
            "a subscription must not duplicate an in-flight manual full refresh"
        );

        manual.abort();
        assert!(
            manual
                .await
                .expect_err("manual refresh cancelled")
                .is_cancelled()
        );
        manual_probe.release();
        let event = tokio::time::timeout(PROBE_WAIT_GUARD, stream.recv())
            .await
            .expect("suppressed subscriber receives the detached manual refresh")
            .expect("config stream")
            .expect("provider statuses");
        assert_eq!(event[0]["type"], "providerStatuses");
        assert_eq!(
            event[0]["payload"]["providers"],
            control.config_snapshot().await["providers"]
        );

        let mut retry = control.subscribe("subscribeServerConfig", cancellation.clone());
        retry
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            1,
            "the detached manual refresh records completion and renews the TTL"
        );
        cancellation.cancel();
    }

    fn replacement_provider_instances() -> Value {
        json!({ "replacement": { "driver": "codex", "enabled": false, "config": {} } })
    }

    #[tokio::test]
    async fn scheduler_control_providers_never_fall_back_to_host_executables() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let (_, mut settings) = control.settings_snapshot().await;
        apply_settings_patch(
            &mut settings,
            json!({ "providerInstances": replacement_provider_instances() }),
        );
        apply_settings_defaults(&mut settings);

        let missing_binary = missing_provider_executable(&temp);
        let host_fallbacks = ["codex", "claudeAgent", "cursor", "grok", "opencode"]
            .into_iter()
            .filter_map(|driver| {
                let target = provider_inventory::maintenance_target(&settings, driver, None)
                    .unwrap_or_else(|| panic!("{driver} has a provider definition"));
                (target.binary_path != missing_binary)
                    .then(|| format!("{driver} -> {}", target.binary_path))
            })
            .collect::<Vec<_>>();
        assert!(
            host_fallbacks.is_empty(),
            "providers must not resolve CLIs from the host PATH: {host_fallbacks:?}"
        );
    }

    async fn assert_stale_manual_refresh_is_replaced_by_settings(disconnect_settings_rpc: bool) {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let manual_probe = control.install_next_full_provider_probe_pause().await;
        let manual_control = control.clone();
        let manual = tokio::spawn(async move {
            manual_control
                .call(
                    "server.refreshProviders",
                    json!({}),
                    CancellationToken::new(),
                )
                .await
        });
        manual_probe.wait_until_entered().await;
        let cancellation = CancellationToken::new();
        let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
        stream
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            1
        );

        let mut events = control.config_events.subscribe();
        let quick_probe = control.install_next_quick_provider_probe_pause().await;
        let full_probe = control.install_next_full_provider_probe_pause().await;
        let settings_control = control.clone();
        let settings_update = tokio::spawn(async move {
            settings_control
                .call(
                    "server.updateSettings",
                    json!({ "patch": { "providerInstances": replacement_provider_instances() } }),
                    CancellationToken::new(),
                )
                .await
        });
        quick_probe.wait_until_entered().await;
        if disconnect_settings_rpc {
            settings_update.abort();
        }
        manual_probe.release();
        manual
            .await
            .expect("manual refresh joins")
            .expect("stale refresh returns current inventory");
        assert_eq!(
            events.try_recv().expect("committed settings")["type"],
            "settingsUpdated"
        );
        assert!(
            matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "the stale manual result must not publish after settings change"
        );

        quick_probe.release();
        if disconnect_settings_rpc {
            assert!(
                settings_update
                    .await
                    .expect_err("settings RPC disconnected")
                    .is_cancelled()
            );
        } else {
            settings_update
                .await
                .expect("settings RPC joins")
                .expect("settings update");
        }
        tokio::time::timeout(PROBE_WAIT_GUARD, full_probe.wait_until_entered())
            .await
            .expect("committed settings must start a replacement full refresh");
        full_probe.release();
        wait_for_full_refresh_idle(&control).await;
        let event = tokio::time::timeout(PROBE_WAIT_GUARD, async {
            loop {
                let batch = stream
                    .recv()
                    .await
                    .expect("config stream")
                    .expect("config event");
                if let Some(event) = batch
                    .into_iter()
                    .find(|event| event["type"] == "providerStatuses")
                {
                    break event;
                }
            }
        })
        .await
        .expect("suppressed subscriber receives current settings inventory");
        assert!(
            event["payload"]["providers"]
                .as_array()
                .expect("providers")
                .iter()
                .any(|provider| provider["instanceId"] == "replacement")
        );
        assert_eq!(
            control
                .latest_full_provider_refresh_generation
                .load(Ordering::Acquire),
            control.settings_generation.load(Ordering::Acquire)
        );
        let mut reconnect = control.subscribe("subscribeServerConfig", cancellation.clone());
        reconnect
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            3,
            "replacement refresh records completion and renews the TTL"
        );
        cancellation.cancel();
    }

    #[tokio::test]
    async fn stale_manual_provider_refresh_is_replaced_by_settings_refresh() {
        assert_stale_manual_refresh_is_replaced_by_settings(false).await;
    }

    #[tokio::test]
    async fn stale_manual_provider_refresh_is_replaced_after_settings_rpc_disconnect() {
        assert_stale_manual_refresh_is_replaced_by_settings(true).await;
    }

    async fn assert_explicit_provider_refresh_publishes_unchanged_inventory(payload: Value) {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        control.refresh_providers(&json!({})).await;
        let old_checked_at = json!("2026-01-01T00:00:00Z");
        let previous = {
            let mut providers = control.providers.write().await;
            for provider in providers.iter_mut() {
                provider["checkedAt"] = old_checked_at.clone();
            }
            providers.clone()
        };
        let instance_id = payload.get("instanceId").and_then(Value::as_str);
        let mut events = control.config_events.subscribe();

        let refreshed = control
            .call(
                "server.refreshProviders",
                payload.clone(),
                CancellationToken::new(),
            )
            .await
            .expect("explicit provider refresh");
        let providers = refreshed["providers"].as_array().expect("providers");
        assert_eq!(providers.len(), previous.len());
        for (previous, next) in previous.iter().zip(providers) {
            let mut expected = previous.clone();
            if instance_id.is_none_or(|id| next["instanceId"] == id) {
                assert_ne!(next["checkedAt"], old_checked_at);
                expected["checkedAt"] = next["checkedAt"].clone();
            }
            assert_eq!(*next, expected, "only probe timestamps should change");
        }
        let event = events
            .try_recv()
            .expect("explicit refresh must publish even when only checkedAt changed");
        assert_eq!(event["type"], "providerStatuses");
        assert_eq!(event["payload"], refreshed);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn explicit_provider_refresh_publishes_unchanged_full_inventory_once() {
        assert_explicit_provider_refresh_publishes_unchanged_inventory(json!({})).await;
    }

    #[tokio::test]
    async fn explicit_provider_refresh_publishes_unchanged_instance_once() {
        assert_explicit_provider_refresh_publishes_unchanged_inventory(
            json!({ "instanceId": "codex" }),
        )
        .await;
    }

    async fn assert_overtaken_explicit_provider_refresh_publishes_current_inventory(
        payload: Value,
    ) {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        control.refresh_providers(&json!({})).await;
        let old_checked_at = json!("2026-01-01T00:00:00Z");
        for provider in control.providers.write().await.iter_mut() {
            provider["checkedAt"] = old_checked_at.clone();
        }
        let mut events = control.config_events.subscribe();
        let pause = control.install_next_full_provider_probe_pause().await;
        let manual_control = control.clone();
        let manual = tokio::spawn(async move {
            manual_control
                .call("server.refreshProviders", payload, CancellationToken::new())
                .await
        });
        pause.wait_until_entered().await;
        let manual_sequence = control.next_provider_probe_sequence.load(Ordering::Acquire);
        let checks = control.start_provider_update_checks();
        wait_for_probe_after(&control, manual_sequence).await;
        wait_for_full_refresh_idle(&control).await;
        checks.shutdown().await;
        let current = control.config_snapshot().await["providers"].clone();
        assert!(
            current
                .as_array()
                .expect("providers")
                .iter()
                .all(|provider| provider["checkedAt"] != old_checked_at)
        );
        assert!(
            matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "automatic timestamp-only refresh must remain silent"
        );

        // The newer full probe owns the TTL; acknowledging this older manual
        // result must neither overwrite that inventory nor renew its completion.
        tokio::time::advance(Duration::from_secs(240)).await;
        pause.release();
        let result = manual
            .await
            .expect("manual refresh joins")
            .expect("manual refresh");
        assert_eq!(result["providers"], current);
        let event = events
            .try_recv()
            .expect("overtaken explicit refresh must publish the current inventory");
        assert_eq!(event["type"], "providerStatuses");
        assert_eq!(event["payload"], result);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        tokio::time::advance(Duration::from_secs(61)).await;
        let next_probe = control.install_next_full_provider_probe_pause().await;
        let cancellation = CancellationToken::new();
        let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
        stream
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        tokio::time::timeout(PROBE_WAIT_GUARD, next_probe.wait_until_entered())
            .await
            .expect("a discarded manual probe must not renew the TTL");
        next_probe.release();
        wait_for_full_refresh_idle(&control).await;
        cancellation.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn overtaken_explicit_provider_refresh_publishes_current_full_inventory_once() {
        assert_overtaken_explicit_provider_refresh_publishes_current_inventory(json!({})).await;
    }

    #[tokio::test(start_paused = true)]
    async fn overtaken_explicit_provider_refresh_publishes_current_instance_once() {
        assert_overtaken_explicit_provider_refresh_publishes_current_inventory(
            json!({ "instanceId": "codex" }),
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn manual_provider_refresh_bypasses_and_renews_subscription_ttl() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let cancellation = CancellationToken::new();
        control
            .call("server.refreshProviders", json!({}), cancellation.clone())
            .await
            .expect("initial manual refresh");

        tokio::time::advance(Duration::from_secs(240)).await;
        let refreshed = control
            .call("server.refreshProviders", json!({}), cancellation.clone())
            .await
            .expect("manual refresh within the TTL");
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            2
        );
        assert_eq!(
            refreshed["providers"],
            control.config_snapshot().await["providers"]
        );

        // The first refresh is expired, but the manual refresh is still fresh.
        tokio::time::advance(Duration::from_secs(61)).await;
        let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
        stream
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot");
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            2,
            "a completed manual refresh must renew the subscription TTL"
        );
        cancellation.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn settings_changes_refresh_providers_within_subscription_ttl() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        control.refresh_providers(&json!({})).await;
        let full_probe = control.install_next_full_provider_probe_pause().await;

        control
            .call(
                "server.updateSettings",
                json!({ "patch": { "enableAssistantStreaming": true } }),
                CancellationToken::new(),
            )
            .await
            .expect("settings update");
        full_probe.wait_until_entered().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            3
        );
        full_probe.release();
        wait_for_full_refresh_idle(&control).await;
    }

    #[tokio::test(start_paused = true)]
    async fn provider_update_checks_run_immediately_and_every_hour() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let before = control.next_provider_probe_sequence.load(Ordering::Acquire);
        let checks = control.start_provider_update_checks();

        let after_startup = wait_for_probe_after(&control, before).await;
        wait_for_full_refresh_idle(&control).await;

        tokio::time::advance(Duration::from_secs(60 * 60 - 1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            after_startup
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        wait_for_probe_after(&control, after_startup).await;
        checks.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn provider_update_check_shutdown_prevents_future_ticks() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let checks = control.start_provider_update_checks();
        let before = control.next_provider_probe_sequence.load(Ordering::Acquire);
        wait_for_probe_after(&control, before).await;
        checks.shutdown().await;
        let stopped_at = control.next_provider_probe_sequence.load(Ordering::Acquire);
        tokio::time::advance(Duration::from_secs(2 * 60 * 60)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            stopped_at
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_provider_update_checks_prevents_future_ticks() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let checks = control.start_provider_update_checks();
        let before = control.next_provider_probe_sequence.load(Ordering::Acquire);
        wait_for_probe_after(&control, before).await;
        wait_for_full_refresh_idle(&control).await;
        drop(checks);
        let stopped_at = control.next_provider_probe_sequence.load(Ordering::Acquire);

        tokio::time::advance(Duration::from_secs(2 * 60 * 60)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            stopped_at
        );
    }

    #[tokio::test(start_paused = true)]
    async fn provider_update_check_shutdown_cancels_a_paused_full_refresh() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let pause = control.install_next_full_provider_probe_pause().await;
        let checks = control.start_provider_update_checks();
        pause.wait_until_entered().await;

        checks.shutdown().await;

        assert!(
            !control
                .full_provider_refresh_running
                .load(Ordering::Acquire),
            "scheduled full refresh must finish before shutdown returns"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn provider_update_checks_coalesce_while_a_full_refresh_is_running() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let pause = control.install_next_full_provider_probe_pause().await;
        let checks = control.start_provider_update_checks();
        pause.wait_until_entered().await;
        let running_sequence = control.next_provider_probe_sequence.load(Ordering::Acquire);
        let requested = control
            .provider_update_refresh_attempts
            .load(Ordering::Acquire);

        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        wait_for_scheduler_request_after(&control, requested).await;
        assert_eq!(
            control.next_provider_probe_sequence.load(Ordering::Acquire),
            running_sequence,
        );

        pause.release();
        checks.shutdown().await;
    }

    #[tokio::test]
    async fn full_refresh_handoff_covers_settings_changed_after_its_final_snapshot() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let pause = control
            .install_next_full_provider_refresh_handoff_pause()
            .await;
        let (generation, settings) = control.settings_snapshot().await;
        let cwd = std::env::current_dir().unwrap_or_else(|_| control.config.base_dir.clone());
        let _ = control.start_full_provider_refresh(
            generation,
            settings,
            cwd,
            FullProviderRefreshTrigger::SettingsChange,
        );
        pause.wait_until_entered().await;

        control
            .update_settings(json!({
                "patch": { "providers": { "codex": { "enabled": false } } }
            }))
            .await
            .expect("settings update succeeds");
        let expected_generation = control.settings_generation.load(Ordering::Acquire);
        pause.release();
        wait_for_full_refresh_idle(&control).await;

        assert_eq!(
            control
                .latest_full_provider_refresh_generation
                .load(Ordering::Acquire),
            expected_generation,
        );
    }

    #[tokio::test]
    async fn removed_provider_update_state_is_not_retained_or_reused() {
        let directory = tempfile::tempdir().expect("state directory");
        let executable = write_cursor_update_fixture(directory.path()).await;
        let control = control_with_cursor_update_fixture(executable.clone()).await;
        let pause = control.install_next_full_provider_probe_pause().await;
        let updating = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        pause.wait_until_entered().await;

        control
            .update_settings(json!({
                "patch": {
                    "providerInstances": {
                        "replacement": {
                            "driver": "grok",
                            "enabled": false,
                            "config": {}
                        }
                    }
                }
            }))
            .await
            .expect("settings replace provider");
        assert!(
            control
                .providers
                .read()
                .await
                .iter()
                .all(|provider| provider["instanceId"] != "cursor-work")
        );

        let mut removed_snapshot = json!({
            "instanceId": "cursor-work",
            "driver": "cursor",
        });
        control
            .provider_maintenance
            .overlay_update_state(&mut removed_snapshot);
        assert!(removed_snapshot.get("updateState").is_none());

        control
            .update_settings(json!({
                "patch": {
                    "providerInstances": {
                        "cursor-work": {
                            "driver": "cursor",
                            "enabled": true,
                            "config": { "binaryPath": executable }
                        }
                    }
                }
            }))
            .await
            .expect("re-add cursor provider");
        let refreshed = control
            .refresh_providers(&json!({ "instanceId": "cursor-work" }))
            .await;
        let readded = refreshed["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("re-added cursor provider");
        assert!(readded.get("updateState").is_none());

        pause.release();
        let result = updating
            .await
            .expect("provider update joins")
            .expect("provider update returns snapshots");
        let readded = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("re-added cursor provider");
        assert!(readded.get("updateState").is_none());
        let providers = control.providers.read().await;
        let readded = providers
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("stored re-added cursor provider");
        assert!(readded.get("updateState").is_none());
    }

    #[tokio::test]
    async fn unrelated_settings_commit_after_capability_resolution_uses_current_generation() {
        let directory = tempfile::tempdir().expect("Cursor fixture root");
        let executable = compile_cursor_update_fixture(directory.path(), "2026.01.01-old").await;
        let release_directory = executable
            .parent()
            .expect("Cursor release directory")
            .to_path_buf();
        tokio::fs::write(release_directory.join("next-version"), "2026.02.02-new")
            .await
            .expect("next Cursor version");
        let control = control_with_cursor_update_fixture(executable).await;
        let pause = control.install_next_provider_capabilities_pause().await;
        let updating = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        pause.wait_until_entered().await;

        control
            .update_settings(json!({ "patch": { "enableAssistantStreaming": true } }))
            .await
            .expect("commit unrelated settings change");
        pause.release();

        let result = updating
            .await
            .expect("provider update joins")
            .expect("unchanged target remains reservable");
        assert!(release_directory.join("update-invoked").is_file());
        assert_eq!(
            tokio::fs::read_to_string(release_directory.join("version-state"))
                .await
                .expect("updated Cursor version"),
            "2026.02.02-new"
        );
        let provider = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("Cursor provider");
        assert!(
            matches!(
                provider["updateState"]["status"].as_str(),
                Some("succeeded" | "unchanged")
            ),
            "a completed update must not be reported as failed"
        );
    }

    #[tokio::test]
    async fn environment_change_after_capability_resolution_fails_reservation_closed() {
        let directory = tempfile::tempdir().expect("Cursor fixture root");
        let executable = compile_cursor_update_fixture(directory.path(), "2026.01.01-old").await;
        let release_directory = executable.parent().expect("Cursor release directory");
        let control = control_with_cursor_update_fixture(executable.clone()).await;
        let pause = control.install_next_provider_capabilities_pause().await;
        let updating = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        pause.wait_until_entered().await;

        control
            .update_settings(json!({
                "patch": {
                    "providerInstances": {
                        "cursor-work": {
                            "driver": "cursor",
                            "enabled": true,
                            "config": { "binaryPath": executable },
                            "environment": [
                                { "name": "BIBCODE_RESERVATION_RACE", "value": "changed" }
                            ]
                        }
                    }
                }
            }))
            .await
            .expect("change provider environment");
        pause.release();

        let error = updating
            .await
            .expect("provider update joins")
            .expect_err("changed target must fail before reservation");
        assert_eq!(error["_tag"], "ServerProviderUpdateError");
        assert!(!release_directory.join("update-invoked").exists());
    }

    #[tokio::test]
    async fn queued_update_revalidates_binary_and_environment_before_execution() {
        let old_root = tempfile::tempdir().expect("old Cursor fixture root");
        let new_root = tempfile::tempdir().expect("new Cursor fixture root");
        let old_executable = compile_cursor_update_fixture(old_root.path(), "2026.01.01-old").await;
        let new_executable = compile_cursor_update_fixture(new_root.path(), "2026.02.02-new").await;
        tokio::fs::write(
            old_executable
                .parent()
                .expect("old release directory")
                .join("next-version"),
            "2026.03.03-stale-command-ran",
        )
        .await
        .expect("old next version");
        let control = control_with_cursor_update_fixture(old_executable.clone()).await;
        let command_lock = control.provider_maintenance.command_lock("cursor-agent");
        let blocker = command_lock.lock_owned().await;
        let mut events = control.config_events.subscribe();
        let updating = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let event = events.recv().await.expect("queued update event");
                if event["type"] == "providerStatuses"
                    && event["payload"]["providers"]
                        .as_array()
                        .is_some_and(|providers| {
                            providers.iter().any(|provider| {
                                provider["instanceId"] == "cursor-work"
                                    && provider["updateState"]["status"] == "queued"
                            })
                        })
                {
                    break;
                }
            }
        })
        .await
        .expect("queued state published");

        control
            .update_settings(json!({
                "patch": {
                    "providerInstances": {
                        "cursor-work": {
                            "driver": "cursor",
                            "enabled": true,
                            "config": { "binaryPath": new_executable }
                        }
                    }
                }
            }))
            .await
            .expect("replace queued provider target");
        drop(blocker);

        let result = updating
            .await
            .expect("queued update joins")
            .expect("queued update returns snapshots");
        assert_eq!(
            tokio::fs::read_to_string(
                old_executable
                    .parent()
                    .expect("old release directory")
                    .join("version-state")
            )
            .await
            .expect("old version state"),
            "2026.01.01-old"
        );
        assert_eq!(
            tokio::fs::read_to_string(
                new_executable
                    .parent()
                    .expect("new release directory")
                    .join("version-state")
            )
            .await
            .expect("new version state"),
            "2026.02.02-new"
        );
        let provider = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("reconfigured Cursor provider");
        assert!(provider.get("updateState").is_none());
    }

    #[tokio::test]
    async fn settings_change_during_fresh_preprobe_prevents_stale_command_execution() {
        let old_root = tempfile::tempdir().expect("old Cursor fixture root");
        let new_root = tempfile::tempdir().expect("new Cursor fixture root");
        let old_executable = compile_cursor_update_fixture(old_root.path(), "2026.01.01-old").await;
        let new_executable = compile_cursor_update_fixture(new_root.path(), "2026.02.02-new").await;
        let old_release = old_executable.parent().expect("old release directory");
        let control = control_with_cursor_update_fixture(old_executable.clone()).await;
        let pre_run_pause = control
            .install_next_provider_update_after_version_probe_pause()
            .await;
        let mut updating = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::select! {
            () = pre_run_pause.wait_until_entered() => {}
            result = &mut updating => {
                panic!("provider update completed before the fresh pre-probe entered: {result:?}")
            }
        }

        control
            .update_settings(json!({
                "patch": {
                    "providerInstances": {
                        "cursor-work": {
                            "driver": "cursor",
                            "enabled": true,
                            "config": { "binaryPath": new_executable }
                        }
                    }
                }
            }))
            .await
            .expect("replace target during pre-probe");
        pre_run_pause.release();

        let result = updating
            .await
            .expect("provider update joins")
            .expect("stale provider update returns snapshots");

        assert!(
            !old_release.join("update-invoked").exists(),
            "the old update command must not run after the target changes"
        );
        let provider = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("replacement Cursor provider");
        assert!(provider.get("updateState").is_none());
    }

    #[tokio::test]
    async fn absent_target_verification_removes_update_state_while_settings_refresh_is_paused() {
        let directory = tempfile::tempdir().expect("state directory");
        let control =
            control_with_cursor_update_fixture(write_cursor_update_fixture(directory.path()).await)
                .await;
        let mut events = control.config_events.subscribe();
        let running_pause = control.install_next_provider_update_running_pause().await;
        let mut updating = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        CancellationToken::new(),
                    )
                    .await
            })
        };
        tokio::select! {
            () = running_pause.wait_until_entered() => {}
            result = &mut updating => {
                panic!("provider update completed before publishing its running state: {result:?}")
            }
        }
        let running_published = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            event["type"] == "providerStatuses"
                && event["payload"]["providers"]
                    .as_array()
                    .is_some_and(|providers| {
                        providers.iter().any(|provider| {
                            provider["instanceId"] == "cursor-work"
                                && provider["updateState"]["status"] == "running"
                        })
                    })
        });
        assert!(
            running_published,
            "running state was not published before the barrier"
        );

        let quick_pause = control.install_next_quick_provider_probe_pause().await;
        let settings_update = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_settings(json!({
                        "patch": {
                            "providerInstances": {
                                "replacement": {
                                    "driver": "grok",
                                    "enabled": false,
                                    "config": {}
                                }
                            }
                        }
                    }))
                    .await
            })
        };
        quick_pause.wait_until_entered().await;
        running_pause.release();

        let result = updating
            .await
            .expect("provider update joins")
            .expect("provider update returns snapshots");
        quick_pause.release();
        settings_update
            .await
            .expect("settings update joins")
            .expect("settings update succeeds");
        let provider = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("cached cursor provider");
        assert!(provider.get("updateState").is_none());
    }

    #[tokio::test]
    async fn cancelled_provider_update_publishes_failed_state() {
        let directory = tempfile::tempdir().expect("state directory");
        let control =
            control_with_cursor_update_fixture(write_cursor_update_fixture(directory.path()).await)
                .await;
        let mut events = control.config_events.subscribe();
        let cancellation = CancellationToken::new();
        let running_pause = control.install_next_provider_update_running_pause().await;
        let mut updating = {
            let control = control.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                control
                    .update_provider(
                        &json!({ "provider": "cursor", "instanceId": "cursor-work" }),
                        cancellation,
                    )
                    .await
            })
        };
        tokio::select! {
            () = running_pause.wait_until_entered() => {}
            result = &mut updating => {
                panic!("provider update completed before publishing its running state: {result:?}")
            }
        }
        let running_published = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            event["type"] == "providerStatuses"
                && event["payload"]["providers"]
                    .as_array()
                    .is_some_and(|providers| {
                        providers.iter().any(|provider| {
                            provider["instanceId"] == "cursor-work"
                                && provider["updateState"]["status"] == "running"
                        })
                    })
        });
        assert!(
            running_published,
            "running state was not published before the barrier"
        );
        cancellation.cancel();
        running_pause.release();

        let result = updating
            .await
            .expect("provider update joins")
            .expect("cancelled update returns snapshots");
        let provider = result["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["instanceId"] == "cursor-work")
            .expect("cursor provider");
        assert_eq!(provider["updateState"]["status"], "failed");
        assert!(provider["updateState"]["finishedAt"].is_string());
    }

    #[derive(Clone)]
    struct TestAgentActivityHandler {
        calls: Arc<StdMutex<Vec<(AgentActivitySource, bool, u64)>>>,
    }

    impl AgentActivitySettingsHandler for TestAgentActivityHandler {
        fn transition(
            &self,
            source: AgentActivitySource,
            enabled: bool,
            settings_generation: u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = AgentActivityTransitionReport> + Send + '_>,
        > {
            Box::pin(async move {
                self.calls.lock().expect("handler calls").push((
                    source,
                    enabled,
                    settings_generation,
                ));
                AgentActivityTransitionReport {
                    enabled,
                    settings_generation,
                    ..AgentActivityTransitionReport::default()
                }
            })
        }
    }

    #[derive(Clone)]
    struct ControllerAgentActivityHandler {
        chat: AgentActivityController,
        terminal: AgentActivityController,
        calls: Arc<StdMutex<Vec<(AgentActivitySource, bool, u64)>>>,
    }

    impl AgentActivitySettingsHandler for ControllerAgentActivityHandler {
        fn transition(
            &self,
            source: AgentActivitySource,
            enabled: bool,
            settings_generation: u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = AgentActivityTransitionReport> + Send + '_>,
        > {
            Box::pin(async move {
                self.calls.lock().expect("handler calls").push((
                    source,
                    enabled,
                    settings_generation,
                ));
                let controller = match source {
                    AgentActivitySource::Chat => &self.chat,
                    AgentActivitySource::Terminal => &self.terminal,
                };
                let state = if enabled {
                    controller.enable()
                } else {
                    controller.disable().await.state
                };
                AgentActivityTransitionReport {
                    enabled: state.enabled,
                    settings_generation,
                    observation_generation: state.generation,
                    ..AgentActivityTransitionReport::default()
                }
            })
        }
    }

    #[derive(Clone)]
    struct BlockingAgentActivityHandler {
        calls: Arc<StdMutex<Vec<(AgentActivitySource, bool, u64)>>>,
        first_transition_entered: Arc<Notify>,
        release_first_transition: Arc<Notify>,
    }

    impl AgentActivitySettingsHandler for BlockingAgentActivityHandler {
        fn transition(
            &self,
            source: AgentActivitySource,
            enabled: bool,
            settings_generation: u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = AgentActivityTransitionReport> + Send + '_>,
        > {
            Box::pin(async move {
                let is_first = {
                    let mut calls = self.calls.lock().expect("handler calls");
                    calls.push((source, enabled, settings_generation));
                    calls.len() == 1
                };
                if is_first {
                    self.first_transition_entered.notify_one();
                    self.release_first_transition.notified().await;
                }
                AgentActivityTransitionReport {
                    enabled,
                    settings_generation,
                    ..AgentActivityTransitionReport::default()
                }
            })
        }
    }

    #[test]
    fn native_settings_default_grok_to_disabled() {
        let mut settings = json!({});
        apply_settings_defaults(&mut settings);

        assert_eq!(settings["providers"]["grok"]["enabled"], false);
    }

    #[test]
    fn native_settings_default_provider_update_checks_to_enabled() {
        let mut settings = json!({});
        apply_settings_defaults(&mut settings);

        assert_eq!(settings["enableProviderUpdateChecks"], true);
    }

    #[tokio::test]
    async fn agent_activity_setting_defaults_publishes_persists_and_rejects_strings() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;

        assert_eq!(
            control.settings.read().await["enableChatAgentActivity"],
            true
        );
        let mut events = control.config_events.subscribe();

        let updated = control
            .update_settings(json!({"patch":{"enableChatAgentActivity":false}}))
            .await
            .expect("disable agent activity");
        assert_eq!(updated["enableChatAgentActivity"], false);
        assert_eq!(
            control.settings.read().await["enableChatAgentActivity"],
            false
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("settings update publishes an event")
            .expect("settings update event");
        assert_eq!(event["type"], "settingsUpdated");
        assert_eq!(
            event["payload"]["settings"]["enableChatAgentActivity"],
            false
        );

        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(settings_path)
                .await
                .expect("persisted settings"),
        )
        .expect("valid JSON");
        assert_eq!(persisted["enableChatAgentActivity"], false);

        assert!(
            control
                .update_settings(json!({"patch":{"enableChatAgentActivity":"false"}}))
                .await
                .is_err()
        );
    }

    #[test]
    fn profiler_setting_must_be_a_boolean() {
        assert!(validate_settings_document(&json!({"enableProfiler": true})).is_ok());
        assert!(validate_settings_document(&json!({})).is_ok());
        assert!(validate_settings_document(&json!({"enableProfiler": "true"})).is_err());
    }

    #[tokio::test]
    async fn agent_activity_legacy_key_is_removed_on_the_next_persisted_settings_write() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({"enableAgentActivity": false}),
        );

        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        assert_eq!(
            control.settings.read().await["enableChatAgentActivity"],
            false
        );
        assert_eq!(
            control.settings.read().await["enableTerminalAgentActivity"],
            false
        );

        control
            .update_settings(json!({"patch":{"terminalDefaultShell":"/bin/test-shell"}}))
            .await
            .expect("persist migrated settings");
        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(settings_path)
                .await
                .expect("persisted settings"),
        )
        .expect("valid JSON");
        assert!(persisted.get("enableAgentActivity").is_none());
        assert_eq!(persisted["enableChatAgentActivity"], false);
        assert_eq!(persisted["enableTerminalAgentActivity"], false);
    }

    #[tokio::test]
    async fn agent_activity_malformed_legacy_value_is_rejected_before_normalization() {
        #[cfg(feature = "hermetic-test-guard")]
        if crate::test_support::reexec_without_provider_path(
            "production::control::tests::agent_activity_malformed_legacy_value_is_rejected_before_normalization",
        ) {
            return;
        }
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory");
        tokio::fs::write(&settings_path, br#"{"enableAgentActivity":"false"}"#)
            .await
            .expect("malformed legacy settings");

        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        assert!(
            control
                .update_settings(json!({"patch":{"terminalDefaultShell":"/bin/test-shell"}}))
                .await
                .is_err()
        );
        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(settings_path)
                .await
                .expect("original settings"),
        )
        .expect("valid JSON");
        assert_eq!(persisted["enableAgentActivity"], "false");
        assert!(persisted.get("enableChatAgentActivity").is_none());
    }

    #[tokio::test]
    async fn agent_activity_terminal_only_update_dispatches_after_persistence() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        let calls = Arc::new(StdMutex::new(Vec::new()));
        control
            .attach_agent_activity_handler(Arc::new(TestAgentActivityHandler {
                calls: calls.clone(),
            }))
            .await;
        let updated = control
            .update_settings(json!({"patch":{"enableTerminalAgentActivity":true}}))
            .await
            .expect("enable terminal activity");

        assert_eq!(updated["enableChatAgentActivity"], true);
        assert_eq!(updated["enableTerminalAgentActivity"], true);
        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(&settings_path)
                .await
                .expect("persisted settings"),
        )
        .expect("valid persisted settings");
        assert_eq!(persisted["enableChatAgentActivity"], true);
        assert_eq!(persisted["enableTerminalAgentActivity"], true);
        assert_eq!(
            *calls.lock().expect("handler calls"),
            vec![(AgentActivitySource::Terminal, true, 1)]
        );
        assert!(
            control
                .agent_activity_enabled(AgentActivitySource::Chat)
                .await
        );
        assert!(
            control
                .agent_activity_enabled(AgentActivitySource::Terminal)
                .await
        );
    }

    #[tokio::test]
    async fn agent_activity_chat_only_update_dispatches_after_persistence() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        let calls = Arc::new(StdMutex::new(Vec::new()));
        control
            .attach_agent_activity_handler(Arc::new(TestAgentActivityHandler {
                calls: calls.clone(),
            }))
            .await;

        control
            .update_settings(json!({"patch":{"enableChatAgentActivity":false}}))
            .await
            .expect("disable chat activity");

        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(&settings_path)
                .await
                .expect("persisted settings"),
        )
        .expect("valid persisted settings");
        assert_eq!(persisted["enableChatAgentActivity"], false);
        assert_eq!(persisted["enableTerminalAgentActivity"], false);
        assert_eq!(
            *calls.lock().expect("handler calls"),
            vec![(AgentActivitySource::Chat, false, 1)]
        );
        assert!(
            !control
                .agent_activity_enabled(AgentActivitySource::Chat)
                .await
        );
        assert!(
            !control
                .agent_activity_enabled(AgentActivitySource::Terminal)
                .await
        );
    }

    #[tokio::test]
    async fn agent_activity_two_source_update_dispatches_chat_then_terminal() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        let calls = Arc::new(StdMutex::new(Vec::new()));
        control
            .attach_agent_activity_handler(Arc::new(TestAgentActivityHandler {
                calls: calls.clone(),
            }))
            .await;

        control
            .update_settings(json!({
                "patch": {
                    "enableChatAgentActivity": false,
                    "enableTerminalAgentActivity": true,
                }
            }))
            .await
            .expect("change both activity settings");

        assert_eq!(
            *calls.lock().expect("handler calls"),
            vec![
                (AgentActivitySource::Chat, false, 1),
                (AgentActivitySource::Terminal, true, 1),
            ]
        );
    }

    #[tokio::test]
    async fn failed_persistence_does_not_call_agent_activity_handler() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        tokio::fs::remove_file(&settings_path)
            .await
            .expect("replace the fixture with a persistence blocker");
        tokio::fs::create_dir_all(&settings_path)
            .await
            .expect("directory blocks atomic settings rename");
        let calls = Arc::new(StdMutex::new(Vec::new()));
        control
            .attach_agent_activity_handler(Arc::new(TestAgentActivityHandler {
                calls: calls.clone(),
            }))
            .await;

        assert!(
            control
                .update_settings(json!({"patch":{"enableChatAgentActivity":false}}))
                .await
                .is_err()
        );
        assert!(calls.lock().expect("handler calls").is_empty());
        assert!(
            control
                .agent_activity_enabled(AgentActivitySource::Chat)
                .await
        );
        assert!(
            !control
                .agent_activity_enabled(AgentActivitySource::Terminal)
                .await
        );
    }

    #[tokio::test]
    async fn agent_activity_commit_survives_caller_cancellation_and_publishes_after_ordered_transitions()
     {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let first_transition_entered = Arc::new(Notify::new());
        let release_first_transition = Arc::new(Notify::new());
        control
            .attach_agent_activity_handler(Arc::new(BlockingAgentActivityHandler {
                calls: calls.clone(),
                first_transition_entered: first_transition_entered.clone(),
                release_first_transition: release_first_transition.clone(),
            }))
            .await;
        let mut events = control.config_events.subscribe();

        let update = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_settings(json!({
                        "patch": {
                            "enableChatAgentActivity": false,
                            "enableTerminalAgentActivity": true,
                        }
                    }))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), first_transition_entered.notified())
            .await
            .expect("first transition starts after persistence");
        let persisted_before_transition_release: Value = serde_json::from_slice(
            &tokio::fs::read(&settings_path)
                .await
                .expect("settings persisted before transition"),
        )
        .expect("valid settings persisted before transition");
        assert_eq!(
            persisted_before_transition_release["enableChatAgentActivity"],
            false
        );
        assert_eq!(
            persisted_before_transition_release["enableTerminalAgentActivity"],
            true
        );
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        update.abort();
        release_first_transition.notify_one();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("detached commit publishes settings")
            .expect("settings event");
        assert_eq!(
            event["payload"]["settings"]["enableChatAgentActivity"],
            false
        );
        assert_eq!(
            event["payload"]["settings"]["enableTerminalAgentActivity"],
            true
        );

        assert_eq!(
            *calls.lock().expect("handler calls"),
            vec![
                (AgentActivitySource::Chat, false, 1),
                (AgentActivitySource::Terminal, true, 1),
            ]
        );
        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(&settings_path)
                .await
                .expect("split settings"),
        )
        .expect("valid split settings");
        assert_eq!(persisted["enableChatAgentActivity"], false);
        assert_eq!(persisted["enableTerminalAgentActivity"], true);
        assert!(
            !control
                .agent_activity_enabled(AgentActivitySource::Chat)
                .await
        );
        assert!(
            control
                .agent_activity_enabled(AgentActivitySource::Terminal)
                .await
        );
        assert_eq!(control.settings_generation.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn agent_activity_persisted_settings_converge_after_cancellation_before_commit_task_setup()
     {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;
        let chat = AgentActivityController::new(true);
        let terminal = AgentActivityController::new(false);
        let calls = Arc::new(StdMutex::new(Vec::new()));
        control
            .attach_agent_activity_handler(Arc::new(ControllerAgentActivityHandler {
                chat: chat.clone(),
                terminal: terminal.clone(),
                calls: calls.clone(),
            }))
            .await;
        let mut events = control.config_events.subscribe();
        let persisted = control.install_next_settings_persisted_pause().await;
        let update = {
            let control = control.clone();
            tokio::spawn(async move {
                control
                    .update_settings(json!({"patch":{"enableChatAgentActivity":false}}))
                    .await
            })
        };

        persisted.wait_until_entered().await;
        let disk_during_pause: Value = serde_json::from_slice(
            &tokio::fs::read(&settings_path)
                .await
                .expect("persisted settings"),
        )
        .expect("valid persisted settings");
        assert_eq!(disk_during_pause["enableChatAgentActivity"], false);
        assert_eq!(
            control.settings.read().await["enableChatAgentActivity"],
            true
        );
        assert!(chat.snapshot().enabled);
        assert!(!terminal.snapshot().enabled);
        assert!(calls.lock().expect("handler calls").is_empty());
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));

        update.abort();
        assert!(update.await.expect_err("caller cancelled").is_cancelled());
        persisted.release();

        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("detached commit publishes after caller cancellation")
            .expect("settings event");
        assert_eq!(
            event["payload"]["settings"]["enableChatAgentActivity"],
            false
        );
        assert_eq!(
            control.settings.read().await["enableChatAgentActivity"],
            false
        );
        assert!(!chat.snapshot().enabled);
        assert!(!terminal.snapshot().enabled);
        assert_eq!(
            *calls.lock().expect("handler calls"),
            vec![(AgentActivitySource::Chat, false, 1)]
        );
        let disk_after_commit: Value = serde_json::from_slice(
            &tokio::fs::read(settings_path)
                .await
                .expect("committed settings"),
        )
        .expect("valid committed settings");
        assert_eq!(disk_after_commit["enableChatAgentActivity"], false);
    }

    #[test]
    fn default_agent_is_defaulted_and_validated() {
        let mut settings = json!({});
        apply_settings_defaults(&mut settings);
        assert_eq!(
            settings["defaultAgent"],
            json!({"kind":"chat","instanceId":"codex"})
        );
        assert!(validate_settings_document(&settings).is_ok());

        settings["defaultAgent"] = json!({"kind":"terminal","instanceId":"claudeAgent"});
        assert!(validate_settings_document(&settings).is_ok());

        settings["defaultAgent"] = json!({"kind":"shell","instanceId":"codex"});
        assert!(validate_settings_document(&settings).is_err());
    }

    #[tokio::test]
    async fn default_agent_update_persists_as_a_whole_selection() {
        let temp = tempfile::tempdir().expect("control root");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;

        let updated = control
            .update_settings(json!({
                "patch":{"defaultAgent":{"kind":"terminal","instanceId":"claudeAgent"}}
            }))
            .await
            .expect("default agent update");
        assert_eq!(
            updated["defaultAgent"],
            json!({"kind":"terminal","instanceId":"claudeAgent"})
        );
        assert!(
            control
                .update_settings(json!({"patch":{"defaultAgent":{"kind":"terminal"}}}))
                .await
                .is_err()
        );

        let persisted: Value = serde_json::from_slice(
            &tokio::fs::read(settings_path)
                .await
                .expect("persisted settings"),
        )
        .expect("valid JSON");
        assert_eq!(persisted["defaultAgent"], updated["defaultAgent"]);
    }

    #[tokio::test]
    async fn workspace_update_normalizes_existing_directories_and_rejects_invalid_targets() {
        let temp = tempfile::tempdir().expect("control root");
        let workspace = temp.path().join("workspace");
        tokio::fs::create_dir(&workspace).await.expect("workspace");
        let file = temp.path().join("file.txt");
        tokio::fs::write(&file, b"file").await.expect("file");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy":"test"})).await;

        let updated = control
            .update_settings(json!({
                "patch": {"worktreeBaseDirectory": workspace.to_string_lossy()}
            }))
            .await
            .expect("valid workspace");
        assert_eq!(
            PathBuf::from(updated["worktreeBaseDirectory"].as_str().expect("path")),
            super::super::host_paths::process_compatible_path(
                tokio::fs::canonicalize(&workspace)
                    .await
                    .expect("canonical workspace"),
            )
        );

        for (path, failure) in [
            ("relative/worktrees".to_owned(), "relative_path"),
            (
                temp.path().join("missing").to_string_lossy().into_owned(),
                "missing",
            ),
            (file.to_string_lossy().into_owned(), "not_directory"),
        ] {
            let error = control
                .update_settings(json!({"patch":{"worktreeBaseDirectory":path}}))
                .await
                .expect_err("invalid workspace");
            assert_eq!(error["_tag"], "WorktreeWorkspaceError");
            assert_eq!(error["failure"], failure);
            assert_eq!(
                control.settings.read().await["worktreeBaseDirectory"],
                updated["worktreeBaseDirectory"]
            );
            let persisted: Value = serde_json::from_slice(
                &tokio::fs::read(&settings_path)
                    .await
                    .expect("persisted settings"),
            )
            .expect("valid persisted JSON");
            assert_eq!(
                persisted["worktreeBaseDirectory"],
                updated["worktreeBaseDirectory"]
            );
        }
    }

    #[test]
    fn quick_and_failed_probes_retain_rich_metadata_but_update_health() {
        let current = json!({
            "instanceId": "codex",
            "driver": "codex",
            "status": "ready",
            "checkedAt": "old",
            "models": [{ "slug": "gpt-rich" }],
            "slashCommands": [{ "name": "goal" }],
            "skills": [{ "name": "review" }],
            "agents": [{ "name": "builder" }]
        });
        let quick = provider_inventory::ProviderProbeResult {
            snapshot: json!({
                "instanceId": "codex",
                "driver": "codex",
                "status": "warning",
                "checkedAt": "new",
                "models": [{ "slug": "gpt-fallback" }],
                "slashCommands": [{ "name": "goal" }],
                "skills": [],
                "agents": []
            }),
            rich_metadata: provider_inventory::RichMetadataOutcome::NotRequested,
            models_authoritative: false,
        };
        let failed = provider_inventory::ProviderProbeResult {
            rich_metadata: provider_inventory::RichMetadataOutcome::Failed,
            ..quick.clone()
        };

        for result in [quick, failed] {
            let merged = merge_provider_snapshot(Some(&current), result);
            assert_eq!(merged["status"], "warning");
            assert_eq!(merged["checkedAt"], "new");
            assert_eq!(merged["models"], current["models"]);
            assert_eq!(merged["skills"], current["skills"]);
            assert_eq!(merged["agents"], current["agents"]);
        }
    }

    async fn publish_inventory_for_test(
        control: &NativeServerControl,
        snapshots: Vec<Value>,
        partial: bool,
    ) {
        let (generation, settings) = control.settings_snapshot().await;
        let sequence = control.begin_provider_probe();
        control
            .publish_provider_snapshots_if_current(
                snapshots
                    .into_iter()
                    .map(|snapshot| provider_inventory::ProviderProbeResult {
                        snapshot,
                        rich_metadata: provider_inventory::RichMetadataOutcome::Succeeded,
                        models_authoritative: true,
                    })
                    .collect(),
                partial,
                generation,
                &settings,
                sequence,
                ProviderSnapshotPublication::IfChanged,
            )
            .await
            .expect("current probe is accepted");
    }

    #[tokio::test]
    async fn provider_statuses_ignore_probe_timestamps_but_store_them() {
        let mut unexpected_publications = Vec::new();
        for partial in [false, true] {
            let temp = tempfile::tempdir().expect("state directory");
            let control = scheduler_control(&temp).await;
            let mut snapshot = control.providers.read().await[0].clone();
            snapshot["checkedAt"] = json!("2026-09-26T10:00:00Z");
            snapshot["models"] = json!([{ "slug": "initial-model" }]);
            snapshot["versionAdvisory"] = json!({
                "status": "unknown",
                "currentVersion": null,
                "latestVersion": null,
                "updateCommand": null,
                "canUpdate": false,
                "checkedAt": "2026-09-26T10:00:00Z",
                "message": null
            });
            let mut events = control.config_events.subscribe();
            publish_inventory_for_test(&control, vec![snapshot.clone()], partial).await;
            assert_eq!(
                events.try_recv().expect("initial inventory")["type"],
                "providerStatuses"
            );

            for path in PROVIDER_PROBE_TIMESTAMP_PATHS {
                let pointer = format!("/{}", path.join("/"));
                *snapshot.pointer_mut(&pointer).expect("probe timestamp") =
                    json!("2026-09-26T11:00:00Z");
                publish_inventory_for_test(&control, vec![snapshot.clone()], partial).await;
                let config = control.config_snapshot().await;
                let stored = config["providers"]
                    .as_array()
                    .expect("provider snapshot")
                    .iter()
                    .find(|provider| provider["instanceId"] == snapshot["instanceId"])
                    .expect("refreshed provider");
                assert_eq!(
                    stored.pointer(&pointer),
                    Some(&json!("2026-09-26T11:00:00Z"))
                );
                while let Ok(event) = events.try_recv() {
                    assert_eq!(event["type"], "providerStatuses");
                    unexpected_publications.push((partial, pointer.clone()));
                }
            }
        }
        assert!(
            unexpected_publications.is_empty(),
            "timestamp-only refreshes published providerStatuses: {unexpected_publications:?}"
        );
    }

    #[tokio::test]
    async fn provider_statuses_publish_once_for_model_and_skill_changes() {
        for partial in [false, true] {
            let temp = tempfile::tempdir().expect("state directory");
            let control = scheduler_control(&temp).await;
            let mut snapshot = control.providers.read().await[0].clone();
            snapshot["models"] = json!([{ "slug": "initial-model" }]);
            let mut events = control.config_events.subscribe();
            publish_inventory_for_test(&control, vec![snapshot.clone()], partial).await;
            events.try_recv().expect("initial inventory");

            for (field, value) in [
                ("models", json!([{ "slug": "new-model" }])),
                ("skills", json!([{ "name": "new-skill" }])),
            ] {
                snapshot[field] = value.clone();
                publish_inventory_for_test(&control, vec![snapshot.clone()], partial).await;
                let event = events.try_recv().expect("changed inventory publishes");
                assert_eq!(event["type"], "providerStatuses");
                let published = event["payload"]["providers"]
                    .as_array()
                    .expect("provider statuses")
                    .iter()
                    .find(|provider| provider["instanceId"] == snapshot["instanceId"])
                    .expect("changed provider");
                assert_eq!(published[field], value);
                assert!(matches!(
                    events.try_recv(),
                    Err(broadcast::error::TryRecvError::Empty)
                ));

                publish_inventory_for_test(&control, vec![snapshot.clone()], partial).await;
                assert!(
                    matches!(
                        events.try_recv(),
                        Err(broadcast::error::TryRecvError::Empty)
                    ),
                    "an unchanged {field} inventory must not publish again (partial={partial})"
                );
            }
        }
    }

    #[tokio::test]
    async fn provider_statuses_publish_only_changed_update_state() {
        let temp = tempfile::tempdir().expect("state directory");
        let control = scheduler_control(&temp).await;
        let (generation, settings) = control.settings_snapshot().await;
        let target = provider_inventory::maintenance_target(&settings, "cursor", Some("cursor"))
            .expect("configured Cursor target");
        let reservation = control
            .provider_maintenance
            .reserve_target(&target, generation)
            .expect("reserve provider update");
        let mut events = control.config_events.subscribe();

        for (status, finished_at) in [
            ("running", None),
            ("succeeded", Some("2026-09-26T11:00:00Z")),
        ] {
            let state = provider_update_state(
                status,
                Some("2026-09-26T10:00:00Z"),
                finished_at,
                "Provider update",
                None,
            );
            for first_publication in [true, false] {
                let (_, retained) = control
                    .publish_provider_update_state(&target, reservation.token(), state.clone())
                    .await;
                assert!(retained);
                if first_publication {
                    let event = events.try_recv().expect("changed update state publishes");
                    assert_eq!(event["type"], "providerStatuses");
                    let cursor = event["payload"]["providers"]
                        .as_array()
                        .expect("provider statuses")
                        .iter()
                        .find(|provider| provider["instanceId"] == "cursor")
                        .expect("Cursor status");
                    assert_eq!(cursor["updateState"]["status"], status);
                }
                assert!(
                    matches!(
                        events.try_recv(),
                        Err(broadcast::error::TryRecvError::Empty)
                    ),
                    "unchanged update state must not publish providerStatuses"
                );
            }
        }
    }

    #[test]
    fn authoritative_models_survive_a_failed_capabilities_probe() {
        let current = json!({
            "instanceId": "claudeAgent",
            "driver": "claudeAgent",
            "models": [{ "slug": "claude-too-new" }],
            "slashCommands": [{ "name": "old-command" }],
            "skills": [{ "name": "old-skill" }],
            "agents": [{ "name": "old-agent" }]
        });
        let refreshed = provider_inventory::ProviderProbeResult {
            snapshot: json!({
                "instanceId": "claudeAgent",
                "driver": "claudeAgent",
                "models": [{ "slug": "claude-supported" }],
                "slashCommands": [],
                "skills": [],
                "agents": []
            }),
            rich_metadata: provider_inventory::RichMetadataOutcome::Failed,
            models_authoritative: true,
        };

        let merged = merge_provider_snapshot(Some(&current), refreshed);

        assert_eq!(merged["models"], json!([{ "slug": "claude-supported" }]));
        assert_eq!(merged["slashCommands"], current["slashCommands"]);
        assert_eq!(merged["skills"], current["skills"]);
        assert_eq!(merged["agents"], current["agents"]);
    }

    #[test]
    fn disabled_replacement_does_not_retain_metadata_from_another_driver() {
        let current = json!({
            "instanceId": "shared",
            "driver": "codex",
            "enabled": true,
            "models": [{ "slug": "gpt-rich" }],
            "slashCommands": [{ "name": "goal" }],
            "skills": [{ "name": "review" }],
            "agents": [{ "name": "builder" }]
        });
        let replacement = provider_inventory::ProviderProbeResult {
            snapshot: json!({
                "instanceId": "shared",
                "driver": "claudeAgent",
                "enabled": false,
                "models": [{ "slug": "claude-disabled" }],
                "slashCommands": [{ "name": "loop" }],
                "skills": [],
                "agents": []
            }),
            rich_metadata: provider_inventory::RichMetadataOutcome::NotRequested,
            models_authoritative: false,
        };

        let merged = merge_provider_snapshot(Some(&current), replacement);

        assert_eq!(merged["driver"], "claudeAgent");
        assert_eq!(merged["enabled"], false);
        assert_eq!(merged["models"], json!([{ "slug": "claude-disabled" }]));
        assert_eq!(merged["slashCommands"], json!([{ "name": "loop" }]));
        assert_eq!(merged["skills"], json!([]));
        assert_eq!(merged["agents"], json!([]));
    }

    #[test]
    fn successful_rich_probe_can_authoritatively_clear_metadata() {
        let current = json!({
            "instanceId": "codex",
            "models": [{ "slug": "retired" }],
            "slashCommands": [{ "name": "old" }],
            "skills": [{ "name": "old" }],
            "agents": [{ "name": "old" }]
        });
        let merged = merge_provider_snapshot(
            Some(&current),
            provider_inventory::ProviderProbeResult {
                snapshot: json!({
                    "instanceId": "codex",
                    "models": [],
                    "slashCommands": [],
                    "skills": [],
                    "agents": []
                }),
                rich_metadata: provider_inventory::RichMetadataOutcome::Succeeded,
                models_authoritative: true,
            },
        );

        assert_eq!(merged["models"], json!([]));
        assert_eq!(merged["skills"], json!([]));
        assert_eq!(merged["agents"], json!([]));
    }

    #[tokio::test]
    async fn partial_builtin_provider_settings_agree_with_runtime_reader() {
        let temp = tempfile::tempdir().expect("state directory");
        // This helper writes only binaryPath for each legacy provider and pins
        // every probed instance to an absent executable.
        let control = scheduler_control(&temp).await;
        let runtime =
            crate::server_settings::ProviderSettingsStore::new(control.config.state_dir())
                .get()
                .await
                .expect("runtime settings");
        let runtime = serde_json::to_value(runtime.providers).expect("runtime providers");
        let snapshot = control.config_snapshot().await;
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for driver in crate::test_support::hermetic_providers::BUILTIN_PROVIDER_DRIVERS {
            actual.push((
                *driver,
                snapshot["settings"]["providers"][driver]["enabled"].clone(),
            ));
            expected.push((*driver, runtime[driver]["enabled"].clone()));
            assert_eq!(
                snapshot["settings"]["providers"][driver]["binaryPath"],
                runtime[driver]["binaryPath"],
                "{driver}"
            );
        }
        assert_eq!(actual, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn absent_settings_file_defaults_agree_with_runtime_reader() {
        use crate::test_support::{reexec, run_on_current_thread};

        const TEST: &str =
            "production::control::tests::absent_settings_file_defaults_agree_with_runtime_reader";
        if let Some(child) = reexec::enter(TEST, "isolated-provider-path") {
            run_on_current_thread(async {
                let temp = tempfile::tempdir().expect("state directory");
                let config = running_test_config(temp.path());
                let state_dir = config.state_dir();
                assert!(!state_dir.join("settings.json").exists());
                let control = NativeServerControl::new(config, json!({"policy": "test"})).await;
                let runtime = crate::server_settings::ProviderSettingsStore::new(&state_dir)
                    .get()
                    .await
                    .expect("runtime defaults");
                let runtime = serde_json::to_value(runtime.providers).expect("runtime providers");
                let snapshot = control.config_snapshot().await;
                for driver in crate::test_support::hermetic_providers::BUILTIN_PROVIDER_DRIVERS {
                    assert_eq!(
                        snapshot["settings"]["providers"][driver]["enabled"],
                        runtime[driver]["enabled"],
                        "{driver}"
                    );
                    assert_eq!(
                        snapshot["settings"]["providers"][driver]["binaryPath"],
                        runtime[driver]["binaryPath"],
                        "{driver}"
                    );
                }
                assert!(
                    snapshot["providers"]
                        .as_array()
                        .expect("inventory")
                        .iter()
                        .all(|provider| provider["installed"] == false)
                );
                assert!(!state_dir.join("settings.json").exists());
            });
            child.complete();
            return;
        }

        // With no file there is nowhere to pin executables in settings. Isolate
        // PATH and HOME in an exact-test child, leaving parallel tests untouched.
        let sandbox = tempfile::tempdir().expect("provider lookup sandbox");
        reexec::run(TEST, "isolated-provider-path", None, |command| {
            command
                .current_dir(sandbox.path())
                .env("PATH", sandbox.path())
                .env("HOME", sandbox.path())
                .env("USERPROFILE", sandbox.path())
                .env("CLAUDE_CONFIG_DIR", sandbox.path())
                .env("XDG_CONFIG_HOME", sandbox.path());
        });
    }

    #[test]
    fn builtin_provider_defaults_enable_cursor_with_cursor_agent() {
        let mut settings = json!({});
        apply_settings_defaults(&mut settings);
        assert_eq!(
            settings["providers"],
            crate::server_settings::built_in_provider_defaults()
        );
        assert_eq!(settings["providers"]["cursor"]["enabled"], true);
        assert_eq!(
            settings["providers"]["cursor"]["binaryPath"],
            "cursor-agent"
        );
    }

    #[test]
    fn provider_session_defaults_are_defaulted_and_replaced_as_a_whole() {
        let mut settings = json!({
            "providers": {
                "codex": {
                    "enabled": false,
                    "binaryPath": "/opt/bin/codex"
                }
            },
            "providerInstances": {
                "work": {
                    "driver": "codex",
                    "displayName": "Work"
                }
            }
        });

        apply_settings_defaults(&mut settings);
        assert_eq!(settings["providerSessionDefaults"], json!({}));
        let providers = settings["providers"].clone();
        let provider_instances = settings["providerInstances"].clone();

        apply_settings_patch(
            &mut settings,
            json!({
                "providerSessionDefaults": {
                    "codex": {
                        "model": "gpt-5.4",
                        "options": [{"id": "reasoningEffort", "value": "medium"}]
                    },
                    "claudeAgent": {
                        "model": "claude-sonnet-4-6"
                    }
                }
            }),
        );
        assert_eq!(
            settings["providerSessionDefaults"],
            json!({
                "codex": {
                    "model": "gpt-5.4",
                    "options": [{"id": "reasoningEffort", "value": "medium"}]
                },
                "claudeAgent": {
                    "model": "claude-sonnet-4-6"
                }
            })
        );
        assert_eq!(settings["providers"], providers);
        assert_eq!(settings["providerInstances"], provider_instances);

        apply_settings_patch(
            &mut settings,
            json!({
                "providerSessionDefaults": {
                    "codex": {
                        "model": "gpt-5.4-mini",
                        "options": [{"id": "fastMode", "value": true}]
                    }
                }
            }),
        );
        assert_eq!(
            settings["providerSessionDefaults"],
            json!({
                "codex": {
                    "model": "gpt-5.4-mini",
                    "options": [{"id": "fastMode", "value": true}]
                }
            })
        );
        assert_eq!(settings["providers"], providers);
        assert_eq!(settings["providerInstances"], provider_instances);

        apply_settings_patch(&mut settings, json!({"observability":{"legacy":true}}));
        apply_settings_defaults(&mut settings);
        assert!(settings.get("observability").is_none());
    }

    #[tokio::test]
    async fn server_settings_expose_terminal_webgl_default_and_patch() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = ServerConfig::new(temp.path());
        config.environment_id = "environment-webgl".to_owned();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;

        let settings = control
            .call("server.getSettings", json!({}), CancellationToken::new())
            .await
            .expect("settings");
        assert_eq!(settings["terminal"]["webglEnabled"], true);

        let updated = control
            .update_settings(json!({ "patch": { "terminal": { "webglEnabled": false } } }))
            .await
            .expect("patch applies");
        assert_eq!(updated["terminal"]["webglEnabled"], false);
        // The unrelated hermetic setting survives the terminal patch.
        assert_eq!(updated["enableProviderUpdateChecks"], false);
    }

    #[tokio::test]
    async fn concurrent_settings_updates_preserve_every_committed_patch() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = ServerConfig::new(temp.path());
        config.environment_id = "environment-concurrent-settings".to_owned();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;
        control.install_settings_update_barrier(24).await;

        let updates = (0..24)
            .map(|index| {
                let control = control.clone();
                tokio::spawn(async move {
                    control
                        .update_settings(json!({
                            "patch": {
                                "concurrentUpdates": {
                                    format!("update-{index}"): true
                                },
                                "providerInstances": {
                                    "concurrency-test": {
                                        "driver": "codex",
                                        "environment": [{
                                            "name": "TOKEN",
                                            "value": format!("secret-{index}"),
                                            "sensitive": true
                                        }]
                                    }
                                }
                            }
                        }))
                        .await
                })
            })
            .collect::<Vec<_>>();
        for update in updates {
            update
                .await
                .expect("settings task joins")
                .expect("settings update succeeds");
        }

        let settings = control
            .call("server.getSettings", json!({}), CancellationToken::new())
            .await
            .expect("settings remain readable");
        for index in 0..24 {
            assert_eq!(
                settings["concurrentUpdates"][format!("update-{index}")],
                true
            );
        }
        let persisted = read_json::<Value>(control.settings_path.clone())
            .await
            .expect("settings file reads")
            .expect("settings file exists");
        assert_eq!(
            persisted["concurrentUpdates"],
            settings["concurrentUpdates"]
        );
    }

    #[tokio::test]
    async fn older_probe_completion_cannot_overwrite_newer_same_generation_snapshot() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = ServerConfig::new(temp.path());
        config.environment_id = "environment-provider-probe-order".to_owned();
        let settings_path = config.state_dir().join("settings.json");
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        tokio::fs::write(
            settings_path,
            br#"{
                "providers": {
                    "codex": {"enabled": false},
                    "claudeAgent": {"enabled": false},
                    "cursor": {"enabled": false},
                    "grok": {"enabled": false},
                    "opencode": {"enabled": false}
                }
            }"#,
        )
        .await
        .expect("disabled provider fixture");
        hermetic_providers::ensure_hermetic_settings(&config.state_dir());
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;
        let (generation, settings) = control.settings_snapshot().await;

        let older_sequence = control.begin_provider_probe();
        let newer_sequence = control.begin_provider_probe();
        let older_release = Arc::new(Notify::new());
        let older_entered = Arc::new(Notify::new());
        let older_completion = {
            let control = control.clone();
            let settings = settings.clone();
            let older_release = older_release.clone();
            let older_entered = older_entered.clone();
            tokio::spawn(async move {
                older_entered.notify_one();
                older_release.notified().await;
                control
                    .publish_provider_snapshots_if_current(
                        vec![provider_inventory::ProviderProbeResult {
                            snapshot: json!({
                                "instanceId": "codex",
                                "driver": "codex",
                                "checkedAt": "older-completion",
                                "models": [],
                                "slashCommands": [],
                                "skills": [],
                                "agents": []
                            }),
                            rich_metadata: provider_inventory::RichMetadataOutcome::Succeeded,
                            models_authoritative: true,
                        }],
                        false,
                        generation,
                        &settings,
                        older_sequence,
                        ProviderSnapshotPublication::IfChanged,
                    )
                    .await
            })
        };
        older_entered.notified().await;

        control
            .publish_provider_snapshots_if_current(
                vec![provider_inventory::ProviderProbeResult {
                    snapshot: json!({
                        "instanceId": "codex",
                        "driver": "codex",
                        "checkedAt": "newer-completion",
                        "models": [],
                        "slashCommands": [],
                        "skills": [],
                        "agents": []
                    }),
                    rich_metadata: provider_inventory::RichMetadataOutcome::Succeeded,
                    models_authoritative: true,
                }],
                false,
                generation,
                &settings,
                newer_sequence,
                ProviderSnapshotPublication::IfChanged,
            )
            .await
            .expect("newer probe publishes");
        older_release.notify_one();

        assert!(
            older_completion
                .await
                .expect("older completion joins")
                .is_none(),
            "older probe request must not publish after a newer request"
        );
        assert_eq!(
            control.providers.read().await[0]["checkedAt"],
            "newer-completion"
        );
    }

    #[tokio::test]
    async fn concurrent_settings_stream_discards_stale_provider_probes_in_commit_order() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = running_test_config(temp.path());
        config.environment_id = "environment-concurrent-stream".to_owned();
        let settings_path = config.state_dir().join("settings.json");
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        tokio::fs::write(
            &settings_path,
            br#"{
                "providers": {
                    "codex": {"enabled": false},
                    "claudeAgent": {"enabled": false},
                    "cursor": {"enabled": false},
                    "grok": {"enabled": false},
                    "opencode": {"enabled": false}
                }
            }"#,
        )
        .await
        .expect("disabled provider fixture");
        hermetic_providers::ensure_hermetic_settings(&config.state_dir());
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;

        let initial_full_probe = control.install_next_full_provider_probe_pause().await;
        let cancellation = CancellationToken::new();
        let mut stream = control.subscribe("subscribeServerConfig", cancellation.clone());
        let snapshot = stream
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot batch");
        assert_eq!(snapshot[0]["type"], "snapshot");
        initial_full_probe.wait_until_entered().await;

        let stale_quick_probe = control.install_next_quick_provider_probe_pause().await;
        let stale_control = control.clone();
        let stale_binary = missing_provider_executable(&temp);
        let stale_update = tokio::spawn(async move {
            stale_control
                .update_settings(json!({
                    "patch": {
                        "streamCommit": "first",
                        "providerInstances": {
                            "stale_instance": {
                                "driver": "codex",
                                "enabled": false,
                                "config": {"binaryPath": stale_binary}
                            }
                        }
                    }
                }))
                .await
        });
        stale_quick_probe.wait_until_entered().await;

        control
            .update_settings(json!({
                "patch": {
                    "streamCommit": "second",
                    "providerInstances": {
                        "current_instance": {
                            "driver": "codex",
                            "enabled": false,
                            "config": {"binaryPath": missing_provider_executable(&temp)}
                        }
                    }
                }
            }))
            .await
            .expect("current update succeeds");
        stale_quick_probe.release();
        stale_update
            .await
            .expect("stale update joins")
            .expect("stale update committed before it was delayed");
        initial_full_probe.release();

        tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
            while control
                .full_provider_refresh_running
                .load(Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("full provider refresh completes");

        let mut events = Vec::new();
        while let Ok(Some(batch)) =
            tokio::time::timeout(tokio::time::Duration::from_millis(50), stream.recv()).await
        {
            events.extend(batch.expect("config event batch"));
        }
        cancellation.cancel();

        let settings_events = events
            .iter()
            .filter(|event| event["type"] == "settingsUpdated")
            .collect::<Vec<_>>();
        assert_eq!(
            settings_events
                .iter()
                .map(|event| event["payload"]["settings"]["streamCommit"].as_str())
                .collect::<Vec<_>>(),
            vec![Some("first"), Some("second")]
        );
        let final_settings_index = events
            .iter()
            .rposition(|event| event["type"] == "settingsUpdated")
            .expect("final settings event");
        for event in events.iter().skip(final_settings_index + 1) {
            if event["type"] == "providerStatuses" {
                let providers = event["payload"]["providers"]
                    .as_array()
                    .expect("provider status payload");
                assert!(
                    providers
                        .iter()
                        .all(|provider| provider["instanceId"] != "stale_instance"),
                    "stale provider probe was published after the final settings event"
                );
            }
        }

        let memory_settings = control.settings.read().await.clone();
        let memory_providers = control.providers.read().await.clone();
        let persisted_settings = read_json::<Value>(&settings_path)
            .await
            .expect("settings file reads")
            .expect("settings file exists");
        assert_eq!(memory_settings, persisted_settings);
        assert_eq!(
            settings_events.last().expect("last settings event")["payload"]["settings"],
            memory_settings
        );
        let published_providers = events
            .iter()
            .rev()
            .find(|event| event["type"] == "providerStatuses")
            .expect("last provider event")["payload"]["providers"]
            .as_array()
            .expect("published providers")
            .clone();
        // A final refresh may store newer timestamps without another event.
        assert_eq!(published_providers.len(), memory_providers.len());
        assert!(
            published_providers
                .iter()
                .zip(&memory_providers)
                .all(|(published, stored)| provider_snapshot_content_eq(published, stored))
        );
    }

    #[tokio::test]
    async fn malformed_settings_surface_structured_errors_and_refuse_mutation() {
        #[cfg(feature = "hermetic-test-guard")]
        if crate::test_support::reexec_without_provider_path(
            "production::control::tests::malformed_settings_surface_structured_errors_and_refuse_mutation",
        ) {
            return;
        }
        let temp = tempfile::tempdir().expect("state directory");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        tokio::fs::write(&settings_path, b"{not-json")
            .await
            .expect("malformed settings fixture");
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;

        let read_error = control
            .call("server.getSettings", json!({}), CancellationToken::new())
            .await
            .expect_err("malformed settings are not presented as defaults");
        assert_eq!(read_error["_tag"], "ServerSettingsError");
        assert_eq!(read_error["operation"], "read-file");
        assert_eq!(
            control
                .call("server.getConfig", json!({}), CancellationToken::new())
                .await
                .expect_err("malformed settings reject config reads"),
            read_error
        );
        let mut config_stream =
            control.subscribe("subscribeServerConfig", CancellationToken::new());
        assert_eq!(
            config_stream
                .recv()
                .await
                .expect("config stream reports its load failure")
                .expect_err("malformed settings reject config subscriptions"),
            read_error
        );

        let update_error = control
            .update_settings(json!({"patch":{"enableAssistantStreaming":true}}))
            .await
            .expect_err("malformed settings refuse mutation");
        assert_eq!(update_error, read_error);
        assert_eq!(
            tokio::fs::read(&settings_path)
                .await
                .expect("malformed file remains readable"),
            b"{not-json"
        );
    }

    #[tokio::test]
    async fn schema_invalid_settings_surface_structured_errors_and_preserve_original_bytes() {
        #[cfg(feature = "hermetic-test-guard")]
        if crate::test_support::reexec_without_provider_path(
            "production::control::tests::schema_invalid_settings_surface_structured_errors_and_preserve_original_bytes",
        ) {
            return;
        }
        let cases = [
            ("top-level array", br#"[]"#.as_slice()),
            (
                "provider defaults array",
                br#"{"providerSessionDefaults":[]}"#.as_slice(),
            ),
            (
                "provider default model",
                br#"{"providerSessionDefaults":{"codex":{"model":42}}}"#.as_slice(),
            ),
            (
                "provider default option",
                br#"{"providerSessionDefaults":{"codex":{"model":"gpt-5.4","options":[{"id":"reasoningEffort","value":1}]}}}"#
                    .as_slice(),
            ),
            ("providers array", br#"{"providers":[]}"#.as_slice()),
            (
                "provider instances array",
                br#"{"providerInstances":[]}"#.as_slice(),
            ),
            (
                "provider instance null display name",
                br#"{"providerInstances":{"codex":{"driver":"codex","displayName":null}}}"#
                    .as_slice(),
            ),
            (
                "provider instance null accent color",
                br#"{"providerInstances":{"codex":{"driver":"codex","accentColor":null}}}"#
                    .as_slice(),
            ),
            (
                "model selection shape",
                br#"{"textGenerationModelSelection":{"instanceId":"codex","model":[]}}"#
                    .as_slice(),
            ),
            (
                "terminal shape",
                br#"{"terminal":{"webglEnabled":"yes"}}"#.as_slice(),
            ),
        ];

        for (name, original) in cases {
            let temp = tempfile::tempdir().expect("state directory");
            let mut config = ServerConfig::new(temp.path());
            config.environment_id = format!("environment-invalid-{name}");
            let settings_path = config.state_dir().join("settings.json");
            tokio::fs::create_dir_all(config.state_dir())
                .await
                .expect("state directory exists");
            tokio::fs::write(&settings_path, original)
                .await
                .expect("schema-invalid settings fixture");
            let control = NativeServerControl::new(config, json!({"policy": "test"})).await;

            let read_error = control
                .call("server.getSettings", json!({}), CancellationToken::new())
                .await
                .expect_err(name);
            assert_eq!(read_error["_tag"], "ServerSettingsError", "{name}");
            assert_eq!(read_error["operation"], "normalize", "{name}");
            assert_eq!(
                control
                    .call("server.getConfig", json!({}), CancellationToken::new())
                    .await
                    .expect_err(name),
                read_error,
                "{name}"
            );
            let mut config_stream =
                control.subscribe("subscribeServerConfig", CancellationToken::new());
            assert_eq!(
                config_stream
                    .recv()
                    .await
                    .expect("config stream reports its load failure")
                    .expect_err(name),
                read_error,
                "{name}"
            );
            assert_eq!(
                control
                    .update_settings(json!({"patch":{"enableAssistantStreaming":true}}))
                    .await
                    .expect_err(name),
                read_error,
                "{name}"
            );
            assert_eq!(
                tokio::fs::read(&settings_path)
                    .await
                    .expect("schema-invalid file remains readable"),
                original,
                "{name}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_settings_patch_is_transactional_and_publishes_no_events() {
        #[cfg(feature = "hermetic-test-guard")]
        if crate::test_support::reexec_without_provider_path(
            "production::control::tests::invalid_settings_patch_is_transactional_and_publishes_no_events",
        ) {
            return;
        }
        let temp = tempfile::tempdir().expect("state directory");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        let original = br#"{"enableAssistantStreaming":false}"#;
        tokio::fs::write(&settings_path, original)
            .await
            .expect("valid settings fixture");
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;
        let before = control.settings.read().await.clone();
        let mut events = control.config_events.subscribe();

        let error = control
            .update_settings(json!({"patch":{"terminal":{"webglEnabled":"yes"}}}))
            .await
            .expect_err("invalid patch is rejected");

        assert_eq!(error["_tag"], "ServerSettingsError");
        assert_eq!(error["operation"], "normalize");
        assert_eq!(*control.settings.read().await, before);
        assert_eq!(
            tokio::fs::read(&settings_path)
                .await
                .expect("settings file remains readable"),
            original
        );
        assert!(
            tokio::time::timeout(tokio::time::Duration::from_millis(50), events.recv())
                .await
                .is_err(),
            "invalid update must not publish settings or provider events"
        );
    }

    #[test]
    fn duplicate_case_insensitive_provider_environment_names_are_rejected() {
        let settings = json!({
            "providerInstances": {
                "codex-work": {
                    "driver": "codex",
                    "environment": [
                        { "name": "PATH", "value": "/first" },
                        { "name": "Path", "value": "/second" }
                    ]
                }
            }
        });

        assert_eq!(
            validate_settings_document(&settings).unwrap_err(),
            "providerInstances.codex-work.environment[1].name duplicates PATH"
        );
    }

    #[tokio::test]
    async fn settings_validation_accepts_fractional_fetch_intervals() {
        let temp = tempfile::tempdir().expect("state directory");
        let config = ServerConfig::new(temp.path());
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        hermetic_providers::write_hermetic_settings(
            &config.state_dir(),
            json!({"automaticGitFetchInterval": 0.1}),
        );
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;

        let settings = control
            .call("server.getSettings", json!({}), CancellationToken::new())
            .await
            .expect("fractional millisecond interval is contract-valid");

        assert_eq!(settings["automaticGitFetchInterval"], json!(0.1));
    }

    #[tokio::test]
    async fn settings_validation_preserves_open_unknown_keys_and_legacy_options() {
        #[cfg(feature = "hermetic-test-guard")]
        if crate::test_support::reexec_without_provider_path(
            "production::control::tests::settings_validation_preserves_open_unknown_keys_and_legacy_options",
        ) {
            return;
        }
        let temp = tempfile::tempdir().expect("state directory");
        let config = ServerConfig::new(temp.path());
        let settings_path = config.state_dir().join("settings.json");
        tokio::fs::create_dir_all(config.state_dir())
            .await
            .expect("state directory exists");
        tokio::fs::write(
            &settings_path,
            br#"{
                "futureSetting": {"nested": [1, true, null]},
                "providerInstances": {
                    "fork_one": {
                        "driver": "forkDriver",
                        "config": {"futureDriverField": {"kept": true}}
                    }
                },
                "providerSessionDefaults": {
                    "forkDriver": {
                        "model": "future-model",
                        "options": {"effort": "high", "futureNumericValue": 1}
                    }
                }
            }"#,
        )
        .await
        .expect("open settings fixture");

        hermetic_providers::ensure_hermetic_settings(&config.state_dir());
        let control = NativeServerControl::new(config, json!({"policy": "test"})).await;
        let settings = control
            .call("server.getSettings", json!({}), CancellationToken::new())
            .await
            .expect("open settings remain readable");

        assert_eq!(
            settings["futureSetting"],
            json!({"nested": [1, true, null]})
        );
        assert_eq!(
            settings["providerInstances"]["fork_one"]["config"]["futureDriverField"],
            json!({"kept": true})
        );
        assert_eq!(
            settings["providerSessionDefaults"]["forkDriver"]["options"],
            json!({"effort": "high", "futureNumericValue": 1})
        );
    }

    #[test]
    fn environment_descriptor_advertises_complete_worktree_catalog_surface() {
        let temp = tempfile::tempdir().expect("state directory");
        let config = running_test_config(temp.path());
        let descriptor = environment_descriptor(&config, false);
        assert_eq!(descriptor["capabilities"]["repositoryIdentity"], true);
        assert_eq!(descriptor["capabilities"]["worktreeCatalog"], true);
        assert_eq!(
            descriptor["capabilities"]["worktreeCatalogRefreshReason"],
            true
        );
        assert_eq!(descriptor["capabilities"]["vcsStatusSummary"], true);
        for capability in [
            "gitManagerReads",
            "gitManagerCommitOperations",
            "gitManagerBranchSyncOperations",
            "gitManagerStashMergeOperations",
            "gitManagerMergeIntoOperations",
            "gitManagerPartialStaging",
            "gitManagerRewriteOperations",
            "gitManagerTagOperations",
            "gitManagerLiveSignal",
            "gitManagerPullRequests",
            "gitPullRequestBranchSelection",
            "pullRequestsReads",
            "pullRequestsMutations",
        ] {
            assert_eq!(
                descriptor["capabilities"][capability], true,
                "Git Manager capability {capability} was not advertised"
            );
        }
        // Clients send the profiler RPCs only to servers that advertise them.
        assert_eq!(descriptor["capabilities"]["profiler"], true);
    }

    #[test]
    fn environment_descriptor_advertises_remote_update_control_and_support() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = running_test_config(temp.path());
        config.boot_id = Some(uuid::Uuid::nil());
        let descriptor = environment_descriptor(&config, false);
        assert_eq!(descriptor["bootId"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(descriptor["capabilities"]["remoteUpdateControl"], true);
        assert_eq!(descriptor["capabilities"]["remoteUpdateProgress"], true);
        assert_eq!(descriptor["capabilities"]["terminalOrderedInput"], true);
        assert_eq!(descriptor["capabilities"]["terminalSizeOwnership"], true);
        assert_eq!(descriptor["capabilities"]["previewTunnels"], true);
        assert_eq!(descriptor["capabilities"]["terminalImagePaste"], true);
        assert_eq!(descriptor["capabilities"]["pullRequestCreateOptions"], true);
        assert_eq!(descriptor["capabilities"]["vcsCloneReattach"], true);
        assert_eq!(descriptor["capabilities"]["attachmentStaging"], true);
        assert_eq!(
            descriptor["remoteUpdateSupport"],
            serde_json::json!({
                "installMode": "manual",
                "reason": "manual-update-required",
                "installKind": "unknown"
            })
        );
    }

    #[test]
    fn environment_descriptor_advertises_the_protocol_compatibility_window() {
        let temp = tempfile::tempdir().expect("state directory");
        let config = running_test_config(temp.path());
        let descriptor = environment_descriptor(&config, false);
        assert_eq!(descriptor["remoteProtocolVersion"], 1);
        assert_eq!(descriptor["minCompatibleRemoteProtocol"], 1);
    }

    #[tokio::test]
    async fn environment_descriptor_publishes_prepared_storage_identity_without_paths() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = ServerConfig::new(temp.path());
        let resolved = crate::resolve_data_root(config.data_root_request.clone())
            .expect("resolved test data root");
        config.base_dir = resolved.effective.clone();
        config.resolved_data_root = Some(resolved.clone());
        crate::persistence::StatePaths::from_config(&config)
            .ensure_directories_without_database_side_effects()
            .await
            .expect("state directories");
        let prepared = crate::persistence::prepare_store(&config)
            .await
            .expect("prepared test store");
        let expected = prepared.storage_instance_id.to_string();
        config.storage_instance_id = Some(prepared.storage_instance_id);

        let descriptor = environment_descriptor(&config, true);

        assert_eq!(descriptor["storageInstanceId"], expected);
        let encoded = descriptor.to_string();
        assert!(!encoded.contains(&resolved.requested.display().to_string()));
        assert!(!encoded.contains(&resolved.effective.display().to_string()));
        for forbidden in [
            "baseDir",
            "resolvedDataRoot",
            "requestedRoot",
            "effectiveRoot",
            "isFilesystemAlias",
        ] {
            assert!(
                descriptor.get(forbidden).is_none(),
                "forbidden field {forbidden}"
            );
        }
        prepared.database.close().await;
    }

    #[tokio::test]
    async fn unit_build_covers_server_control_settings_keybindings_and_streams() {
        let temp = tempfile::tempdir().expect("state directory");
        let mut config = running_test_config(temp.path());
        config.environment_id = "environment-1".to_owned();
        config.environment_label = "Environment One".to_owned();
        hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
        let control = NativeServerControl::new(config.clone(), json!({"policy":"test"})).await;

        let snapshot = control.config_snapshot().await;
        assert_eq!(snapshot["environment"]["environmentId"], "environment-1");
        assert_eq!(
            snapshot["environment"]["capabilities"]["worktreeCatalogRefreshReason"],
            true
        );
        assert_eq!(snapshot["auth"]["policy"], "test");
        assert!(!current_directory(&config).is_empty());
        assert!(!platform_os().is_empty());
        assert!(!platform_arch().is_empty());
        assert!(
            environment_descriptor(&config, false)["capabilities"]["repositoryIdentity"] == true
        );
        assert_eq!(
            environment_descriptor(&config, false)["capabilities"]["worktreeCatalog"],
            true
        );
        assert_eq!(
            environment_descriptor(&config, false)["capabilities"]["worktreeCatalogRefreshReason"],
            true
        );
        let editors = available_editors();
        assert!(
            editors
                .iter()
                .all(|id| super::super::editor_launch::editor_definition(id).is_some())
        );
        assert!(!editors.contains(&"intellij"));

        let call = |method, payload, cancellation| control.call(method, payload, cancellation);
        assert!(
            call("server.getSettings", json!({}), CancellationToken::new())
                .await
                .expect("settings")
                .is_object()
        );
        assert!(
            call("server.getConfig", json!({}), CancellationToken::new())
                .await
                .expect("config")
                .is_object()
        );
        assert_eq!(
            call(
                "server.updateProvider",
                json!({"provider":"grok"}),
                CancellationToken::new(),
            )
            .await
            .expect_err("provider update unavailable")["_tag"],
            "ServerProviderUpdateError"
        );
        assert_eq!(
            call("server.unknown", json!({}), CancellationToken::new())
                .await
                .expect_err("unknown method")["_tag"],
            "InvalidRequest"
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            call("server.getConfig", json!({}), cancelled)
                .await
                .expect_err("cancelled call")["_tag"],
            "RequestCancelled"
        );
        assert_eq!(
            control
                .update_settings(json!({}))
                .await
                .expect_err("missing patch")["operation"],
            "normalize"
        );
        assert!(control.update_settings(json!({"patch":[]})).await.is_err());

        let updated = control
            .update_settings(json!({
                "patch":{
                    "enableAssistantStreaming":true,
                    "providerInstances":{
                        "work":{
                            "driver":"codex",
                            "environment":[
                                {"name":"TOKEN","value":"secret","sensitive":true},
                                {"name":"PLAIN","value":"visible","sensitive":false}
                            ]
                        }
                    }
                }
            }))
            .await
            .expect("settings update");
        assert_eq!(updated["enableAssistantStreaming"], true);
        assert_eq!(
            updated["providerInstances"]["work"]["environment"][0]["valueRedacted"],
            true
        );
        assert!(secret_path(&control.state_directory, "work", "TOKEN").is_file());
        let observability = observability_snapshot(&control.state_directory);
        assert_eq!(observability["localTracingEnabled"], true);
        assert_eq!(observability["otlpTracesEnabled"], false);
        assert_eq!(observability["otlpMetricsEnabled"], false);

        let mut merge_target = json!({"nested":{"left":1},"replace":1});
        merge_patch(
            &mut merge_target,
            json!({"nested":{"right":2},"replace":{"value":3}}),
        );
        assert_eq!(merge_target["nested"], json!({"left":1,"right":2}));
        assert_eq!(merge_target["replace"]["value"], 3);
        merge_patch(&mut merge_target, json!("scalar"));
        assert_eq!(merge_target, "scalar");
        let mut defaults = Value::Null;
        apply_settings_defaults(&mut defaults);
        apply_settings_patch(
            &mut defaults,
            json!({
                "automaticGitFetchInterval":5000,
                "textGenerationModelSelection":{"model":"custom"},
                "providers":{"codex":{"enabled":false}}
            }),
        );
        assert_eq!(defaults["providers"]["codex"]["enabled"], false);

        let added = control
            .update_keybinding(
                "server.upsertKeybinding",
                json!({"key":"ctrl+shift+k","command":"terminal.newCenter"}),
            )
            .await
            .expect("keybinding adds");
        assert!(
            !added["keybindings"]
                .as_array()
                .expect("keybindings")
                .is_empty()
        );
        let removed = control
            .update_keybinding(
                "server.removeKeybinding",
                json!({"key":"ctrl+shift+k","command":"terminal.newCenter"}),
            )
            .await
            .expect("keybinding removes");
        assert!(
            !removed["keybindings"]
                .as_array()
                .expect("keybindings")
                .iter()
                .any(|binding| {
                    binding["command"] == "terminal.newCenter"
                        && binding["shortcut"]["key"] == "k"
                        && binding["shortcut"]["ctrlKey"] == true
                        && binding["shortcut"]["shiftKey"] == true
                })
        );
        assert!(
            control
                .update_keybinding("server.upsertKeybinding", json!({"key":"bad"}))
                .await
                .is_err()
        );
        assert_eq!(
            settings_error(Path::new("settings.json"), "read", "bad")["_tag"],
            "ServerSettingsError"
        );
        assert_eq!(
            keybindings_error(Path::new("keys.json"), "bad")["_tag"],
            "KeybindingsConfigParseError"
        );

        let refreshed = control.refresh_providers(&json!({})).await;
        assert!(refreshed["providers"].is_array());
        let _ = control
            .merge_provider_snapshots(
                vec![provider_inventory::ProviderProbeResult {
                    snapshot: json!({"instanceId":"unit-provider","status":"disabled"}),
                    rich_metadata: provider_inventory::RichMetadataOutcome::Succeeded,
                    models_authoritative: true,
                }],
                true,
            )
            .await;
        control.publish_provider_snapshots(&[json!({"instanceId":"unit-provider"})]);
        control.publish(json!({"type":"unit"}));
        assert!(control.trace_diagnostics().is_object());

        let lifecycle_cancellation = CancellationToken::new();
        let mut lifecycle =
            control.subscribe("subscribeServerLifecycle", lifecycle_cancellation.clone());
        let welcome = lifecycle
            .recv()
            .await
            .expect("lifecycle stream")
            .expect("welcome batch");
        assert_eq!(welcome[0]["type"], "welcome");
        let ready = lifecycle
            .recv()
            .await
            .expect("lifecycle stream")
            .expect("ready batch");
        assert_eq!(ready[0]["type"], "ready");
        lifecycle_cancellation.cancel();

        let config_cancellation = CancellationToken::new();
        let mut config_stream =
            control.subscribe("subscribeServerConfig", config_cancellation.clone());
        let config_event = config_stream
            .recv()
            .await
            .expect("config stream")
            .expect("snapshot batch");
        assert_eq!(config_event[0]["type"], "snapshot");
        config_cancellation.cancel();

        let unknown_cancellation = CancellationToken::new();
        let mut unknown_stream = control.subscribe("unknown", unknown_cancellation);
        assert!(unknown_stream.recv().await.is_none());
        assert!(!now_iso().is_empty());
    }
}
