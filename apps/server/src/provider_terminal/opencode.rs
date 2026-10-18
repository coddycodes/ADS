use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

use base64::Engine as _;
use futures_util::FutureExt as _;
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinHandle,
};

#[cfg(test)]
use super::TerminalObserverGeneration;
use super::model::TerminalObserverGenerationLease;
use super::{
    NativeTerminalCommand, PreparedTerminalLaunch, PreparedTerminalObserver,
    ProviderTerminalObserverFactory, ProviderTerminalObserverFactoryInput,
    TerminalAgentActivityAdmission, TerminalAgentActivityControl, TerminalAgentActivityObservation,
    TerminalAgentActivityObservationKind, TerminalAgentActivityState,
    TerminalAgentActivityTransition, TerminalGenerationActivityPublisher,
    TerminalObserverWorkerContext,
};
#[cfg(not(windows))]
use crate::process::supervised::spawn_retrying_busy_executable;
#[cfg(test)]
use crate::provider::opencode::sse::OPENCODE_SSE_EVENT_LIMIT;
use crate::{
    activity::{ActivityCapabilities, ActivityHistoryRecovery, ProviderActivityMutation},
    diagnostics::{
        AttributionKind, AttributionScope, NativeProcessSampler, ProcessAttributionRegistry,
        ProcessRegistration, ProcessRegistrationError, ProcessRegistrationMetadata,
        RegistrationSource,
    },
    process::{
        Platform, configure_supervised_background_command_wrap,
        supervised::{SupervisedOverflow, SupervisedRunRequest, run_supervised},
    },
    provider::opencode::sse::OpenCodeSseDecoder,
    provider::opencode::{
        self,
        activity::{MAX_RECONCILED_CHILDREN, OpenCodeActivityOutput, OpenCodeActivityTracker},
    },
};

const OPENCODE_PROBE_OUTPUT_LIMIT: usize = 128 * 1024;
const OPENCODE_CAPABILITY_CACHE_CAPACITY: usize = 64;
const OPENCODE_CONFIG_CONTENT_LIMIT: usize = 64 * 1024;
const OPENCODE_HTTP_BODY_LIMIT: usize = 1024 * 1024;
const OPENCODE_HELPER_READY_TIMEOUT: Duration = Duration::from_secs(3);
const OPENCODE_HELPER_TERM_GRACE: Duration = Duration::from_millis(100);
const OPENCODE_HELPER_REAP_TIMEOUT: Duration = Duration::from_millis(100);
const OPENCODE_HELPER_WAIT_RETRY_DELAY: Duration = Duration::from_millis(100);
const OPENCODE_HELPER_REAPER_CAPACITY: usize = 16;
#[cfg(unix)]
const OPENCODE_WAITID_EINTR_RETRY_LIMIT: usize = 8;
const OPENCODE_PRE_SPAWN_DELETE_TIMEOUT: Duration = Duration::from_millis(100);
const OPENCODE_PREPARATION_BUDGET: Duration = Duration::from_millis(850);
const OPENCODE_REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
const OPENCODE_OBSERVATION_ACK_GRACE: Duration = Duration::from_millis(100);
const OPENCODE_REATTACH_REQUEST_LIMIT: u32 = (MAX_RECONCILED_CHILDREN as u32) * 2 + 2;
const OPENCODE_RECONCILIATION_INTERVAL: Duration = Duration::from_secs(2);
const OPENCODE_SSE_RETRY_MIN: Duration = Duration::from_millis(100);
const OPENCODE_SSE_RETRY_MAX: Duration = Duration::from_secs(2);
const OPENCODE_USERNAME: &str = "opencode";
const OPENCODE_CLEANUP_IDLE: u8 = 0;
const OPENCODE_CLEANUP_IN_PROGRESS: u8 = 1;
const OPENCODE_CLEANUP_REAPED: u8 = 2;
const OPENCODE_CLEANUP_FALLBACK_TERMINATED: u8 = 3;

fn complete_opencode_reattach_timeout(handshake_timeout: Duration) -> Duration {
    handshake_timeout
        .saturating_mul(2)
        .saturating_add(OPENCODE_REQUEST_TIMEOUT.saturating_mul(OPENCODE_REATTACH_REQUEST_LIMIT))
}

#[derive(Clone, Eq, PartialEq)]
pub struct OpenCodeProbeOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl fmt::Debug for OpenCodeProbeOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeProbeOutput")
            .field("success", &self.success)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

pub trait OpenCodeCapabilityProbeRunner: Send + Sync {
    fn run(
        &self,
        executable: &Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeProbeOutput, String>> + Send + '_>>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeCapabilities {
    pub serve: bool,
    /// OpenCode 1: `opencode attach <url> --dir <dir> --session <id>`.
    pub attach: bool,
    /// OpenCode 2: `opencode --server <url> --session <id> <dir>`.
    pub server_session: bool,
}

pub struct CachedOpenCodeCapabilityProbe {
    runner: Arc<dyn OpenCodeCapabilityProbeRunner>,
    cache: tokio::sync::Mutex<OpenCodeCapabilityCache>,
}

impl fmt::Debug for CachedOpenCodeCapabilityProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedOpenCodeCapabilityProbe")
            .finish_non_exhaustive()
    }
}

impl CachedOpenCodeCapabilityProbe {
    #[must_use]
    pub fn new(runner: Arc<dyn OpenCodeCapabilityProbeRunner>) -> Self {
        Self {
            runner,
            cache: tokio::sync::Mutex::new(OpenCodeCapabilityCache::default()),
        }
    }

    pub async fn probe(&self, executable: &Path) -> Option<OpenCodeCapabilities> {
        let path = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
        let fingerprint = OpenCodeExecutableFingerprint::read(&path);
        let mut cache = self.cache.lock().await;
        if let Some(capabilities) = fingerprint
            .as_ref()
            .and_then(|fingerprint| cache.get(fingerprint))
        {
            return Some(capabilities);
        }
        let capabilities = self.probe_uncached(&path).await;
        if OpenCodeExecutableFingerprint::read(&path) != fingerprint {
            return None;
        }
        if let (Some(fingerprint), Some(capabilities)) = (fingerprint, &capabilities) {
            cache.insert(fingerprint, capabilities.clone());
        }
        capabilities
    }

    async fn probe_uncached(&self, executable: &Path) -> Option<OpenCodeCapabilities> {
        let attach = self
            .runner
            .run(executable, vec!["attach".to_owned(), "--help".to_owned()])
            .await
            .ok()?;
        if !attach.success {
            return None;
        }
        let attach_help = bounded_probe_text(&attach.stdout, &attach.stderr);
        let attach = attach_help.contains("opencode attach <url>")
            && attach_help.contains("--dir")
            && attach_help.contains("--session");
        Some(OpenCodeCapabilities {
            // `serve` is proven by the concurrently launched helper reaching
            // its post-bind readiness line and authenticated health endpoint.
            serve: true,
            attach,
            // OpenCode 2 has no `attach`, so it prints its root help, whose `--server` and
            // `--session` flags open a session of a running server.
            server_session: !attach
                && attach_help.contains("--server")
                && attach_help.contains("--session"),
        })
    }
}

#[derive(Debug, Default)]
struct OpenCodeCapabilityCache {
    entries: HashMap<OpenCodeExecutableFingerprint, OpenCodeCapabilities>,
    recency: VecDeque<OpenCodeExecutableFingerprint>,
}

impl OpenCodeCapabilityCache {
    fn get(&mut self, fingerprint: &OpenCodeExecutableFingerprint) -> Option<OpenCodeCapabilities> {
        let capabilities = self.entries.get(fingerprint)?.clone();
        self.recency.retain(|cached| cached != fingerprint);
        self.recency.push_back(fingerprint.clone());
        Some(capabilities)
    }

    fn insert(
        &mut self,
        fingerprint: OpenCodeExecutableFingerprint,
        capabilities: OpenCodeCapabilities,
    ) {
        self.entries
            .retain(|cached, _| cached.path != fingerprint.path);
        self.recency
            .retain(|cached| cached.path != fingerprint.path);
        while self.entries.len() >= OPENCODE_CAPABILITY_CACHE_CAPACITY {
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
struct OpenCodeExecutableFingerprint {
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

impl OpenCodeExecutableFingerprint {
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
    let mut output = String::new();
    for value in [stdout, stderr] {
        let remaining = OPENCODE_PROBE_OUTPUT_LIMIT.saturating_sub(output.len());
        if remaining == 0 {
            break;
        }
        let end = value
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= remaining)
            .last()
            .unwrap_or_default();
        output.push_str(if value.len() <= remaining {
            value
        } else {
            &value[..end]
        });
        output.push('\n');
    }
    output
}

fn parse_opencode_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|part| {
            let components = part.split('.').collect::<Vec<_>>();
            components.len() == 3
                && components.iter().all(|component| {
                    !component.is_empty()
                        && component
                            .chars()
                            .all(|character| character.is_ascii_digit())
                        && (component.len() == 1 || !component.starts_with('0'))
                })
        })
        .map(str::to_owned)
}

#[derive(Clone)]
pub struct OpenCodeHelperLaunch {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub process_attribution: ProcessAttributionRegistry,
}

impl fmt::Debug for OpenCodeHelperLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeHelperLaunch")
            .field("executable", &self.executable)
            .field("arg_count", &self.args.len())
            .field("cwd", &self.cwd)
            .field("env_keys", &self.env.keys())
            .finish()
    }
}

pub trait OpenCodeHelperProcess: Send + Sync + fmt::Debug {
    /// Returns true when the owned helper has already exited or its state can
    /// no longer be observed safely.
    fn has_exited(&self) -> bool {
        false
    }

    /// Emergency signal-only fallback for cancellation during synchronous
    /// teardown or process drop.
    fn terminate(&self);

    fn terminate_and_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.terminate();
        Box::pin(std::future::ready(()))
    }
}

struct OpenCodeHelperGuard {
    process: Option<Arc<dyn OpenCodeHelperProcess>>,
}

impl OpenCodeHelperGuard {
    fn new(process: Arc<dyn OpenCodeHelperProcess>) -> Self {
        Self {
            process: Some(process),
        }
    }

    fn into_process(mut self) -> Arc<dyn OpenCodeHelperProcess> {
        self.process
            .take()
            .expect("OpenCode helper guard owns a process")
    }
}

impl Drop for OpenCodeHelperGuard {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            process.terminate();
        }
    }
}

pub struct OpenCodeHelperReady {
    pub endpoint: String,
    pub process: Arc<dyn OpenCodeHelperProcess>,
}

impl fmt::Debug for OpenCodeHelperReady {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeHelperReady")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

pub trait OpenCodeHelperLauncher: Send + Sync + fmt::Debug {
    fn start(
        &self,
        launch: OpenCodeHelperLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeHelperReady, String>> + Send + '_>>;

    #[doc(hidden)]
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

pub trait OpenCodeEventStream: Send {
    fn discard_next(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn next_data(&mut self) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>>;
}

pub trait OpenCodeRemoteClient: Send {
    fn create_root(
        &mut self,
        model: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>>;

    fn cleanup_pre_spawn(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn abort(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>;

    fn root(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>>;

    fn statuses(&mut self) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>>;

    fn children(
        &mut self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>>;

    fn messages(
        &mut self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>>;

    fn open_event_stream(&mut self) -> OpenCodeEventStreamFuture<'_>;
}

type OpenCodeEventStreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn OpenCodeEventStream>, String>> + Send + 'a>>;
type OpenCodeRemoteClientFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn OpenCodeRemoteClient>, String>> + Send + 'a>>;

pub trait OpenCodeRemoteClientFactory: Send + Sync + fmt::Debug {
    fn connect(
        &self,
        endpoint: &str,
        username: &str,
        password: &str,
        directory: &Path,
    ) -> OpenCodeRemoteClientFuture<'_>;
}

pub struct OpenCodeTerminalObserverFactory {
    probe: Arc<CachedOpenCodeCapabilityProbe>,
    helper: Arc<dyn OpenCodeHelperLauncher>,
    remote: Arc<dyn OpenCodeRemoteClientFactory>,
    handshake_timeout: Duration,
    reattach_timeout: Duration,
}

impl fmt::Debug for OpenCodeTerminalObserverFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeTerminalObserverFactory")
            .field("handshake_timeout", &self.handshake_timeout)
            .field("reattach_timeout", &self.reattach_timeout)
            .finish_non_exhaustive()
    }
}

impl OpenCodeTerminalObserverFactory {
    #[must_use]
    pub fn new(
        probe: Arc<CachedOpenCodeCapabilityProbe>,
        helper: Arc<dyn OpenCodeHelperLauncher>,
        remote: Arc<dyn OpenCodeRemoteClientFactory>,
        handshake_timeout: Duration,
    ) -> Self {
        let reattach_timeout = complete_opencode_reattach_timeout(handshake_timeout);
        Self::new_with_reattach_timeout(probe, helper, remote, handshake_timeout, reattach_timeout)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn new_with_reattach_timeout(
        probe: Arc<CachedOpenCodeCapabilityProbe>,
        helper: Arc<dyn OpenCodeHelperLauncher>,
        remote: Arc<dyn OpenCodeRemoteClientFactory>,
        handshake_timeout: Duration,
        reattach_timeout: Duration,
    ) -> Self {
        Self {
            probe,
            helper,
            remote,
            handshake_timeout,
            reattach_timeout,
        }
    }

    #[must_use]
    pub fn system() -> Self {
        Self::new(
            Arc::new(CachedOpenCodeCapabilityProbe::new(Arc::new(
                SystemOpenCodeCapabilityProbeRunner,
            ))),
            Arc::new(SystemOpenCodeHelperLauncher::default()),
            Arc::new(SystemOpenCodeRemoteClientFactory),
            Duration::from_secs(10),
        )
    }

    async fn prepare_inner(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Option<PreparedTerminalLaunch> {
        tokio::time::timeout(OPENCODE_PREPARATION_BUDGET, self.prepare_bounded(input))
            .await
            .ok()
            .flatten()
    }

    async fn prepare_bounded(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Option<PreparedTerminalLaunch> {
        let model = opencode_model_arg(&input.launch.args).or_else(|| {
            standalone_model(
                &input.launch.args,
                input.launch.launch_env.get("OPENCODE_CONFIG_CONTENT"),
            )
        })?;
        validate_opencode_config(input.launch.launch_env.get("OPENCODE_CONFIG_CONTENT"))?;
        let executable = opencode::resolve_owned_executable(
            Platform::current(),
            Path::new(&input.launch.executable),
        )
        .to_string_lossy()
        .into_owned();
        let password = random_secret()?;
        let mut helper_env = input.launch.launch_env.clone();
        helper_env.insert(
            "OPENCODE_SERVER_USERNAME".to_owned(),
            OPENCODE_USERNAME.to_owned(),
        );
        helper_env.insert("OPENCODE_SERVER_PASSWORD".to_owned(), password.clone());
        // OpenCode 2 prefers an inherited `OPENCODE_PASSWORD` to `OPENCODE_SERVER_PASSWORD`.
        helper_env.insert("OPENCODE_PASSWORD".to_owned(), password.clone());
        let helper_launch = OpenCodeHelperLaunch {
            executable: executable.clone(),
            args: vec![
                "serve".to_owned(),
                "--hostname".to_owned(),
                "127.0.0.1".to_owned(),
                "--port".to_owned(),
                "0".to_owned(),
                // Both majors log at INFO by default; OpenCode 1 spells the flag value `INFO` and
                // OpenCode 2 `info`, and the version is not known before the helper starts.
                "--print-logs".to_owned(),
            ],
            cwd: input.launch.cwd.clone(),
            env: helper_env,
            process_attribution: input.process_attribution.clone(),
        };
        let cleanup_generation = input.launch.generation.observation();
        let cleanup_workers = input.launch.generation.worker_context();
        let helper = async {
            let ready = self.helper.start(helper_launch).await?;
            let resources = Arc::new(OpenCodeObserverResources {
                helper: OpenCodeHelperGuard::new(ready.process).into_process(),
                owned_root: Mutex::new(None),
                launched: AtomicBool::new(false),
                cleanup_state: AtomicU8::new(OPENCODE_CLEANUP_IDLE),
                ownership_transferred: tokio::sync::Notify::new(),
            });
            let cleanup_resources = resources.clone();
            let worker_generation = cleanup_generation.clone();
            if cleanup_workers
                .spawn(async move {
                    monitor_opencode_pre_spawn(cleanup_resources, worker_generation).await;
                })
                .is_err()
            {
                cleanup_opencode_pre_spawn(resources).await;
                return Err("OpenCode pre-launch cleanup worker was unavailable".to_owned());
            }
            Ok((ready.endpoint, resources))
        };
        let (capabilities, ready) = tokio::join!(self.probe.probe(Path::new(&executable)), helper,);
        let (endpoint, resources) = ready.ok()?;
        let capabilities = capabilities?;
        if !capabilities.serve || !(capabilities.attach || capabilities.server_session) {
            return None;
        };
        if !valid_loopback_endpoint(&endpoint) {
            return None;
        }
        let mut remote = match self
            .remote
            .connect(&endpoint, OPENCODE_USERNAME, &password, &input.launch.cwd)
            .await
        {
            Ok(remote) => remote,
            Err(_) => return None,
        };
        let root_session_id = match remote.create_root(&model).await {
            Ok(root_session_id) if valid_native_id(&root_session_id) => root_session_id,
            _ => return None,
        };
        *resources
            .owned_root
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(OpenCodeOwnedRoot {
            remote,
            root_session_id: root_session_id.clone(),
        });
        if resources.helper.has_exited() {
            cleanup_opencode_pre_spawn(resources).await;
            return None;
        }
        let observer = OpenCodePreparedTerminalObserver {
            inner: Arc::new(OpenCodeObserverInner {
                resources,
                endpoint: endpoint.clone(),
                password: password.clone(),
                directory: input.launch.cwd.clone(),
                remote: self.remote.clone(),
                root_session_id: root_session_id.clone(),
                publisher: input.activity_publisher,
                provider_instance_id: input.launch.activity.provider_instance_id,
                handshake_timeout: self.handshake_timeout,
                reattach_timeout: self.reattach_timeout,
                spawned: AtomicBool::new(false),
                activity: Arc::new(TerminalAgentActivityControl::enabled()),
            }),
        };
        let directory = input.launch.cwd.to_string_lossy().into_owned();
        let (args, private_env) = if capabilities.attach {
            (
                vec![
                    "attach".to_owned(),
                    endpoint,
                    "--dir".to_owned(),
                    directory,
                    "--session".to_owned(),
                    root_session_id,
                ],
                BTreeMap::from([
                    (
                        "OPENCODE_SERVER_USERNAME".to_owned(),
                        OPENCODE_USERNAME.to_owned(),
                    ),
                    ("OPENCODE_SERVER_PASSWORD".to_owned(), password),
                ]),
            )
        } else {
            // OpenCode 2's client reads the server password from `OPENCODE_PASSWORD`.
            (
                vec![
                    "--server".to_owned(),
                    endpoint,
                    "--session".to_owned(),
                    root_session_id,
                    directory,
                ],
                BTreeMap::from([("OPENCODE_PASSWORD".to_owned(), password)]),
            )
        };
        Some(PreparedTerminalLaunch {
            executable,
            args,
            private_env,
            observer: Box::new(observer),
        })
    }
}

impl ProviderTerminalObserverFactory for OpenCodeTerminalObserverFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(self.prepare_inner(input))
    }

    /// OpenCode 2 has no `--model`: the requested `--model <model>` becomes a private server
    /// (`--standalone`) whose configuration (`OPENCODE_CONFIG_CONTENT`) selects the model.
    /// OpenCode 1 keeps the command as requested.
    fn native_command(
        &self,
        executable: &Path,
        args: &[String],
        launch_env: &BTreeMap<String, String>,
    ) -> Pin<Box<dyn Future<Output = Option<NativeTerminalCommand>> + Send + '_>> {
        let executable = opencode::resolve_owned_executable(Platform::current(), executable);
        let model = opencode_model_arg(args);
        let config = launch_env.get("OPENCODE_CONFIG_CONTENT").cloned();
        Box::pin(async move {
            let model = model?;
            validate_opencode_config(config.as_ref())?;
            if !self.probe.probe(&executable).await?.server_session {
                return None;
            }
            let mut config = config
                .and_then(|config| serde_json::from_str::<Value>(&config).ok())
                .unwrap_or_else(|| json!({}));
            config["model"] = json!(model);
            Some(NativeTerminalCommand {
                args: vec!["--standalone".to_owned()],
                env: BTreeMap::from([("OPENCODE_CONFIG_CONTENT".to_owned(), config.to_string())]),
            })
        })
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.helper.shutdown()
    }
}

fn opencode_model_arg(args: &[String]) -> Option<String> {
    match args {
        [flag, model] if matches!(flag.as_str(), "--model" | "-m") && valid_model(model) => {
            Some(model.clone())
        }
        [argument] => argument
            .strip_prefix("--model=")
            .filter(|model| valid_model(model))
            .map(str::to_owned),
        _ => None,
    }
}

/// The model of an OpenCode 2 command in its native form (`--standalone`, the model in the
/// configuration).
fn standalone_model(args: &[String], config: Option<&String>) -> Option<String> {
    let [flag] = args else {
        return None;
    };
    if flag != "--standalone" {
        return None;
    }
    serde_json::from_str::<Value>(config?)
        .ok()?
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| valid_model(model))
        .map(str::to_owned)
}

fn valid_model(model: &str) -> bool {
    model.len() <= 256
        && !model.chars().any(char::is_control)
        && model
            .split_once('/')
            .is_some_and(|(provider, id)| !provider.is_empty() && !id.is_empty())
}

fn validate_opencode_config(existing: Option<&String>) -> Option<()> {
    match existing {
        None => Some(()),
        Some(existing) if existing.len() <= OPENCODE_CONFIG_CONTENT_LIMIT => {
            serde_json::from_str::<Value>(existing).ok()?.as_object()?;
            Some(())
        }
        Some(_) => None,
    }
}

fn valid_native_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn valid_loopback_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some_and(|port| port != 0)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none()
}

fn random_secret() -> Option<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug)]
struct OpenCodePreparedTerminalObserver {
    inner: Arc<OpenCodeObserverInner>,
}

impl PreparedTerminalObserver for OpenCodePreparedTerminalObserver {
    fn is_ready_for_on_spawned(&self) -> bool {
        !self.inner.resources.helper.has_exited()
    }

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
        if workers
            .spawn(async move {
                run_opencode_observer(inner, generation).await;
            })
            .is_ok()
        {
            self.inner.resources.transfer_ownership();
        }
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) -> Pin<Box<dyn Future<Output = TerminalAgentActivityTransition> + Send + '_>> {
        Box::pin(async move {
            let (mut transition, observed_epoch) = self
                .inner
                .activity
                .transition_observed_with_epoch(
                    enabled,
                    self.inner
                        .reattach_timeout
                        .saturating_add(OPENCODE_OBSERVATION_ACK_GRACE),
                )
                .await;
            transition.epochs.opencode = observed_epoch.unwrap_or_default();
            transition
        })
    }

    fn agent_activity_enable_ack_timeout(&self) -> Option<Duration> {
        Some(self.inner.reattach_timeout)
    }

    fn diagnostic_label(&self) -> &str {
        "opencode-authenticated-serve-attach"
    }
}

struct OpenCodeObserverInner {
    resources: Arc<OpenCodeObserverResources>,
    endpoint: String,
    password: String,
    directory: PathBuf,
    remote: Arc<dyn OpenCodeRemoteClientFactory>,
    root_session_id: String,
    publisher: TerminalGenerationActivityPublisher,
    provider_instance_id: String,
    handshake_timeout: Duration,
    reattach_timeout: Duration,
    spawned: AtomicBool,
    activity: Arc<TerminalAgentActivityControl>,
}

impl fmt::Debug for OpenCodeObserverInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenCodeObserverInner")
            .field("scope_id", &self.publisher.scope_id())
            .finish_non_exhaustive()
    }
}

