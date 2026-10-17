#![cfg_attr(not(unix), allow(dead_code))]
// Windows compiles the shared factory facade, while the Unix-socket observer
// intentionally remains Unix-only and returns pass-through there.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[cfg(unix)]
use futures_util::{SinkExt, StreamExt};
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::model::TerminalObserverGenerationLease;
#[cfg(unix)]
use super::supervisor::create_owned_generation_directory;
use super::{
    PreparedTerminalLaunch, PreparedTerminalObserver, ProviderTerminalObserverFactory,
    ProviderTerminalObserverFactoryInput, TerminalAgentActivityControl,
    TerminalAgentActivityObservation, TerminalAgentActivityObservationKind,
    TerminalAgentActivityState, TerminalAgentActivityTransition,
    TerminalGenerationActivityPublisher, TerminalObserverWorkerContext,
    supervisor::cleanup_owned_generation_directory,
};
#[cfg(not(windows))]
use crate::process::supervised::spawn_retrying_busy_executable;
#[cfg(test)]
use crate::process::supervised::{
    run_supervised_with_spawn_observer, run_supervised_with_spawn_observer_until,
};
#[cfg(unix)]
use crate::provider::codex::model::CODEX_REMOTE_MESSAGE_MAX_BYTES;
use crate::{
    activity::{ActivityCapabilities, ActivityObservationState, ProviderActivityMutation},
    diagnostics::{
        AttributionKind, AttributionScope, NativeProcessSampler, ProcessAttributionRegistry,
        ProcessRegistration, ProcessRegistrationError, ProcessRegistrationMetadata,
        RegistrationSource,
    },
    process::{
        configure_supervised_background_command_wrap,
        supervised::{SupervisedOverflow, SupervisedRunRequest, run_supervised},
    },
    provider::codex::{
        activity::{BackgroundSnapshotAuthority, CodexActivityOutput, CodexActivityTracker},
        build_initialize_params,
        model::{
            CODEX_RECONCILIATION_BACKGROUND_LIMIT, CODEX_RECONCILIATION_THREAD_LIMIT,
            ThreadBackgroundTerminalsListParams, ThreadListParams, ThreadReadParams,
            decode_background_terminals_list_response, decode_thread_list_response,
            decode_thread_read_response, is_recoverable_thread_resume_error,
        },
    },
};

const CODEX_PROBE_OUTPUT_LIMIT: usize = 128 * 1024;
const CODEX_CAPABILITY_CACHE_CAPACITY: usize = 64;
const CODEX_HELPER_READY_TIMEOUT: Duration = Duration::from_secs(3);
const CODEX_HELPER_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_HELPER_SUPERVISOR_CAPACITY: usize = 32;
const CODEX_HELPER_SUPERVISOR_INIT_TIMEOUT: Duration = Duration::from_millis(100);
const CODEX_PREPARATION_BUDGET: Duration = Duration::from_millis(350);
const CODEX_CONNECT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(3);
const CODEX_ROOT_DISCOVERY_INTERVAL: Duration = Duration::from_secs(1);
const CODEX_ROOT_DISCOVERY_LIMIT: usize = 50;
const CODEX_ROOT_DISCOVERY_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const CODEX_RESUME_MATERIALIZATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CODEX_RECONCILIATION_REQUEST_TIMEOUT: Duration = Duration::from_millis(100);
const CODEX_RECONCILIATION_PASS_TIMEOUT: Duration = Duration::from_millis(300);
const CODEX_ACTIVITY_ENABLE_BARRIER_METHOD: &str = "account/read";
const CODEX_ACTIVITY_RETRY_MIN: Duration = Duration::from_millis(50);
const CODEX_ACTIVITY_RETRY_MAX: Duration = Duration::from_secs(1);
const CODEX_ACTIVITY_REATTACH_ACK_TIMEOUT: Duration = CODEX_CONNECT_INITIALIZE_TIMEOUT
    .saturating_add(CODEX_ROOT_DISCOVERY_INTERVAL)
    .saturating_add(CODEX_ROOT_DISCOVERY_REQUEST_TIMEOUT)
    .saturating_add(CODEX_RESUME_MATERIALIZATION_TIMEOUT)
    .saturating_add(CODEX_RECONCILIATION_PASS_TIMEOUT);
const CODEX_REMOTE_PENDING_NOTIFICATION_LIMIT: usize = 256;
const CODEX_REMOTE_RPC_ERROR_PREFIX: &str = "Codex remote request failed: ";
const UNIX_SOCKET_PATH_SOFT_LIMIT: usize = 100;

#[derive(Clone, Eq, PartialEq)]
pub struct CodexProbeOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl fmt::Debug for CodexProbeOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexProbeOutput")
            .field("success", &self.success)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

pub trait CodexCapabilityProbeRunner: Send + Sync {
    fn run(
        &self,
        executable: &Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<CodexProbeOutput, String>> + Send + '_>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexCapabilities {
    pub version: String,
    pub unix_listener: bool,
    pub websocket_listener: bool,
    pub remote_tui: bool,
}

pub struct CachedCodexCapabilityProbe {
    runner: Arc<dyn CodexCapabilityProbeRunner>,
    cache: tokio::sync::Mutex<CodexCapabilityCache>,
}

impl fmt::Debug for CachedCodexCapabilityProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedCodexCapabilityProbe")
            .finish_non_exhaustive()
    }
}

impl CachedCodexCapabilityProbe {
    #[must_use]
    pub fn new(runner: Arc<dyn CodexCapabilityProbeRunner>) -> Self {
        Self {
            runner,
            cache: tokio::sync::Mutex::new(CodexCapabilityCache::default()),
        }
    }

    pub async fn probe(&self, executable: &Path) -> Option<CodexCapabilities> {
        let path = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
        let fingerprint = CodexExecutableFingerprint::read(&path);
        let mut cache = self.cache.lock().await;
        if let Some(capabilities) = fingerprint
            .as_ref()
            .and_then(|fingerprint| cache.get(fingerprint))
        {
            return Some(capabilities);
        }
        let capabilities = self.probe_uncached(&path).await;
        if let (Some(fingerprint), Some(capabilities)) = (fingerprint, &capabilities) {
            cache.insert(fingerprint, capabilities.clone());
        }
        capabilities
    }

    async fn probe_uncached(&self, executable: &Path) -> Option<CodexCapabilities> {
        let version = self
            .runner
            .run(executable, vec!["--version".to_owned()])
            .await
            .ok()?;
        let root_help = self
            .runner
            .run(executable, vec!["--help".to_owned()])
            .await
            .ok()?;
        let app_server_help = self
            .runner
            .run(
                executable,
                vec!["app-server".to_owned(), "--help".to_owned()],
            )
            .await
            .ok()?;
        if !version.success || !root_help.success || !app_server_help.success {
            return None;
        }
        let version_output = bounded_probe_text(&version.stdout, &version.stderr);
        let root_help = bounded_probe_text(&root_help.stdout, &root_help.stderr);
        let app_server_help = bounded_probe_text(&app_server_help.stdout, &app_server_help.stderr);
        let version = parse_codex_version(&version_output)?;
        Some(CodexCapabilities {
            version,
            unix_listener: app_server_help.contains("--listen")
                && app_server_help.contains("unix://"),
            websocket_listener: app_server_help.contains("--listen")
                && app_server_help.contains("ws://"),
            remote_tui: root_help.contains("--remote") && root_help.contains("unix://"),
        })
    }
}

#[derive(Debug, Default)]
struct CodexCapabilityCache {
    entries: HashMap<CodexExecutableFingerprint, CodexCapabilities>,
    recency: VecDeque<CodexExecutableFingerprint>,
}

impl CodexCapabilityCache {
    fn get(&mut self, fingerprint: &CodexExecutableFingerprint) -> Option<CodexCapabilities> {
        let capabilities = self.entries.get(fingerprint)?.clone();
        self.recency.retain(|cached| cached != fingerprint);
        self.recency.push_back(fingerprint.clone());
        Some(capabilities)
    }

    fn insert(&mut self, fingerprint: CodexExecutableFingerprint, capabilities: CodexCapabilities) {
        self.entries
            .retain(|cached, _| cached.path != fingerprint.path);
        self.recency
            .retain(|cached| cached.path != fingerprint.path);
        while self.entries.len() >= CODEX_CAPABILITY_CACHE_CAPACITY {
            let Some(oldest) = self.recency.pop_front() else {
                self.entries.clear();
                break;
            };
            self.entries.remove(&oldest);
        }
        self.entries.insert(fingerprint.clone(), capabilities);
        self.recency.push_back(fingerprint);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CodexExecutableFingerprint {
    path: PathBuf,
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl CodexExecutableFingerprint {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            path: path.to_path_buf(),
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

fn bounded_probe_text(stdout: &str, stderr: &str) -> String {
    let mut output = String::with_capacity(
        stdout
            .len()
            .saturating_add(stderr.len())
            .min(CODEX_PROBE_OUTPUT_LIMIT),
    );
    for value in [stdout, stderr] {
        let remaining = CODEX_PROBE_OUTPUT_LIMIT.saturating_sub(output.len());
        if remaining == 0 {
            break;
        }
        let boundary = value
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= remaining)
            .last()
            .unwrap_or(0);
        let end = if value.len() <= remaining {
            value.len()
        } else {
            boundary
        };
        output.push_str(&value[..end]);
        if output.len() < CODEX_PROBE_OUTPUT_LIMIT {
            output.push('\n');
        }
    }
    output
}

fn parse_codex_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|part| {
            part.chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
                && part.chars().any(|character| character == '.')
                && part
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
        })
        .map(str::to_owned)
}

#[derive(Clone)]
pub struct CodexHelperLaunch {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub endpoint: String,
    pub socket_path: PathBuf,
    pub process_attribution: ProcessAttributionRegistry,
}

impl fmt::Debug for CodexHelperLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexHelperLaunch")
            .field("executable", &self.executable)
            .field("arg_count", &self.args.len())
            .field("cwd", &self.cwd)
            .field("env_keys", &self.env.keys())
            .field("socket_path", &self.socket_path)
            .finish()
    }
}

pub trait CodexHelperProcess: Send + Sync + fmt::Debug {
    fn terminate(&self);
}

type CodexHelperStartFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Arc<dyn CodexHelperProcess>, String>> + Send + 'a>>;

pub trait CodexHelperLauncher: Send + Sync + fmt::Debug {
    fn start(&self, launch: CodexHelperLaunch) -> CodexHelperStartFuture<'_>;

    #[doc(hidden)]
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

pub trait CodexRemoteClient: Send {
    fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>>;

    fn notify(
        &mut self,
        method: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn drain_request_buffered_notifications(&mut self) -> Vec<Value>;

    fn next(&mut self) -> Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send + '_>>;
}

type CodexRemoteConnectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn CodexRemoteClient>, String>> + Send + 'a>>;

pub trait CodexRemoteClientFactory: Send + Sync + fmt::Debug {
    fn connect(&self, endpoint: &str) -> CodexRemoteConnectFuture<'_>;
}

pub struct CodexTerminalObserverFactory {
    probe: Arc<CachedCodexCapabilityProbe>,
    helper: Arc<dyn CodexHelperLauncher>,
    remote: Arc<dyn CodexRemoteClientFactory>,
    reattach_timeout: Duration,
}

impl fmt::Debug for CodexTerminalObserverFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexTerminalObserverFactory")
            .field("reattach_timeout", &self.reattach_timeout)
            .finish_non_exhaustive()
    }
}

impl CodexTerminalObserverFactory {
    #[must_use]
    pub fn new(
        probe: Arc<CachedCodexCapabilityProbe>,
        helper: Arc<dyn CodexHelperLauncher>,
        remote: Arc<dyn CodexRemoteClientFactory>,
    ) -> Self {
        Self::new_with_reattach_timeout(probe, helper, remote, CODEX_ACTIVITY_REATTACH_ACK_TIMEOUT)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn new_with_reattach_timeout(
        probe: Arc<CachedCodexCapabilityProbe>,
        helper: Arc<dyn CodexHelperLauncher>,
        remote: Arc<dyn CodexRemoteClientFactory>,
        reattach_timeout: Duration,
    ) -> Self {
        Self {
            probe,
            helper,
            remote,
            reattach_timeout,
        }
    }

    #[must_use]
    pub fn system() -> Self {
        Self::new(
            Arc::new(CachedCodexCapabilityProbe::new(Arc::new(
                SystemCodexCapabilityProbeRunner::default(),
            ))),
            Arc::new(SystemCodexHelperLauncher::default()),
            Arc::new(SystemCodexRemoteClientFactory),
        )
    }

    async fn prepare_inner(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Option<PreparedTerminalLaunch> {
        #[cfg(not(unix))]
        {
            let _ = input;
            None
        }
        #[cfg(unix)]
        {
            tokio::time::timeout(CODEX_PREPARATION_BUDGET, self.prepare_unix(input))
                .await
                .ok()
                .flatten()
        }
    }

    #[cfg(unix)]
    async fn prepare_unix(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Option<PreparedTerminalLaunch> {
        let generation_started_at = OffsetDateTime::now_utc().unix_timestamp();
        let executable = PathBuf::from(&input.launch.executable);
        let capabilities = self.probe.probe(&executable).await?;
        if !capabilities.unix_listener || !capabilities.remote_tui {
            return None;
        }
        let args = remote_tui_args(&input.launch.args)?;
        let expected_cwd = effective_tui_cwd(&args, &input.launch.cwd);
        let generation_key = input.launch.generation.id().simple().to_string();
        let generation_dir = create_owned_generation_directory(
            &input.runtime_dir,
            &format!("c{}", &generation_key[..16]),
        )
        .ok()?;
        let socket_path = generation_dir.join("s");
        if socket_path.as_os_str().as_encoded_bytes().len() > UNIX_SOCKET_PATH_SOFT_LIMIT {
            cleanup_owned_generation_directory(&input.runtime_dir, &generation_dir);
            return None;
        }
        let resources = Arc::new(CodexEndpointResources::new(
            socket_path.clone(),
            generation_dir,
            input.runtime_dir.clone(),
        ));
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());
        let helper = self
            .helper
            .start(CodexHelperLaunch {
                executable: input.launch.executable.clone(),
                args: vec![
                    "app-server".to_owned(),
                    "--listen".to_owned(),
                    endpoint.clone(),
                ],
                cwd: input.launch.cwd.clone(),
                env: input.launch.launch_env.clone(),
                endpoint: endpoint.clone(),
                socket_path,
                process_attribution: input.process_attribution.clone(),
            })
            .await
            .ok()?;
        resources.install_helper(helper);
        let mut tui_args = args;
        tui_args.push("--remote".to_owned());
        tui_args.push(endpoint.clone());
        let observer = CodexPreparedTerminalObserver {
            inner: Arc::new(CodexObserverInner {
                resources,
                endpoint,
                publisher: input.activity_publisher,
                remote: self.remote.clone(),
                provider_instance_id: input.launch.activity.provider_instance_id,
                expected_cwd,
                generation_started_at,
                spawned: AtomicBool::new(false),
                connection_epoch: AtomicU64::new(0),
                activity: Arc::new(TerminalAgentActivityControl::enabled()),
                reattach_timeout: self.reattach_timeout,
            }),
        };
        Some(PreparedTerminalLaunch {
            executable: input.launch.executable,
            args: tui_args,
            private_env: BTreeMap::new(),
            observer: Box::new(observer),
        })
    }
}

impl ProviderTerminalObserverFactory for CodexTerminalObserverFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(self.prepare_inner(input))
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.helper.shutdown()
    }
}

