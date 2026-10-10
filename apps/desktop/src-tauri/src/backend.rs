use bibcode_server::diagnostics::{DesktopUiProcessObserver, UnavailableDesktopUiProcessObserver};
#[cfg(unix)]
use bibcode_server::process::isolate_appimage_environment;
use bibcode_server::process::{configure_background_command, configure_background_std_command};
use bibcode_server::{
    DESKTOP_SHUTDOWN_PATH as SERVER_BACKEND_SHUTDOWN_PATH,
    DESKTOP_SHUTDOWN_TOKEN_HEADER as SERVER_BACKEND_SHUTDOWN_TOKEN_HEADER, DataRootRequest,
    DataRootSource, ResolvedDataRoot, ServerConfig, ServerError, ServerRuntime,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Runtime};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt as TokioAsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex as AsyncMutex, Notify, oneshot, watch},
};
use uuid::Uuid;

use crate::config::state_dir;
use crate::network_interfaces::{classify_advertised_address, default_route_ip};
#[cfg(test)]
use crate::test_support::FixtureEvent;

mod ui_process_observer;

const PRIMARY_LOCAL_ENVIRONMENT_ID: &str = "primary";
const DESKTOP_MODE: &str = "desktop";
const BACKEND_BOOTSTRAP_FD: &str = "0";
const TAILSCALE_SERVE_PORT: u16 = 443;
const DESKTOP_LOOPBACK_HOST: &str = "127.0.0.1";
const DESKTOP_LAN_BIND_HOST: &str = "0.0.0.0";
const DEFAULT_BACKEND_PORT: u16 = 3773;
const MAX_TCP_PORT: u16 = u16::MAX;
const DESKTOP_BACKEND_PORT_PROBE_HOSTS: [&str; 3] = ["127.0.0.1", "0.0.0.0", "::"];
pub const BACKEND_READY_EVENT: &str = "desktop:backend-ready";
pub const PROJECT_DATA_STATUS_CHANGED_EVENT: &str = "desktop:project-data-status-changed";
const WSL_SERVER_BINARY_ENV: &str = "BIBCODE_WSL_SERVER_BINARY";
const DESKTOP_SETTINGS_FILE_NAME: &str = "desktop-settings.json";
const BACKEND_READINESS_PATH: &str = "/.well-known/bibcode/environment";
const DEFAULT_BACKEND_READINESS_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_BACKEND_READINESS_INTERVAL: Duration = Duration::from_millis(250);
const DEFAULT_BACKEND_READINESS_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_BACKEND_SOFT_SHUTDOWN_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
const DEFAULT_BACKEND_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_BACKEND_RESTART_INITIAL_DELAY: Duration = Duration::from_millis(250);
const DEFAULT_BACKEND_RESTART_MAX_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_BACKEND_MONITOR_INTERVAL: Duration = Duration::from_millis(250);
/// How long a restart keeps retrying its in-process listener bind on the port
/// its stopped predecessor held, while that port is still being released.
const SAME_PORT_RESTART_BIND_RETRY: Duration = Duration::from_secs(3);
const PRIMARY_BACKEND_LOG_FILE_NAME: &str = "server-child.log";
const WSL_BACKEND_LOG_FILE_PREFIX: &str = "server-child-wsl-";
const WSL_BACKEND_LOG_FILE_EXTENSION: &str = ".log";
const WSL_INSTANCE_ID_PREFIX: &str = "wsl:";
const WSL_BACKEND_BIND_HOST: &str = "0.0.0.0";
const WSL_SERVER_SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRunConfig {
    pub environment_id: String,
    pub label: String,
    pub running_distro: Option<String>,
    pub port: u16,
    pub bind_host: String,
    pub local_host: String,
    pub desktop_bootstrap_token: String,
    pub server_exposure_mode: String,
    pub endpoint_url: Option<String>,
    pub advertised_host: Option<String>,
    pub tailscale_serve_enabled: bool,
    pub tailscale_serve_port: u16,
}

impl BackendRunConfig {
    pub fn http_base_url(&self) -> String {
        format!("http://{}:{}", self.local_host, self.port)
    }

    pub fn ws_base_url(&self) -> String {
        format!("ws://{}:{}", self.local_host, self.port)
    }