struct OpenCodeObserverResources {
    helper: Arc<dyn OpenCodeHelperProcess>,
    owned_root: Mutex<Option<OpenCodeOwnedRoot>>,
    launched: AtomicBool,
    cleanup_state: AtomicU8,
    ownership_transferred: tokio::sync::Notify,
}

struct OpenCodeOwnedRoot {
    remote: Box<dyn OpenCodeRemoteClient>,
    root_session_id: String,
}

impl OpenCodeObserverResources {
    fn transfer_ownership(&self) {
        self.launched.store(true, Ordering::Release);
        self.ownership_transferred.notify_one();
    }

    async fn cleanup_helper(&self) {
        let Some(cleanup) = self.begin_cleanup() else {
            return;
        };
        self.helper.terminate_and_reap().await;
        cleanup.complete();
    }

    fn begin_cleanup(&self) -> Option<OpenCodeCleanupClaim<'_>> {
        self.cleanup_state
            .compare_exchange(
                OPENCODE_CLEANUP_IDLE,
                OPENCODE_CLEANUP_IN_PROGRESS,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| OpenCodeCleanupClaim {
                resources: self,
                completed: false,
            })
    }
}

impl Drop for OpenCodeObserverResources {
    fn drop(&mut self) {
        if self
            .cleanup_state
            .compare_exchange(
                OPENCODE_CLEANUP_IDLE,
                OPENCODE_CLEANUP_FALLBACK_TERMINATED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.helper.terminate();
        }
    }
}

struct OpenCodeCleanupClaim<'a> {
    resources: &'a OpenCodeObserverResources,
    completed: bool,
}

impl OpenCodeCleanupClaim<'_> {
    fn complete(mut self) {
        self.resources
            .cleanup_state
            .store(OPENCODE_CLEANUP_REAPED, Ordering::Release);
        self.completed = true;
    }
}

impl Drop for OpenCodeCleanupClaim<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        self.resources.helper.terminate();
        self.resources
            .cleanup_state
            .store(OPENCODE_CLEANUP_FALLBACK_TERMINATED, Ordering::Release);
    }
}

async fn monitor_opencode_pre_spawn(
    resources: Arc<OpenCodeObserverResources>,
    generation: TerminalObserverGenerationLease,
) {
    if resources.launched.load(Ordering::Acquire) {
        return;
    }
    tokio::select! {
        _ = resources.ownership_transferred.notified() => {}
        _ = generation.cancelled() => {
            if !resources.launched.load(Ordering::Acquire) {
                cleanup_opencode_pre_spawn(resources).await;
            }
        }
    }
}

async fn cleanup_opencode_pre_spawn(resources: Arc<OpenCodeObserverResources>) {
    let Some(cleanup) = resources.begin_cleanup() else {
        return;
    };
    let owned_root = resources
        .owned_root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(mut owned_root) = owned_root {
        let _ = tokio::time::timeout(
            OPENCODE_PRE_SPAWN_DELETE_TIMEOUT,
            owned_root
                .remote
                .cleanup_pre_spawn(&owned_root.root_session_id),
        )
        .await;
    }
    resources.helper.terminate_and_reap().await;
    cleanup.complete();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpenCodeObserverMode {
    Live,
    Dormant,
    Unavailable,
}

enum OpenCodeReplacementOutcome {
    Connected(Box<dyn OpenCodeEventStream>),
    Failed { dormant_stream_healthy: bool },
    Superseded { dormant_stream_healthy: bool },
    Cancelled,
}

enum OpenCodeObserverEvent {
    Cancelled,
    ActivityChanged(bool),
    Reconcile,
    Data(Result<Vec<u8>, String>),
    Discard(Result<(), String>),
}

enum OpenCodeLiveWork<T> {
    Cancelled,
    ActivityChanged(bool),
    Completed(T),
}

enum OpenCodeLiveData {
    Stale,
    Malformed,
    Output {
        admission: TerminalAgentActivityAdmission,
        output: OpenCodeActivityOutput,
    },
}

#[derive(Clone, Copy)]
struct OpenCodeLiveFence<'a> {
    activity: &'a TerminalAgentActivityControl,
    generation: &'a TerminalObserverGenerationLease,
    state: TerminalAgentActivityState,
}

#[derive(Clone, Copy)]
struct OpenCodeLivePublication<'a> {
    fence: OpenCodeLiveFence<'a>,
    admission: TerminalAgentActivityAdmission,
}

impl OpenCodeLiveFence<'_> {
    fn is_current(self) -> bool {
        self.generation.cancellation_reason().is_none()
            && self.generation.is_current()
            && self.activity.snapshot() == self.state
    }
}

impl OpenCodeLivePublication<'_> {
    fn is_current(self) -> bool {
        opencode_live_work_is_current(self.fence, &self.admission)
    }
}

async fn wait_for_opencode_live_work<T>(
    activity: &mut tokio::sync::watch::Receiver<TerminalAgentActivityState>,
    generation: &TerminalObserverGenerationLease,
    work: impl Future<Output = T>,
) -> OpenCodeLiveWork<T> {
    tokio::pin!(work);
    tokio::select! {
        biased;
        _ = generation.cancelled() => OpenCodeLiveWork::Cancelled,
        changed = activity.changed() => OpenCodeLiveWork::ActivityChanged(changed.is_ok()),
        completed = &mut work => OpenCodeLiveWork::Completed(completed),
    }
}

fn admit_opencode_live_work(
    fence: OpenCodeLiveFence<'_>,
) -> Option<TerminalAgentActivityAdmission> {
    if !fence.is_current() {
        return None;
    }
    let admission = fence.activity.admit()?;
    (fence.activity.admission_is_current(&admission) && fence.is_current()).then_some(admission)
}

fn opencode_live_work_is_current(
    fence: OpenCodeLiveFence<'_>,
    admission: &TerminalAgentActivityAdmission,
) -> bool {
    fence.activity.admission_is_current(admission) && fence.is_current()
}

fn decode_and_track_opencode_live_data(
    fence: OpenCodeLiveFence<'_>,
    tracker: &mut OpenCodeActivityTracker,
    data: &[u8],
    received_at_ms: u64,
) -> OpenCodeLiveData {
    let Some(admission) = admit_opencode_live_work(fence) else {
        return OpenCodeLiveData::Stale;
    };
    let Ok(event) = serde_json::from_slice::<Value>(data) else {
        return OpenCodeLiveData::Malformed;
    };
    if !opencode_live_work_is_current(fence, &admission) {
        return OpenCodeLiveData::Stale;
    }
    let output = tracker.handle_observed_event_at(&event, received_at_ms);
    if !opencode_live_work_is_current(fence, &admission) {
        return OpenCodeLiveData::Stale;
    }
    OpenCodeLiveData::Output { admission, output }
}

async fn run_opencode_observer(
    inner: Arc<OpenCodeObserverInner>,
    generation: TerminalObserverGenerationLease,
) {
    let mut activity = inner.activity.subscribe();
    let owned_root = inner
        .resources
        .owned_root
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let mut remote = if let Some(owned_root) = owned_root {
        owned_root.remote
    } else {
        let Ok(remote) = inner
            .remote
            .connect(
                &inner.endpoint,
                OPENCODE_USERNAME,
                &inner.password,
                &inner.directory,
            )
            .await
        else {
            generation.cancelled().await;
            inner.resources.cleanup_helper().await;
            return;
        };
        remote
    };
    let root_session_id = inner.root_session_id.clone();
    let correlated = tokio::time::timeout(
        inner.handshake_timeout,
        wait_for_owned_root(&mut *remote, &root_session_id, &generation),
    )
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if !correlated {
        keep_attach_until_cancellation(
            &inner.resources,
            &generation,
            &mut *remote,
            &root_session_id,
        )
        .await;
        return;
    }
    let capabilities = ActivityCapabilities {
        actors: true,
        attributed_activity: true,
        background_work: false,
        history_recovery: ActivityHistoryRecovery::Full,
        terminal_observation: true,
        targeted_actor_cancellation: false,
    };
    if !inner
        .publisher
        .publish_correlated("opencode", Some(&inner.provider_instance_id), capabilities)
        .await
        .unwrap_or(false)
    {
        keep_attach_until_cancellation(
            &inner.resources,
            &generation,
            &mut *remote,
            &root_session_id,
        )
        .await;
        return;
    }

    let mut tracker = OpenCodeActivityTracker::new(&root_session_id);
    let mut sequence = 0_u64;
    reconcile_opencode(
        &inner.publisher,
        &mut *remote,
        &mut tracker,
        &root_session_id,
        &mut sequence,
        None,
    )
    .await;

    let mut epoch = 0_u64;
    let mut stream =
        establish_opencode_event_stream(&mut *remote, &generation, inner.handshake_timeout).await;
    let (mut mode, mut live_state) = if stream.is_some() {
        let state = inner.activity.snapshot();
        let mode = if state.enabled {
            OpenCodeObserverMode::Live
        } else {
            OpenCodeObserverMode::Dormant
        };
        mark_opencode_observation(&inner, state, epoch, mode);
        (mode, (mode == OpenCodeObserverMode::Live).then_some(state))
    } else {
        let state = inner.activity.snapshot();
        mark_opencode_observation(&inner, state, epoch, OpenCodeObserverMode::Unavailable);
        (OpenCodeObserverMode::Unavailable, None)
    };
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + OPENCODE_RECONCILIATION_INTERVAL,
        OPENCODE_RECONCILIATION_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if stream.is_none() {
            stream =
                establish_opencode_event_stream(&mut *remote, &generation, inner.handshake_timeout)
                    .await;
            if generation.cancellation_reason().is_some() {
                stop_opencode_observer(&inner, &mut *remote, &root_session_id).await;
                return;
            }
            if stream.is_some() {
                tracker = OpenCodeActivityTracker::new(&root_session_id);
                let state = inner.activity.snapshot();
                mode = if state.enabled {
                    interval
                        .reset_at(tokio::time::Instant::now() + OPENCODE_RECONCILIATION_INTERVAL);
                    live_state = Some(state);
                    OpenCodeObserverMode::Live
                } else {
                    live_state = None;
                    OpenCodeObserverMode::Dormant
                };
                mark_opencode_observation(&inner, state, epoch, mode);
                continue;
            }

            let state = inner.activity.snapshot();
            mode = OpenCodeObserverMode::Unavailable;
            mark_opencode_observation(&inner, state, epoch, mode);
            tokio::select! {
                biased;
                _ = generation.cancelled() => {
                    stop_opencode_observer(&inner, &mut *remote, &root_session_id).await;
                    return;
                }
                changed = activity.changed() => {
                    if changed.is_err() {
                        stop_opencode_observer(&inner, &mut *remote, &root_session_id).await;
                        return;
                    }
                }
                _ = tokio::time::sleep(OPENCODE_SSE_RETRY_MAX) => {}
            }
            continue;
        }

        let event = {
            let current_stream = stream.as_mut().expect("OpenCode stream is present");
            match mode {
                OpenCodeObserverMode::Live => tokio::select! {
                    biased;
                    _ = generation.cancelled() => OpenCodeObserverEvent::Cancelled,
                    changed = activity.changed() => {
                        OpenCodeObserverEvent::ActivityChanged(changed.is_ok())
                    }
                    _ = interval.tick() => OpenCodeObserverEvent::Reconcile,
                    data = current_stream.next_data() => OpenCodeObserverEvent::Data(data),
                },
                OpenCodeObserverMode::Dormant | OpenCodeObserverMode::Unavailable => {
                    tokio::select! {
                        biased;
                        _ = generation.cancelled() => OpenCodeObserverEvent::Cancelled,
                        changed = activity.changed() => {
                            OpenCodeObserverEvent::ActivityChanged(changed.is_ok())
                        }
                        discarded = current_stream.discard_next() => {
                            OpenCodeObserverEvent::Discard(discarded)
                        }
                    }
                }
            }
        };

        let event = match event {
            OpenCodeObserverEvent::Reconcile => {
                let Some(state) = live_state else {
                    continue;
                };
                let fence = OpenCodeLiveFence {
                    activity: &inner.activity,
                    generation: &generation,
                    state,
                };
                let Some(admission) = admit_opencode_live_work(fence) else {
                    continue;
                };
                let publication = OpenCodeLivePublication { fence, admission };
                let reconciliation = reconcile_opencode(
                    &inner.publisher,
                    &mut *remote,
                    &mut tracker,
                    &root_session_id,
                    &mut sequence,
                    Some(publication),
                );
                match wait_for_opencode_live_work(&mut activity, &generation, reconciliation).await
                {
                    OpenCodeLiveWork::Cancelled => OpenCodeObserverEvent::Cancelled,
                    OpenCodeLiveWork::ActivityChanged(changed) => {
                        OpenCodeObserverEvent::ActivityChanged(changed)
                    }
                    OpenCodeLiveWork::Completed(()) => continue,
                }
            }
            event => event,
        };

        match event {
            OpenCodeObserverEvent::Cancelled => {
                stop_opencode_observer(&inner, &mut *remote, &root_session_id).await;
                return;
            }
            OpenCodeObserverEvent::ActivityChanged(false) => {
                stop_opencode_observer(&inner, &mut *remote, &root_session_id).await;
                return;
            }
            OpenCodeObserverEvent::ActivityChanged(true) => {
                let mut state = *activity.borrow();
                loop {
                    if !state.enabled {
                        live_state = None;
                        mode = OpenCodeObserverMode::Dormant;
                        mark_opencode_observation(&inner, state, epoch, mode);
                        break;
                    }
                    if mode == OpenCodeObserverMode::Live && live_state == Some(state) {
                        mark_opencode_observation(&inner, state, epoch, mode);
                        break;
                    }

                    live_state = None;
                    mode = OpenCodeObserverMode::Dormant;
                    let deadline = tokio::time::Instant::now() + inner.reattach_timeout;
                    let outcome = {
                        let dormant_stream =
                            stream.as_mut().expect("dormant OpenCode stream is present");
                        prepare_opencode_replacement(
                            &mut *remote,
                            &mut **dormant_stream,
                            &mut activity,
                            state,
                            &generation,
                            deadline,
                        )
                        .await
                    };
                    match outcome {
                        OpenCodeReplacementOutcome::Connected(replacement) => {
                            let next_epoch = epoch.saturating_add(1);
                            let promoted = commit_opencode_observation(
                                &inner,
                                state,
                                next_epoch,
                                OpenCodeObserverMode::Live,
                                || {
                                    stream = Some(replacement);
                                    epoch = next_epoch;
                                    tracker = OpenCodeActivityTracker::new(&root_session_id);
                                    live_state = Some(state);
                                    mode = OpenCodeObserverMode::Live;
                                    interval.reset_at(
                                        tokio::time::Instant::now()
                                            + OPENCODE_RECONCILIATION_INTERVAL,
                                    );
                                },
                            );
                            if promoted {
                                break;
                            }
                            state = inner.activity.snapshot();
                        }
                        OpenCodeReplacementOutcome::Failed {
                            dormant_stream_healthy,
                        } => {
                            if !dormant_stream_healthy {
                                stream = None;
                                epoch = epoch.saturating_add(1);
                            }
                            mode = OpenCodeObserverMode::Unavailable;
                            mark_opencode_observation(
                                &inner,
                                inner.activity.snapshot(),
                                epoch,
                                mode,
                            );
                            break;
                        }
                        OpenCodeReplacementOutcome::Superseded {
                            dormant_stream_healthy,
                        } => {
                            let current = inner.activity.snapshot();
                            if !dormant_stream_healthy {
                                stream = None;
                                epoch = epoch.saturating_add(1);
                                mode = OpenCodeObserverMode::Unavailable;
                                mark_opencode_observation(&inner, current, epoch, mode);
                                break;
                            }
                            state = current;
                        }
                        OpenCodeReplacementOutcome::Cancelled => {
                            stop_opencode_observer(&inner, &mut *remote, &root_session_id).await;
                            return;
                        }
                    }
                }
            }
            OpenCodeObserverEvent::Data(Ok(data)) => {
                let Some(state) = live_state else {
                    continue;
                };
                let fence = OpenCodeLiveFence {
                    activity: &inner.activity,
                    generation: &generation,
                    state,
                };
                let (admission, output) = match decode_and_track_opencode_live_data(
                    fence,
                    &mut tracker,
                    &data,
                    now_millis(),
                ) {
                    OpenCodeLiveData::Stale => continue,
                    OpenCodeLiveData::Malformed => {
                        stream = None;
                        epoch = epoch.saturating_add(1);
                        live_state = None;
                        mode = OpenCodeObserverMode::Unavailable;
                        mark_opencode_observation(&inner, inner.activity.snapshot(), epoch, mode);
                        continue;
                    }
                    OpenCodeLiveData::Output { admission, output } => (admission, output),
                };
                if !opencode_live_work_is_current(fence, &admission) {
                    continue;
                }
                apply_opencode_output(
                    &inner.publisher,
                    &format!("opencode:terminal:sse:{sequence}"),
                    output.mutations,
                    Some(OpenCodeLivePublication { fence, admission }),
                )
                .await;
                sequence = sequence.saturating_add(1);
            }
            OpenCodeObserverEvent::Reconcile => unreachable!("reconciliation is resolved above"),
            OpenCodeObserverEvent::Data(Err(_)) | OpenCodeObserverEvent::Discard(Err(_)) => {
                stream = None;
                epoch = epoch.saturating_add(1);
                live_state = None;
                mode = OpenCodeObserverMode::Unavailable;
                mark_opencode_observation(&inner, inner.activity.snapshot(), epoch, mode);
            }
            OpenCodeObserverEvent::Discard(Ok(())) => {}
        }
    }
}

fn mark_opencode_observation(
    inner: &OpenCodeObserverInner,
    state: TerminalAgentActivityState,
    epoch: u64,
    mode: OpenCodeObserverMode,
) -> bool {
    commit_opencode_observation(inner, state, epoch, mode, || {})
}

fn commit_opencode_observation(
    inner: &OpenCodeObserverInner,
    state: TerminalAgentActivityState,
    epoch: u64,
    mode: OpenCodeObserverMode,
    commit: impl FnOnce(),
) -> bool {
    let kind = match mode {
        OpenCodeObserverMode::Live => TerminalAgentActivityObservationKind::Live,
        OpenCodeObserverMode::Dormant => TerminalAgentActivityObservationKind::Dormant,
        OpenCodeObserverMode::Unavailable => TerminalAgentActivityObservationKind::Unavailable,
    };
    inner.activity.commit_observed(
        TerminalAgentActivityObservation { state, epoch, kind },
        || {
            commit();
        },
    )
}

async fn stop_opencode_observer(
    inner: &OpenCodeObserverInner,
    remote: &mut dyn OpenCodeRemoteClient,
    root_session_id: &str,
) {
    let _ = remote.abort(root_session_id).await;
    inner.resources.cleanup_helper().await;
}

async fn establish_opencode_event_stream(
    remote: &mut dyn OpenCodeRemoteClient,
    generation: &TerminalObserverGenerationLease,
    timeout: Duration,
) -> Option<Box<dyn OpenCodeEventStream>> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut retry = OPENCODE_SSE_RETRY_MIN;
    loop {
        let opened = tokio::select! {
            biased;
            _ = generation.cancelled() => return None,
            opened = tokio::time::timeout_at(deadline, remote.open_event_stream()) => opened,
        };
        if let Ok(Ok(mut stream)) = opened
            && wait_for_opencode_connected(&mut *stream, generation, deadline).await
        {
            return Some(stream);
        }

        let retry_at = tokio::time::Instant::now() + retry;
        if retry_at >= deadline {
            return None;
        }
        tokio::select! {
            biased;
            _ = generation.cancelled() => return None,
            _ = tokio::time::sleep_until(retry_at) => {}
        }
        retry = retry.saturating_mul(2).min(OPENCODE_SSE_RETRY_MAX);
    }
}

