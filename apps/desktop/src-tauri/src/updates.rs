use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use bibcode_server::{
    DESKTOP_MAINTENANCE_TOKEN_HEADER, MAINTENANCE_UPDATE_CANCEL_PATH,
    MAINTENANCE_UPDATE_COMMIT_PATH, MAINTENANCE_UPDATE_PREPARE_PATH,
    MAINTENANCE_UPDATE_STATUS_PATH, MaintenanceError, PrepareForUpdateResult, UpdateMaintenance,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[cfg(test)]
use crate::test_support::FixtureEvent;
use crate::{
    backend::{
        BackendRecoveryEntry, BackendRunConfig, BackendStartFailure, BackendSupervisor,
        BackendUpdateSnapshot, emit_project_data_status_changed,
    },
    config::{app_version, runtime_info},
};

const UPDATE_STATE_EVENT: &str = "desktop:update-state";
const STATUS_DISABLED: &str = "disabled";
const STATUS_IDLE: &str = "idle";
const STATUS_CHECKING: &str = "checking";
const STATUS_UP_TO_DATE: &str = "up-to-date";
const STATUS_AVAILABLE: &str = "available";
const STATUS_DOWNLOADING: &str = "downloading";
const STATUS_DOWNLOADED: &str = "downloaded";
const STATUS_ERROR: &str = "error";
const STARTUP_UPDATE_CHECK_DELAY: Duration = Duration::from_secs(15);
const BACKGROUND_UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);
const UPDATE_PROTECTION_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const UPDATE_PROTECTION_STATUS_TIMEOUT: Duration = Duration::from_secs(2);
const UPDATE_PROTECTION_PREPARE_TIMEOUT: Duration = Duration::from_secs(45);
const UPDATE_PROTECTION_FINISH_TIMEOUT: Duration = Duration::from_secs(10);

/// The primary calls its owner directly, including while shared on a wide bind.
/// WSL and other external backends retain the loopback HTTP maintenance API.
#[derive(Clone)]
pub(crate) enum UpdateProtectionTransport {
    Http,
    InProcess(Arc<UpdateMaintenance>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MaintenanceFinish {
    Commit,
    Cancel,
}

impl MaintenanceFinish {
    const fn path(self) -> &'static str {
        match self {
            Self::Commit => MAINTENANCE_UPDATE_COMMIT_PATH,
            Self::Cancel => MAINTENANCE_UPDATE_CANCEL_PATH,
        }
    }

    const fn step(self) -> MaintenanceStep {
        match self {
            Self::Commit => MaintenanceStep::Commit,
            Self::Cancel => MaintenanceStep::Cancel,
        }
    }
}

#[derive(Clone, Copy)]
enum MaintenanceStep {
    Prepare,
    Commit,
    Cancel,
}

impl MaintenanceStep {
    const fn timeout_detail(self) -> &'static str {
        match self {
            Self::Prepare => "timed out after 45 seconds",
            Self::Commit | Self::Cancel => "timed out after 10 seconds",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum UpdatePhase {
    #[default]
    Idle,
    Checking,
    Available,
    Protecting,
    Installing,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ProtectionStatus {
    Pending,
    Protected,
    Failed,
    Excluded,
    Skipped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProtectionStage {
    WaitingForMutations,
    QuiescingRuntime,
    AcquiringStoreLock,
    CheckpointingDatabase,
    CreatingVerifiedBackup,
    StoppingBackend,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMaintenanceProgress {
    #[serde(default)]
    pub(crate) stage: Option<ProtectionStage>,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    in_flight_mutations: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopUpdateProtection {
    environment_id: String,
    label: String,
    #[serde(skip)]
    primary: bool,
    status: ProtectionStatus,
    message: Option<String>,
    stage: Option<ProtectionStage>,
    elapsed_ms: Option<u64>,
    blocked_operation_count: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum BackendRecoveryReason {
    PortInUse,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopBackendRecovery {
    environment_id: String,
    label: String,
    reason: BackendRecoveryReason,
    port: u16,
}

impl From<BackendRecoveryEntry> for DesktopBackendRecovery {
    fn from(entry: BackendRecoveryEntry) -> Self {
        Self {
            environment_id: entry.environment_id,
            label: entry.label,
            reason: match entry.failure {
                BackendStartFailure::PortInUse { .. } => BackendRecoveryReason::PortInUse,
                BackendStartFailure::Other { .. } => BackendRecoveryReason::Other,
            },
            port: entry.port,
        }
    }
}

fn backend_recovery<R: Runtime>(app: &AppHandle<R>) -> Vec<BackendRecoveryEntry> {
    app.try_state::<BackendSupervisor>()
        .map(|backend| backend.backend_recovery())
        .unwrap_or_default()
}

fn apply_named_secondary_exclusions(
    protection: &mut [DesktopUpdateProtection],
    exclusions: &BTreeSet<String>,
) -> Result<(), String> {
    for entry in protection
        .iter_mut()
        .filter(|entry| entry.status == ProtectionStatus::Failed)
    {
        if entry.primary {
            return Err(format!(
                "The primary environment {} must be protected before installation.",
                entry.label
            ));
        }
        if exclusions.contains(&entry.environment_id) {
            entry.status = ProtectionStatus::Excluded;
        } else {
            return Err(format!(
                "The secondary environment {} must be protected or explicitly excluded by name.",
                entry.label
            ));
        }
    }
    Ok(())
}

fn protection_bypass_allowed(phase: UpdatePhase, protection: &[DesktopUpdateProtection]) -> bool {
    phase == UpdatePhase::Failed
        && protection
            .iter()
            .any(|entry| entry.status == ProtectionStatus::Failed)
}

struct DownloadedUpdate {
    update: Update,
    version: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopUpdateInstallInput {
    #[serde(default)]
    excluded_environment_ids: Vec<String>,
    #[serde(default)]
    skip_protection: bool,
}

struct PreparedBackend {
    config: BackendRunConfig,
    transport: UpdateProtectionTransport,
    operation_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteUpdateRequest {
    pub label: String,
    pub detail: Option<String>,
}

#[derive(Default)]
struct DesktopUpdateInner {
    available_update: Option<Update>,
    available_version: Option<String>,
    downloaded_update: Option<DownloadedUpdate>,
    downloaded_version: Option<String>,
    status: Option<String>,
    download_percent: Option<f64>,
    checked_at: Option<String>,
    message: Option<String>,
    error_context: Option<&'static str>,
    can_retry: bool,
    check_in_flight: bool,
    download_in_flight: bool,
    install_in_flight: bool,
    phase: UpdatePhase,
    protection: Vec<DesktopUpdateProtection>,
    requested_by: Option<RemoteUpdateRequest>,
}

#[cfg(test)]
struct InstallFailure {
    message: String,
    on_failure: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Default)]
pub struct DesktopUpdateManager {
    inner: Mutex<DesktopUpdateInner>,
    #[cfg(test)]
    check_attempts: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    panic_next_check: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    background_check_completion: FixtureEvent,
    #[cfg(test)]
    background_timer_armed: FixtureEvent,
    #[cfg(test)]
    install_failure: Mutex<Option<InstallFailure>>,
}

#[derive(Clone, Copy)]
enum UpdateOperation {
    Check,
    Download,
}

struct UpdateOperationGuard<'a, R: Runtime> {
    manager: &'a DesktopUpdateManager,
    app: AppHandle<R>,
    operation: UpdateOperation,
    prior_state: DesktopUpdateInner,
    armed: bool,
}

impl<R: Runtime> UpdateOperationGuard<'_, R> {
    fn finish(mut self, update: impl FnOnce(&mut DesktopUpdateInner)) -> DesktopUpdateInner {
        let state = {
            let mut inner = self.manager.lock_inner();
            update(&mut inner);
            self.operation.clear_in_flight(&mut inner);
            inner.clone_without_updates()
        };
        self.armed = false;
        state
    }
}

impl<R: Runtime> Drop for UpdateOperationGuard<'_, R> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let state = {
            let mut inner = self.manager.lock_inner();
            self.operation.clear_in_flight(&mut inner);
            inner.restore_visible_state(&self.prior_state);
            inner.clone_without_updates()
        };
        state.emit(&self.app);
    }
}

impl UpdateOperation {
    fn clear_in_flight(self, inner: &mut DesktopUpdateInner) {
        match self {
            Self::Check => inner.check_in_flight = false,
            Self::Download => inner.download_in_flight = false,
        }
    }
}

impl DesktopUpdateManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_inner(&self) -> MutexGuard<'_, DesktopUpdateInner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            let inner = poisoned.into_inner();
            self.inner.clear_poison();
            tracing::warn!(
                "desktop update state lock was poisoned by an earlier panic; keeping the last state"
            );
            inner
        })
    }

    pub(crate) fn install_in_flight(&self) -> bool {
        self.lock_inner().install_in_flight
    }

    pub(crate) fn emit_current_state<R: Runtime>(&self, app: &AppHandle<R>) {
        emit_update_state(app, &self.state(app));
    }

    pub fn state<R: Runtime>(&self, app: &AppHandle<R>) -> Value {
        let inner = self.lock_inner();
        match app.updater() {
            Ok(_) => update_state_value(app, true, &inner),
            Err(error) if is_updater_disabled(&error) => disabled_update_state(app),
            Err(error) => error_update_state(app, "check", error.to_string()),
        }
    }

    /// Records the first remote requester; later clients join without another notice.
    pub(crate) fn begin_remote_request<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        request: RemoteUpdateRequest,
    ) -> bool {
        let mut began = false;
        let state = self.replace_inner(|inner| {
            if inner.requested_by.is_none() {
                inner.requested_by = Some(request);
                began = true;
            }
        });
        if began {
            state.emit(app);
        }
        began
    }

    pub(crate) fn clear_remote_request<R: Runtime>(&self, app: &AppHandle<R>) {
        self.replace_inner(|inner| inner.requested_by = None)
            .emit(app);
    }

    pub(crate) fn note_remote_secondary_protection_failure<R: Runtime>(&self, app: &AppHandle<R>) {
        const FINISH_ON_HOST: &str = "Finish the update on the host.";
        let mut changed = false;
        let state = self.replace_inner(|inner| {
            if inner.phase != UpdatePhase::Failed
                || !inner
                    .protection
                    .iter()
                    .any(|entry| !entry.primary && entry.status == ProtectionStatus::Failed)
                || inner
                    .protection
                    .iter()
                    .any(|entry| entry.primary && entry.status == ProtectionStatus::Failed)
            {
                return;
            }
            let message = inner.message.get_or_insert_with(String::new);
            if message.ends_with(FINISH_ON_HOST) {
                return;
            }
            if !message.is_empty() {
                if !message.ends_with(['.', '!', '?']) {
                    message.push('.');
                }
                message.push(' ');
            }
            message.push_str(FINISH_ON_HOST);
            changed = true;
        });
        if changed {
            state.emit(app);
        }
    }

    pub async fn check_for_update<R: Runtime>(&self, app: AppHandle<R>) -> Value {
        #[cfg(test)]
        {
            self.check_attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(
                !self
                    .panic_next_check
                    .swap(false, std::sync::atomic::Ordering::SeqCst),
                "synthetic panic for background_scheduler_survives_a_panicking_check"
            );
        }

        let busy_state = {
            let inner = self.lock_inner();
            (!can_begin_check(&app, &inner)).then(|| update_state_value(&app, true, &inner))
        };
        if let Some(state) = busy_state {
            return json!({
                "checked": false,
                "state": state,
            });
        }

        let updater = match app.updater() {
            Ok(updater) => updater,
            Err(error) if is_updater_disabled(&error) => {
                return disabled_update_check_result(disabled_update_state(&app));
            }
            Err(error) => {
                let state = self.record_error_state(&app, "check", error.to_string());
                return json!({
                    "checked": false,
                    "state": state,
                });
            }
        };

        let (check_guard, state) = match self.begin_check(&app) {
            Some(admission) => admission,
            None => {
                return json!({
                    "checked": false,
                    "state": self.current_state(&app),
                });
            }
        };
        state.emit(&app);

        match updater.check().await {
            Ok(Some(update)) => {
                let version = update.version.clone();
                let checked_at = now_rfc3339();
                let state = check_guard.finish(|inner| {
                    inner.available_version = Some(version.clone());
                    inner.available_update = Some(update);
                    inner.downloaded_update = None;
                    inner.downloaded_version = None;
                    inner.download_percent = None;
                    inner.status = Some(STATUS_AVAILABLE.to_string());
                    inner.checked_at = Some(checked_at);
                    inner.message = None;
                    inner.error_context = None;
                    inner.can_retry = false;
                    inner.phase = UpdatePhase::Available;
                    inner.protection.clear();
                });
                let update_state = state.emit(&app);
                json!({
                    "checked": true,
                    "state": update_state,
                })
            }
            Ok(None) => {
                let checked_at = now_rfc3339();
                let state = check_guard.finish(|inner| {
                    inner.available_update = None;
                    inner.available_version = None;
                    inner.downloaded_update = None;
                    inner.downloaded_version = None;
                    inner.download_percent = None;
                    inner.status = Some(STATUS_UP_TO_DATE.to_string());
                    inner.checked_at = Some(checked_at);
                    inner.message = None;
                    inner.error_context = None;
                    inner.can_retry = false;
                    inner.phase = UpdatePhase::Idle;
                    inner.protection.clear();
                });
                let update_state = state.emit(&app);
                json!({
                    "checked": true,
                    "state": update_state,
                })
            }
            Err(error) => {
                let message = error.to_string();
                let state = check_guard.finish(|inner| {
                    inner.status = Some(STATUS_ERROR.to_string());
                    inner.message = Some(message);
                    inner.error_context = Some("check");
                    inner.can_retry = true;
                    inner.phase = UpdatePhase::Failed;
                });
                let state = state.emit(&app);
                json!({
                    "checked": false,
                    "state": state,
                })
            }
        }
    }

