use std::collections::HashMap;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

use serde::Serialize;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewUrl};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::platform::{Platform, PlatformWebviewOps, PreviewPlatformError};
use super::{PendingBounds, PreviewHostState};

pub const STATE_EVENT: &str = "preview://state";
pub const NEW_WINDOW_EVENT: &str = "preview://new-window";

fn preview_new_window_url(url: &Url) -> Option<&Url> {
    matches!(url.scheme(), "http" | "https").then_some(url)
}

#[derive(Debug, Clone, Serialize)]
struct NewWindowEvent {
    #[serde(rename = "tabId")]
    tab_id: String,
    url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CreationProgress {
    incarnation: u64,
    revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginCreation {
    Started(CreationProgress),
    InProgress { incarnation: u64 },
    AlreadyCreated { incarnation: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecyclePhase {
    Creating { cancelled: bool },
    Created,
    Closing,
    Cleaning { cancelled: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TabLifecycle {
    incarnation: u64,
    label: String,
    revision: u64,
    nav_generation: Option<u64>,
    phase: LifecyclePhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LabelOwnership {
    tab_id: String,
    incarnation: u64,
}

#[derive(Debug, Default)]
struct HostLifecycleState {
    next_incarnation: u64,
    next_nav_generation: u64,
    tabs: HashMap<String, TabLifecycle>,
    labels: HashMap<String, LabelOwnership>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationSettlement {
    Created,
    Removed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationCommit {
    Committed,
    Retry,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClosePlan {
    Absent,
    Pending,
    Native { label: String, incarnation: u64 },
    InProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundsReconciliation {
    Current,
    Retry { revision: u64 },
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BoundsUpdate {
    Pending,
    Ignore,
    Creating(CreationProgress),
    Created {
        progress: CreationProgress,
        label: String,
    },
}

#[derive(Debug, Default)]
struct HostCoordinator {
    state: Mutex<HostLifecycleState>,
    changed: Condvar,
}

// When both shared states are needed, callers lock PreviewRegistry first and
// HostCoordinator second. Native webview operations run after both are released.
impl HostCoordinator {
    fn begin(&self, tab_id: &str, label: &str) -> Result<BeginCreation, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        if let Some(lifecycle) = state.tabs.get(tab_id) {
            return Ok(match lifecycle.phase {
                LifecyclePhase::Created => BeginCreation::AlreadyCreated {
                    incarnation: lifecycle.incarnation,
                },
                LifecyclePhase::Creating { .. }
                | LifecyclePhase::Closing
                | LifecyclePhase::Cleaning { .. } => BeginCreation::InProgress {
                    incarnation: lifecycle.incarnation,
                },
            });
        }
        if let Some(owner) = state.labels.get(label) {
            return Err(format!(
                "preview webview label {label} is already owned by tab {}",
                owner.tab_id
            ));
        }

        state.next_incarnation = state.next_incarnation.wrapping_add(1);
        if state.next_incarnation == 0 {
            state.next_incarnation = 1;
        }
        let progress = CreationProgress {
            incarnation: state.next_incarnation,
            revision: 0,
        };
        state.labels.insert(
            label.to_string(),
            LabelOwnership {
                tab_id: tab_id.to_string(),
                incarnation: progress.incarnation,
            },
        );
        state.tabs.insert(
            tab_id.to_string(),
            TabLifecycle {
                incarnation: progress.incarnation,
                label: label.to_string(),
                revision: 0,
                nav_generation: None,
                phase: LifecyclePhase::Creating { cancelled: false },
            },
        );
        Ok(BeginCreation::Started(progress))
    }

    fn wait_until_settled(
        &self,
        tab_id: &str,
        incarnation: u64,
    ) -> Result<CreationSettlement, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        loop {
            match state.tabs.get(tab_id) {
                Some(lifecycle) if lifecycle.incarnation != incarnation => {
                    return Ok(CreationSettlement::Superseded);
                }
                Some(lifecycle) if lifecycle.phase == LifecyclePhase::Created => {
                    return Ok(CreationSettlement::Created);
                }
                Some(_) => {
                    state = self
                        .changed
                        .wait(state)
                        .map_err(|error| error.to_string())?;
                }
                None => return Ok(CreationSettlement::Removed),
            }
        }
    }

    fn current_creation(
        &self,
        tab_id: &str,
        incarnation: u64,
    ) -> Result<Option<(CreationProgress, bool)>, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        Ok(state.tabs.get(tab_id).and_then(|lifecycle| {
            if lifecycle.incarnation != incarnation {
                return None;
            }
            match lifecycle.phase {
                LifecyclePhase::Creating { cancelled } => Some((
                    CreationProgress {
                        incarnation,
                        revision: lifecycle.revision,
                    },
                    cancelled,
                )),
                _ => None,
            }
        }))
    }

    fn is_created(&self, tab_id: &str, label: &str, incarnation: u64) -> Result<bool, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        Ok(state.tabs.get(tab_id).is_some_and(|lifecycle| {
            lifecycle.incarnation == incarnation
                && lifecycle.label == label
                && lifecycle.phase == LifecyclePhase::Created
        }))
    }

    fn begin_bounds_update(&self, tab_id: &str) -> Result<BoundsUpdate, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let Some(lifecycle) = state.tabs.get_mut(tab_id) else {
            return Ok(BoundsUpdate::Pending);
        };
        match lifecycle.phase {
            LifecyclePhase::Creating { cancelled: false } => {
                lifecycle.revision = lifecycle.revision.wrapping_add(1);
                Ok(BoundsUpdate::Creating(CreationProgress {
                    incarnation: lifecycle.incarnation,
                    revision: lifecycle.revision,
                }))
            }
            LifecyclePhase::Created => {
                lifecycle.revision = lifecycle.revision.wrapping_add(1);
                Ok(BoundsUpdate::Created {
                    progress: CreationProgress {
                        incarnation: lifecycle.incarnation,
                        revision: lifecycle.revision,
                    },
                    label: lifecycle.label.clone(),
                })
            }
            LifecyclePhase::Creating { cancelled: true }
            | LifecyclePhase::Closing
            | LifecyclePhase::Cleaning { .. } => Ok(BoundsUpdate::Ignore),
        }
    }

    fn commit_creation(
        &self,
        tab_id: &str,
        incarnation: u64,
        revision: u64,
    ) -> Result<CreationCommit, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let Some(lifecycle) = state.tabs.get_mut(tab_id) else {
            return Ok(CreationCommit::Cancelled);
        };
        if lifecycle.incarnation != incarnation {
            return Ok(CreationCommit::Cancelled);
        }
        match lifecycle.phase {
            LifecyclePhase::Creating { cancelled: true } => Ok(CreationCommit::Cancelled),
            LifecyclePhase::Creating { cancelled: false } if lifecycle.revision != revision => {
                Ok(CreationCommit::Retry)
            }
            LifecyclePhase::Creating { cancelled: false } => {
                lifecycle.phase = LifecyclePhase::Created;
                self.changed.notify_all();
                Ok(CreationCommit::Committed)
            }
            LifecyclePhase::Created | LifecyclePhase::Closing | LifecyclePhase::Cleaning { .. } => {
                Ok(CreationCommit::Cancelled)
            }
        }
    }

    fn begin_close(&self, tab_id: &str) -> Result<ClosePlan, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let Some(lifecycle) = state.tabs.get_mut(tab_id) else {
            return Ok(ClosePlan::Absent);
        };
        match lifecycle.phase {
            LifecyclePhase::Creating { .. } => {
                lifecycle.phase = LifecyclePhase::Creating { cancelled: true };
                lifecycle.nav_generation = None;
                Ok(ClosePlan::Pending)
            }
            LifecyclePhase::Created => {
                lifecycle.phase = LifecyclePhase::Closing;
                lifecycle.nav_generation = None;
                Ok(ClosePlan::Native {
                    label: lifecycle.label.clone(),
                    incarnation: lifecycle.incarnation,
                })
            }
            LifecyclePhase::Closing => Ok(ClosePlan::InProgress),
            LifecyclePhase::Cleaning { .. } => {
                lifecycle.phase = LifecyclePhase::Cleaning { cancelled: true };
                Ok(ClosePlan::Pending)
            }
        }
    }

    fn claim_creation_cleanup(
        &self,
        tab_id: &str,
        label: &str,
        incarnation: u64,
    ) -> Result<Option<bool>, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let ownership_matches = state
            .labels
            .get(label)
            .is_some_and(|owner| owner.tab_id == tab_id && owner.incarnation == incarnation);
        if !ownership_matches {
            return Ok(None);
        }
        let Some(lifecycle) = state.tabs.get_mut(tab_id) else {
            return Ok(None);
        };
        if lifecycle.incarnation != incarnation || lifecycle.label != label {
            return Ok(None);
        }
        let LifecyclePhase::Creating { cancelled } = lifecycle.phase else {
            return Ok(None);
        };
        lifecycle.phase = LifecyclePhase::Cleaning { cancelled };
        lifecycle.nav_generation = None;
        Ok(Some(cancelled))
    }

    fn finish_creation_cleanup(
        &self,
        tab_id: &str,
        label: &str,
        incarnation: u64,
    ) -> Result<Option<bool>, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let cancelled = state.tabs.get(tab_id).and_then(|lifecycle| {
            if lifecycle.incarnation != incarnation || lifecycle.label != label {
                return None;
            }
            match lifecycle.phase {
                LifecyclePhase::Cleaning { cancelled } => Some(cancelled),
                _ => None,
            }
        });
        let ownership_matches = state
            .labels
            .get(label)
            .is_some_and(|owner| owner.tab_id == tab_id && owner.incarnation == incarnation);
        let Some(cancelled) = cancelled.filter(|_| ownership_matches) else {
            return Ok(None);
        };
        state.tabs.remove(tab_id);
        state.labels.remove(label);
        self.changed.notify_all();
        Ok(Some(cancelled))
    }

    fn finish_close(&self, tab_id: &str, incarnation: u64) -> Result<bool, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let Some(lifecycle) = state.tabs.get(tab_id) else {
            return Ok(false);
        };
        if lifecycle.incarnation != incarnation || lifecycle.phase != LifecyclePhase::Closing {
            return Ok(false);
        }
        let label = lifecycle.label.clone();
        state.tabs.remove(tab_id);
        if state
            .labels
            .get(&label)
            .is_some_and(|owner| owner.tab_id == tab_id && owner.incarnation == incarnation)
        {
            state.labels.remove(&label);
        }
        self.changed.notify_all();
        Ok(true)
    }

    fn restore_close(&self, tab_id: &str, incarnation: u64) -> Result<bool, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let Some(lifecycle) = state.tabs.get_mut(tab_id) else {
            return Ok(false);
        };
        if lifecycle.incarnation != incarnation || lifecycle.phase != LifecyclePhase::Closing {
            return Ok(false);
        }
        lifecycle.phase = LifecyclePhase::Created;
        self.changed.notify_all();
        Ok(true)
    }

    fn record_navigation(&self, tab_id: &str, incarnation: u64) -> Result<Option<u64>, String> {
        let mut state = self.state.lock().map_err(|error| error.to_string())?;
        let is_active = state.tabs.get(tab_id).is_some_and(|lifecycle| {
            lifecycle.incarnation == incarnation && lifecycle.phase == LifecyclePhase::Created
        });
        if !is_active {
            return Ok(None);
        }
        state.next_nav_generation = state.next_nav_generation.wrapping_add(1);
        if state.next_nav_generation == 0 {
            state.next_nav_generation = 1;
        }
        let generation = state.next_nav_generation;
        if let Some(lifecycle) = state.tabs.get_mut(tab_id) {
            lifecycle.nav_generation = Some(generation);
        }
        Ok(Some(generation))
    }

    fn is_current_navigation(&self, tab_id: &str, incarnation: u64, generation: u64) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.tabs.get(tab_id).is_some_and(|lifecycle| {
                lifecycle.incarnation == incarnation
                    && lifecycle.phase == LifecyclePhase::Created
                    && lifecycle.nav_generation == Some(generation)
            })
        })
    }

    fn reconcile_bounds(
        &self,
        tab_id: &str,
        incarnation: u64,
        applied_revision: u64,
    ) -> Result<BoundsReconciliation, String> {
        let state = self.state.lock().map_err(|error| error.to_string())?;
        let Some(lifecycle) = state.tabs.get(tab_id) else {
            return Ok(BoundsReconciliation::Inactive);
        };
        if lifecycle.incarnation != incarnation || lifecycle.phase != LifecyclePhase::Created {
            return Ok(BoundsReconciliation::Inactive);
        }
        if lifecycle.revision == applied_revision {
            Ok(BoundsReconciliation::Current)
        } else {
            Ok(BoundsReconciliation::Retry {
                revision: lifecycle.revision,
            })
        }
    }

    #[cfg(test)]
    fn owns_label(&self, tab_id: &str, label: &str, incarnation: u64) -> bool {
        self.state.lock().is_ok_and(|state| {
            state
                .labels
                .get(label)
                .is_some_and(|owner| owner.tab_id == tab_id && owner.incarnation == incarnation)
        })
    }
}