    pub fn to_environment_bootstrap(&self) -> Value {
        json!({
            "id": &self.environment_id,
            "label": &self.label,
            "runningDistro": &self.running_distro,
            "httpBaseUrl": self.http_base_url(),
            "wsBaseUrl": self.ws_base_url(),
            "bootstrapToken": self.desktop_bootstrap_token,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackendDesktopSettings {
    server_exposure_mode: String,
    tailscale_serve_enabled: bool,
    tailscale_serve_port: u16,
    wsl_backend_enabled: bool,
    wsl_only: bool,
    wsl_distro: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendDesktopSettingsDocument {
    server_exposure_mode: Option<String>,
    tailscale_serve_enabled: Option<bool>,
    tailscale_serve_port: Option<u64>,
    wsl_backend_enabled: Option<bool>,
    wsl_only: Option<bool>,
    wsl_distro: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendLaunchPlan {
    pub target: BackendLaunchTarget,
    pub log_path: Option<PathBuf>,
    pub config: BackendRunConfig,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub(crate) enum BackendPlanError {
    #[error("WSL primary is unavailable: {detail}")]
    WslPrimaryUnavailable { detail: String },
    #[error("desktop backend planning failed: {detail}")]
    Other { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackendUnavailableEnvironment {
    pub environment_id: String,
    pub label: String,
    pub configured_distro: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultLaunchPlans {
    plans: Vec<BackendLaunchPlan>,
    unavailable_secondary: Option<BackendUnavailableEnvironment>,
}

impl BackendUnavailableEnvironment {
    fn to_environment_bootstrap(&self) -> Value {
        json!({
            "id": &self.environment_id,
            "label": &self.label,
            "configuredDistro": &self.configured_distro,
            "runningDistro": null,
            "httpBaseUrl": null,
            "wsBaseUrl": null,
            "preflightError": {
                "kind": "wsl-secondary-unavailable",
                "detail": &self.detail,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendLaunchTarget {
    InProcess {
        base_dir: PathBuf,
        data_root: ResolvedDataRoot,
    },
    ExternalProcess {
        program: String,
        args: Vec<String>,
        bootstrap_line: String,
        data_root: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslBackendLaunchPlanInput {
    pub environment_id: String,
    pub label: String,
    pub running_distro: String,
    pub port: u16,
    pub renderer_host: String,
    pub desktop_bootstrap_token: String,
    pub binary_path: String,
    pub data_root: String,
}

impl BackendLaunchPlan {
    pub fn local(base_dir: PathBuf, config: BackendRunConfig) -> Self {
        let data_root = ResolvedDataRoot {
            source: DataRootSource::Cli,
            requested: base_dir.clone(),
            effective: base_dir.clone(),
            is_filesystem_alias: false,
        };
        Self {
            target: BackendLaunchTarget::InProcess {
                base_dir,
                data_root,
            },
            log_path: None,
            config,
        }
    }

    fn with_data_root(mut self, data_root: ResolvedDataRoot) -> Self {
        if let BackendLaunchTarget::InProcess {
            base_dir,
            data_root: target_data_root,
        } = &mut self.target
        {
            *base_dir = data_root.effective.clone();
            *target_data_root = data_root;
        }
        self
    }

    pub fn with_log_path(mut self, log_path: PathBuf) -> Self {
        self.log_path = Some(log_path);
        self
    }

    pub fn wsl(input: WslBackendLaunchPlanInput) -> Self {
        let config = BackendRunConfig {
            environment_id: input.environment_id,
            label: input.label,
            running_distro: Some(input.running_distro.clone()),
            port: input.port,
            bind_host: WSL_BACKEND_BIND_HOST.to_string(),
            local_host: input.renderer_host,
            desktop_bootstrap_token: input.desktop_bootstrap_token,
            server_exposure_mode: "local-only".to_string(),
            endpoint_url: None,
            advertised_host: None,
            tailscale_serve_enabled: false,
            tailscale_serve_port: TAILSCALE_SERVE_PORT,
        };
        let bootstrap = json!({
            "mode": DESKTOP_MODE,
            "noBrowser": true,
            "port": config.port,
            "host": &config.bind_host,
            "desktopBootstrapToken": &config.desktop_bootstrap_token,
            "bibcodeHome": &input.data_root,
            "wslTransport": true,
            "tailscaleServeEnabled": false,
            "tailscaleServePort": TAILSCALE_SERVE_PORT,
        });
        let args = vec![
            "-d".to_string(),
            input.running_distro,
            "--exec".to_string(),
            "env".to_string(),
            format!("PATH={WSL_SERVER_SYSTEM_PATH}"),
            input.binary_path,
            "serve".to_string(),
            "--bootstrap-fd".to_string(),
            BACKEND_BOOTSTRAP_FD.to_string(),
        ];

        Self {
            target: BackendLaunchTarget::ExternalProcess {
                program: "wsl.exe".to_string(),
                args,
                bootstrap_line: format!("{bootstrap}\n"),
                data_root: Some(input.data_root),
            },
            log_path: None,
            config,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendReadinessConfig {
    pub timeout: Duration,
    pub interval: Duration,
    pub request_timeout: Duration,
}

impl Default for BackendReadinessConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_BACKEND_READINESS_TIMEOUT,
            interval: DEFAULT_BACKEND_READINESS_INTERVAL,
            request_timeout: DEFAULT_BACKEND_READINESS_REQUEST_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendShutdownConfig {
    pub timeout: Duration,
}

impl Default for BackendShutdownConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_BACKEND_SHUTDOWN_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendRestartConfig {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub monitor_interval: Duration,
}

impl Default for BackendRestartConfig {
    fn default() -> Self {
        Self {
            initial_delay: DEFAULT_BACKEND_RESTART_INITIAL_DELAY,
            max_delay: DEFAULT_BACKEND_RESTART_MAX_DELAY,
            monitor_interval: DEFAULT_BACKEND_MONITOR_INTERVAL,
        }
    }
}

#[derive(Clone)]
struct ManagedBackendChild {
    run_id: u64,
    config: BackendRunConfig,
    child: Arc<AsyncMutex<Child>>,
    stop_requested: Arc<AtomicBool>,
}

impl ManagedBackendChild {
    fn new(run_id: u64, config: BackendRunConfig, child: Child) -> Self {
        Self {
            run_id,
            config,
            child: Arc::new(AsyncMutex::new(child)),
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
    }
}

impl std::fmt::Debug for ManagedBackendChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedBackendChild")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

/// The runtime's maintenance owner stays off the comparable launch configuration.
#[derive(Clone)]
pub(crate) struct InProcessUpdateMaintenance(pub(crate) Arc<bibcode_server::UpdateMaintenance>);

impl std::fmt::Debug for InProcessUpdateMaintenance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InProcessUpdateMaintenance")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct ManagedBackendRuntime {
    run_id: u64,
    // Supervisor slots can outlive their runtime; update snapshots retain strong owners.
    update_maintenance: Option<Weak<bibcode_server::UpdateMaintenance>>,
    stop_requested: Arc<AtomicBool>,
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    completion: Arc<Notify>,
    join_result: Arc<AsyncMutex<Option<Result<(), String>>>>,
    #[cfg(test)]
    stop_requested_event: Arc<FixtureEvent>,
}

impl ManagedBackendRuntime {
    fn new(run_id: u64, handle: bibcode_server::ServerHandle) -> Self {
        let update_maintenance = handle
            .update_maintenance()
            .map(|maintenance| Arc::downgrade(&maintenance));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let completion = Arc::new(Notify::new());
        let join_result = Arc::new(AsyncMutex::new(None));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let completion_task = completion.clone();
        let join_result_task = join_result.clone();

        tauri::async_runtime::spawn(async move {
            let handle = handle;
            tokio::select! {
                _ = &mut shutdown_rx => {
                    handle.shutdown();
                }
                () = handle.wait_for_shutdown() => {}
            }

            let result = handle.join().await.map_err(|error| error.to_string());
            *join_result_task.lock().await = Some(result);
            completion_task.notify_waiters();
        });

        Self {
            run_id,
            update_maintenance,
            stop_requested,
            shutdown: Arc::new(Mutex::new(Some(shutdown_tx))),
            completion,
            join_result,
            #[cfg(test)]
            stop_requested_event: Arc::default(),
        }
    }

    fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        #[cfg(test)]
        self.stop_requested_event.publish();
        if let Ok(mut shutdown) = self.shutdown.lock()
            && let Some(sender) = shutdown.take()
        {
            let _ = sender.send(());
        }
    }

    async fn wait_for_completion(&self) -> Result<(), String> {
        loop {
            let notified = self.completion.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if let Some(result) = self.join_result.lock().await.clone() {
                return result;
            }
            notified.await;
        }
    }
}

impl std::fmt::Debug for ManagedBackendRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedBackendRuntime")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
enum ManagedBackend {
    Child(Box<ManagedBackendChild>),
    Runtime(Box<ManagedBackendRuntime>),
}

#[derive(Debug, Default)]
struct BackendSlotState {
    launch_plan: Option<BackendLaunchPlan>,
    backend: Option<ManagedBackend>,
    pid: Option<u32>,
    last_error: Option<String>,
    recovery_failure: Option<BackendStartFailure>,
    plan_error: Option<BackendPlanError>,
    unavailable: Option<BackendUnavailableEnvironment>,
    restart_attempt: u32,
    restart_scheduled: bool,
}

#[derive(Debug, Default)]
struct BackendState {
    slots: BTreeMap<String, BackendSlotState>,
    published_recovery: Vec<BackendRecoveryEntry>,
    next_run_id: u64,
    in_flight_starts: usize,
    update_coordination: bool,
    lifecycle: BackendLifecycle,
}

impl BackendState {
    fn backend_recovery(&self) -> Vec<BackendRecoveryEntry> {
        self.slots
            .values()
            .filter_map(|slot| {
                let plan = slot.launch_plan.as_ref()?;
                let failure = slot.recovery_failure.as_ref()?;
                slot.backend
                    .is_none()
                    .then(|| BackendRecoveryEntry::new(plan, failure.clone()))
            })
            .collect()
    }
}

#[derive(Debug)]
enum BackendLifecycle {
    Active {
        epoch: u64,
    },
    Stopping {
        epoch: u64,
        terminate: bool,
        completion: watch::Receiver<Option<Result<(), String>>>,
        late_start_cleanup_error: Option<String>,
    },
    Stopped {
        epoch: u64,
        result: Result<(), String>,
    },
    Terminated {
        result: Result<(), String>,
    },
}

impl Default for BackendLifecycle {
    fn default() -> Self {
        Self::Active { epoch: 0 }
    }
}

#[derive(Debug)]
struct BackendStartPermit {
    epoch: u64,
    run_id: u64,
    state: Arc<Mutex<BackendState>>,
    start_completed: Arc<Notify>,
}

impl BackendStartPermit {
    fn record_cleanup_error(&self, error: String) {
        let mut state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        if let BackendLifecycle::Stopping {
            late_start_cleanup_error,
            ..
        } = &mut state.lifecycle
        {
            late_start_cleanup_error
                .get_or_insert_with(|| format!("Late desktop backend cleanup failed: {error}"));
        }
    }
}

impl Drop for BackendStartPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            debug_assert!(state.in_flight_starts > 0);
            state.in_flight_starts = state.in_flight_starts.saturating_sub(1);
        }
        self.start_completed.notify_waiters();
    }
}

fn backend_slot_key(plan: &BackendLaunchPlan) -> String {
    plan.config.environment_id.clone()
}

#[cfg(test)]
type BackendStartPublishGate = (oneshot::Sender<ManagedBackend>, oneshot::Receiver<()>);

/// Each start's slot key and the listener bind retry window it passed on.
#[cfg(test)]
type ListenerBindRetryRecord = Vec<(String, Option<Duration>)>;

trait WslCommandResolver: Send + Sync {
    fn command(&self) -> std::process::Command;

    fn server_binary_candidates(&self) -> Result<Vec<PathBuf>, String>;
}

trait BackendPortResolver: Send + Sync {
    fn port(&self) -> u16;
}

#[derive(Debug, Default)]
struct SystemBackendPortResolver;

impl BackendPortResolver for SystemBackendPortResolver {
    fn port(&self) -> u16 {
        crate::config::bibcode_env_var("BIBCODE_PORT")
            .and_then(|value| value.into_string().ok())
            .and_then(|value| value.parse::<u16>().ok())
            .or_else(select_desktop_backend_port)
            .or_else(portpicker::pick_unused_port)
            .unwrap_or(DEFAULT_BACKEND_PORT)
    }
}

#[derive(Debug, Default)]
struct SystemWslCommandResolver;

impl WslCommandResolver for SystemWslCommandResolver {
    fn command(&self) -> std::process::Command {
        std::process::Command::new("wsl.exe")
    }

    fn server_binary_candidates(&self) -> Result<Vec<PathBuf>, String> {
        let mut candidates = Vec::new();
        if let Some(path) = crate::config::bibcode_env_var(WSL_SERVER_BINARY_ENV)
            && !path.is_empty()
        {
            candidates.push(PathBuf::from(path));
        }
        let current_dir = std::env::current_dir().map_err(|error| {
            format!("Could not resolve current directory for WSL binary discovery: {error}")
        })?;
        let target_root = current_dir.join("target");
        for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            for profile in ["debug", "release"] {
                candidates.push(target_root.join(triple).join(profile).join("bibcode"));
            }
        }
        Ok(candidates)
    }
}

type BackendRecoveryListener = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct BackendSupervisor {
    state: Arc<Mutex<BackendState>>,
    start_completed: Arc<Notify>,
    recovery_listener: Arc<Mutex<Option<BackendRecoveryListener>>>,
    ui_process_observer: Arc<Mutex<Option<Arc<dyn DesktopUiProcessObserver>>>>,
    remote_update_delegate:
        Arc<Mutex<Option<Arc<dyn bibcode_server::remote_update::RemoteUpdateDelegate>>>>,
    remote_update_support: Arc<Mutex<Option<bibcode_server::remote_update::RemoteUpdateSupport>>>,
    backend_port_resolver: Arc<dyn BackendPortResolver>,
    wsl_command_resolver: Arc<dyn WslCommandResolver>,
    #[cfg(test)]
    start_publish_gate: Arc<Mutex<Option<BackendStartPublishGate>>>,
    #[cfg(test)]
    shutdown_cleanup_reached: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    #[cfg(test)]
    late_start_cleanup_failure: Arc<Mutex<Option<String>>>,
    #[cfg(test)]
    concurrent_stop_waiting: Arc<FixtureEvent>,
    #[cfg(test)]
    runtime_published: Arc<FixtureEvent>,
    #[cfg(test)]
    listener_bind_retries: Arc<Mutex<ListenerBindRetryRecord>>,
}

impl fmt::Debug for BackendSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendSupervisor")
            .field("state", &self.state)
            .field("start_completed", &self.start_completed)
            .field("ui_process_observer", &self.ui_process_observer)
            .field("remote_update_delegate", &"<delegate slot>")
            .field("remote_update_support", &self.remote_update_support)
            .finish_non_exhaustive()
    }
}

impl Default for BackendSupervisor {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            start_completed: Arc::default(),
            recovery_listener: Arc::default(),
            ui_process_observer: Arc::default(),
            remote_update_delegate: Arc::default(),
            remote_update_support: Arc::default(),
            backend_port_resolver: Arc::new(SystemBackendPortResolver),
            wsl_command_resolver: Arc::new(SystemWslCommandResolver),
            #[cfg(test)]
            start_publish_gate: Arc::default(),
            #[cfg(test)]
            shutdown_cleanup_reached: Arc::default(),
            #[cfg(test)]
            late_start_cleanup_failure: Arc::default(),
            #[cfg(test)]
            concurrent_stop_waiting: Arc::default(),
            #[cfg(test)]
            runtime_published: Arc::default(),
            #[cfg(test)]
            listener_bind_retries: Arc::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackendUpdateEnvironment {
    pub environment_id: String,
    pub label: String,
    pub primary: bool,
    pub running: bool,
    pub config: Option<BackendRunConfig>,
    pub unprotected_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct BackendUpdateSnapshot {
    pub environments: Vec<BackendUpdateEnvironment>,
    running_plans: Vec<BackendLaunchPlan>,
    unavailable_environments: Vec<BackendUnavailableEnvironment>,
    in_process_maintenance: BTreeMap<String, InProcessUpdateMaintenance>,
}

impl BackendUpdateSnapshot {
    pub(crate) fn in_process_maintenance(
        &self,
        environment_id: &str,
    ) -> Option<Arc<bibcode_server::UpdateMaintenance>> {
        self.in_process_maintenance
            .get(environment_id)
            .map(|owner| owner.0.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum BackendStartFailure {
    #[error("{detail}")]
    PortInUse { port: u16, detail: String },
    #[error("{detail}")]
    Other { detail: String },
}

impl BackendStartFailure {
    fn other(detail: impl Into<String>) -> Self {
        Self::Other {
            detail: detail.into(),
        }
    }
}

impl From<BackendStartFailure> for String {
    fn from(failure: BackendStartFailure) -> Self {
        failure.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackendRecoveryEntry {
    pub environment_id: String,
    pub label: String,
    pub port: u16,
    pub failure: BackendStartFailure,
}

impl BackendRecoveryEntry {
    fn new(plan: &BackendLaunchPlan, failure: BackendStartFailure) -> Self {
        let port = match &failure {
            BackendStartFailure::PortInUse { port, .. } => *port,
            BackendStartFailure::Other { .. } => plan.config.port,
        };
        Self {
            environment_id: plan.config.environment_id.clone(),
            label: plan.config.label.clone(),
            port,
            failure,
        }
    }
}

#[derive(Debug)]
pub(crate) struct BackendRecoveryFailure {
    pub entries: Vec<BackendRecoveryEntry>,
}

impl fmt::Display for BackendRecoveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Could not restart every desktop backend: ")?;
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                write!(formatter, "; ")?;
            }
            write!(formatter, "{}: {}", entry.label, entry.failure)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BackendProjectDataTarget {
    pub environment_id: String,
    pub label: String,
    pub running_distro: Option<String>,
    pub running: bool,
    pub launch_plan: BackendLaunchPlan,
}

#[derive(Debug)]
pub(crate) struct BackendProjectDataOperation {
    supervisor: BackendSupervisor,
    target: BackendProjectDataTarget,
    stopped: bool,
    _reservation: BackendProjectDataReservation,
}

#[derive(Debug)]
struct BackendProjectDataReservation {
    supervisor: BackendSupervisor,
}

impl Drop for BackendProjectDataReservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.supervisor.state.lock() {
            state.update_coordination = false;
        }
    }
}

impl BackendProjectDataOperation {
    pub(crate) fn target(&self) -> &BackendProjectDataTarget {
        &self.target
    }

    pub(crate) async fn stop_selected(&mut self) -> Result<(), String> {
        if self.stopped || !self.target.running {
            self.stopped = true;
            return Ok(());
        }
        let managed = {
            let mut state = self
                .supervisor
                .state
                .lock()
                .map_err(|error| format!("backend supervisor mutex poisoned: {error}"))?;
            let slot = state
                .slots
                .get_mut(&self.target.environment_id)
                .ok_or_else(|| {
                    "The selected desktop backend is no longer registered.".to_owned()
                })?;
            slot.restart_scheduled = false;
            slot.backend.take().ok_or_else(|| {
                "The selected desktop backend stopped before recovery began.".to_owned()
            })?
        };
        stop_managed_backend(managed, BackendShutdownConfig::default()).await?;
        self.stopped = true;
        Ok(())
    }

    pub(crate) async fn restart_after_commit(&self) -> Result<(), BackendStartFailure> {
        let result = self
            .supervisor
            .start_with_options_inner(
                self.target.launch_plan.clone(),
                BackendReadinessConfig::default(),
                BackendRestartConfig::default(),
                true,
                true,
                // Only a target this operation stopped was holding its port.
                self.target.running.then_some(SAME_PORT_RESTART_BIND_RETRY),
            )
            .await
            .map(|_| ());
        if let Err(failure) = &result {
            self.supervisor
                .record_recovery_restart_failure(&self.target.launch_plan, failure);
        }
        self.supervisor.publish_recovery_change();
        result
    }
}

impl BackendSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_wsl_resolver(wsl_command_resolver: Arc<dyn WslCommandResolver>) -> Self {
        Self {
            wsl_command_resolver,
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_backend_port_resolver(backend_port_resolver: Arc<dyn BackendPortResolver>) -> Self {
        Self {
            backend_port_resolver,
            ..Self::default()
        }
    }

    #[cfg(test)]
    async fn test_wsl_plan(&self, distro: &str) -> Result<BackendLaunchPlan, String> {
        resolve_wsl_launch_plan_for_distro(
            self.wsl_command_resolver.as_ref(),
            distro.to_owned(),
            DEFAULT_BACKEND_PORT,
            "test-token".to_owned(),
            PathBuf::from("test-backend.log"),
            format!("{WSL_INSTANCE_ID_PREFIX}{distro}"),
            format!("WSL {distro}"),
        )
    }

    pub(crate) fn snapshot_for_update(&self) -> BackendUpdateSnapshot {
        let state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        let mut environments = Vec::with_capacity(state.slots.len());
        let mut running_plans = Vec::new();
        let mut unavailable_environments = Vec::new();
        let mut in_process_maintenance = BTreeMap::new();
        for (slot_key, slot) in &state.slots {
            let primary = slot_key == PRIMARY_LOCAL_ENVIRONMENT_ID;
            let running = slot.backend.is_some();
            if running && let Some(plan) = &slot.launch_plan {
                running_plans.push(plan.clone());
            }
            if let Some(unavailable) = &slot.unavailable {
                unavailable_environments.push(unavailable.clone());
            }
            let (environment_id, label, config) = if let Some(plan) = &slot.launch_plan {
                (
                    plan.config.environment_id.clone(),
                    plan.config.label.clone(),
                    Some(plan.config.clone()),
                )
            } else if let Some(unavailable) = &slot.unavailable {
                (
                    unavailable.environment_id.clone(),
                    unavailable.label.clone(),
                    None,
                )
            } else {
                (
                    slot_key.clone(),
                    if primary {
                        "Local".to_string()
                    } else {
                        slot_key.clone()
                    },
                    None,
                )
            };
            if let Some(ManagedBackend::Runtime(runtime)) = &slot.backend
                && let Some(owner) = &runtime.update_maintenance
                && let Some(owner) = owner.upgrade()
            {
                in_process_maintenance
                    .insert(environment_id.clone(), InProcessUpdateMaintenance(owner));
            }
            environments.push(BackendUpdateEnvironment {
                environment_id,
                label,
                primary,
                running,
                config,
                unprotected_reason: (!running).then(|| {
                    slot.unavailable
                        .as_ref()
                        .map(|unavailable| unavailable.detail.clone())
                        .or_else(|| slot.last_error.clone())
                        .unwrap_or_else(|| "The configured backend is not running.".to_string())
                }),
            });
        }
        BackendUpdateSnapshot {
            environments,
            running_plans,
            unavailable_environments,
            in_process_maintenance,
        }
    }

    pub(crate) fn project_data_targets(&self) -> Vec<BackendProjectDataTarget> {
        self.state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .slots
            .values()
            .filter_map(|slot| {
                let launch_plan = slot.launch_plan.clone()?;
                Some(BackendProjectDataTarget {
                    environment_id: launch_plan.config.environment_id.clone(),
                    label: launch_plan.config.label.clone(),
                    running_distro: launch_plan.config.running_distro.clone(),
                    running: slot.backend.is_some(),
                    launch_plan,
                })
            })
            .collect()
    }

    pub(crate) async fn begin_project_data_operation(
        &self,
        environment_id: &str,
    ) -> Result<BackendProjectDataOperation, String> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|error| format!("backend supervisor mutex poisoned: {error}"))?;
            if state.update_coordination {
                return Err(
                    "Another exclusive desktop data operation is already running.".to_owned(),
                );
            }
            state.update_coordination = true;
        }
        let reservation = BackendProjectDataReservation {
            supervisor: self.clone(),
        };
        self.wait_for_in_flight_starts().await;
        let target = self
            .project_data_targets()
            .into_iter()
            .find(|target| target.environment_id == environment_id);
        match target {
            Some(target) => Ok(BackendProjectDataOperation {
                supervisor: self.clone(),
                target,
                stopped: false,
                _reservation: reservation,
            }),
            None => Err("The selected project-data environment is not registered.".to_owned()),
        }
    }

    pub(crate) async fn begin_update_snapshot(&self) -> Result<BackendUpdateSnapshot, String> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|error| format!("backend supervisor mutex poisoned: {error}"))?;
            if state.update_coordination {
                return Err("Desktop backend update protection is already in progress.".to_string());
            }
            state.update_coordination = true;
        }
        self.wait_for_in_flight_starts().await;
        Ok(self.snapshot_for_update())
    }

    pub(crate) fn update_coordination_in_progress(&self) -> bool {
        self.state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .update_coordination
    }

    pub(crate) fn expect_update_snapshot_exit(&self, snapshot: &BackendUpdateSnapshot) {
        let state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        for environment in &snapshot.environments {
            if !environment.running {
                continue;
            }
            if let Some(backend) = state
                .slots
                .get(&environment.environment_id)
                .and_then(|slot| slot.backend.as_ref())
            {
                match backend {
                    ManagedBackend::Child(child) => child.request_stop(),
                    ManagedBackend::Runtime(runtime) => {
                        runtime.stop_requested.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
    }

    pub(crate) async fn stop_update_snapshot(
        &self,
        snapshot: &BackendUpdateSnapshot,
    ) -> Result<(), String> {
        self.expect_update_snapshot_exit(snapshot);
        self.stop(BackendShutdownConfig::default()).await
    }

    pub(crate) async fn restart_update_snapshot(
        &self,
        snapshot: &BackendUpdateSnapshot,
    ) -> Result<(), BackendRecoveryFailure> {
        let mut entries = Vec::new();
        for plan in &snapshot.running_plans {
            if let Err(error) = self
                .start_with_options_inner(
                    plan.clone(),
                    BackendReadinessConfig::default(),
                    BackendRestartConfig::default(),
                    true,
                    true,
                    Some(SAME_PORT_RESTART_BIND_RETRY),
                )
                .await
            {
                self.record_plan_error_with_classification(plan, error.to_string(), None);
                if let Some(slot) = self
                    .state
                    .lock()
                    .expect("backend supervisor mutex poisoned")
                    .slots
                    .get_mut(&backend_slot_key(plan))
                    && slot.launch_plan.as_ref() == Some(plan)
                    && slot.backend.is_none()
                {
                    // Shutdown or a topology restart may already have removed/replaced this slot.
                    slot.recovery_failure = Some(error.clone());
                }
                entries.push(BackendRecoveryEntry::new(plan, error));
            }
        }
        for unavailable in &snapshot.unavailable_environments {
            self.record_unavailable_environment(unavailable.clone());
        }
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .update_coordination = false;
        self.publish_recovery_change();
        if entries.is_empty() {
            Ok(())
        } else {
            Err(BackendRecoveryFailure { entries })
        }
    }

    fn install_ui_process_observer(&self, observer: Arc<dyn DesktopUiProcessObserver>) {
        *self
            .ui_process_observer
            .lock()
            .expect("desktop UI observer mutex poisoned") = Some(observer);
    }

    fn ui_process_observer_for_start(&self) -> Arc<dyn DesktopUiProcessObserver> {
        self.ui_process_observer
            .lock()
            .expect("desktop UI observer mutex poisoned")
            .clone()
            .unwrap_or_else(|| Arc::new(UnavailableDesktopUiProcessObserver))
    }

    pub fn install_remote_update_integration(
        &self,
        delegate: Arc<dyn bibcode_server::remote_update::RemoteUpdateDelegate>,
        support: bibcode_server::remote_update::RemoteUpdateSupport,
    ) {
        *self.remote_update_delegate.lock().expect("delegate slot") = Some(delegate);
        *self.remote_update_support.lock().expect("support slot") = Some(support);
    }

    pub(crate) fn install_recovery_listener(&self, listener: Arc<dyn Fn() + Send + Sync>) {
        *self
            .recovery_listener
            .lock()
            .expect("recovery listener mutex poisoned") = Some(listener);
    }

    pub(crate) fn backend_recovery(&self) -> Vec<BackendRecoveryEntry> {
        self.state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .backend_recovery()
    }

    fn publish_recovery_change(&self) {
        {
            let mut state = self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            let recovery = state.backend_recovery();
            if state.published_recovery == recovery {
                return;
            }
            state.published_recovery = recovery;
        }
        let listener = self
            .recovery_listener
            .lock()
            .expect("recovery listener mutex poisoned")
            .clone();
        // The listener reads update state, whose only nested lock order is updates -> supervisor.
        if let Some(listener) = listener {
            listener();
        }
    }

    fn record_recovery_restart_failure(
        &self,
        plan: &BackendLaunchPlan,
        failure: &BackendStartFailure,
    ) {
        {
            let mut state = self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            if let Some(slot) = state.slots.get_mut(&backend_slot_key(plan))
                && slot.recovery_failure.is_some()
            {
                slot.recovery_failure = Some(failure.clone());
            }
        }
        self.publish_recovery_change();
    }

    pub fn local_environment_bootstraps(&self) -> Vec<Value> {
        let state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        let mut bootstraps = Vec::new();
        if let Some(slot) = state.slots.get(PRIMARY_LOCAL_ENVIRONMENT_ID)
            && let Some(plan) = &slot.launch_plan
            && slot.last_error.is_none()
        {
            bootstraps.push(plan.config.to_environment_bootstrap());
        }
        for (slot_key, slot) in &state.slots {
            if slot_key == PRIMARY_LOCAL_ENVIRONMENT_ID {
                continue;
            }
            if let Some(unavailable) = &slot.unavailable {
                bootstraps.push(unavailable.to_environment_bootstrap());
            } else if let Some(plan) = &slot.launch_plan
                && slot.last_error.is_none()
            {
                bootstraps.push(plan.config.to_environment_bootstrap());
            }
        }
        bootstraps
    }

    pub fn current_run_config(&self) -> Option<BackendRunConfig> {
        let state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        state
            .slots
            .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
            .and_then(|slot| slot.launch_plan.as_ref())
            .or_else(|| {
                state
                    .slots
                    .values()
                    .find_map(|slot| slot.launch_plan.as_ref())
            })
            .map(|plan| plan.config.clone())
    }

    pub fn record_error(&self, error: impl Into<String>) {
        let mut state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        state
            .slots
            .entry(PRIMARY_LOCAL_ENVIRONMENT_ID.to_string())
            .or_default()
            .last_error = Some(error.into());
        drop(state);
        self.publish_recovery_change();
    }

    pub(crate) fn record_planning_error(&self, error: BackendPlanError) {
        let mut state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        let slot = state
            .slots
            .entry(PRIMARY_LOCAL_ENVIRONMENT_ID.to_string())
            .or_default();
        slot.launch_plan = None;
        slot.recovery_failure = None;
        slot.backend = None;
        slot.pid = None;
        slot.last_error = Some(error.to_string());
        slot.plan_error = Some(error);
        slot.restart_scheduled = false;
        drop(state);
        self.publish_recovery_change();
    }

    pub(crate) fn primary_plan_error(&self) -> Option<BackendPlanError> {
        self.state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .slots
            .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
            .and_then(|slot| slot.plan_error.clone())
    }

    pub(crate) fn secondary_unavailable_environment(
        &self,
    ) -> Option<BackendUnavailableEnvironment> {
        self.state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .slots
            .iter()
            .filter(|(slot_key, _)| slot_key.as_str() != PRIMARY_LOCAL_ENVIRONMENT_ID)
            .find_map(|(_, slot)| slot.unavailable.clone())
    }

    pub async fn start_default<R: Runtime>(
        &self,
        app: AppHandle<R>,
    ) -> Result<BackendRunConfig, String> {
        self.start_default_with_reason(app, "started", Some("local-only"), &BTreeSet::new())
            .await
    }

    /// Plans and starts the default topology. `held_ports` are the ports the
    /// backends stopped for this restart held; empty for a first start.
    async fn start_default_with_reason<R: Runtime>(
        &self,
        app: AppHandle<R>,
        reason: &'static str,
        exposure_override: Option<&str>,
        held_ports: &BTreeSet<u16>,
    ) -> Result<BackendRunConfig, String> {
        self.install_ui_process_observer(ui_process_observer::for_app(&app));
        let selection = match default_launch_plans(
            &app,
            self.backend_port_resolver.as_ref(),
            self.wsl_command_resolver.as_ref(),
            exposure_override,
        ) {
            Ok(selection) => selection,
            Err(error) => {
                self.record_planning_error(error.clone());
                return Err(error.to_string());
            }
        };
        let DefaultLaunchPlans {
            mut plans,
            unavailable_secondary,
        } = selection;
        let primary_index = plans
            .iter()
            .position(|plan| plan.config.environment_id == PRIMARY_LOCAL_ENVIRONMENT_ID)
            .unwrap_or(0);
        let primary_plan = plans.remove(primary_index);
        let primary_config = match self
            .start_default_plan(primary_plan.clone(), held_ports)
            .await
        {
            Ok(config) => config,
            Err(detail) => {
                let plan_error = classify_primary_start_error(&primary_plan, &detail);
                let error = plan_error
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or(detail);
                self.record_plan_error_with_classification(
                    &primary_plan,
                    error.clone(),
                    plan_error,
                );
                emit_project_data_status_changed(&app, &primary_plan.config.environment_id);
                return Err(error);
            }
        };

        if let Some(unavailable) = unavailable_secondary {
            self.record_unavailable_environment(unavailable);
        }

        for plan in plans {
            if let Err(error) = self.start_default_plan(plan.clone(), held_ports).await {
                self.record_plan_error(&plan, error.clone());
                tracing::warn!(
                    target: "bibcode_desktop_tauri::backend",
                    environment_id = plan.config.environment_id,
                    "secondary desktop backend launch failed: {error}"
                );
            }
        }

        emit_backend_ready(&app, reason, self.local_environment_bootstraps())?;
        Ok(primary_config)
    }

    pub async fn restart_default_if_active_preserving_exposure<R: Runtime>(
        &self,
        app: AppHandle<R>,
    ) -> Result<Option<BackendRunConfig>, String> {
        let exposure_override = self
            .current_run_config()
            .map(|config| config.server_exposure_mode);
        self.restart_default_if_active_with_override(app, exposure_override.as_deref())
            .await
    }

    pub async fn restart_default_if_active_with_exposure<R: Runtime>(
        &self,
        app: AppHandle<R>,
        desired: &str,
    ) -> Result<Option<BackendRunConfig>, String> {
        if !matches!(desired, "local-only" | "network-accessible") {
            return Err(format!("Unsupported server exposure mode: {desired}"));
        }
        self.restart_default_if_active_with_override(app, Some(desired))
            .await
    }

    async fn restart_default_if_active_with_override<R: Runtime>(
        &self,
        app: AppHandle<R>,
        exposure_override: Option<&str>,
    ) -> Result<Option<BackendRunConfig>, String> {
        let (is_active, held_ports) = {
            let state = self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            let is_active = state.slots.values().any(|slot| {
                slot.backend.is_some() || slot.launch_plan.is_some() || slot.last_error.is_some()
            });
            let held_ports = state
                .slots
                .values()
                .filter(|slot| slot.backend.is_some())
                .filter_map(|slot| slot.launch_plan.as_ref())
                .map(|plan| plan.config.port)
                .collect::<BTreeSet<_>>();
            (is_active, held_ports)
        };
        if !is_active {
            return Ok(None);
        }

        self.stop(BackendShutdownConfig::default()).await?;
        self.start_default_with_reason(app, "restarted", exposure_override, &held_ports)
            .await
            .map(Some)
    }

    #[cfg(test)]
    pub(crate) async fn start(&self, plan: BackendLaunchPlan) -> Result<BackendRunConfig, String> {
        self.start_with_options(
            plan,
            BackendReadinessConfig::default(),
            BackendRestartConfig::default(),
        )
        .await
    }

    #[cfg(test)]
    async fn start_with_options(
        &self,
        plan: BackendLaunchPlan,
        readiness: BackendReadinessConfig,
        restart: BackendRestartConfig,
    ) -> Result<BackendRunConfig, String> {
        self.start_with_options_inner(plan, readiness, restart, true, false, None)
            .await
            .map_err(String::from)
    }

    /// Starts one plan of the default topology. A plan whose port a backend
    /// stopped for this restart held gets the same-port bind retry window.
    async fn start_default_plan(
        &self,
        plan: BackendLaunchPlan,
        held_ports: &BTreeSet<u16>,
    ) -> Result<BackendRunConfig, String> {
        let listener_bind_retry = held_ports
            .contains(&plan.config.port)
            .then_some(SAME_PORT_RESTART_BIND_RETRY);
        self.start_with_options_inner(
            plan,
            BackendReadinessConfig::default(),
            BackendRestartConfig::default(),
            true,
            false,
            listener_bind_retry,
        )
        .await
        .map_err(String::from)
    }

    /// Starts `plan`. `listener_bind_retry` is set only by a restart onto the
    /// port its stopped predecessor held; in-process servers then retry a
    /// bind that finds the port still in use for that long.
    async fn start_with_options_inner(
        &self,
        plan: BackendLaunchPlan,
        readiness: BackendReadinessConfig,
        restart: BackendRestartConfig,
        reset_restart_attempt: bool,
        update_recovery: bool,
        listener_bind_retry: Option<Duration>,
    ) -> Result<BackendRunConfig, BackendStartFailure> {
        let permit = self
            .begin_start_with_update_recovery(reset_restart_attempt, update_recovery)
            .map_err(BackendStartFailure::other)?;
        let ui_process_observer = self.ui_process_observer_for_start();
        let remote_update_delegate = self
            .remote_update_delegate
            .lock()
            .expect("delegate slot")
            .clone();
        let remote_update_support = *self.remote_update_support.lock().expect("support slot");
        #[cfg(test)]
        self.listener_bind_retries
            .lock()
            .expect("listener bind retry test record mutex poisoned")
            .push((backend_slot_key(&plan), listener_bind_retry));
        let (config, managed, pid) = start_managed_backend(
            plan.clone(),
            readiness,
            ui_process_observer,
            remote_update_delegate,
            remote_update_support,
            permit.run_id,
            listener_bind_retry,
        )
        .await?;
        #[cfg(test)]
        self.wait_for_start_publish_gate(&managed).await;
        let mut active_plan = plan;
        active_plan.config = config.clone();
        let monitor_plan = active_plan.clone();
        let slot_key = backend_slot_key(&active_plan);
        let installed = {
            let mut state = self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            if matches!(
                &state.lifecycle,
                BackendLifecycle::Active { epoch } if *epoch == permit.epoch
            ) {
                let slot = state.slots.entry(slot_key.clone()).or_default();
                let previous = slot.backend.take();
                slot.launch_plan = Some(active_plan);
                slot.pid = pid;
                slot.last_error = None;
                slot.recovery_failure = None;
                slot.plan_error = None;
                slot.unavailable = None;
                slot.restart_scheduled = false;
                if reset_restart_attempt {
                    slot.restart_attempt = 0;
                }
                slot.backend = Some(managed.clone());
                Some((managed.clone(), previous))
            } else {
                None
            }
        };
        let Some((managed_for_monitor, previous)) = installed else {
            let cleanup = stop_managed_backend(managed, BackendShutdownConfig::default()).await;
            #[cfg(test)]
            let cleanup = self.inject_late_start_cleanup_failure(cleanup);
            if let Err(error) = &cleanup {
                permit.record_cleanup_error(error.clone());
            }
            return Err(BackendStartFailure::other(match cleanup {
                Ok(()) => {
                    "Desktop backend shutdown began before startup could be published.".to_string()
                }
                Err(error) => format!(
                    "Desktop backend shutdown began before startup could be published; \
                     late backend cleanup failed: {error}"
                ),
            }));
        };
        self.publish_recovery_change();
        #[cfg(test)]
        self.runtime_published.publish();

        if let Some(previous) = previous {
            let _ = stop_managed_backend(previous, BackendShutdownConfig::default()).await;
        }

        spawn_backend_monitor(
            self.clone(),
            managed_for_monitor,
            monitor_plan,
            readiness,
            restart,
        );

        Ok(config)
    }

    #[cfg(test)]
    fn begin_start(&self, allow_restart_after_stop: bool) -> Result<BackendStartPermit, String> {
        self.begin_start_with_update_recovery(allow_restart_after_stop, false)
    }

    fn begin_start_with_update_recovery(
        &self,
        allow_restart_after_stop: bool,
        update_recovery: bool,
    ) -> Result<BackendStartPermit, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| format!("backend supervisor mutex poisoned: {error}"))?;
        if state.update_coordination && !update_recovery {
            return Err("Desktop backend startup is paused during update protection.".to_string());
        }
        let in_flight_starts = state
            .in_flight_starts
            .checked_add(1)
            .ok_or_else(|| "Too many desktop backend starts are in flight.".to_string())?;
        let epoch = match &state.lifecycle {
            BackendLifecycle::Active { epoch } => *epoch,
            BackendLifecycle::Stopped {
                epoch,
                result: Ok(()),
            } if allow_restart_after_stop => epoch.saturating_add(1),
            BackendLifecycle::Stopped {
                result: Err(error), ..
            } if allow_restart_after_stop => {
                return Err(format!(
                    "Desktop backend cleanup failed; restart is unsafe: {error}"
                ));
            }
            BackendLifecycle::Stopping { .. } => {
                return Err("Desktop backend shutdown is still in progress.".to_string());
            }
            BackendLifecycle::Stopped { .. } => {
                return Err(
                    "Desktop backend has stopped; automatic restart was cancelled.".to_string(),
                );
            }
            BackendLifecycle::Terminated { .. } => {
                return Err("Desktop backend is terminating and cannot be restarted.".to_string());
            }
        };
        if matches!(state.lifecycle, BackendLifecycle::Stopped { .. }) {
            state.lifecycle = BackendLifecycle::Active { epoch };
        }
        let run_id = state.next_run_id;
        state.next_run_id = state.next_run_id.saturating_add(1);
        state.in_flight_starts = in_flight_starts;
        Ok(BackendStartPermit {
            epoch,
            run_id,
            state: self.state.clone(),
            start_completed: self.start_completed.clone(),
        })
    }

    async fn wait_for_in_flight_starts(&self) {
        loop {
            let notified = self.start_completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .in_flight_starts
                == 0
            {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    fn set_start_publish_gate(
        &self,
        reached: oneshot::Sender<ManagedBackend>,
        release: oneshot::Receiver<()>,
    ) {
        *self
            .start_publish_gate
            .lock()
            .expect("backend start test gate mutex poisoned") = Some((reached, release));
    }

    #[cfg(test)]
    async fn wait_for_start_publish_gate(&self, managed: &ManagedBackend) {
        let gate = self
            .start_publish_gate
            .lock()
            .expect("backend start test gate mutex poisoned")
            .take();
        if let Some((reached, release)) = gate {
            let _ = reached.send(managed.clone());
            let _ = release.await;
        }
    }

    #[cfg(test)]
    fn set_shutdown_cleanup_reached(&self, reached: oneshot::Sender<()>) {
        *self
            .shutdown_cleanup_reached
            .lock()
            .expect("backend shutdown test notification mutex poisoned") = Some(reached);
    }

    #[cfg(test)]
    fn notify_shutdown_cleanup_reached(&self) {
        if let Some(reached) = self
            .shutdown_cleanup_reached
            .lock()
            .expect("backend shutdown test notification mutex poisoned")
            .take()
        {
            let _ = reached.send(());
        }
    }

    #[cfg(test)]
    fn set_late_start_cleanup_failure(&self, error: impl Into<String>) {
        *self
            .late_start_cleanup_failure
            .lock()
            .expect("late backend cleanup test failure mutex poisoned") = Some(error.into());
    }

    #[cfg(test)]
    fn inject_late_start_cleanup_failure(&self, cleanup: Result<(), String>) -> Result<(), String> {
        match self
            .late_start_cleanup_failure
            .lock()
            .expect("late backend cleanup test failure mutex poisoned")
            .take()
        {
            Some(error) => Err(error),
            None => cleanup,
        }
    }

    pub async fn stop(&self, shutdown: BackendShutdownConfig) -> Result<(), String> {
        self.stop_inner(shutdown, false).await
    }

    pub(crate) async fn stop_for_exit(
        &self,
        shutdown: BackendShutdownConfig,
    ) -> Result<(), String> {
        self.stop_inner(shutdown, true).await
    }

    async fn stop_inner(
        &self,
        shutdown: BackendShutdownConfig,
        terminate: bool,
    ) -> Result<(), String> {
        let (mut completion, pending_shutdown) = {
            let mut state = self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            if let BackendLifecycle::Stopping {
                terminate: active_terminate,
                completion,
                ..
            } = &mut state.lifecycle
            {
                *active_terminate |= terminate;
                #[cfg(test)]
                self.concurrent_stop_waiting.publish();
                (completion.clone(), None)
            } else if let BackendLifecycle::Active { epoch } = state.lifecycle {
                let shutdown_epoch = epoch.saturating_add(1);
                let (completion_tx, completion_rx) = watch::channel(None);
                state.lifecycle = BackendLifecycle::Stopping {
                    epoch: shutdown_epoch,
                    terminate,
                    completion: completion_rx.clone(),
                    late_start_cleanup_error: None,
                };

                let backends = state
                    .slots
                    .values_mut()
                    .filter_map(|slot| slot.backend.take())
                    .collect::<Vec<_>>();
                state.slots.clear();
                (
                    completion_rx,
                    Some((shutdown_epoch, backends, completion_tx)),
                )
            } else {
                let completed_result = match &state.lifecycle {
                    BackendLifecycle::Stopped { result, .. }
                    | BackendLifecycle::Terminated { result } => result.clone(),
                    BackendLifecycle::Active { .. } | BackendLifecycle::Stopping { .. } => {
                        unreachable!("active and stopping lifecycle handled above")
                    }
                };
                if terminate && matches!(state.lifecycle, BackendLifecycle::Stopped { .. }) {
                    state.lifecycle = BackendLifecycle::Terminated {
                        result: completed_result.clone(),
                    };
                }
                let (_completion_tx, completion_rx) = watch::channel(Some(completed_result));
                (completion_rx, None)
            }
        };

        self.publish_recovery_change();
        if let Some((shutdown_epoch, backends, completion_tx)) = pending_shutdown {
            let supervisor = self.clone();
            tauri::async_runtime::spawn(async move {
                let mut first_error = None;
                for backend in backends {
                    if let Err(error) = stop_managed_backend(backend, shutdown).await
                        && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                }
                #[cfg(test)]
                supervisor.notify_shutdown_cleanup_reached();
                supervisor.wait_for_in_flight_starts().await;

                {
                    let mut state = supervisor
                        .state
                        .lock()
                        .expect("backend supervisor mutex poisoned");
                    if let BackendLifecycle::Stopping {
                        epoch,
                        terminate,
                        late_start_cleanup_error,
                        ..
                    } = &mut state.lifecycle
                        && *epoch == shutdown_epoch
                    {
                        let result = match (first_error, late_start_cleanup_error.take()) {
                            (None, None) => Ok(()),
                            (Some(error), None) | (None, Some(error)) => Err(error),
                            (Some(error), Some(late_error)) => {
                                Err(format!("{error}; {late_error}"))
                            }
                        };
                        let terminate = *terminate;
                        let _ = completion_tx.send(Some(result.clone()));
                        state.lifecycle = if terminate {
                            BackendLifecycle::Terminated { result }
                        } else {
                            BackendLifecycle::Stopped {
                                epoch: *epoch,
                                result,
                            }
                        };
                    }
                }
            });
        }

        completion
            .wait_for(|result| result.is_some())
            .await
            .map_err(|_| "Desktop backend shutdown task ended without a result.".to_string())?
            .clone()
            .expect("shutdown completion was checked above")
    }

    fn restart_still_desired(&self, slot_key: &str) -> bool {
        let state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        state
            .slots
            .get(slot_key)
            .map(|slot| {
                slot.restart_scheduled && slot.launch_plan.is_some() && slot.backend.is_none()
            })
            .unwrap_or(false)
    }

    fn schedule_restart(
        &self,
        plan: BackendLaunchPlan,
        readiness: BackendReadinessConfig,
        restart: BackendRestartConfig,
        reason: String,
    ) {
        let slot_key = backend_slot_key(&plan);
        let (attempt, delay) = {
            let mut state = self
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            let Some(slot) = state.slots.get_mut(&slot_key) else {
                return;
            };
            if slot.launch_plan.is_none() {
                return;
            }
            slot.backend = None;
            slot.pid = None;
            slot.last_error = Some(reason.clone());
            slot.unavailable = unavailable_wsl_secondary_from_plan(&plan, &reason);
            slot.restart_attempt = slot.restart_attempt.saturating_add(1);
            slot.restart_scheduled = true;
            let attempt = slot.restart_attempt;
            (attempt, restart_delay_for_attempt(attempt, &restart))
        };

        self.publish_recovery_change();
        tracing::warn!(
            target: "bibcode_desktop_tauri::backend",
            "desktop backend restart attempt {attempt} scheduled after {delay:?}"
        );

        let supervisor = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            if !supervisor.restart_still_desired(&slot_key) {
                return;
            }
            // The plan carries the port the crashed backend was bound to.
            if let Err(error) = supervisor
                .start_with_options_inner(
                    plan.clone(),
                    readiness,
                    restart,
                    false,
                    false,
                    Some(SAME_PORT_RESTART_BIND_RETRY),
                )
                .await
            {
                supervisor.schedule_restart(plan, readiness, restart, error.into());
            }
        });
    }

    fn record_plan_error(&self, plan: &BackendLaunchPlan, error: String) {
        self.record_plan_error_with_classification(plan, error, None);
    }

    fn record_plan_error_with_classification(
        &self,
        plan: &BackendLaunchPlan,
        error: String,
        plan_error: Option<BackendPlanError>,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        let slot = state.slots.entry(backend_slot_key(plan)).or_default();
        slot.launch_plan = Some(plan.clone());
        slot.backend = None;
        slot.pid = None;
        slot.unavailable = unavailable_wsl_secondary_from_plan(plan, &error);
        slot.last_error = Some(error);
        slot.plan_error = plan_error;
        slot.restart_scheduled = false;
        drop(state);
        self.publish_recovery_change();
    }

    pub(crate) fn record_unavailable_environment(
        &self,
        unavailable: BackendUnavailableEnvironment,
    ) {
        let mut state = self
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        let slot = state
            .slots
            .entry(unavailable.environment_id.clone())
            .or_default();
        slot.launch_plan = None;
        slot.recovery_failure = None;
        slot.backend = None;
        slot.pid = None;
        slot.last_error = Some(unavailable.detail.clone());
        slot.plan_error = None;
        slot.unavailable = Some(unavailable);
        slot.restart_scheduled = false;
        drop(state);
        self.publish_recovery_change();
    }

    #[cfg(test)]
    fn next_run_id(&self) -> Result<u64, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| format!("backend supervisor mutex poisoned: {error}"))?;
        let run_id = state.next_run_id;
        state.next_run_id = state.next_run_id.saturating_add(1);
        Ok(run_id)
    }
}

#[cfg(any(unix, test))]
pub(crate) async fn shutdown_backend_after_termination(
    backend: BackendSupervisor,
    termination: impl std::future::Future<Output = ()> + Send,
    request_exit: impl FnOnce() + Send,
) {
    termination.await;
    if let Err(error) = backend
        .stop_for_exit(BackendShutdownConfig::default())
        .await
    {
        tracing::warn!("failed to stop Tauri desktop backend after termination signal: {error}");
    }
    request_exit();
}

#[cfg(unix)]
pub(crate) fn install_termination_signal_handler<R: Runtime>(
    app: AppHandle<R>,
    backend: BackendSupervisor,
) {
    tauri::async_runtime::spawn(shutdown_backend_after_termination(
        backend,
        wait_for_termination_signal(),
        move || app.exit(0),
    ));
}

#[cfg(unix)]
async fn wait_for_termination_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(terminate) => terminate,
        Err(error) => {
            tracing::error!("failed to install desktop termination handler: {error}");
            std::future::pending().await
        }
    };
    if terminate.recv().await.is_none() {
        std::future::pending().await
    }
    // A logout signals the whole session; the processes that die with BiBCode are not
    // crashes.
    bibcode_server::diagnostics::profiler::exits::note_terminating();
}

fn emit_backend_ready<R: Runtime>(
    app: &AppHandle<R>,
    reason: &'static str,
    bootstraps: Vec<Value>,
) -> Result<(), String> {
    app.emit(
        BACKEND_READY_EVENT,
        json!({
            "reason": reason,
            "bootstraps": bootstraps,
        }),
    )
    .map_err(|error| format!("Could not emit desktop backend readiness: {error}"))
}

pub(crate) fn emit_project_data_status_changed<R: Runtime>(
    app: &AppHandle<R>,
    environment_id: &str,
) {
    if let Err(error) = app.emit(
        PROJECT_DATA_STATUS_CHANGED_EVENT,
        json!({ "environmentId": environment_id }),
    ) {
        tracing::warn!(
            target: "bibcode_desktop_tauri::backend",
            environment_id,
            "desktop project-data status invalidation failed: Could not emit project-data status change: {error}"
        );
    }
}

async fn start_managed_backend(
    plan: BackendLaunchPlan,
    readiness: BackendReadinessConfig,
    ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
    remote_update_delegate: Option<Arc<dyn bibcode_server::remote_update::RemoteUpdateDelegate>>,
    remote_update_support: Option<bibcode_server::remote_update::RemoteUpdateSupport>,
    run_id: u64,
    listener_bind_retry: Option<Duration>,
) -> Result<(BackendRunConfig, ManagedBackend, Option<u32>), BackendStartFailure> {
    match &plan.target {
        BackendLaunchTarget::InProcess { data_root, .. } => {
            let mut server_config = server_config_for_launch(data_root.clone(), &plan.config);
            #[cfg(test)]
            {
                prepare_isolated_test_server_settings(&data_root.effective)
                    .map_err(BackendStartFailure::other)?;
                server_config = server_config.with_hosting_executable_dir_for_integration_test(
                    data_root.effective.join("missing-hosting-bin"),
                );
            }
            if let Some(support) = remote_update_support {
                server_config = server_config.with_remote_update_support(support);
            }
            if let Some(window) = listener_bind_retry {
                server_config = server_config.with_listener_bind_retry(window);
            }
            let handle = match remote_update_delegate {
                Some(delegate) => {
                    ServerRuntime::start_with_desktop_integration(
                        server_config,
                        ui_process_observer,
                        delegate,
                    )
                    .await
                }
                None => {
                    ServerRuntime::start_with_ui_process_observer(
                        server_config,
                        ui_process_observer,
                    )
                    .await
                }
            }
            .map_err(|error| {
                let detail = format!("Could not start in-process desktop backend: {error}");
                match error {
                    ServerError::Bind { source, .. }
                        if source.kind() == io::ErrorKind::AddrInUse =>
                    {
                        BackendStartFailure::PortInUse {
                            port: plan.config.port,
                            detail,
                        }
                    }
                    _ => BackendStartFailure::other(detail),
                }
            })?;

            let mut config = plan.config.clone();
            config.port = handle.local_addr().port();
            if let Err(error) = wait_for_http_ready(&config.http_base_url(), &readiness).await {
                handle.shutdown();
                let _ = handle.join().await;
                return Err(BackendStartFailure::other(error));
            }
            crate::crash_monitor::follow_server(&handle);
            crate::webview_exit::follow_server(&handle);

            Ok((
                config.clone(),
                ManagedBackend::Runtime(Box::new(ManagedBackendRuntime::new(run_id, handle))),
                None,
            ))
        }
        BackendLaunchTarget::ExternalProcess {
            program,
            args,
            bootstrap_line,
            ..
        } => {
            // BiBCode backend binaries need the AppImage's bundled libraries.
            // Keep this launch and the desktop/WebKitGTK environment intact;
            // the server isolates only its user-facing child commands.
            let mut command = Command::new(program);
            configure_background_command(&mut command);
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let mut child = command.spawn().map_err(|error| {
                BackendStartFailure::other(format!(
                    "Could not start desktop backend using {program}: {error}"
                ))
            })?;

            let mut stdin = child.stdin.take().ok_or_else(|| {
                BackendStartFailure::other(
                    "Desktop backend child process did not expose stdin for bootstrap delivery.",
                )
            })?;
            stdin
                .write_all(bootstrap_line.as_bytes())
                .await
                .map_err(|error| {
                    BackendStartFailure::other(format!(
                        "Could not write desktop backend bootstrap: {error}"
                    ))
                })?;
            drop(stdin);

            drain_output("stdout", child.stdout.take(), plan.log_path.clone());
            drain_output("stderr", child.stderr.take(), plan.log_path.clone());

            let config = plan.config.clone();
            if let Err(error) = wait_for_http_ready(&config.http_base_url(), &readiness).await {
                let _ = child.start_kill();
                return Err(BackendStartFailure::other(error));
            }

            let pid = child.id();
            Ok((
                config.clone(),
                ManagedBackend::Child(Box::new(ManagedBackendChild::new(run_id, config, child))),
                pid,
            ))
        }
    }
}

#[cfg(test)]
fn prepare_isolated_test_server_settings(base_dir: &Path) -> Result<(), String> {
    let state_dir = base_dir.join("userdata");
    fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create isolated desktop test state at {}: {error}",
            state_dir.display()
        )
    })?;
    let settings_path = state_dir.join("settings.json");
    let mut settings = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&settings_path)
    {
        Ok(settings) => settings,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to create isolated desktop test settings at {}: {error}",
                settings_path.display()
            ));
        }
    };
    let mut fixture =
        crate::test_support::hermetic_providers::hermetic_provider_settings(&state_dir);
    for provider in crate::test_support::hermetic_providers::BUILTIN_PROVIDER_DRIVERS {
        fixture["providers"][provider]["enabled"] = Value::Bool(false);
    }
    serde_json::to_writer(&mut settings, &fixture).map_err(|error| {
        format!(
            "failed to write isolated desktop test settings at {}: {error}",
            settings_path.display()
        )
    })
}

fn server_config_for_launch(
    data_root: ResolvedDataRoot,
    config: &BackendRunConfig,
) -> ServerConfig {
    let mut server_config =
        ServerConfig::new(data_root.effective.clone()).with_bind(&config.bind_host, config.port);
    server_config.data_root_request = DataRootRequest::explicit(
        data_root.source,
        data_root.requested.clone(),
        PathBuf::new(),
    );
    server_config.resolved_data_root = Some(data_root);
    server_config.mode = bibcode_server::ServerMode::Desktop;
    server_config.no_browser = true;
    server_config.desktop_bootstrap_token = Some(config.desktop_bootstrap_token.clone());
    server_config.environment_id = config.environment_id.clone();
    server_config.environment_label = config.label.clone();
    server_config
}

fn spawn_backend_monitor(
    supervisor: BackendSupervisor,
    backend: ManagedBackend,
    plan: BackendLaunchPlan,
    readiness: BackendReadinessConfig,
    restart: BackendRestartConfig,
) {
    tauri::async_runtime::spawn(async move {
        match backend {
            ManagedBackend::Child(child) => {
                loop {
                    tokio::time::sleep(restart.monitor_interval).await;
                    if child.stop_requested.load(Ordering::SeqCst) {
                        return;
                    }

                    let exit = {
                        let mut process = child.child.lock().await;
                        process.try_wait()
                    };

                    match exit {
                        Ok(None) => {}
                        Ok(Some(status)) => {
                            if child.stop_requested.load(Ordering::SeqCst) {
                                return;
                            }
                            supervisor.schedule_restart(
                            plan.clone(),
                            readiness,
                            restart,
                            format!("Desktop backend child exited unexpectedly with status {status}."),
                        );
                            return;
                        }
                        Err(error) => {
                            supervisor.schedule_restart(
                                plan.clone(),
                                readiness,
                                restart,
                                format!("Could not inspect desktop backend child status: {error}"),
                            );
                            return;
                        }
                    }
                }
            }
            ManagedBackend::Runtime(runtime) => {
                if let Err(error) = runtime.wait_for_completion().await
                    && !runtime.stop_requested.load(Ordering::SeqCst)
                {
                    supervisor.schedule_restart(plan, readiness, restart, error);
                }
            }
        }
    });
}

async fn stop_managed_backend(
    backend: ManagedBackend,
    shutdown: BackendShutdownConfig,
) -> Result<(), String> {
    match backend {
        ManagedBackend::Child(child) => stop_managed_child(*child, shutdown).await,
        ManagedBackend::Runtime(runtime) => {
            runtime.request_stop();
            tokio::time::timeout(shutdown.timeout, runtime.wait_for_completion())
                .await
                .map_err(|_| {
                    format!(
                        "Timed out after {:?} while stopping in-process desktop backend.",
                        shutdown.timeout
                    )
                })?
        }
    }
}

async fn stop_managed_child(
    child: ManagedBackendChild,
    shutdown: BackendShutdownConfig,
) -> Result<(), String> {
    child.request_stop();

    let soft_shutdown_timeout = shutdown
        .timeout
        .min(DEFAULT_BACKEND_SOFT_SHUTDOWN_REQUEST_TIMEOUT);
    let soft_shutdown_requested =
        request_backend_soft_shutdown(&child.config, soft_shutdown_timeout)
            .await
            .inspect_err(|error| {
                tracing::debug!("desktop backend soft shutdown request failed: {error}");
            })
            .is_ok();

    let mut process = child.child.lock().await;

    if matches!(process.try_wait(), Ok(Some(_))) {
        return Ok(());
    }

    let graceful_requested =
        soft_shutdown_requested || request_child_soft_termination(&mut process);
    if !graceful_requested && let Err(error) = process.start_kill() {
        tracing::debug!(
            "desktop backend child was already stopped or could not be killed: {error}"
        );
    }

    match tokio::time::timeout(shutdown.timeout, process.wait()).await {
        Ok(Ok(_status)) => Ok(()),
        Ok(Err(error)) => Err(format!(
            "Could not wait for desktop backend shutdown: {error}"
        )),
        Err(_) if graceful_requested => {
            if let Err(error) = process.start_kill() {
                tracing::debug!(
                    "desktop backend child was already stopped or could not be force-killed: {error}"
                );
            }
            match tokio::time::timeout(shutdown.timeout, process.wait()).await {
                Ok(Ok(_status)) => Ok(()),
                Ok(Err(error)) => Err(format!(
                    "Could not wait for forced desktop backend shutdown: {error}"
                )),
                Err(_) => Err(format!(
                    "Timed out after {:?} while force-stopping desktop backend.",
                    shutdown.timeout
                )),
            }
        }
        Err(_) => Err(format!(
            "Timed out after {:?} while stopping desktop backend.",
            shutdown.timeout
        )),
    }
}

#[cfg(unix)]
fn request_child_soft_termination(child: &mut Child) -> bool {
    let Some(pid) = child.id() else {
        return false;
    };
    let mut command = std::process::Command::new("kill");
    isolate_appimage_environment(&mut command);
    command
        .args(["-TERM", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn request_child_soft_termination(_child: &mut Child) -> bool {
    false
}

fn restart_delay_for_attempt(attempt: u32, restart: &BackendRestartConfig) -> Duration {
    if attempt <= 1 {
        return restart.initial_delay.min(restart.max_delay);
    }

    let multiplier = 1_u32
        .checked_shl(attempt.saturating_sub(1))
        .unwrap_or(u32::MAX);
    restart
        .initial_delay
        .saturating_mul(multiplier)
        .min(restart.max_delay)
}

async fn request_backend_soft_shutdown(
    config: &BackendRunConfig,
    timeout: Duration,
) -> Result<(), String> {
    let mut url = url::Url::parse(&config.http_base_url())
        .map_err(|error| format!("Invalid backend shutdown URL: {error}"))?;
    url.set_path(SERVER_BACKEND_SHUTDOWN_PATH);
    url.set_query(None);
    url.set_fragment(None);

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| format!("Could not create backend shutdown HTTP client: {error}"))?;
    let response = client
        .post(url)
        .header(
            SERVER_BACKEND_SHUTDOWN_TOKEN_HEADER,
            &config.desktop_bootstrap_token,
        )
        .send()
        .await
        .map_err(|error| format!("Could not request desktop backend shutdown: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "Desktop backend shutdown endpoint returned {}.",
            response.status().as_u16()
        ))
    }
}

async fn wait_for_http_ready(
    base_url: &str,
    readiness: &BackendReadinessConfig,
) -> Result<(), String> {
    let started_at = Instant::now();

    loop {
        let attempt_error = match probe_http_ready(base_url, readiness.request_timeout).await {
            Ok(true) => return Ok(()),
            Ok(false) => "readiness endpoint returned a non-success status".to_string(),
            Err(error) => error,
        };

        if started_at.elapsed() >= readiness.timeout {
            return Err(format!(
                "Desktop backend did not become ready at {base_url}{BACKEND_READINESS_PATH} within {:?}: {}",
                readiness.timeout, attempt_error,
            ));
        }

        tokio::time::sleep(readiness.interval).await;
    }
}

async fn probe_http_ready(base_url: &str, request_timeout: Duration) -> Result<bool, String> {
    let base_url = base_url.to_string();
    tokio::task::spawn_blocking(move || probe_http_ready_blocking(&base_url, request_timeout))
        .await
        .map_err(|error| format!("Desktop backend readiness task failed: {error}"))?
}

fn probe_http_ready_blocking(base_url: &str, request_timeout: Duration) -> Result<bool, String> {
    let base = url::Url::parse(base_url)
        .map_err(|error| format!("Invalid backend URL {base_url}: {error}"))?;
    if base.scheme() != "http" {
        return Err(format!(
            "Unsupported backend readiness URL scheme: {}",
            base.scheme()
        ));
    }
    let ready_url = base
        .join(BACKEND_READINESS_PATH)
        .map_err(|error| format!("Invalid backend readiness path: {error}"))?;
    let host = ready_url
        .host_str()
        .ok_or_else(|| format!("Backend readiness URL has no host: {ready_url}"))?;
    let port = ready_url
        .port_or_known_default()
        .ok_or_else(|| format!("Backend readiness URL has no port: {ready_url}"))?;
    let address = format!("{host}:{port}");
    let mut addresses = address.to_socket_addrs().map_err(|error| {
        format!("Could not resolve backend readiness address {address}: {error}")
    })?;
    let Some(socket_address) = addresses.next() else {
        return Err(format!(
            "Could not resolve any backend readiness address for {address}"
        ));
    };

    let mut stream =
        TcpStream::connect_timeout(&socket_address, request_timeout).map_err(|error| {
            format!("Could not connect to backend readiness endpoint {ready_url}: {error}")
        })?;
    stream
        .set_read_timeout(Some(request_timeout))
        .map_err(|error| format!("Could not set backend readiness read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(request_timeout))
        .map_err(|error| format!("Could not set backend readiness write timeout: {error}"))?;

    let path = if let Some(query) = ready_url.query() {
        format!("{}?{query}", ready_url.path())
    } else {
        ready_url.path().to_string()
    };
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("Could not write backend readiness request: {error}"))?;

    let mut buffer = [0_u8; 128];
    let count = stream
        .read(&mut buffer)
        .map_err(|error| format!("Could not read backend readiness response: {error}"))?;
    let response = String::from_utf8_lossy(&buffer[..count]);
    let status_line = response.lines().next().unwrap_or_default();
    Ok(status_line.starts_with("HTTP/1.1 2") || status_line.starts_with("HTTP/1.0 2"))
}

fn drain_output(
    stream_name: &'static str,
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    log_path: Option<PathBuf>,
) {
    let Some(mut stream) = stream else {
        return;
    };

    tauri::async_runtime::spawn(async move {
        let mut log_file = log_path.as_deref().and_then(open_backend_log_file);
        let mut buffer = [0_u8; 4096];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) => break,
                Ok(count) => {
                    let text = String::from_utf8_lossy(&buffer[..count]);
                    tracing::debug!(target: "bibcode_desktop_tauri::backend", stream = stream_name, "{text}");
                    if let Some(file) = log_file.as_mut()
                        && let Err(error) =
                            write_backend_log_chunk(file, stream_name, &buffer[..count])
                    {
                        tracing::debug!(target: "bibcode_desktop_tauri::backend", stream = stream_name, "backend output log write failed: {error}");
                        log_file = None;
                    }
                }
                Err(error) => {
                    tracing::debug!(target: "bibcode_desktop_tauri::backend", stream = stream_name, "backend output drain failed: {error}");
                    break;
                }
            }
        }
    });
}

fn open_backend_log_file(path: &Path) -> Option<fs::File> {
    if let Some(directory) = path.parent()
        && let Err(error) = fs::create_dir_all(directory)
    {
        tracing::debug!(target: "bibcode_desktop_tauri::backend", "backend log directory creation failed: {error}");
        return None;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            tracing::debug!(target: "bibcode_desktop_tauri::backend", "backend log file open failed: {error}");
            error
        })
        .ok()
}

fn write_backend_log_chunk(file: &mut fs::File, stream_name: &str, chunk: &[u8]) -> io::Result<()> {
    file.write_all(format!("[{stream_name}] ").as_bytes())?;
    file.write_all(chunk)?;
    if !chunk.ends_with(b"\n") {
        file.write_all(b"\n")?;
    }
    file.flush()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WslDistroEntry {
    name: String,
    is_default: bool,
}

fn decode_wsl_command_output(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let values = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&values);
    }
    String::from_utf8_lossy(bytes).to_string()
}

fn parse_wsl_distro_entries(raw: &str) -> Vec<WslDistroEntry> {
    raw.lines()
        .filter_map(|line| {
            let line = line.replace('\0', "");
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("NAME") {
                return None;
            }
            let is_default = trimmed.starts_with('*');
            let without_marker = trimmed.trim_start_matches('*').trim();
            let name = without_marker.split_whitespace().next()?;
            if name.eq_ignore_ascii_case("name") {
                return None;
            }
            Some(WslDistroEntry {
                name: name.to_string(),
                is_default,
            })
        })
        .collect()
}

fn run_wsl_command(
    resolver: &dyn WslCommandResolver,
    distro: &str,
    args: &[&str],
) -> Result<String, String> {
    let mut command = resolver.command();
    configure_background_std_command(&mut command);
    let output = command
        .args(["-d", distro, "--"])
        .args(args)
        .output()
        .map_err(|error| format!("Could not run wsl.exe for distro {distro}: {error}"))?;
    if !output.status.success() {
        let stderr = decode_wsl_command_output(&output.stderr);
        return Err(format!(
            "wsl.exe for distro {distro} exited with status {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(decode_wsl_command_output(&output.stdout))
}

fn list_wsl_distros(resolver: &dyn WslCommandResolver) -> Result<Vec<WslDistroEntry>, String> {
    let mut command = resolver.command();
    configure_background_std_command(&mut command);
    let output = command
        .args(["-l", "-v"])
        .output()
        .map_err(|error| format!("Could not list WSL distributions: {error}"))?;
    if !output.status.success() {
        let stderr = decode_wsl_command_output(&output.stderr);
        return Err(format!(
            "wsl.exe -l -v exited with status {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(parse_wsl_distro_entries(&decode_wsl_command_output(
        &output.stdout,
    )))
}

fn resolve_wsl_distro(
    resolver: &dyn WslCommandResolver,
    settings: &BackendDesktopSettings,
) -> Result<String, String> {
    if let Some(distro) = &settings.wsl_distro {
        return Ok(distro.clone());
    }
    let distros = list_wsl_distros(resolver)?;
    distros
        .iter()
        .find(|distro| distro.is_default)
        .or_else(|| distros.first())
        .map(|distro| distro.name.clone())
        .ok_or_else(|| "WSL has no installed distributions.".to_string())
}

fn resolve_wsl_path(
    resolver: &dyn WslCommandResolver,
    distro: &str,
    windows_path: &Path,
) -> Result<String, String> {
    let windows_path = windows_path.to_string_lossy();
    let output = run_wsl_command(resolver, distro, &["wslpath", "-a", &windows_path])?;
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("wslpath returned no Linux path for {windows_path}"))
}

fn resolve_wsl_server_binary(
    resolver: &dyn WslCommandResolver,
    distro: &str,
) -> Result<String, String> {
    let candidates = resolver.server_binary_candidates()?;
    for candidate in candidates {
        if candidate.is_file() {
            return resolve_wsl_path(resolver, distro, &candidate);
        }
    }

    Err(format!(
        "Could not find a Linux bibcode server binary for WSL. Set {WSL_SERVER_BINARY_ENV} or build one under target/<triple>/(debug|release)/bibcode."
    ))
}

fn resolve_wsl_renderer_host(resolver: &dyn WslCommandResolver, distro: &str) -> Option<String> {
    let output = run_wsl_command(resolver, distro, &["hostname", "-I"]).ok()?;
    output
        .split_whitespace()
        .find(|value| value.parse::<Ipv4Addr>().is_ok())
        .map(ToOwned::to_owned)
}

fn resolve_wsl_data_root(
    resolver: &dyn WslCommandResolver,
    distro: &str,
) -> Result<String, String> {
    let environment = run_wsl_command(resolver, distro, &["env"])?;
    let value = environment
        .lines()
        .find_map(|line| line.strip_prefix("BIBCODE_HOME="))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            environment
                .lines()
                .find_map(|line| line.strip_prefix("HOME="))
                .filter(|value| !value.is_empty())
                .map(|home| format!("{}/.bibcode", home.trim_end_matches('/')))
        })
        .ok_or_else(|| format!("WSL distro {distro} did not report BIBCODE_HOME or HOME."))?;
    if !value.starts_with('/') || value.contains('\r') || value.contains('\0') {
        return Err(format!(
            "WSL distro {distro} reported a project data root that is not an absolute Linux path."
        ));
    }
    Ok(value)
}

fn resolve_wsl_launch_plan_for_distro(
    resolver: &dyn WslCommandResolver,
    running_distro: String,
    port: u16,
    desktop_bootstrap_token: String,
    log_path: PathBuf,
    environment_id: String,
    label: String,
) -> Result<BackendLaunchPlan, String> {
    let binary_path = resolve_wsl_server_binary(resolver, &running_distro)?;
    let data_root = resolve_wsl_data_root(resolver, &running_distro)?;
    let renderer_host = resolve_wsl_renderer_host(resolver, &running_distro)
        .unwrap_or_else(|| DESKTOP_LOOPBACK_HOST.to_string());

    Ok(BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
        environment_id,
        label,
        running_distro,
        port,
        renderer_host,
        desktop_bootstrap_token,
        binary_path,
        data_root,
    })
    .with_log_path(log_path))
}