    pub async fn download_update<R: Runtime>(&self, app: AppHandle<R>) -> Value {
        let busy_state = {
            let inner = self.lock_inner();
            (!can_begin_download(&app, &inner)).then(|| update_state_value(&app, true, &inner))
        };
        if let Some(state) = busy_state {
            return json!({
                "accepted": false,
                "completed": false,
                "state": state,
            });
        }

        if let Err(error) = app.updater() {
            if is_updater_disabled(&error) {
                return disabled_update_action_result(disabled_update_state(&app));
            }
            let state = self.record_error_state(&app, "download", error.to_string());
            return json!({
                "accepted": false,
                "completed": false,
                "state": state,
            });
        }

        let (update, download_guard, state) = {
            let mut inner = self.lock_inner();
            if !can_begin_download(&app, &inner) {
                return json!({
                    "accepted": false,
                    "completed": false,
                    "state": update_state_value(&app, true, &inner),
                });
            }
            let Some(update) = inner.available_update.clone() else {
                drop(inner);
                let state = self.record_error_state(
                    &app,
                    "download",
                    "No checked update is available to download.".to_string(),
                );
                return json!({
                    "accepted": false,
                    "completed": false,
                    "state": state,
                });
            };
            let prior_state = inner.clone_without_updates();
            inner.download_in_flight = true;
            inner.status = Some(STATUS_DOWNLOADING.to_string());
            inner.download_percent = Some(0.0);
            inner.message = None;
            inner.error_context = None;
            inner.can_retry = false;
            inner.phase = UpdatePhase::Available;
            inner.protection.clear();
            let state = inner.clone_without_updates();
            let guard = UpdateOperationGuard {
                manager: self,
                app: app.clone(),
                operation: UpdateOperation::Download,
                prior_state,
                armed: true,
            };
            (update, guard, state)
        };

        let version = update.version.clone();
        state.emit(&app);
        let mut downloaded_bytes = 0_u64;
        let progress_app = app.clone();
        let bytes = update
            .download(
                |chunk_length, content_length| {
                    downloaded_bytes = downloaded_bytes.saturating_add(chunk_length as u64);
                    if let Some(total_bytes) = content_length.filter(|value| *value > 0) {
                        let percent = ((downloaded_bytes as f64 / total_bytes as f64) * 100.0)
                            .clamp(0.0, 100.0);
                        self.replace_inner(|inner| {
                            inner.download_percent = Some(percent);
                        })
                        .emit(&progress_app);
                    }
                },
                || {},
            )
            .await;

        match bytes {
            Ok(bytes) => {
                let state = download_guard.finish(|inner| {
                    inner.downloaded_update = Some(DownloadedUpdate {
                        update,
                        version: version.clone(),
                        bytes,
                    });
                    inner.downloaded_version = Some(version);
                    inner.status = Some(STATUS_DOWNLOADED.to_string());
                    inner.download_percent = Some(100.0);
                    inner.message = None;
                    inner.error_context = None;
                    inner.can_retry = false;
                    inner.phase = UpdatePhase::Available;
                });
                let update_state = state.emit(&app);
                json!({
                    "accepted": true,
                    "completed": true,
                    "state": update_state,
                })
            }
            Err(error) => {
                let message = error.to_string();
                let state = download_guard.finish(|inner| {
                    inner.status = Some(STATUS_ERROR.to_string());
                    inner.message = Some(message);
                    inner.error_context = Some("download");
                    inner.can_retry = true;
                    inner.phase = UpdatePhase::Failed;
                });
                let state = state.emit(&app);
                json!({
                    "accepted": true,
                    "completed": false,
                    "state": state,
                })
            }
        }
    }

    pub async fn install_update<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        backend: &BackendSupervisor,
        input: DesktopUpdateInstallInput,
    ) -> Value {
        let unavailable_state = {
            let mut inner = self.lock_inner();
            if !backend.backend_recovery().is_empty() {
                let mut state = update_state_value(app, true, &inner);
                state["message"] = Value::String(
                    "Restart the server before retrying the installation.".to_owned(),
                );
                Some(state)
            } else if inner.install_in_flight {
                Some(update_state_value(app, true, &inner))
            } else if input.skip_protection
                && !protection_bypass_allowed(inner.phase, &inner.protection)
            {
                let mut state = update_state_value(app, true, &inner);
                state["message"] = Value::String(
                    "Backup protection can be skipped only after a protection attempt fails."
                        .to_owned(),
                );
                Some(state)
            } else {
                inner.install_in_flight = true;
                None
            }
        };
        if let Some(state) = unavailable_state {
            return json!({
                "accepted": false,
                "completed": false,
                "state": state,
            });
        }

        if let Err(error) = app.updater() {
            if is_updater_disabled(&error) {
                self.replace_inner(|inner| inner.install_in_flight = false);
                return disabled_update_action_result(disabled_update_state(app));
            }
            self.replace_inner(|inner| inner.install_in_flight = false);
            let state = self.record_error_state(app, "install", error.to_string());
            return json!({
                "accepted": false,
                "completed": false,
                "state": state,
            });
        }

        let downloaded = self.lock_inner().downloaded_update.take();
        let Some(downloaded) = downloaded else {
            self.replace_inner(|inner| inner.install_in_flight = false);
            let state = self.record_error_state(
                app,
                "install",
                "No downloaded update is available to install.".to_string(),
            );
            return json!({
                "accepted": false,
                "completed": false,
                "state": state,
            });
        };
        let snapshot = match backend.begin_update_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return self.finish_failed_install(app, downloaded, Vec::new(), error);
            }
        };
        let mut protection = snapshot
            .environments
            .iter()
            .map(|environment| DesktopUpdateProtection {
                environment_id: environment.environment_id.clone(),
                label: environment.label.clone(),
                primary: environment.primary,
                status: if environment.running {
                    ProtectionStatus::Pending
                } else {
                    ProtectionStatus::Failed
                },
                message: environment.unprotected_reason.clone(),
                stage: environment
                    .running
                    .then_some(ProtectionStage::WaitingForMutations),
                elapsed_ms: None,
                blocked_operation_count: None,
            })
            .collect::<Vec<_>>();
        if !protection.iter().any(|entry| entry.primary) {
            protection.insert(
                0,
                DesktopUpdateProtection {
                    environment_id: "primary".to_string(),
                    label: "Local".to_string(),
                    primary: true,
                    status: ProtectionStatus::Failed,
                    message: Some("The primary desktop backend is not running.".to_string()),
                    stage: None,
                    elapsed_ms: None,
                    blocked_operation_count: None,
                },
            );
        }
        self.replace_inner(|inner| {
            inner.phase = UpdatePhase::Protecting;
            inner.protection = protection.clone();
        });

        self.replace_inner(|inner| {
            inner.message = None;
            inner.error_context = None;
            inner.can_retry = false;
        })
        .emit(app);

        let exclusions = input
            .excluded_environment_ids
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut prepared = Vec::new();
        if input.skip_protection {
            for entry in &mut protection {
                entry.status = ProtectionStatus::Skipped;
                entry.message = Some("Backup protection was skipped for this update.".to_owned());
                entry.stage = Some(ProtectionStage::StoppingBackend);
                entry.elapsed_ms = None;
                entry.blocked_operation_count = None;
            }
        } else {
            for environment in &snapshot.environments {
                if !environment.running {
                    continue;
                }
                let Some(config) = environment.config.clone() else {
                    set_protection_failure(
                        &mut protection,
                        &environment.environment_id,
                        "The running backend has no launch configuration.".to_string(),
                    );
                    continue;
                };
                if !environment.primary && exclusions.contains(&environment.environment_id) {
                    set_protection_failure(
                        &mut protection,
                        &environment.environment_id,
                        "This secondary environment was explicitly excluded.".to_string(),
                    );
                    continue;
                }
                let environment_id = environment.environment_id.clone();
                let transport = if environment.primary {
                    snapshot.in_process_maintenance(&environment_id).map_or(
                        UpdateProtectionTransport::Http,
                        UpdateProtectionTransport::InProcess,
                    )
                } else {
                    UpdateProtectionTransport::Http
                };
                match prepare_backend_for_update(&config, &transport, |progress| {
                    if set_protection_progress(&mut protection, &environment_id, &progress) {
                        self.replace_inner(|inner| inner.protection = protection.clone())
                            .emit(app);
                    }
                })
                .await
                {
                    Ok(result) => {
                        set_protection_status(
                            &mut protection,
                            &environment.environment_id,
                            ProtectionStatus::Protected,
                            None,
                        );
                        prepared.push(PreparedBackend {
                            config,
                            transport,
                            operation_id: result.operation_id,
                        });
                    }
                    Err(error) => {
                        set_protection_failure(&mut protection, &environment.environment_id, error);
                    }
                }
                self.replace_inner(|inner| inner.protection = protection.clone())
                    .emit(app);
            }

            if let Err(error) = apply_named_secondary_exclusions(&mut protection, &exclusions) {
                let recovery_error = cancel_stop_and_restart(backend, &snapshot, &prepared)
                    .await
                    .err();
                let message = append_recovery_error(error, recovery_error);
                return self.finish_failed_install(app, downloaded, protection, message);
            }
        }

        self.replace_inner(|inner| inner.protection = protection.clone())
            .emit(app);
        backend.expect_update_snapshot_exit(&snapshot);
        let mut commit_error = None;
        if !input.skip_protection {
            for operation in &prepared {
                if let Err(error) = finish_backend_update(
                    &operation.config,
                    &operation.transport,
                    MaintenanceFinish::Commit,
                    &operation.operation_id,
                )
                .await
                    && commit_error.is_none()
                {
                    commit_error = Some(error);
                }
            }
        }

        if let Some(error) = commit_error {
            let recovery_error = stop_and_restart(backend, &snapshot).await.err();
            let message = append_recovery_error(error, recovery_error);
            return self.finish_failed_install(app, downloaded, protection, message);
        }
        if let Err(error) = backend.stop_update_snapshot(&snapshot).await {
            let recovery_error = backend
                .restart_update_snapshot(&snapshot)
                .await
                .err()
                .map(|error| error.to_string());
            let message = append_recovery_error(error, recovery_error);
            return self.finish_failed_install(app, downloaded, protection, message);
        }

        self.replace_inner(|inner| {
            inner.phase = UpdatePhase::Installing;
            inner.protection = protection.clone();
        })
        .emit(app);

        match self.install_downloaded(&downloaded) {
            Ok(()) => {
                let state = self.replace_inner(|inner| {
                    inner.status = Some(STATUS_DOWNLOADED.to_string());
                    inner.downloaded_version = Some(downloaded.version);
                    inner.download_percent = Some(100.0);
                    inner.message = None;
                    inner.error_context = None;
                    inner.can_retry = false;
                    inner.install_in_flight = false;
                    inner.phase = UpdatePhase::Installing;
                    inner.protection = protection;
                });
                let update_state = state.emit(app);
                let result = json!({
                    "accepted": true,
                    "completed": true,
                    "state": update_state,
                });
                if restart_required_after_install(std::env::consts::OS) {
                    // A main-thread restart skips RunEvent::Exit.
                    crate::relaunch::prepare_descriptors_for_relaunch();
                    app.restart();
                }
                result
            }
            Err(error) => {
                let recovery_error = backend
                    .restart_update_snapshot(&snapshot)
                    .await
                    .err()
                    .map(|error| error.to_string());
                let message = append_recovery_error(error, recovery_error);
                self.finish_failed_install(app, downloaded, protection, message)
            }
        }
    }

    fn install_downloaded(&self, downloaded: &DownloadedUpdate) -> Result<(), String> {
        #[cfg(test)]
        if let Some(error) = self
            .install_failure
            .lock()
            .expect("desktop update install failure mutex poisoned")
            .take()
        {
            if let Some(on_failure) = error.on_failure {
                on_failure();
            }
            return Err(error.message);
        }
        crate::windows_update_restart::prepare_restart_handoff();
        downloaded
            .update
            .install(&downloaded.bytes)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn fail_next_install(&self, error: impl Into<String>) {
        *self
            .install_failure
            .lock()
            .expect("desktop update install failure mutex poisoned") = Some(InstallFailure {
            message: error.into(),
            on_failure: None,
        });
    }

    fn finish_failed_install<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        downloaded: DownloadedUpdate,
        protection: Vec<DesktopUpdateProtection>,
        message: String,
    ) -> Value {
        let state = self.replace_inner(|inner| {
            inner.downloaded_version = Some(downloaded.version.clone());
            inner.downloaded_update = Some(downloaded);
            inner.download_percent = Some(100.0);
            inner.status = Some(STATUS_ERROR.to_string());
            inner.message = Some(message);
            inner.error_context = Some("install");
            inner.can_retry = true;
            inner.install_in_flight = false;
            inner.phase = UpdatePhase::Failed;
            inner.protection = protection;
        });
        let update_state = state.emit(app);
        for entry in backend_recovery(app) {
            emit_project_data_status_changed(app, &entry.environment_id);
        }
        json!({
            "accepted": true,
            "completed": false,
            "state": update_state,
        })
    }

    fn replace_inner(&self, update: impl FnOnce(&mut DesktopUpdateInner)) -> DesktopUpdateInner {
        let mut inner = self.lock_inner();
        update(&mut inner);
        inner.clone_without_updates()
    }

    fn begin_check<R: Runtime>(
        &self,
        app: &AppHandle<R>,
    ) -> Option<(UpdateOperationGuard<'_, R>, DesktopUpdateInner)> {
        let mut inner = self.lock_inner();
        if !can_begin_check(app, &inner) {
            return None;
        }
        let prior_state = inner.clone_without_updates();
        inner.check_in_flight = true;
        inner.status = Some(STATUS_CHECKING.to_string());
        inner.message = None;
        inner.error_context = None;
        inner.can_retry = false;
        inner.phase = UpdatePhase::Checking;
        inner.protection.clear();
        let state = inner.clone_without_updates();
        Some((
            UpdateOperationGuard {
                manager: self,
                app: app.clone(),
                operation: UpdateOperation::Check,
                prior_state,
                armed: true,
            },
            state,
        ))
    }

    fn current_state<R: Runtime>(&self, app: &AppHandle<R>) -> Value {
        let inner = self.lock_inner();
        update_state_value(app, true, &inner)
    }

    fn record_error_state<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        context: &'static str,
        message: String,
    ) -> Value {
        let state = self.replace_inner(|inner| {
            inner.status = Some(STATUS_ERROR.to_string());
            inner.message = Some(message);
            inner.error_context = Some(context);
            inner.can_retry = true;
            inner.phase = UpdatePhase::Failed;
        });
        state.emit(app)
    }
}