fn host_coordinator() -> &'static HostCoordinator {
    static COORDINATOR: OnceLock<HostCoordinator> = OnceLock::new();
    COORDINATOR.get_or_init(HostCoordinator::default)
}

/// Ensures an unwind during native child creation cannot strand an owned label
/// or a child webview registered by this exact incarnation.
struct CreationLease {
    app: AppHandle,
    tab_id: String,
    label: String,
    incarnation: u64,
    child: Option<tauri::Webview>,
    creation_attempted: bool,
    armed: bool,
}

impl CreationLease {
    fn new(app: &AppHandle, tab_id: &str, label: &str, incarnation: u64) -> Self {
        Self {
            app: app.clone(),
            tab_id: tab_id.to_string(),
            label: label.to_string(),
            incarnation,
            child: None,
            creation_attempted: false,
            armed: true,
        }
    }

    fn mark_creation_attempted(&mut self) {
        self.creation_attempted = true;
    }

    fn attach_child(&mut self, child: &tauri::Webview) {
        self.child = Some(child.clone());
    }

    fn disarm(&mut self) {
        self.armed = false;
        self.child = None;
    }
}

impl Drop for CreationLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match host_coordinator().claim_creation_cleanup(&self.tab_id, &self.label, self.incarnation)
        {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }

        // Drop must stay best-effort and must never replace an active panic.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(webview) = self.child.as_ref() {
                let _ = webview.close();
            } else if self.creation_attempted {
                // The label was absent before add_child and remains reserved by
                // this incarnation, so a newly registered child is ours.
                if let Some(webview) = self.app.webviews().get(&self.label) {
                    let _ = webview.close();
                }
            }
        }));

        let state = self.app.state::<PreviewHostState>();
        if let Ok(mut registry) = state.0.lock() {
            let cancelled = host_coordinator()
                .finish_creation_cleanup(&self.tab_id, &self.label, self.incarnation)
                .ok()
                .flatten()
                .unwrap_or(false);
            if cancelled
                && registry
                    .get(&self.tab_id)
                    .is_some_and(|entry| entry.label == self.label && !entry.created)
            {
                registry.remove(&self.tab_id);
            }
        } else {
            let _ = host_coordinator().finish_creation_cleanup(
                &self.tab_id,
                &self.label,
                self.incarnation,
            );
        }
    }
}