fn effective_tui_cwd(args: &[String], launch_cwd: &Path) -> PathBuf {
    let configured = args.windows(2).rev().find_map(|pair| {
        matches!(pair[0].as_str(), "-C" | "--cd").then(|| PathBuf::from(&pair[1]))
    });
    let configured = configured.or_else(|| {
        args.iter().rev().find_map(|argument| {
            argument
                .strip_prefix("--cd=")
                .map(PathBuf::from)
                .or_else(|| argument.strip_prefix("-C=").map(PathBuf::from))
        })
    });
    let path = configured.map_or_else(
        || launch_cwd.to_path_buf(),
        |configured| {
            if configured.is_absolute() {
                configured
            } else {
                launch_cwd.join(configured)
            }
        },
    );
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn remote_tui_args(args: &[String]) -> Option<Vec<String>> {
    const VALUE_FLAGS: &[&str] = &[
        "-c",
        "--config",
        "--enable",
        "--disable",
        "-m",
        "--model",
        "-p",
        "--profile",
        "-s",
        "--sandbox",
        "-C",
        "--cd",
        "--add-dir",
        "-a",
        "--ask-for-approval",
        "--local-provider",
    ];
    const BOOLEAN_FLAGS: &[&str] = &[
        "--oss",
        "--dangerously-bypass-approvals-and-sandbox",
        "--dangerously-bypass-hook-trust",
        "--search",
        "--no-alt-screen",
        "--strict-config",
    ];
    let mut retained = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == "--remote" || argument == "--remote-auth-token-env" {
            return None;
        }
        if BOOLEAN_FLAGS.contains(&argument.as_str()) {
            retained.push(argument.clone());
            index += 1;
            continue;
        }
        if VALUE_FLAGS.contains(&argument.as_str()) {
            let value = args.get(index + 1)?;
            if value.is_empty() {
                return None;
            }
            retained.push(argument.clone());
            retained.push(value.clone());
            index += 2;
            continue;
        }
        if VALUE_FLAGS
            .iter()
            .any(|flag| argument.starts_with(&format!("{flag}=")))
        {
            retained.push(argument.clone());
            index += 1;
            continue;
        }
        return None;
    }
    Some(retained)
}

#[derive(Debug)]
struct CodexPreparedTerminalObserver {
    inner: Arc<CodexObserverInner>,
}

impl PreparedTerminalObserver for CodexPreparedTerminalObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) {
        if self.inner.spawned.swap(true, Ordering::AcqRel) {
            return;
        }
        let inner = self.inner.clone();
        // A refused worker leaves the terminal unobserved. Its remote TUI still uses the App
        // Server, which the observer's drop stops when the terminal generation ends.
        let _ = workers.spawn(async move {
            run_codex_observer(inner, generation).await;
        });
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) -> Pin<Box<dyn Future<Output = TerminalAgentActivityTransition> + Send + '_>> {
        Box::pin(async move {
            let mut transition = self
                .inner
                .activity
                .transition_observed(enabled, self.inner.reattach_timeout)
                .await;
            transition.epochs.codex = self.inner.connection_epoch.load(Ordering::Acquire);
            transition
        })
    }

    fn agent_activity_enable_ack_timeout(&self) -> Option<Duration> {
        Some(self.inner.reattach_timeout)
    }

    fn diagnostic_label(&self) -> &str {
        "codex-remote-app-server"
    }
}

struct CodexEndpointResources {
    helper: Mutex<Option<Arc<dyn CodexHelperProcess>>>,
    socket_path: PathBuf,
    generation_dir: PathBuf,
    runtime_dir: PathBuf,
    cleaned: AtomicBool,
}

impl CodexEndpointResources {
    fn new(socket_path: PathBuf, generation_dir: PathBuf, runtime_dir: PathBuf) -> Self {
        Self {
            helper: Mutex::new(None),
            socket_path,
            generation_dir,
            runtime_dir,
            cleaned: AtomicBool::new(false),
        }
    }

    fn install_helper(&self, helper: Arc<dyn CodexHelperProcess>) {
        *self
            .helper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(helper);
    }

    fn cleanup(&self) {
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(helper) = self
            .helper
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            helper.terminate();
        }
        cleanup_owned_generation_directory(&self.runtime_dir, &self.generation_dir);
    }
}