async fn wait_for_opencode_connected(
    stream: &mut dyn OpenCodeEventStream,
    generation: &TerminalObserverGenerationLease,
    deadline: tokio::time::Instant,
) -> bool {
    loop {
        let data = tokio::select! {
            biased;
            _ = generation.cancelled() => return false,
            data = tokio::time::timeout_at(deadline, stream.next_data()) => data,
        };
        let Ok(Ok(data)) = data else {
            return false;
        };
        let Ok(event) = serde_json::from_slice::<Value>(&data) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) == Some("server.connected") {
            return true;
        }
    }
}

async fn prepare_opencode_replacement(
    remote: &mut dyn OpenCodeRemoteClient,
    dormant_stream: &mut dyn OpenCodeEventStream,
    activity: &mut tokio::sync::watch::Receiver<TerminalAgentActivityState>,
    expected_state: TerminalAgentActivityState,
    generation: &TerminalObserverGenerationLease,
    deadline: tokio::time::Instant,
) -> OpenCodeReplacementOutcome {
    let mut dormant_stream_healthy = true;
    let opening = remote.open_event_stream();
    tokio::pin!(opening);
    let mut replacement = loop {
        if tokio::time::Instant::now() >= deadline {
            return OpenCodeReplacementOutcome::Failed {
                dormant_stream_healthy,
            };
        }
        tokio::select! {
            biased;
            _ = generation.cancelled() => return OpenCodeReplacementOutcome::Cancelled,
            changed = activity.changed() => {
                if changed.is_err() || *activity.borrow() != expected_state {
                    return OpenCodeReplacementOutcome::Superseded {
                        dormant_stream_healthy,
                    };
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return OpenCodeReplacementOutcome::Failed {
                    dormant_stream_healthy,
                };
            }
            opened = &mut opening => {
                match opened {
                    Ok(stream) => break stream,
                    Err(_) => {
                        return OpenCodeReplacementOutcome::Failed {
                            dormant_stream_healthy,
                        };
                    }
                }
            }
            discarded = dormant_stream.discard_next(), if dormant_stream_healthy => {
                if discarded.is_err() {
                    dormant_stream_healthy = false;
                }
            }
        }
    };

    let is_connected = {
        let connected = wait_for_opencode_connected(&mut *replacement, generation, deadline);
        tokio::pin!(connected);
        loop {
            if tokio::time::Instant::now() >= deadline {
                return OpenCodeReplacementOutcome::Failed {
                    dormant_stream_healthy,
                };
            }
            tokio::select! {
                biased;
                _ = generation.cancelled() => return OpenCodeReplacementOutcome::Cancelled,
                changed = activity.changed() => {
                    if changed.is_err() || *activity.borrow() != expected_state {
                        return OpenCodeReplacementOutcome::Superseded {
                            dormant_stream_healthy,
                        };
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return OpenCodeReplacementOutcome::Failed {
                        dormant_stream_healthy,
                    };
                }
                is_connected = &mut connected => break is_connected,
                discarded = dormant_stream.discard_next(), if dormant_stream_healthy => {
                    if discarded.is_err() {
                        dormant_stream_healthy = false;
                    }
                }
            }
        }
    };
    let current = *activity.borrow();
    if current != expected_state {
        OpenCodeReplacementOutcome::Superseded {
            dormant_stream_healthy,
        }
    } else if is_connected {
        OpenCodeReplacementOutcome::Connected(replacement)
    } else {
        OpenCodeReplacementOutcome::Failed {
            dormant_stream_healthy,
        }
    }
}

async fn keep_attach_until_cancellation(
    resources: &OpenCodeObserverResources,
    generation: &TerminalObserverGenerationLease,
    remote: &mut dyn OpenCodeRemoteClient,
    root_session_id: &str,
) {
    generation.cancelled().await;
    let _ = remote.abort(root_session_id).await;
    resources.cleanup_helper().await;
}

async fn wait_for_owned_root(
    remote: &mut dyn OpenCodeRemoteClient,
    root_session_id: &str,
    generation: &TerminalObserverGenerationLease,
) -> Option<bool> {
    loop {
        if !generation.is_current() {
            return Some(false);
        }
        if let Ok(root) =
            tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, remote.root(root_session_id)).await
            && let Ok(root) = root
            && root.get("id").and_then(Value::as_str) == Some(root_session_id)
            && root.get("parentID").is_none_or(Value::is_null)
        {
            return Some(true);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn reconcile_opencode(
    publisher: &TerminalGenerationActivityPublisher,
    remote: &mut dyn OpenCodeRemoteClient,
    tracker: &mut OpenCodeActivityTracker,
    root_session_id: &str,
    sequence: &mut u64,
    live_publication: Option<OpenCodeLivePublication<'_>>,
) {
    let mut queue = VecDeque::from([root_session_id.to_owned()]);
    let mut children = Vec::new();
    while let Some(parent) = queue.pop_front() {
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        if children.len() >= MAX_RECONCILED_CHILDREN {
            break;
        }
        let Ok(Ok(response)) =
            tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, remote.children(&parent)).await
        else {
            continue;
        };
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        let remaining = MAX_RECONCILED_CHILDREN.saturating_sub(children.len());
        let (output, accepted) = tracker.reconcile_children_limited(&parent, &response, remaining);
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        apply_opencode_output(
            publisher,
            &format!("opencode:terminal:children:{sequence}"),
            output.mutations,
            live_publication,
        )
        .await;
        *sequence = sequence.saturating_add(1);
        for child_id in accepted {
            queue.push_back(child_id.clone());
            children.push(child_id);
        }
    }
    if let Ok(Ok(statuses)) =
        tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, remote.statuses()).await
    {
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        for session_id in &children {
            if live_publication.is_some_and(|publication| !publication.is_current()) {
                return;
            }
            let Some(status) = statuses.get(session_id) else {
                continue;
            };
            let output = tracker.handle_event(&json!({
                "type": "session.status",
                "properties": {
                    "sessionID": session_id,
                    "status": status,
                }
            }));
            if live_publication.is_some_and(|publication| !publication.is_current()) {
                return;
            }
            apply_opencode_output(
                publisher,
                &format!("opencode:terminal:status:{sequence}"),
                output.mutations,
                live_publication,
            )
            .await;
            *sequence = sequence.saturating_add(1);
        }
    }
    for session_id in children {
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        let Ok(Ok(messages)) =
            tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, remote.messages(&session_id)).await
        else {
            continue;
        };
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        let output = tracker.handle_history(&session_id, &messages);
        if live_publication.is_some_and(|publication| !publication.is_current()) {
            return;
        }
        apply_opencode_output(
            publisher,
            &format!("opencode:terminal:messages:{sequence}"),
            output.mutations,
            live_publication,
        )
        .await;
        *sequence = sequence.saturating_add(1);
    }
}

async fn apply_opencode_output(
    publisher: &TerminalGenerationActivityPublisher,
    native_event_id: &str,
    mutations: Vec<ProviderActivityMutation>,
    live_publication: Option<OpenCodeLivePublication<'_>>,
) {
    if mutations.is_empty() {
        return;
    }
    let created_at = now_iso();
    let _ = match live_publication {
        Some(publication) => {
            publisher
                .apply_admitted(
                    publication.fence.activity,
                    &publication.admission,
                    native_event_id,
                    mutations,
                    &created_at,
                )
                .await
        }
        None => {
            publisher
                .apply(native_event_id, mutations, &created_at)
                .await
        }
    };
}

fn now_millis() -> u64 {
    OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .checked_div(1_000_000)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or_default()
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

#[derive(Debug)]
struct SystemOpenCodeCapabilityProbeRunner;

impl OpenCodeCapabilityProbeRunner for SystemOpenCodeCapabilityProbeRunner {
    fn run(
        &self,
        executable: &Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeProbeOutput, String>> + Send + '_>> {
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
            let output = run_supervised(
                SupervisedRunRequest {
                    command,
                    stdin: None,
                    timeout: Duration::from_millis(900),
                    cleanup_timeout: Duration::from_secs(2),
                    max_output_bytes: OPENCODE_PROBE_OUTPUT_LIMIT,
                    overflow: SupervisedOverflow::Truncate,
                },
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map_err(|error| format!("OpenCode capability probe failed: {error:?}"))?;
            Ok(OpenCodeProbeOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
            })
        })
    }
}

#[derive(Debug)]
struct SystemOpenCodeHelperLauncher {
    readiness_timeout: Duration,
    reaper: Arc<OpenCodeRetainedReaper>,
    #[cfg(test)]
    fixture_events: Option<OpenCodeHelperFixtureEvents>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct OpenCodeHelperFixtureEvents {
    spawned: Arc<crate::test_support::FixtureEvent>,
    release: Arc<crate::test_support::FixtureEvent>,
    admission_race: Option<Arc<OpenCodeHelperAdmissionRace>>,
    reaped: Arc<crate::test_support::FixtureEvent>,
    pid_path: PathBuf,
    reap_timeout: Option<OpenCodeHelperReapTimeoutEvents>,
    stdout_join: Option<OpenCodeHelperStdoutJoinEvents>,
    wait_error: Option<OpenCodeHelperWaitErrorEvents>,
}

#[cfg(test)]
#[derive(Debug)]
struct OpenCodeHelperAdmissionRace {
    spawned: crate::test_support::FixtureEvent,
    release: crate::test_support::FixtureEvent,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct OpenCodeHelperReapTimeoutEvents {
    foreground_wait_started: Arc<crate::test_support::FixtureEvent>,
    foreground_return_release: Arc<crate::test_support::FixtureEvent>,
    background_wait_started: Arc<crate::test_support::FixtureEvent>,
    background_wait_release: Arc<crate::test_support::FixtureEvent>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct OpenCodeHelperStdoutJoinEvents {
    ownership_registered: Arc<crate::test_support::FixtureEvent>,
    join_started: Arc<crate::test_support::FixtureEvent>,
    join_release: Arc<crate::test_support::FixtureEvent>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct OpenCodeHelperWaitErrorEvents {
    failures_remaining: Arc<std::sync::atomic::AtomicUsize>,
    fail_persistently: Arc<AtomicBool>,
    injected: Arc<crate::test_support::FixtureEvent>,
    recorded: Arc<crate::test_support::FixtureEvent>,
    retry_started: Arc<crate::test_support::FixtureEvent>,
}

#[cfg(test)]
impl OpenCodeHelperWaitErrorEvents {
    fn should_inject(&self) -> bool {
        self.fail_persistently.load(Ordering::Acquire)
            || self
                .failures_remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
    }
}

#[cfg(test)]
impl OpenCodeHelperFixtureEvents {
    fn publish_pid(&self, process_id: Option<u32>) -> Result<(), String> {
        let process_id =
            process_id.ok_or_else(|| "OpenCode helper PID is unavailable".to_owned())?;
        let mut temporary_name = self.pid_path.as_os_str().to_owned();
        temporary_name.push(".tmp");
        let temporary_path = PathBuf::from(temporary_name);
        std::fs::write(&temporary_path, process_id.to_string())
            .map_err(|error| format!("failed staging OpenCode helper PID: {error}"))?;
        std::fs::rename(&temporary_path, &self.pid_path)
            .map_err(|error| format!("failed publishing OpenCode helper PID: {error}"))?;
        Ok(())
    }

    fn publish_spawned(&self, process_id: Option<u32>) -> Result<(), String> {
        self.publish_pid(process_id)?;
        self.spawned.publish();
        Ok(())
    }
}

impl Default for SystemOpenCodeHelperLauncher {
    fn default() -> Self {
        Self {
            readiness_timeout: OPENCODE_HELPER_READY_TIMEOUT,
            reaper: Arc::new(OpenCodeRetainedReaper::default()),
            #[cfg(test)]
            fixture_events: None,
        }
    }
}

impl SystemOpenCodeHelperLauncher {
    #[cfg(all(test, unix))]
    fn with_fixture_events(
        readiness_timeout: Duration,
        fixture_events: OpenCodeHelperFixtureEvents,
    ) -> Self {
        Self {
            readiness_timeout,
            reaper: Arc::new(OpenCodeRetainedReaper::default()),
            fixture_events: Some(fixture_events),
        }
    }
}

impl OpenCodeHelperLauncher for SystemOpenCodeHelperLauncher {
    fn start(
        &self,
        launch: OpenCodeHelperLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeHelperReady, String>> + Send + '_>> {
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
                    "failed to start OpenCode helper: executable was not found or was refused by the hermetic test guard"
                        .to_owned()
                })?;
                launch.executable = executable.to_string_lossy().into_owned();
                launch
            };
            let reaper_permit = self.reaper.reserve()?;
            let mut command = CommandWrap::with_new(&launch.executable, |command| {
                command
                    .args(&launch.args)
                    .current_dir(&launch.cwd)
                    .envs(&launch.env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null());
                crate::provider::environment::sanitize_provider_subprocess_environment(command);
            });
            configure_supervised_background_command_wrap(&mut command);
            #[cfg(not(windows))]
            let mut child = spawn_retrying_busy_executable(
                &mut command,
                tokio::time::Instant::now() + self.readiness_timeout,
            )
            .await
            .map_err(|error| format!("failed to start OpenCode helper: {error}"))?;
            #[cfg(windows)]
            let mut child = command
                .spawn()
                .map_err(|error| format!("failed to start OpenCode helper: {error}"))?;
            #[cfg(test)]
            if let Some(events) = self.fixture_events.as_ref()
                && let Some(race) = events.admission_race.as_ref()
            {
                if let Err(error) = events.publish_pid(child.id()) {
                    terminate_and_reap_unregistered_opencode_helper(&mut *child).await;
                    return Err(error);
                }
                race.spawned.publish();
                race.release
                    .wait_after_bounded(0, "the test to release the admission-race helper")
                    .await;
            }
            let identity = child
                .id()
                .and_then(|pid| NativeProcessSampler::process_identity(pid).ok());
            let Some(identity) = identity else {
                terminate_and_reap_unregistered_opencode_helper(&mut *child).await;
                return Err("spawned OpenCode helper has no stable process identity".to_owned());
            };
            match child.try_wait() {
                Ok(None) => {}
                Ok(Some(_)) => {
                    terminate_and_reap_unregistered_opencode_helper(&mut *child).await;
                    return Err("OpenCode helper exited before ownership admission".to_owned());
                }
                Err(error) => {
                    terminate_and_reap_unregistered_opencode_helper(&mut *child).await;
                    return Err(format!(
                        "failed to revalidate OpenCode helper process identity: {error}"
                    ));
                }
            }
            let process_registration = match launch.process_attribution.register_identity(
                identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Helper,
                    label: "OpenCode server helper".to_owned(),
                    source: RegistrationSource::Helper,
                    driver: None,
                },
            ) {
                Ok(registration) => registration,
                Err(
                    error @ (ProcessRegistrationError::Shutdown
                    | ProcessRegistrationError::Capacity),
                ) => {
                    terminate_and_reap_unregistered_opencode_helper(&mut *child).await;
                    return Err(error.to_string());
                }
            };
            let stdout = match child.stdout().take() {
                Some(stdout) => stdout,
                None => {
                    terminate_and_reap_unregistered_opencode_helper(&mut *child).await;
                    drop(process_registration);
                    return Err("OpenCode helper stdout is unavailable".to_owned());
                }
            };
            let process = Arc::new(SystemOpenCodeHelperProcess {
                process_group_id: child.id().and_then(|pid| i32::try_from(pid).ok()),
                #[cfg(unix)]
                process_group_identity_reserved: AtomicBool::new(child.id().is_some()),
                child: Mutex::new(Some(child)),
                process_registration: Mutex::new(Some(process_registration)),
                stdout_task: Mutex::new(None),
                reaper: self.reaper.clone(),
                reaper_permit: Mutex::new(Some(reaper_permit)),
                #[cfg(test)]
                fixture_events: self.fixture_events.clone(),
            });
            #[cfg(test)]
            if let Some(events) = self.fixture_events.as_ref() {
                if let Err(error) = events.publish_spawned(
                    process
                        .process_group_id
                        .and_then(|pid| u32::try_from(pid).ok()),
                ) {
                    process.terminate_and_reap().await;
                    return Err(error);
                }
                events
                    .release
                    .wait_after_bounded(0, "the test to release the spawned helper")
                    .await;
            }
            #[cfg(test)]
            if let Some(stdout_join) = self
                .fixture_events
                .as_ref()
                .and_then(|events| events.stdout_join.as_ref())
            {
                let stdout_join = stdout_join.clone();
                let stdout_task = tokio::spawn(async move {
                    stdout_join.join_started.publish();
                    stdout_join
                        .join_release
                        .wait_after_bounded(0, "the test to release the retained stdout join")
                        .await;
                });
                *process
                    .stdout_task
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stdout_task);
            }
            let mut stdout = BufReader::new(stdout);
            let endpoint = match tokio::time::timeout(
                self.readiness_timeout,
                read_opencode_readiness(&mut stdout),
            )
            .await
            {
                Ok(Ok(endpoint)) => endpoint,
                Ok(Err(error)) => {
                    process.terminate_and_reap().await;
                    return Err(error);
                }
                Err(_) => {
                    process.terminate_and_reap().await;
                    return Err("OpenCode helper readiness timed out".to_owned());
                }
            };
            if !valid_loopback_endpoint(&endpoint) {
                process.terminate_and_reap().await;
                return Err("OpenCode helper advertised an invalid endpoint".to_owned());
            }
            let stdout_task = tokio::spawn(drain_opencode_stdout(stdout));
            *process
                .stdout_task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stdout_task);
            Ok(OpenCodeHelperReady { endpoint, process })
        })
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.reaper.shutdown())
    }
}

async fn read_opencode_readiness(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<String, String> {
    let mut total = 0_usize;
    loop {
        let mut line = Vec::new();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| format!("failed reading OpenCode helper readiness: {error}"))?;
        if bytes_read == 0 {
            return Err("OpenCode helper exited before readiness".to_owned());
        }
        total = total.saturating_add(line.len());
        if total > OPENCODE_PROBE_OUTPUT_LIMIT {
            return Err("OpenCode helper readiness output exceeded its bound".to_owned());
        }
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches(['\r', '\n']);
        // OpenCode 1 prints `opencode server listening on <url>`, OpenCode 2 `server listening
        // on <url>`.
        if let Some(endpoint) = line
            .strip_prefix("opencode server listening on ")
            .or_else(|| line.strip_prefix("server listening on "))
        {
            return Ok(endpoint.trim().to_owned());
        }
    }
}

async fn terminate_and_reap_unregistered_opencode_helper(child: &mut dyn ChildWrapper) {
    let _ = child.start_kill();
    if tokio::time::timeout(OPENCODE_HELPER_REAP_TIMEOUT, child.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            timeout_ms = OPENCODE_HELPER_REAP_TIMEOUT.as_millis(),
            "unregistered OpenCode helper reap exceeded its bounded cleanup window"
        );
        let _ = child.wait().await;
    }
}

async fn drain_opencode_stdout(mut stdout: BufReader<tokio::process::ChildStdout>) {
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        match stdout.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

struct SystemOpenCodeHelperProcess {
    process_group_id: Option<i32>,
    #[cfg(unix)]
    process_group_identity_reserved: AtomicBool,
    child: Mutex<Option<Box<dyn ChildWrapper>>>,
    process_registration: Mutex<Option<ProcessRegistration>>,
    stdout_task: Mutex<Option<JoinHandle<()>>>,
    reaper: Arc<OpenCodeRetainedReaper>,
    reaper_permit: Mutex<Option<OwnedSemaphorePermit>>,
    #[cfg(test)]
    fixture_events: Option<OpenCodeHelperFixtureEvents>,
}

impl fmt::Debug for SystemOpenCodeHelperProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SystemOpenCodeHelperProcess")
            .finish_non_exhaustive()
    }
}

impl OpenCodeHelperProcess for SystemOpenCodeHelperProcess {
    fn has_exited(&self) -> bool {
        let mut child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(child) = child.as_mut() else {
            return true;
        };
        #[cfg(unix)]
        {
            match child_has_exited_without_reaping(child.as_ref()) {
                Ok(exited) => exited,
                Err(_) => {
                    // An uncertain observation no longer proves that the
                    // original leader reserves its numeric PID/PGID. Fail open
                    // as exited, but permanently disable process-group signals.
                    self.process_group_identity_reserved
                        .store(false, Ordering::Release);
                    true
                }
            }
        }
        #[cfg(not(unix))]
        {
            child.try_wait().map_or(true, |status| status.is_some())
        }
    }