fn record_nav_event(tab_id: &str, incarnation: u64) -> Option<u64> {
    host_coordinator()
        .record_navigation(tab_id, incarnation)
        .ok()
        .flatten()
}

fn is_current_nav_event(tab_id: &str, incarnation: u64, generation: u64) -> bool {
    host_coordinator().is_current_navigation(tab_id, incarnation, generation)
}

fn lock_current_nav_event(
    tab_id: &str,
    incarnation: u64,
    generation: u64,
) -> Option<MutexGuard<'static, HostLifecycleState>> {
    let state = host_coordinator().state.lock().ok()?;
    state
        .tabs
        .get(tab_id)
        .is_some_and(|lifecycle| {
            lifecycle.incarnation == incarnation
                && lifecycle.phase == LifecyclePhase::Created
                && lifecycle.nav_generation == Some(generation)
        })
        .then_some(state)
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
#[allow(dead_code)]
pub enum NavStatus {
    Idle,
    Loading {
        url: String,
        title: String,
    },
    Success {
        url: String,
        title: String,
    },
    LoadFailed {
        url: String,
        title: String,
        code: i32,
        description: String,
    },
}

/// Wire shape of `DesktopPreviewTabState`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabStatePayload {
    pub tab_id: String,
    pub web_contents_id: Option<i64>,
    pub nav_status: NavStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom_factor: f64,
    pub controller: &'static str,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateEvent {
    #[serde(rename = "tabId")]
    pub tab_id: String,
    pub state: TabStatePayload,
}