impl fmt::Debug for CodexEndpointResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexEndpointResources")
            .field("socket_path", &self.socket_path)
            .field("generation_dir", &self.generation_dir)
            .field("cleaned", &self.cleaned.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl Drop for CodexEndpointResources {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct CodexObserverInner {
    resources: Arc<CodexEndpointResources>,
    endpoint: String,
    publisher: TerminalGenerationActivityPublisher,
    remote: Arc<dyn CodexRemoteClientFactory>,
    provider_instance_id: String,
    expected_cwd: PathBuf,
    generation_started_at: i64,
    spawned: AtomicBool,
    connection_epoch: AtomicU64,
    activity: Arc<TerminalAgentActivityControl>,
    reattach_timeout: Duration,
}

impl fmt::Debug for CodexObserverInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexObserverInner")
            .field("endpoint", &self.endpoint)
            .field("resources", &self.resources)
            .field("provider_instance_id", &self.provider_instance_id)
            .field("generation_started_at", &self.generation_started_at)
            .field("spawned", &self.spawned.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl CodexObserverInner {
    fn cleanup(&self) {
        self.resources.cleanup();
    }
}

impl Drop for CodexObserverInner {
    fn drop(&mut self) {
        self.cleanup();
    }
}

enum CodexObserverWait<T> {
    Cancelled,
    ActivityChanged(bool),
    Completed(T),
}

async fn run_codex_observer(
    inner: Arc<CodexObserverInner>,
    generation: TerminalObserverGenerationLease,
) {
    run_codex_observer_inner(&inner, &generation).await;
    inner.cleanup();
}

async fn run_codex_observer_inner(
    inner: &CodexObserverInner,
    generation: &TerminalObserverGenerationLease,
) {
    let mut activity = inner.activity.subscribe();
    let Ok(mut client) = connect_initialized_codex_client(inner).await else {
        generation.cancelled().await;
        return;
    };
    let mut epoch = 0_u64;
    inner.connection_epoch.store(epoch, Ordering::Release);
    let mut discovery_deadline = tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
    let mut discovery_state = CodexRootDiscoveryState::Ready;
    let root = loop {
        if discovery_state == CodexRootDiscoveryState::Ready
            && tokio::time::Instant::now() >= discovery_deadline
        {
            match discover_codex_tui_root(
                &mut *client,
                inner.generation_started_at,
                &inner.expected_cwd,
            )
            .await
            {
                CodexRootDiscoveryOutcome::Found(root) => break root,
                CodexRootDiscoveryOutcome::Retry => {
                    discovery_deadline =
                        tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
                }
                CodexRootDiscoveryOutcome::WaitForResponse => {
                    discovery_state = CodexRootDiscoveryState::WaitingForResponse;
                }
                CodexRootDiscoveryOutcome::Stop => {
                    discovery_state = CodexRootDiscoveryState::Disabled;
                }
            }
            continue;
        }
        let next = if discovery_state == CodexRootDiscoveryState::Ready {
            tokio::time::timeout_at(discovery_deadline, client.next()).await
        } else {
            Ok(client.next().await)
        };
        match next {
            Ok(Ok(Some(envelope))) => {
                if discovery_state == CodexRootDiscoveryState::WaitingForResponse
                    && is_codex_json_rpc_response(&envelope)
                {
                    discovery_state = CodexRootDiscoveryState::Ready;
                    discovery_deadline =
                        tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
                    continue;
                }
                if let Some(root) =
                    tui_root_thread(&envelope, inner.generation_started_at, &inner.expected_cwd)
                {
                    break root;
                }
            }
            Ok(Ok(None)) | Ok(Err(_)) => {
                drop(client);
                let Some(reconnected) = reconnect_codex_root_discovery_client(
                    inner,
                    generation,
                    &mut activity,
                    &mut epoch,
                )
                .await
                else {
                    park_codex_observer_unavailable(inner, generation, &mut activity, epoch).await;
                    return;
                };
                client = reconnected;
                discovery_state = CodexRootDiscoveryState::Ready;
                discovery_deadline = tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
            }
            Err(_) => {
                match discover_codex_tui_root(
                    &mut *client,
                    inner.generation_started_at,
                    &inner.expected_cwd,
                )
                .await
                {
                    CodexRootDiscoveryOutcome::Found(root) => break root,
                    CodexRootDiscoveryOutcome::Retry => {
                        discovery_deadline =
                            tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
                    }
                    CodexRootDiscoveryOutcome::WaitForResponse => {
                        discovery_state = CodexRootDiscoveryState::WaitingForResponse;
                    }
                    CodexRootDiscoveryOutcome::Stop => {
                        discovery_state = CodexRootDiscoveryState::Disabled;
                    }
                }
            }
        }
    };

    // Initial attachment may repair activity that happened before the observer
    // found the TUI root. Toggle recovery never decodes this response.
    let resume_deadline = tokio::time::Instant::now() + CODEX_RESUME_MATERIALIZATION_TIMEOUT;
    let resumed = loop {
        let response = tokio::select! {
            biased;
            _ = generation.cancelled() => return,
            response = tokio::time::timeout_at(
                resume_deadline,
                client.request("thread/resume", json!({"threadId": root})),
            ) => response,
        };
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) if is_codex_resume_materialization_pending(&error) => {
                let retry_at = tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
                if retry_at >= resume_deadline {
                    generation.cancelled().await;
                    return;
                }
                tokio::select! {
                    biased;
                    _ = generation.cancelled() => return,
                    _ = tokio::time::sleep_until(retry_at) => {}
                }
                continue;
            }
            Ok(Err(error)) if is_codex_resume_transport_error(&error) => {
                let Some(reconnected) =
                    reconnect_initialized_codex_client(inner, generation, resume_deadline).await
                else {
                    generation.cancelled().await;
                    return;
                };
                epoch = epoch.wrapping_add(1);
                inner.connection_epoch.store(epoch, Ordering::Release);
                client = reconnected;
                continue;
            }
            Ok(Err(_)) | Err(_) => {
                generation.cancelled().await;
                return;
            }
        };
        let Ok(resumed) = decode_thread_read_response(response) else {
            generation.cancelled().await;
            return;
        };
        if resumed.thread.id.as_deref() != Some(&root) {
            generation.cancelled().await;
            return;
        }
        break resumed;
    };

    if !inner
        .publisher
        .publish_correlated(
            "codex",
            Some(&inner.provider_instance_id),
            ActivityCapabilities::structured_full(false),
        )
        .await
        .unwrap_or(false)
    {
        return;
    }

    let mut tracker = CodexActivityTracker::new_for_terminal_observation(Some(&root));
    tracker.seed_actor(&root);
    let native_event_key_prefix = format!("codex:terminal-observation:{epoch}");
    let started_at_ms = current_unix_millis();
    let mut root_output = tracker.handle_notification(
        "thread/status/changed",
        &json!({
            "threadId": root,
            "status": {"type": "active", "activeFlags": []},
        }),
        started_at_ms,
        0,
    );
    let recovered = tracker.reconcile_thread_history(&resumed.thread);
    root_output.mutations.extend(recovered.mutations);
    root_output.request_reconciliation |= recovered.request_reconciliation;
    apply_activity_output(
        inner,
        &format!("{native_event_key_prefix}:root"),
        root_output,
    )
    .await;
    reconcile_codex_terminal(
        inner,
        &native_event_key_prefix,
        &mut *client,
        &mut tracker,
        &root,
        0,
    )
    .await;

    let initial_state = *activity.borrow_and_update();
    let mut live_state = initial_state.enabled.then_some(initial_state);
    inner
        .activity
        .mark_observed(TerminalAgentActivityObservation {
            state: initial_state,
            epoch,
            kind: if initial_state.enabled {
                TerminalAgentActivityObservationKind::Live
            } else {
                TerminalAgentActivityObservationKind::Dormant
            },
        });
    let mut receive_sequence = 1_u128;
    if initial_state.enabled {
        publish_codex_live_scope(inner, epoch, receive_sequence).await;
        receive_sequence = receive_sequence.saturating_add(1);
    }

    let mut reconciliation_sequence = 1_u128;
    let mut pending_barrier = None;
    let mut pending_reconciliation = false;
    let mut retry_at = None;
    let mut retry_delay = CODEX_ACTIVITY_RETRY_MIN;
    let mut recovering = false;

    'owner: loop {
        if recovering {
            let recovery_deadline = tokio::time::Instant::now() + inner.reattach_timeout;
            let recovery =
                reconnect_attached_codex_client(inner, generation, &root, recovery_deadline);
            tokio::pin!(recovery);
            let outcome = tokio::select! {
                biased;
                _ = generation.cancelled() => CodexObserverWait::Cancelled,
                changed = activity.changed() => {
                    CodexObserverWait::ActivityChanged(changed.is_ok())
                }
                recovered = &mut recovery => CodexObserverWait::Completed(recovered),
            };
            match outcome {
                CodexObserverWait::Cancelled => break,
                CodexObserverWait::ActivityChanged(false) => break,
                CodexObserverWait::ActivityChanged(true) => {
                    let desired = *activity.borrow_and_update();
                    inner
                        .activity
                        .mark_observed(TerminalAgentActivityObservation {
                            state: desired,
                            epoch,
                            kind: TerminalAgentActivityObservationKind::Unavailable,
                        });
                    continue;
                }
                CodexObserverWait::Completed(Some(reconnected)) => {
                    client = reconnected;
                    tracker = CodexActivityTracker::new_for_terminal_observation(Some(&root));
                    tracker.seed_actor(&root);
                    pending_reconciliation = false;
                    recovering = false;
                    retry_delay = CODEX_ACTIVITY_RETRY_MIN;
                    let desired = *activity.borrow_and_update();
                    if desired.enabled {
                        pending_barrier = Some(desired);
                    } else {
                        inner
                            .activity
                            .mark_observed(TerminalAgentActivityObservation {
                                state: desired,
                                epoch,
                                kind: TerminalAgentActivityObservationKind::Dormant,
                            });
                    }
                    continue;
                }
                CodexObserverWait::Completed(None) => {
                    let desired = inner.activity.snapshot();
                    inner
                        .activity
                        .mark_observed(TerminalAgentActivityObservation {
                            state: desired,
                            epoch,
                            kind: TerminalAgentActivityObservationKind::Unavailable,
                        });
                    let wake_at = tokio::time::Instant::now() + retry_delay;
                    retry_delay = retry_delay.saturating_mul(2).min(CODEX_ACTIVITY_RETRY_MAX);
                    tokio::select! {
                        biased;
                        _ = generation.cancelled() => break 'owner,
                        changed = activity.changed() => {
                            if changed.is_err() {
                                break 'owner;
                            }
                            let desired = *activity.borrow_and_update();
                            inner.activity.mark_observed(TerminalAgentActivityObservation {
                                state: desired,
                                epoch,
                                kind: TerminalAgentActivityObservationKind::Unavailable,
                            });
                        }
                        _ = tokio::time::sleep_until(wake_at) => {}
                    }
                    continue;
                }
            }
        }

        if let Some(desired) = pending_barrier.take() {
            let barrier_epoch = epoch;
            let deadline = tokio::time::Instant::now() + inner.reattach_timeout;
            let outcome = {
                let barrier = cross_codex_enable_barrier(&mut *client, generation, deadline);
                tokio::pin!(barrier);
                tokio::select! {
                    biased;
                    _ = generation.cancelled() => CodexObserverWait::Cancelled,
                    changed = activity.changed() => {
                        CodexObserverWait::ActivityChanged(changed.is_ok())
                    }
                    crossed = &mut barrier => CodexObserverWait::Completed(crossed),
                }
            };
            match outcome {
                CodexObserverWait::Cancelled => break,
                CodexObserverWait::ActivityChanged(false) => break,
                CodexObserverWait::ActivityChanged(true) => {
                    client.drain_request_buffered_notifications();
                    let changed = *activity.borrow_and_update();
                    live_state = None;
                    pending_reconciliation = false;
                    retry_at = None;
                    if changed.enabled {
                        pending_barrier = Some(changed);
                    } else {
                        inner
                            .activity
                            .mark_observed(TerminalAgentActivityObservation {
                                state: changed,
                                epoch,
                                kind: TerminalAgentActivityObservationKind::Dormant,
                            });
                    }
                    continue;
                }
                CodexObserverWait::Completed(crossed)
                    if crossed
                        && barrier_epoch == inner.connection_epoch.load(Ordering::Acquire)
                        && inner.activity.snapshot() == desired =>
                {
                    tracker = CodexActivityTracker::new_for_terminal_observation(Some(&root));
                    tracker.seed_actor(&root);
                    pending_reconciliation = false;
                    retry_at = None;
                    retry_delay = CODEX_ACTIVITY_RETRY_MIN;
                    live_state = Some(desired);
                    inner
                        .activity
                        .mark_observed(TerminalAgentActivityObservation {
                            state: desired,
                            epoch: barrier_epoch,
                            kind: TerminalAgentActivityObservationKind::Live,
                        });
                    publish_codex_live_scope(inner, barrier_epoch, receive_sequence).await;
                    receive_sequence = receive_sequence.saturating_add(1);
                    continue;
                }
                CodexObserverWait::Completed(_) => {
                    live_state = None;
                    pending_reconciliation = false;
                    if barrier_epoch == inner.connection_epoch.load(Ordering::Acquire)
                        && inner.activity.snapshot() == desired
                    {
                        inner
                            .activity
                            .mark_observed(TerminalAgentActivityObservation {
                                state: desired,
                                epoch: barrier_epoch,
                                kind: TerminalAgentActivityObservationKind::Unavailable,
                            });
                        retry_at = Some(tokio::time::Instant::now() + retry_delay);
                        retry_delay = retry_delay.saturating_mul(2).min(CODEX_ACTIVITY_RETRY_MAX);
                    }
                    continue;
                }
            }
        }

        if pending_reconciliation {
            let desired = inner.activity.snapshot();
            if live_state != Some(desired) {
                pending_reconciliation = false;
                continue;
            }
            let outcome = {
                let native_event_key_prefix = format!("codex:terminal-observation:{epoch}");
                let reconciliation = reconcile_codex_terminal(
                    inner,
                    &native_event_key_prefix,
                    &mut *client,
                    &mut tracker,
                    &root,
                    reconciliation_sequence,
                );
                tokio::pin!(reconciliation);
                tokio::select! {
                    biased;
                    _ = generation.cancelled() => CodexObserverWait::Cancelled,
                    changed = activity.changed() => {
                        CodexObserverWait::ActivityChanged(changed.is_ok())
                    }
                    _ = &mut reconciliation => CodexObserverWait::Completed(()),
                }
            };
            pending_reconciliation = false;
            match outcome {
                CodexObserverWait::Cancelled => break,
                CodexObserverWait::ActivityChanged(false) => break,
                CodexObserverWait::ActivityChanged(true) => {
                    client.drain_request_buffered_notifications();
                    let changed = *activity.borrow_and_update();
                    retry_at = None;
                    if !changed.enabled {
                        live_state = None;
                        pending_barrier = None;
                        inner
                            .activity
                            .mark_observed(TerminalAgentActivityObservation {
                                state: changed,
                                epoch,
                                kind: TerminalAgentActivityObservationKind::Dormant,
                            });
                    } else if live_state == Some(changed) {
                        inner
                            .activity
                            .mark_observed(TerminalAgentActivityObservation {
                                state: changed,
                                epoch,
                                kind: TerminalAgentActivityObservationKind::Live,
                            });
                    } else {
                        live_state = None;
                        pending_barrier = Some(changed);
                    }
                }
                CodexObserverWait::Completed(()) => {
                    reconciliation_sequence = reconciliation_sequence.saturating_add(1);
                }
            }
            continue;
        }

        tokio::select! {
            biased;
            _ = generation.cancelled() => break,
            changed = activity.changed() => {
                if changed.is_err() {
                    break;
                }
                let desired = *activity.borrow_and_update();
                retry_at = None;
                if !desired.enabled {
                    live_state = None;
                    pending_barrier = None;
                    pending_reconciliation = false;
                    inner.activity.mark_observed(TerminalAgentActivityObservation {
                        state: desired,
                        epoch,
                        kind: TerminalAgentActivityObservationKind::Dormant,
                    });
                } else if live_state == Some(desired) {
                    inner.activity.mark_observed(TerminalAgentActivityObservation {
                        state: desired,
                        epoch,
                        kind: TerminalAgentActivityObservationKind::Live,
                    });
                } else {
                    pending_barrier = Some(desired);
                }
            }
            _ = async {
                if let Some(wake_at) = retry_at {
                    tokio::time::sleep_until(wake_at).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                retry_at = None;
                let desired = inner.activity.snapshot();
                if desired.enabled && live_state != Some(desired) {
                    pending_barrier = Some(desired);
                }
            }
            envelope = client.next() => {
                let envelope = match envelope {
                    Ok(Some(envelope)) => envelope,
                    Ok(None) | Err(_) => {
                        epoch = epoch.wrapping_add(1);
                        inner.connection_epoch.store(epoch, Ordering::Release);
                        live_state = None;
                        pending_barrier = None;
                        pending_reconciliation = false;
                        retry_at = None;
                        recovering = true;
                        let desired = inner.activity.snapshot();
                        inner.activity.mark_observed(TerminalAgentActivityObservation {
                            state: desired,
                            epoch,
                            kind: TerminalAgentActivityObservationKind::Unavailable,
                        });
                        continue;
                    }
                };
                let desired = inner.activity.snapshot();
                if live_state != Some(desired)
                    || epoch != inner.connection_epoch.load(Ordering::Acquire)
                {
                    continue;
                }
                let Some(admission) = inner.activity.admit() else {
                    continue;
                };
                let output = tracker.handle_envelope(&envelope, receive_sequence);
                let reconcile = output.request_reconciliation;
                if !inner.activity.admission_is_current(&admission)
                    || epoch != inner.connection_epoch.load(Ordering::Acquire)
                {
                    continue;
                }
                apply_activity_output(
                    inner,
                    &format!(
                        "codex:terminal-observation:{epoch}:live:{receive_sequence}"
                    ),
                    output,
                )
                .await;
                receive_sequence = receive_sequence.saturating_add(1);
                pending_reconciliation |= reconcile
                    && inner.activity.admission_is_current(&admission)
                    && epoch == inner.connection_epoch.load(Ordering::Acquire);
            }
        }
    }
}

async fn publish_codex_live_scope(inner: &CodexObserverInner, epoch: u64, receive_sequence: u128) {
    let mut enabled = CodexActivityOutput::default();
    enabled.mutations.push(ProviderActivityMutation::SetScope {
        capabilities: ActivityCapabilities::structured_full(true),
        observation_state: ActivityObservationState::Live,
    });
    apply_activity_output(
        inner,
        &format!("codex:terminal-observation:{epoch}:live:{receive_sequence}"),
        enabled,
    )
    .await;
}

async fn cross_codex_enable_barrier(
    client: &mut dyn CodexRemoteClient,
    generation: &TerminalObserverGenerationLease,
    deadline: tokio::time::Instant,
) -> bool {
    let response = tokio::select! {
        biased;
        _ = generation.cancelled() => return false,
        response = tokio::time::timeout_at(
            deadline,
            client.request(
                CODEX_ACTIVITY_ENABLE_BARRIER_METHOD,
                json!({"refreshToken": false}),
            ),
        ) => response,
    };
    client.drain_request_buffered_notifications();
    matches!(response, Ok(Ok(_)))
}

async fn reconnect_attached_codex_client(
    inner: &CodexObserverInner,
    generation: &TerminalObserverGenerationLease,
    root: &str,
    deadline: tokio::time::Instant,
) -> Option<Box<dyn CodexRemoteClient>> {
    let mut client = reconnect_initialized_codex_client(inner, generation, deadline).await?;
    let response = tokio::select! {
        biased;
        _ = generation.cancelled() => return None,
        response = tokio::time::timeout_at(
            deadline,
            client.request("thread/resume", json!({"threadId": root})),
        ) => response,
    };
    matches!(response, Ok(Ok(_))).then_some(client)
}

async fn reconnect_codex_root_discovery_client(
    inner: &CodexObserverInner,
    generation: &TerminalObserverGenerationLease,
    activity: &mut tokio::sync::watch::Receiver<TerminalAgentActivityState>,
    epoch: &mut u64,
) -> Option<Box<dyn CodexRemoteClient>> {
    *epoch = epoch.wrapping_add(1);
    inner.connection_epoch.store(*epoch, Ordering::Release);
    let desired = inner.activity.snapshot();
    inner
        .activity
        .mark_observed(TerminalAgentActivityObservation {
            state: desired,
            epoch: *epoch,
            kind: TerminalAgentActivityObservationKind::Unavailable,
        });
    let reconnect = reconnect_initialized_codex_client(
        inner,
        generation,
        tokio::time::Instant::now() + inner.reattach_timeout,
    );
    tokio::pin!(reconnect);
    loop {
        tokio::select! {
            biased;
            _ = generation.cancelled() => return None,
            changed = activity.changed() => {
                if changed.is_err() {
                    return None;
                }
                let desired = *activity.borrow_and_update();
                inner.activity.mark_observed(TerminalAgentActivityObservation {
                    state: desired,
                    epoch: *epoch,
                    kind: TerminalAgentActivityObservationKind::Unavailable,
                });
            }
            reconnected = &mut reconnect => return reconnected,
        }
    }
}

async fn park_codex_observer_unavailable(
    inner: &CodexObserverInner,
    generation: &TerminalObserverGenerationLease,
    activity: &mut tokio::sync::watch::Receiver<TerminalAgentActivityState>,
    epoch: u64,
) {
    loop {
        tokio::select! {
            biased;
            _ = generation.cancelled() => return,
            changed = activity.changed() => {
                if changed.is_err() {
                    return;
                }
                let desired = *activity.borrow_and_update();
                inner.activity.mark_observed(TerminalAgentActivityObservation {
                    state: desired,
                    epoch,
                    kind: TerminalAgentActivityObservationKind::Unavailable,
                });
            }
        }
    }
}

async fn connect_initialized_codex_client(
    inner: &CodexObserverInner,
) -> Result<Box<dyn CodexRemoteClient>, String> {
    let mut client = inner.remote.connect(&inner.endpoint).await?;
    client
        .request(
            "initialize",
            build_initialize_params(env!("CARGO_PKG_VERSION")),
        )
        .await?;
    client.notify("initialized").await?;
    Ok(client)
}

async fn reconnect_initialized_codex_client(
    inner: &CodexObserverInner,
    generation: &TerminalObserverGenerationLease,
    deadline: tokio::time::Instant,
) -> Option<Box<dyn CodexRemoteClient>> {
    loop {
        let connected = tokio::select! {
            biased;
            _ = generation.cancelled() => return None,
            connected = tokio::time::timeout_at(deadline, connect_initialized_codex_client(inner)) => connected,
        };
        if let Ok(Ok(client)) = connected {
            return Some(client);
        }
        let retry_at = tokio::time::Instant::now() + CODEX_ROOT_DISCOVERY_INTERVAL;
        if retry_at >= deadline {
            return None;
        }
        tokio::select! {
            biased;
            _ = generation.cancelled() => return None,
            _ = tokio::time::sleep_until(retry_at) => {}
        }
    }
}

fn tui_root_thread(
    envelope: &Value,
    generation_started_at: i64,
    expected_cwd: &Path,
) -> Option<String> {
    let method = envelope.get("method").and_then(Value::as_str)?;
    if method != "thread/started" {
        return None;
    }
    tui_root_candidate(
        envelope.pointer("/params/thread")?,
        generation_started_at,
        expected_cwd,
    )
}

fn tui_root_candidate(
    thread: &Value,
    generation_started_at: i64,
    expected_cwd: &Path,
) -> Option<String> {
    let thread = thread.as_object()?;
    let created_at = thread.get("createdAt")?.as_i64()?;
    if created_at < generation_started_at {
        return None;
    }
    let thread_id = thread
        .get("id")
        .and_then(Value::as_str)
        .filter(|thread_id| !thread_id.trim().is_empty())?;
    if thread.get("sessionId").and_then(Value::as_str) != Some(thread_id)
        || !thread.get("forkedFromId").is_some_and(Value::is_null)
        || !thread.get("parentThreadId").is_some_and(Value::is_null)
        || !matches!(
            thread.get("source").and_then(Value::as_str),
            Some("cli" | "vscode")
        )
        || !thread
            .get("threadSource")
            .is_some_and(|source| source.is_null() || source.as_str() == Some("user"))
    {
        return None;
    }
    let cwd = Path::new(thread.get("cwd")?.as_str()?);
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    (cwd == expected_cwd).then(|| thread_id.to_owned())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CodexRootDiscoveryState {
    Ready,
    WaitingForResponse,
    Disabled,
}

enum CodexRootDiscoveryOutcome {
    Found(String),
    Retry,
    WaitForResponse,
    Stop,
}

fn is_codex_json_rpc_response(value: &Value) -> bool {
    value.get("id").is_some()
        && value.get("method").is_none()
        && (value.get("result").is_some() || value.get("error").is_some())
}

fn is_codex_resume_materialization_pending(error: &str) -> bool {
    let Some(payload) = error.strip_prefix(CODEX_REMOTE_RPC_ERROR_PREFIX) else {
        return false;
    };
    let Ok(error) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    if error.get("code").and_then(Value::as_i64) != Some(-32600) {
        return false;
    }
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return false;
    };
    is_recoverable_thread_resume_error(message)
}

fn is_codex_resume_transport_error(error: &str) -> bool {
    !error.starts_with(CODEX_REMOTE_RPC_ERROR_PREFIX)
}

async fn discover_codex_tui_root(
    client: &mut dyn CodexRemoteClient,
    generation_started_at: i64,
    expected_cwd: &Path,
) -> CodexRootDiscoveryOutcome {
    let response = match tokio::time::timeout(
        CODEX_ROOT_DISCOVERY_REQUEST_TIMEOUT,
        client.request(
            "thread/list",
            json!({
                "limit": CODEX_ROOT_DISCOVERY_LIMIT,
                "sortKey": "created_at",
                "sortDirection": "desc",
                "sourceKinds": ["cli", "vscode"],
                "cwd": expected_cwd.to_string_lossy(),
                "useStateDbOnly": true,
            }),
        ),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) if error.starts_with("Codex remote request failed:") => {
            return CodexRootDiscoveryOutcome::Retry;
        }
        Ok(Err(_)) => return CodexRootDiscoveryOutcome::Stop,
        Err(_) => return CodexRootDiscoveryOutcome::WaitForResponse,
    };
    let Ok(decoded) = decode_thread_list_response(response.clone()) else {
        return CodexRootDiscoveryOutcome::Stop;
    };
    if decoded.next_cursor.is_some() {
        return CodexRootDiscoveryOutcome::Retry;
    }
    let Some(candidates) = response.get("data").and_then(Value::as_array) else {
        return CodexRootDiscoveryOutcome::Stop;
    };
    let mut matching = candidates
        .iter()
        .take(CODEX_ROOT_DISCOVERY_LIMIT)
        .filter_map(|thread| tui_root_candidate(thread, generation_started_at, expected_cwd));
    let Some(root) = matching.next() else {
        return CodexRootDiscoveryOutcome::Retry;
    };
    if matching.next().is_some() {
        CodexRootDiscoveryOutcome::Retry
    } else {
        CodexRootDiscoveryOutcome::Found(root)
    }
}

async fn apply_activity_output(
    inner: &CodexObserverInner,
    native_event_key: &str,
    output: CodexActivityOutput,
) {
    if output.mutations.is_empty() {
        return;
    }
    let _ = inner
        .publisher
        .apply(native_event_key, output.mutations, &current_timestamp())
        .await;
}