    fn terminate(&self) {
        let child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let stdout_task = self
            .stdout_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let permit = self
            .reaper_permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let process_registration = self
            .process_registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(mut child) = child else {
            return;
        };
        let Some(permit) = permit else {
            return;
        };
        #[cfg(unix)]
        let process_group_id = self.reserved_process_group_id();
        #[cfg(not(unix))]
        let process_group_id = self.process_group_id;
        #[cfg(unix)]
        if let Some(process_group_id) = process_group_id {
            // SAFETY: the process-group ID was captured from this owned helper
            // and non-reaping observation still proves its leader identity is
            // reserved.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
        self.reaper.submit(
            child,
            process_group_id,
            permit,
            process_registration,
            stdout_task,
            #[cfg(test)]
            self.fixture_events.clone(),
        );
    }

    fn terminate_and_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        #[cfg(unix)]
        let process_group_id = self.reserved_process_group_id();
        #[cfg(not(unix))]
        let process_group_id = self.process_group_id;
        let stdout_task = self
            .stdout_task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let permit = self
            .reaper_permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let process_registration = self
            .process_registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let foreground_done = match (child, permit) {
            (Some(child), Some(permit)) => {
                #[cfg(unix)]
                if let Some(process_group_id) = process_group_id {
                    // SAFETY: the process-group ID was captured from this owned
                    // helper and its leader identity is still reserved.
                    unsafe {
                        libc::kill(-process_group_id, libc::SIGTERM);
                    }
                }
                #[cfg(test)]
                let fixture_events = self.fixture_events.clone();
                Some(self.reaper.submit(
                    child,
                    process_group_id,
                    permit,
                    process_registration,
                    stdout_task,
                    #[cfg(test)]
                    fixture_events,
                ))
            }
            _ => None,
        };
        Box::pin(async move {
            let Some(mut foreground_done) = foreground_done else {
                return;
            };
            while !*foreground_done.borrow() {
                if foreground_done.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

struct SystemOpenCodeReapGuard {
    child: Option<Box<dyn ChildWrapper>>,
    #[cfg(unix)]
    process_group_id: Option<i32>,
}

impl SystemOpenCodeReapGuard {
    fn child_mut(&mut self) -> &mut dyn ChildWrapper {
        self.child
            .as_mut()
            .expect("OpenCode reap guard owns a child")
            .as_mut()
    }

    fn disarm(&mut self) {
        self.child.take();
    }
}

impl Drop for SystemOpenCodeReapGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            // SAFETY: this guard receives a group ID only while the owned
            // leader identity remains reserved. Cancellation drops the guard
            // before that leader can be reaped.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
    }
}

#[cfg(unix)]
impl SystemOpenCodeHelperProcess {
    fn reserved_process_group_id(&self) -> Option<i32> {
        self.process_group_identity_reserved
            .load(Ordering::Acquire)
            .then_some(self.process_group_id)
            .flatten()
    }
}

#[cfg(unix)]
fn child_has_exited_without_reaping(child: &dyn ChildWrapper) -> std::io::Result<bool> {
    let Some(process_id) = child.id() else {
        return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
    };
    observe_child_exit_with(process_id, || waitid_child_once(process_id))
}

#[cfg(unix)]
fn observe_child_exit_with(
    process_id: u32,
    mut waitid_child: impl FnMut() -> std::io::Result<Option<libc::pid_t>>,
) -> std::io::Result<bool> {
    let mut interrupted_retries = 0_usize;
    let observed_process_id = loop {
        match waitid_child() {
            Err(error)
                if error.kind() == std::io::ErrorKind::Interrupted
                    && interrupted_retries < OPENCODE_WAITID_EINTR_RETRY_LIMIT =>
            {
                interrupted_retries = interrupted_retries.saturating_add(1);
            }
            observation => break observation?,
        }
    };
    let Some(observed_process_id) = observed_process_id else {
        return Ok(false);
    };
    if observed_process_id != process_id as libc::pid_t {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "waitid returned a different child identity",
        ));
    }
    Ok(true)
}

#[cfg(unix)]
fn waitid_child_once(process_id: u32) -> std::io::Result<Option<libc::pid_t>> {
    let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `information` points to writable siginfo storage and P_PID
    // limits observation to this owned child. WNOWAIT is essential: keeping
    // the exited leader waitable reserves its PID/PGID until group cleanup has
    // sent TERM/KILL and the final `Child::wait` reaps it.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            process_id as libc::id_t,
            information.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful `waitid` initialized the zeroed siginfo storage.
    let information = unsafe { information.assume_init() };
    // SAFETY: `si_pid` reads the process field of initialized siginfo. A zero
    // PID means WNOHANG found no exited state.
    let observed_process_id = unsafe { information.si_pid() };
    if observed_process_id == 0 {
        return Ok(None);
    }
    Ok(Some(observed_process_id))
}

enum OpenCodeForegroundReap {
    Reaped,
    #[cfg(test)]
    TimedOut,
    WaitFailed(String),
}

async fn reap_opencode_helper(
    cleanup: &mut SystemOpenCodeReapGuard,
    process_group_id: Option<i32>,
    foreground_done: &watch::Sender<bool>,
    #[cfg(test)] fixture_events: Option<&OpenCodeHelperFixtureEvents>,
) -> OpenCodeForegroundReap {
    tokio::time::sleep(OPENCODE_HELPER_TERM_GRACE).await;
    #[cfg(not(unix))]
    let _ = process_group_id;
    #[cfg(unix)]
    if let Some(process_group_id) = process_group_id {
        // SAFETY: a negative PID addresses only the process group created for
        // this owned helper, captured before its parent could exit.
        unsafe {
            libc::kill(-process_group_id, libc::SIGKILL);
        }
    }
    let _ = cleanup.child_mut().start_kill();
    #[cfg(test)]
    if let Some(reap_timeout) = fixture_events.and_then(|events| events.reap_timeout.as_ref()) {
        reap_timeout.foreground_wait_started.publish();
        let _ = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>()).await;
        return OpenCodeForegroundReap::TimedOut;
    }
    let mut wait = Box::pin(wait_opencode_child(
        cleanup.child_mut(),
        #[cfg(test)]
        fixture_events,
    ));
    match tokio::time::timeout(OPENCODE_HELPER_REAP_TIMEOUT, wait.as_mut()).await {
        Ok(Ok(_)) => {
            drop(wait);
            cleanup.disarm();
            OpenCodeForegroundReap::Reaped
        }
        Ok(Err(error)) => {
            OpenCodeForegroundReap::WaitFailed(format!("OpenCode helper wait failed: {error}"))
        }
        Err(_) => {
            // `process-wrap` job/process-group waits may own kernel observation state that is not
            // cancellation-safe. Publish the foreground budget without dropping that in-flight
            // wait, then let the retained reaper finish the same future in the background.
            foreground_done.send_replace(true);
            let result = wait.as_mut().await;
            drop(wait);
            match result {
                Ok(_) => {
                    cleanup.disarm();
                    OpenCodeForegroundReap::Reaped
                }
                Err(error) => OpenCodeForegroundReap::WaitFailed(format!(
                    "OpenCode helper wait failed: {error}"
                )),
            }
        }
    }
}

async fn wait_opencode_child(
    child: &mut dyn ChildWrapper,
    #[cfg(test)] fixture_events: Option<&OpenCodeHelperFixtureEvents>,
) -> std::io::Result<std::process::ExitStatus> {
    loop {
        #[cfg(test)]
        if let Some(wait_error) = fixture_events.and_then(|events| events.wait_error.as_ref())
            && wait_error.should_inject()
        {
            wait_error.injected.publish();
            return Err(std::io::Error::other(
                "injected OpenCode helper wait failure",
            ));
        }
        match child.wait().await {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

#[derive(Clone, Debug)]
struct OpenCodeRetainedReaperTask {
    join: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    join_succeeded: Arc<AtomicBool>,
    completed: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

impl OpenCodeRetainedReaperTask {
    fn try_join_finished(&self) -> bool {
        if self.join_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let Ok(mut join) = self.join.try_lock() else {
            return false;
        };
        if self.join_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let Some(handle) = join.as_mut() else {
            return false;
        };
        if !handle.is_finished() {
            return false;
        }
        let result = (&mut *handle)
            .now_or_never()
            .expect("a finished OpenCode retained reaper join is ready");
        join.take();
        self.record_join_result(result)
    }

    async fn join_completed(&self) -> bool {
        if self.join_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let mut join = self.join.lock().await;
        if self.join_succeeded.load(Ordering::Acquire) {
            return true;
        }
        let Some(handle) = join.as_mut() else {
            return false;
        };
        let result = (&mut *handle).await;
        join.take();
        self.record_join_result(result)
    }

    fn record_join_result(&self, result: Result<(), tokio::task::JoinError>) -> bool {
        match result {
            Ok(()) => {
                self.join_succeeded.store(true, Ordering::Release);
                true
            }
            Err(error) => {
                *self
                    .failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!(
                    "OpenCode helper retained reaper task join failed: {error}"
                ));
                false
            }
        }
    }
}

#[derive(Debug)]
enum OpenCodeRetainedReaperEntry {
    Pending,
    Running(OpenCodeRetainedReaperTask),
}

#[derive(Debug)]
struct OpenCodeRetainedReaperRegistryState {
    next_task_id: u64,
    next_drain_epoch: u64,
    active_drain_epoch: Option<u64>,
    entries: BTreeMap<u64, OpenCodeRetainedReaperEntry>,
}

#[derive(Debug)]
struct OpenCodeRetainedReaper {
    permits: Arc<Semaphore>,
    registry: Mutex<OpenCodeRetainedReaperRegistryState>,
    changed: Arc<tokio::sync::Notify>,
    drain_epoch: watch::Sender<Option<u64>>,
}

#[derive(Debug)]
struct OpenCodePendingReaperRegistration {
    reaper: Arc<OpenCodeRetainedReaper>,
    task_id: u64,
    promoted: bool,
}

impl Default for OpenCodeRetainedReaper {
    fn default() -> Self {
        let (drain_epoch, _) = watch::channel(None);
        Self {
            permits: Arc::new(Semaphore::new(OPENCODE_HELPER_REAPER_CAPACITY)),
            registry: Mutex::new(OpenCodeRetainedReaperRegistryState {
                next_task_id: 1,
                next_drain_epoch: 1,
                active_drain_epoch: None,
                entries: BTreeMap::new(),
            }),
            changed: Arc::new(tokio::sync::Notify::new()),
            drain_epoch,
        }
    }
}

impl OpenCodeRetainedReaper {
    fn reserve(&self) -> Result<OwnedSemaphorePermit, String> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| "OpenCode helper cleanup capacity is exhausted".to_owned())
    }

    fn reserve_pending(self: &Arc<Self>) -> OpenCodePendingReaperRegistration {
        let terminal_tasks = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry
                .entries
                .iter()
                .filter_map(|(task_id, entry)| match entry {
                    OpenCodeRetainedReaperEntry::Running(task)
                        if task.completed.load(Ordering::Acquire) =>
                    {
                        Some((*task_id, task.clone()))
                    }
                    OpenCodeRetainedReaperEntry::Pending
                    | OpenCodeRetainedReaperEntry::Running(_) => None,
                })
                .collect::<Vec<_>>()
        };
        let joined = terminal_tasks
            .into_iter()
            .filter_map(|(task_id, task)| {
                task.try_join_finished()
                    .then(|| (task_id, task.join.clone()))
            })
            .collect::<Vec<_>>();
        let task_id = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (task_id, join) in joined {
                let remove = matches!(
                    registry.entries.get(&task_id),
                    Some(OpenCodeRetainedReaperEntry::Running(task))
                        if task.join_succeeded.load(Ordering::Acquire)
                            && Arc::ptr_eq(&task.join, &join)
                );
                if remove {
                    registry.entries.remove(&task_id);
                }
            }
            if registry.entries.is_empty() && registry.active_drain_epoch.take().is_some() {
                self.drain_epoch.send_replace(None);
            }
            let task_id = loop {
                let task_id = registry.next_task_id;
                registry.next_task_id = registry.next_task_id.wrapping_add(1);
                if !registry.entries.contains_key(&task_id) {
                    break task_id;
                }
            };
            let replaced = registry
                .entries
                .insert(task_id, OpenCodeRetainedReaperEntry::Pending);
            debug_assert!(replaced.is_none(), "new retained reaper task ID is vacant");
            task_id
        };
        self.changed.notify_waiters();
        OpenCodePendingReaperRegistration {
            reaper: self.clone(),
            task_id,
            promoted: false,
        }
    }

    fn submit(
        self: &Arc<Self>,
        child: Box<dyn ChildWrapper>,
        process_group_id: Option<i32>,
        permit: OwnedSemaphorePermit,
        process_registration: Option<ProcessRegistration>,
        stdout_task: Option<JoinHandle<()>>,
        #[cfg(test)] fixture_events: Option<OpenCodeHelperFixtureEvents>,
    ) -> watch::Receiver<bool> {
        let registration = self.reserve_pending();
        self.submit_reserved(
            registration,
            OpenCodeReapSubmission {
                child,
                process_group_id,
                permit,
                process_registration,
                stdout_task,
                #[cfg(test)]
                fixture_events,
            },
        )
    }

    fn submit_reserved(
        self: &Arc<Self>,
        mut registration: OpenCodePendingReaperRegistration,
        submission: OpenCodeReapSubmission,
    ) -> watch::Receiver<bool> {
        let OpenCodeReapSubmission {
            child,
            process_group_id,
            permit,
            process_registration,
            stdout_task,
            #[cfg(test)]
            fixture_events,
        } = submission;
        let cleanup = SystemOpenCodeReapGuard {
            child: Some(child),
            #[cfg(unix)]
            process_group_id,
        };
        assert!(
            Arc::ptr_eq(self, &registration.reaper),
            "OpenCode retained reaper registration belongs to a different registry"
        );
        let (foreground_done, foreground_wait) = watch::channel(false);
        let completed = Arc::new(AtomicBool::new(false));
        let task_completed = completed.clone();
        let failure = Arc::new(Mutex::new(None));
        let task_failure = failure.clone();
        let changed = self.changed.clone();
        let mut drain_epoch = self.drain_epoch.subscribe();
        #[cfg(test)]
        let ownership_registered = fixture_events
            .as_ref()
            .and_then(|events| events.stdout_join.as_ref())
            .map(|events| events.ownership_registered.clone());
        let join = tokio::spawn(async move {
            let _permit = permit;
            let _process_registration = process_registration;
            let mut cleanup = cleanup;
            let mut consumed_drain_epoch = None;
            if let Some(stdout_task) = stdout_task {
                #[cfg(test)]
                let retain_stdout_join = fixture_events
                    .as_ref()
                    .and_then(|events| events.stdout_join.as_ref())
                    .is_some();
                #[cfg(not(test))]
                let retain_stdout_join = false;
                if !retain_stdout_join {
                    stdout_task.abort();
                }
                let _ = stdout_task.await;
            }
            let foreground_reap = reap_opencode_helper(
                &mut cleanup,
                process_group_id,
                &foreground_done,
                #[cfg(test)]
                fixture_events.as_ref(),
            )
            .await;
            let mut wait_failed = match foreground_reap {
                OpenCodeForegroundReap::Reaped => {
                    #[cfg(test)]
                    if let Some(events) = fixture_events.as_ref() {
                        events.reaped.publish();
                    }
                    foreground_done.send_replace(true);
                    task_completed.store(true, Ordering::Release);
                    changed.notify_waiters();
                    return;
                }
                OpenCodeForegroundReap::WaitFailed(error) => {
                    let first_failure = {
                        let mut failure = task_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let first_failure = failure.is_none();
                        *failure = Some(error);
                        first_failure
                    };
                    #[cfg(test)]
                    if let Some(wait_error) = fixture_events
                        .as_ref()
                        .and_then(|events| events.wait_error.as_ref())
                    {
                        wait_error.recorded.publish();
                    }
                    if first_failure {
                        changed.notify_waiters();
                    }
                    true
                }
                #[cfg(test)]
                OpenCodeForegroundReap::TimedOut => false,
            };
            #[cfg(test)]
            if let Some(reap_timeout) = fixture_events
                .as_ref()
                .and_then(|events| events.reap_timeout.as_ref())
            {
                reap_timeout.background_wait_started.publish();
                reap_timeout
                    .foreground_return_release
                    .wait_after_bounded(0, "the test to release the foreground reap return")
                    .await;
            }
            foreground_done.send_replace(true);
            #[cfg(test)]
            if let Some(reap_timeout) = fixture_events
                .as_ref()
                .and_then(|events| events.reap_timeout.as_ref())
            {
                reap_timeout
                    .background_wait_release
                    .wait_after_bounded(0, "the test to release the background reap wait")
                    .await;
            }
            loop {
                if wait_failed {
                    let retry_delay = tokio::time::sleep(OPENCODE_HELPER_WAIT_RETRY_DELAY);
                    tokio::pin!(retry_delay);
                    loop {
                        let active_drain_epoch = *drain_epoch.borrow_and_update();
                        if active_drain_epoch.is_some()
                            && active_drain_epoch != consumed_drain_epoch
                        {
                            consumed_drain_epoch = active_drain_epoch;
                            #[cfg(test)]
                            if let Some(wait_error) = fixture_events
                                .as_ref()
                                .and_then(|events| events.wait_error.as_ref())
                            {
                                wait_error.retry_started.publish();
                            }
                            break;
                        }
                        tokio::select! {
                            _ = &mut retry_delay => {
                                #[cfg(test)]
                                if let Some(wait_error) = fixture_events
                                    .as_ref()
                                    .and_then(|events| events.wait_error.as_ref())
                                {
                                    wait_error.retry_started.publish();
                                }
                                break;
                            }
                            changed = drain_epoch.changed() => {
                                if changed.is_err() {
                                    std::future::pending::<()>().await;
                                }
                            }
                        }
                    }
                }
                match wait_opencode_child(
                    cleanup.child_mut(),
                    #[cfg(test)]
                    fixture_events.as_ref(),
                )
                .await
                {
                    Ok(_) => {
                        *task_failure
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                        break;
                    }
                    Err(error) => {
                        let first_failure = {
                            let mut failure = task_failure
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let first_failure = failure.is_none();
                            *failure = Some(format!("OpenCode helper wait failed: {error}"));
                            first_failure
                        };
                        #[cfg(test)]
                        if let Some(wait_error) = fixture_events
                            .as_ref()
                            .and_then(|events| events.wait_error.as_ref())
                        {
                            wait_error.recorded.publish();
                        }
                        if first_failure {
                            changed.notify_waiters();
                        }
                        wait_failed = true;
                    }
                }
            }
            cleanup.disarm();
            #[cfg(test)]
            if let Some(events) = fixture_events.as_ref() {
                events.reaped.publish();
            }
            task_completed.store(true, Ordering::Release);
            changed.notify_waiters();
        });
        let mut retained_task = Some(OpenCodeRetainedReaperTask {
            join: Arc::new(tokio::sync::Mutex::new(Some(join))),
            join_succeeded: Arc::new(AtomicBool::new(false)),
            completed,
            failure,
        });
        let promotion_error = {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match registry.entries.get_mut(&registration.task_id) {
                Some(entry @ OpenCodeRetainedReaperEntry::Pending) => {
                    *entry = OpenCodeRetainedReaperEntry::Running(
                        retained_task
                            .take()
                            .expect("retained reaper task is promoted exactly once"),
                    );
                    registration.promoted = true;
                    None
                }
                Some(OpenCodeRetainedReaperEntry::Running(_)) => {
                    Some("OpenCode retained reaper pending registration was already promoted")
                }
                None => Some(
                    "OpenCode retained reaper pending registration disappeared before promotion",
                ),
            }
        };
        if let Some(message) = promotion_error {
            let retained_task = retained_task
                .take()
                .expect("failed retained reaper promotion keeps the spawned task");
            let mut join = retained_task
                .join
                .try_lock()
                .expect("unpublished retained reaper join owner is uncontended");
            join.take()
                .expect("failed retained reaper promotion keeps its join handle")
                .abort();
            panic!("{message}");
        }
        self.changed.notify_waiters();
        #[cfg(test)]
        if let Some(ownership_registered) = ownership_registered {
            ownership_registered.publish();
        }
        foreground_wait
    }

    async fn shutdown(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let completed = {
                let registry = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                registry
                    .entries
                    .iter()
                    .filter_map(|(task_id, entry)| match entry {
                        OpenCodeRetainedReaperEntry::Running(task)
                            if task.completed.load(Ordering::Acquire) =>
                        {
                            Some((*task_id, task.clone()))
                        }
                        OpenCodeRetainedReaperEntry::Pending
                        | OpenCodeRetainedReaperEntry::Running(_) => None,
                    })
                    .collect::<Vec<_>>()
            };
            let mut joined = Vec::with_capacity(completed.len());
            for (task_id, task) in completed {
                if task.join_completed().await {
                    joined.push((task_id, task.join));
                }
            }
            let (failure, linearized_empty) = {
                let mut registry = self
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for (task_id, join) in joined {
                    let remove = matches!(
                        registry.entries.get(&task_id),
                        Some(OpenCodeRetainedReaperEntry::Running(task))
                            if task.join_succeeded.load(Ordering::Acquire)
                                && Arc::ptr_eq(&task.join, &join)
                    );
                    if remove {
                        registry.entries.remove(&task_id);
                    }
                }
                let failure = registry.entries.values().find_map(|entry| match entry {
                    OpenCodeRetainedReaperEntry::Pending => None,
                    OpenCodeRetainedReaperEntry::Running(task) => task
                        .failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone(),
                });
                let linearized_empty = registry.entries.is_empty();
                if linearized_empty {
                    if registry.active_drain_epoch.take().is_some() {
                        self.drain_epoch.send_replace(None);
                    }
                } else if registry.active_drain_epoch.is_none() {
                    let drain_epoch = registry.next_drain_epoch;
                    registry.next_drain_epoch = registry
                        .next_drain_epoch
                        .checked_add(1)
                        .expect("OpenCode retained reaper drain epoch exhausted");
                    registry.active_drain_epoch = Some(drain_epoch);
                    self.drain_epoch.send_replace(Some(drain_epoch));
                }
                (failure, linearized_empty)
            };
            if linearized_empty {
                return;
            }
            if let Some(error) = failure {
                tracing::warn!(%error, "OpenCode helper retained reaper drain is waiting for a successful kernel wait");
            }
            notified.await;
        }
    }
}

struct OpenCodeReapSubmission {
    child: Box<dyn ChildWrapper>,
    process_group_id: Option<i32>,
    permit: OwnedSemaphorePermit,
    process_registration: Option<ProcessRegistration>,
    stdout_task: Option<JoinHandle<()>>,
    #[cfg(test)]
    fixture_events: Option<OpenCodeHelperFixtureEvents>,
}

impl Drop for OpenCodePendingReaperRegistration {
    fn drop(&mut self) {
        if self.promoted {
            return;
        }
        let removed = {
            let mut registry = self
                .reaper
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let removed = matches!(
                registry.entries.get(&self.task_id),
                Some(OpenCodeRetainedReaperEntry::Pending)
            );
            if removed {
                registry.entries.remove(&self.task_id);
                if registry.entries.is_empty() && registry.active_drain_epoch.take().is_some() {
                    self.reaper.drain_epoch.send_replace(None);
                }
            }
            removed
        };
        if removed {
            self.reaper.changed.notify_waiters();
        }
    }
}

impl Drop for SystemOpenCodeHelperProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[derive(Debug)]
struct SystemOpenCodeRemoteClientFactory;