pub(crate) fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Emit a state snapshot from a worker thread.
///
/// Platform navigation metadata uses native completion callbacks, so callers
/// must not invoke this from the webview UI thread.
#[allow(clippy::too_many_arguments)]
fn emit_state(
    app: &AppHandle,
    webview: &tauri::Webview,
    tab_id: &str,
    label: &str,
    incarnation: u64,
    generation: u64,
    url: &str,
    nav_status: NavStatus,
) {
    if !is_current_nav_event(tab_id, incarnation, generation) {
        return;
    }
    let (can_go_back, can_go_forward) = (
        Platform::can_go_back(webview).unwrap_or(false),
        Platform::can_go_forward(webview).unwrap_or(false),
    );
    let state = app.state::<PreviewHostState>();
    let mut registry = match state.0.lock() {
        Ok(registry) => registry,
        Err(error) => {
            tracing::warn!("failed to lock preview registry for state event: {error}");
            return;
        }
    };
    let Some(_generation_guard) = lock_current_nav_event(tab_id, incarnation, generation) else {
        return;
    };
    let Some(entry) = registry
        .get_mut(tab_id)
        .filter(|entry| entry.created && entry.label == label)
    else {
        return;
    };
    entry.last_url = url.to_string();
    let zoom = entry.zoom;
    drop(registry);
    let payload = StateEvent {
        tab_id: tab_id.to_string(),
        state: TabStatePayload {
            tab_id: tab_id.to_string(),
            web_contents_id: None,
            nav_status,
            can_go_back,
            can_go_forward,
            zoom_factor: zoom,
            controller: "human",
            updated_at: now_iso(),
        },
    };
    if let Err(error) = app.emit(STATE_EVENT, payload) {
        tracing::warn!("failed to emit preview state: {error}");
    }
}

/// Resolve the current title via platform ops, tolerating failures.
fn current_title(webview: &tauri::Webview) -> String {
    Platform::title(webview).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn ensure_macos_isolated_profile_available(available: bool) -> Result<(), String> {
    if available {
        Ok(())
    } else {
        Err(
            "native preview requires macOS 14 or newer for an isolated persistent profile"
                .to_string(),
        )
    }
}

pub fn is_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2::available!(macos = 14.0)
    }

    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// The SHA-256 of an environment's preview partition key.
fn preview_partition_hash(environment_id: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("bibcode-preview:{environment_id}")).into()
}

/// The preview profile directory, relative to the app data directory, for an
/// environment's previews. The local environment (`None`) keeps the original
/// shared profile, so its saved preview data survives; any other environment
/// gets its own, named by a hash so the id never reaches the path.
// macOS partitions by data store instead; kept there for its tests.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn preview_profile_dir(environment_id: Option<&str>) -> std::path::PathBuf {
    match environment_id {
        None => std::path::PathBuf::from("preview-profile"),
        Some(id) => {
            let hex: String = preview_partition_hash(id)[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            std::path::Path::new("preview-profiles").join(hex)
        }
    }
}

/// The macOS website data store identifier for an environment's previews,
/// following the same partitioning as [`preview_profile_dir`].
// Other platforms partition by directory; kept there for its tests.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn preview_data_store_id(environment_id: Option<&str>) -> [u8; 16] {
    match environment_id {
        None => *b"bibcodepreview01",
        Some(id) => {
            let hash = preview_partition_hash(id);
            let mut store = [0_u8; 16];
            store.copy_from_slice(&hash[..16]);
            store
        }
    }
}