async fn reconcile_codex_terminal(
    inner: &CodexObserverInner,
    native_event_key_prefix: &str,
    client: &mut dyn CodexRemoteClient,
    tracker: &mut CodexActivityTracker,
    root: &str,
    reconciliation_sequence: u128,
) {
    let _ = tokio::time::timeout(
        CODEX_RECONCILIATION_PASS_TIMEOUT,
        reconcile_codex_terminal_inner(
            inner,
            native_event_key_prefix,
            client,
            tracker,
            root,
            reconciliation_sequence,
        ),
    )
    .await;
}

async fn bounded_codex_reconciliation_request(
    client: &mut dyn CodexRemoteClient,
    method: &str,
    params: Value,
) -> Result<Value, ()> {
    tokio::time::timeout(
        CODEX_RECONCILIATION_REQUEST_TIMEOUT,
        client.request(method, params),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

async fn reconcile_codex_terminal_inner(
    inner: &CodexObserverInner,
    native_event_key_prefix: &str,
    client: &mut dyn CodexRemoteClient,
    tracker: &mut CodexActivityTracker,
    root: &str,
    reconciliation_sequence: u128,
) -> Result<(), ()> {
    // Background processes are an independent snapshot authority. Request
    // them first so a stalled descendant endpoint cannot starve their update.
    let background = bounded_codex_reconciliation_request(
        client,
        "thread/backgroundTerminals/list",
        serde_json::to_value(ThreadBackgroundTerminalsListParams {
            thread_id: root,
            limit: u16::try_from(CODEX_RECONCILIATION_BACKGROUND_LIMIT)
                .expect("Codex background reconciliation limit fits u16"),
            cursor: None,
        })
        .expect("Codex terminal background-terminal params serialize"),
    )
    .await
    .and_then(|value| decode_background_terminals_list_response(value).map_err(|_| ()));
    let background = background.ok();
    if let Some(background) = background {
        let output = tracker.reconcile_background_terminals(
            &background.data,
            &current_timestamp(),
            BackgroundSnapshotAuthority::Complete,
        );
        apply_activity_output(
            inner,
            &format!("{native_event_key_prefix}:reconcile:{reconciliation_sequence}:background"),
            output,
        )
        .await;
    }

    let list = bounded_codex_reconciliation_request(
        client,
        "thread/list",
        serde_json::to_value(ThreadListParams {
            ancestor_thread_id: root,
            limit: u16::try_from(CODEX_RECONCILIATION_THREAD_LIMIT)
                .expect("Codex descendant reconciliation limit fits u16"),
            cursor: None,
        })
        .expect("Codex terminal thread/list params serialize"),
    )
    .await
    .and_then(|value| decode_thread_list_response(value).map_err(|_| ()));
    let list = list.ok();
    if let Some(list) = list {
        let descendants = tracker.reconcile_descendants(&list.data);
        apply_activity_output(
            inner,
            &format!("{native_event_key_prefix}:reconcile:{reconciliation_sequence}:descendants"),
            descendants.output,
        )
        .await;
        for thread_id in descendants.threads_to_read {
            let thread = bounded_codex_reconciliation_request(
                client,
                "thread/read",
                serde_json::to_value(ThreadReadParams {
                    thread_id: &thread_id,
                    include_turns: true,
                })
                .expect("Codex terminal thread/read params serialize"),
            )
            .await
            .and_then(|value| decode_thread_read_response(value).map_err(|_| ()));
            let thread = thread
                .ok()
                .filter(|thread| thread.thread.id.as_deref() == Some(thread_id.as_str()));
            if let Some(thread) = thread {
                let output = tracker.reconcile_thread_history(&thread.thread);
                apply_activity_output(
                    inner,
                    &format!(
                        "{native_event_key_prefix}:reconcile:{reconciliation_sequence}:thread:{thread_id}"
                    ),
                    output,
                )
                .await;
            }
        }
    }
    Ok(())
}

fn current_unix_millis() -> u64 {
    u64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or_default()
}

fn current_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[derive(Debug)]
struct SystemCodexCapabilityProbeRunner {
    timeout: Duration,
    cleanup_timeout: Duration,
    #[cfg(test)]
    integration_deadline: Option<tokio::time::Instant>,
    #[cfg(test)]
    fixture_spawned: Option<CodexProbeSpawnFixture>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct CodexProbeSpawnFixture {
    spawned: Arc<crate::test_support::FixtureEvent>,
    pid_path: PathBuf,
}

#[cfg(test)]
impl CodexProbeSpawnFixture {
    fn publish(&self, process_id: Option<u32>) {
        let process_id = process_id.expect("Codex probe PID is available after spawn");
        let mut temporary_name = self.pid_path.as_os_str().to_owned();
        temporary_name.push(".tmp");
        let temporary_path = PathBuf::from(temporary_name);
        std::fs::write(&temporary_path, process_id.to_string())
            .expect("stage Codex probe PID after spawn");
        std::fs::rename(&temporary_path, &self.pid_path)
            .expect("publish Codex probe PID after spawn");
        self.spawned.publish();
    }
}

impl Default for SystemCodexCapabilityProbeRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            cleanup_timeout: Duration::from_secs(2),
            #[cfg(test)]
            integration_deadline: None,
            #[cfg(test)]
            fixture_spawned: None,
        }
    }
}

impl SystemCodexCapabilityProbeRunner {
    #[cfg(test)]
    const fn with_integration_deadline(
        deadline: tokio::time::Instant,
        cleanup_timeout: Duration,
    ) -> Self {
        Self {
            timeout: Duration::from_secs(3),
            cleanup_timeout,
            integration_deadline: Some(deadline),
            fixture_spawned: None,
        }
    }

    #[cfg(test)]
    fn with_spawn_fixture(mut self, fixture_spawned: CodexProbeSpawnFixture) -> Self {
        self.fixture_spawned = Some(fixture_spawned);
        self
    }
}

impl CodexCapabilityProbeRunner for SystemCodexCapabilityProbeRunner {
    fn run(
        &self,
        executable: &Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<CodexProbeOutput, String>> + Send + '_>> {
        let executable = executable.to_path_buf();
        Box::pin(async move {
            #[cfg(feature = "hermetic-test-guard")]
            let executable = crate::hermetic_guard::checked_provider_executable(
                &executable,
                std::env::current_dir().ok().as_deref(),
                std::env::var_os("PATH").as_deref(),
            )
            .ok_or_else(|| {
                "probe executable was not found or was refused by the hermetic test guard"
                    .to_owned()
            })?;
            let mut command = tokio::process::Command::new(executable);
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            crate::provider::environment::sanitize_provider_subprocess_environment(&mut command);
            let request = SupervisedRunRequest {
                command,
                stdin: None,
                timeout: self.timeout,
                cleanup_timeout: self.cleanup_timeout,
                max_output_bytes: CODEX_PROBE_OUTPUT_LIMIT,
                overflow: SupervisedOverflow::Truncate,
            };
            let cancellation = tokio_util::sync::CancellationToken::new();
            #[cfg(test)]
            let output = if let Some(fixture_spawned) = self.fixture_spawned.clone() {
                match self.integration_deadline {
                    Some(deadline) => {
                        run_supervised_with_spawn_observer_until(
                            request,
                            &cancellation,
                            deadline,
                            move |process_id| fixture_spawned.publish(process_id),
                        )
                        .await
                    }
                    None => {
                        run_supervised_with_spawn_observer(
                            request,
                            &cancellation,
                            move |process_id| fixture_spawned.publish(process_id),
                        )
                        .await
                    }
                }
            } else {
                run_supervised(request, &cancellation).await
            };
            #[cfg(not(test))]
            let output = run_supervised(request, &cancellation).await;
            let output =
                output.map_err(|error| format!("Codex capability probe failed: {error:?}"))?;
            Ok(CodexProbeOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
            })
        })
    }
}

#[derive(Debug)]
struct SystemCodexHelperLauncher {
    readiness_timeout: Duration,
    owned_tasks: Arc<CodexHelperTaskRegistry>,
    #[cfg(test)]
    integration_deadline: Option<tokio::time::Instant>,
    #[cfg(test)]
    fixture_events: Option<CodexHelperFixtureEvents>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct CodexHelperFixtureEvents {
    spawned: Arc<crate::test_support::FixtureEvent>,
    admission_race: Option<Arc<CodexHelperAdmissionRace>>,
    ready: Arc<crate::test_support::FixtureEvent>,
    reaped: Arc<crate::test_support::FixtureEvent>,
    pid_path: PathBuf,
    watchdog_spawn_race: Option<Arc<CodexHelperWatchdogSpawnRace>>,
}

#[cfg(test)]
#[derive(Debug)]
struct CodexHelperAdmissionRace {
    spawned: crate::test_support::FixtureEvent,
    release: crate::test_support::FixtureEvent,
}

#[cfg(test)]
#[derive(Debug)]
struct CodexHelperWatchdogSpawnRace {
    release_publication: std::sync::Barrier,
    publication_complete: std::sync::Barrier,
    cleanup_started: crate::test_support::FixtureEvent,
    cleanup_release: crate::test_support::FixtureEvent,
    owner_joined: crate::test_support::FixtureEvent,
}

#[cfg(test)]
impl CodexHelperWatchdogSpawnRace {
    fn new() -> Self {
        Self {
            release_publication: std::sync::Barrier::new(2),
            publication_complete: std::sync::Barrier::new(2),
            cleanup_started: crate::test_support::FixtureEvent::default(),
            cleanup_release: crate::test_support::FixtureEvent::default(),
            owner_joined: crate::test_support::FixtureEvent::default(),
        }
    }
}

#[cfg(test)]
impl CodexHelperFixtureEvents {
    fn publish_pid(&self, process_id: Option<u32>) -> Result<(), String> {
        let process_id = process_id.ok_or_else(|| "Codex helper PID is unavailable".to_owned())?;
        let mut temporary_name = self.pid_path.as_os_str().to_owned();
        temporary_name.push(".tmp");
        let temporary_path = PathBuf::from(temporary_name);
        std::fs::write(&temporary_path, process_id.to_string())
            .map_err(|error| format!("failed staging Codex helper PID: {error}"))?;
        std::fs::rename(&temporary_path, &self.pid_path)
            .map_err(|error| format!("failed publishing Codex helper PID: {error}"))?;
        Ok(())
    }

    fn publish_spawned(&self, process_id: Option<u32>) -> Result<(), String> {
        if let Some(race) = self.watchdog_spawn_race.as_ref() {
            race.release_publication.wait();
        }
        self.publish_pid(process_id)?;
        self.spawned.publish();
        if let Some(race) = self.watchdog_spawn_race.as_ref() {
            race.publication_complete.wait();
        }
        Ok(())
    }
}

impl Default for SystemCodexHelperLauncher {
    fn default() -> Self {
        Self {
            readiness_timeout: CODEX_HELPER_READY_TIMEOUT,
            owned_tasks: Arc::new(CodexHelperTaskRegistry::default()),
            #[cfg(test)]
            integration_deadline: None,
            #[cfg(test)]
            fixture_events: None,
        }
    }
}

impl SystemCodexHelperLauncher {
    #[cfg(test)]
    fn with_fixture_events(
        readiness_timeout: Duration,
        fixture_events: CodexHelperFixtureEvents,
    ) -> Self {
        Self {
            readiness_timeout,
            owned_tasks: Arc::new(CodexHelperTaskRegistry::default()),
            integration_deadline: None,
            fixture_events: Some(fixture_events),
        }
    }

    #[cfg(test)]
    fn with_integration_fixture_events(
        deadline: tokio::time::Instant,
        fixture_events: CodexHelperFixtureEvents,
    ) -> Self {
        Self {
            readiness_timeout: CODEX_HELPER_READY_TIMEOUT,
            owned_tasks: Arc::new(CodexHelperTaskRegistry::default()),
            integration_deadline: Some(deadline),
            fixture_events: Some(fixture_events),
        }
    }
}

impl CodexHelperLauncher for SystemCodexHelperLauncher {
    fn start(&self, launch: CodexHelperLaunch) -> CodexHelperStartFuture<'_> {
        Box::pin(async move {
            #[cfg(feature = "hermetic-test-guard")]
            let launch = {
                let mut launch = launch;
                let search_path =
                    crate::production::provider_runtime::effective_provider_search_path(
                        launch.env.iter().map(|(name, value)| {
                            (std::ffi::OsStr::new(name), std::ffi::OsStr::new(value))
                        }),
                    );
                let executable = crate::hermetic_guard::checked_provider_executable(
                    Path::new(&launch.executable),
                    Some(&launch.cwd),
                    search_path.as_deref(),
                )
                .ok_or_else(|| {
                    "failed to start Codex App Server helper: executable was not found or was refused by the hermetic test guard"
                        .to_owned()
                })?;
                launch.executable = executable.to_string_lossy().into_owned();
                launch
            };
            let supervisor = SYSTEM_CODEX_HELPER_SUPERVISOR
                .wait_ready()
                .await
                .ok_or_else(|| "Codex helper supervisor is unavailable".to_owned())?;
            let permit = supervisor
                .reserve()
                .ok_or_else(|| "Codex helper supervisor is at capacity".to_owned())?;
            let owned_task = self.owned_tasks.register();
            let mut command = CommandWrap::with_new(&launch.executable, |command| {
                command
                    .args(&launch.args)
                    .current_dir(&launch.cwd)
                    .envs(&launch.env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                crate::provider::environment::sanitize_provider_subprocess_environment(command);
            });
            configure_supervised_background_command_wrap(&mut command);
            #[cfg(not(windows))]
            let child = spawn_retrying_busy_executable(
                &mut command,
                tokio::time::Instant::now() + self.readiness_timeout,
            )
            .await
            .map_err(|error| format!("failed to start Codex App Server helper: {error}"))?;
            #[cfg(windows)]
            let child = command
                .spawn()
                .map_err(|error| format!("failed to start Codex App Server helper: {error}"))?;
            #[cfg(not(unix))]
            let mut child = child;
            #[cfg(test)]
            if let Some(events) = self.fixture_events.as_ref()
                && let Some(race) = events.admission_race.as_ref()
            {
                if let Err(error) = events.publish_pid(child.id()) {
                    terminate_and_reap_codex_helper(child).await;
                    return Err(error);
                }
                race.spawned.publish();
                race.release.wait_after(0).await;
            }
            let identity = child
                .id()
                .and_then(|pid| NativeProcessSampler::process_identity(pid).ok());
            let Some(identity) = identity else {
                terminate_and_reap_codex_helper(child).await;
                return Err("spawned Codex helper has no stable process identity".to_owned());
            };
            #[cfg(unix)]
            let liveness = codex_helper_root_exited_without_reaping(identity.pid)
                .map(|exited| exited.then_some(()));
            #[cfg(not(unix))]
            let liveness = child.try_wait();
            match liveness {
                Ok(None) => {}
                Ok(Some(_)) => {
                    terminate_and_reap_codex_helper(child).await;
                    return Err("Codex helper exited before ownership admission".to_owned());
                }
                Err(error) => {
                    terminate_and_reap_codex_helper(child).await;
                    return Err(format!(
                        "failed to revalidate Codex helper process identity: {error}"
                    ));
                }
            }
            let registration = match launch.process_attribution.register_identity(
                identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Helper,
                    label: "Codex App Server helper".to_owned(),
                    source: RegistrationSource::Helper,
                    driver: None,
                },
            ) {
                Ok(registration) => registration,
                Err(
                    error @ (ProcessRegistrationError::Shutdown
                    | ProcessRegistrationError::Capacity),
                ) => {
                    terminate_and_reap_codex_helper(child).await;
                    return Err(error.to_string());
                }
            };
            #[cfg(test)]
            if let Some(events) = self.fixture_events.as_ref()
                && let Err(error) = events.publish_spawned(child.id())
            {
                terminate_and_reap_codex_helper(child).await;
                return Err(error);
            }
            let (process, ready) = supervisor.supervise(
                child,
                launch.socket_path,
                CodexHelperSupervisionOwnership {
                    registration: Some(registration),
                    permit,
                    task: owned_task,
                },
                self.readiness_timeout,
                #[cfg(test)]
                CodexHelperSupervisionFixture {
                    integration_deadline: self.integration_deadline,
                    reaped: self
                        .fixture_events
                        .as_ref()
                        .map(|events| events.reaped.clone()),
                    watchdog_spawn_race: self
                        .fixture_events
                        .as_ref()
                        .and_then(|events| events.watchdog_spawn_race.clone()),
                },
            );
            ready
                .await
                .map_err(|_| "Codex helper supervisor stopped before readiness".to_owned())??;
            #[cfg(test)]
            if let Some(events) = self.fixture_events.as_ref() {
                events.ready.publish();
            }
            Ok(Arc::new(process) as Arc<dyn CodexHelperProcess>)
        })
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.owned_tasks.drain())
    }
}