impl OpenCodeRemoteClientFactory for SystemOpenCodeRemoteClientFactory {
    fn connect(
        &self,
        endpoint: &str,
        username: &str,
        password: &str,
        directory: &Path,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn OpenCodeRemoteClient>, String>> + Send + '_>>
    {
        let endpoint = endpoint.to_owned();
        let username = username.to_owned();
        let password = password.to_owned();
        let directory = directory.to_path_buf();
        Box::pin(async move {
            if !valid_loopback_endpoint(&endpoint) {
                return Err("OpenCode endpoint is not an authenticated loopback URL".to_owned());
            }
            let authorization = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            );
            let mut authorization = reqwest::header::HeaderValue::from_str(&authorization)
                .map_err(|_| "OpenCode authorization header is invalid".to_owned())?;
            authorization.set_sensitive(true);
            let mut directory_header =
                reqwest::header::HeaderValue::from_str(&directory.to_string_lossy())
                    .map_err(|_| "OpenCode directory header is invalid".to_owned())?;
            directory_header.set_sensitive(true);
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::AUTHORIZATION, authorization);
            headers.insert("x-opencode-directory", directory_header);
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .default_headers(headers)
                .connect_timeout(OPENCODE_REQUEST_TIMEOUT)
                .build()
                .map_err(|error| format!("failed to build OpenCode client: {error}"))?;
            let mut remote = SystemOpenCodeRemoteClient {
                client,
                endpoint,
                directory,
                protocol: opencode::OpenCodeProtocol::V1,
            };
            let valid_version = |version: Option<&str>| {
                version.is_some_and(|version| {
                    parse_opencode_version(version).as_deref() == Some(version)
                })
            };
            // OpenCode 2 answers V1 paths with its web UI, which is not JSON.
            match remote
                .request_json(reqwest::Method::GET, &["global", "health"], None)
                .await
            {
                Ok(health) => {
                    if health.get("healthy").and_then(Value::as_bool) != Some(true)
                        || !valid_version(health.get("version").and_then(Value::as_str))
                    {
                        return Err("OpenCode helper health was invalid".to_owned());
                    }
                }
                Err(_) => {
                    let info = remote
                        .request_json(reqwest::Method::GET, &["api", "info"], None)
                        .await?;
                    let version = info.get("version").and_then(Value::as_str);
                    if !valid_version(version) || !version.is_some_and(|v| v.starts_with("2.")) {
                        return Err("OpenCode helper health was invalid".to_owned());
                    }
                    remote.protocol = opencode::OpenCodeProtocol::V2;
                }
            }
            Ok(Box::new(remote) as Box<dyn OpenCodeRemoteClient>)
        })
    }
}

struct SystemOpenCodeRemoteClient {
    client: reqwest::Client,
    endpoint: String,
    directory: PathBuf,
    /// OpenCode 2 serves `/api/*`; its answers are mapped onto the OpenCode 1 shapes the
    /// activity tracker consumes.
    protocol: opencode::OpenCodeProtocol,
}

struct SystemOpenCodeEventStream {
    response: reqwest::Response,
    sse_decoder: OpenCodeSseDecoder,
}

impl fmt::Debug for SystemOpenCodeRemoteClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SystemOpenCodeRemoteClient")
            .field("endpoint", &self.endpoint)
            .field("directory", &self.directory)
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

impl SystemOpenCodeRemoteClient {
    fn is_v2(&self) -> bool {
        self.protocol == opencode::OpenCodeProtocol::V2
    }

    /// A request whose answer body does not matter.
    async fn request_ok(
        &self,
        method: reqwest::Method,
        segments: &[&str],
        body: Option<Value>,
    ) -> Result<(), String> {
        tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, async {
            let mut request = self.client.request(method, self.url(segments)?);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let response = request
                .send()
                .await
                .map_err(|_| "OpenCode request failed".to_owned())?;
            if !response.status().is_success() {
                return Err(format!(
                    "OpenCode request failed with {}",
                    response.status()
                ));
            }
            Ok(())
        })
        .await
        .map_err(|_| "OpenCode request timed out".to_owned())?
    }

    async fn get_url_json(&self, url: url::Url) -> Result<Value, String> {
        tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, async {
            let response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|_| "OpenCode request failed".to_owned())?;
            if !response.status().is_success() {
                return Err(format!(
                    "OpenCode request failed with {}",
                    response.status()
                ));
            }
            read_bounded_json(response).await
        })
        .await
        .map_err(|_| "OpenCode request timed out".to_owned())?
    }

    fn url(&self, segments: &[&str]) -> Result<url::Url, String> {
        let mut url =
            url::Url::parse(&self.endpoint).map_err(|_| "invalid OpenCode endpoint".to_owned())?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|()| "invalid OpenCode endpoint path".to_owned())?;
            path.pop_if_empty();
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    async fn request_json(
        &self,
        method: reqwest::Method,
        segments: &[&str],
        body: Option<Value>,
    ) -> Result<Value, String> {
        tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, async {
            let mut request = self.client.request(method, self.url(segments)?);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let response = request
                .send()
                .await
                .map_err(|_| "OpenCode request failed".to_owned())?;
            if !response.status().is_success() {
                return Err(format!(
                    "OpenCode request failed with {}",
                    response.status()
                ));
            }
            read_bounded_json(response).await
        })
        .await
        .map_err(|_| "OpenCode request timed out".to_owned())?
    }
}

impl SystemOpenCodeEventStream {
    async fn read_chunk(&mut self) -> Result<(), String> {
        let chunk = self
            .response
            .chunk()
            .await
            .map_err(|_| "OpenCode SSE read failed".to_owned())?
            .ok_or_else(|| "OpenCode SSE ended".to_owned())?;
        self.sse_decoder.push(&chunk)
    }
}

impl OpenCodeEventStream for SystemOpenCodeEventStream {
    fn discard_next(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                match self.sse_decoder.discard_event() {
                    Ok(true) => return Ok(()),
                    Ok(false) => self.read_chunk().await?,
                    Err(error) => return Err(error),
                }
            }
        })
    }

    fn next_data(&mut self) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let buffered_length = self.sse_decoder.buffered_len();
                match self.sse_decoder.take_data() {
                    Ok(Some(data)) => return Ok(data),
                    Ok(None) if self.sse_decoder.buffered_len() < buffered_length => continue,
                    Ok(None) => self.read_chunk().await?,
                    Err(error) => return Err(error),
                }
            }
        })
    }
}

/// OpenCode 2's event stream as the OpenCode 1 events the activity tracker consumes. One V2 event
/// can stand for several V1 events, or for none.
struct OpenCodeV2EventStream {
    inner: SystemOpenCodeEventStream,
    normalizer: opencode::v2_activity::V2ActivityNormalizer,
    pending: VecDeque<Vec<u8>>,
}

impl OpenCodeEventStream for OpenCodeV2EventStream {
    fn discard_next(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            if self.pending.pop_front().is_some() {
                return Ok(());
            }
            self.inner.discard_next().await
        })
    }

    fn next_data(&mut self) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                if let Some(data) = self.pending.pop_front() {
                    return Ok(data);
                }
                let data = self.inner.next_data().await?;
                // Data that is not JSON goes on as is, for the consumer to reject.
                let Ok(event) = serde_json::from_slice::<Value>(&data) else {
                    return Ok(data);
                };
                self.pending.extend(
                    self.normalizer
                        .normalize(&event)
                        .iter()
                        .filter_map(|event| serde_json::to_vec(event).ok()),
                );
            }
        })
    }
}

impl OpenCodeRemoteClient for SystemOpenCodeRemoteClient {
    fn create_root(
        &mut self,
        model: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let model = model.to_owned();
        Box::pin(async move {
            let (provider_id, id) = model
                .split_once('/')
                .ok_or_else(|| "OpenCode model is not provider-qualified".to_owned())?;
            let model = json!({
                "providerID": provider_id,
                "id": id,
            });
            if self.is_v2() {
                let response = self
                    .request_json(
                        reqwest::Method::POST,
                        &["api", "session"],
                        Some(json!({
                            "location": { "directory": self.directory.to_string_lossy() },
                            "model": model,
                        })),
                    )
                    .await?;
                return response
                    .pointer("/data/id")
                    .and_then(Value::as_str)
                    .filter(|id| valid_native_id(id))
                    .map(str::to_owned)
                    .ok_or_else(|| "OpenCode root response did not contain a valid ID".to_owned());
            }
            let response = self
                .request_json(
                    reqwest::Method::POST,
                    &["session"],
                    Some(json!({ "model": model })),
                )
                .await?;
            response
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| valid_native_id(id))
                .map(str::to_owned)
                .ok_or_else(|| "OpenCode root response did not contain a valid ID".to_owned())
        })
    }

    fn cleanup_pre_spawn(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let root_session_id = root_session_id.to_owned();
        Box::pin(async move {
            if self.is_v2() {
                return self
                    .request_ok(
                        reqwest::Method::DELETE,
                        &["api", "session", &root_session_id],
                        None,
                    )
                    .await;
            }
            self.request_json(
                reqwest::Method::DELETE,
                &["session", &root_session_id],
                None,
            )
            .await
            .map(|_| ())
        })
    }

    fn abort(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let root_session_id = root_session_id.to_owned();
        Box::pin(async move {
            if self.is_v2() {
                return self
                    .request_ok(
                        reqwest::Method::POST,
                        &["api", "session", &root_session_id, "interrupt"],
                        None,
                    )
                    .await;
            }
            self.request_json(
                reqwest::Method::POST,
                &["session", &root_session_id, "abort"],
                None,
            )
            .await
            .map(|_| ())
        })
    }

    fn root(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let root_session_id = root_session_id.to_owned();
        Box::pin(async move {
            if self.is_v2() {
                let response = self
                    .request_json(
                        reqwest::Method::GET,
                        &["api", "session", &root_session_id],
                        None,
                    )
                    .await?;
                return Ok(response.get("data").cloned().unwrap_or(Value::Null));
            }
            self.request_json(reqwest::Method::GET, &["session", &root_session_id], None)
                .await
        })
    }

    fn statuses(&mut self) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        Box::pin(async move {
            if self.is_v2() {
                let response = self
                    .request_json(reqwest::Method::GET, &["api", "session", "active"], None)
                    .await?;
                return Ok(opencode::v2_activity::statuses(&response));
            }
            self.request_json(reqwest::Method::GET, &["session", "status"], None)
                .await
        })
    }

    fn children(
        &mut self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let session_id = session_id.to_owned();
        Box::pin(async move {
            if self.is_v2() {
                let mut url = self.url(&["api", "session"])?;
                url.query_pairs_mut()
                    .append_pair("parentID", &session_id)
                    .append_pair("limit", &MAX_RECONCILED_CHILDREN.to_string());
                let response = self.get_url_json(url).await?;
                return Ok(opencode::v2_activity::children(&response));
            }
            self.request_json(
                reqwest::Method::GET,
                &["session", &session_id, "children"],
                None,
            )
            .await
        })
    }

    fn messages(
        &mut self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let session_id = session_id.to_owned();
        Box::pin(async move {
            if self.is_v2() {
                let mut url = self.url(&["api", "session", &session_id, "message"])?;
                url.query_pairs_mut().append_pair("limit", "200");
                let response = self.get_url_json(url).await?;
                return Ok(opencode::v2_activity::history(&session_id, &response));
            }
            let mut url = self.url(&["session", &session_id, "message"])?;
            url.query_pairs_mut().append_pair("limit", "200");
            tokio::time::timeout(OPENCODE_REQUEST_TIMEOUT, async {
                let response = self
                    .client
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| "OpenCode request failed".to_owned())?;
                if !response.status().is_success() {
                    return Err(format!(
                        "OpenCode request failed with {}",
                        response.status()
                    ));
                }
                read_bounded_json(response).await
            })
            .await
            .map_err(|_| "OpenCode request timed out".to_owned())?
        })
    }

    fn open_event_stream(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn OpenCodeEventStream>, String>> + Send + '_>>
    {
        Box::pin(async move {
            let url = if self.is_v2() {
                self.url(&["api", "event"])?
            } else {
                let mut url = self.url(&["event"])?;
                url.query_pairs_mut()
                    .append_pair("directory", &self.directory.to_string_lossy());
                url
            };
            let response = tokio::time::timeout(
                OPENCODE_REQUEST_TIMEOUT,
                self.client
                    .get(url)
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .send(),
            )
            .await
            .map_err(|_| "OpenCode SSE connection timed out".to_owned())?
            .map_err(|_| "OpenCode SSE connection failed".to_owned())?;
            if !response.status().is_success()
                || !response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with("text/event-stream"))
            {
                return Err("OpenCode SSE endpoint rejected the observer".to_owned());
            }
            let stream = SystemOpenCodeEventStream {
                response,
                sse_decoder: OpenCodeSseDecoder::default(),
            };
            if self.is_v2() {
                return Ok(Box::new(OpenCodeV2EventStream {
                    inner: stream,
                    normalizer: opencode::v2_activity::V2ActivityNormalizer::default(),
                    pending: VecDeque::new(),
                }) as Box<dyn OpenCodeEventStream>);
            }
            Ok(Box::new(stream) as Box<dyn OpenCodeEventStream>)
        })
    }
}

async fn read_bounded_json(mut response: reqwest::Response) -> Result<Value, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "OpenCode response read failed".to_owned())?
    {
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > OPENCODE_HTTP_BODY_LIMIT)
        {
            return Err("OpenCode response exceeded its bound".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "OpenCode response was invalid JSON".to_owned())
}

#[cfg(test)]
mod tests {

    #[cfg(all(feature = "hermetic-test-guard", unix))]
    #[tokio::test]
    async fn hermetic_guard_refuses_direct_opencode_probe_and_helper_before_spawn() {
        let Some(executable) = crate::hermetic_guard::refusal_fixture(
            "provider_terminal::opencode::tests::hermetic_guard_refuses_direct_opencode_probe_and_helper_before_spawn",
            "2.1.220",
        ) else {
            return;
        };
        assert!(
            SystemOpenCodeCapabilityProbeRunner
                .run(&executable, vec![])
                .await
                .is_err()
        );
        let launch = OpenCodeHelperLaunch {
            executable: executable.display().to_string(),
            args: vec![],
            cwd: executable.parent().unwrap().to_path_buf(),
            env: BTreeMap::new(),
            process_attribution: ProcessAttributionRegistry::new(),
        };
        assert!(
            SystemOpenCodeHelperLauncher::default()
                .start(launch)
                .await
                .is_err()
        );
    }
    use std::{convert::Infallible, process::ExitStatus, sync::atomic::AtomicUsize};

    use axum::{
        Router,
        body::{Body, Bytes, to_bytes},
        extract::{Request, State},
        http::{Method, Response, StatusCode, header},
        routing::any,
    };
    use futures_util::{StreamExt, stream};

    use super::*;
    use crate::test_support::within_fixture_deadline;
    #[cfg(unix)]
    use crate::test_support::{FixtureEvent, TestSandbox};

    #[cfg(unix)]
    type OpenCodeHelperStart = JoinHandle<Result<OpenCodeHelperReady, String>>;

    /// Waits for `event` to pass `checkpoint` while the helper start task
    /// runs. A start that ends first, for example because the helper could
    /// not spawn, fails the test with its own result instead of leaving the
    /// wait to hang.
    #[cfg(unix)]
    async fn wait_during_start(
        start: &mut OpenCodeHelperStart,
        event: &FixtureEvent,
        checkpoint: u64,
        what: &str,
    ) {
        within_fixture_deadline(what, async {
            tokio::select! {
                biased;
                () = event.wait_after(checkpoint) => {}
                result = &mut *start => {
                    panic!("the OpenCode helper start task ended before {what}: {result:?}")
                }
            }
        })
        .await;
    }

    #[cfg(unix)]
    async fn join_start(start: OpenCodeHelperStart) -> Result<OpenCodeHelperReady, String> {
        within_fixture_deadline("the OpenCode helper start task to finish", start)
            .await
            .expect("OpenCode helper start task")
    }