pub fn create_tab(
    app: &AppHandle,
    tab_id: &str,
    environment_id: Option<&str>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    ensure_macos_isolated_profile_available(is_supported())?;

    let blank_url = Url::parse("about:blank").map_err(|error| error.to_string())?;
    let window = app
        .get_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    #[cfg(not(target_os = "macos"))]
    let profile_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("failed to resolve preview profile directory: {error}"))?
        .join(preview_profile_dir(environment_id));

    let state = app.state::<PreviewHostState>();
    let (label, initial_bounds, progress) = {
        let mut registry = state.0.lock().map_err(|error| error.to_string())?;
        let entry = registry.upsert_pending(tab_id);
        let label = entry.label.clone();
        let bounds = entry.bounds.unwrap_or(PendingBounds {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        match host_coordinator().begin(tab_id, &label)? {
            BeginCreation::Started(progress) => (label, bounds, progress),
            BeginCreation::InProgress { incarnation } => {
                drop(registry);
                let settlement = host_coordinator().wait_until_settled(tab_id, incarnation)?;
                let registry = state.0.lock().map_err(|error| error.to_string())?;
                let created = settlement == CreationSettlement::Created
                    && registry.get(tab_id).is_some_and(|entry| {
                        entry.created
                            && entry.label == label
                            && host_coordinator()
                                .is_created(tab_id, &label, incarnation)
                                .unwrap_or(false)
                    });
                return created
                    .then_some(())
                    .ok_or_else(|| format!("preview tab {tab_id} creation did not complete"));
            }
            BeginCreation::AlreadyCreated { incarnation } => {
                let created =
                    entry.created && host_coordinator().is_created(tab_id, &label, incarnation)?;
                return created
                    .then_some(())
                    .ok_or_else(|| format!("preview tab {tab_id} lifecycle is inconsistent"));
            }
        }
    };
    let mut creation_lease = CreationLease::new(app, tab_id, &label, progress.incarnation);

    if app.webviews().contains_key(&label) {
        return Err(format!(
            "preview webview {label} already exists without lifecycle ownership"
        ));
    }

    let app_for_events = app.clone();
    let tab_for_events = tab_id.to_string();
    let label_for_events = label.clone();
    let incarnation_for_events = progress.incarnation;
    let mut builder = tauri::webview::WebviewBuilder::new(&label, WebviewUrl::External(blank_url))
        .on_navigation(|url| {
            url.as_str() == "about:blank" || matches!(url.scheme(), "http" | "https")
        })
        .on_page_load(move |webview, payload| {
            let url = payload.url().to_string();
            if url == "about:blank" {
                return;
            }
            let Some(generation) = record_nav_event(&tab_for_events, incarnation_for_events) else {
                tracing::warn!("failed to record preview navigation generation");
                return;
            };
            let app = app_for_events.clone();
            let tab_id = tab_for_events.clone();
            let label = label_for_events.clone();
            let webview = webview.clone();
            let event = payload.event();
            tauri::async_runtime::spawn_blocking(move || {
                if !is_current_nav_event(&tab_id, incarnation_for_events, generation) {
                    return;
                }
                let title = current_title(&webview);
                let nav_status = match event {
                    PageLoadEvent::Started => NavStatus::Loading {
                        url: url.clone(),
                        title,
                    },
                    PageLoadEvent::Finished => NavStatus::Success {
                        url: url.clone(),
                        title,
                    },
                };
                emit_state(
                    &app,
                    &webview,
                    &tab_id,
                    &label,
                    incarnation_for_events,
                    generation,
                    &url,
                    nav_status,
                );
            });
        })
        .on_new_window({
            let app = app.clone();
            let tab_id = tab_id.to_string();
            move |url, _features| {
                if let Some(url) = preview_new_window_url(&url) {
                    let payload = NewWindowEvent {
                        tab_id: tab_id.clone(),
                        url: url.to_string(),
                    };
                    if let Err(error) = app.emit(NEW_WINDOW_EVENT, payload) {
                        tracing::warn!("failed to emit preview new-window request: {error}");
                    }
                }
                tauri::webview::NewWindowResponse::Deny
            }
        });

    // Preview browsing data is isolated from the main application session and,
    // per environment, from other environments' previews.
    #[cfg(target_os = "macos")]
    {
        builder = builder.data_store_identifier(preview_data_store_id(environment_id));
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.data_directory(profile_dir);
    }

    creation_lease.mark_creation_attempted();
    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(initial_bounds.x, initial_bounds.y),
            LogicalSize::new(
                initial_bounds.width.max(1.0),
                initial_bounds.height.max(1.0),
            ),
        )
        .map_err(|error| error.to_string())?;
    creation_lease.attach_child(&webview);

    loop {
        let snapshot = match (|| -> Result<Option<_>, String> {
            let registry = state.0.lock().map_err(|error| error.to_string())?;
            let Some(entry) = registry
                .get(tab_id)
                .filter(|entry| entry.label == label && !entry.created)
            else {
                return Ok(None);
            };
            let Some((current, cancelled)) =
                host_coordinator().current_creation(tab_id, progress.incarnation)?
            else {
                return Ok(None);
            };
            if cancelled {
                return Ok(None);
            }
            Ok(Some((
                entry.bounds.unwrap_or(PendingBounds {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }),
                entry.visible,
                current.revision,
            )))
        })() {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                return Err(format!("preview tab {tab_id} creation was cancelled"));
            }
            Err(error) => return Err(error),
        };

        let apply_result = webview
            .set_position(LogicalPosition::new(snapshot.0.x, snapshot.0.y))
            .and_then(|()| {
                webview.set_size(LogicalSize::new(
                    snapshot.0.width.max(1.0),
                    snapshot.0.height.max(1.0),
                ))
            })
            .and_then(|()| {
                if snapshot.1 {
                    webview.show()
                } else {
                    webview.hide()
                }
            });
        if let Err(error) = apply_result {
            return Err(error.to_string());
        }

        let commit = (|| -> Result<CreationCommit, String> {
            let mut registry = state.0.lock().map_err(|error| error.to_string())?;
            let Some(entry) = registry
                .get_mut(tab_id)
                .filter(|entry| entry.label == label && !entry.created)
            else {
                return Ok(CreationCommit::Cancelled);
            };
            let outcome =
                host_coordinator().commit_creation(tab_id, progress.incarnation, snapshot.2)?;
            if outcome == CreationCommit::Committed {
                entry.created = true;
            }
            Ok(outcome)
        })();
        match commit {
            Ok(CreationCommit::Committed) => {
                creation_lease.disarm();
                return Ok(());
            }
            Ok(CreationCommit::Retry) => {}
            Ok(CreationCommit::Cancelled) => {
                return Err(format!("preview tab {tab_id} creation was cancelled"));
            }
            Err(error) => return Err(error),
        }
    }
}

fn tab_webview(app: &AppHandle, tab_id: &str) -> Result<(tauri::Webview, u64), String> {
    let state = app.state::<PreviewHostState>();
    let (webview, incarnation) = {
        let registry = state.0.lock().map_err(|error| error.to_string())?;
        let entry = registry
            .get(tab_id)
            .filter(|entry| entry.created)
            .ok_or_else(|| format!("preview tab {tab_id} does not exist"))?;
        let lifecycle = host_coordinator()
            .state
            .lock()
            .map_err(|error| error.to_string())?;
        let incarnation = lifecycle
            .tabs
            .get(tab_id)
            .filter(|lifecycle| {
                lifecycle.phase == LifecyclePhase::Created && lifecycle.label == entry.label
            })
            .map(|lifecycle| lifecycle.incarnation)
            .ok_or_else(|| format!("preview tab {tab_id} lifecycle is not active"))?;
        let webview = app
            .webviews()
            .get(&entry.label)
            .cloned()
            .ok_or_else(|| format!("preview webview {} not found", entry.label))?;
        (webview, incarnation)
    };
    Ok((webview, incarnation))
}