#[derive(Debug, Default)]
struct CodexHelperTaskRegistry {
    active: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
}

impl CodexHelperTaskRegistry {
    fn register(self: &Arc<Self>) -> CodexHelperTaskGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        CodexHelperTaskGuard {
            registry: Arc::clone(self),
        }
    }

    async fn drain(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Debug)]
struct CodexHelperTaskGuard {
    registry: Arc<CodexHelperTaskRegistry>,
}

#[derive(Debug)]
struct CodexHelperSupervisionOwnership {
    registration: Option<ProcessRegistration>,
    permit: tokio::sync::OwnedSemaphorePermit,
    task: CodexHelperTaskGuard,
}

impl Drop for CodexHelperTaskGuard {
    fn drop(&mut self) {
        self.registry.active.fetch_sub(1, Ordering::AcqRel);
        self.registry.changed.notify_waiters();
    }
}

// Helper ownership outlives generation-scoped observer runtimes so teardown
// cannot abort the kill-and-wait phase before the child is reaped.
static SYSTEM_CODEX_HELPER_SUPERVISOR: LazyLock<CodexHelperSupervisorInitializer> =
    LazyLock::new(|| {
        CodexHelperSupervisorInitializer::start(
            CODEX_HELPER_SUPERVISOR_CAPACITY,
            CODEX_HELPER_SUPERVISOR_INIT_TIMEOUT,
        )
    });

#[derive(Clone, Debug)]
struct CodexHelperSupervisor {
    runtime: tokio::runtime::Handle,
    slots: Arc<tokio::sync::Semaphore>,
}

#[derive(Clone, Debug)]
enum CodexHelperSupervisorState {
    Initializing,
    Ready(CodexHelperSupervisor),
    Failed,
}

#[derive(Debug)]
struct CodexHelperSupervisorInitializer {
    state: tokio::sync::watch::Receiver<CodexHelperSupervisorState>,
    timeout: Duration,
    shutdown: tokio_util::sync::CancellationToken,
}

impl CodexHelperSupervisorInitializer {
    fn start(capacity: usize, timeout: Duration) -> Self {
        Self::start_with(capacity, timeout, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
    }

    fn start_with<F>(capacity: usize, timeout: Duration, initialize: F) -> Self
    where
        F: FnOnce() -> Result<tokio::runtime::Runtime, String> + Send + 'static,
    {
        let (sender, receiver) =
            tokio::sync::watch::channel(CodexHelperSupervisorState::Initializing);
        let failure_sender = sender.clone();
        let shutdown = tokio_util::sync::CancellationToken::new();
        let thread_shutdown = shutdown.clone();
        let spawned = std::thread::Builder::new()
            .name("codex-helper-supervisor".to_owned())
            .spawn(move || {
                let initialized =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(initialize));
                let runtime = match initialized {
                    Ok(Ok(runtime)) => runtime,
                    Ok(Err(_)) | Err(_) => {
                        let _ = sender.send(CodexHelperSupervisorState::Failed);
                        return;
                    }
                };
                let supervisor = CodexHelperSupervisor {
                    runtime: runtime.handle().clone(),
                    slots: Arc::new(tokio::sync::Semaphore::new(capacity)),
                };
                if sender
                    .send(CodexHelperSupervisorState::Ready(supervisor))
                    .is_err()
                {
                    return;
                }
                runtime.block_on(thread_shutdown.cancelled());
            });
        if spawned.is_err() {
            let _ = failure_sender.send(CodexHelperSupervisorState::Failed);
        }
        Self {
            state: receiver,
            timeout,
            shutdown,
        }
    }

    async fn wait_ready(&self) -> Option<CodexHelperSupervisor> {
        let mut state = self.state.clone();
        tokio::time::timeout(self.timeout, async move {
            loop {
                match state.borrow().clone() {
                    CodexHelperSupervisorState::Initializing => {}
                    CodexHelperSupervisorState::Ready(supervisor) => return Some(supervisor),
                    CodexHelperSupervisorState::Failed => return None,
                }
                if state.changed().await.is_err() {
                    return None;
                }
            }
        })
        .await
        .ok()
        .flatten()
    }
}

impl Drop for CodexHelperSupervisorInitializer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl CodexHelperSupervisor {
    #[cfg(test)]
    fn with_runtime(runtime: tokio::runtime::Handle, capacity: usize) -> Self {
        Self {
            runtime,
            slots: Arc::new(tokio::sync::Semaphore::new(capacity)),
        }
    }

    fn reserve(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().ok()
    }

    fn supervise(
        &self,
        child: Box<dyn ChildWrapper>,
        socket_path: PathBuf,
        ownership: CodexHelperSupervisionOwnership,
        readiness_timeout: Duration,
        #[cfg(test)] fixture: CodexHelperSupervisionFixture,
    ) -> (
        SystemCodexHelperProcess,
        tokio::sync::oneshot::Receiver<Result<(), String>>,
    ) {
        let CodexHelperSupervisionOwnership {
            registration,
            permit,
            task: owned_task,
        } = ownership;
        let termination = tokio_util::sync::CancellationToken::new();
        let child_termination = termination.clone();
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        let CodexHelperSupervisionFixture {
            integration_deadline,
            reaped: fixture_reaped,
            watchdog_spawn_race: fixture_watchdog_spawn_race,
        } = fixture;
        self.runtime.spawn(async move {
            #[cfg(not(unix))]
            let mut child = child;
            let _registration = registration;
            let _owned_task = owned_task;
            let ready_deadline = {
                #[cfg(test)]
                if let Some(deadline) = integration_deadline {
                    deadline
                } else {
                    tokio::time::Instant::now() + readiness_timeout
                }
                #[cfg(not(test))]
                {
                    tokio::time::Instant::now() + readiness_timeout
                }
            };
            async {
                let readiness = loop {
                if socket_path.exists() {
                    break Ok(());
                }
                #[cfg(unix)]
                let liveness = child
                    .id()
                    .ok_or_else(|| "Codex App Server helper PID is unavailable".to_owned())
                    .and_then(codex_helper_root_exited_without_reaping)
                    .map(|exited| exited.then_some(()));
                #[cfg(not(unix))]
                let liveness = child.try_wait().map_err(|error| error.to_string());
                match liveness {
                    Ok(Some(_)) => {
                        break Err("Codex App Server helper exited before readiness".to_owned());
                    }
                    Err(error) => break Err(format!("failed to inspect Codex App Server helper: {error}")),
                    Ok(None) => {}
                }
                if tokio::time::Instant::now() >= ready_deadline {
                    break Err("Codex App Server helper readiness timed out".to_owned());
                }
                tokio::select! {
                    biased;
                    () = child_termination.cancelled() => {
                        #[cfg(test)]
                        if let Some(race) = fixture_watchdog_spawn_race.as_ref() {
                            race.cleanup_started.publish();
                            race.cleanup_release.wait_after(0).await;
                        }
                        terminate_and_reap_codex_helper(child).await;
                        return;
                    }
                    () = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            };
            if let Err(error) = readiness {
                let _ = ready_sender.send(Err(error));
                terminate_and_reap_codex_helper(child).await;
                return;
            }
            if ready_sender.send(Ok(())).is_err() {
                terminate_and_reap_codex_helper(child).await;
                return;
            }
            #[cfg(unix)]
            {
                let root_exit = child.id().map(wait_for_codex_helper_root_exit_without_reaping);
                tokio::select! {
                    biased;
                    () = child_termination.cancelled() => {
                        terminate_and_reap_codex_helper(child).await;
                    }
                    result = async {
                        match root_exit {
                            Some(root_exit) => root_exit.await,
                            None => Err("Codex helper PID was unavailable after readiness".to_owned()),
                        }
                    } => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "failed to observe Codex helper root exit");
                        }
                        terminate_and_reap_codex_helper(child).await;
                    }
                }
            }
            #[cfg(not(unix))]
            {
                tokio::select! {
                    biased;
                    () = child_termination.cancelled() => {
                        terminate_and_reap_codex_helper(child).await;
                    }
                    result = child.wait() => {
                        if let Err(error) = result {
                            tracing::warn!(error = %error, "failed to reap Codex helper after exit");
                        }
                    }
                }
            }
            }
            .await;
            drop(permit);
            #[cfg(test)]
            if let Some(event) = fixture_reaped.as_ref() {
                event.publish();
            }
        });
        (SystemCodexHelperProcess { termination }, ready_receiver)
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct CodexHelperSupervisionFixture {
    integration_deadline: Option<tokio::time::Instant>,
    reaped: Option<Arc<crate::test_support::FixtureEvent>>,
    watchdog_spawn_race: Option<Arc<CodexHelperWatchdogSpawnRace>>,
}

async fn terminate_and_reap_codex_helper(mut child: Box<dyn ChildWrapper>) {
    if let Err(error) = child.start_kill() {
        tracing::warn!(error = %error, "failed to terminate owned Codex helper");
    }
    if tokio::time::timeout(CODEX_HELPER_REAP_TIMEOUT, child.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            timeout_ms = CODEX_HELPER_REAP_TIMEOUT.as_millis(),
            "Codex helper reap exceeded its bounded cleanup window; retaining ownership"
        );
        if let Err(error) = child.wait().await {
            tracing::warn!(error = %error, "failed to reap owned Codex helper");
        }
    }
}

#[cfg(unix)]
fn codex_helper_root_exited_without_reaping(process_id: u32) -> Result<bool, String> {
    let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: information is valid writable storage. WNOWAIT reserves the
    // owned root identity while WNOHANG makes this a liveness observation.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            process_id as libc::id_t,
            information.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(format!(
            "failed observing Codex helper root: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: successful waitid initialized the zeroed siginfo storage.
    let information = unsafe { information.assume_init() };
    // SAFETY: si_pid reads the process field of initialized siginfo.
    Ok(unsafe { information.si_pid() } != 0)
}

#[cfg(unix)]
async fn wait_for_codex_helper_root_exit_without_reaping(process_id: u32) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let process_id = libc::id_t::try_from(process_id)
            .map_err(|_| "Codex helper PID exceeded the platform range".to_owned())?;
        loop {
            let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // SAFETY: information is valid writable storage. WNOWAIT retains
            // the exited leader as the identity reservation for its PGID.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    process_id,
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("failed waiting for Codex helper root: {error}"));
            }
        }
    })
    .await
    .map_err(|error| format!("Codex helper root observer task failed: {error}"))?
}

struct SystemCodexHelperProcess {
    termination: tokio_util::sync::CancellationToken,
}

impl fmt::Debug for SystemCodexHelperProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SystemCodexHelperProcess")
            .finish_non_exhaustive()
    }
}

impl CodexHelperProcess for SystemCodexHelperProcess {
    fn terminate(&self) {
        self.termination.cancel();
    }
}

impl Drop for SystemCodexHelperProcess {
    fn drop(&mut self) {
        self.termination.cancel();
    }
}

#[derive(Debug)]
struct SystemCodexRemoteClientFactory;

impl CodexRemoteClientFactory for SystemCodexRemoteClientFactory {
    fn connect(&self, endpoint: &str) -> CodexRemoteConnectFuture<'_> {
        let endpoint = endpoint.to_owned();
        Box::pin(async move {
            #[cfg(unix)]
            {
                let Some(path) = endpoint.strip_prefix("unix://") else {
                    return Err("unsupported Codex remote endpoint".to_owned());
                };
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|error| format!("failed to connect to Codex App Server: {error}"))?;
                let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(CODEX_REMOTE_MESSAGE_MAX_BYTES))
                    .max_frame_size(Some(CODEX_REMOTE_MESSAGE_MAX_BYTES));
                let (socket, _) = tokio_tungstenite::client_async_with_config(
                    "ws://localhost/rpc",
                    stream,
                    Some(config),
                )
                .await
                .map_err(|error| format!("Codex App Server handshake failed: {error}"))?;
                Ok(Box::new(SystemCodexRemoteClient {
                    socket,
                    next_request_id: 1,
                    pending: VecDeque::new(),
                }) as Box<dyn CodexRemoteClient>)
            }
            #[cfg(not(unix))]
            {
                let _ = endpoint;
                Err("Codex terminal remote transport is unsupported".to_owned())
            }
        })
    }
}

#[cfg(unix)]
struct SystemCodexRemoteClient {
    socket: tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    next_request_id: u64,
    pending: VecDeque<Value>,
}

#[cfg(unix)]
impl SystemCodexRemoteClient {
    async fn read_value(&mut self) -> Result<Option<Value>, String> {
        while let Some(message) = self.socket.next().await {
            let message = message.map_err(|error| format!("Codex remote read failed: {error}"))?;
            let value = match message {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    if text.len() > CODEX_REMOTE_MESSAGE_MAX_BYTES {
                        return Err("Codex remote message exceeded its fixed bound".to_owned());
                    }
                    serde_json::from_slice(text.as_bytes())
                }
                tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                    if bytes.len() > CODEX_REMOTE_MESSAGE_MAX_BYTES {
                        return Err("Codex remote message exceeded its fixed bound".to_owned());
                    }
                    serde_json::from_slice(bytes.as_ref())
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => return Ok(None),
                tokio_tungstenite::tungstenite::Message::Ping(_)
                | tokio_tungstenite::tungstenite::Message::Pong(_)
                | tokio_tungstenite::tungstenite::Message::Frame(_) => continue,
            }
            .map_err(|error| format!("Codex remote JSON was invalid: {error}"))?;
            return Ok(Some(value));
        }
        Ok(None)
    }
}