impl DesktopUpdateInner {
    fn emit<R: Runtime>(&self, app: &AppHandle<R>) -> Value {
        let state = update_state_value(app, true, self);
        emit_update_state(app, &state);
        state
    }

    fn clone_without_updates(&self) -> DesktopUpdateInner {
        DesktopUpdateInner {
            available_update: None,
            available_version: self.available_version.clone(),
            downloaded_update: None,
            downloaded_version: self.downloaded_version.clone(),
            status: self.status.clone(),
            download_percent: self.download_percent,
            checked_at: self.checked_at.clone(),
            message: self.message.clone(),
            error_context: self.error_context,
            can_retry: self.can_retry,
            check_in_flight: false,
            download_in_flight: false,
            install_in_flight: false,
            phase: self.phase,
            protection: self.protection.clone(),
            requested_by: self.requested_by.clone(),
        }
    }

    fn restore_visible_state(&mut self, prior_state: &DesktopUpdateInner) {
        // Only begin_remote_request and clear_remote_request change requested_by.
        self.available_version = prior_state.available_version.clone();
        self.downloaded_version = prior_state.downloaded_version.clone();
        self.status = prior_state.status.clone();
        self.download_percent = prior_state.download_percent;
        self.checked_at = prior_state.checked_at.clone();
        self.message = prior_state.message.clone();
        self.error_context = prior_state.error_context;
        self.can_retry = prior_state.can_retry;
        self.phase = prior_state.phase;
        self.protection = prior_state.protection.clone();
    }
}

fn can_begin_check<R: Runtime>(app: &AppHandle<R>, inner: &DesktopUpdateInner) -> bool {
    backend_recovery(app).is_empty()
        && !inner.check_in_flight
        && !inner.download_in_flight
        && !inner.install_in_flight
        && !matches!(
            inner.status.as_deref(),
            Some(STATUS_DOWNLOADING | STATUS_DOWNLOADED)
        )
}

fn can_begin_download<R: Runtime>(app: &AppHandle<R>, inner: &DesktopUpdateInner) -> bool {
    backend_recovery(app).is_empty()
        && !inner.check_in_flight
        && !inner.download_in_flight
        && !inner.install_in_flight
        && !matches!(
            inner.status.as_deref(),
            Some(STATUS_DOWNLOADING | STATUS_DOWNLOADED)
        )
}

fn set_protection_status(
    protection: &mut [DesktopUpdateProtection],
    environment_id: &str,
    status: ProtectionStatus,
    message: Option<String>,
) {
    if let Some(entry) = protection
        .iter_mut()
        .find(|entry| entry.environment_id == environment_id)
    {
        entry.status = status;
        entry.message = message;
    }
}

fn set_protection_failure(
    protection: &mut [DesktopUpdateProtection],
    environment_id: &str,
    message: String,
) {
    set_protection_status(
        protection,
        environment_id,
        ProtectionStatus::Failed,
        Some(message),
    );
}

fn set_protection_progress(
    protection: &mut [DesktopUpdateProtection],
    environment_id: &str,
    progress: &UpdateMaintenanceProgress,
) -> bool {
    let Some(entry) = protection
        .iter_mut()
        .find(|entry| entry.environment_id == environment_id)
    else {
        return false;
    };
    let changed = entry.stage != progress.stage
        || entry.elapsed_ms != progress.elapsed_ms
        || entry.blocked_operation_count != progress.in_flight_mutations;
    entry.stage = progress.stage;
    entry.elapsed_ms = progress.elapsed_ms;
    entry.blocked_operation_count = progress.in_flight_mutations;
    changed
}

pub(crate) async fn prepare_backend_for_update(
    config: &BackendRunConfig,
    transport: &UpdateProtectionTransport,
    on_progress: impl FnMut(UpdateMaintenanceProgress),
) -> Result<PrepareForUpdateResult, String> {
    prepare_backend_for_update_with_timeout(
        config,
        transport,
        UPDATE_PROTECTION_PREPARE_TIMEOUT,
        on_progress,
    )
    .await
}

pub(crate) async fn prepare_backend_for_update_with_timeout(
    config: &BackendRunConfig,
    transport: &UpdateProtectionTransport,
    timeout: Duration,
    on_progress: impl FnMut(UpdateMaintenanceProgress),
) -> Result<PrepareForUpdateResult, String> {
    tokio::time::timeout(timeout, async {
        match transport {
            UpdateProtectionTransport::Http => prepare_backend_over_http(config, on_progress).await,
            UpdateProtectionTransport::InProcess(maintenance) => {
                prepare_backend_in_process(config, maintenance, on_progress).await
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(maintenance_failure_text(
            config,
            MaintenanceStep::Prepare,
            MaintenanceStep::Prepare.timeout_detail(),
        ))
    })
}

async fn prepare_backend_in_process(
    config: &BackendRunConfig,
    maintenance: &Arc<UpdateMaintenance>,
    mut on_progress: impl FnMut(UpdateMaintenanceProgress),
) -> Result<PrepareForUpdateResult, String> {
    // A caller timeout must not strand the owner in Preparing. Dropping this
    // handle detaches preparation so it can settle and retain its exit guarantees.
    let mut prepare = tokio::spawn({
        let maintenance = maintenance.clone();
        async move { maintenance.prepare().await }
    });
    let first_progress_poll = tokio::time::Instant::now() + UPDATE_PROTECTION_PROGRESS_INTERVAL;
    let mut progress_interval =
        tokio::time::interval_at(first_progress_poll, UPDATE_PROTECTION_PROGRESS_INTERVAL);
    progress_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            result = &mut prepare => {
                return result.map_err(|_| {
                    maintenance_failure_text(
                        config,
                        MaintenanceStep::Prepare,
                        "the preparation task stopped unexpectedly",
                    )
                })?.map_err(|error| {
                    maintenance_failure_text(config, MaintenanceStep::Prepare, &error.to_string())
                });
            }
            _ = progress_interval.tick() => {
                if let Ok(status) =
                    tokio::time::timeout(UPDATE_PROTECTION_STATUS_TIMEOUT, maintenance.status()).await
                    && let Ok(progress) = serde_json::from_value::<UpdateMaintenanceProgress>(status)
                    && progress.stage.is_some()
                {
                    on_progress(progress);
                }
            }
        }
    }
}

async fn prepare_backend_over_http(
    config: &BackendRunConfig,
    mut on_progress: impl FnMut(UpdateMaintenanceProgress),
) -> Result<PrepareForUpdateResult, String> {
    let client = reqwest::Client::new();
    let prepare = client
        .post(format!(
            "{}{}",
            config.http_base_url(),
            MAINTENANCE_UPDATE_PREPARE_PATH
        ))
        .header(
            DESKTOP_MAINTENANCE_TOKEN_HEADER,
            &config.desktop_bootstrap_token,
        )
        .timeout(UPDATE_PROTECTION_PREPARE_TIMEOUT)
        .send();
    tokio::pin!(prepare);
    let first_progress_poll = tokio::time::Instant::now() + UPDATE_PROTECTION_PROGRESS_INTERVAL;
    let mut progress_interval =
        tokio::time::interval_at(first_progress_poll, UPDATE_PROTECTION_PROGRESS_INTERVAL);
    progress_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            response = &mut prepare => {
                let response = response.map_err(|error| {
                    maintenance_transport_error(config, MaintenanceStep::Prepare, &error)
                })?;
                return decode_maintenance_response(config, MaintenanceStep::Prepare, response).await;
            }
            _ = progress_interval.tick() => {
                if let Ok(progress) = fetch_update_maintenance_progress(&client, config).await
                    && progress.stage.is_some()
                {
                    on_progress(progress);
                }
            }
        }
    }
}

async fn fetch_update_maintenance_progress(
    client: &reqwest::Client,
    config: &BackendRunConfig,
) -> Result<UpdateMaintenanceProgress, String> {
    let response = client
        .get(format!(
            "{}{}",
            config.http_base_url(),
            MAINTENANCE_UPDATE_STATUS_PATH
        ))
        .header(
            DESKTOP_MAINTENANCE_TOKEN_HEADER,
            &config.desktop_bootstrap_token,
        )
        .timeout(UPDATE_PROTECTION_STATUS_TIMEOUT)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    response.json().await.map_err(|error| error.to_string())
}