pub fn with_tab_webview<T>(
    app: &AppHandle,
    tab_id: &str,
    operation: impl FnOnce(&tauri::Webview) -> Result<T, String>,
) -> Result<T, String> {
    let (webview, _incarnation) = tab_webview(app, tab_id)?;
    operation(&webview)
}

pub fn platform_err(error: PreviewPlatformError) -> String {
    error.to_string()
}

pub fn set_zoom(app: &AppHandle, tab_id: &str, factor: f64) -> Result<(), String> {
    let (webview, incarnation) = tab_webview(app, tab_id)?;
    webview
        .set_zoom(factor)
        .map_err(|error| error.to_string())?;

    let state = app.state::<PreviewHostState>();
    let mut registry = state.0.lock().map_err(|error| error.to_string())?;
    let Some(entry) = registry.get_mut(tab_id) else {
        return Ok(());
    };
    if host_coordinator().is_created(tab_id, &entry.label, incarnation)? {
        entry.zoom = factor;
    }
    Ok(())
}

pub fn set_bounds(
    app: &AppHandle,
    tab_id: &str,
    bounds: PendingBounds,
    visible: bool,
) -> Result<(), String> {
    #[derive(Clone)]
    struct ApplySnapshot {
        webview: tauri::Webview,
        incarnation: u64,
        revision: u64,
        bounds: PendingBounds,
        visible: bool,
    }

    let state = app.state::<PreviewHostState>();
    let mut snapshot = {
        let mut registry = state.0.lock().map_err(|error| error.to_string())?;
        match host_coordinator().begin_bounds_update(tab_id)? {
            BoundsUpdate::Ignore => return Ok(()),
            BoundsUpdate::Pending => {
                let entry = registry.upsert_pending(tab_id);
                entry.bounds = Some(bounds);
                entry.visible = visible;
                None
            }
            BoundsUpdate::Creating(_progress) => {
                let entry = registry.upsert_pending(tab_id);
                entry.bounds = Some(bounds);
                entry.visible = visible;
                None
            }
            BoundsUpdate::Created { progress, label } => {
                let Some(entry) = registry
                    .get_mut(tab_id)
                    .filter(|entry| entry.created && entry.label == label)
                else {
                    return Err(format!("preview tab {tab_id} lifecycle is inconsistent"));
                };
                entry.bounds = Some(bounds);
                entry.visible = visible;
                let webview = app
                    .webviews()
                    .get(&label)
                    .cloned()
                    .ok_or_else(|| format!("preview webview {label} not found"))?;
                Some(ApplySnapshot {
                    webview,
                    incarnation: progress.incarnation,
                    revision: progress.revision,
                    bounds,
                    visible,
                })
            }
        }
    };

    while let Some(current) = snapshot {
        current
            .webview
            .set_position(LogicalPosition::new(current.bounds.x, current.bounds.y))
            .map_err(|error| error.to_string())?;
        current
            .webview
            .set_size(LogicalSize::new(
                current.bounds.width.max(1.0),
                current.bounds.height.max(1.0),
            ))
            .map_err(|error| error.to_string())?;
        if current.visible {
            current.webview.show().map_err(|error| error.to_string())?;
        } else {
            current.webview.hide().map_err(|error| error.to_string())?;
        }

        snapshot = {
            let registry = state.0.lock().map_err(|error| error.to_string())?;
            match host_coordinator().reconcile_bounds(
                tab_id,
                current.incarnation,
                current.revision,
            )? {
                BoundsReconciliation::Current | BoundsReconciliation::Inactive => None,
                BoundsReconciliation::Retry { revision } => {
                    let Some(entry) = registry.get(tab_id).filter(|entry| entry.created) else {
                        return Ok(());
                    };
                    let Some(bounds) = entry.bounds else {
                        return Ok(());
                    };
                    Some(ApplySnapshot {
                        webview: current.webview,
                        incarnation: current.incarnation,
                        revision,
                        bounds,
                        visible: entry.visible,
                    })
                }
            }
        };
    }
    Ok(())
}