#[cfg(unix)]
impl CodexRemoteClient for SystemCodexRemoteClient {
    fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let method = method.to_owned();
        Box::pin(async move {
            let id = self.next_request_id;
            self.next_request_id = self
                .next_request_id
                .checked_add(1)
                .ok_or_else(|| "Codex remote request id space was exhausted".to_owned())?;
            self.socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"id": id, "method": method, "params": params})
                        .to_string()
                        .into(),
                ))
                .await
                .map_err(|error| format!("Codex remote write failed: {error}"))?;
            loop {
                let value = self
                    .read_value()
                    .await?
                    .ok_or_else(|| "Codex remote closed before response".to_owned())?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(error) = value.get("error") {
                        return Err(format!("Codex remote request failed: {error}"));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                // A timed-out request may complete after a later request has
                // started. Its response is stale, not a future notification.
                if value.get("id").is_some() {
                    continue;
                }
                if self.pending.len() == CODEX_REMOTE_PENDING_NOTIFICATION_LIMIT {
                    self.pending.pop_front();
                }
                self.pending.push_back(value);
            }
        })
    }

    fn notify(
        &mut self,
        method: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let method = method.to_owned();
        Box::pin(async move {
            self.socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"method": method}).to_string().into(),
                ))
                .await
                .map_err(|error| format!("Codex remote write failed: {error}"))
        })
    }

    fn drain_request_buffered_notifications(&mut self) -> Vec<Value> {
        self.pending.drain(..).collect()
    }

    fn next(&mut self) -> Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send + '_>> {
        Box::pin(async move {
            if let Some(value) = self.pending.pop_front() {
                return Ok(Some(value));
            }
            self.read_value().await
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::{FixtureEvent, TestSandbox};

    #[cfg(feature = "hermetic-test-guard")]
    #[tokio::test]
    async fn hermetic_guard_refuses_direct_codex_probe_and_helper_before_spawn() {
        let Some(executable) = crate::hermetic_guard::refusal_fixture(
            "provider_terminal::codex::tests::hermetic_guard_refuses_direct_codex_probe_and_helper_before_spawn",
            "2.1.220",
        ) else {
            return;
        };
        assert!(
            SystemCodexCapabilityProbeRunner::default()
                .run(&executable, vec![])
                .await
                .is_err()
        );
        let launch = CodexHelperLaunch {
            executable: executable.display().to_string(),
            args: vec![],
            cwd: executable.parent().unwrap().to_path_buf(),
            env: BTreeMap::new(),
            endpoint: "unused".to_owned(),
            socket_path: executable.with_extension("socket"),
            process_attribution: ProcessAttributionRegistry::new(),
        };
        assert!(
            SystemCodexHelperLauncher::default()
                .start(launch)
                .await
                .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn system_helper_launcher_retries_while_its_executable_is_busy() {
        use crate::test_support::within_fixture_deadline;

        let fixture = helper_fixture(TestSandbox::new("codex-helper-busy-executable"));
        let writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&fixture.executable)
            .expect("hold the helper executable open for writing");
        let launcher = Arc::new(SystemCodexHelperLauncher::with_fixture_events(
            CODEX_HELPER_READY_TIMEOUT,
            fixture.events.clone(),
        ));
        let launch = fixture.launch(ProcessAttributionRegistry::new());
        let start = tokio::spawn({
            let launcher = launcher.clone();
            async move { launcher.start(launch).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !start.is_finished(),
            "spawn must keep retrying while the script is busy: {:?}",
            within_fixture_deadline("the failed Codex helper start task to finish", start).await
        );
        drop(writer);

        let process = within_fixture_deadline("the retried Codex helper to become ready", start)
            .await
            .expect("Codex helper start task")
            .expect("spawn must succeed once the writer closes");
        process.terminate();
        within_fixture_deadline(
            "the retried Codex helper to be reaped",
            fixture.events.reaped.wait_after(0),
        )
        .await;
        within_fixture_deadline(
            "the retried Codex helper launcher to shut down",
            launcher.shutdown(),
        )
        .await;
        let pid = read_fixture_pid(&fixture.pid_path).expect("retried Codex helper PID");
        assert!(fixture.socket_path.exists(), "helper published its socket");
        assert!(!process_exists(pid), "retried Codex helper survived reap");
        assert_child_was_reaped(pid, "retried Codex helper");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_capability_probe_ignores_appimage_environment() {
        crate::test_support::check_capability_probe_appimage_environment(
            "provider_terminal::codex::tests::system_capability_probe_ignores_appimage_environment",
            |executable, args| async move {
                let output = SystemCodexCapabilityProbeRunner::default()
                    .run(&executable, args)
                    .await?;
                Ok((output.success, output.stdout))
            },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_helper_launcher_ignores_appimage_environment() {
        use crate::test_support::{
            ISOLATING_AND_NO_OP_CASES,
            appimage_environment::{assert_child_environment, check_inherited_environment},
        };

        check_inherited_environment(
            "provider_terminal::codex::tests::system_helper_launcher_ignores_appimage_environment",
            ISOLATING_AND_NO_OP_CASES,
            |expected| {
                let sandbox = TestSandbox::new("codex-helper-appimage-environment");
                let output_path = sandbox.path("environment");
                let socket_path = sandbox.path("helper.sock");
                let executable = sandbox.executable_script(
                    "helper",
                    "/usr/bin/env -0 > \"$4\"\nsocket=${3#unix://}\n: > \"$socket\"\nexec /bin/sleep 3600",
                    "",
                );
                crate::test_support::run_on_current_thread(async {
                    let launcher = SystemCodexHelperLauncher::default();
                    let endpoint = format!("unix://{}", socket_path.to_string_lossy());
                    let process = launcher
                        .start(CodexHelperLaunch {
                            executable: executable.to_string_lossy().into_owned(),
                            args: vec![
                                "app-server".to_owned(),
                                "--listen".to_owned(),
                                endpoint.clone(),
                                output_path.to_string_lossy().into_owned(),
                            ],
                            cwd: sandbox.root().to_path_buf(),
                            env: BTreeMap::new(),
                            endpoint,
                            socket_path,
                            process_attribution: ProcessAttributionRegistry::new(),
                        })
                        .await
                        .expect("Codex system helper launcher");
                    process.terminate();
                    launcher.shutdown().await;
                });
                assert_child_environment(
                    &std::fs::read(output_path).expect("Codex helper child environment"),
                    expected,
                );
            },
        );
    }

    #[derive(Debug)]
    struct CodexHelperFixture {
        sandbox: TestSandbox,
        executable: PathBuf,
        pid_path: PathBuf,
        socket_path: PathBuf,
        events: CodexHelperFixtureEvents,
    }

    fn helper_fixture(sandbox: TestSandbox) -> CodexHelperFixture {
        let pid_path = sandbox.path("helper.pid");
        let socket_path = sandbox.path("helper.sock");
        let executable = sandbox.executable_script(
            "helper",
            "socket=${3#unix://}\n: > \"$socket\"\nexec sleep 3600",
            "",
        );
        CodexHelperFixture {
            sandbox,
            executable,
            pid_path: pid_path.clone(),
            socket_path,
            events: CodexHelperFixtureEvents {
                spawned: Arc::new(FixtureEvent::default()),
                admission_race: None,
                ready: Arc::new(FixtureEvent::default()),
                reaped: Arc::new(FixtureEvent::default()),
                pid_path,
                watchdog_spawn_race: None,
            },
        }
    }

    impl CodexHelperFixture {
        fn launch(&self, process_attribution: ProcessAttributionRegistry) -> CodexHelperLaunch {
            let endpoint = format!("unix://{}", self.socket_path.to_string_lossy());
            CodexHelperLaunch {
                executable: self.executable.to_string_lossy().into_owned(),
                args: vec![
                    "app-server".to_owned(),
                    "--listen".to_owned(),
                    endpoint.clone(),
                ],
                cwd: self.sandbox.root().to_path_buf(),
                env: self
                    .sandbox
                    .environment(std::iter::empty::<(String, String)>()),
                endpoint,
                socket_path: self.socket_path.clone(),
                process_attribution,
            }
        }

        async fn start_and_wait_until(
            &self,
            deadline: tokio::time::Instant,
        ) -> Result<
            (
                Result<Arc<dyn CodexHelperProcess>, String>,
                Result<i32, String>,
            ),
            String,
        > {
            self.start_and_wait_with_deadlines(deadline, deadline).await
        }

        async fn start_and_wait_with_deadlines(
            &self,
            outer_deadline: tokio::time::Instant,
            owner_deadline: tokio::time::Instant,
        ) -> Result<
            (
                Result<Arc<dyn CodexHelperProcess>, String>,
                Result<i32, String>,
            ),
            String,
        > {
            let launcher = SystemCodexHelperLauncher::with_integration_fixture_events(
                owner_deadline,
                self.events.clone(),
            );
            let launch = self.launch(ProcessAttributionRegistry::new());
            let spawned_checkpoint = self.events.spawned.checkpoint();
            let ready_checkpoint = self.events.ready.checkpoint();
            let reaped_checkpoint = self.events.reaped.checkpoint();
            let ready = self.events.ready.clone();
            let spawned = self.events.spawned.clone();
            let mut start = tokio::spawn(async move { launcher.start(launch).await });
            let spawned_outcome = tokio::select! {
                biased;
                result = &mut start => Some(result),
                result = tokio::time::timeout_at(outer_deadline, spawned.wait_after(spawned_checkpoint)) => {
                    if result.is_err() {
                        return Err(self
                            .cancel_start_after_watchdog(
                                start,
                                spawned_checkpoint,
                                reaped_checkpoint,
                                "spawn publication",
                            )
                            .await);
                    }
                    None
                }
            };
            if let Some(result) = spawned_outcome {
                let helper = result
                    .map_err(|error| format!("Codex helper start task failed: {error}"))
                    .and_then(std::convert::identity);
                if helper.is_ok()
                    && (self.events.spawned.checkpoint() <= spawned_checkpoint
                        || self.events.ready.checkpoint() <= ready_checkpoint)
                {
                    return Ok((
                        Err("Codex helper owner completed before fixture readiness".to_owned()),
                        read_fixture_pid(&self.pid_path),
                    ));
                }
                return Ok((helper, read_fixture_pid(&self.pid_path)));
            }
            let pid = read_fixture_pid(&self.pid_path);
            let ready_outcome = tokio::select! {
                biased;
                result = &mut start => Some(result),
                result = tokio::time::timeout_at(outer_deadline, ready.wait_after(ready_checkpoint)) => {
                    if result.is_err() {
                        return Err(self
                            .cancel_start_after_watchdog(
                                start,
                                spawned_checkpoint,
                                reaped_checkpoint,
                                "readiness",
                            )
                            .await);
                    }
                    None
                }
            };
            let helper = match ready_outcome {
                Some(result) => {
                    let helper = result
                        .map_err(|error| format!("Codex helper start task failed: {error}"))
                        .and_then(std::convert::identity);
                    if helper.is_ok() && self.events.ready.checkpoint() <= ready_checkpoint {
                        Err("Codex helper owner completed before fixture readiness".to_owned())
                    } else {
                        helper
                    }
                }
                None => match tokio::time::timeout_at(outer_deadline, &mut start).await {
                    Ok(result) => result
                        .map_err(|error| format!("Codex helper start task failed: {error}"))
                        .and_then(std::convert::identity),
                    Err(_) => {
                        return Err(self
                            .cancel_start_after_watchdog(
                                start,
                                spawned_checkpoint,
                                reaped_checkpoint,
                                "owner completion",
                            )
                            .await);
                    }
                },
            };
            Ok((helper, pid))
        }

        async fn cancel_start_after_watchdog(
            &self,
            start: tokio::task::JoinHandle<Result<Arc<dyn CodexHelperProcess>, String>>,
            spawned_checkpoint: u64,
            reaped_checkpoint: u64,
            milestone: &'static str,
        ) -> String {
            if let Some(race) = self.events.watchdog_spawn_race.as_ref() {
                race.release_publication.wait();
                race.publication_complete.wait();
            }
            start.abort();
            let owner_failure = match start.await {
                Err(error) if error.is_cancelled() => None,
                Err(error) => Some(format!("Codex helper start task failed: {error}")),
                Ok(Err(error)) => Some(error),
                Ok(Ok(helper)) => {
                    drop(helper);
                    None
                }
            };
            if let Some(race) = self.events.watchdog_spawn_race.as_ref() {
                race.owner_joined.publish();
            }
            let published = self.events.spawned.checkpoint() > spawned_checkpoint;
            if published && self.events.reaped.checkpoint() <= reaped_checkpoint {
                tokio::time::timeout(
                    Duration::from_secs(10),
                    self.events.reaped.wait_after(reaped_checkpoint),
                )
                .await
                .expect("Codex helper watchdog cleanup must publish owner completion");
            }
            match owner_failure {
                Some(error) => format!(
                    "Codex helper integration deadline expired before {milestone}; owner failed: {error}"
                ),
                None => format!("Codex helper integration deadline expired before {milestone}"),
            }
        }

        async fn wait_reaped_until(&self, deadline: tokio::time::Instant, checkpoint: u64) {
            tokio::time::timeout_at(deadline, self.events.reaped.wait_after(checkpoint))
                .await
                .expect("Codex helper reap outer watchdog");
        }
    }

    #[derive(Debug)]
    struct CodexBoundedProbeFixture {
        _sandbox: TestSandbox,
        executable: PathBuf,
        spawned: Arc<FixtureEvent>,
        pid_path: PathBuf,
        entered_path: PathBuf,
        stdout_complete_path: PathBuf,
        stderr_complete_path: PathBuf,
        completed_path: PathBuf,
        input_bytes: usize,
    }

    fn bounded_probe_fixture(sandbox: TestSandbox) -> CodexBoundedProbeFixture {
        let input_bytes = CODEX_PROBE_OUTPUT_LIMIT + 8 * 1024;
        let pid_path = sandbox.path("probe.pid");
        let entered_path = sandbox.path("probe.entered");
        let stdout_complete_path = sandbox.path("probe.stdout-complete");
        let stderr_complete_path = sandbox.path("probe.stderr-complete");
        let completed_path = sandbox.path("probe.completed");
        let executable = sandbox.executable_script(
            "large-probe",
            &format!(
                "printf entered > \"$1\"\n\
                 dd if=/dev/zero bs={input_bytes} count=1 2>/dev/null || exit 70\n\
                 printf '{input_bytes}' > \"$2\"\n\
                 dd if=/dev/zero bs={input_bytes} count=1 1>&2 2>/dev/null || exit 71\n\
                 printf '{input_bytes}' > \"$3\"\n\
                 printf completed > \"$4\""
            ),
            "",
        );
        CodexBoundedProbeFixture {
            _sandbox: sandbox,
            executable,
            spawned: Arc::new(FixtureEvent::default()),
            pid_path,
            entered_path,
            stdout_complete_path,
            stderr_complete_path,
            completed_path,
            input_bytes,
        }
    }

    impl CodexBoundedProbeFixture {
        async fn run_until(
            &self,
            deadline: tokio::time::Instant,
        ) -> Result<CodexProbeOutput, String> {
            let runner = SystemCodexCapabilityProbeRunner::with_integration_deadline(
                deadline,
                Duration::from_secs(2),
            )
            .with_spawn_fixture(CodexProbeSpawnFixture {
                spawned: self.spawned.clone(),
                pid_path: self.pid_path.clone(),
            });
            let run = runner.run(
                &self.executable,
                vec![
                    self.entered_path.to_string_lossy().into_owned(),
                    self.stdout_complete_path.to_string_lossy().into_owned(),
                    self.stderr_complete_path.to_string_lossy().into_owned(),
                    self.completed_path.to_string_lossy().into_owned(),
                ],
            );
            tokio::pin!(run);
            match tokio::time::timeout_at(deadline, &mut run).await {
                Ok(result) => result,
                Err(_) => {
                    let _ = tokio::time::timeout(Duration::from_secs(10), &mut run)
                        .await
                        .expect("Codex probe deadline cleanup owner must complete");
                    Err("Codex probe integration deadline expired".to_owned())
                }
            }
        }

        fn assert_completed_and_reaped(&self) -> i32 {
            assert!(
                self.spawned.checkpoint() > 0,
                "Codex probe owner did not publish child spawn"
            );
            assert_eq!(
                std::fs::read_to_string(&self.entered_path)
                    .expect("read Codex probe entry milestone"),
                "entered",
                "Codex probe fixture did not enter its body"
            );
            let expected_input_bytes = self.input_bytes.to_string();
            assert_eq!(
                std::fs::read_to_string(&self.stdout_complete_path)
                    .expect("read Codex probe stdout milestone"),
                expected_input_bytes,
                "Codex probe fixture did not publish its exact stdout input"
            );
            assert_eq!(
                std::fs::read_to_string(&self.stderr_complete_path)
                    .expect("read Codex probe stderr milestone"),
                expected_input_bytes,
                "Codex probe fixture did not publish its exact stderr input"
            );
            assert_eq!(
                std::fs::read_to_string(&self.completed_path)
                    .expect("read Codex probe completion milestone"),
                "completed",
                "Codex probe fixture body did not complete"
            );
            let pid = read_fixture_pid(&self.pid_path).expect("Codex probe fixture PID");
            assert!(
                !process_exists(pid),
                "Codex probe process survived owner completion"
            );
            assert_child_was_reaped(pid, "Codex probe");
            pid
        }
    }

    fn read_fixture_pid(path: &Path) -> Result<i32, String> {
        std::fs::read_to_string(path)
            .map_err(|error| format!("failed reading fixture PID: {error}"))?
            .parse::<i32>()
            .map_err(|error| format!("fixture PID was not numeric: {error}"))
    }

    #[test]
    fn bounded_probe_text_includes_separators_within_the_limit() {
        let stdout = "x".repeat(CODEX_PROBE_OUTPUT_LIMIT);

        let output = bounded_probe_text(&stdout, "stderr");

        assert_eq!(output.len(), CODEX_PROBE_OUTPUT_LIMIT);
    }

    #[test]
    fn terminal_tracker_uses_sub_agent_activity_only_to_trigger_list_reconciliation() {
        let mut tracker = CodexActivityTracker::new_for_terminal_observation(Some("terminal-root"));
        tracker.seed_actor("terminal-root");

        let live_hint = tracker.handle_notification(
            "item/started",
            &json!({
                "threadId": "terminal-root",
                "turnId": "turn-1",
                "item": {
                    "id": "hint-1",
                    "type": "subAgentActivity",
                    "agentThreadId": "hint-only-child",
                    "agentPath": "root/Hint only child",
                    "kind": "started"
                }
            }),
            1_000,
            0,
        );
        let empty_list = tracker.reconcile_descendants(&[]);

        assert!(
            live_hint.request_reconciliation,
            "a terminal hint may wake the existing list-based reconciliation"
        );
        assert!(live_hint.hinted_descendant_ids.is_empty());
        assert!(live_hint.mutations.is_empty());
        assert!(empty_list.output.mutations.is_empty());
        assert!(empty_list.threads_to_read.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_codex_helpers_publish_distinct_pids_and_reap_in_parallel() {
        let left = helper_fixture(TestSandbox::new("left"));
        let right = helper_fixture(TestSandbox::new("right"));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let left_reaped_checkpoint = left.events.reaped.checkpoint();
        let right_reaped_checkpoint = right.events.reaped.checkpoint();
        let (left_start, right_start) = tokio::join!(
            left.start_and_wait_until(deadline),
            right.start_and_wait_until(deadline)
        );
        let (left_helper, left_pid) = left_start.expect("left helper integration owner");
        let (right_helper, right_pid) = right_start.expect("right helper integration owner");
        let left_pid = left_pid.expect("left PID");
        let right_pid = right_pid.expect("right PID");
        assert_ne!(left_pid, right_pid);
        left_helper.expect("left helper").terminate();
        right_helper.expect("right helper").terminate();
        tokio::join!(
            left.wait_reaped_until(deadline, left_reaped_checkpoint),
            right.wait_reaped_until(deadline, right_reaped_checkpoint)
        );
        assert!(!process_exists(left_pid), "left Codex helper survived reap");
        assert!(
            !process_exists(right_pid),
            "right Codex helper survived reap"
        );
        assert_child_was_reaped(left_pid, "left Codex helper");
        assert_child_was_reaped(right_pid, "right Codex helper");
    }

    #[tokio::test]
    async fn helper_fixture_reports_owner_failure_before_readiness_watchdog() {
        let mut fixture = helper_fixture(TestSandbox::new("missing-helper"));
        fixture.executable = fixture.sandbox.path("missing-helper");
        let deadline = tokio::time::Instant::now() + Duration::from_millis(250);

        let observed = tokio::time::timeout(
            Duration::from_millis(250),
            fixture.start_and_wait_until(deadline),
        )
        .await
        .expect("the helper owner error must not be hidden by the readiness watchdog")
        .expect("missing helper must report its owned result before the integration deadline");

        let error = observed.0.expect_err("missing helper must fail to spawn");
        assert!(
            error.contains("failed to start Codex App Server helper"),
            "unexpected helper owner error: {error}"
        );
        assert!(
            observed.1.is_err(),
            "a helper that did not spawn cannot publish a PID"
        );
    }

    #[tokio::test]
    async fn helper_integration_watchdog_aborts_joins_and_reaps_the_owned_start() {
        let mut fixture = helper_fixture(TestSandbox::new("helper-watchdog-cleanup"));
        fixture.executable =
            fixture
                .sandbox
                .executable_script("unready-helper", "exec sleep 3600", "");
        let outer_deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        let owner_deadline = tokio::time::Instant::now() + Duration::from_secs(15);

        let error = fixture
            .start_and_wait_with_deadlines(outer_deadline, owner_deadline)
            .await
            .expect_err("the integration watchdog must expire before helper readiness");

        assert!(
            error.contains("before readiness"),
            "watchdog error omitted its last milestone: {error}"
        );
        let pid = read_fixture_pid(&fixture.pid_path).expect("watchdog helper PID");
        assert!(
            fixture.events.reaped.checkpoint() > 0,
            "watchdog cleanup did not publish supervisor completion"
        );
        assert!(
            !process_exists(pid),
            "watchdog helper survived owner cleanup"
        );
        assert_child_was_reaped(pid, "watchdog Codex helper");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn helper_watchdog_closes_spawn_publication_race_before_returning() {
        let mut fixture = helper_fixture(TestSandbox::new("helper-spawn-watchdog-race"));
        fixture.executable =
            fixture
                .sandbox
                .executable_script("unready-helper", "exec sleep 3600", "");
        let race = Arc::new(CodexHelperWatchdogSpawnRace::new());
        fixture.events.watchdog_spawn_race = Some(race.clone());
        let outer_deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        let owner_deadline = tokio::time::Instant::now() + Duration::from_secs(15);

        let cleanup = fixture.start_and_wait_with_deadlines(outer_deadline, owner_deadline);
        tokio::pin!(cleanup);
        tokio::select! {
            biased;
            result = &mut cleanup => {
                panic!("watchdog returned before gated supervisor cleanup: {result:?}");
            }
            result = tokio::time::timeout(
                Duration::from_secs(5),
                race.owner_joined.wait_after(0),
            ) => {
                result.expect("spawn-race helper owner was not joined");
            }
        }
        tokio::time::timeout(Duration::from_secs(5), race.cleanup_started.wait_after(0))
            .await
            .expect("spawn-race supervisor cleanup did not start");
        std::future::poll_fn(|context| {
            assert!(
                cleanup.as_mut().poll(context).is_pending(),
                "watchdog returned while supervisor cleanup was still gated"
            );
            std::task::Poll::Ready(())
        })
        .await;
        race.cleanup_release.publish();
        let error = cleanup
            .await
            .expect_err("spawn publication must lose the integration watchdog race");

        assert!(
            error.contains("before spawn publication"),
            "watchdog error omitted its last milestone: {error}"
        );
        assert!(
            fixture.events.spawned.checkpoint() > 0,
            "deterministic race did not publish the PID after the stale snapshot"
        );
        let pid = read_fixture_pid(&fixture.pid_path).expect("spawn-race helper PID");
        assert!(
            fixture.events.reaped.checkpoint() > 0,
            "spawn-race watchdog returned before supervisor completion"
        );
        assert!(!process_exists(pid), "spawn-race helper survived cleanup");
        assert_child_was_reaped(pid, "spawn-race Codex helper");
    }

    #[tokio::test]
    async fn helper_reap_event_is_published_after_its_capacity_is_released() {
        let sandbox = TestSandbox::new("helper-capacity-recovery");
        let executable = sandbox.executable_script("helper", "exec sleep 3600", "");
        let socket_path = sandbox.path("ready.sock");
        std::fs::write(&socket_path, b"ready").expect("publish helper readiness fixture");
        let mut command = CommandWrap::with_new(executable, |_| {});
        configure_supervised_background_command_wrap(&mut command);
        let child = command.spawn().expect("spawn helper capacity fixture");
        let pid = i32::try_from(child.id().expect("helper capacity fixture PID"))
            .expect("helper capacity fixture PID fits i32");
        let supervisor = CodexHelperSupervisor::with_runtime(tokio::runtime::Handle::current(), 1);
        let permit = supervisor
            .reserve()
            .expect("reserve only helper capacity slot");
        let reaped = Arc::new(FixtureEvent::default());
        let (helper, ready) = supervisor.supervise(
            child,
            socket_path,
            CodexHelperSupervisionOwnership {
                registration: None,
                permit,
                task: Arc::new(CodexHelperTaskRegistry::default()).register(),
            },
            CODEX_HELPER_READY_TIMEOUT,
            CodexHelperSupervisionFixture {
                reaped: Some(reaped.clone()),
                ..CodexHelperSupervisionFixture::default()
            },
        );
        ready
            .await
            .expect("capacity helper supervisor ready owner")
            .expect("capacity helper readiness");

        helper.terminate();
        tokio::time::timeout(Duration::from_secs(10), reaped.wait_after(0))
            .await
            .expect("capacity helper reap outer watchdog");

        let recovered = supervisor
            .reserve()
            .expect("reap publication must observe released helper capacity");
        drop(recovered);
        assert!(!process_exists(pid), "capacity helper survived owner reap");
        assert_child_was_reaped(pid, "capacity Codex helper");
    }

    #[tokio::test]
    async fn default_system_helper_readiness_timeout_terminates_and_reaps_the_owned_process() {
        let sandbox = TestSandbox::new("codex-helper-timeout");
        let pid_path = sandbox.path("helper.pid");
        let socket_path = sandbox.path("never-created.sock");
        let executable = sandbox.executable_script("unready-helper", "exec sleep 3600", "");
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());
        let events = CodexHelperFixtureEvents {
            spawned: Arc::new(FixtureEvent::default()),
            admission_race: None,
            ready: Arc::new(FixtureEvent::default()),
            reaped: Arc::new(FixtureEvent::default()),
            pid_path: pid_path.clone(),
            watchdog_spawn_race: None,
        };
        let reaped_checkpoint = events.reaped.checkpoint();
        let launcher = SystemCodexHelperLauncher {
            fixture_events: Some(events.clone()),
            ..SystemCodexHelperLauncher::default()
        };
        assert_eq!(launcher.readiness_timeout, Duration::from_secs(3));

        let error = launcher
            .start(CodexHelperLaunch {
                executable: executable.to_string_lossy().into_owned(),
                args: vec![
                    "app-server".to_owned(),
                    "--listen".to_owned(),
                    endpoint.clone(),
                ],
                cwd: sandbox.root().to_path_buf(),
                env: sandbox.environment(std::iter::empty::<(String, String)>()),
                endpoint,
                socket_path,
                process_attribution: ProcessAttributionRegistry::new(),
            })
            .await
            .expect_err("unready helper must fail at the default readiness timeout");

        assert!(
            error.contains("readiness timed out"),
            "unready helper failed outside the readiness boundary: {error}"
        );
        let pid = read_fixture_pid(&pid_path).expect("unready helper PID");
        tokio::time::timeout(
            Duration::from_secs(10),
            events.reaped.wait_after(reaped_checkpoint),
        )
        .await
        .expect("default-timeout helper reap outer watchdog");
        assert!(!process_exists(pid), "unready helper survived cleanup");
        assert_child_was_reaped(pid, "default-timeout Codex helper");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn frozen_runtime_reaps_its_inflight_codex_helper_without_touching_a_peer_runtime() {
        let mut owned = helper_fixture(TestSandbox::new("codex-helper-owned-freeze"));
        let peer = helper_fixture(TestSandbox::new("codex-helper-peer-runtime"));
        let admission_race = Arc::new(CodexHelperAdmissionRace {
            spawned: FixtureEvent::default(),
            release: FixtureEvent::default(),
        });
        owned.events.admission_race = Some(admission_race.clone());
        let owned_registry = ProcessAttributionRegistry::new();
        let peer_registry = ProcessAttributionRegistry::new();
        let owner_deadline = tokio::time::Instant::now() + Duration::from_secs(15);

        let peer_launcher = SystemCodexHelperLauncher::with_integration_fixture_events(
            owner_deadline,
            peer.events.clone(),
        );
        let peer_helper = peer_launcher
            .start(peer.launch(peer_registry.clone()))
            .await
            .expect("peer runtime helper starts");
        let peer_pid = read_fixture_pid(&peer.pid_path).expect("peer helper PID");
        let peer_identity = NativeProcessSampler::process_identity(
            u32::try_from(peer_pid).expect("positive peer helper PID"),
        )
        .expect("peer helper identity");

        let owned_launcher = SystemCodexHelperLauncher::with_integration_fixture_events(
            owner_deadline,
            owned.events.clone(),
        );
        let owned_tasks = Arc::clone(&owned_launcher.owned_tasks);
        let owned_launch = owned.launch(owned_registry.clone());
        let owned_start = tokio::spawn(async move { owned_launcher.start(owned_launch).await });
        admission_race.spawned.wait_after(0).await;
        let owned_pid = read_fixture_pid(&owned.pid_path).expect("owned helper PID");
        let owned_identity = NativeProcessSampler::process_identity(
            u32::try_from(owned_pid).expect("positive owned helper PID"),
        )
        .expect("owned helper identity");

        assert!(owned_registry.freeze_and_snapshot_identities().is_empty());
        admission_race.release.publish();
        let error = owned_start
            .await
            .expect("owned helper start task")
            .expect_err("frozen runtime rejects inflight helper admission");
        assert!(
            error.contains("closed for shutdown"),
            "unexpected error: {error}"
        );
        tokio::time::timeout_at(owner_deadline, owned_tasks.drain())
            .await
            .expect("rejected helper owner must finish before the integration deadline");
        assert!(
            !matches!(
                NativeProcessSampler::process_identity(owned_identity.pid),
                Ok(current) if current == owned_identity
            ),
            "rejected owned helper survived exact cleanup"
        );
        assert_eq!(
            NativeProcessSampler::process_identity(peer_identity.pid)
                .expect("peer helper remains live"),
            peer_identity
        );
        assert_eq!(
            peer_registry.freeze_and_snapshot_identities(),
            [peer_identity],
            "peer runtime keeps its independent helper registration"
        );
        assert_child_was_reaped(owned_pid, "rejected Codex helper");

        let peer_reaped_checkpoint = peer.events.reaped.checkpoint();
        peer_helper.terminate();
        let peer_cleanup = tokio::time::timeout_at(owner_deadline, async {
            peer.events.reaped.wait_after(peer_reaped_checkpoint).await;
            peer_launcher.shutdown().await;
        })
        .await;
        if peer_cleanup.is_err() {
            peer_helper.terminate();
            tokio::time::timeout(Duration::from_secs(10), peer_launcher.shutdown())
                .await
                .expect("peer helper cleanup must join the retained owner");
            panic!("peer helper owner exceeded its integration deadline");
        }
        assert!(
            !matches!(
                NativeProcessSampler::process_identity(peer_identity.pid),
                Ok(current) if current == peer_identity
            ),
            "peer helper survived its own owner cleanup"
        );
        assert_child_was_reaped(peer_pid, "peer Codex helper");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn natural_codex_helper_root_exit_reaps_late_group_descendant_before_owner_release() {
        let sandbox = TestSandbox::new("codex-helper-natural-root-exit");
        let pid_path = sandbox.path("helper.pid");
        let child_pid_path = sandbox.path("descendant.pid");
        let socket_path = sandbox.path("helper.sock");
        let executable = sandbox.executable_script(
            "helper",
            "socket=${3#unix://}\n/bin/sh -c 'trap \"\" HUP; echo $$ > \"$BIBCODE_CHILD_PID\"; exec sleep 30' & while [ ! -s \"$BIBCODE_CHILD_PID\" ]; do :; done\n: > \"$socket\"\nsleep 1\nexit 0",
            "",
        );
        let events = CodexHelperFixtureEvents {
            spawned: Arc::new(FixtureEvent::default()),
            admission_race: None,
            ready: Arc::new(FixtureEvent::default()),
            reaped: Arc::new(FixtureEvent::default()),
            pid_path,
            watchdog_spawn_race: None,
        };
        let reaped_checkpoint = events.reaped.checkpoint();
        let owner_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let launcher = SystemCodexHelperLauncher::with_integration_fixture_events(
            owner_deadline,
            events.clone(),
        );
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());
        let child_pid_environment = child_pid_path.to_string_lossy().into_owned();
        let helper = launcher
            .start(CodexHelperLaunch {
                executable: executable.to_string_lossy().into_owned(),
                args: vec![
                    "app-server".to_owned(),
                    "--listen".to_owned(),
                    endpoint.clone(),
                ],
                cwd: sandbox.root().to_path_buf(),
                env: sandbox.environment([("BIBCODE_CHILD_PID", &child_pid_environment)]),
                endpoint,
                socket_path,
                process_attribution: ProcessAttributionRegistry::new(),
            })
            .await
            .expect("Codex helper reaches readiness before natural root exit");
        let descendant_pid = tokio::fs::read_to_string(&child_pid_path)
            .await
            .expect("late Codex descendant PID")
            .trim()
            .parse::<i32>()
            .expect("numeric late Codex descendant PID");
        let descendant_identity = NativeProcessSampler::process_identity(
            u32::try_from(descendant_pid).expect("positive late Codex descendant PID"),
        )
        .expect("capture late Codex descendant identity");

        let owner_completed = tokio::time::timeout_at(owner_deadline, async {
            events.reaped.wait_after(reaped_checkpoint).await;
            launcher.shutdown().await;
        })
        .await;
        if owner_completed.is_err() {
            helper.terminate();
            tokio::time::timeout(Duration::from_secs(10), launcher.shutdown())
                .await
                .expect("Codex helper deadline cleanup must join the retained owner");
            panic!("Codex helper natural-exit owner exceeded its integration deadline");
        }
        let descendant_is_live = NativeProcessSampler::default()
            .collect_rows()
            .await
            .expect("sample late Codex descendant status")
            .into_iter()
            .find(|row| {
                row.pid == descendant_identity.pid
                    && row.started_at == descendant_identity.started_at
            })
            .is_some_and(|row| !row.status.to_ascii_lowercase().contains("zomb"));
        assert!(
            !descendant_is_live,
            "late Codex helper descendant survived natural root exit"
        );
        drop(helper);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_codex_probes_bound_both_output_streams_in_parallel() {
        let left = bounded_probe_fixture(TestSandbox::new("codex-probe-left"));
        let right = bounded_probe_fixture(TestSandbox::new("codex-probe-right"));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

        let (left_output, right_output) =
            tokio::join!(left.run_until(deadline), right.run_until(deadline));

        for (fixture, output) in [(&left, left_output), (&right, right_output)] {
            let output = output.expect("large Codex probe");
            assert!(output.success);
            assert_eq!(output.stdout.len(), CODEX_PROBE_OUTPUT_LIMIT);
            assert_eq!(output.stderr.len(), CODEX_PROBE_OUTPUT_LIMIT);
            assert_eq!(fixture.input_bytes, 136 * 1024);
            fixture.assert_completed_and_reaped();
        }
    }

    #[tokio::test]
    async fn default_system_probe_timeout_terminates_and_reaps_the_owned_process() {
        let sandbox = TestSandbox::new("codex-timeout");
        let pid_path = sandbox.path("probe.pid");
        let executable = sandbox.executable_script("hung-probe", "exec sleep 3600", "");

        let spawned = Arc::new(FixtureEvent::default());
        let runner = SystemCodexCapabilityProbeRunner::default().with_spawn_fixture(
            CodexProbeSpawnFixture {
                spawned: spawned.clone(),
                pid_path: pid_path.clone(),
            },
        );
        assert_eq!(runner.timeout, Duration::from_secs(3));
        let error = runner
            .run(&executable, Vec::new())
            .await
            .expect_err("hung probe must fail closed");

        assert!(spawned.checkpoint() > 0, "hung probe did not spawn");
        assert!(
            error.contains("Timeout"),
            "hung probe failed outside the default timeout boundary: {error}"
        );
        let pid = std::fs::read_to_string(&pid_path)
            .expect("probe pid")
            .parse::<i32>()
            .expect("numeric probe pid");
        assert!(
            !process_exists(pid),
            "timed-out probe process was not terminated and reaped"
        );
        assert_child_was_reaped(pid, "timed-out Codex probe");
    }

    #[tokio::test]
    async fn cancelling_helper_start_before_readiness_still_reaps_the_child() {
        let sandbox = TestSandbox::new("codex-cancelled-start");
        let pid_path = sandbox.path("helper.pid");
        let socket_path = sandbox.path("never-created.sock");
        let executable = sandbox.executable_script("unready-helper", "exec sleep 3600", "");
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());
        let events = CodexHelperFixtureEvents {
            spawned: Arc::new(FixtureEvent::default()),
            admission_race: None,
            ready: Arc::new(FixtureEvent::default()),
            reaped: Arc::new(FixtureEvent::default()),
            pid_path: pid_path.clone(),
            watchdog_spawn_race: None,
        };
        let launcher = SystemCodexHelperLauncher::with_fixture_events(
            Duration::from_millis(100),
            events.clone(),
        );
        let launch = CodexHelperLaunch {
            executable: executable.to_string_lossy().into_owned(),
            args: vec![
                "app-server".to_owned(),
                "--listen".to_owned(),
                endpoint.clone(),
            ],
            cwd: sandbox.root().to_path_buf(),
            env: sandbox.environment(std::iter::empty::<(String, String)>()),
            endpoint,
            socket_path,
            process_attribution: ProcessAttributionRegistry::new(),
        };
        let start = tokio::spawn(async move { launcher.start(launch).await });
        tokio::time::timeout(Duration::from_secs(10), events.spawned.wait_after(0))
            .await
            .expect("Codex helper PID publication outer watchdog");
        let pid = read_fixture_pid(&pid_path).expect("Codex helper PID");
        start.abort();
        let _ = start.await;
        tokio::time::timeout(Duration::from_secs(10), events.reaped.wait_after(0))
            .await
            .expect("cancelled Codex helper reap outer watchdog");
        assert!(
            !process_exists(pid),
            "cancelled helper start did not retain ownership through reap"
        );
    }

    #[tokio::test]
    async fn helper_supervisor_refuses_saturation_before_process_admission() {
        let supervisor = CodexHelperSupervisor::with_runtime(tokio::runtime::Handle::current(), 1);
        let permit = supervisor.reserve().expect("first helper slot");

        assert!(
            supervisor.reserve().is_none(),
            "a saturated helper supervisor must refuse admission before spawning a child"
        );

        drop(permit);
        assert!(
            supervisor.reserve().is_some(),
            "releasing the owned helper slot must restore bounded capacity"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn helper_task_registry_drain_observes_the_last_concurrent_owner_drop() {
        let registry = Arc::new(CodexHelperTaskRegistry::default());
        let guard = registry.register();
        let drain_registry = registry.clone();
        let drain = tokio::spawn(async move { drain_registry.drain().await });

        tokio::task::yield_now().await;
        assert!(
            !drain.is_finished(),
            "drain must wait for an active helper owner"
        );
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), drain)
            .await
            .expect("last helper owner wakes drain")
            .expect("drain task");
    }

    #[tokio::test]
    async fn helper_supervisor_initialization_is_bounded_and_late_failure_exits() {
        let (exited_sender, exited_receiver) = std::sync::mpsc::sync_channel(1);
        let initializer =
            CodexHelperSupervisorInitializer::start_with(1, Duration::from_millis(50), move || {
                std::thread::sleep(Duration::from_millis(150));
                let _ = exited_sender.send(());
                Err("delayed initialization failure".to_owned())
            });
        let started = std::time::Instant::now();

        assert!(
            initializer.wait_ready().await.is_none(),
            "late helper initialization must fail open"
        );
        assert!(
            started.elapsed() < CODEX_PREPARATION_BUDGET,
            "helper initialization blocked the fixed terminal preparation budget"
        );
        assert!(
            exited_receiver
                .recv_timeout(Duration::from_millis(500))
                .is_ok(),
            "late failed initialization thread did not exit cleanly"
        );
    }

    #[tokio::test]
    async fn failed_helper_supervisor_initialization_does_not_poison_future_initializers() {
        let failed =
            CodexHelperSupervisorInitializer::start_with(1, Duration::from_millis(50), || {
                Err("injected initialization failure".to_owned())
            });
        assert!(failed.wait_ready().await.is_none());

        let ready =
            CodexHelperSupervisorInitializer::start_with(1, Duration::from_millis(100), || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
            });
        assert!(
            ready.wait_ready().await.is_some(),
            "one failed initializer must not poison later initialization"
        );
    }

    #[tokio::test]
    async fn late_successful_helper_initialization_is_ready_for_a_later_launch_and_reaped() {
        struct RuntimeExitSignal(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for RuntimeExitSignal {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(0);
        let (constructed_sender, mut constructed_receiver) = tokio::sync::oneshot::channel();
        let (task_started_sender, task_started_receiver) = tokio::sync::oneshot::channel();
        let (runtime_exited_sender, runtime_exited_receiver) = tokio::sync::oneshot::channel();
        let initializer =
            CodexHelperSupervisorInitializer::start_with(1, Duration::from_millis(50), move || {
                release_receiver
                    .recv()
                    .map_err(|_| "late initialization release was dropped".to_owned())?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.spawn(async move {
                    let _exit_signal = RuntimeExitSignal(Some(runtime_exited_sender));
                    let _ = task_started_sender.send(());
                    std::future::pending::<()>().await;
                });
                let _ = constructed_sender.send(());
                Ok(runtime)
            });
        let first_started = std::time::Instant::now();

        assert!(
            initializer.wait_ready().await.is_none(),
            "the launch waiting on cold initialization must fail open at its fixed bound"
        );
        assert!(
            first_started.elapsed() < CODEX_PREPARATION_BUDGET,
            "cold initialization blocked the terminal preparation budget"
        );
        assert!(
            constructed_receiver.try_recv().is_err(),
            "no helper runtime may be constructed before initialization is released"
        );
        release_sender
            .send(())
            .expect("release late successful initialization");
        tokio::time::timeout(Duration::from_millis(500), constructed_receiver)
            .await
            .expect("late runtime construction completed")
            .expect("late runtime construction signal");

        let supervisor = initializer
            .wait_ready()
            .await
            .expect("late successful initialization must serve a later launch");
        let permit = supervisor
            .reserve()
            .expect("no helper capacity is admitted before the supervisor is ready");
        drop(permit);
        tokio::time::timeout(Duration::from_millis(500), task_started_receiver)
            .await
            .expect("supervisor runtime task started")
            .expect("supervisor runtime task start signal");

        drop(supervisor);
        drop(initializer);
        tokio::time::timeout(Duration::from_millis(500), runtime_exited_receiver)
            .await
            .expect("dropping the initializer stopped its runtime thread")
            .expect("supervisor runtime exit signal");
    }

    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn system_remote_client_uses_websocket_rpc_over_a_real_unix_socket() {
        let root = tempfile::tempdir().expect("UDS directory");
        let socket_path = root.path().join("app-server.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind UDS");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept UDS client");
            let mut socket =
                tokio_tungstenite::accept_hdr_async(stream, |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                    assert_eq!(request.uri().path(), "/rpc");
                    Ok(response)
                })
                .await
                .expect("accept WebSocket");
            let request = socket
                .next()
                .await
                .expect("request frame")
                .expect("request message")
                .into_text()
                .expect("text request");
            let request: Value = serde_json::from_str(&request).expect("request JSON");
            assert_eq!(request["method"], "initialize");
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "id": request["id"],
                        "result": {"accepted": true},
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("response");
            let notification = socket
                .next()
                .await
                .expect("notification frame")
                .expect("notification message")
                .into_text()
                .expect("text notification");
            let notification: Value =
                serde_json::from_str(&notification).expect("notification JSON");
            assert_eq!(notification["method"], "initialized");
        });
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());

        let mut client = SystemCodexRemoteClientFactory
            .connect(&endpoint)
            .await
            .expect("connect real UDS");
        let response = client
            .request("initialize", serde_json::json!({"clientInfo": {}}))
            .await
            .expect("initialize response");
        assert_eq!(response, serde_json::json!({"accepted": true}));
        client.notify("initialized").await.expect("initialized");
        server.await.expect("UDS server");
    }

    #[tokio::test]
    async fn system_remote_client_rejects_oversized_messages_before_json_decode() {
        let root = tempfile::tempdir().expect("UDS directory");
        let socket_path = root.path().join("oversized-app-server.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind UDS");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept UDS client");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept WebSocket");
            let request = socket
                .next()
                .await
                .expect("request frame")
                .expect("request message")
                .into_text()
                .expect("text request");
            let request: Value = serde_json::from_str(&request).expect("request JSON");
            let _ = socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::json!({
                        "id": request["id"],
                        "result": {
                            "payload": "x".repeat(CODEX_REMOTE_MESSAGE_MAX_BYTES)
                        },
                    })
                    .to_string()
                    .into(),
                ))
                .await;
        });
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());
        let mut client = SystemCodexRemoteClientFactory
            .connect(&endpoint)
            .await
            .expect("connect real UDS");

        let result = client.request("thread/read", json!({})).await;

        assert!(
            result.is_err(),
            "oversized WebSocket messages must fail before serde allocates their JSON graph"
        );
        drop(client);
        server.await.expect("UDS server");
    }

    #[tokio::test]
    async fn system_remote_client_discards_late_responses_and_bounds_queued_notifications() {
        let root = tempfile::tempdir().expect("UDS directory");
        let socket_path = root.path().join("late-response-app-server.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind UDS");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept UDS client");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept WebSocket");
            let request_a = socket
                .next()
                .await
                .expect("request A frame")
                .expect("request A message")
                .into_text()
                .expect("request A text");
            let request_a: Value = serde_json::from_str(&request_a).expect("request A JSON");
            let request_b = socket
                .next()
                .await
                .expect("request B frame")
                .expect("request B message")
                .into_text()
                .expect("request B text");
            let request_b: Value = serde_json::from_str(&request_b).expect("request B JSON");

            for sequence in 0..=CODEX_REMOTE_PENDING_NOTIFICATION_LIMIT {
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        json!({
                            "method": "fixture/event",
                            "params": {"sequence": sequence},
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .expect("queued notification");
            }
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({
                        "id": request_a["id"],
                        "result": {"request": "A"},
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("late response A");
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({
                        "id": request_b["id"],
                        "result": {"request": "B"},
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .expect("response B");
            socket
                .send(tokio_tungstenite::tungstenite::Message::Close(None))
                .await
                .expect("close frame");
        });
        let endpoint = format!("unix://{}", socket_path.to_string_lossy());
        let mut client = SystemCodexRemoteClientFactory
            .connect(&endpoint)
            .await
            .expect("connect real UDS");

        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                client.request("fixture/request-a", json!({})),
            )
            .await
            .is_err(),
            "request A must time out before its late response"
        );
        let response_b = client
            .request("fixture/request-b", json!({}))
            .await
            .expect("request B response");
        assert_eq!(response_b, json!({"request": "B"}));

        let mut sequences = Vec::new();
        while let Some(notification) = client.next().await.expect("queued notification") {
            assert_eq!(notification["method"], "fixture/event");
            sequences.push(
                notification["params"]["sequence"]
                    .as_u64()
                    .expect("notification sequence"),
            );
        }
        assert_eq!(
            sequences.len(),
            CODEX_REMOTE_PENDING_NOTIFICATION_LIMIT,
            "notification queue must retain a fixed maximum"
        );
        assert_eq!(sequences.first(), Some(&1));
        assert_eq!(
            sequences.last(),
            Some(
                &u64::try_from(CODEX_REMOTE_PENDING_NOTIFICATION_LIMIT).expect("small queue limit")
            )
        );
        server.await.expect("UDS server");
    }

    fn process_exists(pid: i32) -> bool {
        // SAFETY: kill with signal 0 performs an existence/permission check
        // without delivering a signal.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn assert_child_was_reaped(pid: i32, label: &str) {
        let mut status = 0;
        // SAFETY: `status` is valid writable storage and WNOHANG does not block.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let error = std::io::Error::last_os_error();
        assert_eq!(
            result, -1,
            "{label} PID {pid} remained waitable after its owner returned"
        );
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ECHILD),
            "{label} PID {pid} was absent but not positively reaped by its owner"
        );
    }
}