fn resolve_wsl_primary_launch_plan(
    resolver: &dyn WslCommandResolver,
    settings: &BackendDesktopSettings,
    port: u16,
    desktop_bootstrap_token: String,
    log_path: PathBuf,
) -> Result<BackendLaunchPlan, String> {
    let running_distro = resolve_wsl_distro(resolver, settings)?;
    resolve_wsl_launch_plan_for_distro(
        resolver,
        running_distro,
        port,
        desktop_bootstrap_token,
        log_path,
        PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
        "Local".to_string(),
    )
}

fn resolve_wsl_secondary_launch_plan<R: Runtime>(
    resolver: &dyn WslCommandResolver,
    app: &AppHandle<R>,
    settings: &BackendDesktopSettings,
    primary_port: u16,
) -> Result<BackendLaunchPlan, String> {
    let running_distro = resolve_wsl_distro(resolver, settings)?;
    let port = pick_desktop_backend_port_excluding(&[primary_port]).ok_or_else(|| {
        format!("Could not find an available desktop backend port outside {primary_port}.")
    })?;
    let log_path = wsl_backend_log_path(app, &running_distro)?;
    let (environment_id, label) = configured_wsl_secondary_identity(settings);
    resolve_wsl_launch_plan_for_distro(
        resolver,
        running_distro,
        port,
        Uuid::new_v4().simple().to_string(),
        log_path,
        environment_id,
        label,
    )
}

fn select_default_launch_plans<NativePrimary, WslPrimary, WslSecondary>(
    settings: &BackendDesktopSettings,
    native_primary: NativePrimary,
    wsl_primary: WslPrimary,
    wsl_secondary: WslSecondary,
) -> Result<DefaultLaunchPlans, BackendPlanError>
where
    NativePrimary: FnOnce() -> Result<BackendLaunchPlan, String>,
    WslPrimary: FnOnce() -> Result<BackendLaunchPlan, String>,
    WslSecondary: FnOnce() -> Result<BackendLaunchPlan, String>,
{
    if settings.wsl_only {
        return wsl_primary()
            .map(|plan| DefaultLaunchPlans {
                plans: vec![plan],
                unavailable_secondary: None,
            })
            .map_err(|detail| BackendPlanError::WslPrimaryUnavailable { detail });
    }

    let primary_plan = native_primary().map_err(|detail| BackendPlanError::Other { detail })?;
    let mut plans = vec![primary_plan];
    let mut unavailable_secondary = None;
    if settings.wsl_backend_enabled {
        match wsl_secondary() {
            Ok(plan) => plans.push(plan),
            Err(error) => {
                tracing::warn!(
                    target: "bibcode_desktop_tauri::backend",
                    "skipping secondary WSL backend launch planning: {error}"
                );
                unavailable_secondary = Some(configured_wsl_secondary_unavailable(settings, error));
            }
        }
    }
    Ok(DefaultLaunchPlans {
        plans,
        unavailable_secondary,
    })
}

fn configured_wsl_secondary_identity(settings: &BackendDesktopSettings) -> (String, String) {
    match settings.wsl_distro.as_deref() {
        Some(distro) => (
            format!("{WSL_INSTANCE_ID_PREFIX}{distro}"),
            format!("WSL ({distro})"),
        ),
        None => (
            format!("{WSL_INSTANCE_ID_PREFIX}default"),
            "WSL (default)".to_string(),
        ),
    }
}

fn configured_wsl_secondary_unavailable(
    settings: &BackendDesktopSettings,
    detail: String,
) -> BackendUnavailableEnvironment {
    let (environment_id, label) = configured_wsl_secondary_identity(settings);
    BackendUnavailableEnvironment {
        environment_id,
        label,
        configured_distro: settings.wsl_distro.clone(),
        detail,
    }
}

fn classify_primary_start_error(
    plan: &BackendLaunchPlan,
    detail: &str,
) -> Option<BackendPlanError> {
    matches!(
        &plan.target,
        BackendLaunchTarget::ExternalProcess { program, .. } if program == "wsl.exe"
    )
    .then(|| BackendPlanError::WslPrimaryUnavailable {
        detail: detail.to_string(),
    })
}

fn unavailable_wsl_secondary_from_plan(
    plan: &BackendLaunchPlan,
    detail: &str,
) -> Option<BackendUnavailableEnvironment> {
    if plan.config.environment_id == PRIMARY_LOCAL_ENVIRONMENT_ID
        || !matches!(
            &plan.target,
            BackendLaunchTarget::ExternalProcess { program, .. } if program == "wsl.exe"
        )
    {
        return None;
    }
    let configured_distro = plan
        .config
        .environment_id
        .strip_prefix(WSL_INSTANCE_ID_PREFIX)
        .filter(|distro| *distro != "default")
        .map(str::to_string);
    Some(BackendUnavailableEnvironment {
        environment_id: plan.config.environment_id.clone(),
        label: plan.config.label.clone(),
        configured_distro,
        detail: detail.to_string(),
    })
}

fn default_launch_plans<R: Runtime>(
    app: &AppHandle<R>,
    backend_port_resolver: &dyn BackendPortResolver,
    wsl_command_resolver: &dyn WslCommandResolver,
    exposure_override: Option<&str>,
) -> Result<DefaultLaunchPlans, BackendPlanError> {
    let mut settings =
        read_backend_desktop_settings(app).map_err(|detail| BackendPlanError::Other { detail })?;
    if let Some(desired) = exposure_override {
        if !matches!(desired, "local-only" | "network-accessible") {
            return Err(BackendPlanError::Other {
                detail: format!("Unsupported server exposure mode: {desired}"),
            });
        }
        settings.server_exposure_mode = desired.to_string();
    }
    let log_path =
        primary_backend_log_path(app).map_err(|detail| BackendPlanError::Other { detail })?;
    let port = backend_port_resolver.port();
    let desktop_bootstrap_token = Uuid::new_v4().simple().to_string();
    let native_bootstrap_token = desktop_bootstrap_token.clone();
    let native_log_path = log_path.clone();
    select_default_launch_plans(
        &settings,
        || {
            let data_root = crate::config::data_root(app)?;
            let exposure = resolve_backend_exposure(&settings, port);
            let config = BackendRunConfig {
                environment_id: PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
                label: "Local".to_string(),
                running_distro: None,
                port,
                bind_host: exposure.bind_host,
                local_host: DESKTOP_LOOPBACK_HOST.to_string(),
                desktop_bootstrap_token: native_bootstrap_token,
                server_exposure_mode: exposure.mode,
                endpoint_url: exposure.endpoint_url,
                advertised_host: exposure.advertised_host,
                tailscale_serve_enabled: settings.tailscale_serve_enabled,
                tailscale_serve_port: settings.tailscale_serve_port,
            };
            Ok(
                BackendLaunchPlan::local(data_root.effective.clone(), config)
                    .with_data_root(data_root)
                    .with_log_path(native_log_path),
            )
        },
        || {
            resolve_wsl_primary_launch_plan(
                wsl_command_resolver,
                &settings,
                port,
                desktop_bootstrap_token,
                log_path,
            )
        },
        || resolve_wsl_secondary_launch_plan(wsl_command_resolver, app, &settings, port),
    )
}