pub(crate) async fn finish_backend_update(
    config: &BackendRunConfig,
    transport: &UpdateProtectionTransport,
    finish: MaintenanceFinish,
    operation_id: &str,
) -> Result<(), String> {
    let step = finish.step();
    tokio::time::timeout(UPDATE_PROTECTION_FINISH_TIMEOUT, async {
        match transport {
            UpdateProtectionTransport::Http => {
                finish_backend_update_over_http(config, finish, operation_id).await
            }
            UpdateProtectionTransport::InProcess(maintenance) => {
                let operation_id = uuid::Uuid::parse_str(operation_id).map_err(|_| {
                    maintenance_failure_text(
                        config,
                        step,
                        &MaintenanceError::OperationMismatch.to_string(),
                    )
                })?;
                let result = match finish {
                    MaintenanceFinish::Commit => maintenance.commit(operation_id).await,
                    MaintenanceFinish::Cancel => maintenance.cancel(operation_id).await,
                };
                result
                    .map_err(|error| maintenance_failure_text(config, step, &error.to_string()))?;
                // Match the HTTP route: a quiesced backend exits after either finish.
                maintenance.shutdown_after_response();
                Ok(())
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(maintenance_failure_text(
            config,
            step,
            step.timeout_detail(),
        ))
    })
}

async fn finish_backend_update_over_http(
    config: &BackendRunConfig,
    finish: MaintenanceFinish,
    operation_id: &str,
) -> Result<(), String> {
    let response = reqwest::Client::new()
        .post(format!("{}{}", config.http_base_url(), finish.path()))
        .header(
            DESKTOP_MAINTENANCE_TOKEN_HEADER,
            &config.desktop_bootstrap_token,
        )
        .json(&json!({ "operationId": operation_id }))
        .timeout(UPDATE_PROTECTION_FINISH_TIMEOUT)
        .send()
        .await
        .map_err(|error| maintenance_transport_error(config, finish.step(), &error))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(maintenance_response_error(config, finish.step(), response).await)
    }
}

async fn decode_maintenance_response<T: serde::de::DeserializeOwned>(
    config: &BackendRunConfig,
    step: MaintenanceStep,
    response: reqwest::Response,
) -> Result<T, String> {
    if !response.status().is_success() {
        return Err(maintenance_response_error(config, step, response).await);
    }
    response.json::<T>().await.map_err(|error| {
        if error.is_timeout() {
            maintenance_transport_error(config, step, &error)
        } else {
            format!(
                "Could not decode the update protection result for {}: {error}",
                config.label
            )
        }
    })
}

async fn maintenance_response_error(
    config: &BackendRunConfig,
    step: MaintenanceStep,
    response: reqwest::Response,
) -> String {
    let fallback = if response.status() == reqwest::StatusCode::NOT_FOUND {
        "update maintenance is not available"
    } else {
        "the server gave no reason"
    };
    match response.json::<UpdateMaintenanceErrorResponse>().await {
        Ok(error) if !error.message.trim().is_empty() => {
            maintenance_failure_text(config, step, &error.message)
        }
        Err(error) if error.is_timeout() => maintenance_transport_error(config, step, &error),
        _ => maintenance_failure_text(config, step, fallback),
    }
}

fn maintenance_transport_error(
    config: &BackendRunConfig,
    step: MaintenanceStep,
    error: &reqwest::Error,
) -> String {
    if error.is_timeout() {
        maintenance_failure_text(config, step, step.timeout_detail())
    } else {
        maintenance_failure_text(config, step, &error.to_string())
    }
}

fn maintenance_failure_text(
    config: &BackendRunConfig,
    step: MaintenanceStep,
    detail: &str,
) -> String {
    let detail = bounded_maintenance_error_detail(detail);
    let punctuation = if detail.ends_with(['.', '!', '?']) {
        ""
    } else {
        "."
    };
    let label = &config.label;
    match step {
        MaintenanceStep::Prepare => {
            format!("Could not prepare {label} for update protection: {detail}{punctuation}")
        }
        MaintenanceStep::Commit => {
            format!("Could not finish update protection for {label}: {detail}{punctuation}")
        }
        MaintenanceStep::Cancel => {
            format!("Could not cancel update protection for {label}: {detail}{punctuation}")
        }
    }
}

#[derive(Deserialize)]
struct UpdateMaintenanceErrorResponse {
    message: String,
}

fn bounded_maintenance_error_detail(message: &str) -> String {
    const MAX_CHARS: usize = 320;

    let single_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_CHARS {
        return single_line;
    }
    let mut bounded = single_line.chars().take(MAX_CHARS).collect::<String>();
    bounded.push_str("...");
    bounded
}

async fn cancel_stop_and_restart(
    backend: &BackendSupervisor,
    snapshot: &BackendUpdateSnapshot,
    prepared: &[PreparedBackend],
) -> Result<(), String> {
    let mut errors = Vec::new();
    backend.expect_update_snapshot_exit(snapshot);
    for operation in prepared {
        if let Err(error) = finish_backend_update(
            &operation.config,
            &operation.transport,
            MaintenanceFinish::Cancel,
            &operation.operation_id,
        )
        .await
        {
            errors.push(error);
        }
    }
    if let Err(error) = stop_and_restart(backend, snapshot).await {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn stop_and_restart(
    backend: &BackendSupervisor,
    snapshot: &BackendUpdateSnapshot,
) -> Result<(), String> {
    let stop = backend.stop_update_snapshot(snapshot).await;
    let restart = backend.restart_update_snapshot(snapshot).await;
    match (stop, restart) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(stop), Ok(())) => Err(stop),
        (Ok(()), Err(restart)) => Err(restart.to_string()),
        (Err(stop), Err(restart)) => Err(format!("{stop}; {restart}")),
    }
}

fn append_recovery_error(message: String, recovery_error: Option<String>) -> String {
    match recovery_error {
        Some(recovery_error) => {
            format!("{message} Desktop backend recovery also failed: {recovery_error}")
        }
        None => message,
    }
}

pub async fn run_background_update_checks<R: Runtime>(app: AppHandle<R>) {
    let startup_delay = tokio::time::sleep(STARTUP_UPDATE_CHECK_DELAY);
    #[cfg(test)]
    app.state::<DesktopUpdateManager>()
        .background_timer_armed
        .publish();
    startup_delay.await;
    loop {
        match run_isolated_background_check(app.clone()).await {
            Ok(result) if result["state"]["errorContext"] == "check" => {
                tracing::warn!(
                    "background update check failed: {}",
                    result["state"]["message"]
                        .as_str()
                        .unwrap_or("unknown error")
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!("background update check task failed: {error}");
            }
        }
        #[cfg(test)]
        app.state::<DesktopUpdateManager>()
            .background_check_completion
            .publish();
        let background_delay = tokio::time::sleep(BACKGROUND_UPDATE_CHECK_INTERVAL);
        #[cfg(test)]
        app.state::<DesktopUpdateManager>()
            .background_timer_armed
            .publish();
        background_delay.await;
    }
}

async fn run_isolated_background_check<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Value, tauri::Error> {
    tauri::async_runtime::spawn(async move {
        app.state::<DesktopUpdateManager>()
            .check_for_update(app.clone())
            .await
    })
    .await
}

fn is_updater_disabled(error: &UpdaterError) -> bool {
    matches!(error, UpdaterError::EmptyEndpoints)
}

fn restart_required_after_install(target_os: &str) -> bool {
    matches!(target_os, "linux" | "macos")
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn emit_update_state<R: Runtime>(app: &AppHandle<R>, state: &Value) {
    if let Err(error) = app.emit(UPDATE_STATE_EVENT, state.clone()) {
        tracing::debug!("failed to emit Tauri update state event: {error}");
    }
}

pub fn disabled_update_state<R: Runtime>(app: &AppHandle<R>) -> Value {
    let runtime = runtime_info();
    json!({
        "enabled": false,
        "status": STATUS_DISABLED,
        "currentVersion": app_version(app),
        "hostArch": runtime["hostArch"].clone(),
        "appArch": runtime["appArch"].clone(),
        "runningUnderArm64Translation": runtime["runningUnderArm64Translation"].clone(),
        "availableVersion": null,
        "downloadedVersion": null,
        "downloadPercent": null,
        "checkedAt": null,
        "message": null,
        "errorContext": null,
        "canRetry": false,
        "phase": UpdatePhase::Idle,
        "protection": [],
        "backendRecovery": [],
        "requestedBy": null,
    })
}

fn error_update_state<R: Runtime>(
    app: &AppHandle<R>,
    context: &'static str,
    message: String,
) -> Value {
    let runtime = runtime_info();
    json!({
        "enabled": true,
        "status": STATUS_ERROR,
        "currentVersion": app_version(app),
        "hostArch": runtime["hostArch"].clone(),
        "appArch": runtime["appArch"].clone(),
        "runningUnderArm64Translation": runtime["runningUnderArm64Translation"].clone(),
        "availableVersion": null,
        "downloadedVersion": null,
        "downloadPercent": null,
        "checkedAt": null,
        "message": message,
        "errorContext": context,
        "canRetry": true,
        "phase": UpdatePhase::Failed,
        "protection": [],
        "backendRecovery": [],
        "requestedBy": null,
    })
}

fn update_state_value<R: Runtime>(
    app: &AppHandle<R>,
    enabled: bool,
    inner: &DesktopUpdateInner,
) -> Value {
    let runtime = runtime_info();
    json!({
        "enabled": enabled,
        "status": inner.status.as_deref().unwrap_or(STATUS_IDLE),
        "currentVersion": app_version(app),
        "hostArch": runtime["hostArch"].clone(),
        "appArch": runtime["appArch"].clone(),
        "runningUnderArm64Translation": runtime["runningUnderArm64Translation"].clone(),
        "availableVersion": inner.available_version,
        "downloadedVersion": inner.downloaded_version,
        "downloadPercent": inner.download_percent,
        "checkedAt": inner.checked_at,
        "message": inner.message,
        "errorContext": inner.error_context,
        "canRetry": inner.can_retry,
        "phase": inner.phase,
        "protection": inner.protection,
        "requestedBy": inner.requested_by,
        "backendRecovery": backend_recovery(app).into_iter().map(DesktopBackendRecovery::from).collect::<Vec<_>>(),
    })
}

pub fn disabled_update_check_result(state: Value) -> Value {
    json!({
        "checked": false,
        "state": state,
    })
}

pub fn disabled_update_action_result(state: Value) -> Value {
    json!({
        "accepted": false,
        "completed": false,
        "state": state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendLaunchPlan, BackendShutdownConfig, BackendUnavailableEnvironment};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };
    use tauri::{
        Manager,
        test::{MockRuntime, mock_builder, mock_context, noop_assets},
    };

    const TEST_PUBLIC_KEY: &str = "untrusted comment: minisign public key E7620F1842B4E81F\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=\ntrusted comment: timestamp:1555779966\tfile:test\nQtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

    #[test]
    fn refused_recovery_install_preserves_original_message() {
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_owned());
        let manager = DesktopUpdateManager::new();
        let root = tempfile::tempdir().expect("isolated data");
        let supervisor = pending_recovery_supervisor(root.path());
        app.manage(supervisor.clone());
        manager.replace_inner(|inner| {
            inner.message = Some("original installer and restart failure".to_owned());
        });
        for skip_protection in [false, true] {
            let result = tauri::async_runtime::block_on(manager.install_update(
                app.handle(),
                &supervisor,
                DesktopUpdateInstallInput {
                    skip_protection,
                    ..Default::default()
                },
            ));
            assert_eq!(result["accepted"], false);
            assert_eq!(
                result["state"]["message"],
                "Restart the server before retrying the installation."
            );
            assert_eq!(
                manager.state(app.handle())["message"],
                "original installer and restart failure"
            );
        }
    }

    fn pending_recovery_supervisor(root: &std::path::Path) -> BackendSupervisor {
        crate::backend::recovery_test_supervisor(
            BackendLaunchPlan::local(
                root.to_path_buf(),
                BackendRunConfig {
                    port: 43117,
                    ..test_backend_config()
                },
            ),
            BackendStartFailure::PortInUse {
                port: 43118,
                detail: "held port".to_owned(),
            },
        )
    }

    #[tokio::test]
    async fn recovery_listener_reemits_supervisor_changes() {
        use tauri::Listener;
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_owned());
        let root = tempfile::tempdir().expect("isolated data");
        let supervisor = pending_recovery_supervisor(root.path());
        app.manage(supervisor.clone());
        let handle = app.handle().clone();
        supervisor.install_recovery_listener(Arc::new(move || {
            handle
                .state::<DesktopUpdateManager>()
                .emit_current_state(&handle);
        }));
        let (sender, receiver) = mpsc::channel();
        app.listen_any(UPDATE_STATE_EVENT, move |event| {
            sender
                .send(event.payload().to_owned())
                .expect("update event");
        });
        supervisor.record_error("held port");
        let state: Value =
            serde_json::from_str(&receiver.try_recv().expect("recovery event")).expect("JSON");
        assert_eq!(
            state["backendRecovery"],
            json!([{
                "environmentId": "primary", "label": "Local", "reason": "port-in-use", "port": 43118,
            }])
        );
        supervisor.record_error("unchanged classification");
        assert!(
            receiver.try_recv().is_err(),
            "unchanged recovery emits nothing"
        );
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("stop");
        let state: Value =
            serde_json::from_str(&receiver.try_recv().expect("cleared event")).expect("JSON");
        assert_eq!(state["backendRecovery"], json!([]));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn pending_backend_recovery_blocks_checks_downloads_and_all_install_attempts() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update endpoint");
        listener
            .set_nonblocking(true)
            .expect("nonblocking endpoint");
        let app = updater_test_app(format!(
            "http://{}/latest.json",
            listener.local_addr().expect("address")
        ));
        let manager = DesktopUpdateManager::new();
        let root = tempfile::tempdir().expect("isolated data");
        let supervisor = pending_recovery_supervisor(root.path());
        app.manage(supervisor.clone());
        manager.replace_inner(|inner| {
            inner.status = Some(STATUS_ERROR.to_owned());
            inner.phase = UpdatePhase::Failed;
            inner.message = Some("installer failed".to_owned());
        });
        let before = manager.current_state(app.handle());
        let check = manager.check_for_update(app.handle().clone()).await;
        assert_eq!(check["checked"], false);
        assert_eq!(check["state"], before);
        let download = manager.download_update(app.handle().clone()).await;
        assert_eq!(download["accepted"], false);
        assert_eq!(download["state"], before);
        assert_eq!(
            listener.accept().expect_err("no update request").kind(),
            std::io::ErrorKind::WouldBlock
        );
        for skip_protection in [false, true] {
            let result = manager
                .install_update(
                    app.handle(),
                    &supervisor,
                    DesktopUpdateInstallInput {
                        skip_protection,
                        excluded_environment_ids: vec![],
                    },
                )
                .await;
            assert_eq!(result["accepted"], false);
            assert_eq!(result["completed"], false);
            assert_eq!(
                result["state"]["message"],
                "Restart the server before retrying the installation."
            );
            assert_eq!(
                result["state"]["backendRecovery"],
                before["backendRecovery"]
            );
            assert!(!manager.install_in_flight());
            assert_eq!(manager.state(app.handle())["message"], "installer failed");
        }
    }

    #[tokio::test]
    async fn failed_install_publishes_typed_backend_recovery_and_status_invalidation() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::failed_install_publishes_typed_backend_recovery_and_status_invalidation",
        ) else {
            return;
        };
        use tauri::Listener;
        let (base_url, update_server) = spawn_update_server("test");
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = DesktopUpdateManager::new();
        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            true
        );
        assert_eq!(
            manager.download_update(app.handle().clone()).await["completed"],
            true
        );
        update_server.join().expect("update server finished");
        let root = tempfile::tempdir().expect("isolated data");
        let supervisor = BackendSupervisor::new();
        app.manage(supervisor.clone());
        let port = crate::test_support::free_test_port();
        supervisor
            .start(BackendLaunchPlan::local(
                root.path().to_path_buf(),
                BackendRunConfig {
                    port,
                    ..test_backend_config()
                },
            ))
            .await
            .expect("primary starts");
        let held_port = Arc::new(Mutex::new(None));
        let hook_holder = held_port.clone();
        manager.fail_next_install("synthetic installer failure");
        manager
            .install_failure
            .lock()
            .expect("failure fixture")
            .as_mut()
            .expect("failure")
            .on_failure = Some(Box::new(move || {
            *hook_holder.lock().expect("holder") =
                Some(TcpListener::bind(("127.0.0.1", port)).expect("stopped port can be held"));
        }));
        let (sender, receiver) = mpsc::channel();
        app.listen_any(
            crate::backend::PROJECT_DATA_STATUS_CHANGED_EVENT,
            move |event| {
                sender
                    .send(event.payload().to_owned())
                    .expect("status event");
            },
        );
        let result = manager
            .install_update(
                app.handle(),
                &supervisor,
                DesktopUpdateInstallInput::default(),
            )
            .await;
        assert_eq!(result["completed"], false);
        assert_eq!(
            result["state"]["backendRecovery"],
            json!([{
                "environmentId": "primary", "label": "Local", "reason": "port-in-use", "port": port,
            }])
        );
        let targets = supervisor.project_data_targets();
        assert_eq!(targets.len(), 1);
        assert!(!targets[0].running);
        assert!(result["state"]["message"].as_str().expect("message").starts_with(&format!(
            "synthetic installer failure Desktop backend recovery also failed: Could not restart every desktop backend: Local: Could not start in-process desktop backend: failed to bind the server listener on 127.0.0.1:{port}: "
        )));
        assert_eq!(
            serde_json::from_str::<Value>(&receiver.try_recv().expect("invalidation"))
                .expect("JSON"),
            json!({"environmentId":"primary"})
        );
        assert!(
            receiver.try_recv().is_err(),
            "one invalidation per failed environment"
        );
        let original_message = manager.state(app.handle())["message"].clone();
        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            false
        );
        assert_eq!(
            manager.download_update(app.handle().clone()).await["accepted"],
            false
        );
        for skip_protection in [false, true] {
            let refused = manager
                .install_update(
                    app.handle(),
                    &supervisor,
                    DesktopUpdateInstallInput {
                        skip_protection,
                        ..Default::default()
                    },
                )
                .await;
            assert_eq!(refused["accepted"], false);
            assert_eq!(manager.state(app.handle())["message"], original_message);
        }
        drop(held_port.lock().expect("holder").take());
        crate::data_safety::retry_project_data(&supervisor, "primary")
            .await
            .expect("restart slot");
        assert_eq!(manager.state(app.handle())["backendRecovery"], json!([]));
        // Reaching checking proves the supervisor restart alone released admission, with no settle call.
        let check = manager
            .begin_check(app.handle())
            .expect("recovery no longer blocks checks");
        assert_eq!(manager.current_state(app.handle())["phase"], "checking");
        drop(check);
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("cleanup");
        isolated.complete();
    }

    #[test]
    fn maintenance_error_details_are_single_line_and_bounded() {
        assert_eq!(
            bounded_maintenance_error_detail("backup\n  verification failed"),
            "backup verification failed"
        );
        let oversized = "x".repeat(400);
        let bounded = bounded_maintenance_error_detail(&oversized);
        assert_eq!(bounded.chars().count(), 323);
        assert!(bounded.ends_with("..."));
    }

    #[test]
    fn maintenance_failures_use_consistent_bounded_sentences() {
        let config = test_backend_config();
        for (step, detail, expected) in [
            (
                MaintenanceStep::Prepare,
                "backup\n verification failed",
                "Could not prepare Local for update protection: backup verification failed.",
            ),
            (
                MaintenanceStep::Commit,
                "no prepared operation.",
                "Could not finish update protection for Local: no prepared operation.",
            ),
            (
                MaintenanceStep::Cancel,
                "operation mismatch!",
                "Could not cancel update protection for Local: operation mismatch!",
            ),
            (
                MaintenanceStep::Cancel,
                "operation mismatch?",
                "Could not cancel update protection for Local: operation mismatch?",
            ),
        ] {
            assert_eq!(maintenance_failure_text(&config, step, detail), expected);
        }
        assert_eq!(
            maintenance_failure_text(&config, MaintenanceStep::Prepare, &"x".repeat(400)),
            format!(
                "Could not prepare Local for update protection: {}...",
                "x".repeat(320)
            )
        );
    }

    #[tokio::test]
    async fn http_maintenance_failures_report_messages_or_readable_fallbacks() {
        for (step, status, body, expected) in [
            (
                MaintenanceStep::Prepare,
                "409 Conflict",
                r#"{"message":"backup\n verification failed"}"#,
                "Could not prepare Local for update protection: backup verification failed.",
            ),
            (
                MaintenanceStep::Prepare,
                "404 Not Found",
                "Not Found",
                "Could not prepare Local for update protection: update maintenance is not available.",
            ),
            (
                MaintenanceStep::Commit,
                "500 Internal Server Error",
                "unreadable body",
                "Could not finish update protection for Local: the server gave no reason.",
            ),
            (
                MaintenanceStep::Cancel,
                "409 Conflict",
                r#"{"message":"operation mismatch!"}"#,
                "Could not cancel update protection for Local: operation mismatch!",
            ),
            (
                MaintenanceStep::Cancel,
                "404 Not Found",
                r#"{"message":"  "}"#,
                "Could not cancel update protection for Local: update maintenance is not available.",
            ),
        ] {
            let listener =
                TcpListener::bind("127.0.0.1:0").expect("maintenance server should bind");
            let mut config = test_backend_config();
            config.port = listener.local_addr().expect("maintenance address").port();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("maintenance request");
                assert_request_read(&mut stream, "maintenance request should read");
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("maintenance response should write");
            });
            let result = match step {
                MaintenanceStep::Prepare => {
                    prepare_backend_for_update(&config, &UpdateProtectionTransport::Http, |_| {})
                        .await
                        .map(|_| ())
                }
                MaintenanceStep::Commit | MaintenanceStep::Cancel => {
                    let finish = match step {
                        MaintenanceStep::Commit => MaintenanceFinish::Commit,
                        _ => MaintenanceFinish::Cancel,
                    };
                    finish_backend_update(
                        &config,
                        &UpdateProtectionTransport::Http,
                        finish,
                        "00000000-0000-4000-8000-000000000000",
                    )
                    .await
                }
            };
            server.join().expect("maintenance server should stop");
            assert_eq!(result.expect_err("maintenance should fail"), expected);
        }
    }

    #[test]
    fn primary_protection_failure_cannot_be_excluded() {
        let mut protection = vec![DesktopUpdateProtection {
            environment_id: "primary".to_string(),
            label: "Local".to_string(),
            primary: true,
            status: ProtectionStatus::Failed,
            message: Some("backup failed".to_string()),
            stage: None,
            elapsed_ms: None,
            blocked_operation_count: None,
        }];

        let error = apply_named_secondary_exclusions(
            &mut protection,
            &["primary".to_string()].into_iter().collect(),
        )
        .expect_err("primary failure must remain blocking");

        assert!(error.contains("primary"));
        assert_eq!(protection[0].status, ProtectionStatus::Failed);
    }

    #[test]
    fn failed_secondary_requires_an_exact_named_exclusion() {
        let failed = DesktopUpdateProtection {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            primary: false,
            status: ProtectionStatus::Failed,
            message: Some("backup failed".to_string()),
            stage: None,
            elapsed_ms: None,
            blocked_operation_count: None,
        };
        let mut without_exclusion = vec![failed.clone()];
        assert!(
            apply_named_secondary_exclusions(&mut without_exclusion, &Default::default()).is_err()
        );

        let mut with_exclusion = vec![failed];
        apply_named_secondary_exclusions(
            &mut with_exclusion,
            &["wsl:Ubuntu".to_string()].into_iter().collect(),
        )
        .expect("the exact secondary may be excluded");
        assert_eq!(with_exclusion[0].status, ProtectionStatus::Excluded);
    }

    #[test]
    fn remote_secondary_protection_notice_requires_a_settled_secondary_only_failure() {
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_string());
        let manager = DesktopUpdateManager::new();
        let message = "The secondary environment WSL (Ubuntu) must be protected or explicitly excluded by name.";
        for (phase, primary_status, secondary_status) in [
            (
                UpdatePhase::Failed,
                ProtectionStatus::Failed,
                ProtectionStatus::Failed,
            ),
            (
                UpdatePhase::Failed,
                ProtectionStatus::Failed,
                ProtectionStatus::Protected,
            ),
            (
                UpdatePhase::Protecting,
                ProtectionStatus::Protected,
                ProtectionStatus::Failed,
            ),
            (
                UpdatePhase::Failed,
                ProtectionStatus::Protected,
                ProtectionStatus::Protected,
            ),
        ] {
            manager.replace_inner(|inner| {
                inner.phase = phase;
                inner.message = Some(message.to_owned());
                inner.protection = [(true, primary_status), (false, secondary_status)]
                    .into_iter()
                    .map(|(primary, status)| DesktopUpdateProtection {
                        environment_id: if primary { "primary" } else { "wsl:Ubuntu" }.to_owned(),
                        label: if primary { "Local" } else { "WSL (Ubuntu)" }.to_owned(),
                        primary,
                        status,
                        message: None,
                        stage: None,
                        elapsed_ms: None,
                        blocked_operation_count: None,
                    })
                    .collect();
            });
            manager.note_remote_secondary_protection_failure(app.handle());
            assert_eq!(manager.state(app.handle())["message"], message);
        }

        let recovery_message =
            append_recovery_error(message.to_owned(), Some("port in use".to_owned()));
        manager.replace_inner(|inner| {
            inner.protection[1].status = ProtectionStatus::Failed;
            inner.message = Some(recovery_message.clone());
        });
        manager.note_remote_secondary_protection_failure(app.handle());
        assert_eq!(
            manager.state(app.handle())["message"],
            format!("{recovery_message}. Finish the update on the host.")
        );
        for message in ["Protection failed!", "Protection failed?"] {
            manager.replace_inner(|inner| inner.message = Some(message.to_owned()));
            manager.note_remote_secondary_protection_failure(app.handle());
            assert_eq!(
                manager.state(app.handle())["message"],
                format!("{message} Finish the update on the host.")
            );
        }
    }

    #[test]
    fn update_state_serializes_protecting_before_installing() {
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_string());
        let handle = app.handle();
        let mut state = DesktopUpdateInner {
            phase: UpdatePhase::Protecting,
            ..DesktopUpdateInner::default()
        };
        state.protection.push(DesktopUpdateProtection {
            environment_id: "primary".to_string(),
            label: "Local".to_string(),
            primary: true,
            status: ProtectionStatus::Pending,
            message: None,
            stage: Some(ProtectionStage::WaitingForMutations),
            elapsed_ms: Some(12_000),
            blocked_operation_count: Some(1),
        });

        let protecting = update_state_value(handle, true, &state);
        assert_eq!(protecting["phase"], "protecting");
        assert_eq!(protecting["protection"][0]["status"], "pending");
        assert_eq!(
            protecting["protection"][0]["stage"],
            "waiting-for-mutations"
        );
        assert_eq!(protecting["protection"][0]["elapsedMs"], 12_000);
        assert_eq!(protecting["protection"][0]["blockedOperationCount"], 1);

        state.phase = UpdatePhase::Installing;
        let installing = update_state_value(handle, true, &state);
        assert_eq!(installing["phase"], "installing");
    }

    #[test]
    fn protection_bypass_requires_a_previous_failed_attempt() {
        let failed = DesktopUpdateProtection {
            environment_id: "primary".to_string(),
            label: "Local".to_string(),
            primary: true,
            status: ProtectionStatus::Failed,
            message: Some("backup failed".to_string()),
            stage: None,
            elapsed_ms: None,
            blocked_operation_count: None,
        };

        assert!(!protection_bypass_allowed(
            UpdatePhase::Available,
            std::slice::from_ref(&failed)
        ));
        assert!(protection_bypass_allowed(UpdatePhase::Failed, &[failed]));
    }

    #[tokio::test]
    async fn install_command_rejects_a_forged_first_attempt_bypass() {
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_string());
        let manager = DesktopUpdateManager::new();
        manager.replace_inner(|inner| {
            inner.phase = UpdatePhase::Available;
            inner.protection = vec![DesktopUpdateProtection {
                environment_id: "primary".to_string(),
                label: "Local".to_string(),
                primary: true,
                status: ProtectionStatus::Failed,
                message: Some("forged renderer state".to_string()),
                stage: None,
                elapsed_ms: None,
                blocked_operation_count: None,
            }];
        });

        let result = manager
            .install_update(
                app.handle(),
                &BackendSupervisor::new(),
                DesktopUpdateInstallInput {
                    skip_protection: true,
                    ..DesktopUpdateInstallInput::default()
                },
            )
            .await;

        assert_eq!(result["accepted"], false);
        assert!(
            result["state"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("only after a protection attempt fails"))
        );
        assert!(
            !manager
                .inner
                .lock()
                .expect("desktop update mutex poisoned")
                .install_in_flight
        );
    }

    #[test]
    fn maintenance_status_decodes_live_prepare_progress() {
        let progress: UpdateMaintenanceProgress = serde_json::from_value(json!({
            "phase": "preparing",
            "stage": "quiescing-runtime",
            "elapsedMs": 750,
            "remainingMs": 29_250,
            "inFlightMutations": 0,
            "blockers": []
        }))
        .expect("maintenance progress should decode");

        assert_eq!(progress.stage, Some(ProtectionStage::QuiescingRuntime));
        assert_eq!(progress.elapsed_ms, Some(750));
        assert_eq!(progress.in_flight_mutations, Some(0));
    }

    #[tokio::test]
    async fn prepare_polls_status_and_publishes_progress_before_completion() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("maintenance server should bind");
        let port = listener
            .local_addr()
            .expect("maintenance server address")
            .port();
        let server = thread::spawn(move || {
            let mut prepare_stream = None;
            for _ in 0..2 {
                let (mut stream, _) = listener
                    .accept()
                    .expect("maintenance request should arrive");
                let mut request = [0_u8; 4096];
                let length = stream
                    .read(&mut request)
                    .expect("maintenance request should read");
                let request = String::from_utf8_lossy(&request[..length]);
                if request.starts_with("GET /api/maintenance/update/status") {
                    let body = json!({
                        "phase": "preparing",
                        "stage": "checkpointing-database",
                        "elapsedMs": 500,
                        "remainingMs": 29_500,
                        "inFlightMutations": 0,
                        "blockers": []
                    })
                    .to_string();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("status response should write");
                } else {
                    assert!(request.starts_with("POST /api/maintenance/update/prepare"));
                    prepare_stream = Some(stream);
                }
            }
            let body = json!({
                "operationId": "00000000-0000-4000-8000-000000000001",
                "storageInstanceId": "00000000-0000-4000-8000-000000000002",
                "backupId": "00000000-0000-4000-8000-000000000003",
                "drainedOperations": 0,
                "expiresAt": "2026-08-24T21:00:00Z"
            })
            .to_string();
            let mut stream = prepare_stream.expect("prepare request should be retained");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("prepare response should write");
        });
        let mut config = test_backend_config();
        config.port = port;
        let mut progress = Vec::new();

        let result =
            prepare_backend_for_update(&config, &UpdateProtectionTransport::Http, |update| {
                progress.push(update)
            })
            .await
            .expect("prepare should complete");

        server.join().expect("maintenance server should stop");
        assert_eq!(result.operation_id, "00000000-0000-4000-8000-000000000001");
        assert!(
            progress.iter().any(|update| {
                update.stage == Some(ProtectionStage::CheckpointingDatabase)
                    && update.elapsed_ms == Some(500)
            }),
            "progress updates: {progress:?}"
        );
    }

    #[tokio::test]
    async fn overlapping_install_returns_protecting_state_without_consuming_the_attempt() {
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_string());
        let handle = app.handle();
        let manager = DesktopUpdateManager::new();
        manager.replace_inner(|inner| {
            inner.install_in_flight = true;
            inner.phase = UpdatePhase::Protecting;
        });

        let result = manager
            .install_update(
                handle,
                &BackendSupervisor::new(),
                DesktopUpdateInstallInput::default(),
            )
            .await;

        assert_eq!(result["accepted"], false);
        assert_eq!(result["state"]["phase"], "protecting");
        assert!(
            manager
                .inner
                .lock()
                .expect("desktop update mutex poisoned")
                .install_in_flight
        );
    }

    #[tokio::test]
    async fn protection_bypass_after_failure_stops_and_restarts_prior_set_on_installer_failure() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::protection_bypass_after_failure_stops_and_restarts_prior_set_on_installer_failure",
        ) else {
            return;
        };
        let (base_url, update_server) = spawn_update_server("test");
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let handle = app.handle();
        let manager = DesktopUpdateManager::new();
        assert_eq!(
            manager.check_for_update(handle.clone()).await["checked"],
            true
        );
        assert_eq!(
            manager.download_update(handle.clone()).await["completed"],
            true
        );
        update_server.join().expect("update server should stop");

        let state = tempfile::tempdir().expect("backend state tempdir should open");
        let supervisor = BackendSupervisor::new();
        let port = crate::test_support::free_test_port();
        let config = BackendRunConfig {
            port,
            ..test_backend_config()
        };
        supervisor
            .start(BackendLaunchPlan::local(state.path().to_path_buf(), config))
            .await
            .expect("primary backend should start");
        supervisor.record_unavailable_environment(BackendUnavailableEnvironment {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            configured_distro: Some("Ubuntu".to_string()),
            detail: "distribution is unavailable".to_string(),
        });

        let blocked = manager
            .install_update(handle, &supervisor, DesktopUpdateInstallInput::default())
            .await;
        assert_eq!(blocked["completed"], false);
        assert_eq!(blocked["state"]["phase"], "failed");
        assert_eq!(
            blocked["state"]["protection"][0]["status"], "protected",
            "primary protection should succeed: {blocked}"
        );
        assert_eq!(blocked["state"]["protection"][1]["status"], "failed");
        assert!(
            !blocked["state"]["message"]
                .as_str()
                .expect("host install should explain the protection failure")
                .contains("Finish the update on the host.")
        );
        manager.note_remote_secondary_protection_failure(handle);
        let remote_message = manager.state(handle)["message"].clone();
        assert!(
            remote_message
                .as_str()
                .is_some_and(|message| message.ends_with(". Finish the update on the host."))
        );
        manager.note_remote_secondary_protection_failure(handle);
        assert_eq!(manager.state(handle)["message"], remote_message);
        let after_blocked = supervisor.snapshot_for_update();
        assert!(
            after_blocked
                .environments
                .iter()
                .any(|environment| environment.primary && environment.running),
            "{blocked}\n{:?}",
            after_blocked.environments
        );

        manager.fail_next_install("synthetic installer failure");
        let failed_install = manager
            .install_update(
                handle,
                &supervisor,
                DesktopUpdateInstallInput {
                    excluded_environment_ids: Vec::new(),
                    skip_protection: true,
                },
            )
            .await;
        assert_eq!(failed_install["completed"], false);
        assert_eq!(failed_install["state"]["phase"], "failed");
        assert_eq!(
            failed_install["state"]["protection"][0]["status"],
            "skipped"
        );
        assert_eq!(
            failed_install["state"]["protection"][1]["status"],
            "skipped"
        );
        assert!(
            failed_install["state"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("synthetic installer failure"))
        );
        let after_restart = supervisor.snapshot_for_update();
        assert!(
            after_restart
                .environments
                .iter()
                .any(|environment| environment.primary && environment.running),
            "{failed_install}\n{:?}",
            after_restart.environments
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted primary should stop");
        isolated.complete();
    }

    fn test_backend_config() -> BackendRunConfig {
        BackendRunConfig {
            environment_id: "primary".to_string(),
            label: "Local".to_string(),
            running_distro: None,
            port: 0,
            bind_host: "127.0.0.1".to_string(),
            local_host: "127.0.0.1".to_string(),
            desktop_bootstrap_token: "desktop-update-test-token".to_string(),
            server_exposure_mode: "local-only".to_string(),
            endpoint_url: None,
            advertised_host: None,
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
        }
    }

    fn assert_request_read(stream: &mut std::net::TcpStream, message: &str) {
        let mut request = [0_u8; 2048];
        assert!(stream.read(&mut request).expect(message) > 0, "{message}");
    }

    fn updater_test_app(endpoint: String) -> tauri::App<MockRuntime> {
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({
                "pubkey": STANDARD.encode(TEST_PUBLIC_KEY),
                "endpoints": [endpoint],
                "dangerousInsecureTransportProtocol": true,
                "windows": null,
            }),
        );
        mock_builder()
            .manage(DesktopUpdateManager::new())
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app")
    }

    fn updater_test_app_with_request_counter(
        requests: Arc<AtomicUsize>,
    ) -> tauri::App<MockRuntime> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("update request should arrive");
                let mut request = [0_u8; 2048];
                let request_len = stream
                    .read(&mut request)
                    .expect("update request should read");
                assert!(request_len > 0, "update request should not be empty");
                requests.fetch_add(1, Ordering::SeqCst);
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .expect("update response should write");
            }
        });
        updater_test_app(format!("{base_url}/latest.json"))
    }

    fn spawn_update_server(payload: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let manifest = serde_json::json!({
            "version": "99.0.0",
            "notes": "Coverage update",
            "pub_date": "2026-07-16T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": STANDARD.encode(TEST_SIGNATURE),
        })
        .to_string();
        let thread = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("update request should arrive");
                let mut request = [0_u8; 2048];
                let read = stream
                    .read(&mut request)
                    .expect("update request should read");
                let request = String::from_utf8_lossy(&request[..read]);
                let (content_type, body) = if request.starts_with("GET /artifact ") {
                    ("application/octet-stream", payload.to_owned())
                } else {
                    ("application/json", manifest.clone())
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("update response should write");
            }
        });
        (base_url, thread)
    }

    fn spawn_no_update_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("update request should arrive");
            let mut request = [0_u8; 2048];
            let request_len = stream
                .read(&mut request)
                .expect("update request should read");
            assert!(request_len > 0, "update request should not be empty");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .expect("update response should write");
        });
        (base_url, thread)
    }

    fn poison_update_lock(manager: &DesktopUpdateManager) {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = manager
                .inner
                .lock()
                .expect("update state lock should not already be poisoned");
            panic!("synthetic panic while holding the update state lock");
        }));
        assert!(panic.is_err());
        assert!(manager.inner.is_poisoned());
    }

    async fn wait_for_fixture_event(event: &FixtureEvent, checkpoint: u64, description: &str) {
        tokio::time::timeout(Duration::from_secs(5), event.wait_after(checkpoint))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
    }

    async fn wait_for_background_check_completion(manager: &DesktopUpdateManager, expected: usize) {
        let expected = u64::try_from(expected).expect("completion count should fit u64");
        manager
            .background_check_completion
            .wait_until_at_least(expected)
            .await;
    }

    async fn wait_for_background_timer(manager: &DesktopUpdateManager, expected: u64) {
        manager
            .background_timer_armed
            .wait_until_at_least(expected)
            .await;
    }

    #[test]
    fn disabled_update_results_preserve_bridge_shapes() {
        let state = serde_json::json!({
            "status": "disabled"
        });

        assert_eq!(
            disabled_update_check_result(state.clone()),
            serde_json::json!({
                "checked": false,
                "state": state.clone(),
            })
        );
        assert_eq!(
            disabled_update_action_result(state.clone()),
            serde_json::json!({
                "accepted": false,
                "completed": false,
                "state": state,
            })
        );
    }

    #[test]
    fn successful_install_restarts_only_non_windows_platforms() {
        assert!(restart_required_after_install("macos"));
        assert!(restart_required_after_install("linux"));
        assert!(!restart_required_after_install("windows"));
    }

    #[test]
    fn manager_state_helpers_clone_metadata_without_runtime_updates() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({"pubkey":"","windows":null}),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app");
        let handle = app.handle();
        let manager = DesktopUpdateManager::new();
        let snapshot = manager.replace_inner(|inner| {
            inner.available_version = Some("2.0.0".to_string());
            inner.downloaded_version = Some("1.9.0".to_string());
            inner.status = Some(STATUS_AVAILABLE.to_string());
            inner.download_percent = Some(25.0);
            inner.checked_at = Some(now_rfc3339());
        });

        assert_eq!(snapshot.available_version.as_deref(), Some("2.0.0"));
        assert_eq!(snapshot.downloaded_version.as_deref(), Some("1.9.0"));
        assert_eq!(snapshot.status.as_deref(), Some(STATUS_AVAILABLE));
        assert_eq!(snapshot.download_percent, Some(25.0));
        assert!(
            snapshot
                .checked_at
                .as_deref()
                .is_some_and(|value| value.contains('T'))
        );
        assert!(is_updater_disabled(&UpdaterError::EmptyEndpoints));
        assert_eq!(disabled_update_state(handle)["status"], STATUS_DISABLED);
        assert!(disabled_update_state(handle).get("channel").is_none());
        assert_eq!(
            error_update_state(handle, "check", "failed".to_owned())["errorContext"],
            "check"
        );
        assert_eq!(
            update_state_value(handle, true, &snapshot)["availableVersion"],
            "2.0.0"
        );
        assert_eq!(
            manager.record_error_state(handle, "install", "failed".to_owned())["errorContext"],
            "install"
        );
        assert_eq!(manager.state(handle)["status"], STATUS_DISABLED);

        tauri::async_runtime::block_on(async {
            let backend = BackendSupervisor::new();
            assert_eq!(
                manager.check_for_update(handle.clone()).await["checked"],
                false
            );
            assert_eq!(
                manager.download_update(handle.clone()).await["accepted"],
                false
            );
            assert_eq!(
                manager
                    .install_update(handle, &backend, DesktopUpdateInstallInput::default())
                    .await["accepted"],
                false
            );
        });
    }

    #[tokio::test]
    async fn manager_checks_downloads_and_rejects_an_invalid_installer() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let (base_url, server) = spawn_update_server("test");
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({
                "pubkey": STANDARD.encode(TEST_PUBLIC_KEY),
                "endpoints": [format!("{base_url}/latest.json")],
                "dangerousInsecureTransportProtocol": true,
                "windows": null,
            }),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app");
        let handle = app.handle();
        let manager = DesktopUpdateManager::new();

        let check = manager.check_for_update(handle.clone()).await;
        assert_eq!(check["checked"], true);
        assert_eq!(check["state"]["status"], STATUS_AVAILABLE);
        assert_eq!(check["state"]["availableVersion"], "99.0.0");

        let download = manager.download_update(handle.clone()).await;
        assert_eq!(download["accepted"], true);
        assert_eq!(download["completed"], true);
        assert_eq!(download["state"]["status"], STATUS_DOWNLOADED);
        assert_eq!(download["state"]["downloadPercent"], 100.0);

        #[cfg(target_os = "macos")]
        {
            let backend = BackendSupervisor::new();
            let install = manager
                .install_update(handle, &backend, DesktopUpdateInstallInput::default())
                .await;
            assert_eq!(install["accepted"], true);
            assert_eq!(install["completed"], false);
            assert_eq!(install["state"]["status"], STATUS_ERROR);
            assert_eq!(install["state"]["errorContext"], "install");
        }

        server.join().expect("update server should stop");
    }

    #[test]
    fn manager_state_reads_recover_a_poisoned_lock() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::manager_state_reads_recover_a_poisoned_lock",
        ) else {
            return;
        };
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_owned());
        let manager = app.state::<DesktopUpdateManager>();
        manager.replace_inner(|inner| {
            inner.status = Some(STATUS_AVAILABLE.to_owned());
            inner.available_version = Some("99.0.0".to_owned());
            inner.phase = UpdatePhase::Available;
        });

        poison_update_lock(&manager);
        let state = manager.state(app.handle());
        assert_eq!(state["status"], STATUS_AVAILABLE);
        assert_eq!(state["availableVersion"], "99.0.0");
        assert_eq!(state["phase"], "available");
        assert!(!manager.inner.is_poisoned());

        poison_update_lock(&manager);
        let bridge_state = crate::bridge::desktop_bridge_get_update_state(
            app.handle().clone(),
            app.state::<DesktopUpdateManager>(),
        )
        .expect("bridge update state read should recover");
        assert_eq!(bridge_state, state);
        assert!(!manager.inner.is_poisoned());
        isolated.complete();
    }

    #[test]
    fn manager_native_reads_recover_a_poisoned_lock() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::manager_native_reads_recover_a_poisoned_lock",
        ) else {
            return;
        };
        use tauri::Listener;

        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_owned());
        let manager = app.state::<DesktopUpdateManager>();
        manager.replace_inner(|inner| {
            inner.install_in_flight = true;
            inner.phase = UpdatePhase::Protecting;
        });

        poison_update_lock(&manager);
        assert!(manager.install_in_flight());
        assert!(!manager.inner.is_poisoned());

        let (sender, receiver) = mpsc::channel();
        app.listen_any(UPDATE_STATE_EVENT, move |event| {
            sender
                .send(event.payload().to_owned())
                .expect("update state event");
        });
        poison_update_lock(&manager);
        manager.emit_current_state(app.handle());
        let state: Value =
            serde_json::from_str(&receiver.try_recv().expect("recovered update state event"))
                .expect("update state JSON");
        assert_eq!(state["phase"], "protecting");
        assert!(!manager.inner.is_poisoned());
        isolated.complete();
    }

    #[test]
    fn operation_guard_recovers_a_poisoned_lock() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::operation_guard_recovers_a_poisoned_lock",
        ) else {
            return;
        };
        let app = updater_test_app("http://127.0.0.1:9/latest.json".to_owned());
        let manager = app.state::<DesktopUpdateManager>();
        let (guard, _) = manager.begin_check(app.handle()).expect("first check");

        poison_update_lock(&manager);
        guard.finish(|inner| inner.status = Some(STATUS_UP_TO_DATE.to_owned()));
        assert!(!manager.inner.is_poisoned());
        assert_eq!(manager.state(app.handle())["status"], STATUS_UP_TO_DATE);

        let (guard, _) = manager
            .begin_check(app.handle())
            .expect("check after completion");
        poison_update_lock(&manager);
        drop(guard);
        assert!(!manager.inner.is_poisoned());
        assert_eq!(manager.state(app.handle())["status"], STATUS_UP_TO_DATE);
        assert!(manager.begin_check(app.handle()).is_some());
        isolated.complete();
    }

    #[tokio::test]
    async fn manager_check_recovers_a_poisoned_lock() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::manager_check_recovers_a_poisoned_lock",
        ) else {
            return;
        };
        let (base_url, server) = spawn_no_update_server();
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = app.state::<DesktopUpdateManager>();

        poison_update_lock(&manager);
        let check = manager.check_for_update(app.handle().clone()).await;
        assert_eq!(check["checked"], true);
        assert_eq!(check["state"]["status"], STATUS_UP_TO_DATE);
        assert!(!manager.inner.is_poisoned());

        server.join().expect("update server should stop");
        isolated.complete();
    }

    #[tokio::test]
    async fn manager_handles_up_to_date_and_missing_update_actions() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let (base_url, server) = spawn_no_update_server();
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({
                "pubkey": STANDARD.encode(TEST_PUBLIC_KEY),
                "endpoints": [format!("{base_url}/latest.json")],
                "dangerousInsecureTransportProtocol": true,
                "windows": null,
            }),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app");
        let handle = app.handle();
        let manager = DesktopUpdateManager::new();

        let check = manager.check_for_update(handle.clone()).await;
        assert_eq!(check["checked"], true);
        assert_eq!(check["state"]["status"], STATUS_UP_TO_DATE);

        let download = manager.download_update(handle.clone()).await;
        assert_eq!(download["accepted"], false);
        assert_eq!(download["state"]["errorContext"], "download");

        let install = manager
            .install_update(
                handle,
                &BackendSupervisor::new(),
                DesktopUpdateInstallInput::default(),
            )
            .await;
        assert_eq!(install["accepted"], false);
        assert_eq!(install["state"]["errorContext"], "install");

        server.join().expect("update server should stop");
    }

    #[tokio::test]
    async fn manager_reports_download_signature_failures() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let (base_url, server) = spawn_update_server("tampered");
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({
                "pubkey": STANDARD.encode(TEST_PUBLIC_KEY),
                "endpoints": [format!("{base_url}/latest.json")],
                "dangerousInsecureTransportProtocol": true,
                "windows": null,
            }),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app");
        let handle = app.handle();
        let manager = DesktopUpdateManager::new();

        assert_eq!(
            manager.check_for_update(handle.clone()).await["checked"],
            true
        );
        let download = manager.download_update(handle.clone()).await;
        assert_eq!(download["accepted"], true);
        assert_eq!(download["completed"], false);
        assert_eq!(download["state"]["status"], STATUS_ERROR);
        assert_eq!(download["state"]["errorContext"], "download");

        server.join().expect("update server should stop");
    }

    #[tokio::test(start_paused = true)]
    async fn background_checks_wait_fifteen_seconds_then_thirty_minutes_after_completion() {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = updater_test_app_with_request_counter(requests.clone());
        let manager = app.state::<DesktopUpdateManager>();
        let task = tokio::spawn(run_background_update_checks(app.handle().clone()));

        wait_for_background_timer(&manager, 1).await;
        tokio::time::advance(Duration::from_secs(14)).await;
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        wait_for_background_check_completion(&manager, 1).await;
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(manager.state(app.handle())["status"], STATUS_UP_TO_DATE);
        wait_for_background_timer(&manager, 2).await;
        tokio::time::advance(Duration::from_secs(30 * 60 - 1)).await;
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        wait_for_background_check_completion(&manager, 2).await;
        assert_eq!(requests.load(Ordering::SeqCst), 2);

        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn background_scheduler_survives_a_panicking_check() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "updates::tests::background_scheduler_survives_a_panicking_check",
        ) else {
            return;
        };
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let (request_sender, request_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("retry check should arrive");
            assert_request_read(&mut stream, "retry check should read");
            server_requests.fetch_add(1, Ordering::SeqCst);
            request_sender
                .send(())
                .expect("test should observe the retry request");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .expect("retry check should respond");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = app.state::<DesktopUpdateManager>();
        manager.panic_next_check.store(true, Ordering::SeqCst);
        let task = tokio::spawn(run_background_update_checks(app.handle().clone()));
        wait_for_background_timer(&manager, 1).await;
        tokio::time::advance(STARTUP_UPDATE_CHECK_DELAY).await;
        let manager = app.state::<DesktopUpdateManager>();
        wait_for_background_check_completion(&manager, 1).await;
        assert_eq!(manager.check_attempts.load(Ordering::SeqCst), 1);
        assert_eq!(manager.background_check_completion.checkpoint(), 1);
        assert_eq!(requests.load(Ordering::SeqCst), 0);
        assert_eq!(manager.state(app.handle())["status"], STATUS_IDLE);
        assert!(!manager.inner.is_poisoned());

        wait_for_background_timer(&manager, 2).await;
        tokio::time::advance(BACKGROUND_UPDATE_CHECK_INTERVAL).await;
        let manager = app.state::<DesktopUpdateManager>();
        wait_for_background_check_completion(&manager, 2).await;
        assert_eq!(manager.check_attempts.load(Ordering::SeqCst), 2);
        request_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("surviving scheduler should issue a retry request");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(manager.state(app.handle())["status"], STATUS_UP_TO_DATE);
        task.abort();
        assert!(
            task.await
                .expect_err("background scheduler should abort")
                .is_cancelled()
        );
        server.join().expect("update server should stop");
        isolated.complete();
    }

    #[tokio::test]
    async fn overlapping_manual_checks_share_one_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let request_started = Arc::new(FixtureEvent::default());
        let request_started_checkpoint = request_started.checkpoint();
        let server_request_started = request_started.clone();
        let (release_sender, release_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("update request should arrive");
            assert_request_read(&mut stream, "update request should read");
            server_requests.fetch_add(1, Ordering::SeqCst);
            server_request_started.publish();
            release_receiver
                .recv()
                .expect("test should release update response");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .expect("update response should write");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());
        let first_manager = manager.clone();
        let first_handle = app.handle().clone();
        let first = tokio::spawn(async move { first_manager.check_for_update(first_handle).await });

        wait_for_fixture_event(
            &request_started,
            request_started_checkpoint,
            "update request should become active",
        )
        .await;
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        let second = tokio::time::timeout(
            Duration::from_millis(100),
            manager.check_for_update(app.handle().clone()),
        )
        .await;
        release_sender
            .send(())
            .expect("server should still await its response");
        let first = first.await.expect("first check should complete");
        server.join().expect("update server should stop");

        let second = second.expect("overlapping check should return without another request");
        assert_eq!(first["checked"], true);
        assert_eq!(second["checked"], false);
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn check_while_downloading_returns_current_state_without_checking() {
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".to_owned(),
            serde_json::json!({"pubkey":"","windows":null}),
        );
        let app = mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("mock Tauri app");
        let manager = DesktopUpdateManager::new();
        manager.replace_inner(|inner| {
            inner.available_version = Some("99.0.0".to_owned());
            inner.status = Some(STATUS_DOWNLOADING.to_owned());
            inner.download_percent = Some(50.0);
        });

        let check = manager.check_for_update(app.handle().clone()).await;

        assert_eq!(check["checked"], false);
        assert_eq!(check["state"]["status"], STATUS_DOWNLOADING);
        assert_eq!(check["state"]["availableVersion"], "99.0.0");
        assert_eq!(check["state"]["downloadPercent"], 50.0);
    }

    #[tokio::test]
    async fn check_while_downloaded_preserves_downloaded_version_and_bytes() {
        let (base_url, server) = spawn_update_server("test");
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = DesktopUpdateManager::new();

        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            true
        );
        assert_eq!(
            manager.download_update(app.handle().clone()).await["completed"],
            true
        );
        let downloaded_bytes = manager
            .inner
            .lock()
            .expect("desktop update mutex should lock")
            .downloaded_update
            .as_ref()
            .expect("downloaded update should remain available")
            .bytes
            .clone();

        let check = manager.check_for_update(app.handle().clone()).await;

        let inner = manager
            .inner
            .lock()
            .expect("desktop update mutex should lock");
        assert_eq!(check["checked"], false);
        assert_eq!(check["state"]["status"], STATUS_DOWNLOADED);
        assert_eq!(check["state"]["downloadedVersion"], "99.0.0");
        assert_eq!(
            inner
                .downloaded_update
                .as_ref()
                .expect("skipped check must retain downloaded update")
                .bytes,
            downloaded_bytes
        );
        drop(inner);
        server.join().expect("update server should stop");
    }

    #[tokio::test]
    async fn failed_check_allows_an_immediate_manual_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = requests.clone();
        let server = thread::spawn(move || {
            for response in [
                b"HTTP/1.1 500 Internal Server Error\r\nConnection: close\r\n\r\n".as_slice(),
                b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".as_slice(),
            ] {
                let (mut stream, _) = listener.accept().expect("update request should arrive");
                assert_request_read(&mut stream, "update request should read");
                server_requests.fetch_add(1, Ordering::SeqCst);
                stream
                    .write_all(response)
                    .expect("update response should write");
            }
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = DesktopUpdateManager::new();

        let failed = manager.check_for_update(app.handle().clone()).await;
        let retry = manager.check_for_update(app.handle().clone()).await;

        assert_eq!(failed["checked"], false);
        assert_eq!(failed["state"]["status"], STATUS_ERROR);
        assert_eq!(retry["checked"], true);
        assert_eq!(retry["state"]["status"], STATUS_UP_TO_DATE);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.join().expect("update server should stop");
    }

    #[tokio::test(start_paused = true)]
    async fn background_checks_only_inspect_available_updates() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let manifest = serde_json::json!({
            "version": "99.0.0",
            "notes": "Coverage update",
            "pub_date": "2026-07-16T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": STANDARD.encode(TEST_SIGNATURE),
        })
        .to_string();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("update request should arrive");
            assert_request_read(&mut stream, "update request should read");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
                manifest.len()
            )
            .expect("update response should write");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = app.state::<DesktopUpdateManager>();
        let task = tokio::spawn(run_background_update_checks(app.handle().clone()));

        wait_for_background_timer(&manager, 1).await;
        tokio::time::advance(Duration::from_secs(15)).await;
        wait_for_background_check_completion(&manager, 1).await;
        let state = manager.state(app.handle());

        assert_eq!(state["status"], STATUS_AVAILABLE);
        assert_eq!(state["downloadedVersion"], Value::Null);
        task.abort();
        server
            .join()
            .expect("background checks must not request the artifact");
    }

    #[tokio::test]
    async fn download_is_rejected_while_a_real_check_is_in_flight() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let manifest = serde_json::json!({
            "version": "99.0.0",
            "notes": "Coverage update",
            "pub_date": "2026-07-16T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": STANDARD.encode(TEST_SIGNATURE),
        })
        .to_string();
        let check_started = Arc::new(FixtureEvent::default());
        let check_started_checkpoint = check_started.checkpoint();
        let server_check_started = check_started.clone();
        let (release_check_sender, release_check_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("initial check should arrive");
            assert_request_read(&mut stream, "initial check should read");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
                manifest.len()
            )
            .expect("initial check should respond");

            let (mut stream, _) = listener.accept().expect("overlapping check should arrive");
            assert_request_read(&mut stream, "overlapping check should read");
            server_check_started.publish();
            release_check_receiver
                .recv()
                .expect("test should release the check");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .expect("overlapping check should respond");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());

        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            true
        );
        let check_manager = manager.clone();
        let check_handle = app.handle().clone();
        let check = tokio::spawn(async move { check_manager.check_for_update(check_handle).await });
        wait_for_fixture_event(
            &check_started,
            check_started_checkpoint,
            "second check should become in flight",
        )
        .await;

        let download = tokio::time::timeout(
            Duration::from_millis(100),
            manager.download_update(app.handle().clone()),
        )
        .await;
        release_check_sender
            .send(())
            .expect("server should still await the check release");
        let check = check.await.expect("check should complete");
        server.join().expect("update server should stop");

        let download = download.expect("download should defer while a check is in flight");
        assert_eq!(download["accepted"], false);
        assert_eq!(download["completed"], false);
        assert_eq!(download["state"]["status"], STATUS_CHECKING);
        assert_eq!(check["checked"], true);
    }

    #[tokio::test]
    async fn real_download_admission_makes_an_overlapping_check_skip_network_io() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let manifest = serde_json::json!({
            "version": "99.0.0",
            "notes": "Coverage update",
            "pub_date": "2026-07-16T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": STANDARD.encode(TEST_SIGNATURE),
        })
        .to_string();
        let download_started = Arc::new(FixtureEvent::default());
        let download_started_checkpoint = download_started.checkpoint();
        let server_download_started = download_started.clone();
        let (release_download_sender, release_download_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("initial check should arrive");
            assert_request_read(&mut stream, "initial check should read");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
                manifest.len()
            )
            .expect("initial check should respond");

            let (mut stream, _) = listener.accept().expect("download should request artifact");
            assert_request_read(&mut stream, "download should read");
            server_download_started.publish();
            release_download_receiver
                .recv()
                .expect("test should release the artifact");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest")
                .expect("download should respond");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());

        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            true
        );
        let download_manager = manager.clone();
        let download_handle = app.handle().clone();
        let download =
            tokio::spawn(async move { download_manager.download_update(download_handle).await });
        wait_for_fixture_event(
            &download_started,
            download_started_checkpoint,
            "download should become active",
        )
        .await;

        let check = manager.check_for_update(app.handle().clone()).await;
        release_download_sender
            .send(())
            .expect("server should still await the artifact release");
        let download = download.await.expect("download should complete");
        server.join().expect("update server should stop");

        assert_eq!(check["checked"], false);
        assert_eq!(check["state"]["status"], STATUS_DOWNLOADING);
        assert_eq!(download["accepted"], true);
        assert_eq!(download["completed"], true);
    }

    #[tokio::test]
    async fn overlapping_download_returns_busy_without_starting_another_request() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let manifest = serde_json::json!({
            "version": "99.0.0",
            "notes": "Coverage update",
            "pub_date": "2026-07-16T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": STANDARD.encode(TEST_SIGNATURE),
        })
        .to_string();
        let artifact_requests = Arc::new(AtomicUsize::new(0));
        let server_artifact_requests = artifact_requests.clone();
        let download_started = Arc::new(FixtureEvent::default());
        let download_started_checkpoint = download_started.checkpoint();
        let server_download_started = download_started.clone();
        let (release_download_sender, release_download_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("initial check should arrive");
            assert_request_read(&mut stream, "initial check should read");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
                manifest.len()
            )
            .expect("initial check should respond");

            let (mut stream, _) = listener.accept().expect("download should request artifact");
            assert_request_read(&mut stream, "download should read");
            server_artifact_requests.fetch_add(1, Ordering::SeqCst);
            server_download_started.publish();
            release_download_receiver
                .recv()
                .expect("test should release the artifact");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest")
                .expect("download should respond");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());
        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            true
        );

        let first_manager = manager.clone();
        let first_handle = app.handle().clone();
        let first = tokio::spawn(async move { first_manager.download_update(first_handle).await });
        wait_for_fixture_event(
            &download_started,
            download_started_checkpoint,
            "download should become active",
        )
        .await;

        let second = tokio::time::timeout(
            Duration::from_millis(100),
            manager.download_update(app.handle().clone()),
        )
        .await;
        release_download_sender
            .send(())
            .expect("server should still await the artifact release");
        let first = first.await.expect("first download should complete");
        server.join().expect("update server should stop");

        let second = second.expect("overlapping download should return immediately");
        assert_eq!(second["accepted"], false);
        assert_eq!(second["completed"], false);
        assert_eq!(second["state"]["status"], STATUS_DOWNLOADING);
        assert_eq!(first["completed"], true);
        assert_eq!(artifact_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn aborting_a_check_restores_idle_state_and_allows_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let check_started = Arc::new(FixtureEvent::default());
        let check_started_checkpoint = check_started.checkpoint();
        let server_check_started = check_started.clone();
        let (release_check_sender, release_check_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("first check should arrive");
            assert_request_read(&mut stream, "first check should read");
            server_check_started.publish();
            release_check_receiver
                .recv()
                .expect("test should release the first check");
            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");

            let (mut stream, _) = listener.accept().expect("retry check should arrive");
            assert_request_read(&mut stream, "retry check should read");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .expect("retry check should respond");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());
        let check_manager = manager.clone();
        let check_handle = app.handle().clone();
        let check = tokio::spawn(async move { check_manager.check_for_update(check_handle).await });
        wait_for_fixture_event(
            &check_started,
            check_started_checkpoint,
            "check should become active",
        )
        .await;

        check.abort();
        assert!(
            check
                .await
                .expect_err("check should be cancelled")
                .is_cancelled()
        );
        release_check_sender
            .send(())
            .expect("server should still await the check release");

        assert_eq!(manager.state(app.handle())["status"], STATUS_IDLE);
        let retry = manager.check_for_update(app.handle().clone()).await;
        assert_eq!(retry["checked"], true);
        assert_eq!(retry["state"]["status"], STATUS_UP_TO_DATE);
        server.join().expect("update server should stop");
    }

    #[tokio::test]
    async fn an_aborted_check_keeps_a_remote_request_recorded_during_it() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let check_started = Arc::new(FixtureEvent::default());
        let check_started_checkpoint = check_started.checkpoint();
        let server_check_started = check_started.clone();
        let (release_check_sender, release_check_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("check should arrive");
            assert_request_read(&mut stream, "check should read");
            server_check_started.publish();
            release_check_receiver
                .recv()
                .expect("test should release the check");
            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());
        let check_manager = manager.clone();
        let check_handle = app.handle().clone();
        let check = tokio::spawn(async move { check_manager.check_for_update(check_handle).await });
        wait_for_fixture_event(
            &check_started,
            check_started_checkpoint,
            "check should become active",
        )
        .await;

        assert!(manager.begin_remote_request(
            app.handle(),
            RemoteUpdateRequest {
                label: "Tablet".to_owned(),
                detail: None
            },
        ));
        check.abort();
        assert!(
            check
                .await
                .expect_err("check should be cancelled")
                .is_cancelled()
        );
        release_check_sender
            .send(())
            .expect("server should still await the check release");

        assert_eq!(manager.state(app.handle())["status"], STATUS_IDLE);
        assert_eq!(
            manager.state(app.handle())["requestedBy"]["label"],
            "Tablet"
        );
        manager.clear_remote_request(app.handle());
        assert_eq!(manager.state(app.handle())["requestedBy"], Value::Null);
        server.join().expect("update server should stop");
    }

    #[tokio::test]
    async fn aborting_a_download_restores_available_state_and_allows_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("update server should bind");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("update server address")
        );
        let manifest = serde_json::json!({
            "version": "99.0.0",
            "notes": "Coverage update",
            "pub_date": "2026-07-16T00:00:00Z",
            "url": format!("{base_url}/artifact"),
            "signature": STANDARD.encode(TEST_SIGNATURE),
        })
        .to_string();
        let download_started = Arc::new(FixtureEvent::default());
        let download_started_checkpoint = download_started.checkpoint();
        let server_download_started = download_started.clone();
        let (release_download_sender, release_download_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("initial check should arrive");
            assert_request_read(&mut stream, "initial check should read");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{manifest}",
                manifest.len()
            )
            .expect("initial check should respond");

            let (mut stream, _) = listener.accept().expect("first download should arrive");
            assert_request_read(&mut stream, "first download should read");
            server_download_started.publish();
            release_download_receiver
                .recv()
                .expect("test should release the first download");
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest");

            let (mut stream, _) = listener.accept().expect("retry download should arrive");
            assert_request_read(&mut stream, "retry download should read");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 4\r\nConnection: close\r\n\r\ntest")
                .expect("retry download should respond");
        });
        let app = updater_test_app(format!("{base_url}/latest.json"));
        let manager = Arc::new(DesktopUpdateManager::new());
        assert_eq!(
            manager.check_for_update(app.handle().clone()).await["checked"],
            true
        );
        let download_manager = manager.clone();
        let download_handle = app.handle().clone();
        let download =
            tokio::spawn(async move { download_manager.download_update(download_handle).await });
        wait_for_fixture_event(
            &download_started,
            download_started_checkpoint,
            "download should become active",
        )
        .await;

        download.abort();
        assert!(
            download
                .await
                .expect_err("download should be cancelled")
                .is_cancelled()
        );
        release_download_sender
            .send(())
            .expect("server should still await the download release");

        let restored = manager.state(app.handle());
        assert_eq!(restored["status"], STATUS_AVAILABLE);
        assert_eq!(restored["availableVersion"], "99.0.0");
        assert_eq!(restored["downloadPercent"], Value::Null);
        let retry = manager.download_update(app.handle().clone()).await;
        assert_eq!(retry["accepted"], true);
        assert_eq!(retry["completed"], true);
        server.join().expect("update server should stop");
    }
}