    /// Builds a launch that executes a sandbox script directly.
    #[cfg(unix)]
    fn sandbox_script_launch(
        sandbox: &TestSandbox,
        script: &Path,
        process_attribution: ProcessAttributionRegistry,
    ) -> OpenCodeHelperLaunch {
        OpenCodeHelperLaunch {
            executable: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: sandbox.root().to_path_buf(),
            env: sandbox.environment(std::iter::empty::<(String, String)>()),
            process_attribution,
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn system_helper_launcher_retries_while_its_executable_is_busy() {
        let sandbox = TestSandbox::new("opencode-helper-busy-executable");
        let script = sandbox.executable_script(
            "helper",
            "printf 'opencode server listening on http://127.0.0.1:43127\\n'\nexec sleep 3600",
            "",
        );
        let writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&script)
            .expect("hold the helper executable open for writing");
        let launcher = Arc::new(SystemOpenCodeHelperLauncher::default());
        let launch = sandbox_script_launch(&sandbox, &script, ProcessAttributionRegistry::new());
        let start = tokio::spawn({
            let launcher = launcher.clone();
            async move { launcher.start(launch).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !start.is_finished(),
            "spawn must keep retrying while the script is busy: {:?}",
            join_start(start).await
        );
        drop(writer);

        let ready = join_start(start)
            .await
            .expect("spawn must succeed once the writer closes");
        within_fixture_deadline(
            "the retried OpenCode helper to be reaped",
            ready.process.terminate_and_reap(),
        )
        .await;
        within_fixture_deadline(
            "the retried OpenCode helper launcher to shut down",
            launcher.shutdown(),
        )
        .await;
        assert_eq!(ready.endpoint, "http://127.0.0.1:43127");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn system_helper_launcher_stops_retrying_a_permanently_busy_executable() {
        let sandbox = TestSandbox::new("opencode-helper-permanently-busy-executable");
        let script = sandbox.executable_script(
            "helper",
            "printf 'opencode server listening on http://127.0.0.1:43127\\n'\nexec sleep 3600",
            "",
        );
        let _writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&script)
            .expect("keep the helper executable open for writing");
        let launcher = SystemOpenCodeHelperLauncher::default();
        let launch = sandbox_script_launch(&sandbox, &script, ProcessAttributionRegistry::new());
        let started = std::time::Instant::now();
        let error = within_fixture_deadline(
            "the permanently busy OpenCode helper to exhaust its spawn retry budget",
            tokio::time::timeout(Duration::from_secs(2), launcher.start(launch)),
        )
        .await
        .expect("spawn retries must stop before the three-second readiness timeout")
        .expect_err("a permanently busy executable must not spawn");
        let elapsed = started.elapsed();
        within_fixture_deadline(
            "the busy OpenCode helper launcher to shut down",
            launcher.shutdown(),
        )
        .await;

        assert!(
            error.starts_with("failed to start OpenCode helper:")
                && error.contains("Text file busy"),
            "unexpected spawn error: {error}"
        );
        assert!(
            elapsed >= Duration::from_millis(500),
            "spawn must retry a busy executable before failing: {elapsed:?}; {error}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "spawn retries must use the one-second budget: {elapsed:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_capability_probe_ignores_appimage_environment() {
        crate::test_support::check_capability_probe_appimage_environment(
            "provider_terminal::opencode::tests::system_capability_probe_ignores_appimage_environment",
            |executable, args| async move {
                let output = SystemOpenCodeCapabilityProbeRunner
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
            "provider_terminal::opencode::tests::system_helper_launcher_ignores_appimage_environment",
            ISOLATING_AND_NO_OP_CASES,
            |expected| {
                let sandbox = TestSandbox::new("opencode-helper-appimage-environment");
                let output_path = sandbox.path("environment");
                let executable = sandbox.executable_script(
                    "helper",
                    "/usr/bin/env -0 > \"$1\"\nprintf 'opencode server listening on http://127.0.0.1:43127\\n'\nexec /bin/sleep 3600",
                    "",
                );
                crate::test_support::run_on_current_thread(async {
                    let launcher = SystemOpenCodeHelperLauncher::default();
                    let ready = within_fixture_deadline(
                        "the system OpenCode helper to become ready",
                        launcher.start(OpenCodeHelperLaunch {
                            executable: executable.to_string_lossy().into_owned(),
                            args: vec![output_path.to_string_lossy().into_owned()],
                            cwd: sandbox.root().to_path_buf(),
                            env: BTreeMap::new(),
                            process_attribution: ProcessAttributionRegistry::new(),
                        }),
                    )
                    .await
                    .expect("OpenCode system helper launcher");
                    within_fixture_deadline(
                        "the system OpenCode helper to be reaped",
                        ready.process.terminate_and_reap(),
                    )
                    .await;
                    within_fixture_deadline(
                        "the system OpenCode helper launcher to shut down",
                        launcher.shutdown(),
                    )
                    .await;
                    assert_eq!(ready.endpoint, "http://127.0.0.1:43127");
                });
                assert_child_environment(
                    &std::fs::read(output_path).expect("OpenCode helper child environment"),
                    expected,
                );
            },
        );
    }

    #[derive(Debug)]
    struct CancellationSensitiveWaitState {
        wait_calls: AtomicUsize,
        cancelled_waits: AtomicUsize,
        wait_started: watch::Sender<bool>,
        release: watch::Sender<bool>,
    }

    impl CancellationSensitiveWaitState {
        fn new() -> Self {
            let (wait_started, _) = watch::channel(false);
            let (release, _) = watch::channel(false);
            Self {
                wait_calls: AtomicUsize::new(0),
                cancelled_waits: AtomicUsize::new(0),
                wait_started,
                release,
            }
        }
    }

    #[derive(Debug)]
    struct CancellationSensitiveWaitChild {
        state: Arc<CancellationSensitiveWaitState>,
    }

    impl ChildWrapper for CancellationSensitiveWaitChild {
        fn inner(&self) -> &dyn ChildWrapper {
            self
        }

        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            self
        }

        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            self
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn wait(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<ExitStatus>> + Send + '_>> {
            self.state.wait_calls.fetch_add(1, Ordering::SeqCst);
            self.state.wait_started.send_replace(true);
            let mut release = self.state.release.subscribe();
            let cancelled_waits = &self.state.cancelled_waits;
            Box::pin(async move {
                struct CancellationGuard<'a> {
                    cancelled_waits: &'a AtomicUsize,
                    completed: bool,
                }

                impl Drop for CancellationGuard<'_> {
                    fn drop(&mut self) {
                        if !self.completed {
                            self.cancelled_waits.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }

                let mut guard = CancellationGuard {
                    cancelled_waits,
                    completed: false,
                };
                while !*release.borrow() {
                    release.changed().await.map_err(|_| {
                        std::io::Error::other("cancellation-sensitive wait release closed")
                    })?;
                }
                guard.completed = true;
                Ok(successful_exit_status())
            })
        }
    }

    fn successful_exit_status() -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            ExitStatus::from_raw(0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt as _;
            ExitStatus::from_raw(0)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retained_reaper_continues_the_same_wait_after_its_foreground_budget() {
        let reaper = Arc::new(OpenCodeRetainedReaper::default());
        let state = Arc::new(CancellationSensitiveWaitState::new());
        let mut wait_started = state.wait_started.subscribe();
        let mut foreground_done = reaper.submit(
            Box::new(CancellationSensitiveWaitChild {
                state: state.clone(),
            }),
            None,
            reaper.reserve().expect("reserve retained reaper capacity"),
            None,
            None,
            None,
        );

        within_fixture_deadline("the fake child wait to start", async {
            while !*wait_started.borrow_and_update() {
                wait_started
                    .changed()
                    .await
                    .expect("wait-start publisher remains live");
            }
        })
        .await;
        tokio::time::advance(OPENCODE_HELPER_TERM_GRACE).await;
        assert_eq!(state.wait_calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(OPENCODE_HELPER_REAP_TIMEOUT).await;
        within_fixture_deadline("the foreground reap budget to complete", async {
            while !*foreground_done.borrow_and_update() {
                foreground_done
                    .changed()
                    .await
                    .expect("foreground completion publisher remains live");
            }
        })
        .await;

        assert_eq!(state.wait_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.cancelled_waits.load(Ordering::SeqCst), 0);

        state.release.send_replace(true);
        within_fixture_deadline("the retained reaper to shut down", reaper.shutdown()).await;
        assert_eq!(state.cancelled_waits.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    #[derive(Debug)]
    struct InvalidReadinessFixture {
        sandbox: TestSandbox,
        executable: PathBuf,
        pid_path: PathBuf,
        events: OpenCodeHelperFixtureEvents,
        pair_started: Arc<tokio::sync::Barrier>,
    }

    #[cfg(unix)]
    fn invalid_readiness_fixture(
        sandbox: TestSandbox,
        pair_started: Arc<tokio::sync::Barrier>,
    ) -> InvalidReadinessFixture {
        let pid_path = sandbox.path("helper.pid");
        let executable = sandbox.executable_script(
            "invalid-opencode-helper",
            "printf 'opencode server listening on http://0.0.0.0:43127\\n'\nexec sleep 3600",
            "",
        );
        InvalidReadinessFixture {
            sandbox,
            executable,
            pid_path: pid_path.clone(),
            events: OpenCodeHelperFixtureEvents {
                spawned: Arc::new(FixtureEvent::default()),
                release: Arc::new(FixtureEvent::default()),
                admission_race: None,
                reaped: Arc::new(FixtureEvent::default()),
                pid_path,
                reap_timeout: None,
                stdout_join: None,
                wait_error: None,
            },
            pair_started,
        }
    }

    #[cfg(unix)]
    impl InvalidReadinessFixture {
        async fn start_and_release(&self) -> (String, i32) {
            let launcher = SystemOpenCodeHelperLauncher::with_fixture_events(
                OPENCODE_HELPER_READY_TIMEOUT,
                self.events.clone(),
            );
            let launch = sandbox_script_launch(
                &self.sandbox,
                &self.executable,
                ProcessAttributionRegistry::new(),
            );
            let mut start = tokio::spawn(async move { launcher.start(launch).await });
            wait_during_start(
                &mut start,
                &self.events.spawned,
                0,
                "the invalid-readiness helper PID publication",
            )
            .await;
            let pid = std::fs::read_to_string(&self.pid_path)
                .expect("OpenCode helper PID")
                .parse::<i32>()
                .expect("numeric OpenCode helper PID");
            within_fixture_deadline(
                "the paired invalid-readiness helper to publish its PID",
                self.pair_started.wait(),
            )
            .await;
            self.events.release.publish();
            let error = join_start(start)
                .await
                .expect_err("invalid OpenCode readiness must fail");
            (error, pid)
        }

        async fn wait_reaped(&self) {
            self.events
                .reaped
                .wait_after_bounded(0, "the invalid-readiness helper to be reaped")
                .await;
        }
    }

    #[cfg(unix)]
    struct RetainedReapFixture {
        _sandbox: TestSandbox,
        pid_path: PathBuf,
        events: OpenCodeHelperFixtureEvents,
        timeout_events: OpenCodeHelperReapTimeoutEvents,
        launcher: Arc<SystemOpenCodeHelperLauncher>,
        launch: OpenCodeHelperLaunch,
    }

    #[cfg(unix)]
    struct PreparedRetainedSubmission {
        child: Box<dyn ChildWrapper>,
        process_group_id: Option<i32>,
        permit: OwnedSemaphorePermit,
        pid: u32,
    }

    #[cfg(unix)]
    fn retained_reap_fixture(name: &str) -> RetainedReapFixture {
        let sandbox = TestSandbox::new(name);
        let pid_path = sandbox.path("helper.pid");
        let executable = sandbox.executable_script(
            "invalid-opencode-helper",
            "printf 'opencode server listening on http://0.0.0.0:43127\\n'\nexec sleep 3600",
            "",
        );
        let timeout_events = OpenCodeHelperReapTimeoutEvents {
            foreground_wait_started: Arc::new(FixtureEvent::default()),
            foreground_return_release: Arc::new(FixtureEvent::default()),
            background_wait_started: Arc::new(FixtureEvent::default()),
            background_wait_release: Arc::new(FixtureEvent::default()),
        };
        let events = OpenCodeHelperFixtureEvents {
            spawned: Arc::new(FixtureEvent::default()),
            release: Arc::new(FixtureEvent::default()),
            admission_race: None,
            reaped: Arc::new(FixtureEvent::default()),
            pid_path: pid_path.clone(),
            reap_timeout: Some(timeout_events.clone()),
            stdout_join: None,
            wait_error: None,
        };
        let launcher = Arc::new(SystemOpenCodeHelperLauncher::with_fixture_events(
            OPENCODE_HELPER_READY_TIMEOUT,
            events.clone(),
        ));
        let launch =
            sandbox_script_launch(&sandbox, &executable, ProcessAttributionRegistry::new());
        RetainedReapFixture {
            _sandbox: sandbox,
            pid_path,
            events,
            timeout_events,
            launcher,
            launch,
        }
    }

    #[cfg(unix)]
    impl RetainedReapFixture {
        fn with_stdout_join(mut self, stdout_join: OpenCodeHelperStdoutJoinEvents) -> Self {
            Arc::get_mut(&mut self.launcher)
                .expect("fixture launcher is uniquely owned")
                .fixture_events
                .as_mut()
                .expect("fixture events")
                .stdout_join = Some(stdout_join);
            self
        }

        fn with_wait_error(mut self, wait_error: OpenCodeHelperWaitErrorEvents) -> Self {
            Arc::get_mut(&mut self.launcher)
                .expect("fixture launcher is uniquely owned")
                .fixture_events
                .as_mut()
                .expect("fixture events")
                .wait_error = Some(wait_error);
            self
        }

        fn without_reap_timeout(mut self) -> Self {
            self.events.reap_timeout = None;
            Arc::get_mut(&mut self.launcher)
                .expect("fixture launcher is uniquely owned")
                .fixture_events
                .as_mut()
                .expect("fixture events")
                .reap_timeout = None;
            self
        }

        async fn prepare_retained_submission(&self) -> PreparedRetainedSubmission {
            let permit = self
                .launcher
                .reaper
                .reserve()
                .expect("retained submission permit");
            self.spawn_prepared_retained_submission(permit)
        }

        async fn prepare_retained_submission_waiting_for_permit(
            &self,
        ) -> PreparedRetainedSubmission {
            let permit = within_fixture_deadline(
                "a retained cleanup permit to return",
                self.launcher.reaper.permits.clone().acquire_owned(),
            )
            .await
            .expect("retained submission semaphore remains open");
            self.spawn_prepared_retained_submission(permit)
        }

        fn spawn_prepared_retained_submission(
            &self,
            permit: OwnedSemaphorePermit,
        ) -> PreparedRetainedSubmission {
            let mut command = CommandWrap::with_new("/bin/sleep", |command| {
                command
                    .arg("3600")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
            });
            configure_supervised_background_command_wrap(&mut command);
            let child = command.spawn().expect("retained submission child");
            let pid = child.id().expect("retained submission child PID");
            PreparedRetainedSubmission {
                child,
                process_group_id: i32::try_from(pid).ok(),
                permit,
                pid,
            }
        }

        fn submit_reserved_real_child(
            &self,
            registration: OpenCodePendingReaperRegistration,
            prepared: PreparedRetainedSubmission,
        ) -> watch::Receiver<bool> {
            self.launcher.reaper.submit_reserved(
                registration,
                OpenCodeReapSubmission {
                    child: prepared.child,
                    process_group_id: prepared.process_group_id,
                    permit: prepared.permit,
                    process_registration: None,
                    stdout_task: None,
                    fixture_events: Some(
                        self.launcher
                            .fixture_events
                            .clone()
                            .expect("retained fixture events"),
                    ),
                },
            )
        }

        fn release_foreground_and_background_waits(&self) {
            self.timeout_events.foreground_return_release.publish();
            self.timeout_events.background_wait_release.publish();
        }

        async fn reach_background_owner(
            &self,
        ) -> (
            tokio::task::JoinHandle<Result<OpenCodeHelperReady, String>>,
            u32,
        ) {
            let launcher = self.launcher.clone();
            let launch = self.launch.clone();
            let mut start = tokio::spawn(async move { launcher.start(launch).await });
            wait_during_start(
                &mut start,
                &self.events.spawned,
                0,
                "the retained helper PID publication",
            )
            .await;
            let pid = std::fs::read_to_string(&self.pid_path)
                .expect("OpenCode helper PID")
                .parse::<u32>()
                .expect("numeric OpenCode helper PID");
            self.events.release.publish();
            wait_during_start(
                &mut start,
                &self.timeout_events.foreground_wait_started,
                0,
                "the foreground reap attempt to start",
            )
            .await;
            wait_during_start(
                &mut start,
                &self.timeout_events.background_wait_started,
                0,
                "the retained reaper to take ownership",
            )
            .await;
            (start, pid)
        }

        async fn assert_reaped(&self, pid: u32) {
            self.events
                .reaped
                .wait_after_bounded(0, "the retained helper to be reaped")
                .await;
            assert!(matches!(
                waitid_child_once(pid),
                Err(error) if error.raw_os_error() == Some(libc::ECHILD)
            ));
            assert_eq!(
                self.launcher.reaper.permits.available_permits(),
                OPENCODE_HELPER_REAPER_CAPACITY,
                "every retained cleanup permit must return after the final wait"
            );
            let registry = self
                .launcher
                .reaper
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                registry.entries.is_empty(),
                "retained registry must be empty after shutdown joins completion"
            );
            assert_eq!(
                registry.active_drain_epoch, None,
                "empty retained registry must reset its drain epoch"
            );
        }
    }

    #[cfg(unix)]
    async fn next_active_drain_epoch(epoch: &mut watch::Receiver<Option<u64>>) -> u64 {
        within_fixture_deadline(
            "the retained reaper to publish an active drain epoch",
            async {
                loop {
                    if let Some(epoch) = *epoch.borrow_and_update() {
                        return epoch;
                    }
                    epoch
                        .changed()
                        .await
                        .expect("drain epoch sender remains live");
                }
            },
        )
        .await
    }

    #[cfg(unix)]
    async fn next_completed_join_owner(
        reaper: &Arc<OpenCodeRetainedReaper>,
    ) -> (u64, Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>) {
        within_fixture_deadline("a retained reaper task to complete", async {
            loop {
                let notified = reaper.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let completed = {
                    let registry = reaper
                        .registry
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    registry.entries.iter().find_map(|(task_id, entry)| {
                        let OpenCodeRetainedReaperEntry::Running(task) = entry else {
                            return None;
                        };
                        task.completed
                            .load(Ordering::Acquire)
                            .then(|| (*task_id, task.join.clone()))
                    })
                };
                if let Some(completed) = completed {
                    return completed;
                }
                notified.await;
            }
        })
        .await
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn completed_submissions_are_pruned_without_shutdown() {
        let fixture =
            retained_reap_fixture("opencode-completed-submission-pruning").without_reap_timeout();
        let submission_count = OPENCODE_HELPER_REAPER_CAPACITY * 2;
        let mut process_ids = Vec::with_capacity(submission_count);
        let mut reaped_checkpoint = fixture.events.reaped.checkpoint();

        for _ in 0..submission_count {
            let prepared = fixture
                .prepare_retained_submission_waiting_for_permit()
                .await;
            process_ids.push(prepared.pid);
            let registration = fixture.launcher.reaper.reserve_pending();
            assert!(
                fixture
                    .launcher
                    .reaper
                    .registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entries
                    .len()
                    <= OPENCODE_HELPER_REAPER_CAPACITY,
                "each normal reservation keeps retained records within live capacity"
            );
            let mut foreground_done = fixture.submit_reserved_real_child(registration, prepared);
            fixture
                .events
                .reaped
                .wait_after_bounded(reaped_checkpoint, "the retained submission to be reaped")
                .await;
            reaped_checkpoint = fixture.events.reaped.checkpoint();
            within_fixture_deadline("the retained foreground reap to complete", async {
                while !*foreground_done.borrow_and_update() {
                    foreground_done
                        .changed()
                        .await
                        .expect("retained foreground completion sender remains live");
                }
            })
            .await;
        }
        let mut returned_permits = Vec::with_capacity(OPENCODE_HELPER_REAPER_CAPACITY);
        for _ in 0..OPENCODE_HELPER_REAPER_CAPACITY {
            returned_permits.push(
                within_fixture_deadline(
                    "every retained cleanup permit to return",
                    fixture.launcher.reaper.permits.clone().acquire_owned(),
                )
                .await
                .expect("all retained cleanup permits return"),
            );
        }

        let registry = fixture
            .launcher
            .reaper
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            registry.entries.len() <= OPENCODE_HELPER_REAPER_CAPACITY,
            "terminal retained records must remain bounded by live cleanup capacity"
        );
        drop(registry);
        for pid in process_ids {
            assert!(matches!(
                waitid_child_once(pid),
                Err(error) if error.raw_os_error() == Some(libc::ECHILD)
            ));
        }
        drop(returned_permits);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_completed_join_keeps_shared_registry_owner() {
        let fixture =
            retained_reap_fixture("opencode-cancelled-completed-join").without_reap_timeout();
        let prepared = fixture.prepare_retained_submission().await;
        let pid = prepared.pid;
        let registration = fixture.launcher.reaper.reserve_pending();
        let _foreground_done = fixture.submit_reserved_real_child(registration, prepared);
        let (task_id, join_owner) = next_completed_join_owner(&fixture.launcher.reaper).await;
        let join_guard =
            within_fixture_deadline("the completed task's join owner lock", join_owner.lock())
                .await;
        assert!(join_guard.is_some(), "registry owns the terminal task join");

        let mut cancelled_shutdown = Box::pin(fixture.launcher.reaper.shutdown());
        let first_poll = std::future::poll_fn(|context| {
            std::task::Poll::Ready(cancelled_shutdown.as_mut().poll(context))
        })
        .await;
        assert!(
            first_poll.is_pending(),
            "shutdown waits at the completed-entry join boundary"
        );
        {
            let registry = fixture
                .launcher
                .reaper
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                registry.entries.contains_key(&task_id),
                "the registry remains the shared join owner while shutdown waits"
            );
        }
        drop(cancelled_shutdown);
        assert!(
            join_guard.is_some(),
            "cancelling shutdown leaves the join handle in its shared owner"
        );
        drop(join_guard);

        let shutdowns = (0..8)
            .map(|_| {
                let launcher = fixture.launcher.clone();
                tokio::spawn(async move { launcher.shutdown().await })
            })
            .collect::<Vec<_>>();
        for shutdown in shutdowns {
            within_fixture_deadline("a concurrent replacement shutdown to finish", shutdown)
                .await
                .expect("concurrent replacement shutdown task");
        }
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_observes_pending_submission_and_new_task_consumes_active_epoch() {
        let wait_error = OpenCodeHelperWaitErrorEvents {
            failures_remaining: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
            fail_persistently: Arc::new(AtomicBool::new(false)),
            injected: Arc::new(FixtureEvent::default()),
            recorded: Arc::new(FixtureEvent::default()),
            retry_started: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-pending-drain-epoch")
            .with_wait_error(wait_error.clone());
        let prepared = fixture.prepare_retained_submission().await;
        let pid = prepared.pid;
        let registration = fixture.launcher.reaper.reserve_pending();
        let mut epoch = fixture.launcher.reaper.drain_epoch.subscribe();
        let shutdown = tokio::spawn({
            let launcher = fixture.launcher.clone();
            async move { launcher.shutdown().await }
        });
        let active_epoch = next_active_drain_epoch(&mut epoch).await;
        assert!(active_epoch > 0, "drain epochs start with a positive value");
        assert!(
            !shutdown.is_finished(),
            "pending registration is live drain work"
        );

        let _foreground_done = fixture.submit_reserved_real_child(registration, prepared);
        fixture.release_foreground_and_background_waits();
        wait_error
            .injected
            .wait_after_bounded(0, "the first injected wait failure")
            .await;
        wait_error
            .retry_started
            .wait_after_bounded(0, "the drain-epoch retry to start")
            .await;
        assert_eq!(
            wait_error.retry_started.checkpoint(),
            1,
            "a task promoted after epoch publication consumes that epoch once"
        );

        within_fixture_deadline("the retained reaper shutdown to finish", shutdown)
            .await
            .expect("retained reaper shutdown task");
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dropped_pending_registration_unblocks_shutdown_and_releases_its_child() {
        let fixture = retained_reap_fixture("opencode-dropped-pending-registration");
        let prepared = fixture.prepare_retained_submission().await;
        let pid = prepared.pid;
        let registration = fixture.launcher.reaper.reserve_pending();
        let mut epoch = fixture.launcher.reaper.drain_epoch.subscribe();
        let shutdown = tokio::spawn({
            let launcher = fixture.launcher.clone();
            async move { launcher.shutdown().await }
        });
        let _active_epoch = next_active_drain_epoch(&mut epoch).await;
        assert!(
            !shutdown.is_finished(),
            "an armed pending registration keeps shutdown live"
        );

        drop(registration);
        within_fixture_deadline("the pending-rollback shutdown to finish", shutdown)
            .await
            .expect("pending rollback shutdown task");
        assert_eq!(
            *epoch.borrow(),
            None,
            "pending rollback resets the active drain epoch"
        );

        drop(prepared);
        within_fixture_deadline("the rolled-back prepared child to be reaped", async {
            loop {
                if matches!(
                    waitid_child_once(pid),
                    Err(error) if error.raw_os_error() == Some(libc::ECHILD)
                ) {
                    return;
                }
                // Tokio reaps dropped children from its orphan queue only after a driver turn.
                // A yield_now loop cannot complete one while an idle worker holds the driver.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert_eq!(
            fixture.launcher.reaper.permits.available_permits(),
            OPENCODE_HELPER_REAPER_CAPACITY,
            "pending rollback releases its reserved cleanup permit"
        );
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn foreground_wait_error_obeys_the_finite_retry_boundary() {
        let wait_error = OpenCodeHelperWaitErrorEvents {
            failures_remaining: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            fail_persistently: Arc::new(AtomicBool::new(true)),
            injected: Arc::new(FixtureEvent::default()),
            recorded: Arc::new(FixtureEvent::default()),
            retry_started: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-foreground-wait-error")
            .with_wait_error(wait_error.clone())
            .without_reap_timeout();
        let prepared = fixture.prepare_retained_submission().await;
        let pid = prepared.pid;
        let registration = fixture.launcher.reaper.reserve_pending();
        let _foreground_done = fixture.submit_reserved_real_child(registration, prepared);

        wait_error
            .injected
            .wait_after_bounded(0, "the foreground injected wait failure")
            .await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            1,
            "a foreground wait failure must not start an ungated second wait"
        );
        tokio::time::advance(Duration::from_millis(99)).await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            1,
            "the foreground wait failure remains parked through 99 ms"
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        wait_error
            .retry_started
            .wait_after_bounded(0, "the 100 ms wait retry to start")
            .await;
        wait_error
            .injected
            .wait_after_bounded(1, "the retried injected wait failure")
            .await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            2,
            "the foreground wait failure retries once at 100 ms"
        );

        wait_error.fail_persistently.store(false, Ordering::Release);
        tokio::time::advance(Duration::from_millis(100)).await;
        within_fixture_deadline(
            "the launcher shutdown to drain the retained child",
            fixture.launcher.shutdown(),
        )
        .await;
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn foreground_timeout_transfers_exact_child_before_error_return() {
        let fixture = retained_reap_fixture("opencode-background-transfer");
        let (start, pid) = fixture.reach_background_owner().await;
        assert_eq!(fixture.events.reaped.checkpoint(), 0);
        fixture.timeout_events.foreground_return_release.publish();

        let error = join_start(start)
            .await
            .expect_err("invalid endpoint must fail");
        assert!(
            error == "OpenCode helper advertised an invalid endpoint"
                || error == "OpenCode helper readiness timed out"
        );
        fixture.timeout_events.background_wait_release.publish();
        within_fixture_deadline(
            "the retained reaper shutdown to finish",
            fixture.launcher.shutdown(),
        )
        .await;
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborting_foreground_waiter_does_not_lose_reaper_ownership() {
        let fixture = retained_reap_fixture("opencode-aborted-waiter");
        let (start, pid) = fixture.reach_background_owner().await;
        start.abort();
        assert!(
            within_fixture_deadline("the aborted foreground waiter to finish", start)
                .await
                .expect_err("foreground waiter is aborted")
                .is_cancelled()
        );

        fixture.timeout_events.foreground_return_release.publish();
        fixture.timeout_events.background_wait_release.publish();
        within_fixture_deadline(
            "the retained reaper to drain after the waiter abort",
            fixture.launcher.shutdown(),
        )
        .await;
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn launcher_shutdown_waits_for_retained_child_true_reap() {
        let fixture = retained_reap_fixture("opencode-shutdown-drain");
        let (start, pid) = fixture.reach_background_owner().await;
        fixture.timeout_events.foreground_return_release.publish();
        let _ = join_start(start).await;

        let shutdown = fixture.launcher.shutdown();
        tokio::pin!(shutdown);
        let first_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(shutdown.as_mut().poll(context)))
                .await;
        assert!(
            first_poll.is_pending(),
            "shutdown must retain the pending reap"
        );
        fixture.timeout_events.background_wait_release.publish();
        within_fixture_deadline(
            "the launcher shutdown to drain the retained child",
            shutdown,
        )
        .await;
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_during_stdout_join_cannot_preempt_registry_ownership() {
        let stdout_join = OpenCodeHelperStdoutJoinEvents {
            ownership_registered: Arc::new(FixtureEvent::default()),
            join_started: Arc::new(FixtureEvent::default()),
            join_release: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-stdout-join-transfer")
            .with_stdout_join(stdout_join.clone());
        let launcher = fixture.launcher.clone();
        let launch = fixture.launch.clone();
        let mut start = tokio::spawn(async move { launcher.start(launch).await });
        wait_during_start(
            &mut start,
            &fixture.events.spawned,
            0,
            "the stdout-join helper PID publication",
        )
        .await;
        let pid = std::fs::read_to_string(&fixture.pid_path)
            .expect("OpenCode helper PID")
            .parse::<u32>()
            .expect("numeric OpenCode helper PID");
        fixture.events.release.publish();
        wait_during_start(
            &mut start,
            &stdout_join.ownership_registered,
            0,
            "registry ownership before the stdout join",
        )
        .await;
        wait_during_start(
            &mut start,
            &stdout_join.join_started,
            0,
            "the retained stdout join to begin",
        )
        .await;

        start.abort();
        assert!(
            within_fixture_deadline("the aborted launch waiter to finish", start)
                .await
                .expect_err("launch waiter abort")
                .is_cancelled()
        );
        assert_eq!(fixture.events.reaped.checkpoint(), 0);
        stdout_join.join_release.publish();
        fixture.timeout_events.foreground_return_release.publish();
        fixture.timeout_events.background_wait_release.publish();
        within_fixture_deadline(
            "the shutdown to drain the stdout-join transfer",
            fixture.launcher.shutdown(),
        )
        .await;
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn transient_wait_error_recovers_automatically_at_retry_boundary() {
        let wait_error = OpenCodeHelperWaitErrorEvents {
            failures_remaining: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
            fail_persistently: Arc::new(AtomicBool::new(false)),
            injected: Arc::new(FixtureEvent::default()),
            recorded: Arc::new(FixtureEvent::default()),
            retry_started: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-transient-wait-error")
            .with_wait_error(wait_error.clone());
        let (start, pid) = fixture.reach_background_owner().await;
        fixture.timeout_events.foreground_return_release.publish();
        let _ = join_start(start).await;
        fixture.timeout_events.background_wait_release.publish();
        wait_error
            .injected
            .wait_after_bounded(0, "the transient injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(0, "the transient wait failure to be recorded")
            .await;

        assert_eq!(fixture.events.reaped.checkpoint(), 0);
        tokio::time::advance(Duration::from_millis(99)).await;
        assert_eq!(
            wait_error.retry_started.checkpoint(),
            0,
            "retry must not hot-loop before its backoff expires"
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_error
            .retry_started
            .wait_after_bounded(0, "the 100 ms wait retry to start")
            .await;
        within_fixture_deadline(
            "the launcher shutdown to drain the retained child",
            fixture.launcher.shutdown(),
        )
        .await;
        fixture.assert_reaped(pid).await;
        assert_eq!(
            wait_error.retry_started.checkpoint(),
            1,
            "a retained transient wait failure retries automatically at its boundary"
        );
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn persistent_wait_error_retries_at_finite_cadence_during_shutdown() {
        let wait_error = OpenCodeHelperWaitErrorEvents {
            failures_remaining: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            fail_persistently: Arc::new(AtomicBool::new(true)),
            injected: Arc::new(FixtureEvent::default()),
            recorded: Arc::new(FixtureEvent::default()),
            retry_started: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-persistent-wait-error")
            .with_wait_error(wait_error.clone());
        let (start, pid) = fixture.reach_background_owner().await;
        fixture.timeout_events.foreground_return_release.publish();
        let _ = join_start(start).await;
        fixture.timeout_events.background_wait_release.publish();
        wait_error
            .injected
            .wait_after_bounded(0, "the first persistent injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(0, "the first persistent wait failure to be recorded")
            .await;

        let mut epoch = fixture.launcher.reaper.drain_epoch.subscribe();
        let launcher = fixture.launcher.clone();
        let shutdown = tokio::spawn(async move { launcher.shutdown().await });
        let active_epoch = next_active_drain_epoch(&mut epoch).await;
        assert!(
            active_epoch > 0,
            "shutdown publishes a positive drain epoch"
        );
        wait_error
            .retry_started
            .wait_after_bounded(0, "the immediate drain-epoch retry to start")
            .await;
        wait_error
            .injected
            .wait_after_bounded(1, "the immediate retry's injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(1, "the immediate retry's wait failure to be recorded")
            .await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            2,
            "shutdown may drive one immediate retry but not a hot loop"
        );

        assert_eq!(
            wait_error.injected.checkpoint(),
            2,
            "persistent failure must stay parked without time or a new signal"
        );
        tokio::time::advance(Duration::from_millis(99)).await;
        assert_eq!(wait_error.injected.checkpoint(), 2);
        let retry_checkpoint = wait_error.retry_started.checkpoint();
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_error
            .retry_started
            .wait_after_bounded(retry_checkpoint, "the 100 ms cadence retry to start")
            .await;
        wait_error
            .injected
            .wait_after_bounded(2, "the cadence retry's injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(2, "the cadence retry's wait failure to be recorded")
            .await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            3,
            "persistent failure retries once at the finite cadence"
        );

        wait_error.fail_persistently.store(false, Ordering::Release);
        tokio::time::advance(Duration::from_millis(100)).await;
        within_fixture_deadline("the retained reaper shutdown to finish", shutdown)
            .await
            .expect("retained reaper shutdown task");
        fixture.assert_reaped(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn empty_registry_reset_starts_a_distinct_drain_epoch() {
        let wait_error = OpenCodeHelperWaitErrorEvents {
            failures_remaining: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            fail_persistently: Arc::new(AtomicBool::new(true)),
            injected: Arc::new(FixtureEvent::default()),
            recorded: Arc::new(FixtureEvent::default()),
            retry_started: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-distinct-drain-epochs")
            .with_wait_error(wait_error.clone());
        fixture.release_foreground_and_background_waits();
        let mut epoch = fixture.launcher.reaper.drain_epoch.subscribe();

        let first = fixture.prepare_retained_submission().await;
        let first_pid = first.pid;
        let first_registration = fixture.launcher.reaper.reserve_pending();
        let _first_foreground = fixture.submit_reserved_real_child(first_registration, first);
        wait_error
            .injected
            .wait_after_bounded(0, "the first submission's injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(0, "the first submission's wait failure to be recorded")
            .await;

        let first_shutdown = tokio::spawn({
            let launcher = fixture.launcher.clone();
            async move { launcher.shutdown().await }
        });
        let first_epoch = next_active_drain_epoch(&mut epoch).await;
        wait_error
            .retry_started
            .wait_after_bounded(0, "the first drain epoch's retry to start")
            .await;
        wait_error
            .injected
            .wait_after_bounded(1, "the first drain epoch's injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(1, "the first drain epoch's wait failure to be recorded")
            .await;
        wait_error.fail_persistently.store(false, Ordering::Release);
        tokio::time::advance(Duration::from_millis(100)).await;
        within_fixture_deadline(
            "the first retained reaper shutdown to finish",
            first_shutdown,
        )
        .await
        .expect("first retained reaper shutdown task");
        fixture.assert_reaped(first_pid).await;
        assert_eq!(
            *epoch.borrow(),
            None,
            "empty-state linearization resets the published drain epoch"
        );

        wait_error.fail_persistently.store(true, Ordering::Release);
        let second = fixture.prepare_retained_submission().await;
        let second_pid = second.pid;
        let second_registration = fixture.launcher.reaper.reserve_pending();
        let injected_before_second = wait_error.injected.checkpoint();
        let recorded_before_second = wait_error.recorded.checkpoint();
        let retry_before_second = wait_error.retry_started.checkpoint();
        let _second_foreground = fixture.submit_reserved_real_child(second_registration, second);
        wait_error
            .injected
            .wait_after_bounded(
                injected_before_second,
                "the second submission's injected wait failure",
            )
            .await;
        wait_error
            .recorded
            .wait_after_bounded(
                recorded_before_second,
                "the second submission's wait failure to be recorded",
            )
            .await;

        let second_shutdown = tokio::spawn({
            let launcher = fixture.launcher.clone();
            async move { launcher.shutdown().await }
        });
        let second_epoch = next_active_drain_epoch(&mut epoch).await;
        assert!(
            second_epoch > first_epoch,
            "a later non-empty registry phase receives a distinct epoch"
        );
        wait_error
            .retry_started
            .wait_after_bounded(
                retry_before_second,
                "the second drain epoch's retry to start",
            )
            .await;
        wait_error
            .injected
            .wait_after_bounded(
                injected_before_second + 1,
                "the second drain epoch's injected wait failure",
            )
            .await;
        wait_error
            .recorded
            .wait_after_bounded(
                recorded_before_second + 1,
                "the second drain epoch's wait failure to be recorded",
            )
            .await;
        let injected_after_immediate_retry = wait_error.injected.checkpoint();

        let mut repeated_shutdowns = Vec::new();
        for _ in 0..8 {
            let mut shutdown = Box::pin(fixture.launcher.reaper.shutdown());
            let first_poll = std::future::poll_fn(|context| {
                std::task::Poll::Ready(shutdown.as_mut().poll(context))
            })
            .await;
            assert!(first_poll.is_pending(), "second drain phase remains live");
            assert_eq!(
                *epoch.borrow(),
                Some(second_epoch),
                "repeated shutdown callers reuse the active epoch"
            );
            repeated_shutdowns.push(shutdown);
        }
        tokio::time::advance(Duration::from_millis(99)).await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            injected_after_immediate_retry,
            "repeated callers cannot add a second immediate retry in one epoch"
        );

        wait_error.fail_persistently.store(false, Ordering::Release);
        let retry_before_cadence = wait_error.retry_started.checkpoint();
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_error
            .retry_started
            .wait_after_bounded(retry_before_cadence, "the 100 ms cadence retry to start")
            .await;
        within_fixture_deadline(
            "the second retained reaper shutdown to finish",
            second_shutdown,
        )
        .await
        .expect("second retained reaper shutdown task");
        for shutdown in repeated_shutdowns {
            within_fixture_deadline("a repeated shutdown caller to finish", shutdown).await;
        }
        fixture.assert_reaped(second_pid).await;
    }

    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn concurrent_and_repeated_shutdowns_coalesce_one_immediate_wait_retry() {
        let wait_error = OpenCodeHelperWaitErrorEvents {
            failures_remaining: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            fail_persistently: Arc::new(AtomicBool::new(true)),
            injected: Arc::new(FixtureEvent::default()),
            recorded: Arc::new(FixtureEvent::default()),
            retry_started: Arc::new(FixtureEvent::default()),
        };
        let fixture = retained_reap_fixture("opencode-coalesced-shutdown-retry")
            .with_wait_error(wait_error.clone());
        let (start, pid) = fixture.reach_background_owner().await;
        fixture.timeout_events.foreground_return_release.publish();
        let _ = join_start(start).await;
        fixture.timeout_events.background_wait_release.publish();
        wait_error
            .injected
            .wait_after_bounded(0, "the first persistent injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(0, "the first persistent wait failure to be recorded")
            .await;

        let mut epoch = fixture.launcher.reaper.drain_epoch.subscribe();
        let mut initial_shutdown = Box::pin(fixture.launcher.reaper.shutdown());
        let first_poll = std::future::poll_fn(|context| {
            std::task::Poll::Ready(initial_shutdown.as_mut().poll(context))
        })
        .await;
        assert!(first_poll.is_pending(), "persistent wait keeps drain live");
        let active_epoch = next_active_drain_epoch(&mut epoch).await;
        wait_error
            .retry_started
            .wait_after_bounded(0, "the immediate drain-epoch retry to start")
            .await;
        wait_error
            .injected
            .wait_after_bounded(1, "the immediate retry's injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(1, "the immediate retry's wait failure to be recorded")
            .await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            2,
            "the active drain epoch permits one immediate retry"
        );

        drop(initial_shutdown);
        let mut repeated_shutdowns = Vec::new();
        for _ in 0..8 {
            let mut shutdown = Box::pin(fixture.launcher.reaper.shutdown());
            let first_poll = std::future::poll_fn(|context| {
                std::task::Poll::Ready(shutdown.as_mut().poll(context))
            })
            .await;
            assert!(first_poll.is_pending(), "repeated drain remains live");
            assert_eq!(
                *epoch.borrow(),
                Some(active_epoch),
                "replacement shutdown callers reuse the cancelled caller's epoch"
            );
            repeated_shutdowns.push(shutdown);
        }

        tokio::time::advance(Duration::from_millis(99)).await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            2,
            "replacement callers cannot bypass 99 ms of the fixed cadence"
        );
        let retry_checkpoint = wait_error.retry_started.checkpoint();
        tokio::time::advance(Duration::from_millis(1)).await;
        wait_error
            .retry_started
            .wait_after_bounded(retry_checkpoint, "the 100 ms cadence retry to start")
            .await;
        wait_error
            .injected
            .wait_after_bounded(2, "the cadence retry's injected wait failure")
            .await;
        wait_error
            .recorded
            .wait_after_bounded(2, "the cadence retry's wait failure to be recorded")
            .await;
        assert_eq!(
            wait_error.injected.checkpoint(),
            3,
            "persistent failure attempts exactly once at 100 ms"
        );

        wait_error.fail_persistently.store(false, Ordering::Release);
        tokio::time::advance(Duration::from_millis(100)).await;
        fixture
            .events
            .reaped
            .wait_after_bounded(0, "the retained helper to be reaped")
            .await;
        for shutdown in repeated_shutdowns {
            within_fixture_deadline("a repeated shutdown caller to finish", shutdown).await;
        }
        fixture.assert_reaped(pid).await;
    }

    #[derive(Debug, Default)]
    struct RecordingOpenCodeHelper {
        terminated: AtomicBool,
    }

    impl OpenCodeHelperProcess for RecordingOpenCodeHelper {
        fn terminate(&self) {
            self.terminated.store(true, Ordering::Release);
        }
    }

    #[derive(Debug, Default)]
    struct PausingOpenCodeHelper {
        terminate_calls: std::sync::atomic::AtomicUsize,
        terminate_and_reap_started: tokio::sync::Notify,
    }

    #[tokio::test]
    async fn cancellation_after_reconciliation_selection_preempts_live_work() {
        #[derive(Debug)]
        struct DropSignal(Arc<AtomicBool>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let activity = TerminalAgentActivityControl::enabled();
        let mut changes = activity.subscribe();
        let generation = TerminalObserverGeneration::new(
            "thread-opencode-live-work".to_owned(),
            "terminal-opencode-live-work".to_owned(),
        );
        let work_dropped = Arc::new(AtomicBool::new(false));
        let reconciliation = {
            let signal = DropSignal(work_dropped.clone());
            async move {
                let _signal = signal;
                std::future::pending::<()>().await;
            }
        };
        let observation = generation.observation();
        let waiting = wait_for_opencode_live_work(&mut changes, &observation, reconciliation);
        tokio::pin!(waiting);
        let first_poll =
            std::future::poll_fn(|context| std::task::Poll::Ready(waiting.as_mut().poll(context)))
                .await;
        assert!(first_poll.is_pending());

        generation.request_cancellation(super::super::TerminalObserverCancellationReason::Closed);
        let outcome = tokio::time::timeout(Duration::from_millis(100), waiting)
            .await
            .expect("cancellation interrupts live reconciliation");

        assert!(matches!(outcome, OpenCodeLiveWork::Cancelled));
        assert!(
            work_dropped.load(Ordering::Acquire),
            "cancelled reconciliation future is dropped immediately"
        );
    }

    #[test]
    fn disable_after_data_selection_rejects_work_before_decode() {
        let activity = TerminalAgentActivityControl::enabled();
        let generation = TerminalObserverGeneration::new(
            "thread-opencode-live-data".to_owned(),
            "terminal-opencode-live-data".to_owned(),
        );
        let selected_state = activity.snapshot();
        let selected_data = serde_json::to_vec(&json!({
            "type": "session.created",
            "properties": {
                "sessionID": "post-select-child",
                "info": {
                    "id": "post-select-child",
                    "parentID": "root-tui-session",
                    "title": "Must remain unpublished",
                    "time": {"created": 1}
                }
            }
        }))
        .expect("selected OpenCode data");
        activity.transition(false);
        let mut tracker = OpenCodeActivityTracker::new("root-tui-session");

        let outcome = decode_and_track_opencode_live_data(
            OpenCodeLiveFence {
                activity: &activity,
                generation: &generation.observation(),
                state: selected_state,
            },
            &mut tracker,
            &selected_data,
            1,
        );

        assert!(
            matches!(outcome, OpenCodeLiveData::Stale),
            "data selected before disable must be rejected before decode or tracker mutation"
        );
    }

    impl OpenCodeHelperProcess for PausingOpenCodeHelper {
        fn terminate(&self) {
            self.terminate_calls.fetch_add(1, Ordering::AcqRel);
        }

        fn terminate_and_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async move {
                self.terminate_and_reap_started.notify_one();
                std::future::pending::<()>().await;
            })
        }
    }

    #[cfg(unix)]
    fn exited_child_remains_waitable(process_id: u32) -> std::io::Result<bool> {
        let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `information` points to writable storage for `waitid`; P_PID
        // limits observation to the child owned by this test, and WNOWAIT keeps
        // its identity reserved until the helper performs group cleanup.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                process_id as libc::id_t,
                information.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful `waitid` initialized the siginfo storage. The
        // zeroed PID denotes that WNOHANG found no exited child yet.
        let information = unsafe { information.assume_init() };
        // SAFETY: `si_pid` reads the process field of initialized siginfo.
        Ok(unsafe { information.si_pid() } == process_id as libc::pid_t)
    }

    #[cfg(unix)]
    #[test]
    fn waitid_observation_retries_an_interrupted_system_call() {
        let process_id = 42_u32;
        let mut attempts = 0_u8;

        let exited = observe_child_exit_with(process_id, || {
            attempts = attempts.saturating_add(1);
            if attempts == 1 {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(Some(process_id as libc::pid_t))
            }
        })
        .expect("interrupted waitid observation is retried");

        assert!(exited, "the retried observation reports the child exit");
        assert_eq!(attempts, 2, "waitid must be retried exactly once here");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_helper_liveness_observes_an_actual_child_exit() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "sleep 30 & exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command.process_group(0);
        let child = command.spawn().expect("actual exited helper");
        let process_id = child.id().expect("actual helper process ID");
        let reaper = Arc::new(OpenCodeRetainedReaper::default());
        let helper = SystemOpenCodeHelperProcess {
            process_group_id: i32::try_from(process_id).ok(),
            process_group_identity_reserved: AtomicBool::new(true),
            child: Mutex::new(Some(Box::new(child))),
            process_registration: Mutex::new(None),
            stdout_task: Mutex::new(None),
            reaper_permit: Mutex::new(Some(reaper.reserve().expect("helper reap permit"))),
            reaper,
            fixture_events: None,
        };

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !helper.has_exited() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("system helper exit becomes observable");

        let waitable_before_cleanup = exited_child_remains_waitable(process_id);
        within_fixture_deadline(
            "the exited helper's group cleanup to reap it",
            helper.terminate_and_reap(),
        )
        .await;
        let waitable_after_cleanup = exited_child_remains_waitable(process_id);

        assert!(
            matches!(waitable_before_cleanup, Ok(true)),
            "liveness observation must not reap the group leader before group cleanup: \
             {waitable_before_cleanup:?}"
        );
        assert_eq!(
            waitable_after_cleanup
                .expect_err("group cleanup must finally reap the helper")
                .raw_os_error(),
            Some(libc::ECHILD),
            "the helper must become non-waitable only after group cleanup"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_helper_disables_group_signal_after_child_identity_is_unobservable() {
        let mut sentinel_command = tokio::process::Command::new("/bin/sleep");
        sentinel_command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        sentinel_command.process_group(0);
        let mut sentinel = sentinel_command.spawn().expect("sentinel process group");
        let sentinel_process_group = sentinel.id().expect("sentinel process group ID");

        let mut exited_command = tokio::process::Command::new("/bin/sh");
        exited_command
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let exited_child = exited_command.spawn().expect("exited helper leader");
        let exited_process_id = exited_child.id().expect("exited helper process ID");
        within_fixture_deadline("the exited helper leader to be reaped manually", async {
            let mut status = 0;
            loop {
                // SAFETY: the PID belongs to the direct child created
                // immediately above, and `status` points to writable
                // wait-status storage.
                let result = unsafe {
                    libc::waitpid(
                        exited_process_id as libc::pid_t,
                        std::ptr::addr_of_mut!(status),
                        libc::WNOHANG,
                    )
                };
                if result == exited_process_id as libc::pid_t {
                    return;
                }
                if result == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                }
                let error = std::io::Error::last_os_error();
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted,
                    "manual reap of the helper leader failed: {error}"
                );
            }
        })
        .await;

        let reaper = Arc::new(OpenCodeRetainedReaper::default());
        let helper = SystemOpenCodeHelperProcess {
            process_group_id: i32::try_from(sentinel_process_group).ok(),
            process_group_identity_reserved: AtomicBool::new(true),
            child: Mutex::new(Some(Box::new(exited_child))),
            process_registration: Mutex::new(None),
            stdout_task: Mutex::new(None),
            reaper_permit: Mutex::new(Some(reaper.reserve().expect("helper reap permit"))),
            reaper,
            fixture_events: None,
        };
        let observed_exited = helper.has_exited();

        let mut replacement_command = tokio::process::Command::new("/bin/sleep");
        replacement_command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let replacement = replacement_command
            .spawn()
            .expect("identity-safe replacement helper");
        *helper
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(replacement));
        helper.terminate();

        let sentinel_was_signaled =
            tokio::time::timeout(Duration::from_millis(250), sentinel.wait())
                .await
                .is_ok();
        if !sentinel_was_signaled {
            let _ = sentinel.start_kill();
            let _ = within_fixture_deadline("the killed sentinel to exit", sentinel.wait()).await;
        }
        within_fixture_deadline(
            "the replacement helper's retained reap to drain",
            helper.reaper.shutdown(),
        )
        .await;

        assert!(
            observed_exited,
            "an unobservable owned child must fail open as exited"
        );
        assert!(
            !sentinel_was_signaled,
            "losing the leader identity must disable signaling the stored numeric process group"
        );
    }

    #[tokio::test]
    async fn cancelling_cleanup_before_reap_preserves_exactly_one_fallback_owner() {
        let helper = Arc::new(PausingOpenCodeHelper::default());
        let resources = Arc::new(OpenCodeObserverResources {
            helper: helper.clone(),
            owned_root: Mutex::new(None),
            launched: AtomicBool::new(false),
            cleanup_state: AtomicU8::new(OPENCODE_CLEANUP_IDLE),
            ownership_transferred: tokio::sync::Notify::new(),
        });
        let cleanup_started = helper.terminate_and_reap_started.notified();
        let cleanup_resources = resources.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_resources.cleanup_helper().await;
        });
        within_fixture_deadline(
            "helper cleanup to start terminate-and-reap",
            cleanup_started,
        )
        .await;

        let completed_before_reap =
            resources.cleanup_state.load(Ordering::Acquire) == OPENCODE_CLEANUP_REAPED;
        cleanup.abort();
        let _ = within_fixture_deadline("the aborted cleanup task to finish", cleanup).await;
        drop(resources);

        assert!(
            !completed_before_reap,
            "cleanup must not publish completion before terminate-and-reap returns"
        );
        assert_eq!(
            helper.terminate_calls.load(Ordering::Acquire),
            1,
            "cancellation must leave exactly one fallback owner to terminate the helper"
        );
    }

    #[tokio::test]
    async fn ownership_transfer_before_cleanup_worker_poll_does_not_stop_helper() {
        let helper = Arc::new(RecordingOpenCodeHelper::default());
        let resources = Arc::new(OpenCodeObserverResources {
            helper: helper.clone(),
            owned_root: Mutex::new(None),
            launched: AtomicBool::new(true),
            cleanup_state: AtomicU8::new(OPENCODE_CLEANUP_IDLE),
            ownership_transferred: tokio::sync::Notify::new(),
        });
        let generation = TerminalObserverGeneration::new(
            "thread-transfer-before-poll".to_owned(),
            "terminal-transfer-before-poll".to_owned(),
        );

        resources.transfer_ownership();
        generation.request_cancellation(
            super::super::TerminalObserverCancellationReason::PreparationRejected,
        );
        within_fixture_deadline(
            "the pre-spawn monitor to observe the ownership transfer",
            monitor_opencode_pre_spawn(resources.clone(), generation.observation()),
        )
        .await;

        assert!(
            !helper.terminated.load(Ordering::Acquire),
            "a transfer notification emitted before the cleanup worker polls must not be lost"
        );
        assert_eq!(
            resources.cleanup_state.load(Ordering::Acquire),
            OPENCODE_CLEANUP_IDLE
        );
    }

    type RecordedRequest = (Method, String, reqwest::header::HeaderMap, Vec<u8>);

    #[derive(Clone, Debug, Default)]
    struct RecordedRequests(Arc<Mutex<Vec<RecordedRequest>>>);

    async fn opencode_fixture_handler(
        State(recorded): State<RecordedRequests>,
        request: Request,
    ) -> Response<Body> {
        let method = request.method().clone();
        let uri = request.uri().to_string();
        let headers = request.headers().clone();
        let body = to_bytes(request.into_body(), OPENCODE_HTTP_BODY_LIMIT)
            .await
            .unwrap_or_default()
            .to_vec();
        recorded
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((method.clone(), uri.clone(), headers, body));
        match (method, uri.as_str()) {
            (Method::GET, "/global/health") => json_response(json!({
                "healthy": true,
                "version": "1.18.4",
            })),
            (Method::POST, "/session") => json_response(json!({
                "id": "root-tui-session",
            })),
            (Method::GET, "/session/root-tui-session") => json_response(json!({
                "id": "root-tui-session",
            })),
            (Method::GET, "/session/child-session/message?limit=200") => {
                json_response(Value::Array(Vec::new()))
            }
            (Method::GET, uri) if uri.starts_with("/event?") => {
                let body = Body::from_stream(
                    stream::once(async {
                        tokio::time::sleep(Duration::from_millis(650)).await;
                        Ok::<Bytes, Infallible>(Bytes::from(vec![
                            b'x';
                            OPENCODE_SSE_EVENT_LIMIT + 1
                        ]))
                    })
                    .chain(stream::once(async {
                        Ok::<Bytes, Infallible>(Bytes::from_static(
                            b"data: {\"type\":\"question.asked\"}\n\
                              \ndata: {\"type\":\"server.connected\",\"properties\":{}}\n\n",
                        ))
                    })),
                );
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(body)
                    .expect("SSE response")
            }
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("not-found response"),
        }
    }

    fn json_response(value: Value) -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .expect("JSON response")
    }

    #[tokio::test]
    async fn system_client_scopes_auth_bounds_history_and_does_not_timeout_sse_lifetime() {
        let recorded = RecordedRequests::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("OpenCode fixture listener");
        let address = listener.local_addr().expect("OpenCode fixture address");
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .fallback(any(opencode_fixture_handler))
                    .with_state(recorded.clone()),
            )
            .into_future(),
        );
        let directory = tempfile::tempdir().expect("OpenCode fixture directory");
        let factory = SystemOpenCodeRemoteClientFactory;
        let mut remote = factory
            .connect(
                &format!("http://127.0.0.1:{}", address.port()),
                OPENCODE_USERNAME,
                "fixture-secret",
                directory.path(),
            )
            .await
            .expect("authenticated OpenCode client");

        assert_eq!(
            remote
                .create_root("openai/gpt-5.2")
                .await
                .expect("owned OpenCode root"),
            "root-tui-session"
        );
        assert_eq!(
            remote
                .root("root-tui-session")
                .await
                .expect("exact OpenCode root")["id"],
            "root-tui-session"
        );
        assert_eq!(
            remote
                .messages("child-session")
                .await
                .expect("bounded OpenCode messages"),
            Value::Array(Vec::new())
        );
        let started = std::time::Instant::now();
        let mut stream = remote
            .open_event_stream()
            .await
            .expect("long-lived OpenCode SSE");
        assert_eq!(
            within_fixture_deadline("the oversized SSE event", stream.next_data())
                .await
                .expect_err("oversized OpenCode SSE"),
            "OpenCode SSE event exceeded its bound"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(
                &within_fixture_deadline("the server.connected SSE event", stream.next_data())
                    .await
                    .expect("long-lived OpenCode SSE"),
            )
            .expect("OpenCode SSE JSON")["type"],
            "server.connected"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "SSE body lifetime must not inherit the 500 ms unary timeout"
        );

        {
            let requests = recorded
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(requests.iter().all(|(_, _, headers, _)| {
                headers
                    .get(reqwest::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some("Basic b3BlbmNvZGU6Zml4dHVyZS1zZWNyZXQ=")
            }));
            assert!(requests.iter().all(|(_, _, headers, _)| {
                headers
                    .get("x-opencode-directory")
                    .and_then(|value| value.to_str().ok())
                    == Some(directory.path().to_string_lossy().as_ref())
            }));
            let create = requests
                .iter()
                .find(|(method, uri, _, _)| *method == Method::POST && uri == "/session")
                .expect("OpenCode create-root request");
            assert_eq!(
                serde_json::from_slice::<Value>(&create.3).expect("create-root JSON")["model"],
                json!({
                    "providerID": "openai",
                    "id": "gpt-5.2",
                })
            );
        }
        server.abort();
        let _ = within_fixture_deadline("the aborted fixture server to stop", server).await;
    }

    /// An OpenCode 2 helper, shaped like 2.0.29: V1 paths answer its web UI, `/api/*` the API.
    async fn opencode_2_fixture_handler(
        State(recorded): State<RecordedRequests>,
        request: Request,
    ) -> Response<Body> {
        let method = request.method().clone();
        let uri = request.uri().to_string();
        let headers = request.headers().clone();
        let body = to_bytes(request.into_body(), OPENCODE_HTTP_BODY_LIMIT)
            .await
            .unwrap_or_default()
            .to_vec();
        recorded
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((method.clone(), uri.clone(), headers, body));
        match (method, uri.as_str()) {
            (Method::GET, "/global/health") => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html")
                .body(Body::from("<!doctype html><title>OpenCode</title>"))
                .expect("web UI response"),
            (Method::GET, "/api/info") => json_response(json!({ "version": "2.0.29" })),
            (Method::POST, "/api/session") => {
                json_response(json!({ "data": { "id": "ses_root" } }))
            }
            (Method::GET, "/api/session/ses_root") => {
                json_response(json!({ "data": { "id": "ses_root" } }))
            }
            (Method::GET, "/api/session/active") => {
                json_response(json!({ "data": { "ses_child": { "type": "running" } } }))
            }
            (Method::GET, uri) if uri.starts_with("/api/session?parentID=ses_root&limit=") => {
                json_response(json!({ "data": [{
                    "id": "ses_child",
                    "parentID": "ses_root",
                    "title": "Inspect repository",
                    "time": { "created": 1, "updated": 2 },
                }]}))
            }
            (Method::GET, "/api/session/ses_child/message?limit=200") => {
                json_response(json!({ "data": [] }))
            }
            (Method::POST, "/api/session/ses_root/interrupt") => {
                json_response(json!({ "interrupted": true }))
            }
            (Method::DELETE, "/api/session/ses_root") => Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Body::empty())
                .expect("deleted response"),
            (Method::GET, "/api/event") => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(
                    "data: {\"id\":\"evt_1\",\"type\":\"server.connected\",\"data\":{}}\n\n\
                     data: {\"id\":\"evt_2\",\"type\":\"session.created\",\"created\":5,\
                     \"data\":{\"sessionID\":\"ses_child\",\"parentID\":\"ses_root\",\
                     \"title\":\"Inspect repository\"}}\n\n",
                ))
                .expect("SSE response"),
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .expect("not-found response"),
        }
    }

    /// Against an OpenCode 2 helper the client speaks `/api/*` and hands the observer the
    /// OpenCode 1 shapes its tracker consumes.
    #[tokio::test]
    async fn system_client_drives_an_opencode_2_helper_through_its_api() {
        let recorded = RecordedRequests::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("OpenCode 2 fixture listener");
        let address = listener.local_addr().expect("OpenCode 2 fixture address");
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .fallback(any(opencode_2_fixture_handler))
                    .with_state(recorded.clone()),
            )
            .into_future(),
        );
        let directory = tempfile::tempdir().expect("OpenCode 2 fixture directory");
        let mut remote = SystemOpenCodeRemoteClientFactory
            .connect(
                &format!("http://127.0.0.1:{}", address.port()),
                OPENCODE_USERNAME,
                "fixture-secret",
                directory.path(),
            )
            .await
            .expect("OpenCode 2 client");

        assert_eq!(
            remote.create_root("mock/mock-model").await.expect("root"),
            "ses_root"
        );
        assert_eq!(
            remote.root("ses_root").await.expect("root")["id"],
            "ses_root"
        );
        assert_eq!(
            remote.statuses().await.expect("statuses"),
            json!({ "ses_child": { "type": "busy" } })
        );
        let children = remote.children("ses_root").await.expect("children");
        assert_eq!(children[0]["id"], "ses_child");
        assert_eq!(children[0]["parentID"], "ses_root");
        assert_eq!(
            remote.messages("ses_child").await.expect("history"),
            json!([])
        );
        remote.abort("ses_root").await.expect("interrupt");
        remote
            .cleanup_pre_spawn("ses_root")
            .await
            .expect("delete root");
        let mut stream = remote.open_event_stream().await.expect("event stream");
        let connected: Value = serde_json::from_slice(
            &within_fixture_deadline("server.connected", stream.next_data())
                .await
                .expect("server.connected"),
        )
        .expect("JSON");
        assert_eq!(connected["type"], "server.connected");
        let created: Value = serde_json::from_slice(
            &within_fixture_deadline("session.created", stream.next_data())
                .await
                .expect("session.created"),
        )
        .expect("JSON");
        assert_eq!(created["type"], "session.created");
        assert_eq!(created["properties"]["info"]["parentID"], "ses_root");

        {
            let requests = recorded
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let create = requests
                .iter()
                .find(|(method, uri, _, _)| *method == Method::POST && uri == "/api/session")
                .expect("create-root request");
            let create: Value = serde_json::from_slice(&create.3).expect("create-root JSON");
            assert_eq!(
                create["model"],
                json!({ "providerID": "mock", "id": "mock-model" })
            );
            assert_eq!(
                create["location"]["directory"],
                directory.path().to_string_lossy().as_ref()
            );
            assert!(requests.iter().all(|(_, _, headers, _)| {
                headers
                    .get(reqwest::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some("Basic b3BlbmNvZGU6Zml4dHVyZS1zZWNyZXQ=")
            }));
        }
        server.abort();
        let _ = within_fixture_deadline("the aborted fixture server to stop", server).await;
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn system_helper_launcher_reads_the_opencode_2_readiness_line() {
        let sandbox = TestSandbox::new("opencode-2-helper-readiness");
        let script = sandbox.executable_script(
            "helper",
            "printf 'server listening on http://127.0.0.1:43128\\n'\nexec sleep 3600",
            "",
        );
        let launcher = SystemOpenCodeHelperLauncher::default();
        let ready = within_fixture_deadline(
            "the OpenCode 2 helper to report readiness",
            launcher.start(sandbox_script_launch(
                &sandbox,
                &script,
                ProcessAttributionRegistry::new(),
            )),
        )
        .await
        .expect("OpenCode 2 readiness");
        within_fixture_deadline(
            "the OpenCode 2 helper to be reaped",
            ready.process.terminate_and_reap(),
        )
        .await;
        within_fixture_deadline("the OpenCode 2 launcher to shut down", launcher.shutdown()).await;
        assert_eq!(ready.endpoint, "http://127.0.0.1:43128");
    }

    #[test]
    fn sse_parser_accepts_lf_and_crlf_frames_and_rejects_oversized_events() {
        let mut decoder = OpenCodeSseDecoder::default();
        decoder
            .push(b"event: message\r\ndata: {\"type\":\"server.connected\"}\r\n\r\n")
            .expect("CRLF SSE chunk");
        assert_eq!(
            decoder
                .take_event()
                .expect("CRLF SSE frame")
                .expect("CRLF SSE event")["type"],
            "server.connected"
        );
        decoder
            .push(b"data: {\"type\":\"session.status\"}\n\n")
            .expect("LF SSE chunk");
        assert_eq!(
            decoder
                .take_event()
                .expect("LF SSE frame")
                .expect("LF SSE event")["type"],
            "session.status"
        );
        let mut buffer = vec![b'x'; OPENCODE_SSE_EVENT_LIMIT + 1];
        buffer.extend_from_slice(b"\n\n");
        decoder.push(&buffer).expect("oversized SSE chunk");
        assert!(decoder.take_event().is_err());
    }

    #[test]
    fn health_version_parser_accepts_only_three_numeric_semver_components() {
        assert_eq!(
            parse_opencode_version("opencode 1.18.4"),
            Some("1.18.4".to_owned())
        );
        for invalid in [
            "1..18.4",
            "1.18.",
            "1.18.4.1",
            "v1.18.4",
            "1.18.4-beta",
            "01.18.4",
        ] {
            assert_eq!(
                parse_opencode_version(invalid),
                None,
                "invalid health version {invalid:?} must fail closed"
            );
        }
    }

    #[test]
    fn loopback_endpoint_rejects_any_url_decoration() {
        assert!(valid_loopback_endpoint("http://127.0.0.1:43127"));
        assert!(valid_loopback_endpoint("http://127.0.0.1:43127/"));
        for invalid in [
            "http://127.0.0.1:43127/?directory=/tmp",
            "http://127.0.0.1:43127/#listener",
        ] {
            assert!(
                !valid_loopback_endpoint(invalid),
                "decorated helper endpoint {invalid:?} must fail closed"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ownership_freeze_rejects_and_reaps_an_opencode_helper_before_admission() {
        let sandbox = TestSandbox::new("opencode-helper-owned-freeze");
        let pid_path = sandbox.path("helper.pid");
        let executable = sandbox.executable_script(
            "opencode-helper",
            "printf 'opencode server listening on http://127.0.0.1:43127\\n'\nexec sleep 3600",
            "",
        );
        let admission_race = Arc::new(OpenCodeHelperAdmissionRace {
            spawned: FixtureEvent::default(),
            release: FixtureEvent::default(),
        });
        let events = OpenCodeHelperFixtureEvents {
            spawned: Arc::new(FixtureEvent::default()),
            release: Arc::new(FixtureEvent::default()),
            admission_race: Some(admission_race.clone()),
            reaped: Arc::new(FixtureEvent::default()),
            pid_path: pid_path.clone(),
            reap_timeout: None,
            stdout_join: None,
            wait_error: None,
        };
        let registry = ProcessAttributionRegistry::new();
        let launcher = SystemOpenCodeHelperLauncher::with_fixture_events(
            OPENCODE_HELPER_READY_TIMEOUT,
            events,
        );
        let launch = sandbox_script_launch(&sandbox, &executable, registry.clone());
        let mut start = tokio::spawn(async move { launcher.start(launch).await });
        wait_during_start(
            &mut start,
            &admission_race.spawned,
            0,
            "the admission-race helper PID publication",
        )
        .await;
        let pid = std::fs::read_to_string(&pid_path)
            .expect("OpenCode helper PID")
            .parse::<u32>()
            .expect("numeric OpenCode helper PID");
        let identity = NativeProcessSampler::process_identity(pid).expect("helper identity");

        assert!(registry.freeze_and_snapshot_identities().is_empty());
        admission_race.release.publish();
        let error = join_start(start)
            .await
            .expect_err("frozen runtime rejects inflight helper admission");
        assert!(
            error.contains("closed for shutdown"),
            "unexpected error: {error}"
        );
        assert!(
            !matches!(
                NativeProcessSampler::process_identity(pid),
                Ok(current) if current == identity
            ),
            "rejected OpenCode helper survived exact cleanup"
        );
        assert!(matches!(
            waitid_child_once(pid),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD)
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[should_panic(
        expected = "the OpenCode helper start task ended before the helper PID publication: Ok(Err(\"failed to start OpenCode helper"
    )]
    async fn fixture_wait_reports_a_helper_start_failure_instead_of_hanging() {
        let sandbox = TestSandbox::new("opencode-helper-start-failure");
        let admission_race = Arc::new(OpenCodeHelperAdmissionRace {
            spawned: FixtureEvent::default(),
            release: FixtureEvent::default(),
        });
        let launcher = SystemOpenCodeHelperLauncher::with_fixture_events(
            OPENCODE_HELPER_READY_TIMEOUT,
            OpenCodeHelperFixtureEvents {
                spawned: Arc::new(FixtureEvent::default()),
                release: Arc::new(FixtureEvent::default()),
                admission_race: Some(admission_race.clone()),
                reaped: Arc::new(FixtureEvent::default()),
                pid_path: sandbox.path("helper.pid"),
                reap_timeout: None,
                stdout_join: None,
                wait_error: None,
            },
        );
        let launch = sandbox_script_launch(
            &sandbox,
            &sandbox.path("missing-opencode-helper"),
            ProcessAttributionRegistry::new(),
        );
        let mut start = tokio::spawn(async move { launcher.start(launch).await });
        wait_during_start(
            &mut start,
            &admission_race.spawned,
            0,
            "the helper PID publication",
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_invalid_helper_readiness_children_reap_in_parallel() {
        let pair_started = Arc::new(tokio::sync::Barrier::new(2));
        let left = invalid_readiness_fixture(
            TestSandbox::new("opencode-invalid-left"),
            pair_started.clone(),
        );
        let right =
            invalid_readiness_fixture(TestSandbox::new("opencode-invalid-right"), pair_started);

        let ((left_error, left_pid), (right_error, right_pid)) =
            tokio::join!(left.start_and_release(), right.start_and_release());

        assert!(
            left_error.contains("invalid endpoint") || left_error.contains("readiness timed out")
        );
        assert!(
            right_error.contains("invalid endpoint") || right_error.contains("readiness timed out")
        );
        assert_ne!(left_pid, right_pid);
        tokio::join!(left.wait_reaped(), right.wait_reaped());
    }
}