pub fn close_tab(app: &AppHandle, tab_id: &str) -> Result<(), String> {
    let state = app.state::<PreviewHostState>();
    let plan = {
        let mut registry = state.0.lock().map_err(|error| error.to_string())?;
        let plan = host_coordinator().begin_close(tab_id)?;
        if matches!(plan, ClosePlan::Absent | ClosePlan::Pending) {
            registry.remove(tab_id);
        }
        plan
    };

    let ClosePlan::Native { label, incarnation } = plan else {
        return Ok(());
    };
    let close_result = app
        .webviews()
        .get(&label)
        .map(|webview| webview.close().map_err(|error| error.to_string()))
        .unwrap_or(Ok(()));

    let mut registry = state.0.lock().map_err(|error| error.to_string())?;
    match close_result {
        Ok(()) => {
            if host_coordinator().finish_close(tab_id, incarnation)?
                && registry
                    .get(tab_id)
                    .is_some_and(|entry| entry.label == label)
            {
                registry.remove(tab_id);
            }
            Ok(())
        }
        Err(error) => {
            let _ = host_coordinator().restore_close(tab_id, incarnation)?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn local_environment_keeps_the_legacy_preview_profile() {
        assert_eq!(
            super::preview_profile_dir(None),
            std::path::Path::new("preview-profile")
        );
        assert_eq!(super::preview_data_store_id(None), *b"bibcodepreview01");
    }

    #[test]
    fn each_environment_gets_its_own_stable_preview_profile() {
        let a = super::preview_profile_dir(Some("env-a"));
        assert_eq!(a.parent(), Some(std::path::Path::new("preview-profiles")));
        let name = a.file_name().and_then(|name| name.to_str()).expect("name");
        assert_eq!(name.len(), 32);
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(a, super::preview_profile_dir(Some("env-a")), "stable");
        assert_ne!(a, super::preview_profile_dir(Some("env-b")));

        // The id never reaches the path: only the hash does.
        let evil = super::preview_profile_dir(Some("../evil"));
        assert_eq!(evil.components().count(), 2, "{}", evil.display());
        assert!(!evil.to_string_lossy().contains("evil"));

        let store_a = super::preview_data_store_id(Some("env-a"));
        assert_ne!(store_a, *b"bibcodepreview01");
        assert_ne!(store_a, super::preview_data_store_id(Some("env-b")));
        assert_eq!(store_a, super::preview_data_store_id(Some("env-a")));
        // Only an absent environment is the local one.
        assert_ne!(
            super::preview_profile_dir(Some("")),
            super::preview_profile_dir(None)
        );
        assert_ne!(
            super::preview_data_store_id(Some("")),
            super::preview_data_store_id(None)
        );
    }

    use super::{
        BeginCreation, BoundsReconciliation, BoundsUpdate, ClosePlan, CreationCommit,
        CreationSettlement, HostCoordinator, NavStatus, StateEvent, TabStatePayload,
    };
    use crate::preview::webview_label_for_tab;

    #[test]
    fn coordinator_retains_incarnation_after_creation_and_reconciles_revisions() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(progress) = coordinator.begin("tab-1", &label).expect("begin")
        else {
            panic!("first create should start");
        };
        assert_eq!(
            coordinator.begin("tab-1", &label),
            Ok(BeginCreation::InProgress {
                incarnation: progress.incarnation,
            })
        );

        let BoundsUpdate::Creating(current) = coordinator
            .begin_bounds_update("tab-1")
            .expect("bump revision")
        else {
            panic!("active creation bounds update");
        };
        assert_eq!(current.revision, 1);
        assert_eq!(
            coordinator
                .commit_creation("tab-1", progress.incarnation, progress.revision)
                .expect("stale commit"),
            CreationCommit::Retry
        );
        let (current, cancelled) = coordinator
            .current_creation("tab-1", progress.incarnation)
            .expect("read current")
            .expect("current creation");
        assert_eq!(current.revision, 1);
        assert!(!cancelled);
        assert_eq!(
            coordinator
                .commit_creation("tab-1", progress.incarnation, current.revision)
                .expect("current commit"),
            CreationCommit::Committed
        );
        assert_eq!(
            coordinator.begin("tab-1", &label),
            Ok(BeginCreation::AlreadyCreated {
                incarnation: progress.incarnation,
            })
        );
        assert!(
            coordinator
                .is_created("tab-1", &label, progress.incarnation)
                .unwrap()
        );
    }

    #[test]
    fn cancelled_creation_retains_its_slot_until_creator_cleanup() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(progress) = coordinator.begin("tab-1", &label).expect("begin")
        else {
            panic!("first create should start");
        };

        assert_eq!(
            coordinator.begin_close("tab-1").unwrap(),
            ClosePlan::Pending
        );
        assert_eq!(
            coordinator.begin("tab-1", &label),
            Ok(BeginCreation::InProgress {
                incarnation: progress.incarnation,
            })
        );
        assert!(
            coordinator
                .current_creation("tab-1", progress.incarnation)
                .expect("read current")
                .expect("cancelled creation is retained")
                .1
        );

        assert_eq!(
            coordinator
                .claim_creation_cleanup("tab-1", &label, progress.incarnation)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            coordinator
                .finish_creation_cleanup("tab-1", &label, progress.incarnation)
                .unwrap(),
            Some(true)
        );
        let BeginCreation::Started(retry) = coordinator
            .begin("tab-1", &label)
            .expect("retry after cleanup")
        else {
            panic!("retry should start a new incarnation");
        };
        assert_ne!(retry.incarnation, progress.incarnation);
    }

    #[test]
    fn sanitized_label_collision_is_rejected_while_first_tab_is_owned() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("a b");
        assert_eq!(label, webview_label_for_tab("a/b"));
        assert!(matches!(
            coordinator
                .begin("a b", &label)
                .expect("first tab owns label"),
            BeginCreation::Started(_)
        ));

        assert!(
            coordinator.begin("a/b", &label).is_err(),
            "different tab IDs sanitizing to one label must not create two children",
        );
    }

    #[test]
    fn cleanup_from_old_incarnation_cannot_release_new_label_owner() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(old) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("old creation");
        };
        assert_eq!(
            coordinator
                .claim_creation_cleanup("tab-1", &label, old.incarnation)
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            coordinator
                .finish_creation_cleanup("tab-1", &label, old.incarnation)
                .unwrap(),
            Some(false)
        );
        let BeginCreation::Started(new) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("new creation");
        };

        assert_eq!(
            coordinator
                .claim_creation_cleanup("tab-1", &label, old.incarnation)
                .unwrap(),
            None
        );
        assert_eq!(
            coordinator
                .finish_creation_cleanup("tab-1", &label, old.incarnation)
                .unwrap(),
            None
        );
        assert!(coordinator.owns_label("tab-1", &label, new.incarnation));
    }

    #[test]
    fn late_callback_from_closed_incarnation_cannot_replace_new_navigation() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(old) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("old creation");
        };
        assert_eq!(
            coordinator
                .commit_creation("tab-1", old.incarnation, old.revision)
                .unwrap(),
            CreationCommit::Committed
        );
        let old_navigation = coordinator
            .record_navigation("tab-1", old.incarnation)
            .unwrap()
            .expect("old navigation");
        let ClosePlan::Native { incarnation, .. } = coordinator.begin_close("tab-1").unwrap()
        else {
            panic!("native close");
        };
        assert!(coordinator.finish_close("tab-1", incarnation).unwrap());

        let BeginCreation::Started(new) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("new creation");
        };
        coordinator
            .commit_creation("tab-1", new.incarnation, new.revision)
            .unwrap();
        let new_navigation = coordinator
            .record_navigation("tab-1", new.incarnation)
            .unwrap()
            .expect("new navigation");

        assert_eq!(
            coordinator
                .record_navigation("tab-1", old.incarnation)
                .unwrap(),
            None
        );
        assert!(!coordinator.is_current_navigation("tab-1", old.incarnation, old_navigation));
        assert!(coordinator.is_current_navigation("tab-1", new.incarnation, new_navigation));
    }

    #[test]
    fn duplicate_wait_is_specific_to_the_observed_incarnation() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(old) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("old creation");
        };
        assert_eq!(
            coordinator
                .claim_creation_cleanup("tab-1", &label, old.incarnation)
                .unwrap(),
            Some(false)
        );
        coordinator
            .finish_creation_cleanup("tab-1", &label, old.incarnation)
            .unwrap();
        let BeginCreation::Started(_new) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("new creation");
        };

        assert_eq!(
            coordinator
                .wait_until_settled("tab-1", old.incarnation)
                .unwrap(),
            CreationSettlement::Superseded
        );
    }

    #[test]
    fn close_during_cleanup_marks_the_exact_incarnation_cancelled() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(progress) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("creation");
        };
        assert_eq!(
            coordinator
                .claim_creation_cleanup("tab-1", &label, progress.incarnation)
                .unwrap(),
            Some(false)
        );

        assert_eq!(
            coordinator.begin_close("tab-1").unwrap(),
            ClosePlan::Pending
        );
        assert_eq!(
            coordinator
                .finish_creation_cleanup("tab-1", &label, progress.incarnation)
                .unwrap(),
            Some(true)
        );
    }

    #[test]
    fn bounds_reconciliation_retries_only_the_same_active_incarnation() {
        let coordinator = HostCoordinator::default();
        let label = webview_label_for_tab("tab-1");
        let BeginCreation::Started(progress) = coordinator.begin("tab-1", &label).unwrap() else {
            panic!("creation");
        };
        coordinator
            .commit_creation("tab-1", progress.incarnation, progress.revision)
            .unwrap();
        assert_eq!(
            coordinator
                .reconcile_bounds("tab-1", progress.incarnation, 0)
                .unwrap(),
            BoundsReconciliation::Current
        );
        let BoundsUpdate::Created {
            progress: update, ..
        } = coordinator.begin_bounds_update("tab-1").unwrap()
        else {
            panic!("active created bounds update");
        };
        assert_eq!(
            coordinator
                .reconcile_bounds("tab-1", progress.incarnation, 0)
                .unwrap(),
            BoundsReconciliation::Retry {
                revision: update.revision,
            }
        );
        coordinator.begin_close("tab-1").unwrap();
        assert_eq!(
            coordinator
                .reconcile_bounds("tab-1", progress.incarnation, update.revision)
                .unwrap(),
            BoundsReconciliation::Inactive
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_support_fails_closed_below_version_14() {
        assert!(super::ensure_macos_isolated_profile_available(true).is_ok());
        let error = super::ensure_macos_isolated_profile_available(false).unwrap_err();
        assert!(error.contains("macOS 14 or newer"));
    }

    #[test]
    fn state_event_serializes_with_the_desktop_preview_contract_shape() {
        let event = StateEvent {
            tab_id: "tab-1".to_string(),
            state: TabStatePayload {
                tab_id: "tab-1".to_string(),
                web_contents_id: None,
                nav_status: NavStatus::Success {
                    url: "https://example.com/".to_string(),
                    title: "Example".to_string(),
                },
                can_go_back: true,
                can_go_forward: false,
                zoom_factor: 1.25,
                controller: "human",
                updated_at: "2026-07-20T00:00:00Z".to_string(),
            },
        };

        assert_eq!(
            serde_json::to_value(event).expect("state event should serialize"),
            json!({
                "tabId": "tab-1",
                "state": {
                    "tabId": "tab-1",
                    "webContentsId": null,
                    "navStatus": {
                        "kind": "Success",
                        "url": "https://example.com/",
                        "title": "Example",
                    },
                    "canGoBack": true,
                    "canGoForward": false,
                    "zoomFactor": 1.25,
                    "controller": "human",
                    "updatedAt": "2026-07-20T00:00:00Z",
                },
            })
        );
    }

    #[test]
    fn preview_new_window_only_forwards_web_urls() {
        let web = tauri::Url::parse("https://example.com/a").unwrap();
        assert_eq!(super::preview_new_window_url(&web), Some(&web));
        let data = tauri::Url::parse("data:text/html,hi").unwrap();
        assert_eq!(super::preview_new_window_url(&data), None);
    }
}