fn primary_backend_log_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(state_dir(app)?
        .join("logs")
        .join(PRIMARY_BACKEND_LOG_FILE_NAME))
}

fn wsl_backend_log_path<R: Runtime>(app: &AppHandle<R>, distro: &str) -> Result<PathBuf, String> {
    let filename = format!(
        "{WSL_BACKEND_LOG_FILE_PREFIX}{}{WSL_BACKEND_LOG_FILE_EXTENSION}",
        sanitize_backend_log_file_segment(distro)
    );
    Ok(state_dir(app)?.join("logs").join(filename))
}

fn sanitize_backend_log_file_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedBackendExposure {
    mode: String,
    bind_host: String,
    endpoint_url: Option<String>,
    advertised_host: Option<String>,
}

pub fn resolve_lan_advertised_host() -> Option<String> {
    select_lan_advertised_host(default_route_ip())
}

fn select_lan_advertised_host(default_route: Option<IpAddr>) -> Option<String> {
    default_route
        .filter(IpAddr::is_ipv4)
        .filter(|ip| classify_advertised_address(*ip).default_eligible)
        .map(|ip| ip.to_string())
}

fn resolve_backend_exposure(
    settings: &BackendDesktopSettings,
    port: u16,
) -> ResolvedBackendExposure {
    resolve_backend_exposure_with(settings, port, resolve_lan_advertised_host)
}

fn resolve_backend_exposure_with(
    settings: &BackendDesktopSettings,
    port: u16,
    resolve_advertised_host: impl FnOnce() -> Option<String>,
) -> ResolvedBackendExposure {
    if settings.server_exposure_mode != "network-accessible" {
        return ResolvedBackendExposure {
            mode: "local-only".to_string(),
            bind_host: DESKTOP_LOOPBACK_HOST.to_string(),
            endpoint_url: None,
            advertised_host: None,
        };
    }

    match resolve_advertised_host() {
        Some(advertised_host) => ResolvedBackendExposure {
            mode: "network-accessible".to_string(),
            bind_host: DESKTOP_LAN_BIND_HOST.to_string(),
            endpoint_url: Some(format!("http://{advertised_host}:{port}")),
            advertised_host: Some(advertised_host),
        },
        None => ResolvedBackendExposure {
            mode: "local-only".to_string(),
            bind_host: DESKTOP_LOOPBACK_HOST.to_string(),
            endpoint_url: None,
            advertised_host: None,
        },
    }
}

fn select_desktop_backend_port() -> Option<u16> {
    select_desktop_backend_port_excluding(&[])
}

fn pick_desktop_backend_port_excluding(excluded: &[u16]) -> Option<u16> {
    select_desktop_backend_port_excluding(excluded).or_else(|| {
        (0..32)
            .filter_map(|_| portpicker::pick_unused_port())
            .find(|port| !excluded.contains(port))
    })
}

fn select_desktop_backend_port_excluding(excluded: &[u16]) -> Option<u16> {
    (DEFAULT_BACKEND_PORT..=MAX_TCP_PORT).find(|port| {
        !excluded.contains(port)
            && DESKTOP_BACKEND_PORT_PROBE_HOSTS
                .iter()
                .all(|host| can_listen_on_host(*port, host))
    })
}

/// Probes whether `port` can still be claimed on `host` without opening a listening socket.
///
/// Port selection checks the loopback and both wildcard addresses so a later
/// share-widening rebind keeps the same port. Binding alone detects the
/// address-in-use conflict; calling `listen` is what makes Windows Defender
/// Firewall treat the executable as a network server and raise its inbound
/// prompt (or silently create Block rules) on the first launch, so the probe
/// deliberately never listens. Unix keeps `SO_REUSEADDR` to match the
/// semantics of the listener the server creates afterwards.
fn bind_port_probe(port: u16, host: &str) -> Option<socket2::Socket> {
    let Ok(mut addresses) = (host, port).to_socket_addrs() else {
        return None;
    };
    let address = addresses.next()?;
    let Ok(socket) = socket2::Socket::new(
        socket2::Domain::for_address(address),
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    ) else {
        return None;
    };
    #[cfg(unix)]
    if socket.set_reuse_address(true).is_err() {
        return None;
    }
    socket.bind(&socket2::SockAddr::from(address)).ok()?;
    Some(socket)
}

fn can_listen_on_host(port: u16, host: &str) -> bool {
    bind_port_probe(port, host).is_some()
}

fn normalize_tailscale_serve_port(value: Option<u64>) -> u16 {
    match value {
        Some(value) if (1..=u16::MAX as u64).contains(&value) => value as u16,
        _ => TAILSCALE_SERVE_PORT,
    }
}

fn normalize_wsl_distro(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| {
            value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
            })
        })
}

fn read_backend_desktop_settings<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<BackendDesktopSettings, String> {
    let path = desktop_base_dir(app)?
        .join(if cfg!(debug_assertions) {
            "dev"
        } else {
            "userdata"
        })
        .join(DESKTOP_SETTINGS_FILE_NAME);

    let raw = match fs::read_to_string(&path) {
        Ok(raw) => Some(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "Could not read desktop backend settings from {}: {error}",
                path.display()
            ));
        }
    };

    Ok(decode_backend_desktop_settings(raw.as_deref()))
}

fn decode_backend_desktop_settings(raw: Option<&str>) -> BackendDesktopSettings {
    let document = raw
        .and_then(|raw| serde_json::from_str::<BackendDesktopSettingsDocument>(raw).ok())
        .unwrap_or_default();

    let wsl_only = document.wsl_only.unwrap_or(false);
    BackendDesktopSettings {
        server_exposure_mode: match document.server_exposure_mode.as_deref() {
            Some("network-accessible") => "network-accessible".to_string(),
            _ => "local-only".to_string(),
        },
        tailscale_serve_enabled: document.tailscale_serve_enabled.unwrap_or(false),
        tailscale_serve_port: normalize_tailscale_serve_port(document.tailscale_serve_port),
        wsl_backend_enabled: wsl_only || document.wsl_backend_enabled.unwrap_or(false),
        wsl_only,
        wsl_distro: normalize_wsl_distro(document.wsl_distro),
    }
}

fn desktop_base_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    crate::config::data_root(app).map(|resolved| resolved.effective)
}

#[cfg(test)]
pub(crate) fn recovery_test_supervisor(
    plan: BackendLaunchPlan,
    failure: BackendStartFailure,
) -> BackendSupervisor {
    let supervisor = BackendSupervisor::new();
    supervisor
        .state
        .lock()
        .expect("test supervisor")
        .slots
        .insert(
            backend_slot_key(&plan),
            BackendSlotState {
                launch_plan: Some(plan),
                recovery_failure: Some(failure),
                ..Default::default()
            },
        );
    supervisor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::free_test_port;
    use bibcode_server::diagnostics::{
        DesktopUiObservation, ProcessIdentity, UiCoverage, UiCoverageStatus,
    };
    #[cfg(unix)]
    use bibcode_server::{DataRootRequest, resolve_data_root};
    use bibcode_server::{DataRootSource, RpcExit, ServerMessage};
    use futures_util::{SinkExt, StreamExt};
    use std::{
        cell::Cell,
        collections::VecDeque,
        io::{Read, Write},
        net::TcpListener,
        pin::Pin,
        sync::{
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
            mpsc,
        },
        task::{Context, Poll},
        thread,
        time::Duration,
    };
    use tauri::Listener;
    use tokio::io::{AsyncRead, ReadBuf};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Message, client::IntoClientRequest},
    };

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn soft_termination_command_ignores_appimage_environment() {
        crate::test_support::with_appimage_test_environment_async(
            "backend::tests::soft_termination_command_ignores_appimage_environment",
            async {
                let appdir = PathBuf::from(std::env::var_os("APPDIR").expect("fixture APPDIR"));
                let kill = appdir
                    .parent()
                    .expect("fixture parent")
                    .join("host-bin/kill");
                crate::test_support::write_executable_fixture(
                    &kill,
                    r#"#!/bin/sh
[ -z "${APPIMAGE+x}" ] && [ -z "${PYTHONHOME+x}" ] || exit 81
[ "$LD_LIBRARY_PATH" = /usr/lib ] || exit 82
[ "$1" = -TERM ] || exit 83
exec /bin/kill "$@"
"#,
                    0o755,
                );
                let mut child = Command::new("/bin/sleep")
                    .arg("60")
                    .kill_on_drop(true)
                    .spawn()
                    .expect("child to terminate");

                let requested = request_child_soft_termination(&mut child);
                if !requested {
                    child.start_kill().expect("failed soft-termination cleanup");
                }
                tokio::time::timeout(Duration::from_secs(5), child.wait())
                    .await
                    .expect("child must terminate")
                    .expect("terminated child must be reaped");

                assert!(
                    requested,
                    "host kill command must receive the isolated environment"
                );
            },
        )
        .await;
    }

    #[derive(Debug)]
    struct TestBackendPortResolver {
        port: u16,
    }

    impl TestBackendPortResolver {
        fn new(port: u16) -> Self {
            Self { port }
        }
    }

    impl BackendPortResolver for TestBackendPortResolver {
        fn port(&self) -> u16 {
            self.port
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn supervisors_keep_distinct_wsl_resolvers() {
        let left =
            BackendSupervisor::with_wsl_resolver(Arc::new(TestWslCommandResolver::new("left-wsl")));
        let right = BackendSupervisor::with_wsl_resolver(Arc::new(TestWslCommandResolver::new(
            "right-wsl",
        )));
        let (left_plan, right_plan) =
            tokio::join!(left.test_wsl_plan("Ubuntu"), right.test_wsl_plan("Ubuntu"),);
        let BackendLaunchTarget::ExternalProcess {
            args: left_args, ..
        } = left_plan.expect("left plan").target
        else {
            panic!("left resolver must produce a WSL process plan");
        };
        let BackendLaunchTarget::ExternalProcess {
            args: right_args, ..
        } = right_plan.expect("right plan").target
        else {
            panic!("right resolver must produce a WSL process plan");
        };
        assert!(left_args.iter().any(|arg| arg == "/left-wsl/bibcode"));
        assert!(right_args.iter().any(|arg| arg == "/right-wsl/bibcode"));
    }

    #[tokio::test]
    async fn retained_fixture_connection_buffers_release_after_readiness() {
        let (port, ready, checkpoint, release, server) = spawn_retained_fixture_connection();
        let mut client =
            TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("fixture client should connect");

        client
            .write_all(&[1])
            .expect("fixture readiness should write");
        wait_for_fixture_event(&ready, checkpoint, "retained fixture readiness").await;
        release.send(()).expect("fixture release should send");

        let mut marker = [0_u8; 1];
        client
            .read_exact(&mut marker)
            .expect("buffered fixture release should read");
        assert_eq!(marker, [1]);
        server.join().expect("fixture server should finish");
    }

    #[derive(Debug)]
    struct TestWslCommandResolver {
        _root: tempfile::TempDir,
        command_path: PathBuf,
        server_binary: PathBuf,
        #[cfg(windows)]
        missing_command: bool,
    }

    impl TestWslCommandResolver {
        fn new(label: &str) -> Self {
            let root = tempfile::Builder::new()
                .prefix(&format!("bibcode-desktop-wsl-{label}-"))
                .tempdir()
                .expect("WSL resolver fixture directory should open");
            let server_binary = root.path().join("bibcode");
            fs::write(&server_binary, b"fixture")
                .expect("WSL resolver server fixture should write");
            #[cfg(unix)]
            let command_path = {
                let path = root.path().join("wsl-fixture.sh");
                crate::test_support::write_executable_fixture(
                    &path,
                    &format!(
                        r#"#!/bin/sh
if [ "$1" = "-l" ]; then
  printf '  NAME STATE VERSION\n* Ubuntu Running 2\n  Debian Stopped 2\n'
  exit 0
fi
if [ "$2" = "Fail" ]; then
  printf 'forced failure\n' >&2
  exit 7
fi
if [ "$4" = "wslpath" ]; then
  if [ "$2" = "Empty" ]; then exit 0; fi
  printf '/{label}/bibcode\n'
  exit 0
fi
if [ "$4" = "hostname" ]; then
  if [ "$2" = "Invalid" ]; then printf 'not-an-address\n'; else printf 'not-an-address 172.20.0.2\n'; fi
  exit 0
fi
if [ "$4" = "env" ]; then
  printf 'HOME=/home/bibcode-test\n'
  exit 0
fi
exit 9
"#
                    ),
                    0o700,
                );
                path
            };
            #[cfg(windows)]
            let command_path = {
                let path = root.path().join("wsl-fixture.cmd");
                fs::write(
                    &path,
                    format!(
                        r#"@echo off
if "%1"=="-l" (
  echo   NAME STATE VERSION
  echo * Ubuntu Running 2
  echo   Debian Stopped 2
  exit /b 0
)
if "%2"=="Fail" (
  echo forced failure 1>&2
  exit /b 7
)
if "%4"=="wslpath" (
  if "%2"=="Empty" exit /b 0
  echo /{label}/bibcode
  exit /b 0
)
if "%4"=="hostname" (
  if "%2"=="Invalid" echo not-an-address
  if not "%2"=="Invalid" echo not-an-address 172.20.0.2
  exit /b 0
)
if "%4"=="env" (
  echo HOME=/home/bibcode-test
  exit /b 0
)
exit /b 9
"#
                    ),
                )
                .expect("WSL resolver command fixture should write");
                path
            };
            Self {
                _root: root,
                command_path,
                server_binary,
                #[cfg(windows)]
                missing_command: false,
            }
        }

        #[cfg(windows)]
        fn with_missing_command(mut self) -> Self {
            self.command_path = self._root.path().join("missing-wsl.exe");
            self.missing_command = true;
            self
        }
    }

    impl WslCommandResolver for TestWslCommandResolver {
        fn command(&self) -> std::process::Command {
            #[cfg(windows)]
            {
                if self.missing_command {
                    return std::process::Command::new(&self.command_path);
                }
                let mut command = std::process::Command::new("cmd.exe");
                command.args(["/d", "/s", "/c"]).arg(&self.command_path);
                command
            }
            #[cfg(not(windows))]
            {
                std::process::Command::new(&self.command_path)
            }
        }

        fn server_binary_candidates(&self) -> Result<Vec<PathBuf>, String> {
            Ok(vec![self.server_binary.clone()])
        }
    }

    #[derive(Debug)]
    struct MarkerDesktopUiProcessObserver;

    impl DesktopUiProcessObserver for MarkerDesktopUiProcessObserver {
        fn observe(
            &self,
            _rows: Arc<[bibcode_server::diagnostics::ProcessRow]>,
            _server_identity: ProcessIdentity,
        ) -> Pin<Box<dyn std::future::Future<Output = DesktopUiObservation> + Send + '_>> {
            Box::pin(async {
                DesktopUiObservation {
                    identities: Vec::new(),
                    coverage: UiCoverage {
                        status: UiCoverageStatus::Available,
                        message: None,
                    },
                }
            })
        }
    }

    #[derive(Debug, Default)]
    struct RecordingDesktopUiProcessObserver {
        observations: AtomicUsize,
    }

    impl RecordingDesktopUiProcessObserver {
        fn observation_count(&self) -> usize {
            self.observations.load(AtomicOrdering::SeqCst)
        }
    }

    impl DesktopUiProcessObserver for RecordingDesktopUiProcessObserver {
        fn observe(
            &self,
            _rows: Arc<[bibcode_server::diagnostics::ProcessRow]>,
            _server_identity: ProcessIdentity,
        ) -> Pin<Box<dyn std::future::Future<Output = DesktopUiObservation> + Send + '_>> {
            self.observations.fetch_add(1, AtomicOrdering::SeqCst);
            Box::pin(async {
                DesktopUiObservation {
                    identities: Vec::new(),
                    coverage: UiCoverage {
                        status: UiCoverageStatus::Available,
                        message: None,
                    },
                }
            })
        }
    }

    #[tokio::test]
    async fn new_supervisor_falls_back_to_unavailable_ui_observation() {
        let supervisor = BackendSupervisor::new();
        let observer = supervisor.ui_process_observer_for_start();
        let observation = observer
            .observe(
                Arc::from([]),
                ProcessIdentity {
                    pid: 1,
                    started_at: 1,
                },
            )
            .await;

        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
    }

    #[test]
    fn configured_ui_observer_is_reused_for_restart_snapshots() {
        let supervisor = BackendSupervisor::new();
        let expected: Arc<dyn DesktopUiProcessObserver> = Arc::new(MarkerDesktopUiProcessObserver);

        supervisor.install_ui_process_observer(expected.clone());
        let first = supervisor.ui_process_observer_for_start();
        let restart = supervisor.ui_process_observer_for_start();

        assert!(Arc::ptr_eq(&expected, &first));
        assert!(Arc::ptr_eq(&first, &restart));
    }

    #[tokio::test]
    async fn configured_ui_observer_reaches_initial_and_restarted_in_process_runtimes() {
        let state = tempfile::tempdir().expect("state tempdir should open");
        let supervisor = BackendSupervisor::new();
        let observer = Arc::new(RecordingDesktopUiProcessObserver::default());
        supervisor.install_ui_process_observer(observer.clone());
        let plan = BackendLaunchPlan::local(state.path().to_path_buf(), local_test_config(0));
        let readiness = BackendReadinessConfig::default();
        let restart = BackendRestartConfig {
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            monitor_interval: Duration::from_millis(10),
        };

        let initial = supervisor
            .start_with_options(plan.clone(), readiness, restart)
            .await
            .expect("initial in-process backend should start");
        request_process_diagnostics(&initial).await;
        assert_eq!(observer.observation_count(), 1);

        let initial_runtime = {
            let state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            let slot = state
                .slots
                .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
                .expect("initial backend slot should exist");
            let Some(ManagedBackend::Runtime(runtime)) = &slot.backend else {
                panic!("initial backend should be in-process");
            };
            runtime.clone()
        };
        initial_runtime.request_stop();
        initial_runtime
            .wait_for_completion()
            .await
            .expect("initial in-process backend should stop");

        supervisor.schedule_restart(plan, readiness, restart, "test restart".to_string());
        let restarted = wait_for_restart_config(&supervisor, readiness.timeout).await;
        request_process_diagnostics(&restarted).await;
        assert_eq!(observer.observation_count(), 2);

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted backend should stop");
    }

    struct ScriptedReader {
        steps: VecDeque<io::Result<Vec<u8>>>,
        completion: Option<oneshot::Sender<()>>,
    }

    impl AsyncRead for ScriptedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.steps.pop_front() {
                Some(Ok(bytes)) => {
                    buffer.put_slice(&bytes);
                    Poll::Ready(Ok(()))
                }
                Some(Err(error)) => {
                    if let Some(completion) = self.completion.take() {
                        let _ = completion.send(());
                    }
                    Poll::Ready(Err(error))
                }
                None => {
                    if let Some(completion) = self.completion.take() {
                        let _ = completion.send(());
                    }
                    Poll::Ready(Ok(()))
                }
            }
        }
    }

    fn spawn_http_response(
        response: &'static [u8],
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let address = listener.local_addr().expect("listener address");
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("listener should accept one request");
            let mut buffer = [0_u8; 4096];
            let count = stream.read(&mut buffer).expect("request should read");
            sender
                .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                .expect("request should be captured");
            stream.write_all(response).expect("response should write");
        });
        (format!("http://{address}"), receiver, server)
    }

    fn spawn_http_responses(
        responses: Vec<&'static [u8]>,
    ) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let address = listener.local_addr().expect("listener address");
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().expect("listener should accept request");
                let mut buffer = [0_u8; 4096];
                let count = stream.read(&mut buffer).expect("request should read");
                sender
                    .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                    .expect("request should be captured");
                stream.write_all(response).expect("response should write");
            }
        });
        (format!("http://{address}"), receiver, server)
    }

    fn spawn_retained_fixture_connection() -> (
        u16,
        Arc<FixtureEvent>,
        u64,
        mpsc::Sender<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("fixture event listener should bind");
        let port = listener
            .local_addr()
            .expect("fixture event listener address")
            .port();
        let ready = Arc::new(FixtureEvent::default());
        let checkpoint = ready.checkpoint();
        let server_ready = ready.clone();
        let (release, wait_for_release) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture event should arrive");
            let mut marker = [0_u8; 1];
            stream
                .read_exact(&mut marker)
                .expect("fixture readiness marker should read");
            assert_eq!(marker, [1]);
            server_ready.publish();
            wait_for_release
                .recv()
                .expect("fixture release should arrive");
            stream
                .write_all(&[1])
                .expect("fixture release marker should write");
        });
        (port, ready, checkpoint, release, server)
    }

    #[cfg(windows)]
    fn spawn_external_backend_http_server(
        shutdown_release: mpsc::Sender<()>,
    ) -> (u16, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let (sender, receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            for (index, response) in [
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".as_slice(),
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
                    .as_slice(),
            ]
            .into_iter()
            .enumerate()
            {
                let (mut stream, _) = listener.accept().expect("listener should accept request");
                let mut buffer = [0_u8; 8192];
                let count = stream.read(&mut buffer).expect("request should read");
                sender
                    .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                    .expect("request should be captured");
                stream.write_all(response).expect("response should write");
                stream.flush().expect("response should flush");
                if index == 1 {
                    shutdown_release
                        .send(())
                        .expect("external backend release should send");
                }
            }
        });
        (port, receiver, server)
    }

    #[cfg(windows)]
    fn powershell_string_literal(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "''"))
    }

    fn test_cli_data_root(base_dir: &Path) -> ResolvedDataRoot {
        ResolvedDataRoot {
            source: DataRootSource::Cli,
            requested: base_dir.to_path_buf(),
            effective: base_dir.to_path_buf(),
            is_filesystem_alias: false,
        }
    }

    async fn start_test_server(
        base_dir: &Path,
    ) -> (bibcode_server::ServerHandle, BackendRunConfig) {
        prepare_isolated_test_server_settings(base_dir)
            .expect("isolated desktop test settings should write");
        let mut config = local_test_config(0);
        let handle = ServerRuntime::start_with_ui_process_observer(
            server_config_for_launch(test_cli_data_root(base_dir), &config)
                .with_hosting_executable_dir_for_integration_test(
                    base_dir.join("missing-hosting-bin"),
                ),
            Arc::new(UnavailableDesktopUiProcessObserver),
        )
        .await
        .expect("test server should start");
        config.port = handle.local_addr().port();
        (handle, config)
    }

    async fn start_rpc_test_server(
        base_dir: &Path,
    ) -> (bibcode_server::ServerHandle, BackendRunConfig) {
        prepare_isolated_test_server_settings(base_dir)
            .expect("isolated desktop test settings should write");
        let mut config = local_test_config(0);
        let server_config = server_config_for_launch(test_cli_data_root(base_dir), &config)
            .with_hosting_executable_dir_for_integration_test(base_dir.join("missing-hosting-bin"))
            .with_unsafe_no_auth();
        let handle = ServerRuntime::start_with_ui_process_observer(
            server_config,
            Arc::new(UnavailableDesktopUiProcessObserver),
        )
        .await
        .expect("RPC test server should start");
        config.port = handle.local_addr().port();
        (handle, config)
    }

    async fn start_wide_test_server(
        base_dir: &Path,
    ) -> (bibcode_server::ServerHandle, BackendRunConfig) {
        prepare_isolated_test_server_settings(base_dir)
            .expect("isolated desktop test settings should write");
        let mut config = local_test_config(0);
        config.bind_host = "0.0.0.0".to_string();
        let handle = ServerRuntime::start_with_ui_process_observer(
            server_config_for_launch(test_cli_data_root(base_dir), &config),
            Arc::new(UnavailableDesktopUiProcessObserver),
        )
        .await
        .expect("wide test server should start");
        config.port = handle.local_addr().port();
        (handle, config)
    }

    async fn request_rpc<S>(
        socket: &mut tokio_tungstenite::WebSocketStream<S>,
        id: usize,
        method: &str,
        payload: Value,
        response_timeout: Duration,
    ) -> ServerMessage
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": id.to_string(),
                    "tag": method,
                    "payload": payload,
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("RPC request should send");
        // Skip the server's 15 s heartbeat Ping and any Pong: tungstenite
        // queues the Pong reply itself and sends it on the next read. The
        // timeout bounds the whole wait.
        let next_frame_past_heartbeat = async {
            loop {
                match socket.next().await {
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    other => break other,
                }
            }
        };
        let frame = tokio::time::timeout(response_timeout, next_frame_past_heartbeat)
            .await
            .expect("RPC response should arrive before the timeout")
            .expect("RPC socket should remain open")
            .expect("RPC response frame should be valid");
        let Message::Text(text) = frame else {
            panic!("expected an RPC text frame, got {frame:?}");
        };
        serde_json::from_str(&text).expect("RPC response should decode")
    }

    async fn request_process_diagnostics(config: &BackendRunConfig) {
        let token: Value = reqwest::Client::new()
            .post(format!("{}/oauth/token", config.http_base_url()))
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:token-exchange",
                ),
                ("subject_token", config.desktop_bootstrap_token.as_str()),
                (
                    "subject_token_type",
                    "urn:bibcode:params:oauth:token-type:environment-bootstrap",
                ),
                (
                    "requested_token_type",
                    "urn:ietf:params:oauth:token-type:access_token",
                ),
            ])
            .send()
            .await
            .expect("desktop bootstrap token should exchange")
            .json()
            .await
            .expect("token response should decode");
        let access_token = token["access_token"]
            .as_str()
            .expect("token response should include access token");
        let mut request = format!("{}/ws", config.ws_base_url())
            .into_client_request()
            .expect("WebSocket request should build");
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {access_token}")
                .parse()
                .expect("authorization header should parse"),
        );
        let (mut socket, _) = connect_async(request)
            .await
            .expect("desktop runtime WebSocket should connect");
        let response = request_rpc(
            &mut socket,
            1,
            "server.getProcessDiagnostics",
            json!({}),
            Duration::from_secs(10),
        )
        .await;
        assert_rpc_completed("server.getProcessDiagnostics", &response);
        socket.close(None).await.expect("RPC socket should close");
    }

    async fn wait_for_restart_config(
        supervisor: &BackendSupervisor,
        readiness_timeout: Duration,
    ) -> BackendRunConfig {
        tokio::time::timeout(readiness_timeout, async {
            loop {
                let checkpoint = supervisor.runtime_published.checkpoint();
                let config = {
                    let state = supervisor
                        .state
                        .lock()
                        .expect("backend supervisor mutex poisoned");
                    state
                        .slots
                        .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
                        .and_then(|slot| {
                            let ManagedBackend::Runtime(runtime) = slot.backend.as_ref()? else {
                                return None;
                            };
                            (runtime.run_id == 1 && !slot.restart_scheduled)
                                .then(|| slot.launch_plan.as_ref().map(|plan| plan.config.clone()))
                                .flatten()
                        })
                };
                if let Some(config) = config {
                    return config;
                }
                supervisor.runtime_published.wait_after(checkpoint).await;
            }
        })
        .await
        .expect("scheduled restart should start an in-process runtime")
    }

    async fn wait_for_fixture_event(event: &FixtureEvent, checkpoint: u64, description: &str) {
        // This is a deadlock guard for test coordination, not a product
        // readiness budget. Windows runners can take several seconds just to
        // schedule a fresh PowerShell process while the Rust suite is loaded.
        tokio::time::timeout(Duration::from_secs(15), event.wait_after(checkpoint))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
    }

    fn assert_rpc_completed(method: &str, message: &ServerMessage) {
        assert!(
            matches!(
                message,
                ServerMessage::Exit {
                    exit: RpcExit::Success { .. } | RpcExit::Failure { .. },
                    ..
                }
            ),
            "{method} should complete with a typed Effect exit, got {message:?}"
        );
    }

    fn local_test_config(port: u16) -> BackendRunConfig {
        BackendRunConfig {
            environment_id: PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
            label: "Local".to_string(),
            running_distro: None,
            port,
            bind_host: "127.0.0.1".to_string(),
            local_host: "127.0.0.1".to_string(),
            desktop_bootstrap_token: "desktop-token".to_string(),
            server_exposure_mode: "local-only".to_string(),
            endpoint_url: None,
            advertised_host: None,
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
        }
    }

    #[test]
    fn isolated_test_server_settings_disable_host_provider_probes() {
        let root = tempfile::tempdir().expect("isolated state tempdir should open");

        prepare_isolated_test_server_settings(root.path())
            .expect("isolated desktop test settings should write");

        let settings: Value = serde_json::from_slice(
            &fs::read(root.path().join("userdata/settings.json"))
                .expect("isolated desktop test settings should read"),
        )
        .expect("isolated desktop test settings should decode");
        assert_eq!(settings["enableProviderUpdateChecks"], false);
        for provider in ["codex", "claudeAgent", "cursor", "grok", "opencode"] {
            assert_eq!(
                settings["providers"][provider]["enabled"], false,
                "{provider} must not probe the developer host during desktop tests"
            );
            let binary = Path::new(
                settings["providers"][provider]["binaryPath"]
                    .as_str()
                    .unwrap(),
            );
            assert!(
                binary.is_absolute() && !binary.exists(),
                "{provider} must resolve only the deliberately absent fixture binary"
            );
        }
    }

    #[tokio::test]
    async fn update_snapshot_tracks_primary_and_secondary_running_set_for_exact_restart() {
        let primary_state = tempfile::tempdir().expect("primary state tempdir should open");
        let secondary_state = tempfile::tempdir().expect("secondary state tempdir should open");
        let supervisor = BackendSupervisor::new();
        let primary_plan = BackendLaunchPlan::local(
            primary_state.path().to_path_buf(),
            local_test_config(free_test_port()),
        );

        supervisor
            .start(primary_plan)
            .await
            .expect("primary backend should start");
        let mut secondary_config = local_test_config(free_test_port());
        secondary_config.environment_id = "wsl:Ubuntu".to_string();
        secondary_config.label = "WSL (Ubuntu)".to_string();
        secondary_config.desktop_bootstrap_token = "secondary-token".to_string();
        let secondary_plan =
            BackendLaunchPlan::local(secondary_state.path().to_path_buf(), secondary_config);
        supervisor
            .start(secondary_plan)
            .await
            .expect("secondary backend should start");

        let snapshot = supervisor.snapshot_for_update();
        assert_eq!(snapshot.environments.len(), 2);
        assert!(snapshot.environments[0].primary);
        assert!(snapshot.environments[0].running);
        assert!(!snapshot.environments[1].primary);
        assert!(snapshot.environments[1].running);

        supervisor
            .stop_update_snapshot(&snapshot)
            .await
            .expect("snapshot backends should stop");
        supervisor
            .restart_update_snapshot(&snapshot)
            .await
            .expect("exact snapshot should restart");

        let restarted = supervisor.snapshot_for_update();
        assert_eq!(
            restarted
                .environments
                .iter()
                .filter(|environment| environment.running)
                .map(|environment| environment.environment_id.as_str())
                .collect::<Vec<_>>(),
            vec!["primary", "wsl:Ubuntu"]
        );
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted backends should stop");
    }

    #[test]
    fn update_snapshot_keeps_a_configured_missing_secondary_explicitly_unprotected() {
        let supervisor = BackendSupervisor::new();
        supervisor.record_unavailable_environment(BackendUnavailableEnvironment {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            configured_distro: Some("Ubuntu".to_string()),
            detail: "distribution is unavailable".to_string(),
        });

        let snapshot = supervisor.snapshot_for_update();
        let environment = snapshot
            .environments
            .iter()
            .find(|environment| environment.environment_id == "wsl:Ubuntu")
            .expect("configured missing secondary should remain in topology");
        assert!(!environment.primary);
        assert!(!environment.running);
        assert_eq!(
            environment.unprotected_reason.as_deref(),
            Some("distribution is unavailable")
        );
    }

    #[tokio::test]
    async fn update_snapshot_excludes_new_backend_starts_until_recovery_finishes() {
        let state = tempfile::tempdir().expect("backend state tempdir should open");
        let supervisor = BackendSupervisor::new();
        let snapshot = supervisor
            .begin_update_snapshot()
            .await
            .expect("update coordination should begin");
        let plan = BackendLaunchPlan::local(state.path().to_path_buf(), local_test_config(0));

        assert!(
            supervisor
                .start(plan.clone())
                .await
                .expect_err("new start must be excluded")
                .contains("paused during update protection")
        );

        supervisor
            .restart_update_snapshot(&snapshot)
            .await
            .expect("empty prior set recovery should finish");
        supervisor
            .start(plan)
            .await
            .expect("starts should resume after recovery");
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("backend should stop");
    }

    fn recorded_listener_bind_retries(supervisor: &BackendSupervisor) -> ListenerBindRetryRecord {
        supervisor
            .listener_bind_retries
            .lock()
            .expect("listener bind retry test record mutex poisoned")
            .clone()
    }

    fn primary_bind_retry(retry: Option<Duration>) -> (String, Option<Duration>) {
        (PRIMARY_LOCAL_ENVIRONMENT_ID.to_owned(), retry)
    }

    /// Takes `port` once its stopped backend has let go of it, allowing for a
    /// descriptor briefly copied into a concurrently spawned child.
    async fn hold_released_port(port: u16) -> TcpListener {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                    Ok(holder) => return holder,
                    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    Err(error) => panic!("port {port} should be free to hold: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the stopped backend should release port {port} within 5s"))
    }

    fn capture_recovery_changes(
        supervisor: &BackendSupervisor,
    ) -> Arc<Mutex<Vec<Vec<BackendRecoveryEntry>>>> {
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let captured = notifications.clone();
        let observed = supervisor.clone();
        supervisor.install_recovery_listener(Arc::new(move || {
            captured
                .lock()
                .expect("notifications")
                .push(observed.backend_recovery());
        }));
        notifications
    }

    #[tokio::test]
    async fn stopped_recovery_is_removed_and_notified_once() {
        let root = tempfile::tempdir().expect("isolated backend data");
        let supervisor = recovery_test_supervisor(
            BackendLaunchPlan::local(root.path().to_path_buf(), local_test_config(43117)),
            BackendStartFailure::PortInUse {
                port: 43118,
                detail: "held port".to_owned(),
            },
        );
        let notifications = capture_recovery_changes(&supervisor);
        supervisor.record_error("held port");
        let recovery = supervisor.backend_recovery();
        assert_eq!(recovery.len(), 1);
        assert_eq!(
            recovery[0].port, 43118,
            "typed bind port wins over the plan port"
        );
        assert_eq!(notifications.lock().expect("notifications").len(), 1);
        supervisor.record_error("same visible failure");
        assert_eq!(notifications.lock().expect("notifications").len(), 1);
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("stop");
        assert!(supervisor.backend_recovery().is_empty());
        assert_eq!(notifications.lock().expect("notifications").len(), 2);
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("idempotent stop");
        assert_eq!(notifications.lock().expect("notifications").len(), 2);
    }

    #[test]
    fn recovery_restart_failure_reclassifies_only_pending_slots() {
        let root = tempfile::tempdir().expect("isolated backend data");
        let plan = BackendLaunchPlan::local(root.path().to_path_buf(), local_test_config(43117));
        let supervisor = recovery_test_supervisor(
            plan.clone(),
            BackendStartFailure::PortInUse {
                port: 43118,
                detail: "held port".to_owned(),
            },
        );
        let notifications = capture_recovery_changes(&supervisor);
        let other = BackendStartFailure::Other {
            detail: "broken store".to_owned(),
        };
        supervisor.record_recovery_restart_failure(&plan, &other);
        assert_eq!(supervisor.backend_recovery()[0].failure, other);
        assert_eq!(supervisor.backend_recovery()[0].port, 43117);
        assert_eq!(notifications.lock().expect("notifications").len(), 1);
        supervisor.record_plan_error(&plan, "new diagnostic".to_owned());
        assert_eq!(supervisor.backend_recovery()[0].failure, other);
        assert_eq!(notifications.lock().expect("notifications").len(), 1);
        supervisor.record_recovery_restart_failure(&plan, &other);
        assert_eq!(notifications.lock().expect("notifications").len(), 1);

        let first_start = BackendSupervisor::new();
        first_start.record_plan_error(&plan, "first start failed".to_owned());
        first_start.record_recovery_restart_failure(&plan, &other);
        assert!(first_start.backend_recovery().is_empty());
    }

    #[test]
    fn removing_recovery_launch_plans_clears_entries_and_notifies() {
        for unavailable in [false, true] {
            let root = tempfile::tempdir().expect("isolated backend data");
            let plan =
                BackendLaunchPlan::local(root.path().to_path_buf(), local_test_config(43117));
            let supervisor = recovery_test_supervisor(
                plan,
                BackendStartFailure::Other {
                    detail: "restart failed".to_owned(),
                },
            );
            let notifications = capture_recovery_changes(&supervisor);
            supervisor.record_error("restart failed");
            if unavailable {
                supervisor.record_unavailable_environment(BackendUnavailableEnvironment {
                    environment_id: "primary".to_owned(),
                    label: "Local".to_owned(),
                    configured_distro: None,
                    detail: "no longer available".to_owned(),
                });
            } else {
                supervisor.record_planning_error(BackendPlanError::Other {
                    detail: "planning failed".to_owned(),
                });
            }
            assert!(supervisor.backend_recovery().is_empty());
            assert_eq!(notifications.lock().expect("notifications").len(), 2);
            let state = supervisor.state.lock().expect("supervisor");
            assert!(state.slots["primary"].recovery_failure.is_none());
        }
    }

    #[tokio::test]
    async fn recovery_publication_tolerates_concurrent_slot_removal() {
        let root = tempfile::tempdir().expect("isolated data");
        let invalid_root = root.path().join("not-a-directory");
        fs::write(&invalid_root, b"file").expect("broken store fixture");
        let plan = BackendLaunchPlan::local(invalid_root, local_test_config(43117));
        let supervisor = recovery_test_supervisor(
            plan.clone(),
            BackendStartFailure::PortInUse {
                port: 43117,
                detail: "previous restart".to_owned(),
            },
        );
        let state = supervisor.state.clone();
        supervisor.install_recovery_listener(Arc::new(move || {
            // Simulate stop clearing the slot after recording an error releases its lock.
            state.lock().expect("supervisor").slots.clear();
        }));
        let snapshot = BackendUpdateSnapshot {
            environments: Vec::new(),
            running_plans: vec![plan],
            unavailable_environments: Vec::new(),
            in_process_maintenance: BTreeMap::new(),
        };
        let failure = supervisor
            .restart_update_snapshot(&snapshot)
            .await
            .expect_err("broken store");
        assert!(matches!(
            failure.entries[0].failure,
            BackendStartFailure::Other { .. }
        ));
        assert!(supervisor.backend_recovery().is_empty());
        assert!(supervisor.project_data_targets().is_empty());
        assert!(!supervisor.update_coordination_in_progress());
    }

    #[tokio::test]
    async fn failed_update_recovery_retains_and_retries_primary() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "backend::tests::failed_update_recovery_retains_and_retries_primary",
        ) else {
            return;
        };
        let root = tempfile::tempdir().expect("isolated backend data");
        let port = free_test_port();
        let supervisor = BackendSupervisor::new();
        let notifications = capture_recovery_changes(&supervisor);
        supervisor
            .start(BackendLaunchPlan::local(
                root.path().to_path_buf(),
                local_test_config(port),
            ))
            .await
            .expect("primary starts");
        let snapshot = supervisor.begin_update_snapshot().await.expect("snapshot");
        supervisor
            .stop_update_snapshot(&snapshot)
            .await
            .expect("stop");
        let holder = hold_released_port(port).await;
        let error = supervisor
            .restart_update_snapshot(&snapshot)
            .await
            .expect_err("port is held");
        assert_eq!(supervisor.backend_recovery(), error.entries);
        assert_eq!(notifications.lock().expect("notifications").len(), 1);
        assert_eq!(error.entries.len(), 1);
        assert_eq!(error.entries[0].environment_id, "primary");
        assert_eq!(error.entries[0].label, "Local");
        assert_eq!(error.entries[0].port, port);
        assert!(
            matches!(error.entries[0].failure, BackendStartFailure::PortInUse { port: actual, .. } if actual == port)
        );
        assert!(error.to_string().starts_with("Could not restart every desktop backend: Local: Could not start in-process desktop backend: "));
        let targets = supervisor.project_data_targets();
        assert_eq!(targets.len(), 1, "the failed plan remains registered");
        assert_eq!(targets[0].environment_id, "primary");
        assert_eq!(targets[0].launch_plan.config.port, port);
        assert!(!targets[0].running);
        assert!(supervisor.local_environment_bootstraps().is_empty());
        drop(
            supervisor
                .begin_project_data_operation("primary")
                .await
                .expect("coordination released"),
        );

        drop(holder);
        crate::data_safety::retry_project_data(&supervisor, "primary")
            .await
            .expect("retry on the same port");
        assert!(supervisor.backend_recovery().is_empty());
        assert_eq!(notifications.lock().expect("notifications").len(), 2);
        assert_eq!(
            supervisor.local_environment_bootstraps()[0]["httpBaseUrl"],
            format!("http://127.0.0.1:{port}")
        );
        let starts = recorded_listener_bind_retries(&supervisor);
        crate::data_safety::retry_project_data(&supervisor, "primary")
            .await
            .expect("running retry is inert");
        assert_eq!(recorded_listener_bind_retries(&supervisor), starts);
        assert_eq!(notifications.lock().expect("notifications").len(), 2);
        assert_eq!(starts.last(), Some(&primary_bind_retry(None)));
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("cleanup");
        isolated.complete();
    }

    #[tokio::test]
    async fn failed_update_recovery_can_restart_through_exposure() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "backend::tests::failed_update_recovery_can_restart_through_exposure",
        ) else {
            return;
        };
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};
        let root = tempfile::tempdir().expect("isolated backend data");
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(root.path().join("data-root")))
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let port = free_test_port();
        let supervisor =
            BackendSupervisor::with_backend_port_resolver(Arc::new(SequenceBackendPortResolver {
                ports: Mutex::new(VecDeque::from([port, port])),
            }));
        let notifications = capture_recovery_changes(&supervisor);
        supervisor
            .start_default(app.handle().clone())
            .await
            .expect("primary starts");
        let snapshot = supervisor.begin_update_snapshot().await.expect("snapshot");
        supervisor
            .stop_update_snapshot(&snapshot)
            .await
            .expect("stop");
        let holder = hold_released_port(port).await;
        supervisor
            .restart_update_snapshot(&snapshot)
            .await
            .expect_err("port is held");
        assert_eq!(supervisor.backend_recovery().len(), 1);
        assert_eq!(notifications.lock().expect("notifications").len(), 1);
        drop(holder);
        let restarted = supervisor
            .restart_default_if_active_preserving_exposure(app.handle().clone())
            .await
            .expect("exposure restart")
            .expect("failed primary is still active");
        assert_eq!(restarted.port, port);
        assert!(supervisor.backend_recovery().is_empty());
        assert_eq!(notifications.lock().expect("notifications").len(), 2);
        assert_eq!(supervisor.local_environment_bootstraps().len(), 1);
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("cleanup");
        isolated.complete();
    }

    #[tokio::test]
    async fn update_recovery_restart_waits_for_a_port_that_is_still_held() {
        let state = tempfile::tempdir().expect("backend state tempdir should open");
        let port = free_test_port();
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(BackendLaunchPlan::local(
                state.path().to_path_buf(),
                local_test_config(port),
            ))
            .await
            .expect("backend should start on its test port");
        let snapshot = supervisor
            .begin_update_snapshot()
            .await
            .expect("update coordination should begin");
        supervisor
            .stop_update_snapshot(&snapshot)
            .await
            .expect("snapshot backend should stop");

        // Something still holds the port for half a second after the stop.
        let holder = hold_released_port(port).await;
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            drop(holder);
        });
        let restart = supervisor.restart_update_snapshot(&snapshot).await;
        release.join().expect("port holder thread should finish");

        restart.expect("update recovery should restart once the port is released");
        assert_eq!(
            supervisor.current_run_config().map(|config| config.port),
            Some(port),
            "update recovery should restart on the port the backend held"
        );
        assert_eq!(
            recorded_listener_bind_retries(&supervisor),
            vec![
                primary_bind_retry(None),
                primary_bind_retry(Some(SAME_PORT_RESTART_BIND_RETRY)),
            ],
            "only the restart onto the held port may retry its bind"
        );
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted backend should stop");
    }

    #[tokio::test]
    async fn project_data_restart_retries_the_bind_only_for_the_target_it_stopped() {
        let running_state = tempfile::tempdir().expect("running target state tempdir should open");
        let running = BackendSupervisor::new();
        running
            .start(BackendLaunchPlan::local(
                running_state.path().to_path_buf(),
                local_test_config(free_test_port()),
            ))
            .await
            .expect("running target should start");
        {
            let mut operation = running
                .begin_project_data_operation(PRIMARY_LOCAL_ENVIRONMENT_ID)
                .await
                .expect("project-data operation should begin");
            operation
                .stop_selected()
                .await
                .expect("the running target should stop");
            operation
                .restart_after_commit()
                .await
                .expect("the stopped target should restart on its port");
        }
        assert_eq!(
            recorded_listener_bind_retries(&running),
            vec![
                primary_bind_retry(None),
                primary_bind_retry(Some(SAME_PORT_RESTART_BIND_RETRY)),
            ],
            "the restart after stopping a running target may retry its bind"
        );
        running
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted target should stop");

        let failed_state = tempfile::tempdir().expect("failed target state tempdir should open");
        let failed = BackendSupervisor::new();
        failed.record_plan_error(
            &BackendLaunchPlan::local(failed_state.path().to_path_buf(), local_test_config(0)),
            "the first start failed".to_owned(),
        );
        {
            let mut operation = failed
                .begin_project_data_operation(PRIMARY_LOCAL_ENVIRONMENT_ID)
                .await
                .expect("project-data operation should begin for a failed target");
            operation
                .stop_selected()
                .await
                .expect("a stopped target needs no stop");
            operation
                .restart_after_commit()
                .await
                .expect("the failed target should start");
        }
        assert_eq!(
            recorded_listener_bind_retries(&failed),
            vec![primary_bind_retry(None)],
            "a target that was not running held no port to wait for"
        );
        failed
            .stop(BackendShutdownConfig::default())
            .await
            .expect("started target should stop");
    }

    #[tokio::test]
    async fn crash_restart_retries_the_bind_on_the_port_the_crashed_backend_held() {
        let state = tempfile::tempdir().expect("backend state tempdir should open");
        let port = free_test_port();
        let supervisor = BackendSupervisor::new();
        let readiness = BackendReadinessConfig::default();
        let restart = BackendRestartConfig {
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            monitor_interval: Duration::from_millis(10),
        };
        supervisor
            .start_with_options(
                BackendLaunchPlan::local(state.path().to_path_buf(), local_test_config(port)),
                readiness,
                restart,
            )
            .await
            .expect("backend should start on its test port");
        let (runtime, plan) = {
            let state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            let slot = state
                .slots
                .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
                .expect("backend slot should exist");
            let Some(ManagedBackend::Runtime(runtime)) = &slot.backend else {
                panic!("backend should be in-process");
            };
            (
                runtime.clone(),
                slot.launch_plan
                    .clone()
                    .expect("running backend should keep its plan"),
            )
        };
        runtime.request_stop();
        runtime
            .wait_for_completion()
            .await
            .expect("backend should stop");

        supervisor.schedule_restart(plan, readiness, restart, "test crash".to_owned());
        let restarted = wait_for_restart_config(&supervisor, readiness.timeout).await;

        assert_eq!(restarted.port, port);
        assert_eq!(
            recorded_listener_bind_retries(&supervisor),
            vec![
                primary_bind_retry(None),
                primary_bind_retry(Some(SAME_PORT_RESTART_BIND_RETRY)),
            ],
            "a crash restart may retry its bind on the port the crashed backend held"
        );
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted backend should stop");
    }

    #[derive(Debug)]
    struct SequenceBackendPortResolver {
        ports: Mutex<VecDeque<u16>>,
    }

    impl BackendPortResolver for SequenceBackendPortResolver {
        fn port(&self) -> u16 {
            self.ports
                .lock()
                .expect("port sequence mutex poisoned")
                .pop_front()
                .expect("every planning should have a port left in the sequence")
        }
    }

    #[tokio::test]
    async fn default_restart_retries_the_bind_only_for_a_port_the_stopped_backend_held() {
        let Some(isolated) = crate::test_support::isolated_scenario(
            "backend::tests::default_restart_retries_the_bind_only_for_a_port_the_stopped_backend_held",
        ) else {
            return;
        };
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let temp = tempfile::tempdir().expect("isolated desktop data root");
        let mut context = mock_context(noop_assets());
        context.config_mut().identifier = format!(
            "com.bibcode.backend-bind-retry-tests-{}",
            std::process::id()
        );
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(temp.path().join("data-root")))
            .build(context)
            .expect("mock Tauri app");
        let port = free_test_port();
        // Start and restart on one port, then re-plan onto an OS-assigned one.
        let supervisor =
            BackendSupervisor::with_backend_port_resolver(Arc::new(SequenceBackendPortResolver {
                ports: Mutex::new(VecDeque::from([port, port, 0])),
            }));

        let started = supervisor
            .start_default(app.handle().clone())
            .await
            .expect("default backend should start");
        assert_eq!(started.port, port);
        let restarted = supervisor
            .restart_default_if_active_preserving_exposure(app.handle().clone())
            .await
            .expect("default backend should restart on its port")
            .expect("active backend should produce a replacement config");
        assert_eq!(restarted.port, port);
        let replanned = supervisor
            .restart_default_if_active_preserving_exposure(app.handle().clone())
            .await
            .expect("default backend should restart on a new port")
            .expect("active backend should produce a replacement config");
        assert_ne!(replanned.port, port);

        assert_eq!(
            recorded_listener_bind_retries(&supervisor),
            vec![
                primary_bind_retry(None),
                primary_bind_retry(Some(SAME_PORT_RESTART_BIND_RETRY)),
                primary_bind_retry(None),
            ],
            "a default restart retries its bind only on a port its stopped backend held"
        );
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("default backend should stop");
        isolated.complete();
    }

    #[test]
    fn builds_primary_bootstrap_for_frontend_resolution() {
        let config = local_test_config(3773);

        let bootstrap = config.to_environment_bootstrap();

        assert_eq!(bootstrap["id"], "primary");
        assert_eq!(bootstrap["label"], "Local");
        assert_eq!(bootstrap["runningDistro"], Value::Null);
        assert_eq!(bootstrap["httpBaseUrl"], "http://127.0.0.1:3773");
        assert_eq!(bootstrap["wsBaseUrl"], "ws://127.0.0.1:3773");
        assert_eq!(bootstrap["bootstrapToken"], "desktop-token");
    }

    #[test]
    fn run_config_builds_http_and_websocket_urls_from_renderer_host() {
        let mut config = local_test_config(65_535);
        config.local_host = "192.0.2.10".to_string();

        assert_eq!(config.http_base_url(), "http://192.0.2.10:65535");
        assert_eq!(config.ws_base_url(), "ws://192.0.2.10:65535");
    }

    #[test]
    fn backend_lifecycle_defaults_match_operational_limits() {
        assert_eq!(
            BackendReadinessConfig::default(),
            BackendReadinessConfig {
                timeout: Duration::from_secs(30),
                interval: Duration::from_millis(250),
                request_timeout: Duration::from_secs(2),
            }
        );
        assert_eq!(
            BackendShutdownConfig::default(),
            BackendShutdownConfig {
                timeout: Duration::from_secs(5),
            }
        );
        assert_eq!(
            BackendRestartConfig::default(),
            BackendRestartConfig {
                initial_delay: Duration::from_millis(250),
                max_delay: Duration::from_secs(5),
                monitor_interval: Duration::from_millis(250),
            }
        );
        assert_eq!(SAME_PORT_RESTART_BIND_RETRY, Duration::from_secs(3));
    }

    #[test]
    fn builds_local_server_launch_plan() {
        let config = local_test_config(3773);
        let plan =
            BackendLaunchPlan::local(PathBuf::from("C:/Users/mauro/.bibcode"), config.clone());

        assert_eq!(plan.log_path, None);
        assert!(matches!(
            plan.target,
            BackendLaunchTarget::InProcess { ref base_dir, .. } if base_dir == &PathBuf::from("C:/Users/mauro/.bibcode")
        ));

        let logged_plan = plan.with_log_path(PathBuf::from(
            "C:/Users/mauro/.bibcode/dev/logs/server-child.log",
        ));
        assert_eq!(
            logged_plan.log_path,
            Some(PathBuf::from(
                "C:/Users/mauro/.bibcode/dev/logs/server-child.log"
            ))
        );
    }

    #[test]
    fn server_config_for_launch_uses_desktop_runtime_settings() {
        let config = local_test_config(3773);
        let server_config = server_config_for_launch(
            ResolvedDataRoot {
                source: DataRootSource::Cli,
                requested: PathBuf::from("C:/Users/mauro/.bibcode"),
                effective: PathBuf::from("C:/Users/mauro/.bibcode"),
                is_filesystem_alias: false,
            },
            &config,
        );

        assert_eq!(server_config.host, "127.0.0.1");
        assert_eq!(server_config.port, 3773);
        assert_eq!(
            server_config.base_dir,
            PathBuf::from("C:/Users/mauro/.bibcode")
        );
        assert_eq!(
            server_config.desktop_bootstrap_token.as_deref(),
            Some("desktop-token")
        );
        assert_eq!(server_config.environment_id, "primary");
        assert_eq!(server_config.environment_label, "Local");
    }

    #[cfg(unix)]
    #[test]
    fn server_config_for_launch_preserves_environment_alias_diagnostics() {
        let temp = tempfile::tempdir().expect("temporary root");
        let target = temp.path().join("target");
        std::fs::create_dir(&target).expect("target directory");
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&target, &alias).expect("alias symlink");
        let data_root = resolve_data_root(DataRootRequest::explicit(
            DataRootSource::Environment,
            alias.clone(),
            temp.path().to_path_buf(),
        ))
        .expect("resolve environment alias");

        let server_config = server_config_for_launch(data_root.clone(), &local_test_config(0));

        assert_eq!(
            server_config.data_root_request.source,
            DataRootSource::Environment
        );
        assert_eq!(server_config.data_root_request.requested, Some(alias));
        assert_eq!(server_config.base_dir, data_root.effective);
        assert_eq!(server_config.resolved_data_root, Some(data_root));
    }

    #[test]
    fn builds_wsl_launch_plan_with_explicit_binary() {
        let plan = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            running_distro: "Ubuntu".to_string(),
            port: 5050,
            renderer_host: "172.27.0.99".to_string(),
            desktop_bootstrap_token: "desktop-token".to_string(),
            binary_path: "/tmp/bibcode's launch/bibcode".to_string(),
            data_root: "/srv/bibcode data".to_string(),
        })
        .with_log_path(PathBuf::from(
            "C:/Users/mauro/.bibcode/dev/logs/server-child-wsl-Ubuntu.log",
        ));

        assert_eq!(plan.config.environment_id, "wsl:Ubuntu");
        assert_eq!(plan.config.label, "WSL (Ubuntu)");
        assert_eq!(plan.config.running_distro, Some("Ubuntu".to_string()));
        assert_eq!(plan.config.http_base_url(), "http://172.27.0.99:5050");
        assert_eq!(plan.config.bind_host, "0.0.0.0");
        assert!(matches!(
            plan.target,
            BackendLaunchTarget::ExternalProcess {
                ref program,
                ref args,
                ref bootstrap_line,
                ref data_root,
            } if program == "wsl.exe"
                && data_root.as_deref() == Some("/srv/bibcode data")
                && args == &vec![
                    "-d".to_string(),
                    "Ubuntu".to_string(),
                    "--exec".to_string(),
                    "env".to_string(),
                    format!("PATH={WSL_SERVER_SYSTEM_PATH}"),
                    "/tmp/bibcode's launch/bibcode".to_string(),
                    "serve".to_string(),
                    "--bootstrap-fd".to_string(),
                    "0".to_string(),
                ]
                && serde_json::from_str::<Value>(bootstrap_line).expect("bootstrap JSON")
                    == json!({
                        "mode": "desktop",
                        "noBrowser": true,
                        "port": 5050,
                        "host": "0.0.0.0",
                        "desktopBootstrapToken": "desktop-token",
                        "bibcodeHome": "/srv/bibcode data",
                        "wslTransport": true,
                        "tailscaleServeEnabled": false,
                        "tailscaleServePort": 443,
                    })
        ));

        let bootstrap = plan.config.to_environment_bootstrap();
        assert_eq!(bootstrap["id"], "wsl:Ubuntu");
        assert_eq!(bootstrap["label"], "WSL (Ubuntu)");
        assert_eq!(bootstrap["runningDistro"], "Ubuntu");
        assert_eq!(
            plan.log_path,
            Some(PathBuf::from(
                "C:/Users/mauro/.bibcode/dev/logs/server-child-wsl-Ubuntu.log"
            ))
        );
    }

    #[test]
    fn sanitizes_backend_log_file_segments_for_wsl_slots() {
        assert_eq!(
            sanitize_backend_log_file_segment("Ubuntu-22.04"),
            "Ubuntu-22.04"
        );
        assert_eq!(
            sanitize_backend_log_file_segment("my org/Ubuntu LTS"),
            "my_org_Ubuntu_LTS"
        );
        assert_eq!(sanitize_backend_log_file_segment(""), "default");
    }

    #[test]
    fn local_environment_bootstraps_include_parallel_primary_and_wsl_slots() {
        let supervisor = BackendSupervisor::new();
        let primary = BackendLaunchPlan::local(
            PathBuf::from("C:/Users/mauro/.bibcode"),
            local_test_config(3773),
        );
        let wsl = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            running_distro: "Ubuntu".to_string(),
            port: 3774,
            renderer_host: "172.27.0.99".to_string(),
            desktop_bootstrap_token: "wsl-token".to_string(),
            binary_path: "/home/test/bibcode".to_string(),
            data_root: "/home/test/.bibcode".to_string(),
        });

        {
            let mut state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
                BackendSlotState {
                    launch_plan: Some(primary),
                    ..BackendSlotState::default()
                },
            );
            state.slots.insert(
                "wsl:Ubuntu".to_string(),
                BackendSlotState {
                    launch_plan: Some(wsl),
                    ..BackendSlotState::default()
                },
            );
        }

        let bootstraps = supervisor.local_environment_bootstraps();

        assert_eq!(bootstraps.len(), 2);
        assert_eq!(bootstraps[0]["id"], "primary");
        assert_eq!(bootstraps[1]["id"], "wsl:Ubuntu");
        assert_eq!(bootstraps[1]["label"], "WSL (Ubuntu)");
        assert_eq!(bootstraps[1]["httpBaseUrl"], "http://172.27.0.99:3774");
    }

    #[test]
    fn current_run_config_prefers_primary_slot_when_secondary_exists() {
        let supervisor = BackendSupervisor::new();
        let primary = BackendLaunchPlan::local(
            PathBuf::from("C:/Users/mauro/.bibcode"),
            local_test_config(3773),
        );
        let wsl = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            running_distro: "Ubuntu".to_string(),
            port: 3774,
            renderer_host: "172.27.0.99".to_string(),
            desktop_bootstrap_token: "wsl-token".to_string(),
            binary_path: "/home/test/bibcode".to_string(),
            data_root: "/home/test/.bibcode".to_string(),
        });

        {
            let mut state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                "wsl:Ubuntu".to_string(),
                BackendSlotState {
                    launch_plan: Some(wsl),
                    ..BackendSlotState::default()
                },
            );
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
                BackendSlotState {
                    launch_plan: Some(primary),
                    ..BackendSlotState::default()
                },
            );
        }

        let config = supervisor
            .current_run_config()
            .expect("primary config should be selected");

        assert_eq!(config.environment_id, "primary");
        assert_eq!(config.running_distro, None);
    }

    #[test]
    fn supervisor_state_queries_handle_empty_error_and_secondary_slots() {
        let supervisor = BackendSupervisor::new();
        assert_eq!(supervisor.current_run_config(), None);
        assert!(supervisor.local_environment_bootstraps().is_empty());

        let secondary = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: "wsl:Debian".to_string(),
            label: "WSL (Debian)".to_string(),
            running_distro: "Debian".to_string(),
            port: 4_101,
            renderer_host: "172.20.0.2".to_string(),
            desktop_bootstrap_token: "secondary-token".to_string(),
            binary_path: "/usr/local/bin/bibcode".to_string(),
            data_root: "/home/test/.bibcode".to_string(),
        });
        {
            let mut state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
                BackendSlotState {
                    launch_plan: Some(BackendLaunchPlan::local(
                        PathBuf::from("C:/state"),
                        local_test_config(4_100),
                    )),
                    ..BackendSlotState::default()
                },
            );
            state.slots.insert(
                "wsl:Debian".to_string(),
                BackendSlotState {
                    launch_plan: Some(secondary.clone()),
                    ..BackendSlotState::default()
                },
            );
        }

        supervisor.record_error("primary failed");
        let bootstraps = supervisor.local_environment_bootstraps();
        assert_eq!(bootstraps.len(), 1);
        assert_eq!(bootstraps[0]["id"], "wsl:Debian");

        {
            let mut state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.remove(PRIMARY_LOCAL_ENVIRONMENT_ID);
        }
        assert_eq!(supervisor.current_run_config(), Some(secondary.config));
    }

    #[test]
    fn record_plan_error_resets_runtime_state_and_hides_bootstrap() {
        let supervisor = BackendSupervisor::new();
        let plan = BackendLaunchPlan::local(PathBuf::from("C:/state"), local_test_config(4_200));

        supervisor.record_plan_error(&plan, "launch failed".to_string());

        assert!(supervisor.local_environment_bootstraps().is_empty());
        let state = supervisor
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        let slot = state
            .slots
            .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
            .expect("primary slot should be recorded");
        assert_eq!(slot.launch_plan.as_ref(), Some(&plan));
        assert!(slot.backend.is_none());
        assert_eq!(slot.pid, None);
        assert_eq!(slot.last_error.as_deref(), Some("launch failed"));
        assert!(!slot.restart_scheduled);
    }

    #[test]
    fn secondary_wsl_start_failure_remains_in_unavailable_topology() {
        let supervisor = BackendSupervisor::new();
        let primary = BackendLaunchPlan::local(PathBuf::from("C:/state"), local_test_config(4_300));
        let secondary = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: "wsl:Ubuntu".to_string(),
            label: "WSL (Ubuntu)".to_string(),
            running_distro: "Ubuntu".to_string(),
            port: 4_301,
            renderer_host: "172.20.0.2".to_string(),
            desktop_bootstrap_token: "secondary-token".to_string(),
            binary_path: "/usr/local/bin/bibcode".to_string(),
            data_root: "/home/test/.bibcode".to_string(),
        });
        {
            let mut state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
                BackendSlotState {
                    launch_plan: Some(primary),
                    ..BackendSlotState::default()
                },
            );
        }

        supervisor.record_plan_error(
            &secondary,
            "WSL process exited before readiness".to_string(),
        );

        assert_eq!(
            supervisor.local_environment_bootstraps(),
            vec![
                local_test_config(4_300).to_environment_bootstrap(),
                json!({
                    "id": "wsl:Ubuntu",
                    "label": "WSL (Ubuntu)",
                    "configuredDistro": "Ubuntu",
                    "runningDistro": null,
                    "httpBaseUrl": null,
                    "wsBaseUrl": null,
                    "preflightError": {
                        "kind": "wsl-secondary-unavailable",
                        "detail": "WSL process exited before readiness",
                    },
                }),
            ]
        );
    }

    #[test]
    fn supervisor_run_ids_increment_and_saturate() {
        let supervisor = BackendSupervisor::new();
        assert_eq!(supervisor.next_run_id(), Ok(0));
        assert_eq!(supervisor.next_run_id(), Ok(1));

        supervisor
            .state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .next_run_id = u64::MAX;
        assert_eq!(supervisor.next_run_id(), Ok(u64::MAX));
        assert_eq!(supervisor.next_run_id(), Ok(u64::MAX));
    }

    #[test]
    fn restart_desire_requires_a_scheduled_plan_without_a_backend() {
        let supervisor = BackendSupervisor::new();
        assert!(!supervisor.restart_still_desired("missing"));

        let plan = BackendLaunchPlan::local(PathBuf::from("C:/state"), local_test_config(4_250));
        supervisor.schedule_restart(
            plan.clone(),
            BackendReadinessConfig::default(),
            BackendRestartConfig::default(),
            "missing slot".to_string(),
        );
        assert!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .slots
                .is_empty()
        );
        let mut state = supervisor
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        state.slots.insert(
            PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
            BackendSlotState {
                launch_plan: Some(plan),
                restart_scheduled: true,
                ..BackendSlotState::default()
            },
        );
        drop(state);
        assert!(supervisor.restart_still_desired(PRIMARY_LOCAL_ENVIRONMENT_ID));

        let mut state = supervisor
            .state
            .lock()
            .expect("backend supervisor mutex poisoned");
        state
            .slots
            .get_mut(PRIMARY_LOCAL_ENVIRONMENT_ID)
            .expect("primary slot")
            .restart_scheduled = false;
        drop(state);
        assert!(!supervisor.restart_still_desired(PRIMARY_LOCAL_ENVIRONMENT_ID));
    }

    #[tokio::test]
    async fn stopping_an_empty_supervisor_is_idempotent() {
        let supervisor = BackendSupervisor::new();

        supervisor
            .stop(BackendShutdownConfig {
                timeout: Duration::ZERO,
            })
            .await
            .expect("empty supervisor should stop");
        supervisor
            .stop(BackendShutdownConfig {
                timeout: Duration::ZERO,
            })
            .await
            .expect("repeated empty stop should succeed");
    }

    #[tokio::test]
    async fn concurrent_stop_callers_wait_for_the_same_cleanup_completion() {
        let supervisor = BackendSupervisor::new();
        let join_result = Arc::new(AsyncMutex::new(Some(Err(
            "shared cleanup failure".to_string()
        ))));
        let runtime = ManagedBackendRuntime {
            run_id: 77,
            update_maintenance: None,
            stop_requested: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Mutex::new(None)),
            completion: Arc::new(Notify::new()),
            join_result: join_result.clone(),
            stop_requested_event: Arc::default(),
        };
        let cleanup_gate = runtime.join_result.lock().await;
        {
            let mut state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_owned(),
                BackendSlotState {
                    backend: Some(ManagedBackend::Runtime(Box::new(runtime.clone()))),
                    ..BackendSlotState::default()
                },
            );
        }

        let stop_requested_checkpoint = runtime.stop_requested_event.checkpoint();
        let first_supervisor = supervisor.clone();
        let first_stop = tokio::spawn(async move {
            first_supervisor
                .stop(BackendShutdownConfig::default())
                .await
        });
        wait_for_fixture_event(
            &runtime.stop_requested_event,
            stop_requested_checkpoint,
            "first stop request",
        )
        .await;

        let concurrent_wait_checkpoint = supervisor.concurrent_stop_waiting.checkpoint();
        let second_supervisor = supervisor.clone();
        let second_stop = tokio::spawn(async move {
            second_supervisor
                .stop(BackendShutdownConfig::default())
                .await
        });
        wait_for_fixture_event(
            &supervisor.concurrent_stop_waiting,
            concurrent_wait_checkpoint,
            "concurrent stop waiter",
        )
        .await;
        assert!(
            !second_stop.is_finished(),
            "a concurrent stop must wait for the cleanup already in progress"
        );

        drop(cleanup_gate);
        let first_error = first_stop
            .await
            .expect("first stop task should join")
            .expect_err("first stop should surface the cleanup failure");
        let second_error = second_stop
            .await
            .expect("second stop task should join")
            .expect_err("second stop should share the cleanup failure");
        assert_eq!(first_error, "shared cleanup failure");
        assert_eq!(second_error, first_error);
        let later_error = supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect_err("later stop callers should retain the completed cleanup failure");
        assert_eq!(later_error, first_error);
        assert!(matches!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .lifecycle,
            BackendLifecycle::Stopped { .. }
        ));
    }

    #[tokio::test]
    async fn failed_shutdown_does_not_allow_an_explicit_restart() {
        let supervisor = BackendSupervisor::new();
        let runtime = ManagedBackendRuntime {
            run_id: 78,
            update_maintenance: None,
            stop_requested: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Mutex::new(None)),
            completion: Arc::new(Notify::new()),
            join_result: Arc::new(AsyncMutex::new(Some(Err(
                "restart-blocking cleanup failure".to_string(),
            )))),
            stop_requested_event: Arc::default(),
        };
        supervisor
            .state
            .lock()
            .expect("backend supervisor mutex poisoned")
            .slots
            .insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_owned(),
                BackendSlotState {
                    backend: Some(ManagedBackend::Runtime(Box::new(runtime))),
                    ..BackendSlotState::default()
                },
            );

        let shutdown_error = supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect_err("shutdown should surface the cleanup failure");
        assert_eq!(shutdown_error, "restart-blocking cleanup failure");
        let restart_error = supervisor
            .begin_start(true)
            .expect_err("failed shutdown must not open a new lifecycle");
        assert!(restart_error.contains("cleanup"), "{restart_error}");
    }

    /// Deadlock guard for supervisor test gates, not a product budget: it
    /// outlasts a slow start's whole readiness wait, so only a gate that can
    /// never open reaches it.
    const TEST_GATE_DEADLINE: Duration = Duration::from_secs(60);

    async fn within_test_gate_deadline<T>(
        future: impl std::future::Future<Output = T>,
        description: &str,
    ) -> T {
        tokio::time::timeout(TEST_GATE_DEADLINE, future)
            .await
            .unwrap_or_else(|_| {
                panic!("timed out after {TEST_GATE_DEADLINE:?} waiting for {description}")
            })
    }

    /// Waits for `start` to reach its pre-publish gate. A start that ends
    /// first, for example because its port was taken, fails the test with
    /// its result instead of leaving the gate's sender parked forever.
    async fn wait_for_start_at_publish_gate<T: std::fmt::Debug>(
        reached: oneshot::Receiver<ManagedBackend>,
        start: &mut tokio::task::JoinHandle<T>,
    ) -> ManagedBackend {
        within_test_gate_deadline(
            async {
                tokio::select! {
                    reached = reached => {
                        reached.expect("the start publish gate should stay registered")
                    }
                    ended = start => {
                        panic!("start ended before reaching the pre-publish gate: {ended:?}");
                    }
                }
            },
            "the start to reach its pre-publish gate",
        )
        .await
    }

    #[tokio::test]
    async fn start_racing_stop_cleans_late_backend_without_publishing_it() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let port = free_test_port();
        let supervisor = BackendSupervisor::new();
        let (publish_reached, wait_for_publish) = oneshot::channel();
        let (allow_publish, publish_release) = oneshot::channel();
        supervisor.set_start_publish_gate(publish_reached, publish_release);
        let (cleanup_reached, wait_for_cleanup) = oneshot::channel();
        supervisor.set_shutdown_cleanup_reached(cleanup_reached);
        let plan = BackendLaunchPlan::local(temp.path().to_path_buf(), local_test_config(port));

        let start_supervisor = supervisor.clone();
        let late_backend = Arc::new(std::sync::OnceLock::<ManagedBackend>::new());
        let backend_at_return = late_backend.clone();
        let mut start = tokio::spawn(async move {
            let result = start_supervisor
                .start_with_options(
                    plan,
                    BackendReadinessConfig::default(),
                    BackendRestartConfig::default(),
                )
                .await;
            let ManagedBackend::Runtime(runtime) = backend_at_return
                .get()
                .expect("the gate must identify the late runtime before release")
            else {
                panic!("local starts must use an in-process runtime");
            };
            // Do not yield between start returning and observing its own runtime.
            let cleanup_result = runtime
                .join_result
                .try_lock()
                .ok()
                .and_then(|result| result.clone());
            (result, cleanup_result)
        });
        let managed = wait_for_start_at_publish_gate(wait_for_publish, &mut start).await;
        late_backend
            .set(managed.clone())
            .expect("late runtime should be captured once");
        let ManagedBackend::Runtime(runtime) = managed else {
            panic!("local starts must use an in-process runtime");
        };
        let cleanup_gate = runtime.join_result.lock().await;
        assert!(cleanup_gate.is_none(), "the late runtime is still running");
        let stop_requested_checkpoint = runtime.stop_requested_event.checkpoint();

        let stop_supervisor = supervisor.clone();
        let stop =
            tokio::spawn(
                async move { stop_supervisor.stop(BackendShutdownConfig::default()).await },
            );
        within_test_gate_deadline(wait_for_cleanup, "shutdown to clean published backends")
            .await
            .expect("shutdown should finish cleaning currently published backends");
        let shutdown_completed_before_start_cleanup = matches!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .lifecycle,
            BackendLifecycle::Stopped { .. } | BackendLifecycle::Terminated { .. }
        );
        allow_publish
            .send(())
            .expect("blocked start should still be waiting");
        wait_for_fixture_event(
            &runtime.stop_requested_event,
            stop_requested_checkpoint,
            "the late runtime's stop request",
        )
        .await;
        assert!(!start.is_finished(), "start must await this runtime's join");
        assert!(
            !stop.is_finished(),
            "stop must await the late start's cleanup"
        );
        drop(cleanup_gate);
        let (start_result, cleanup_result_at_start_return) =
            within_test_gate_deadline(start, "the late start to finish")
                .await
                .expect("start task should join");
        within_test_gate_deadline(stop, "the stop to finish")
            .await
            .expect("stop task should join")
            .expect("stop should succeed after late startup cleanup");
        if start_result.is_ok() {
            supervisor
                .stop(BackendShutdownConfig::default())
                .await
                .expect("RED cleanup should stop a backend published after shutdown");
        }

        assert!(
            !shutdown_completed_before_start_cleanup,
            "stop must wait for starts that began before shutdown"
        );
        let error = start_result.expect_err("start begun before stop must not publish afterward");
        assert!(error.contains("shutdown"), "{error}");
        assert!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .slots
                .is_empty(),
            "late-created backend must not remain installed"
        );
        assert_eq!(
            cleanup_result_at_start_return,
            Some(Ok(())),
            "late-created backend must be cleaned before start returns"
        );
    }

    #[tokio::test]
    async fn late_start_cleanup_failure_is_shared_retained_and_blocks_restart() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let port = free_test_port();
        let supervisor = BackendSupervisor::new();
        let (publish_reached, wait_for_publish) = oneshot::channel();
        let (allow_publish, publish_release) = oneshot::channel();
        supervisor.set_start_publish_gate(publish_reached, publish_release);
        let (cleanup_reached, wait_for_cleanup) = oneshot::channel();
        supervisor.set_shutdown_cleanup_reached(cleanup_reached);
        supervisor.set_late_start_cleanup_failure("injected late cleanup failure");
        let plan = BackendLaunchPlan::local(temp.path().to_path_buf(), local_test_config(port));

        let start_supervisor = supervisor.clone();
        let mut start = tokio::spawn(async move {
            start_supervisor
                .start_with_options(
                    plan,
                    BackendReadinessConfig::default(),
                    BackendRestartConfig::default(),
                )
                .await
        });
        wait_for_start_at_publish_gate(wait_for_publish, &mut start).await;

        let first_stop_supervisor = supervisor.clone();
        let first_stop = tokio::spawn(async move {
            first_stop_supervisor
                .stop(BackendShutdownConfig::default())
                .await
        });
        within_test_gate_deadline(
            wait_for_cleanup,
            "shutdown to reach its in-flight start wait",
        )
        .await
        .expect("shutdown should reach its in-flight start wait");

        let concurrent_wait_checkpoint = supervisor.concurrent_stop_waiting.checkpoint();
        let second_stop_supervisor = supervisor.clone();
        let second_stop = tokio::spawn(async move {
            second_stop_supervisor
                .stop(BackendShutdownConfig::default())
                .await
        });
        wait_for_fixture_event(
            &supervisor.concurrent_stop_waiting,
            concurrent_wait_checkpoint,
            "late-start concurrent stop waiter",
        )
        .await;
        assert!(
            !first_stop.is_finished() && !second_stop.is_finished(),
            "all stop callers must wait for late startup cleanup"
        );

        allow_publish
            .send(())
            .expect("blocked start should still be waiting");
        let start_error = within_test_gate_deadline(start, "the late start to finish")
            .await
            .expect("start task should join")
            .expect_err("start must surface its late cleanup failure");
        assert!(
            start_error.contains("injected late cleanup failure"),
            "{start_error}"
        );

        let first_error = within_test_gate_deadline(first_stop, "the first stop to finish")
            .await
            .expect("first stop task should join")
            .expect_err("stop must surface the late cleanup failure");
        let second_error = within_test_gate_deadline(second_stop, "the concurrent stop to finish")
            .await
            .expect("second stop task should join")
            .expect_err("concurrent stop must share the late cleanup failure");
        assert!(
            first_error.contains("injected late cleanup failure"),
            "{first_error}"
        );
        assert_eq!(second_error, first_error);

        let retained_error = supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect_err("later stop callers must retain the late cleanup failure");
        assert_eq!(retained_error, first_error);
        let restart_error = supervisor
            .begin_start(true)
            .expect_err("a late cleanup failure must block restart");
        assert!(restart_error.contains("cleanup"), "{restart_error}");
        assert!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .slots
                .is_empty(),
            "late-created backend must not remain installed"
        );
    }

    #[tokio::test]
    async fn explicit_start_after_completed_stop_begins_a_new_lifecycle() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let supervisor = BackendSupervisor::new();
        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("initial lifecycle should stop");

        let config = supervisor
            .start(BackendLaunchPlan::local(
                temp.path().to_path_buf(),
                local_test_config(0),
            ))
            .await
            .expect("an explicit start after completed stop should begin a new lifecycle");
        assert!(
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, config.port))
                .await
                .is_ok(),
            "explicitly restarted backend should be reachable"
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("restarted lifecycle should stop cleanly");
    }

    #[tokio::test]
    async fn terminal_stop_rejects_an_early_late_start() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let supervisor = BackendSupervisor::new();
        supervisor
            .stop_for_exit(BackendShutdownConfig::default())
            .await
            .expect("early terminal stop should succeed");

        let error = supervisor
            .start(BackendLaunchPlan::local(
                temp.path().to_path_buf(),
                local_test_config(0),
            ))
            .await
            .expect_err("startup must not reopen after terminal shutdown");
        assert!(error.contains("terminating"), "{error}");
        assert!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .slots
                .is_empty()
        );
    }

    #[test]
    fn parses_wsl_distribution_list_with_default_marker() {
        let entries = parse_wsl_distro_entries(
            "\
  NAME            STATE           VERSION
* Ubuntu-24.04    Running         2
  Debian          Stopped         2
",
        );

        assert_eq!(
            entries,
            vec![
                WslDistroEntry {
                    name: "Ubuntu-24.04".to_string(),
                    is_default: true,
                },
                WslDistroEntry {
                    name: "Debian".to_string(),
                    is_default: false,
                },
            ],
        );
    }

    #[test]
    fn wsl_parser_ignores_headers_blank_rows_and_embedded_nuls() {
        assert_eq!(
            parse_wsl_distro_entries(
                "\0NAME STATE VERSION\0\n\n name Stopped 2\n*\0Fedora\0 Running 2\n"
            ),
            vec![WslDistroEntry {
                name: "Fedora".to_string(),
                is_default: true,
            }]
        );
        assert!(parse_wsl_distro_entries("\n\0\nNAME STATE VERSION\n").is_empty());
    }

    #[test]
    fn decodes_utf16_little_endian_wsl_output() {
        let text = "NAME\0\n*\0 Ubuntu\0\n";
        let mut bytes = vec![0xff, 0xfe];
        for value in text.encode_utf16() {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        assert_eq!(decode_wsl_command_output(&bytes), text);
    }

    #[test]
    fn decodes_plain_and_lossy_wsl_output() {
        assert_eq!(decode_wsl_command_output(b"Ubuntu\n"), "Ubuntu\n");
        assert_eq!(decode_wsl_command_output(&[b'a', 0xff]), "a\u{fffd}");

        let mut odd_utf16 = vec![0xff, 0xfe];
        odd_utf16.extend_from_slice(&u16::from(b'A').to_le_bytes());
        odd_utf16.push(0xff);
        assert_eq!(decode_wsl_command_output(&odd_utf16), "A");
    }

    #[test]
    fn normalizes_wsl_distro_names_for_command_arguments() {
        assert_eq!(
            normalize_wsl_distro(Some("  Ubuntu-24.04  ".to_string())),
            Some("Ubuntu-24.04".to_string()),
        );
        assert_eq!(normalize_wsl_distro(Some("".to_string())), None);
        assert_eq!(
            normalize_wsl_distro(Some("Ubuntu; rm -rf /".to_string())),
            None
        );
        assert_eq!(normalize_wsl_distro(None), None);
        assert_eq!(
            normalize_wsl_distro(Some("Fedora_41.test".to_string())),
            Some("Fedora_41.test".to_string())
        );
    }

    #[test]
    fn configured_wsl_distro_does_not_require_discovery() {
        let resolver = SystemWslCommandResolver;
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: true,
            wsl_only: false,
            wsl_distro: Some("Debian".to_string()),
        };

        assert_eq!(
            resolve_wsl_distro(&resolver, &settings),
            Ok("Debian".to_string())
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_wsl_resolution_covers_discovery_paths_and_command_failures() {
        let resolver = TestWslCommandResolver::new("native-wsl");

        let distros = list_wsl_distros(&resolver).expect("fixture distros should list");
        assert_eq!(
            distros,
            vec![
                WslDistroEntry {
                    name: "Ubuntu".to_string(),
                    is_default: true,
                },
                WslDistroEntry {
                    name: "Debian".to_string(),
                    is_default: false,
                },
            ]
        );
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: true,
            wsl_only: true,
            wsl_distro: None,
        };
        assert_eq!(
            resolve_wsl_distro(&resolver, &settings),
            Ok("Ubuntu".to_string())
        );
        assert_eq!(
            resolve_wsl_path(&resolver, "Ubuntu", Path::new(r"C:\bibcode")),
            Ok("/native-wsl/bibcode".to_string())
        );
        assert_eq!(
            resolve_wsl_renderer_host(&resolver, "Ubuntu"),
            Some("172.20.0.2".to_string())
        );
        assert_eq!(
            resolve_wsl_server_binary(&resolver, "Ubuntu"),
            Ok("/native-wsl/bibcode".to_string())
        );
        let plan = resolve_wsl_launch_plan_for_distro(
            &resolver,
            "Ubuntu".to_string(),
            3773,
            "token".to_string(),
            PathBuf::from("backend.log"),
            "wsl:Ubuntu".to_string(),
            "WSL Ubuntu".to_string(),
        )
        .expect("WSL launch plan should resolve");
        assert_eq!(plan.config.local_host, "172.20.0.2");
        assert_eq!(plan.log_path, Some(PathBuf::from("backend.log")));
        let primary = resolve_wsl_primary_launch_plan(
            &resolver,
            &settings,
            3774,
            "primary-token".to_string(),
            PathBuf::from("primary.log"),
        )
        .expect("primary WSL launch plan should resolve");
        assert_eq!(primary.config.environment_id, PRIMARY_LOCAL_ENVIRONMENT_ID);
        assert_eq!(resolve_wsl_renderer_host(&resolver, "Invalid"), None);
        assert!(
            resolve_wsl_path(&resolver, "Empty", Path::new(r"C:\bibcode"))
                .unwrap_err()
                .contains("returned no Linux path")
        );
        assert!(
            run_wsl_command(&resolver, "Fail", &["hostname", "-I"])
                .unwrap_err()
                .contains("exited with status")
        );

        let missing = TestWslCommandResolver::new("missing").with_missing_command();
        assert!(
            list_wsl_distros(&missing)
                .unwrap_err()
                .contains("Could not list WSL")
        );
        assert!(
            run_wsl_command(&missing, "Ubuntu", &["hostname", "-I"])
                .unwrap_err()
                .contains("Could not run wsl.exe")
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unavailable_wsl_commands_fail_through_native_resolution_helpers() {
        let resolver = SystemWslCommandResolver;
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: true,
            wsl_only: true,
            wsl_distro: Some("Missing".to_string()),
        };

        assert!(
            list_wsl_distros(&resolver)
                .unwrap_err()
                .contains("Could not list WSL")
        );
        assert!(
            resolve_wsl_path(&resolver, "Missing", Path::new("/tmp/repository"))
                .unwrap_err()
                .contains("Could not run wsl.exe")
        );
        assert_eq!(resolve_wsl_renderer_host(&resolver, "Missing"), None);
        assert!(resolve_wsl_server_binary(&resolver, "Missing").is_err());
        assert!(
            resolve_wsl_launch_plan_for_distro(
                &resolver,
                "Missing".to_string(),
                3773,
                "token".to_string(),
                PathBuf::from("backend.log"),
                "wsl:Missing".to_string(),
                "WSL Missing".to_string(),
            )
            .is_err()
        );
        assert!(
            resolve_wsl_primary_launch_plan(
                &resolver,
                &settings,
                3773,
                "token".to_string(),
                PathBuf::from("backend.log"),
            )
            .is_err()
        );
        let _ = select_desktop_backend_port();
        let _ = resolve_lan_advertised_host();
    }

    #[test]
    fn wsl_only_planning_failure_never_builds_a_windows_fallback() {
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: true,
            wsl_only: true,
            wsl_distro: Some("Unavailable".to_string()),
        };
        let native_planned = Cell::new(false);

        let error = select_default_launch_plans(
            &settings,
            || {
                native_planned.set(true);
                panic!("native Windows planning must not run in WSL-only mode")
            },
            || Err("the selected distribution cannot start".to_string()),
            || panic!("secondary WSL planning must not run in WSL-only mode"),
        )
        .expect_err("WSL-only planning must fail closed");

        assert!(matches!(
            error,
            BackendPlanError::WslPrimaryUnavailable { detail }
                if detail == "the selected distribution cannot start"
        ));
        assert!(!native_planned.get());
    }

    #[test]
    fn persisted_wsl_only_intent_never_plans_windows_when_enabled_flag_is_skewed() {
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: false,
            wsl_only: true,
            wsl_distro: Some("Ubuntu".to_string()),
        };
        let native_planned = Cell::new(false);

        let error = select_default_launch_plans(
            &settings,
            || {
                native_planned.set(true);
                panic!("persisted WSL-only intent must prevent native Windows planning")
            },
            || Err("the configured WSL primary is unavailable".to_string()),
            || panic!("secondary WSL planning must not run for WSL-only intent"),
        )
        .expect_err("skewed WSL-only settings must fail closed");

        assert!(matches!(
            error,
            BackendPlanError::WslPrimaryUnavailable { detail }
                if detail == "the configured WSL primary is unavailable"
        ));
        assert!(!native_planned.get());
    }

    #[test]
    fn secondary_wsl_planning_failure_remains_in_desired_topology() {
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: true,
            wsl_only: false,
            wsl_distro: Some("Ubuntu".to_string()),
        };

        let selection = select_default_launch_plans(
            &settings,
            || {
                Ok(BackendLaunchPlan::local(
                    PathBuf::from("C:/state"),
                    local_test_config(4_300),
                ))
            },
            || panic!("WSL primary planning must not run for Windows-primary settings"),
            || Err("the configured WSL distribution is unavailable".to_string()),
        )
        .expect("secondary planning failure must not prevent native primary planning");

        assert_eq!(selection.plans.len(), 1);
        assert_eq!(
            selection.unavailable_secondary,
            Some(BackendUnavailableEnvironment {
                environment_id: "wsl:Ubuntu".to_string(),
                label: "WSL (Ubuntu)".to_string(),
                configured_distro: Some("Ubuntu".to_string()),
                detail: "the configured WSL distribution is unavailable".to_string(),
            })
        );
    }

    #[test]
    fn default_tracking_secondary_identity_is_stable_across_live_and_unavailable_states() {
        let default_tracking = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: true,
            wsl_only: false,
            wsl_distro: None,
        };
        let explicit_ubuntu = BackendDesktopSettings {
            wsl_distro: Some("Ubuntu".to_string()),
            ..default_tracking.clone()
        };

        let (live_id, live_label) = configured_wsl_secondary_identity(&default_tracking);
        let unavailable = configured_wsl_secondary_unavailable(
            &default_tracking,
            "default distro unavailable".to_string(),
        );

        assert_eq!(live_id, "wsl:default");
        assert_eq!(live_label, "WSL (default)");
        assert_eq!(unavailable.environment_id, live_id);
        assert_eq!(unavailable.label, live_label);
        assert_eq!(unavailable.configured_distro, None);
        assert_ne!(
            configured_wsl_secondary_identity(&explicit_ubuntu).0,
            unavailable.environment_id,
            "only an explicit distro choice may replace the default-tracking identity",
        );
    }

    #[test]
    fn wsl_primary_start_failure_remains_typed_and_has_no_bootstrap() {
        let supervisor = BackendSupervisor::new();
        let plan = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: PRIMARY_LOCAL_ENVIRONMENT_ID.to_string(),
            label: "WSL (Ubuntu)".to_string(),
            running_distro: "Ubuntu".to_string(),
            port: 3773,
            renderer_host: "172.27.0.1".to_string(),
            desktop_bootstrap_token: "desktop-token".to_string(),
            binary_path: "/usr/local/bin/bibcode".to_string(),
            data_root: "/home/test/.bibcode".to_string(),
        });
        let error = classify_primary_start_error(&plan, "WSL process exited before readiness")
            .expect("WSL primary startup failures must remain typed");

        supervisor.record_plan_error_with_classification(
            &plan,
            error.to_string(),
            Some(error.clone()),
        );

        assert_eq!(
            supervisor.primary_plan_error(),
            Some(BackendPlanError::WslPrimaryUnavailable {
                detail: "WSL process exited before readiness".to_string(),
            })
        );
        assert!(supervisor.local_environment_bootstraps().is_empty());
        assert_eq!(supervisor.current_run_config(), Some(plan.config));
        assert_eq!(supervisor.project_data_targets().len(), 1);
    }

    #[test]
    fn normalizes_tailscale_ports_and_local_only_exposure() {
        assert_eq!(normalize_tailscale_serve_port(None), 443);
        assert_eq!(normalize_tailscale_serve_port(Some(0)), 443);
        assert_eq!(normalize_tailscale_serve_port(Some(1)), 1);
        assert_eq!(normalize_tailscale_serve_port(Some(65_535)), 65_535);
        assert_eq!(normalize_tailscale_serve_port(Some(65_536)), 443);

        let settings = BackendDesktopSettings {
            server_exposure_mode: "unsupported".to_string(),
            tailscale_serve_enabled: true,
            tailscale_serve_port: 8_443,
            wsl_backend_enabled: false,
            wsl_only: false,
            wsl_distro: None,
        };
        assert_eq!(
            resolve_backend_exposure(&settings, 3_773),
            ResolvedBackendExposure {
                mode: "local-only".to_string(),
                bind_host: "127.0.0.1".to_string(),
                endpoint_url: None,
                advertised_host: None,
            }
        );
    }

    #[test]
    fn desktop_settings_decode_defaults_malformed_input_and_valid_fields() {
        let defaults = decode_backend_desktop_settings(None);
        assert_eq!(
            defaults,
            BackendDesktopSettings {
                server_exposure_mode: "local-only".to_string(),
                tailscale_serve_enabled: false,
                tailscale_serve_port: 443,
                wsl_backend_enabled: false,
                wsl_only: false,
                wsl_distro: None,
            }
        );
        assert_eq!(decode_backend_desktop_settings(Some("not-json")), defaults);

        assert_eq!(
            decode_backend_desktop_settings(Some(
                r#"{
                    "serverExposureMode": "network-accessible",
                    "tailscaleServeEnabled": true,
                    "tailscaleServePort": 8443,
                    "wslBackendEnabled": true,
                    "wslOnly": true,
                    "wslDistro": "  Ubuntu-24.04  "
                }"#,
            )),
            BackendDesktopSettings {
                server_exposure_mode: "network-accessible".to_string(),
                tailscale_serve_enabled: true,
                tailscale_serve_port: 8_443,
                wsl_backend_enabled: true,
                wsl_only: true,
                wsl_distro: Some("Ubuntu-24.04".to_string()),
            }
        );

        let invalid = decode_backend_desktop_settings(Some(
            r#"{
                "serverExposureMode": "public",
                "tailscaleServePort": 70000,
                "wslDistro": "Ubuntu;rm"
            }"#,
        ));
        assert_eq!(invalid.server_exposure_mode, "local-only");
        assert_eq!(invalid.tailscale_serve_port, 443);
        assert_eq!(invalid.wsl_distro, None);
    }

    #[test]
    fn network_exposure_uses_resolved_host_or_falls_back_closed() {
        let settings = BackendDesktopSettings {
            server_exposure_mode: "network-accessible".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: false,
            wsl_only: false,
            wsl_distro: None,
        };

        assert_eq!(
            resolve_backend_exposure_with(&settings, 3_773, || Some("10.0.0.8".to_string())),
            ResolvedBackendExposure {
                mode: "network-accessible".to_string(),
                bind_host: "0.0.0.0".to_string(),
                endpoint_url: Some("http://10.0.0.8:3773".to_string()),
                advertised_host: Some("10.0.0.8".to_string()),
            }
        );
        assert_eq!(
            resolve_backend_exposure_with(&settings, 3_773, || None),
            ResolvedBackendExposure {
                mode: "local-only".to_string(),
                bind_host: "127.0.0.1".to_string(),
                endpoint_url: None,
                advertised_host: None,
            }
        );
    }

    #[test]
    fn lan_advertised_host_accepts_only_private_usable_ipv4_defaults() {
        for accepted in ["192.168.1.20", "100.100.100.100"] {
            let address = accepted.parse::<IpAddr>().expect("accepted fixture");
            assert_eq!(
                select_lan_advertised_host(Some(address)),
                Some(accepted.to_owned())
            );
        }
        for rejected in [
            "0.0.0.0",
            "127.0.0.1",
            "169.254.1.1",
            "8.8.8.8",
            "224.0.0.1",
            "255.255.255.255",
            "fd7a:115c:a1e0::1",
        ] {
            let address = rejected.parse::<IpAddr>().expect("rejected fixture");
            assert_eq!(select_lan_advertised_host(Some(address)), None);
        }
    }

    #[test]
    fn local_only_exposure_does_not_resolve_lan_route() {
        let settings = BackendDesktopSettings {
            server_exposure_mode: "local-only".to_string(),
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
            wsl_backend_enabled: false,
            wsl_only: false,
            wsl_distro: None,
        };
        let resolver_called = Cell::new(false);

        let exposure = resolve_backend_exposure_with(&settings, 3_773, || {
            resolver_called.set(true);
            Some("10.0.0.8".to_string())
        });

        assert!(!resolver_called.get());
        assert_eq!(
            exposure,
            ResolvedBackendExposure {
                mode: "local-only".to_string(),
                bind_host: "127.0.0.1".to_string(),
                endpoint_url: None,
                advertised_host: None,
            }
        );
    }

    #[test]
    fn mock_runtime_resolves_default_backend_paths_and_launch_plan() {
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let temp = tempfile::tempdir().expect("isolated desktop data root");
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(temp.path().join("data-root")))
            .build(mock_context(noop_assets()))
            .expect("mock Tauri app");
        let handle = app.handle();
        assert!(desktop_base_dir(handle).unwrap().is_absolute());
        assert!(
            primary_backend_log_path(handle)
                .unwrap()
                .ends_with(PRIMARY_BACKEND_LOG_FILE_NAME)
        );
        assert!(
            wsl_backend_log_path(handle, "Ubuntu Test")
                .unwrap()
                .ends_with("server-child-wsl-Ubuntu_Test.log")
        );
        assert!(
            !default_launch_plans(
                handle,
                &SystemBackendPortResolver,
                &SystemWslCommandResolver,
                None,
            )
            .unwrap()
            .plans
            .is_empty()
        );
    }

    #[test]
    fn exposure_override_controls_launch_planning_without_changing_persisted_settings() {
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let temp = tempfile::tempdir().expect("isolated desktop data root");
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(temp.path().join("data-root")))
            .build(mock_context(noop_assets()))
            .expect("mock Tauri app");
        let settings_directory = desktop_base_dir(app.handle())
            .expect("desktop base directory")
            .join("dev");
        fs::create_dir_all(&settings_directory).expect("settings directory");
        let settings_path = settings_directory.join(DESKTOP_SETTINGS_FILE_NAME);
        fs::write(
            &settings_path,
            r#"{"serverExposureMode":"network-accessible"}"#,
        )
        .expect("wide desktop settings");

        let local_override = default_launch_plans(
            app.handle(),
            &SystemBackendPortResolver,
            &SystemWslCommandResolver,
            Some("local-only"),
        )
        .expect("local override launch plan")
        .plans
        .into_iter()
        .find(|plan| plan.config.environment_id == PRIMARY_LOCAL_ENVIRONMENT_ID)
        .expect("primary local plan");
        assert_eq!(local_override.config.server_exposure_mode, "local-only");
        assert_eq!(local_override.config.bind_host, DESKTOP_LOOPBACK_HOST);

        fs::write(&settings_path, r#"{"serverExposureMode":"local-only"}"#)
            .expect("local desktop settings");
        let wide_override = default_launch_plans(
            app.handle(),
            &SystemBackendPortResolver,
            &SystemWslCommandResolver,
            Some("network-accessible"),
        )
        .expect("wide override launch plan")
        .plans
        .into_iter()
        .find(|plan| plan.config.environment_id == PRIMARY_LOCAL_ENVIRONMENT_ID)
        .expect("primary wide plan");
        assert_eq!(
            wide_override.config.server_exposure_mode,
            "network-accessible"
        );
        assert_eq!(wide_override.config.bind_host, DESKTOP_LAN_BIND_HOST);
    }

    #[tokio::test]
    async fn mock_runtime_starts_restarts_and_stops_the_default_backend() {
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let temp = tempfile::tempdir().expect("isolated desktop data root");
        let test_data_root = temp.path().join("data-root");
        let mut context = mock_context(noop_assets());
        context.config_mut().identifier =
            format!("com.bibcode.backend-tests-{}", std::process::id());
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(test_data_root.clone()))
            .build(context)
            .expect("mock Tauri app");
        let base_dir = desktop_base_dir(app.handle()).expect("desktop base directory");
        assert_eq!(
            base_dir,
            temp.path()
                .canonicalize()
                .expect("temporary root should canonicalize")
                .join("data-root")
        );
        let supervisor = BackendSupervisor::with_backend_port_resolver(Arc::new(
            TestBackendPortResolver::new(0),
        ));

        let started = supervisor
            .start_default(app.handle().clone())
            .await
            .expect("default backend should start");
        assert_ne!(started.port, 0);
        assert_eq!(started.server_exposure_mode, "local-only");
        assert!(
            TcpListener::bind((Ipv4Addr::LOCALHOST, started.port)).is_err(),
            "the running server must retain its published listener address"
        );

        let restarted = supervisor
            .restart_default_if_active_preserving_exposure(app.handle().clone())
            .await
            .expect("default backend should restart")
            .expect("active backend should produce a replacement config");
        assert_ne!(restarted.port, 0);
        assert_eq!(restarted.server_exposure_mode, "local-only");
        assert!(
            TcpListener::bind((Ipv4Addr::LOCALHOST, restarted.port)).is_err(),
            "the replacement server must retain its published listener address"
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("default backend should stop");
        // The shutdown is joined, but a loaded CI runner can still report the
        // address busy for a moment while the kernel tears the socket down,
        // so give the release a bounded grace period instead of one probe.
        let released = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(probe) = bind_port_probe(restarted.port, "127.0.0.1") {
                    return probe;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            released.is_ok(),
            "joined backend shutdown must release the listener address within 5s"
        );
    }

    #[tokio::test]
    async fn failed_default_backend_retains_project_data_target_and_emits_status_change() {
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let temp = tempfile::tempdir().expect("isolated desktop data root");
        let test_data_root = temp.path().join("data-root");
        let mut context = mock_context(noop_assets());
        context.config_mut().identifier = format!(
            "com.bibcode.backend-project-data-failure-tests-{}",
            std::process::id(),
        );
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(test_data_root.clone()))
            .build(context)
            .expect("mock Tauri app");

        let initial = BackendSupervisor::with_backend_port_resolver(Arc::new(
            TestBackendPortResolver::new(0),
        ));
        initial
            .start_default(app.handle().clone())
            .await
            .expect("default backend should initialize the isolated store");
        initial
            .stop(BackendShutdownConfig::default())
            .await
            .expect("default backend should close before corruption is injected");

        let marker = test_data_root.join("userdata").join("environment-id");
        fs::write(&marker, b"malformed-environment-id\n")
            .expect("environment marker fixture should write");

        let (event_sender, event_receiver) = mpsc::channel();
        app.listen_any(PROJECT_DATA_STATUS_CHANGED_EVENT, move |event| {
            event_sender
                .send(event.payload().to_string())
                .expect("project-data event payload should be captured");
        });

        let supervisor = BackendSupervisor::new();
        let error = supervisor
            .start_default(app.handle().clone())
            .await
            .expect_err("malformed marker must fail desktop backend startup closed");

        assert!(
            error.contains("storage instance marker") && error.contains("malformed"),
            "unexpected error: {error}",
        );
        assert!(supervisor.local_environment_bootstraps().is_empty());
        assert!(
            supervisor.backend_recovery().is_empty(),
            "first starts are not update recovery"
        );
        let targets = supervisor.project_data_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].environment_id, PRIMARY_LOCAL_ENVIRONMENT_ID);
        assert!(!targets[0].running);
        match &targets[0].launch_plan.target {
            BackendLaunchTarget::InProcess { base_dir, .. } => {
                assert_eq!(
                    base_dir,
                    &test_data_root
                        .canonicalize()
                        .expect("isolated data root should canonicalize"),
                );
            }
            BackendLaunchTarget::ExternalProcess { .. } => {
                panic!("native test backend should retain an in-process launch plan");
            }
        }
        let payload = event_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("project-data status invalidation should be emitted");
        assert_eq!(
            serde_json::from_str::<Value>(&payload).expect("event payload should be JSON"),
            json!({ "environmentId": PRIMARY_LOCAL_ENVIRONMENT_ID }),
        );
        assert_eq!(
            fs::read(&marker).expect("malformed marker should remain readable"),
            b"malformed-environment-id\n",
        );
    }

    #[test]
    fn writes_backend_log_chunks_with_stream_prefixes() {
        let path = std::env::temp_dir().join(format!(
            "bibcode-tauri-backend-log-{}-{}.log",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("test log should open");

            write_backend_log_chunk(&mut file, "stdout", b"ready").expect("stdout should write");
            write_backend_log_chunk(&mut file, "stderr", b"warn\n").expect("stderr should write");
        }

        let contents = fs::read_to_string(&path).expect("test log should read");
        assert!(contents.contains("[stdout] ready\n"));
        assert!(contents.contains("[stderr] warn\n"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn backend_log_open_creates_parents_and_rejects_invalid_targets() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let nested = temp.path().join("nested").join("backend.log");
        let mut file = open_backend_log_file(&nested).expect("nested log should open");
        write_backend_log_chunk(&mut file, "stdout", b"").expect("empty chunk should write");
        drop(file);
        assert_eq!(
            fs::read_to_string(&nested).expect("nested log should read"),
            "[stdout] \n"
        );

        let parent_file = temp.path().join("not-a-directory");
        fs::write(&parent_file, b"file").expect("parent fixture should write");
        assert!(open_backend_log_file(&parent_file.join("backend.log")).is_none());
        assert!(open_backend_log_file(temp.path()).is_none());
    }

    #[tokio::test]
    async fn drain_output_persists_chunks_and_stops_on_end_or_read_error() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let log_path = temp.path().join("backend.log");
        let (completion, completed) = oneshot::channel();
        drain_output(
            "stdout",
            Some(ScriptedReader {
                steps: VecDeque::from([Ok(b"first".to_vec()), Ok(b"second\n".to_vec())]),
                completion: Some(completion),
            }),
            Some(log_path.clone()),
        );
        completed.await.expect("output drain should reach EOF");
        assert_eq!(
            fs::read_to_string(&log_path).expect("drained log should read"),
            "[stdout] first\n[stdout] second\n"
        );

        let (completion, completed) = oneshot::channel();
        drain_output(
            "stderr",
            Some(ScriptedReader {
                steps: VecDeque::from([Err(io::Error::other("read failed"))]),
                completion: Some(completion),
            }),
            None,
        );
        completed
            .await
            .expect("output drain should observe read error");
    }

    #[test]
    fn drain_output_without_a_stream_is_a_noop() {
        let stream: Option<tokio::io::Empty> = None;
        drain_output("stdout", stream, None);
    }

    #[tokio::test]
    async fn wait_for_http_ready_accepts_environment_endpoint_success() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("listener should accept one request");
            let mut buffer = [0_u8; 1024];
            let count = stream.read(&mut buffer).expect("request should read");
            let request = String::from_utf8_lossy(&buffer[..count]);
            assert!(request.starts_with("GET /.well-known/bibcode/environment "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                .expect("response should write");
        });

        let readiness = BackendReadinessConfig {
            timeout: Duration::from_secs(2),
            interval: Duration::from_millis(10),
            request_timeout: Duration::from_secs(1),
        };

        wait_for_http_ready(&format!("http://127.0.0.1:{port}"), &readiness)
            .await
            .expect("environment endpoint should become ready");
    }

    #[tokio::test]
    async fn readiness_probe_accepts_http_10_and_rejects_non_success_or_malformed_status() {
        for (response, expected) in [
            (
                b"HTTP/1.0 204 No Content\r\ncontent-length: 0\r\n\r\n" as &'static [u8],
                true,
            ),
            (
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n" as &'static [u8],
                false,
            ),
            (b"not-http\r\n\r\n" as &'static [u8], false),
        ] {
            let (base_url, request, server) = spawn_http_response(response);
            assert_eq!(
                probe_http_ready(&base_url, Duration::from_secs(1)).await,
                Ok(expected)
            );
            let request = request.recv().expect("request should be captured");
            assert!(request.starts_with("GET /.well-known/bibcode/environment HTTP/1.1"));
            server.join().expect("server should finish");
        }
    }

    #[test]
    fn readiness_probe_rejects_invalid_and_unsupported_urls() {
        let invalid = probe_http_ready_blocking("not a URL", Duration::from_millis(10))
            .expect_err("invalid URL should fail");
        assert!(invalid.contains("Invalid backend URL"));

        let unsupported = probe_http_ready_blocking("https://localhost", Duration::from_millis(10))
            .expect_err("HTTPS should fail closed");
        assert_eq!(
            unsupported,
            "Unsupported backend readiness URL scheme: https"
        );
    }

    #[tokio::test]
    async fn readiness_wait_reports_the_last_non_success_status_at_deadline() {
        let (base_url, _request, server) =
            spawn_http_response(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
        let error = wait_for_http_ready(
            &base_url,
            &BackendReadinessConfig {
                timeout: Duration::ZERO,
                interval: Duration::ZERO,
                request_timeout: Duration::from_secs(1),
            },
        )
        .await
        .expect_err("non-success should miss readiness deadline");

        assert!(error.contains(&format!("{base_url}{BACKEND_READINESS_PATH}")));
        assert!(error.contains("readiness endpoint returned a non-success status"));
        server.join().expect("server should finish");
    }

    #[tokio::test]
    async fn readiness_wait_retries_after_non_success_until_endpoint_is_ready() {
        let (base_url, requests, server) = spawn_http_responses(vec![
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}",
        ]);

        wait_for_http_ready(
            &base_url,
            &BackendReadinessConfig {
                timeout: Duration::from_secs(2),
                interval: Duration::ZERO,
                request_timeout: Duration::from_secs(1),
            },
        )
        .await
        .expect("second readiness response should succeed");

        assert!(requests.recv().expect("first request").starts_with("GET "));
        assert!(requests.recv().expect("second request").starts_with("GET "));
        server.join().expect("server should finish");
    }

    #[tokio::test]
    async fn requests_soft_shutdown_endpoint_with_desktop_bootstrap_token() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("listener should accept one request");
            let mut buffer = [0_u8; 2048];
            let count = stream.read(&mut buffer).expect("request should read");
            let request = String::from_utf8_lossy(&buffer[..count]).to_string();
            sender.send(request).expect("request should be captured");
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\ncontent-length: 22\r\n\r\n{\"shuttingDown\":true}",
                )
                .expect("response should write");
        });
        let config = local_test_config(port);

        request_backend_soft_shutdown(&config, Duration::from_secs(1))
            .await
            .expect("soft shutdown should be requested");

        let request = receiver.recv().expect("request should be captured");
        assert!(request.starts_with("POST /.well-known/bibcode/desktop/shutdown HTTP/1.1"));
        assert!(request.contains("x-bibcode-desktop-bootstrap-token: desktop-token"));
    }

    #[tokio::test]
    async fn soft_shutdown_rejects_non_success_status() {
        let (base_url, request, server) =
            spawn_http_response(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n");
        let port = url::Url::parse(&base_url)
            .expect("base URL should parse")
            .port()
            .expect("base URL should have port");
        let error = request_backend_soft_shutdown(&local_test_config(port), Duration::from_secs(1))
            .await
            .expect_err("non-success shutdown should fail");

        assert_eq!(error, "Desktop backend shutdown endpoint returned 403.");
        assert!(
            request
                .recv()
                .expect("request should be captured")
                .starts_with("POST /.well-known/bibcode/desktop/shutdown HTTP/1.1")
        );
        server.join().expect("server should finish");
    }

    #[tokio::test]
    async fn managed_backend_reports_process_spawn_and_local_bind_failures() {
        let missing = BackendLaunchPlan {
            target: BackendLaunchTarget::ExternalProcess {
                program: format!("missing-bibcode-backend-{}", Uuid::new_v4().simple()),
                args: Vec::new(),
                bootstrap_line: "{}\n".to_string(),
                data_root: None,
            },
            log_path: None,
            config: local_test_config(4_300),
        };
        let error = start_managed_backend(
            missing,
            BackendReadinessConfig {
                timeout: Duration::ZERO,
                interval: Duration::ZERO,
                request_timeout: Duration::ZERO,
            },
            Arc::new(UnavailableDesktopUiProcessObserver),
            None,
            None,
            0,
            None,
        )
        .await
        .expect_err("missing executable should fail");
        assert!(matches!(error, BackendStartFailure::Other { .. }));
        assert!(
            error
                .to_string()
                .contains("Could not start desktop backend using")
        );

        let temp = tempfile::tempdir().expect("tempdir should open");
        let occupied = TcpListener::bind(("127.0.0.1", 0)).expect("port fixture should bind");
        let port = occupied.local_addr().expect("listener address").port();
        let error = start_managed_backend(
            BackendLaunchPlan::local(temp.path().to_path_buf(), local_test_config(port)),
            BackendReadinessConfig {
                timeout: Duration::ZERO,
                interval: Duration::ZERO,
                request_timeout: Duration::ZERO,
            },
            Arc::new(UnavailableDesktopUiProcessObserver),
            None,
            None,
            1,
            None,
        )
        .await
        .expect_err("occupied local port should fail");
        assert!(
            matches!(error, BackendStartFailure::PortInUse { port: actual, .. } if actual == port)
        );
        assert!(error.to_string().starts_with(&format!("Could not start in-process desktop backend: failed to bind the server listener on 127.0.0.1:{port}: ")));
    }

    #[tokio::test]
    async fn in_process_start_cleans_up_when_renderer_readiness_is_unreachable() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let mut config = local_test_config(0);
        config.local_host = "127.0.0.2".to_string();
        let error = start_managed_backend(
            BackendLaunchPlan::local(temp.path().to_path_buf(), config),
            BackendReadinessConfig {
                timeout: Duration::ZERO,
                interval: Duration::ZERO,
                request_timeout: Duration::from_millis(20),
            },
            Arc::new(UnavailableDesktopUiProcessObserver),
            None,
            None,
            8,
            None,
        )
        .await
        .expect_err("unreachable renderer address should fail readiness");

        assert!(matches!(error, BackendStartFailure::Other { .. }));
        assert!(
            error
                .to_string()
                .contains("Desktop backend did not become ready")
        );
        assert!(error.to_string().contains(BACKEND_READINESS_PATH));
    }

    #[tokio::test]
    async fn managed_runtime_stop_is_idempotent_and_waits_for_server_completion() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, _config) = start_test_server(temp.path()).await;
        let runtime = ManagedBackendRuntime::new(41, handle);
        assert_eq!(
            format!("{runtime:?}"),
            "ManagedBackendRuntime { run_id: 41, .. }"
        );
        assert!(!runtime.stop_requested.load(Ordering::SeqCst));

        runtime.request_stop();
        runtime.request_stop();
        assert!(runtime.stop_requested.load(Ordering::SeqCst));
        runtime
            .wait_for_completion()
            .await
            .expect("runtime should join cleanly");
        runtime
            .wait_for_completion()
            .await
            .expect("completed runtime result should remain available");
    }

    #[tokio::test]
    async fn a_stopped_runtime_releases_its_maintenance_owner() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, _config) = start_test_server(temp.path()).await;
        let weak = Arc::downgrade(&handle.update_maintenance().expect("in-process owner"));
        let runtime = ManagedBackendRuntime::new(44, handle);
        let runtime_probe = runtime.clone();

        runtime.request_stop();
        runtime_probe
            .wait_for_completion()
            .await
            .expect("runtime should join cleanly");
        assert!(
            weak.upgrade().is_none(),
            "retaining a stopped runtime must not keep its maintenance owner alive"
        );
        drop(runtime_probe);
    }

    #[tokio::test]
    async fn update_snapshot_carries_the_in_process_maintenance_owner_of_a_running_runtime() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, config) = start_test_server(temp.path()).await;
        let owner = handle.update_maintenance().expect("in-process owner");
        let backend = BackendSupervisor::new();
        let runtime = ManagedBackendRuntime::new(43, handle);
        let runtime_probe = runtime.clone();
        {
            let mut state = backend
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_owned(),
                BackendSlotState {
                    launch_plan: Some(BackendLaunchPlan::local(temp.path().to_path_buf(), config)),
                    backend: Some(ManagedBackend::Runtime(Box::new(runtime))),
                    ..BackendSlotState::default()
                },
            );
        }

        let snapshot = backend.snapshot_for_update();
        assert!(Arc::ptr_eq(
            &snapshot
                .in_process_maintenance(PRIMARY_LOCAL_ENVIRONMENT_ID)
                .expect("the primary's in-process owner must reach update protection"),
            &owner,
        ));
        assert!(snapshot.in_process_maintenance("wsl:Ubuntu").is_none());

        runtime_probe.request_stop();
        runtime_probe
            .wait_for_completion()
            .await
            .expect("runtime should join cleanly");
    }

    #[tokio::test]
    async fn a_wide_bound_primary_is_protected_in_process_and_exits_after_commit() {
        use crate::updates::{
            MaintenanceFinish, UpdateProtectionTransport, finish_backend_update,
            prepare_backend_for_update,
        };
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, config) = start_wide_test_server(temp.path()).await;

        let http = prepare_backend_for_update(&config, &UpdateProtectionTransport::Http, |_| {})
            .await
            .expect_err("the wide bind hides the HTTP maintenance API");
        assert_eq!(
            http,
            "Could not prepare Local for update protection: update maintenance is not available."
        );

        // Hold preparation at the real store lock until a progress poll arrives.
        let mut store_lock = Some(
            bibcode_server::persistence::StoreOperationGuard::acquire(
                temp.path(),
                tokio_util::sync::CancellationToken::new(),
                Duration::from_secs(2),
            )
            .await
            .expect("test should hold the store-operation lock"),
        );
        let transport = UpdateProtectionTransport::InProcess(
            handle.update_maintenance().expect("in-process owner"),
        );
        let mut stages = Vec::new();
        let prepared = prepare_backend_for_update(&config, &transport, |progress| {
            stages.push(progress.stage);
            store_lock.take();
        })
        .await
        .expect("in-process prepare succeeds on a wide bind");
        assert!(
            stages.iter().any(Option::is_some),
            "in-process preparation should publish a progress stage"
        );
        finish_backend_update(
            &config,
            &transport,
            MaintenanceFinish::Commit,
            &prepared.operation_id,
        )
        .await
        .expect("in-process commit succeeds");
        tokio::time::timeout(Duration::from_secs(2), handle.wait_for_shutdown())
            .await
            .expect("commit exits the backend like the HTTP path");
        handle.join().await.expect("backend joins");
    }

    #[tokio::test]
    async fn an_in_process_prepare_that_times_out_still_settles_instead_of_wedging() {
        use crate::updates::{
            MaintenanceFinish, UpdateProtectionTransport, finish_backend_update,
            prepare_backend_for_update_with_timeout,
        };
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, config) = start_test_server(temp.path()).await;
        let maintenance = handle.update_maintenance().expect("in-process owner");
        let transport = UpdateProtectionTransport::InProcess(maintenance.clone());
        let store_lock = bibcode_server::persistence::StoreOperationGuard::acquire(
            temp.path(),
            tokio_util::sync::CancellationToken::new(),
            Duration::from_secs(2),
        )
        .await
        .expect("test should hold the store-operation lock");

        let error = prepare_backend_for_update_with_timeout(
            &config,
            &transport,
            Duration::from_millis(50),
            |_| {},
        )
        .await
        .expect_err("preparation should exceed the caller's bound");
        assert_eq!(
            error,
            "Could not prepare Local for update protection: timed out after 45 seconds."
        );
        assert_eq!(maintenance.status().await["phase"], "preparing");

        drop(store_lock);
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = maintenance.status().await;
                if status["phase"] != "preparing" {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("timed-out preparation should still settle after the store lock is released");
        assert_eq!(settled["phase"], "prepared");

        finish_backend_update(
            &config,
            &transport,
            MaintenanceFinish::Cancel,
            settled["result"]["operationId"]
                .as_str()
                .expect("prepared operation should have an id"),
        )
        .await
        .expect("cancel accepts the operation that outlived its caller");
        tokio::time::timeout(Duration::from_secs(2), handle.wait_for_shutdown())
            .await
            .expect("cancel exits the backend like the HTTP path");
        handle.join().await.expect("backend joins");
    }

    #[tokio::test]
    async fn both_transports_report_the_same_text_for_a_mismatched_operation() {
        use crate::updates::{MaintenanceFinish, UpdateProtectionTransport, finish_backend_update};
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, config) = start_test_server(temp.path()).await;
        let foreign = "00000000-0000-4000-8000-000000000000";

        let over_http = finish_backend_update(
            &config,
            &UpdateProtectionTransport::Http,
            MaintenanceFinish::Cancel,
            foreign,
        )
        .await
        .expect_err("no prepared operation over HTTP");
        let in_process = finish_backend_update(
            &config,
            &UpdateProtectionTransport::InProcess(
                handle.update_maintenance().expect("in-process owner"),
            ),
            MaintenanceFinish::Cancel,
            foreign,
        )
        .await
        .expect_err("no prepared operation in process");
        assert_eq!(over_http, in_process);
        assert_eq!(
            in_process,
            "Could not cancel update protection for Local: no prepared update maintenance operation is active."
        );

        handle.shutdown();
        handle.join().await.expect("backend joins");
    }

    #[tokio::test]
    async fn an_unparsable_operation_id_reads_the_same_on_both_transports() {
        use crate::updates::{
            MaintenanceFinish, UpdateProtectionTransport, finish_backend_update,
            prepare_backend_for_update,
        };
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, config) = start_test_server(temp.path()).await;
        let transport = UpdateProtectionTransport::InProcess(
            handle.update_maintenance().expect("in-process owner"),
        );
        let prepared = prepare_backend_for_update(&config, &transport, |_| {})
            .await
            .expect("in-process preparation succeeds");
        let over_http = finish_backend_update(
            &config,
            &UpdateProtectionTransport::Http,
            MaintenanceFinish::Commit,
            "not-a-uuid",
        )
        .await
        .expect_err("HTTP rejects an unparsable operation id");
        let in_process =
            finish_backend_update(&config, &transport, MaintenanceFinish::Commit, "not-a-uuid")
                .await
                .expect_err("in-process protection rejects an unparsable operation id");
        assert_eq!(over_http, in_process);
        assert_eq!(
            in_process,
            "Could not finish update protection for Local: the update maintenance operation does not match the active operation."
        );

        finish_backend_update(
            &config,
            &transport,
            MaintenanceFinish::Cancel,
            &prepared.operation_id,
        )
        .await
        .expect("cancel accepts the real operation id");
        tokio::time::timeout(Duration::from_secs(2), handle.wait_for_shutdown())
            .await
            .expect("cancel exits the backend like the HTTP path");
        handle.join().await.expect("backend joins");
    }

    #[tokio::test]
    async fn termination_waits_for_in_process_backend_cleanup_before_exit() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, _config) = start_test_server(temp.path()).await;
        let backend = BackendSupervisor::new();
        let runtime = ManagedBackendRuntime::new(42, handle);
        let runtime_probe = runtime.clone();
        {
            let mut state = backend
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            state.slots.insert(
                PRIMARY_LOCAL_ENVIRONMENT_ID.to_owned(),
                BackendSlotState {
                    backend: Some(ManagedBackend::Runtime(Box::new(runtime))),
                    ..BackendSlotState::default()
                },
            );
        }
        let (terminate, termination) = oneshot::channel();
        let exit_requested = Arc::new(AtomicBool::new(false));
        let exit_requested_task = exit_requested.clone();
        let termination_waiting = Arc::new(FixtureEvent::default());
        let termination_waiting_checkpoint = termination_waiting.checkpoint();
        let termination_waiting_task = termination_waiting.clone();

        let shutdown = tokio::spawn(shutdown_backend_after_termination(
            backend.clone(),
            async move {
                termination_waiting_task.publish();
                let _ = termination.await;
            },
            move || exit_requested_task.store(true, Ordering::SeqCst),
        ));
        wait_for_fixture_event(
            &termination_waiting,
            termination_waiting_checkpoint,
            "termination listener",
        )
        .await;
        assert!(
            !exit_requested.load(Ordering::SeqCst),
            "exit must not be requested before termination"
        );

        terminate.send(()).expect("termination should be observed");
        shutdown.await.expect("termination task should join");

        assert!(exit_requested.load(Ordering::SeqCst));
        assert!(
            backend
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .slots
                .is_empty(),
            "backend state must be drained before exit"
        );
        assert!(
            matches!(
                runtime_probe.join_result.lock().await.as_ref(),
                Some(Ok(()))
            ),
            "in-process server must join before exit is requested"
        );
    }

    #[cfg(unix)]
    #[test]
    fn termination_signal_listener_can_be_created_without_tokio_reactor() {
        let _listener = wait_for_termination_signal();
    }

    #[tokio::test]
    async fn in_process_desktop_runtime_serves_production_rpc_domains() {
        let state = tempfile::tempdir().expect("state tempdir should open");
        let workspace = tempfile::tempdir().expect("workspace tempdir should open");
        let (handle, config) = start_rpc_test_server(state.path()).await;
        let (mut socket, _) = connect_async(format!("{}/ws", config.ws_base_url()))
            .await
            .expect("desktop runtime WebSocket should connect");

        for (id, method) in [
            "cloud.getRelayClientStatus",
            "filesystem.browse",
            "git.preparePullRequestThread",
            "git.resolvePullRequest",
            "orchestration.dispatchCommand",
            "orchestration.getArchivedShellSnapshot",
            "orchestration.getFullThreadDiff",
            "orchestration.getTurnDiff",
            "orchestration.replayEvents",
            "preview.close",
            "preview.list",
            "preview.navigate",
            "preview.open",
            "preview.refresh",
            "preview.reportStatus",
            "preview.resize",
            "previewAutomation.focusHost",
            "previewAutomation.respond",
            "projects.createEntry",
            "projects.deleteEntry",
            "projects.duplicateEntry",
            "projects.listEntries",
            "projects.readFile",
            "projects.renameEntry",
            "projects.searchEntries",
            "projects.writeFile",
            "review.getDiffPreview",
            "server.discoverSourceControl",
            "server.getConfig",
            "server.getProcessDiagnostics",
            "server.getProcessResourceHistory",
            "server.getProviderUsage",
            "server.getSettings",
            "server.getTraceDiagnostics",
            "server.removeKeybinding",
            "server.signalProcess",
            "server.updateProvider",
            "server.updateSettings",
            "server.upsertKeybinding",
            "shell.openInEditor",
            "sourceControl.cloneRepository",
            "sourceControl.lookupRepository",
            "sourceControl.publishRepository",
            "vcs.createRef",
            "vcs.discardFiles",
            "vcs.generateCommitMessage",
            "vcs.listCommits",
            "vcs.pull",
            "vcs.refreshStatus",
            "vcs.stageFiles",
            "vcs.switchRef",
            "vcs.unstageFiles",
        ]
        .into_iter()
        .enumerate()
        {
            let message = request_rpc(
                &mut socket,
                id + 1,
                method,
                json!({}),
                Duration::from_secs(45),
            )
            .await;
            assert_rpc_completed(method, &message);
        }

        let cwd = workspace.path().to_string_lossy();
        let initialized = request_rpc(
            &mut socket,
            100,
            "vcs.init",
            json!({ "cwd": cwd }),
            Duration::from_secs(45),
        )
        .await;
        assert!(
            matches!(
                &initialized,
                ServerMessage::Exit {
                    exit: RpcExit::Success { .. },
                    ..
                }
            ),
            "vcs.init should succeed, got {initialized:?}"
        );
        let refs = request_rpc(
            &mut socket,
            101,
            "vcs.listRefs",
            json!({ "cwd": cwd, "limit": 25 }),
            Duration::from_secs(45),
        )
        .await;
        assert!(matches!(
            refs,
            ServerMessage::Exit {
                exit: RpcExit::Success { .. },
                ..
            }
        ));

        let opened = request_rpc(
            &mut socket,
            102,
            "terminal.open",
            json!({
                "threadId": "desktop-runtime-smoke",
                "terminalId": "desktop-runtime-terminal",
                "cwd": cwd,
                "cols": 80,
                "rows": 24,
                "env": {}
            }),
            Duration::from_secs(45),
        )
        .await;
        assert!(matches!(
            opened,
            ServerMessage::Exit {
                exit: RpcExit::Success { .. },
                ..
            }
        ));
        let resized = request_rpc(
            &mut socket,
            103,
            "terminal.resize",
            json!({
                "threadId": "desktop-runtime-smoke",
                "terminalId": "desktop-runtime-terminal",
                "cols": 100,
                "rows": 30
            }),
            Duration::from_secs(45),
        )
        .await;
        assert!(matches!(
            resized,
            ServerMessage::Exit {
                exit: RpcExit::Success { .. },
                ..
            }
        ));
        let closed = request_rpc(
            &mut socket,
            104,
            "terminal.close",
            json!({
                "threadId": "desktop-runtime-smoke",
                "terminalId": "desktop-runtime-terminal"
            }),
            Duration::from_secs(45),
        )
        .await;
        assert!(matches!(
            closed,
            ServerMessage::Exit {
                exit: RpcExit::Success { .. },
                ..
            }
        ));

        socket.close(None).await.expect("RPC socket should close");
        handle.shutdown();
        handle.join().await.expect("desktop runtime should join");
    }

    #[tokio::test]
    async fn stop_managed_backend_shuts_down_an_in_process_runtime() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let (handle, _config) = start_test_server(temp.path()).await;
        let runtime = ManagedBackendRuntime::new(42, handle);

        stop_managed_backend(
            ManagedBackend::Runtime(Box::new(runtime)),
            BackendShutdownConfig {
                timeout: Duration::from_secs(2),
            },
        )
        .await
        .expect("managed runtime should stop");
    }

    #[tokio::test]
    async fn public_start_persists_the_os_assigned_in_process_port() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let supervisor = BackendSupervisor::new();

        let started = supervisor
            .start(BackendLaunchPlan::local(
                temp.path().to_path_buf(),
                local_test_config(0),
            ))
            .await
            .expect("ephemeral-port runtime should start");

        assert_ne!(started.port, 0);
        assert_eq!(supervisor.current_run_config(), Some(started.clone()));
        assert_eq!(
            supervisor.local_environment_bootstraps()[0]["httpBaseUrl"],
            started.http_base_url()
        );
        supervisor
            .stop(BackendShutdownConfig {
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("ephemeral-port runtime should stop");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn stop_managed_backend_accepts_an_already_exited_child() {
        let mut process = Command::new("cmd.exe")
            .args(["/D", "/C", "exit", "0"])
            .spawn()
            .expect("fixture child should start");
        process.wait().await.expect("fixture child should exit");
        let mut config = local_test_config(4_350);
        config.local_host = "invalid host".to_string();
        let child = ManagedBackendChild::new(9, config, process);
        assert_eq!(
            format!("{child:?}"),
            "ManagedBackendChild { run_id: 9, .. }"
        );

        stop_managed_backend(
            ManagedBackend::Child(Box::new(child)),
            BackendShutdownConfig {
                timeout: Duration::ZERO,
            },
        )
        .await
        .expect("already exited child should be accepted");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_managed_backend_terminates_a_live_child() {
        let process = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .expect("fixture child should start");
        let child = ManagedBackendChild::new(9, local_test_config(1), process);
        let observed_child = child.clone();
        assert_eq!(
            format!("{child:?}"),
            "ManagedBackendChild { run_id: 9, .. }"
        );

        stop_managed_backend(
            ManagedBackend::Child(Box::new(child)),
            BackendShutdownConfig {
                timeout: Duration::from_secs(2),
            },
        )
        .await
        .expect("live child should stop after soft termination");

        assert!(observed_child.stop_requested.load(Ordering::SeqCst));
        assert!(
            observed_child
                .child
                .lock()
                .await
                .try_wait()
                .expect("child status should remain inspectable")
                .is_some()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn external_backend_receives_bootstrap_serves_readiness_and_shuts_down() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let bootstrap_path = temp.path().join("bootstrap.txt");
        let shutdown_observed_path = temp.path().join("shutdown-observed.txt");
        let (
            bootstrap_event_port,
            bootstrap_observed,
            bootstrap_checkpoint,
            shutdown_release,
            bootstrap_server,
        ) = spawn_retained_fixture_connection();
        let (port, requests, server) = spawn_external_backend_http_server(shutdown_release);
        let script = format!(
            r#"
$bootstrap = [Console]::In.ReadToEnd()
[IO.File]::WriteAllText({}, $bootstrap)
$client = [Net.Sockets.TcpClient]::new('127.0.0.1', {})
$stream = $client.GetStream()
$stream.WriteByte(1)
if ($stream.ReadByte() -ne 1) {{ exit 12 }}
$client.Dispose()
[IO.File]::WriteAllText({}, 'shutdown-observed')
"#,
            powershell_string_literal(&bootstrap_path),
            bootstrap_event_port,
            powershell_string_literal(&shutdown_observed_path),
        );
        let bootstrap_line = "{\"mode\":\"desktop-test\"}\n".to_string();
        let plan = BackendLaunchPlan {
            target: BackendLaunchTarget::ExternalProcess {
                program: "powershell.exe".to_string(),
                args: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                bootstrap_line: bootstrap_line.clone(),
                data_root: None,
            },
            log_path: Some(temp.path().join("child.log")),
            config: local_test_config(port),
        };

        let (config, backend, pid) = start_managed_backend(
            plan,
            BackendReadinessConfig {
                timeout: Duration::from_secs(5),
                interval: Duration::from_millis(10),
                request_timeout: Duration::from_secs(1),
            },
            Arc::new(UnavailableDesktopUiProcessObserver),
            None,
            None,
            10,
            None,
        )
        .await
        .expect("external backend should become ready");
        assert_eq!(config.port, port);
        assert!(pid.is_some());
        assert!(matches!(backend, ManagedBackend::Child(_)));
        let readiness = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("readiness request should be captured")
            .to_ascii_lowercase();
        assert!(readiness.starts_with(&format!("get {BACKEND_READINESS_PATH} http/1.1")));

        wait_for_fixture_event(
            &bootstrap_observed,
            bootstrap_checkpoint,
            "external backend bootstrap capture",
        )
        .await;
        assert_eq!(
            fs::read_to_string(&bootstrap_path).expect("bootstrap should be captured"),
            bootstrap_line
        );

        stop_managed_backend(backend, BackendShutdownConfig::default())
            .await
            .expect("external backend should shut down gracefully");
        let shutdown = requests
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown request should be captured")
            .to_ascii_lowercase();
        assert!(shutdown.starts_with("post /.well-known/bibcode/desktop/shutdown http/1.1"));
        assert!(shutdown.contains("x-bibcode-desktop-bootstrap-token: desktop-token"));
        server.join().expect("test HTTP server should finish");
        bootstrap_server
            .join()
            .expect("bootstrap event server should finish");
        assert_eq!(
            fs::read_to_string(&shutdown_observed_path)
                .expect("child should record observing shutdown"),
            "shutdown-observed"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn child_that_ignores_soft_shutdown_is_force_killed_after_timeout() {
        let (base_url, requests, server) = spawn_http_responses(vec![
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}",
            b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\n\r\n{}",
        ]);
        let port = url::Url::parse(&base_url)
            .expect("base URL should parse")
            .port()
            .expect("base URL should have port");
        let plan = BackendLaunchPlan {
            target: BackendLaunchTarget::ExternalProcess {
                program: "powershell.exe".to_string(),
                args: vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    "Wait-Event".to_string(),
                ],
                bootstrap_line: "{}\n".to_string(),
                data_root: None,
            },
            log_path: None,
            config: local_test_config(port),
        };
        let (_config, backend, _pid) = start_managed_backend(
            plan,
            BackendReadinessConfig {
                timeout: Duration::from_secs(2),
                interval: Duration::ZERO,
                request_timeout: Duration::from_secs(1),
            },
            Arc::new(UnavailableDesktopUiProcessObserver),
            None,
            None,
            11,
            None,
        )
        .await
        .expect("independent readiness endpoint should accept child");

        stop_managed_backend(
            backend,
            // Windows process termination is asynchronous. Exercise the
            // production shutdown contract instead of coupling this lifecycle
            // assertion to a subsecond CI scheduler window.
            BackendShutdownConfig::default(),
        )
        .await
        .expect("unresponsive child should be force-killed");

        assert!(
            requests
                .recv()
                .expect("readiness request")
                .starts_with("GET ")
        );
        assert!(
            requests
                .recv()
                .expect("shutdown request")
                .starts_with("POST ")
        );
        server.join().expect("server should finish");
    }

    #[tokio::test]
    async fn soft_shutdown_reports_connection_failure() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("port fixture should bind");
        let port = listener.local_addr().expect("listener address").port();
        drop(listener);

        let error =
            request_backend_soft_shutdown(&local_test_config(port), Duration::from_millis(250))
                .await
                .expect_err("closed endpoint should reject shutdown request");
        assert!(error.contains("Could not request desktop backend shutdown"));
    }

    #[test]
    fn port_probe_reports_occupied_ports_without_a_listening_socket() {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).expect("test listener should bind");
        let port = occupied.local_addr().expect("listener address").port();

        assert!(!can_listen_on_host(port, "127.0.0.1"));
        assert!(can_listen_on_host(0, "127.0.0.1"));
        assert!(can_listen_on_host(0, "0.0.0.0"));

        let probe = bind_port_probe(0, "127.0.0.1").expect("probe socket should bind");
        let probe_port = probe
            .local_addr()
            .expect("probe address should be readable")
            .as_socket()
            .expect("probe address should be an IP socket")
            .port();
        // The retained probe owns its exact port but never listens on it.
        assert!(
            TcpStream::connect_timeout(
                &(Ipv4Addr::LOCALHOST, probe_port).into(),
                Duration::from_millis(250),
            )
            .is_err()
        );
    }

    #[test]
    fn port_selection_excludes_requested_ports_and_wsl_candidates_cover_both_architectures() {
        let selected = pick_desktop_backend_port_excluding(&[DEFAULT_BACKEND_PORT])
            .expect("an alternate backend port should be available");
        assert_ne!(selected, DEFAULT_BACKEND_PORT);
        assert_ne!(selected, 0);
        assert!(can_listen_on_host(0, "127.0.0.1"));
        assert!(!can_listen_on_host(0, "127.0.0.1\0"));

        let candidates = SystemWslCommandResolver
            .server_binary_candidates()
            .expect("candidate discovery should work");
        for suffix in [
            PathBuf::from("target/x86_64-unknown-linux-gnu/debug/bibcode"),
            PathBuf::from("target/x86_64-unknown-linux-gnu/release/bibcode"),
            PathBuf::from("target/aarch64-unknown-linux-gnu/debug/bibcode"),
            PathBuf::from("target/aarch64-unknown-linux-gnu/release/bibcode"),
        ] {
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.ends_with(&suffix)),
                "missing WSL candidate ending in {}",
                suffix.display()
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_child_soft_termination_is_not_reported_as_available() {
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/C", "exit", "0"])
            .spawn()
            .expect("fixture child should start");

        assert!(!request_child_soft_termination(&mut child));
        child.wait().await.expect("fixture child should exit");
    }

    #[tokio::test]
    async fn local_runtime_starts_without_child_process_and_clears_state_on_stop() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let port = free_test_port();
        let plan = BackendLaunchPlan::local(temp.path().to_path_buf(), local_test_config(port));
        let supervisor = BackendSupervisor::new();

        supervisor
            .start_with_options(
                plan,
                BackendReadinessConfig {
                    timeout: Duration::from_secs(2),
                    interval: Duration::from_millis(10),
                    request_timeout: Duration::from_millis(500),
                },
                BackendRestartConfig {
                    initial_delay: Duration::from_millis(20),
                    max_delay: Duration::from_millis(20),
                    monitor_interval: Duration::from_millis(10),
                },
            )
            .await
            .expect("local runtime should start");

        assert_eq!(supervisor.local_environment_bootstraps().len(), 1);

        {
            let state = supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned");
            let slot = state
                .slots
                .get(PRIMARY_LOCAL_ENVIRONMENT_ID)
                .expect("primary slot should exist");
            assert!(matches!(slot.backend, Some(ManagedBackend::Runtime(_))));
            assert_eq!(slot.pid, None);
        }

        supervisor
            .stop(BackendShutdownConfig {
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("supervisor should stop runtime");

        assert!(supervisor.local_environment_bootstraps().is_empty());
        assert!(
            supervisor
                .state
                .lock()
                .expect("backend supervisor mutex poisoned")
                .slots
                .is_empty()
        );
    }

    #[test]
    fn restart_delay_uses_exponential_backoff_with_cap() {
        let restart = BackendRestartConfig {
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(180),
            monitor_interval: Duration::from_millis(10),
        };

        assert_eq!(
            restart_delay_for_attempt(1, &restart),
            Duration::from_millis(50)
        );
        assert_eq!(
            restart_delay_for_attempt(2, &restart),
            Duration::from_millis(100)
        );
        assert_eq!(
            restart_delay_for_attempt(3, &restart),
            Duration::from_millis(180)
        );
        assert_eq!(
            restart_delay_for_attempt(8, &restart),
            Duration::from_millis(180)
        );
    }

    #[tokio::test]
    async fn restarting_local_runtime_replaces_the_previous_in_process_server() {
        let temp = tempfile::tempdir().expect("tempdir should open");
        let first_port = free_test_port();
        let second_port = loop {
            let candidate = free_test_port();
            if candidate != first_port {
                break candidate;
            }
        };
        let supervisor = BackendSupervisor::new();

        supervisor
            .start_with_options(
                BackendLaunchPlan::local(temp.path().to_path_buf(), local_test_config(first_port)),
                BackendReadinessConfig {
                    timeout: Duration::from_secs(2),
                    interval: Duration::from_millis(10),
                    request_timeout: Duration::from_millis(500),
                },
                BackendRestartConfig {
                    initial_delay: Duration::from_millis(20),
                    max_delay: Duration::from_millis(20),
                    monitor_interval: Duration::from_millis(10),
                },
            )
            .await
            .expect("first local runtime should start");

        supervisor
            .start_with_options(
                BackendLaunchPlan::local(temp.path().to_path_buf(), local_test_config(second_port)),
                BackendReadinessConfig {
                    timeout: Duration::from_secs(2),
                    interval: Duration::from_millis(10),
                    request_timeout: Duration::from_millis(500),
                },
                BackendRestartConfig {
                    initial_delay: Duration::from_millis(20),
                    max_delay: Duration::from_millis(20),
                    monitor_interval: Duration::from_millis(10),
                },
            )
            .await
            .expect("second local runtime should restart in place");

        assert_eq!(
            supervisor
                .current_run_config()
                .expect("current config should exist")
                .port,
            second_port
        );
        let first_runtime_stopped = match probe_http_ready(
            &format!("http://127.0.0.1:{first_port}"),
            Duration::from_millis(250),
        )
        .await
        {
            Ok(ready) => !ready,
            Err(_) => true,
        };
        assert!(
            first_runtime_stopped,
            "first local runtime should be shut down before replacement"
        );
        assert!(
            probe_http_ready(
                &format!("http://127.0.0.1:{second_port}"),
                Duration::from_millis(250)
            )
            .await
            .expect("replacement runtime probe should complete"),
            "replacement local runtime should answer readiness checks"
        );

        supervisor
            .stop(BackendShutdownConfig {
                timeout: Duration::from_secs(2),
            })
            .await
            .expect("supervisor should stop replacement runtime");
    }
}
