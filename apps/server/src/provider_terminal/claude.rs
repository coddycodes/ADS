#![cfg_attr(not(unix), allow(dead_code))]
// Windows compiles the shared factory facade, while this authenticated hook
// observer intentionally remains Unix-only and returns pass-through there.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::{Future, IntoFuture},
    io,
    net::TcpListener as StdTcpListener,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[cfg(unix)]
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddrV4},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::IntoResponse,
    routing::post,
};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[cfg(all(test, unix))]
use super::TerminalObserverGeneration;
use super::model::TerminalObserverGenerationLease;
#[cfg(unix)]
use super::supervisor::create_owned_generation_directory;
use super::{
    PreparedTerminalLaunch, PreparedTerminalObserver, ProviderTerminalObserverFactory,
    ProviderTerminalObserverFactoryInput, TerminalAgentActivityAdmission,
    TerminalAgentActivityControl, TerminalAgentActivityObservation,
    TerminalAgentActivityObservationKind, TerminalAgentActivityTransition,
    TerminalGenerationActivityPublisher, TerminalObserverWorkerContext,
    supervisor::cleanup_owned_generation_directory,
};
#[cfg(test)]
use crate::process::supervised::{
    run_supervised_with_spawn_observer, run_supervised_with_spawn_observer_until,
};
use crate::{
    activity::{
        ActivityActorSummary, ActivityCapabilities, ActivityHistoryRecovery, ActivityLifecycle,
        ActivityObservationState, ActivityWorkItemSummary, ProviderActivityMutation,
    },
    process::supervised::{SupervisedOverflow, SupervisedRunRequest, run_supervised},
    provider::claude::{
        activity::{ClaudeActivityInputSource, ClaudeActivityTracker},
        transcript::{ClaudeTranscriptRecoveryRequest, recover_transcript},
    },
};

const CLAUDE_PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
const CLAUDE_CAPABILITY_CACHE_CAPACITY: usize = 64;
const CLAUDE_CAPABILITY_PROBE_CONCURRENCY: usize = 8;
const CLAUDE_PREPARATION_BUDGET: Duration = Duration::from_millis(3_250);
const CLAUDE_HOOK_BODY_LIMIT: usize = 1024 * 1024;
const CLAUDE_HOOK_QUEUE_CAPACITY: usize = 128;
const CLAUDE_HOOK_REQUEST_CAPACITY: usize = 32;
const CLAUDE_HOOK_BODY_TIMEOUT: Duration = Duration::from_secs(2);
const CLAUDE_HOOK_RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
const CLAUDE_HOOK_CORRELATION_HEADER: &str = "X-Coddy Studio-Launch-Correlation";
const CLAUDE_HOOK_PATH_LIMIT: usize = 4 * 1024;
const CLAUDE_HOOK_TEXT_LIMIT: usize = 16 * 1024;
const CLAUDE_HOOK_OBJECT_LIMIT: usize = 64 * 1024;
// Anthropic introduced HTTP hooks in Claude Code v2.1.63. Versions before
// that explicitly reported that HTTP hooks were unsupported.
const MIN_PROVEN_HTTP_HOOK_VERSION: (u64, u64, u64) = (2, 1, 63);
// Approval evidence for this exact macOS arm64 build:
// - offline SHA-256:
//   8addc857f3fe64d5a0368af9ee50321b50afb4a6918ba3ef018ab84f5dbbe081
// - embedded source GIT_SHA: 4073f59596e272f39393db4f96abc5f4b10eff21
// - embedded merge markers:
//   "3. **Merge**: Add to existing hooks, don't replace"
//   "Failed to merge hooks from "
// Production verifies the full on-disk SHA-256 and corroborates it with the
// embedded codesign identity/CDHash; unknown platforms and builds fail closed.
const APPROVED_ADDITIVE_HOOK_VERSION: &str = "2.1.220";
const APPROVED_ADDITIVE_HOOK_BINARY_LENGTH: u64 = 256_908_272;
#[cfg(target_os = "macos")]
const APPROVED_ADDITIVE_HOOK_SHA256: &str =
    "8addc857f3fe64d5a0368af9ee50321b50afb4a6918ba3ef018ab84f5dbbe081";
#[cfg(any(target_os = "macos", all(test, unix)))]
const APPROVED_ADDITIVE_HOOK_IDENTIFIER: &str = "com.anthropic.claude-code";
#[cfg(any(target_os = "macos", all(test, unix)))]
const APPROVED_ADDITIVE_HOOK_TEAM: &str = "Q6L2SF6YDW";
#[cfg(any(target_os = "macos", all(test, unix)))]
const APPROVED_ADDITIVE_HOOK_CDHASH: &str = "474c5a3154406ea5ee537b0a25f24f9292b1570b";
#[cfg(any(target_os = "macos", all(test, unix)))]
const APPROVED_ADDITIVE_HOOK_CDHASH_FULL: &str =
    "474c5a3154406ea5ee537b0a25f24f9292b1570bc9eb0a4e555659e95a376f34";

#[derive(Clone, Eq, PartialEq)]
pub struct ClaudeProbeOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl fmt::Debug for ClaudeProbeOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeProbeOutput")
            .field("success", &self.success)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

pub trait ClaudeCapabilityProbeRunner: Send + Sync {
    fn run(
        &self,
        executable: &Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_>>;
}

pub trait ClaudeAdditiveHookAttestor: Send + Sync {
    fn prove(
        &self,
        executable: &Path,
        version: &str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

pub trait ClaudeExecutablePinner: Send + Sync {
    fn pin(&self, source: &Path, destination: &Path) -> io::Result<()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeCapabilities {
    pub version: String,
    pub additional_settings: bool,
    pub authenticated_http_hooks: bool,
    pub additive_hook_merge: bool,
}

pub struct CachedClaudeCapabilityProbe {
    runner: Arc<dyn ClaudeCapabilityProbeRunner>,
    attestor: Arc<dyn ClaudeAdditiveHookAttestor>,
    cache: tokio::sync::Mutex<ClaudeCapabilityCache>,
    probe_gates: Mutex<HashMap<ClaudeExecutableFingerprint, std::sync::Weak<ClaudeProbeGate>>>,
    probe_permits: Semaphore,
}

impl fmt::Debug for CachedClaudeCapabilityProbe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedClaudeCapabilityProbe")
            .finish_non_exhaustive()
    }
}

impl CachedClaudeCapabilityProbe {
    #[must_use]
    pub fn new(runner: Arc<dyn ClaudeCapabilityProbeRunner>) -> Self {
        Self::with_attestor(runner, Arc::new(ApprovedClaudeAdditiveHookAttestor))
    }

    #[must_use]
    pub fn with_attestor(
        runner: Arc<dyn ClaudeCapabilityProbeRunner>,
        attestor: Arc<dyn ClaudeAdditiveHookAttestor>,
    ) -> Self {
        Self {
            runner,
            attestor,
            cache: tokio::sync::Mutex::new(ClaudeCapabilityCache::default()),
            probe_gates: Mutex::new(HashMap::new()),
            probe_permits: Semaphore::new(CLAUDE_CAPABILITY_PROBE_CONCURRENCY),
        }
    }

    pub async fn probe(&self, executable: &Path) -> Option<ClaudeCapabilities> {
        let path = std::fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
        let fingerprint = ClaudeExecutableFingerprint::read(&path)?;
        let capabilities = self.probe_cacheable(&path, fingerprint.clone()).await?;
        let additive_hook_merge = if capabilities.authenticated_http_hooks {
            self.attest_stable_build(&path, &fingerprint, &capabilities.version)
                .await
        } else {
            false
        };
        Some(ClaudeCapabilities {
            version: capabilities.version,
            additional_settings: capabilities.additional_settings,
            authenticated_http_hooks: capabilities.authenticated_http_hooks,
            additive_hook_merge,
        })
    }

    async fn probe_pinned(&self, source: &Path, pinned: &Path) -> Option<ClaudeCapabilities> {
        let source = std::fs::canonicalize(source).ok()?;
        let source_fingerprint = ClaudeExecutableFingerprint::read(&source)?;
        let pinned_fingerprint = ClaudeExecutableFingerprint::read(pinned)?;
        let (capabilities, approved_pinned_build) = tokio::join!(
            self.probe_cacheable(&source, source_fingerprint.clone()),
            self.attest_stable_build(pinned, &pinned_fingerprint, APPROVED_ADDITIVE_HOOK_VERSION,),
        );
        let capabilities = capabilities?;
        if ClaudeExecutableFingerprint::read(&source).as_ref() != Some(&source_fingerprint) {
            return None;
        }
        let additive_hook_merge = capabilities.authenticated_http_hooks
            && capabilities.version == APPROVED_ADDITIVE_HOOK_VERSION
            && approved_pinned_build;
        Some(ClaudeCapabilities {
            version: capabilities.version,
            additional_settings: capabilities.additional_settings,
            authenticated_http_hooks: capabilities.authenticated_http_hooks,
            additive_hook_merge,
        })
    }

    async fn probe_cacheable(
        &self,
        path: &Path,
        fingerprint: ClaudeExecutableFingerprint,
    ) -> Option<ClaudeCacheableCapabilities> {
        if let Some(capabilities) = self.cache.lock().await.get(&fingerprint) {
            return Some(capabilities);
        }
        let probe_gate = self.probe_gate(&fingerprint);
        let _probe_guard = probe_gate.lock.lock().await;
        if let Some(capabilities) = self.cache.lock().await.get(&fingerprint) {
            return Some(capabilities);
        }
        if probe_gate.failed.load(Ordering::Acquire) {
            return None;
        }
        let _probe_permit = self.probe_permits.acquire().await.ok()?;
        let capabilities = self.probe_uncached(path).await;
        if ClaudeExecutableFingerprint::read(path).as_ref() != Some(&fingerprint) {
            probe_gate.failed.store(true, Ordering::Release);
            return None;
        }
        if let Some(capabilities) = &capabilities {
            let mut cache = self.cache.lock().await;
            if let Some(cached) = cache.get(&fingerprint) {
                return Some(cached);
            }
            cache.insert(fingerprint, capabilities.clone());
        } else {
            probe_gate.failed.store(true, Ordering::Release);
        }
        capabilities
    }

    async fn attest_stable_build(
        &self,
        executable: &Path,
        expected_fingerprint: &ClaudeExecutableFingerprint,
        version: &str,
    ) -> bool {
        let Ok(_attestation_permit) = self.probe_permits.acquire().await else {
            return false;
        };
        let Some(before) = ClaudeExecutableFingerprint::read(executable) else {
            return false;
        };
        if &before != expected_fingerprint {
            return false;
        }
        let approved = self.attestor.prove(executable, version).await;
        approved
            && ClaudeExecutableFingerprint::read(executable)
                .is_some_and(|after| after == before && &after == expected_fingerprint)
    }

    fn probe_gate(&self, fingerprint: &ClaudeExecutableFingerprint) -> Arc<ClaudeProbeGate> {
        let mut gates = self
            .probe_gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(fingerprint).and_then(std::sync::Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(ClaudeProbeGate::default());
        gates.insert(fingerprint.clone(), Arc::downgrade(&gate));
        gate
    }

    async fn probe_uncached(&self, executable: &Path) -> Option<ClaudeCacheableCapabilities> {
        let (version, help) = tokio::join!(
            self.runner.run(executable, vec!["--version".to_owned()]),
            self.runner.run(executable, vec!["--help".to_owned()]),
        );
        let version = version.ok()?;
        let help = help.ok()?;
        if !version.success || !help.success {
            return None;
        }
        let version = parse_claude_version(&bounded_probe_text(&version.stdout, &version.stderr))?;
        let version_tuple = parse_version_tuple(&version)?;
        let help = bounded_probe_text(&help.stdout, &help.stderr);
        let additional_settings =
            help.contains("--settings <file-or-json>") && help.contains("--setting-sources");
        let authenticated_http_hooks = version_tuple >= MIN_PROVEN_HTTP_HOOK_VERSION;
        Some(ClaudeCacheableCapabilities {
            version,
            additional_settings,
            authenticated_http_hooks,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClaudeCacheableCapabilities {
    version: String,
    additional_settings: bool,
    authenticated_http_hooks: bool,
}

#[derive(Debug, Default)]
struct ClaudeProbeGate {
    lock: tokio::sync::Mutex<()>,
    failed: AtomicBool,
}

#[derive(Debug, Default)]
struct ClaudeCapabilityCache {
    entries: HashMap<ClaudeExecutableFingerprint, ClaudeCacheableCapabilities>,
    recency: VecDeque<ClaudeExecutableFingerprint>,
}

impl ClaudeCapabilityCache {
    fn get(
        &mut self,
        fingerprint: &ClaudeExecutableFingerprint,
    ) -> Option<ClaudeCacheableCapabilities> {
        let value = self.entries.get(fingerprint)?.clone();
        self.recency.retain(|cached| cached != fingerprint);
        self.recency.push_back(fingerprint.clone());
        Some(value)
    }

    fn insert(
        &mut self,
        fingerprint: ClaudeExecutableFingerprint,
        capabilities: ClaudeCacheableCapabilities,
    ) {
        self.entries
            .retain(|cached, _| cached.path != fingerprint.path);
        self.recency
            .retain(|cached| cached.path != fingerprint.path);
        while self.entries.len() >= CLAUDE_CAPABILITY_CACHE_CAPACITY {
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
struct ClaudeExecutableFingerprint {
    path: PathBuf,
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change_time_seconds: i64,
    #[cfg(unix)]
    change_time_nanoseconds: i64,
}

impl ClaudeExecutableFingerprint {
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
            change_time_seconds: metadata.ctime(),
            #[cfg(unix)]
            change_time_nanoseconds: metadata.ctime_nsec(),
        })
    }
}

#[derive(Debug, Default)]
struct ClaudeListenerCounts {
    current: AtomicUsize,
    maximum: AtomicUsize,
}

impl ClaudeListenerCounts {
    fn acquire(self: &Arc<Self>) -> Option<ClaudeListenerLease> {
        let current = self
            .current
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .ok()?
            + 1;
        self.maximum.fetch_max(current, Ordering::AcqRel);
        Some(ClaudeListenerLease {
            counts: self.clone(),
        })
    }
}

#[derive(Debug)]
struct ClaudeListenerLease {
    counts: Arc<ClaudeListenerCounts>,
}

impl Drop for ClaudeListenerLease {
    fn drop(&mut self) {
        let previous = self.counts.current.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "Claude listener count underflow");
    }
}

pub struct ClaudeTerminalObserverFactory {
    probe: Arc<CachedClaudeCapabilityProbe>,
    pinner: Arc<dyn ClaudeExecutablePinner>,
    listener_counts: Arc<ClaudeListenerCounts>,
}

impl fmt::Debug for ClaudeTerminalObserverFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeTerminalObserverFactory")
            .finish_non_exhaustive()
    }
}

impl ClaudeTerminalObserverFactory {
    #[must_use]
    pub fn new(probe: Arc<CachedClaudeCapabilityProbe>) -> Self {
        Self::with_pinner(probe, Arc::new(NativeClaudeExecutablePinner))
    }

    #[must_use]
    pub fn with_pinner(
        probe: Arc<CachedClaudeCapabilityProbe>,
        pinner: Arc<dyn ClaudeExecutablePinner>,
    ) -> Self {
        Self {
            probe,
            pinner,
            listener_counts: Arc::new(ClaudeListenerCounts::default()),
        }
    }

    /// Returns current and maximum bound listener counts for black-box integration diagnostics.
    #[doc(hidden)]
    #[must_use]
    pub fn listener_counts_for_integration_test(&self) -> (usize, usize) {
        (
            self.listener_counts.current.load(Ordering::Acquire),
            self.listener_counts.maximum.load(Ordering::Acquire),
        )
    }

    #[must_use]
    pub fn system() -> Self {
        Self::new(Arc::new(CachedClaudeCapabilityProbe::new(Arc::new(
            SystemClaudeCapabilityProbeRunner::default(),
        ))))
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
            tokio::time::timeout(CLAUDE_PREPARATION_BUDGET, self.prepare_unix(input))
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
        if unsafe_interactive_settings_args(&input.launch.args) {
            return None;
        }
        let correlation = input.launch.generation.id().simple().to_string();
        let generation_key = input.launch.generation.id().simple().to_string();
        let generation_dir = create_owned_generation_directory(
            &input.runtime_dir,
            &format!("h{}", &generation_key[..16]),
        )
        .ok()?;
        let pinned_executable = generation_dir.join("claude");
        let overlay_path = generation_dir.join("settings.json");
        let mut preparation_resources = ClaudePreparationResources {
            runtime_dir: input.runtime_dir.clone(),
            generation_dir: generation_dir.clone(),
            armed: true,
        };
        // The clone is generation-scoped: provider self-updates may replace
        // the configured path for future terminals, while this terminal keeps
        // executing the exact object attested below.
        if self
            .pinner
            .pin(Path::new(&input.launch.executable), &pinned_executable)
            .and_then(|()| restrict_pinned_executable(&pinned_executable))
            .is_err()
        {
            return None;
        }
        let capabilities = self
            .probe
            .probe_pinned(Path::new(&input.launch.executable), &pinned_executable)
            .await;
        if !capabilities.is_some_and(|capabilities| {
            capabilities.additional_settings
                && capabilities.authenticated_http_hooks
                && capabilities.additive_hook_merge
        }) {
            return None;
        }
        let listener = match StdTcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => listener,
            Err(_) => return None,
        };
        if listener.set_nonblocking(true).is_err() {
            return None;
        }
        let address = listener.local_addr().ok()?;
        if !address.ip().is_loopback() {
            return None;
        }
        let token = match random_secret() {
            Some(token) => token,
            None => return None,
        };
        let endpoint = format!("http://{address}/claude-hook");
        let overlay = claude_hook_settings(&endpoint, &token, &correlation);
        if write_private_overlay(&overlay_path, &overlay).is_err() {
            return None;
        }
        let listener_lease = self.listener_counts.acquire()?;
        preparation_resources.armed = false;
        let resources = Arc::new(ClaudeObserverResources {
            listener: Mutex::new(Some((listener, listener_lease))),
            overlay_path: overlay_path.clone(),
            generation_dir,
            runtime_dir: input.runtime_dir.clone(),
            cleaned: AtomicBool::new(false),
        });
        let observer = ClaudePreparedTerminalObserver {
            inner: Arc::new(ClaudeObserverInner {
                resources,
                publisher: input.activity_publisher,
                provider_instance_id: input.launch.activity.provider_instance_id,
                token: Arc::new(token.clone()),
                correlation: Arc::new(correlation),
                spawned: AtomicBool::new(false),
                activity: Arc::new(TerminalAgentActivityControl::enabled()),
                listener_lifecycle: AtomicU64::new(pack_claude_listener_lifecycle(
                    ClaudeListenerLifecycle {
                        ready: false,
                        epoch: 0,
                    },
                )),
            }),
        };
        let mut args = input.launch.args;
        args.push("--settings".to_owned());
        args.push(overlay_path.to_string_lossy().into_owned());
        Some(PreparedTerminalLaunch {
            executable: pinned_executable.to_string_lossy().into_owned(),
            args,
            private_env: BTreeMap::new(),
            observer: Box::new(observer),
        })
    }
}

impl ProviderTerminalObserverFactory for ClaudeTerminalObserverFactory {
    fn requires_private_executable_pin(&self) -> bool {
        true
    }

    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(self.prepare_inner(input))
    }
}

#[derive(Debug)]
struct ClaudePreparedTerminalObserver {
    inner: Arc<ClaudeObserverInner>,
}

impl PreparedTerminalObserver for ClaudePreparedTerminalObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) {
        if self.inner.spawned.swap(true, Ordering::AcqRel) {
            return;
        }
        let listener = self
            .inner
            .resources
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some((listener, listener_lease)) = listener else {
            publish_claude_listener_unavailable(&self.inner);
            return;
        };
        let inner = self.inner.clone();
        // A refused worker drops the listener and leaves the terminal unobserved. The running
        // process still executes its pinned copy with its settings overlay, so the
        // generation directory stays until the observer's drop when the generation ends.
        if workers
            .spawn(async move {
                run_claude_observer(inner, generation, listener, listener_lease).await;
            })
            .is_err()
        {
            publish_claude_listener_unavailable(&self.inner);
        }
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) -> Pin<Box<dyn Future<Output = TerminalAgentActivityTransition> + Send + '_>> {
        Box::pin(async move {
            let (state, mut transition) = self.inner.activity.transition_state(enabled);
            let lifecycle = self.inner.listener_lifecycle_snapshot();
            let kind = lifecycle.observation_kind(state.enabled);
            self.inner
                .activity
                .mark_observed(TerminalAgentActivityObservation {
                    state,
                    epoch: lifecycle.epoch,
                    kind,
                });
            transition.epochs.claude = lifecycle.epoch;
            if kind == TerminalAgentActivityObservationKind::Unavailable {
                transition.failed = transition.failed.saturating_add(1);
                transition.unavailable = transition.unavailable.saturating_add(1);
            }
            transition
        })
    }

    fn diagnostic_label(&self) -> &str {
        "claude-authenticated-http-hooks"
    }
}

struct ClaudeObserverResources {
    listener: Mutex<Option<(StdTcpListener, ClaudeListenerLease)>>,
    overlay_path: PathBuf,
    generation_dir: PathBuf,
    runtime_dir: PathBuf,
    cleaned: AtomicBool,
}

impl ClaudeObserverResources {
    fn cleanup(&self) {
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        self.listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        cleanup_owned_generation_directory(&self.runtime_dir, &self.generation_dir);
    }
}

#[derive(Debug)]
struct NativeClaudeExecutablePinner;

impl ClaudeExecutablePinner for NativeClaudeExecutablePinner {
    fn pin(&self, source: &Path, destination: &Path) -> io::Result<()> {
        clone_executable(source, destination)
    }
}

#[cfg(target_os = "macos")]
fn clone_executable(source: &Path, destination: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "destination contains NUL"))?;
    // SAFETY: both C strings are NUL-terminated and remain alive for the call.
    let result = unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_executable(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native copy-on-write executable pinning is unavailable",
    ))
}

#[cfg(unix)]
fn restrict_pinned_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))
}

struct ClaudePreparationResources {
    runtime_dir: PathBuf,
    generation_dir: PathBuf,
    armed: bool,
}

impl Drop for ClaudePreparationResources {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        cleanup_owned_generation_directory(&self.runtime_dir, &self.generation_dir);
    }
}

impl fmt::Debug for ClaudeObserverResources {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeObserverResources")
            .field("overlay_path", &self.overlay_path)
            .finish_non_exhaustive()
    }
}

struct ClaudeObserverInner {
    resources: Arc<ClaudeObserverResources>,
    publisher: TerminalGenerationActivityPublisher,
    provider_instance_id: String,
    token: Arc<String>,
    correlation: Arc<String>,
    spawned: AtomicBool,
    activity: Arc<TerminalAgentActivityControl>,
    listener_lifecycle: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClaudeListenerLifecycle {
    ready: bool,
    epoch: u64,
}

const CLAUDE_LISTENER_READY_BIT: u64 = 1;

fn pack_claude_listener_lifecycle(lifecycle: ClaudeListenerLifecycle) -> u64 {
    (lifecycle.epoch << 1) | u64::from(lifecycle.ready)
}

fn unpack_claude_listener_lifecycle(value: u64) -> ClaudeListenerLifecycle {
    ClaudeListenerLifecycle {
        ready: value & CLAUDE_LISTENER_READY_BIT != 0,
        epoch: value >> 1,
    }
}

impl ClaudeListenerLifecycle {
    fn observation_kind(self, activity_enabled: bool) -> TerminalAgentActivityObservationKind {
        if !self.ready {
            TerminalAgentActivityObservationKind::Unavailable
        } else if activity_enabled {
            TerminalAgentActivityObservationKind::Live
        } else {
            TerminalAgentActivityObservationKind::Dormant
        }
    }
}

impl ClaudeObserverInner {
    fn listener_lifecycle_snapshot(&self) -> ClaudeListenerLifecycle {
        unpack_claude_listener_lifecycle(self.listener_lifecycle.load(Ordering::Acquire))
    }

    fn mark_listener_ready(&self) -> ClaudeListenerLifecycle {
        let mut current = self.listener_lifecycle.load(Ordering::Acquire);
        loop {
            let previous = unpack_claude_listener_lifecycle(current);
            let lifecycle = ClaudeListenerLifecycle {
                ready: true,
                epoch: previous.epoch.wrapping_add(1) & (u64::MAX >> 1),
            };
            match self.listener_lifecycle.compare_exchange_weak(
                current,
                pack_claude_listener_lifecycle(lifecycle),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return lifecycle,
                Err(observed) => current = observed,
            }
        }
    }

    fn mark_listener_unavailable(&self) -> ClaudeListenerLifecycle {
        let previous = self
            .listener_lifecycle
            .fetch_and(!CLAUDE_LISTENER_READY_BIT, Ordering::AcqRel);
        let lifecycle = unpack_claude_listener_lifecycle(previous);
        ClaudeListenerLifecycle {
            ready: false,
            epoch: lifecycle.epoch,
        }
    }
}

impl fmt::Debug for ClaudeObserverInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeObserverInner")
            .field("resources", &self.resources)
            .field("provider_instance_id", &self.provider_instance_id)
            .finish_non_exhaustive()
    }
}

impl Drop for ClaudeObserverInner {
    fn drop(&mut self) {
        publish_claude_listener_unavailable(self);
        self.resources.cleanup();
    }
}

struct ClaudeHookRequest {
    value: Value,
    admission: TerminalAgentActivityAdmission,
    response: oneshot::Sender<StatusCode>,
}

#[derive(Clone)]
struct ClaudeHookState {
    token: Arc<String>,
    correlation: Arc<String>,
    activity: Arc<TerminalAgentActivityControl>,
    requests: mpsc::Sender<ClaudeHookRequest>,
    request_permits: Arc<Semaphore>,
}

async fn run_claude_observer(
    inner: Arc<ClaudeObserverInner>,
    generation: TerminalObserverGenerationLease,
    listener: StdTcpListener,
    _listener_lease: ClaudeListenerLease,
) {
    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
        // The process still runs its pinned copy: the observer's drop removes the generation.
        publish_claude_listener_unavailable(&inner);
        return;
    };
    let lifecycle = inner.mark_listener_ready();
    let state = inner.activity.snapshot();
    inner
        .activity
        .mark_observed(TerminalAgentActivityObservation {
            state,
            epoch: lifecycle.epoch,
            kind: lifecycle.observation_kind(state.enabled),
        });
    let (request_sender, request_receiver) = mpsc::channel(CLAUDE_HOOK_QUEUE_CAPACITY);
    let state = ClaudeHookState {
        token: inner.token.clone(),
        correlation: inner.correlation.clone(),
        activity: inner.activity.clone(),
        requests: request_sender,
        request_permits: Arc::new(Semaphore::new(CLAUDE_HOOK_REQUEST_CAPACITY)),
    };
    let router = Router::new()
        .route("/claude-hook", post(capture_claude_hook))
        .with_state(state);
    let server = Box::pin(async move {
        let _ = axum::serve(listener, router).into_future().await;
    });
    drive_claude_observer(inner, generation, request_receiver, server).await;
}

async fn drive_claude_observer(
    inner: Arc<ClaudeObserverInner>,
    generation: TerminalObserverGenerationLease,
    mut request_receiver: mpsc::Receiver<ClaudeHookRequest>,
    mut server: Pin<Box<dyn Future<Output = ()> + Send>>,
) {
    let mut root_session_id: Option<String> = None;
    let mut tracker: Option<ClaudeActivityTracker> = None;
    let mut event_sequence = 0_u64;
    let mut active_actors = HashMap::<String, ActivityActorSummary>::new();
    let mut active_work_items = HashMap::<String, ActivityWorkItemSummary>::new();
    loop {
        tokio::select! {
            biased;
            _ = generation.cancelled() => {
                break;
            },
            request = request_receiver.recv() => {
                let Some(request) = request else { break; };
                let ClaudeHookRequest {
                    value,
                    admission,
                    response,
                } = request;
                let status = process_claude_hook(
                    &inner,
                    &generation,
                    &mut root_session_id,
                    &mut tracker,
                    &mut event_sequence,
                    &mut active_actors,
                    &mut active_work_items,
                    value,
                    admission,
                ).await;
                let _ = response.send(status);
            }
            () = &mut server => {
                break;
            }
        }
    }
    publish_claude_listener_unavailable(&inner);
    interrupt_claude_activity(&inner, &active_actors, &active_work_items, event_sequence).await;
    // A hook server that ended on its own leaves the process running its pinned copy with its
    // settings overlay: only an ended generation removes them now, otherwise the observer's
    // drop does when the generation ends.
    if generation.cancellation_reason().is_some() {
        inner.resources.cleanup();
    }
}

fn publish_claude_listener_unavailable(inner: &ClaudeObserverInner) {
    let lifecycle = inner.mark_listener_unavailable();
    let state = inner.activity.snapshot();
    inner
        .activity
        .mark_observed(TerminalAgentActivityObservation {
            state,
            epoch: lifecycle.epoch,
            kind: lifecycle.observation_kind(state.enabled),
        });
}

#[allow(clippy::too_many_arguments)]
async fn process_claude_hook(
    inner: &ClaudeObserverInner,
    generation: &TerminalObserverGenerationLease,
    root_session_id: &mut Option<String>,
    tracker: &mut Option<ClaudeActivityTracker>,
    event_sequence: &mut u64,
    active_actors: &mut HashMap<String, ActivityActorSummary>,
    active_work_items: &mut HashMap<String, ActivityWorkItemSummary>,
    value: Value,
    admission: TerminalAgentActivityAdmission,
) -> StatusCode {
    if !inner.activity.admission_is_current(&admission) {
        return StatusCode::NO_CONTENT;
    }
    let Some(session_id) = validate_claude_hook(&value) else {
        return StatusCode::BAD_REQUEST;
    };
    if let Some(root) = root_session_id.as_deref() {
        if root != session_id {
            return StatusCode::CONFLICT;
        }
    } else {
        let capabilities = ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: false,
            history_recovery: ActivityHistoryRecovery::None,
            terminal_observation: true,
            targeted_actor_cancellation: false,
        };
        if !inner.activity.admission_is_current(&admission) {
            return StatusCode::NO_CONTENT;
        }
        let published = inner
            .publisher
            .publish_correlated("claude", Some(&inner.provider_instance_id), capabilities)
            .await
            .unwrap_or(false);
        if !inner.activity.admission_is_current(&admission) {
            return StatusCode::NO_CONTENT;
        }
        if !published {
            return StatusCode::GONE;
        }
        if !inner.activity.admission_is_current(&admission) {
            return StatusCode::NO_CONTENT;
        }
        *root_session_id = Some(session_id.to_owned());
        *tracker = Some(ClaudeActivityTracker::new(session_id));
    }
    let recovery_request = value
        .get("agent_id")
        .and_then(Value::as_str)
        .and_then(|agent_id| {
            let tracker = tracker
                .as_ref()
                .expect("tracker is installed with root correlation");
            ClaudeTranscriptRecoveryRequest::from_authenticated_hook(
                &value,
                tracker.is_correlated_actor(session_id, agent_id),
            )
        });
    let mut staged_tracker = tracker
        .as_ref()
        .expect("tracker is installed with root correlation")
        .clone();
    let output = staged_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &value,
        current_unix_millis(),
    );
    if !output.mutations.is_empty() {
        let native_event_key = format!("claude:terminal-hook:{event_sequence}");
        if !inner.activity.admission_is_current(&admission) {
            return StatusCode::NO_CONTENT;
        }
        let applied = inner
            .publisher
            .apply(
                &native_event_key,
                output.mutations.clone(),
                &current_timestamp(),
            )
            .await;
        if !inner.activity.admission_is_current(&admission) {
            return StatusCode::NO_CONTENT;
        }
        if !matches!(applied, Ok(ref deltas) if !deltas.is_empty()) {
            return StatusCode::GONE;
        }
        if !inner.activity.admission_is_current(&admission) {
            return StatusCode::NO_CONTENT;
        }
        retain_active_claude_activity(&output.mutations, active_actors, active_work_items);
        *event_sequence = event_sequence.saturating_add(1);
    }
    if !inner.activity.admission_is_current(&admission) {
        return StatusCode::NO_CONTENT;
    }
    *tracker = Some(staged_tracker);
    if let Some(request) = recovery_request {
        let cancellation = CancellationToken::new();
        let recovered = match await_claude_recovery_while_current(
            &inner.activity,
            generation,
            cancellation.clone(),
            recover_transcript(request, cancellation),
        )
        .await
        {
            Ok(recovered) => recovered,
            Err(status) => return status,
        };
        if let Some(recovered) = recovered {
            if !inner.activity.admission_is_current(&admission) {
                return StatusCode::NO_CONTENT;
            }
            let mut staged_tracker = tracker
                .as_ref()
                .expect("tracker is installed with root correlation")
                .clone();
            let output = staged_tracker.handle_recovered_records(
                &recovered.agent_id,
                &recovered.agent_type,
                &recovered.records,
            );
            let mut mutations = output.mutations;
            mutations.push(ProviderActivityMutation::SetScope {
                capabilities: ActivityCapabilities {
                    actors: true,
                    attributed_activity: true,
                    background_work: false,
                    history_recovery: ActivityHistoryRecovery::Bounded,
                    terminal_observation: true,
                    targeted_actor_cancellation: false,
                },
                observation_state: ActivityObservationState::Live,
            });
            if !inner.activity.admission_is_current(&admission) {
                return StatusCode::NO_CONTENT;
            }
            let applied = inner
                .publisher
                .apply(
                    &recovered.native_event_id,
                    mutations.clone(),
                    &current_timestamp(),
                )
                .await;
            if !inner.activity.admission_is_current(&admission) {
                return StatusCode::NO_CONTENT;
            }
            if !matches!(applied, Ok(ref deltas) if !deltas.is_empty()) {
                return StatusCode::GONE;
            }
            if !inner.activity.admission_is_current(&admission) {
                return StatusCode::NO_CONTENT;
            }
            retain_active_claude_activity(&mutations, active_actors, active_work_items);
            if !inner.activity.admission_is_current(&admission) {
                return StatusCode::NO_CONTENT;
            }
            *tracker = Some(staged_tracker);
        }
    }
    StatusCode::NO_CONTENT
}

async fn await_claude_recovery_while_current<T>(
    activity: &TerminalAgentActivityControl,
    generation: &TerminalObserverGenerationLease,
    cancellation: CancellationToken,
    recovery: impl Future<Output = T>,
) -> Result<T, StatusCode> {
    let mut activity_changes = activity.subscribe();
    tokio::select! {
        recovered = recovery => Ok(recovered),
        _ = activity_changes.changed() => {
            cancellation.cancel();
            Err(StatusCode::NO_CONTENT)
        }
        _ = generation.cancelled() => {
            cancellation.cancel();
            Err(StatusCode::GONE)
        }
    }
}

fn retain_active_claude_activity(
    mutations: &[ProviderActivityMutation],
    active_actors: &mut HashMap<String, ActivityActorSummary>,
    active_work_items: &mut HashMap<String, ActivityWorkItemSummary>,
) {
    for mutation in mutations {
        match mutation {
            ProviderActivityMutation::UpsertActor(actor) => {
                if actor.status.is_terminal() {
                    active_actors.remove(&actor.id);
                } else {
                    active_actors.insert(actor.id.clone(), actor.clone());
                }
            }
            ProviderActivityMutation::RemoveActor { actor_id } => {
                active_actors.remove(actor_id);
            }
            ProviderActivityMutation::UpsertWorkItem(work_item) => {
                if work_item.status.is_terminal() {
                    active_work_items.remove(&work_item.id);
                } else {
                    active_work_items.insert(work_item.id.clone(), work_item.clone());
                }
            }
            ProviderActivityMutation::RemoveWorkItem { work_item_id } => {
                active_work_items.remove(work_item_id);
            }
            ProviderActivityMutation::SetScope { .. }
            | ProviderActivityMutation::SetSectionHealth { .. }
            | ProviderActivityMutation::AppendEntry(_) => {}
        }
    }
}

async fn interrupt_claude_activity(
    inner: &ClaudeObserverInner,
    active_actors: &HashMap<String, ActivityActorSummary>,
    active_work_items: &HashMap<String, ActivityWorkItemSummary>,
    event_sequence: u64,
) {
    let Some(admission) = inner.activity.admit() else {
        return;
    };
    let mut mutations = active_actors
        .keys()
        .filter_map(|actor_id| {
            ProviderActivityMutation::set_actor_status(actor_id.clone(), "interrupted").ok()
        })
        .collect::<Vec<_>>();
    let now = current_timestamp();
    mutations.extend(active_work_items.values().cloned().map(|mut work_item| {
        work_item.status = ActivityLifecycle::Interrupted;
        work_item.updated_at = now.clone();
        work_item.terminal_at = Some(now.clone());
        ProviderActivityMutation::UpsertWorkItem(work_item)
    }));
    if mutations.is_empty() {
        return;
    }
    if !inner.activity.admission_is_current(&admission) {
        return;
    }
    let _ = inner
        .publisher
        .apply(
            &format!("claude:terminal-interrupted:{event_sequence}"),
            mutations,
            &now,
        )
        .await;
}

async fn capture_claude_hook(
    State(state): State<ClaudeHookState>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    let Ok(_permit) = state.request_permits.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    if !authorized(&headers, &state.token, &state.correlation) {
        return StatusCode::FORBIDDEN;
    }
    let Some(admission) = state.activity.admit() else {
        return StatusCode::NO_CONTENT;
    };
    if !headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE;
    }
    let Ok(Ok(bytes)) = tokio::time::timeout(
        CLAUDE_HOOK_BODY_TIMEOUT,
        to_bytes(body, CLAUDE_HOOK_BODY_LIMIT),
    )
    .await
    else {
        return StatusCode::PAYLOAD_TOO_LARGE;
    };
    if !state.activity.admission_is_current(&admission) {
        return StatusCode::NO_CONTENT;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return StatusCode::BAD_REQUEST;
    };
    if !value.is_object() {
        return StatusCode::BAD_REQUEST;
    }
    if !state.activity.admission_is_current(&admission) {
        return StatusCode::NO_CONTENT;
    }
    let (response, receiver) = oneshot::channel();
    if state
        .requests
        .try_send(ClaudeHookRequest {
            value,
            admission,
            response,
        })
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    tokio::time::timeout(CLAUDE_HOOK_RESPONSE_TIMEOUT, receiver)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn authorized(headers: &HeaderMap, expected_token: &str, expected_correlation: &str) -> bool {
    let Some(token) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    let Some(correlation) = headers
        .get(CLAUDE_HOOK_CORRELATION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    constant_time_equal(token, expected_token)
        && constant_time_equal(correlation, expected_correlation)
}

fn constant_time_equal(provided: &str, expected: &str) -> bool {
    provided.len() == expected.len() && bool::from(provided.as_bytes().ct_eq(expected.as_bytes()))
}

fn claude_hook_settings(endpoint: &str, token: &str, correlation: &str) -> Value {
    let handler = json!({
        "type": "http",
        "url": endpoint,
        "timeout": 1,
        "headers": {
            "Authorization": format!("Bearer {token}"),
            CLAUDE_HOOK_CORRELATION_HEADER: correlation,
        },
    });
    let hook = || json!([{ "hooks": [handler.clone()] }]);
    json!({
        "hooks": {
            "SubagentStart": hook(),
            "SubagentStop": hook(),
            "PreToolUse": hook(),
            "PostToolUse": hook(),
            "PostToolUseFailure": hook(),
        }
    })
}

fn unsafe_interactive_settings_args(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--settings" | "--setting-sources" | "--safe-mode" | "--bare"
        ) || arg.starts_with("--settings=")
            || arg.starts_with("--setting-sources=")
    })
}

fn safe_identity_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str).filter(|value| {
        !value.is_empty() && value.chars().count() <= 256 && !value.chars().any(char::is_control)
    })
}

fn validate_claude_hook(value: &Value) -> Option<&str> {
    let event = safe_identity_field(value, "hook_event_name")?;
    let session_id = safe_identity_field(value, "session_id")?;
    let transcript_path = safe_absolute_path_field(value, "transcript_path")?;
    if transcript_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("jsonl")
    {
        return None;
    }
    safe_absolute_path_field(value, "cwd")?;
    match event {
        "SubagentStart" => {
            safe_identity_field(value, "agent_id")?;
            safe_identity_field(value, "agent_type")?;
        }
        "SubagentStop" => {
            safe_identity_field(value, "agent_id")?;
            safe_identity_field(value, "agent_type")?;
            value.get("stop_hook_active")?.as_bool()?;
            let agent_transcript_path = safe_absolute_path_field(value, "agent_transcript_path")?;
            if agent_transcript_path
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("jsonl")
            {
                return None;
            }
            bounded_text_field(value, "last_assistant_message")?;
        }
        "PreToolUse" => {
            validate_tool_hook(value)?;
            bounded_object_field(value, "tool_input")?;
        }
        "PostToolUse" => {
            validate_tool_hook(value)?;
            bounded_object_field(value, "tool_input")?;
            bounded_json_field(value, "tool_response")?;
            validate_optional_duration(value)?;
        }
        "PostToolUseFailure" => {
            validate_tool_hook(value)?;
            bounded_object_field(value, "tool_input")?;
            bounded_text_field(value, "error")?;
            if value
                .get("is_interrupt")
                .is_some_and(|value| !value.is_boolean())
            {
                return None;
            }
            validate_optional_duration(value)?;
        }
        _ => return None,
    }
    Some(session_id)
}

fn validate_tool_hook(value: &Value) -> Option<()> {
    safe_identity_field(value, "tool_name")?;
    safe_identity_field(value, "tool_use_id")?;
    validate_optional_identity(value, "agent_id")?;
    validate_optional_identity(value, "agent_type")?;
    Some(())
}

fn validate_optional_identity(value: &Value, field: &str) -> Option<()> {
    if value.get(field).is_some() {
        safe_identity_field(value, field)?;
    }
    Some(())
}

fn validate_optional_duration(value: &Value) -> Option<()> {
    if value.get("duration_ms").is_some_and(|value| {
        !value
            .as_u64()
            .is_some_and(|duration| duration <= 86_400_000)
    }) {
        return None;
    }
    Some(())
}

fn safe_absolute_path_field(value: &Value, field: &str) -> Option<PathBuf> {
    let path = value.get(field)?.as_str()?;
    if path.is_empty() || path.len() > CLAUDE_HOOK_PATH_LIMIT || path.chars().any(char::is_control)
    {
        return None;
    }
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

fn bounded_text_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)?
        .as_str()
        .filter(|text| text.len() <= CLAUDE_HOOK_TEXT_LIMIT)
}

fn bounded_object_field<'a>(
    value: &'a Value,
    field: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    let object = value.get(field)?.as_object()?;
    (serde_json::to_vec(object).ok()?.len() <= CLAUDE_HOOK_OBJECT_LIMIT).then_some(object)
}

fn bounded_json_field<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    let field = value.get(field)?;
    (serde_json::to_vec(field).ok()?.len() <= CLAUDE_HOOK_OBJECT_LIMIT).then_some(field)
}

fn random_secret() -> Option<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug)]
struct ApprovedClaudeAdditiveHookAttestor;

impl ClaudeAdditiveHookAttestor for ApprovedClaudeAdditiveHookAttestor {
    fn prove(
        &self,
        executable: &Path,
        version: &str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        let executable = executable.to_path_buf();
        let version = version.to_owned();
        Box::pin(async move { approved_additive_hook_build(&executable, &version).await })
    }
}

async fn approved_additive_hook_build(executable: &Path, version: &str) -> bool {
    if version != APPROVED_ADDITIVE_HOOK_VERSION {
        return false;
    }
    let Some(before) = ClaudeExecutableFingerprint::read(executable) else {
        return false;
    };
    if before.length != APPROVED_ADDITIVE_HOOK_BINARY_LENGTH
        || !std::fs::metadata(executable).is_ok_and(|metadata| metadata.is_file())
    {
        return false;
    }
    approved_macos_codesign_build(executable).await
        && ClaudeExecutableFingerprint::read(executable).is_some_and(|after| after == before)
}

#[cfg(target_os = "macos")]
async fn approved_macos_codesign_build(executable: &Path) -> bool {
    let executable_for_hash = executable.to_path_buf();
    let (digest, display) = tokio::join!(
        tokio::task::spawn_blocking(move || sha256_file(&executable_for_hash)),
        run_codesign(executable, &["-dvvv", "--verbose=4"]),
    );
    if !digest
        .ok()
        .and_then(Result::ok)
        .is_some_and(|digest| digest == APPROVED_ADDITIVE_HOOK_SHA256)
    {
        return false;
    }
    let Some(display) = display else {
        return false;
    };
    if !display.success {
        return false;
    }
    let evidence = bounded_probe_text(&display.stdout, &display.stderr);
    codesign_output_matches_approved_build(&evidence)
}

#[cfg(target_os = "macos")]
fn sha256_file(path: &Path) -> io::Result<String> {
    let file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    let length = u32::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "executable is too large"))?;
    use std::os::fd::AsRawFd as _;
    // SAFETY: the file remains open for the mapping lifetime; the mapping is
    // read-only, private, and bounded by the inspected file length.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length as usize,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    struct Mapping {
        address: *mut libc::c_void,
        length: usize,
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: this is the exact live mapping created above.
            let _ = unsafe { libc::munmap(self.address, self.length) };
        }
    }
    let mapping = Mapping {
        address: mapping,
        length: length as usize,
    };
    let mut digest = [0_u8; 32];
    // SAFETY: CommonCrypto accepts a readable buffer of `length` bytes and a
    // writable 32-byte digest output. Both remain live for the call.
    let result = unsafe { CC_SHA256(mapping.address.cast_const(), length, digest.as_mut_ptr()) };
    if result.is_null() {
        return Err(io::Error::other("CommonCrypto SHA-256 failed"));
    }
    Ok(crate::crypto::lowercase_hex(&digest))
}

#[cfg(target_os = "macos")]
#[link(name = "System")]
unsafe extern "C" {
    fn CC_SHA256(
        data: *const libc::c_void,
        length: u32,
        digest: *mut libc::c_uchar,
    ) -> *mut libc::c_uchar;
}

#[cfg(not(target_os = "macos"))]
async fn approved_macos_codesign_build(_executable: &Path) -> bool {
    false
}

#[cfg(target_os = "macos")]
async fn run_codesign(executable: &Path, args: &[&str]) -> Option<ClaudeProbeOutput> {
    let mut command = tokio::process::Command::new("/usr/bin/codesign");
    command
        .args(args)
        .arg(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_supervised(
        SupervisedRunRequest {
            command,
            stdin: None,
            timeout: Duration::from_millis(500),
            cleanup_timeout: Duration::from_millis(500),
            max_output_bytes: 16 * 1024,
            overflow: SupervisedOverflow::Error,
        },
        &CancellationToken::new(),
    )
    .await
    .ok()?;
    Some(ClaudeProbeOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
    })
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn codesign_output_matches_approved_build(output: &str) -> bool {
    [
        format!("Identifier={APPROVED_ADDITIVE_HOOK_IDENTIFIER}"),
        format!("TeamIdentifier={APPROVED_ADDITIVE_HOOK_TEAM}"),
        format!("CDHash={APPROVED_ADDITIVE_HOOK_CDHASH}"),
        format!("CandidateCDHashFull sha256={APPROVED_ADDITIVE_HOOK_CDHASH_FULL}"),
    ]
    .iter()
    .all(|expected| output.lines().any(|line| line == expected))
}

#[cfg(unix)]
fn write_private_overlay(path: &Path, value: &Value) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.flush()?;
    file.sync_all()
}

fn parse_claude_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|part| {
            let candidate = part
                .trim_matches(|character: char| !(character.is_ascii_digit() || character == '.'));
            parse_version_tuple(candidate).is_some()
        })
        .map(|part| {
            part.trim_matches(|character: char| !(character.is_ascii_digit() || character == '.'))
                .to_owned()
        })
}

fn parse_version_tuple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    (parts.next().is_none()).then_some((major, minor, patch))
}

fn bounded_probe_text(stdout: &str, stderr: &str) -> String {
    let mut output = String::with_capacity(
        stdout
            .len()
            .saturating_add(stderr.len())
            .min(CLAUDE_PROBE_OUTPUT_LIMIT),
    );
    for value in [stdout, stderr] {
        let remaining = CLAUDE_PROBE_OUTPUT_LIMIT.saturating_sub(output.len());
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
        if output.len() < CLAUDE_PROBE_OUTPUT_LIMIT {
            output.push('\n');
        }
    }
    output
}

fn current_unix_millis() -> u64 {
    OffsetDateTime::now_utc()
        .unix_timestamp_nanos()
        .checked_div(1_000_000)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or_default()
}

fn current_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

#[derive(Debug)]
struct SystemClaudeCapabilityProbeRunner {
    timeout: Duration,
    #[cfg(test)]
    integration_deadline: Option<tokio::time::Instant>,
    #[cfg(test)]
    fixture_spawned: Option<ClaudeProbeSpawnFixture>,
    #[cfg(test)]
    fixture_cancellation: Option<CancellationToken>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct ClaudeProbeSpawnFixture {
    spawned: Arc<crate::test_support::FixtureEvent>,
    pid_path: PathBuf,
}

#[cfg(test)]
impl ClaudeProbeSpawnFixture {
    fn publish(&self, process_id: Option<u32>) {
        let process_id = process_id.expect("Claude probe PID is available after spawn");
        let mut temporary_name = self.pid_path.as_os_str().to_owned();
        temporary_name.push(".tmp");
        let temporary_path = PathBuf::from(temporary_name);
        std::fs::write(&temporary_path, process_id.to_string())
            .expect("stage Claude probe PID after spawn");
        std::fs::rename(&temporary_path, &self.pid_path)
            .expect("publish Claude probe PID after spawn");
        self.spawned.publish();
    }
}

impl SystemClaudeCapabilityProbeRunner {
    const fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            #[cfg(test)]
            integration_deadline: None,
            #[cfg(test)]
            fixture_spawned: None,
            #[cfg(test)]
            fixture_cancellation: None,
        }
    }

    #[cfg(test)]
    const fn with_integration_deadline(deadline: tokio::time::Instant) -> Self {
        Self {
            timeout: Duration::from_secs(2),
            integration_deadline: Some(deadline),
            fixture_spawned: None,
            fixture_cancellation: None,
        }
    }

    #[cfg(test)]
    fn with_spawn_fixture(mut self, fixture_spawned: ClaudeProbeSpawnFixture) -> Self {
        self.fixture_spawned = Some(fixture_spawned);
        self
    }

    #[cfg(test)]
    fn with_fixture_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.fixture_cancellation = Some(cancellation);
        self
    }
}

impl Default for SystemClaudeCapabilityProbeRunner {
    fn default() -> Self {
        Self::with_timeout(Duration::from_secs(2))
    }
}

impl ClaudeCapabilityProbeRunner for SystemClaudeCapabilityProbeRunner {
    fn run(
        &self,
        executable: &Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_>> {
        let executable = executable.to_path_buf();
        Box::pin(async move {
            #[cfg(feature = "hermetic-test-guard")]
            let executable = crate::hermetic_guard::checked_provider_executable(
                &executable,
                std::env::current_dir().ok().as_deref(),
                std::env::var_os("PATH").as_deref(),
            )
            .ok_or_else(|| {
                "Spawn: probe executable was not found or was refused by the hermetic test guard"
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
                cleanup_timeout: Duration::from_secs(1),
                max_output_bytes: CLAUDE_PROBE_OUTPUT_LIMIT,
                overflow: SupervisedOverflow::Truncate,
            };
            #[cfg(test)]
            let cancellation = self.fixture_cancellation.clone().unwrap_or_default();
            #[cfg(not(test))]
            let cancellation = CancellationToken::new();
            #[cfg(test)]
            let output = match (self.integration_deadline, self.fixture_spawned.clone()) {
                (Some(deadline), Some(fixture_spawned)) => {
                    run_supervised_with_spawn_observer_until(
                        request,
                        &cancellation,
                        deadline,
                        move |process_id| fixture_spawned.publish(process_id),
                    )
                    .await
                }
                (Some(deadline), None) => {
                    run_supervised_with_spawn_observer_until(
                        request,
                        &cancellation,
                        deadline,
                        |_| {},
                    )
                    .await
                }
                (None, Some(fixture_spawned)) => {
                    run_supervised_with_spawn_observer(request, &cancellation, move |process_id| {
                        fixture_spawned.publish(process_id)
                    })
                    .await
                }
                (None, None) => run_supervised(request, &cancellation).await,
            };
            #[cfg(not(test))]
            let output = run_supervised(request, &cancellation).await;
            let output =
                output.map_err(|error| format!("Claude capability probe failed: {error:?}"))?;
            Ok(ClaudeProbeOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
            })
        })
    }
}

#[cfg(all(test, unix))]
mod tests {

    #[cfg(all(feature = "hermetic-test-guard", unix))]
    #[tokio::test]
    async fn hermetic_guard_refuses_direct_claude_probe_before_spawn() {
        let Some(executable) = crate::hermetic_guard::refusal_fixture(
            "provider_terminal::claude::tests::hermetic_guard_refuses_direct_claude_probe_before_spawn",
            "2.1.220",
        ) else {
            return;
        };
        assert!(
            SystemClaudeCapabilityProbeRunner::default()
                .run(&executable, vec![])
                .await
                .is_err()
        );
    }
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::{
        activity::{ActivityProjection, ActivityRepository, ActivityScopeRef},
        persistence::{Database, run_migrations},
        test_support::TestSandbox,
    };

    #[cfg(target_os = "linux")]
    #[test]
    fn system_capability_probe_ignores_appimage_environment() {
        crate::test_support::check_capability_probe_appimage_environment(
            "provider_terminal::claude::tests::system_capability_probe_ignores_appimage_environment",
            |executable, args| async move {
                let output = SystemClaudeCapabilityProbeRunner::default()
                    .run(&executable, args)
                    .await?;
                Ok((output.success, output.stdout))
            },
        );
    }

    #[test]
    fn hook_authorization_accepts_canonical_and_legacy_correlation_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer token".parse().unwrap());
        headers.insert(
            CLAUDE_HOOK_CORRELATION_HEADER,
            "correlation".parse().unwrap(),
        );
        assert!(authorized(&headers, "token", "correlation"));
    }

    #[derive(Debug)]
    struct BarrierClaudeProbeRunner {
        version_barrier: Arc<tokio::sync::Barrier>,
    }

    impl ClaudeCapabilityProbeRunner for BarrierClaudeProbeRunner {
        fn run(
            &self,
            _executable: &Path,
            args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_>> {
            Box::pin(async move {
                if args == ["--version"] {
                    self.version_barrier.wait().await;
                    return Ok(ClaudeProbeOutput {
                        success: true,
                        stdout: "2.1.220 (Claude Code)\n".to_owned(),
                        stderr: String::new(),
                    });
                }
                Ok(ClaudeProbeOutput {
                    success: true,
                    stdout: "--settings <file-or-json>\n--setting-sources\n".to_owned(),
                    stderr: String::new(),
                })
            })
        }
    }

    #[derive(Debug)]
    struct ApprovedClaudeAttestorFixture;

    impl ClaudeAdditiveHookAttestor for ApprovedClaudeAttestorFixture {
        fn prove(
            &self,
            _executable: &Path,
            version: &str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            let approved = version == APPROVED_ADDITIVE_HOOK_VERSION;
            Box::pin(async move { approved })
        }
    }

    #[derive(Debug, Default)]
    struct ContentAwareClaudeAttestor {
        calls: AtomicUsize,
    }

    impl ClaudeAdditiveHookAttestor for ContentAwareClaudeAttestor {
        fn prove(
            &self,
            executable: &Path,
            version: &str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let executable = executable.to_path_buf();
            let approved_version = version == APPROVED_ADDITIVE_HOOK_VERSION;
            Box::pin(async move {
                approved_version
                    && std::fs::read(executable).is_ok_and(|bytes| bytes == b"approved")
            })
        }
    }

    #[derive(Debug)]
    struct ReplacingClaudeAttestor {
        replacement: PathBuf,
    }

    impl ClaudeAdditiveHookAttestor for ReplacingClaudeAttestor {
        fn prove(
            &self,
            executable: &Path,
            version: &str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
            let executable = executable.to_path_buf();
            let replacement = self.replacement.clone();
            let approved_version = version == APPROVED_ADDITIVE_HOOK_VERSION;
            Box::pin(async move {
                std::fs::rename(replacement, executable).expect("replace during attestation");
                approved_version
            })
        }
    }

    #[derive(Debug, Default)]
    struct CountingClaudeProbeRunner {
        calls: AtomicUsize,
    }

    impl ClaudeCapabilityProbeRunner for CountingClaudeProbeRunner {
        fn run(
            &self,
            _executable: &Path,
            args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                if args == ["--version"] {
                    Ok(ClaudeProbeOutput {
                        success: true,
                        stdout: "2.1.220 (Claude Code)\n".to_owned(),
                        stderr: String::new(),
                    })
                } else {
                    Ok(ClaudeProbeOutput {
                        success: true,
                        stdout: "--settings <file-or-json>\n--setting-sources\n".to_owned(),
                        stderr: String::new(),
                    })
                }
            })
        }
    }

    #[derive(Debug, Default)]
    struct FailingClaudeProbeRunner {
        calls: AtomicUsize,
    }

    impl ClaudeCapabilityProbeRunner for FailingClaudeProbeRunner {
        fn run(
            &self,
            _executable: &Path,
            _args: Vec<String>,
        ) -> Pin<Box<dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                Err("injected probe failure".to_owned())
            })
        }
    }

    #[derive(Debug)]
    struct ClaudeBoundedProbeFixture {
        _sandbox: TestSandbox,
        executable: PathBuf,
        spawned: Arc<crate::test_support::FixtureEvent>,
        pid_path: PathBuf,
        entered_path: PathBuf,
        stdout_complete_path: PathBuf,
        stderr_complete_path: PathBuf,
        completed_path: PathBuf,
        input_bytes: usize,
    }

    fn bounded_probe_fixture(sandbox: TestSandbox) -> ClaudeBoundedProbeFixture {
        let input_bytes = CLAUDE_PROBE_OUTPUT_LIMIT + 8 * 1024;
        let pid_path = sandbox.path("probe.pid");
        let entered_path = sandbox.path("probe.entered");
        let stdout_complete_path = sandbox.path("probe.stdout-complete");
        let stderr_complete_path = sandbox.path("probe.stderr-complete");
        let completed_path = sandbox.path("probe.completed");
        let executable = sandbox.executable_script(
            "large-claude-probe",
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
        ClaudeBoundedProbeFixture {
            _sandbox: sandbox,
            executable,
            spawned: Arc::new(crate::test_support::FixtureEvent::default()),
            pid_path,
            entered_path,
            stdout_complete_path,
            stderr_complete_path,
            completed_path,
            input_bytes,
        }
    }

    impl ClaudeBoundedProbeFixture {
        async fn run_until(
            &self,
            deadline: tokio::time::Instant,
        ) -> Result<ClaudeProbeOutput, String> {
            SystemClaudeCapabilityProbeRunner::with_integration_deadline(deadline)
                .with_spawn_fixture(ClaudeProbeSpawnFixture {
                    spawned: self.spawned.clone(),
                    pid_path: self.pid_path.clone(),
                })
                .run(
                    &self.executable,
                    vec![
                        self.entered_path.to_string_lossy().into_owned(),
                        self.stdout_complete_path.to_string_lossy().into_owned(),
                        self.stderr_complete_path.to_string_lossy().into_owned(),
                        self.completed_path.to_string_lossy().into_owned(),
                    ],
                )
                .await
        }

        fn assert_completed_and_reaped(&self) -> i32 {
            assert!(
                self.spawned.checkpoint() > 0,
                "Claude probe owner did not publish child spawn"
            );
            assert_eq!(
                std::fs::read_to_string(&self.entered_path)
                    .expect("read Claude probe entry milestone"),
                "entered",
                "Claude probe fixture did not enter its body"
            );
            let expected_input_bytes = self.input_bytes.to_string();
            assert_eq!(
                std::fs::read_to_string(&self.stdout_complete_path)
                    .expect("read Claude probe stdout milestone"),
                expected_input_bytes,
                "Claude probe fixture did not publish its exact stdout input"
            );
            assert_eq!(
                std::fs::read_to_string(&self.stderr_complete_path)
                    .expect("read Claude probe stderr milestone"),
                expected_input_bytes,
                "Claude probe fixture did not publish its exact stderr input"
            );
            assert_eq!(
                std::fs::read_to_string(&self.completed_path)
                    .expect("read Claude probe completion milestone"),
                "completed",
                "Claude probe fixture body did not complete"
            );
            let pid = read_fixture_pid(&self.pid_path).expect("Claude probe fixture PID");
            assert!(
                !process_exists(pid),
                "Claude probe process survived owner completion"
            );
            assert_child_was_reaped(pid, "Claude probe");
            pid
        }
    }

    async fn await_probe_owner_with_observation_watchdog(
        mut owner: Pin<&mut (dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_)>,
        cancellation: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<ClaudeProbeOutput, String> {
        tokio::select! {
            biased;
            result = owner.as_mut() => result,
            () = tokio::time::sleep_until(deadline) => {
                cancellation.cancel();
                let owner_result = owner.await;
                match owner_result {
                    Ok(_) => Err("Claude probe observation deadline expired after owner success".to_owned()),
                    Err(error) => Err(format!(
                        "Claude probe observation deadline expired; owner failed: {error}"
                    )),
                }
            }
        }
    }

    fn read_fixture_pid(path: &Path) -> Result<i32, String> {
        std::fs::read_to_string(path)
            .map_err(|error| format!("failed reading fixture PID: {error}"))?
            .parse::<i32>()
            .map_err(|error| format!("fixture PID was not numeric: {error}"))
    }

    fn process_exists(pid: i32) -> bool {
        // SAFETY: signal zero checks existence and permission without
        // delivering a signal.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn assert_child_was_reaped(pid: i32, label: &str) {
        let mut status = 0;
        // SAFETY: `status` is valid writable storage and WNOHANG does not
        // block.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let error = io::Error::last_os_error();
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

    fn restore_modified_time(source: &Path, target: &Path) {
        let status = std::process::Command::new("touch")
            .arg("-r")
            .arg(source)
            .arg(target)
            .status()
            .expect("run touch");
        assert!(status.success(), "restore fixture modified time");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_claude_probes_bound_both_output_streams_in_parallel() {
        let left = bounded_probe_fixture(TestSandbox::new("claude-probe-left"));
        let right = bounded_probe_fixture(TestSandbox::new("claude-probe-right"));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);

        let (left_output, right_output) =
            tokio::join!(left.run_until(deadline), right.run_until(deadline));

        let mut probe_pids = Vec::new();
        for (fixture, output) in [(&left, left_output), (&right, right_output)] {
            let output = output.expect("large Claude probe");
            assert!(output.success);
            assert_eq!(output.stdout.len(), CLAUDE_PROBE_OUTPUT_LIMIT);
            assert_eq!(output.stderr.len(), CLAUDE_PROBE_OUTPUT_LIMIT);
            assert_eq!(fixture.input_bytes, 72 * 1024);
            probe_pids.push(fixture.assert_completed_and_reaped());
        }
        assert_ne!(probe_pids[0], probe_pids[1]);
    }

    #[tokio::test]
    async fn probe_fixture_reports_owner_failure_before_observation_watchdog() {
        let sandbox = TestSandbox::new("claude-probe-owner-error");
        let cancellation = CancellationToken::new();
        let runner = SystemClaudeCapabilityProbeRunner::with_integration_deadline(
            tokio::time::Instant::now() + Duration::from_secs(15),
        )
        .with_fixture_cancellation(cancellation.clone());
        let mut owner = runner.run(&sandbox.path("missing-claude-probe"), Vec::new());

        let error = await_probe_owner_with_observation_watchdog(
            owner.as_mut(),
            cancellation,
            tokio::time::Instant::now() + Duration::from_millis(250),
        )
        .await
        .expect_err("missing Claude probe must report its owned spawn error");

        assert!(
            error.contains("Spawn"),
            "unexpected Claude probe owner error: {error}"
        );
        assert!(
            !error.contains("observation deadline expired"),
            "the observation watchdog hid the immediate owner error: {error}"
        );
    }

    #[tokio::test]
    async fn probe_observation_watchdog_cancels_joins_and_reaps_the_owned_process() {
        let sandbox = TestSandbox::new("claude-probe-watchdog-cleanup");
        let pid_path = sandbox.path("probe.pid");
        let executable = sandbox.executable_script("hung-claude-probe", "exec sleep 3600", "");
        let spawned = Arc::new(crate::test_support::FixtureEvent::default());
        let cancellation = CancellationToken::new();
        let owner_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let runner = SystemClaudeCapabilityProbeRunner::with_integration_deadline(owner_deadline)
            .with_spawn_fixture(ClaudeProbeSpawnFixture {
                spawned: spawned.clone(),
                pid_path: pid_path.clone(),
            })
            .with_fixture_cancellation(cancellation.clone());
        let spawn_checkpoint = spawned.checkpoint();
        let mut owner = runner.run(&executable, Vec::new());

        tokio::select! {
            biased;
            result = owner.as_mut() => {
                panic!("Claude probe owner returned before spawn publication: {result:?}");
            }
            result = tokio::time::timeout_at(
                owner_deadline,
                spawned.wait_after(spawn_checkpoint),
            ) => {
                result.expect("Claude probe did not publish its PID before the owner deadline");
            }
        }

        let error = await_probe_owner_with_observation_watchdog(
            owner.as_mut(),
            cancellation,
            tokio::time::Instant::now() + Duration::from_millis(100),
        )
        .await
        .expect_err("Claude probe observation watchdog must expire");

        assert!(
            error.contains("observation deadline expired"),
            "watchdog error omitted its boundary: {error}"
        );
        assert!(spawned.checkpoint() > 0, "watchdog probe did not spawn");
        let pid = read_fixture_pid(&pid_path).expect("watchdog Claude probe PID");
        assert!(
            !process_exists(pid),
            "watchdog Claude probe survived cleanup"
        );
        assert_child_was_reaped(pid, "watchdog Claude probe");
    }

    #[tokio::test]
    async fn default_system_probe_timeout_terminates_and_reaps_the_owned_process() {
        let sandbox = TestSandbox::new("claude-probe-default-timeout");
        let pid_path = sandbox.path("probe.pid");
        let executable = sandbox.executable_script("hung-claude-probe", "exec sleep 3600", "");
        let spawned = Arc::new(crate::test_support::FixtureEvent::default());
        let runner = SystemClaudeCapabilityProbeRunner::default().with_spawn_fixture(
            ClaudeProbeSpawnFixture {
                spawned: spawned.clone(),
                pid_path: pid_path.clone(),
            },
        );
        assert_eq!(runner.timeout, Duration::from_secs(2));

        let error = runner
            .run(&executable, Vec::new())
            .await
            .expect_err("hung Claude probe must fail closed");

        assert!(spawned.checkpoint() > 0, "hung Claude probe did not spawn");
        assert!(
            error.contains("Timeout"),
            "hung Claude probe failed outside the default timeout boundary: {error}"
        );
        let pid = read_fixture_pid(&pid_path).expect("default-timeout Claude probe PID");
        assert!(
            !process_exists(pid),
            "timed-out Claude probe process was not terminated and reaped"
        );
        assert_child_was_reaped(pid, "default-timeout Claude probe");
    }

    #[tokio::test]
    async fn capability_cache_does_not_serialize_external_probes_for_distinct_binaries() {
        let root = tempfile::tempdir().expect("probe directory");
        let first = root.path().join("claude-first");
        let second = root.path().join("claude-second");
        std::fs::write(&first, b"first").expect("first executable");
        std::fs::write(&second, b"second").expect("second executable");
        let probe = Arc::new(CachedClaudeCapabilityProbe::with_attestor(
            Arc::new(BarrierClaudeProbeRunner {
                version_barrier: Arc::new(tokio::sync::Barrier::new(2)),
            }),
            Arc::new(ApprovedClaudeAttestorFixture),
        ));

        let completed = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(probe.probe(&first), probe.probe(&second))
        })
        .await;

        assert!(
            completed.is_ok(),
            "the capability-cache mutex must not be held across external probes"
        );
    }

    #[tokio::test]
    async fn capability_cache_singleflights_concurrent_probes_for_the_same_binary() {
        let root = tempfile::tempdir().expect("probe directory");
        let executable = root.path().join("claude");
        std::fs::write(&executable, b"same").expect("executable");
        let runner = Arc::new(CountingClaudeProbeRunner::default());
        let probe = Arc::new(CachedClaudeCapabilityProbe::with_attestor(
            runner.clone(),
            Arc::new(ApprovedClaudeAttestorFixture),
        ));

        let (first, second) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(probe.probe(&executable), probe.probe(&executable))
        })
        .await
        .expect("same-key probes must complete within the cache deadline");

        assert_eq!(first, second);
        assert!(first.is_some());
        assert_eq!(
            runner.calls.load(Ordering::Acquire),
            2,
            "one version/help pair must serve both same-key callers"
        );
    }

    #[tokio::test]
    async fn capability_cache_shares_concurrent_failures_but_allows_a_later_retry() {
        let root = tempfile::tempdir().expect("probe directory");
        let executable = root.path().join("claude");
        std::fs::write(&executable, b"same").expect("executable");
        let runner = Arc::new(FailingClaudeProbeRunner::default());
        let probe = Arc::new(CachedClaudeCapabilityProbe::with_attestor(
            runner.clone(),
            Arc::new(ApprovedClaudeAttestorFixture),
        ));

        let (first, second) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(probe.probe(&executable), probe.probe(&executable))
        })
        .await
        .expect("same-key failures must complete within the cache deadline");

        assert!(first.is_none());
        assert!(second.is_none());
        assert_eq!(
            runner.calls.load(Ordering::Acquire),
            2,
            "concurrent callers must share the first failed version/help pair"
        );
        assert!(probe.probe(&executable).await.is_none());
        assert_eq!(
            runner.calls.load(Ordering::Acquire),
            4,
            "a later caller must be allowed to retry a transient version/help failure"
        );
    }

    #[tokio::test]
    async fn capability_cache_never_reuses_exact_build_attestation() {
        let root = tempfile::tempdir().expect("probe directory");
        let executable = root.path().join("claude");
        std::fs::write(&executable, b"approved").expect("approved executable");
        let runner = Arc::new(CountingClaudeProbeRunner::default());
        let attestor = Arc::new(ContentAwareClaudeAttestor::default());
        let probe = CachedClaudeCapabilityProbe::with_attestor(runner.clone(), attestor.clone());

        assert!(
            probe
                .probe(&executable)
                .await
                .is_some_and(|capabilities| capabilities.additive_hook_merge)
        );
        assert!(
            probe
                .probe(&executable)
                .await
                .is_some_and(|capabilities| capabilities.additive_hook_merge)
        );
        assert_eq!(
            runner.calls.load(Ordering::Acquire),
            2,
            "version/help capabilities may be cached"
        );
        assert_eq!(
            attestor.calls.load(Ordering::Acquire),
            2,
            "exact-build approval must run once per launch preparation"
        );
    }

    #[tokio::test]
    async fn same_length_rewrite_with_restored_mtime_cannot_reuse_build_approval() {
        let root = tempfile::tempdir().expect("probe directory");
        let executable = root.path().join("claude");
        let timestamp_source = root.path().join("approved-timestamp");
        std::fs::write(&executable, b"approved").expect("approved executable");
        std::fs::write(&timestamp_source, b"timestamp").expect("timestamp source");
        restore_modified_time(&executable, &timestamp_source);
        let original_modified = std::fs::metadata(&executable)
            .expect("approved metadata")
            .modified()
            .expect("approved modified time");
        let runner = Arc::new(CountingClaudeProbeRunner::default());
        let attestor = Arc::new(ContentAwareClaudeAttestor::default());
        let probe = CachedClaudeCapabilityProbe::with_attestor(runner, attestor.clone());

        assert!(
            probe
                .probe(&executable)
                .await
                .is_some_and(|capabilities| capabilities.additive_hook_merge)
        );
        std::fs::write(&executable, b"rejected").expect("same-length replacement");
        restore_modified_time(&timestamp_source, &executable);
        let replaced_metadata = std::fs::metadata(&executable).expect("replacement metadata");
        assert_eq!(replaced_metadata.len(), 8);
        assert_eq!(
            replaced_metadata
                .modified()
                .expect("replacement modified time"),
            original_modified,
            "the regression fixture must defeat length/mtime-only fingerprints"
        );

        assert!(
            probe
                .probe(&executable)
                .await
                .is_some_and(|capabilities| !capabilities.additive_hook_merge),
            "the rewritten executable must fail closed"
        );
        assert_eq!(
            attestor.calls.load(Ordering::Acquire),
            2,
            "the rewritten executable must be re-attested"
        );
    }

    #[tokio::test]
    async fn executable_replacement_during_attestation_fails_identity_stability_check() {
        let root = tempfile::tempdir().expect("probe directory");
        let executable = root.path().join("claude");
        let replacement = root.path().join("claude-replacement");
        std::fs::write(&executable, b"approved").expect("approved executable");
        std::fs::write(&replacement, b"replaced").expect("same-length replacement");
        restore_modified_time(&executable, &replacement);
        let probe = CachedClaudeCapabilityProbe::with_attestor(
            Arc::new(CountingClaudeProbeRunner::default()),
            Arc::new(ReplacingClaudeAttestor { replacement }),
        );

        let capabilities = probe.probe(&executable).await;

        assert!(
            !capabilities.is_some_and(|capabilities| capabilities.additive_hook_merge),
            "a path whose file identity changes during attestation must fail closed"
        );
    }

    #[test]
    fn additive_hook_attestation_rejects_any_unapproved_codesign_identity() {
        let approved = format!(
            "Identifier={APPROVED_ADDITIVE_HOOK_IDENTIFIER}\n\
             TeamIdentifier={APPROVED_ADDITIVE_HOOK_TEAM}\n\
             CDHash={APPROVED_ADDITIVE_HOOK_CDHASH}\n\
             CandidateCDHashFull sha256={APPROVED_ADDITIVE_HOOK_CDHASH_FULL}\n"
        );
        assert!(codesign_output_matches_approved_build(&approved));
        for marker in [
            APPROVED_ADDITIVE_HOOK_IDENTIFIER,
            APPROVED_ADDITIVE_HOOK_TEAM,
            APPROVED_ADDITIVE_HOOK_CDHASH,
            APPROVED_ADDITIVE_HOOK_CDHASH_FULL,
        ] {
            assert!(
                !codesign_output_matches_approved_build(&approved.replace(marker, "unapproved")),
                "every allowlisted codesign marker must be mandatory"
            );
        }
    }

    fn attestation_fixture_executable() -> Option<PathBuf> {
        let executable =
            PathBuf::from(std::env::var_os("CODDY_STUDIO_CLAUDE_ATTESTATION_FIXTURE")?);
        if !executable.is_file() {
            return None;
        }
        #[cfg(feature = "hermetic-test-guard")]
        let executable =
            crate::hermetic_guard::checked_provider_executable(&executable, None, None)?;
        Some(executable)
    }

    #[cfg(feature = "hermetic-test-guard")]
    #[test]
    fn attestation_fixture_never_uses_an_ambient_home_installation() {
        const CASE: &str = "claude-attestation-owned-home";
        const TEST: &str = "provider_terminal::claude::tests::attestation_fixture_never_uses_an_ambient_home_installation";
        if TestSandbox::is_isolated_case(CASE, TEST) {
            assert!(attestation_fixture_executable().is_none());
            println!(
                "CODDY_STUDIO_ATTESTATION_FIXTURE_CHILD_PID={}",
                std::process::id()
            );
            return;
        }
        let sandbox = TestSandbox::new(CASE);
        let path = sandbox.path(".local/share/claude/versions/2.1.220");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"owned installation tripwire; never executed").unwrap();
        let output = sandbox.run_isolated_case_without_environment(
            CASE,
            TEST,
            &[
                ("HOME", sandbox.root().as_os_str()),
                ("USERPROFILE", sandbox.root().as_os_str()),
            ],
            &["CODDY_STUDIO_CLAUDE_ATTESTATION_FIXTURE"],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .matches("CODDY_STUDIO_ATTESTATION_FIXTURE_CHILD_PID=")
                .count(),
            1,
            "exactly one owned child must complete and be reaped"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let child_pid = stdout
            .split("CODDY_STUDIO_ATTESTATION_FIXTURE_CHILD_PID=")
            .nth(1)
            .and_then(|value| value.split_whitespace().next())
            .expect("completed child PID marker");
        eprintln!("attestation fixture: exactly one child {child_pid} completed and reaped");
    }

    #[tokio::test]
    async fn installed_v21220_binary_matches_the_approved_additive_hook_attestation() {
        let Some(executable) = attestation_fixture_executable() else {
            return;
        };

        assert!(
            approved_additive_hook_build(&executable, "2.1.220").await,
            "the locally installed, versioned Claude build must match its reviewed fingerprint and source-semantic markers"
        );
    }

    #[tokio::test]
    async fn installed_v21220_probe_and_per_launch_attestation_fit_preparation_budget() {
        let Some(executable) = attestation_fixture_executable() else {
            return;
        };
        let probe = CachedClaudeCapabilityProbe::new(Arc::new(
            SystemClaudeCapabilityProbeRunner::default(),
        ));

        for phase in ["cold", "cached-capabilities"] {
            let started = std::time::Instant::now();
            let capabilities =
                tokio::time::timeout(CLAUDE_PREPARATION_BUDGET, probe.probe(&executable))
                    .await
                    .unwrap_or_else(|_| {
                        panic!("{phase} installed probe exceeded {CLAUDE_PREPARATION_BUDGET:?}")
                    })
                    .unwrap_or_else(|| panic!("{phase} installed probe failed"));
            assert!(capabilities.additive_hook_merge);
            assert!(
                started.elapsed() < CLAUDE_PREPARATION_BUDGET,
                "{phase} installed preparation exceeded {CLAUDE_PREPARATION_BUDGET:?}"
            );
        }
    }

    #[tokio::test]
    async fn disabling_activity_cancels_transcript_recovery_before_record_processing() {
        let activity = Arc::new(TerminalAgentActivityControl::enabled());
        let generation = TerminalObserverGeneration::new(
            "thread-recovery-cancel".to_owned(),
            "terminal-recovery-cancel".to_owned(),
        );
        let cancellation = CancellationToken::new();
        let recovery_cancellation = cancellation.clone();
        let (started, started_receiver) = oneshot::channel();
        let recovery_activity = activity.clone();
        let recovery_generation = generation.clone();
        let recovery = tokio::spawn(async move {
            await_claude_recovery_while_current(
                &recovery_activity,
                &recovery_generation.observation(),
                cancellation,
                async move {
                    let _ = started.send(());
                    recovery_cancellation.cancelled().await;
                },
            )
            .await
        });

        started_receiver.await.expect("transcript recovery started");
        let _ = activity.transition_state(false);

        assert_eq!(
            recovery.await.expect("recovery task"),
            Err(StatusCode::NO_CONTENT),
            "a dormant transition must cancel recovery before recovered records are normalized",
        );
    }

    #[tokio::test]
    async fn unexpected_server_exit_interrupts_correlated_activity() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let generation = TerminalObserverGeneration::new(
            "thread-server-exit".to_owned(),
            "terminal-server-exit".to_owned(),
        );
        let publisher = TerminalGenerationActivityPublisher::new(
            generation.clone(),
            projection.clone(),
            Arc::new(tokio::sync::Mutex::new(())),
        );
        let root = tempfile::tempdir().expect("observer resources");
        let generation_dir = root.path().join("generation");
        std::fs::create_dir(&generation_dir).expect("generation directory");
        let overlay_path = generation_dir.join("settings.json");
        std::fs::write(&overlay_path, b"{}").expect("overlay");
        let pinned_executable = generation_dir.join("claude");
        std::fs::write(&pinned_executable, b"approved").expect("pinned executable");
        let inner = Arc::new(ClaudeObserverInner {
            resources: Arc::new(ClaudeObserverResources {
                listener: Mutex::new(None),
                overlay_path,
                runtime_dir: root.path().to_path_buf(),
                generation_dir,
                cleaned: AtomicBool::new(false),
            }),
            publisher,
            provider_instance_id: "claudeAgent".to_owned(),
            token: Arc::new("unused".to_owned()),
            correlation: Arc::new("unused".to_owned()),
            spawned: AtomicBool::new(true),
            activity: Arc::new(TerminalAgentActivityControl::enabled()),
            listener_lifecycle: AtomicU64::new(pack_claude_listener_lifecycle(
                ClaudeListenerLifecycle {
                    ready: false,
                    epoch: 0,
                },
            )),
        });
        let (requests, receiver) = mpsc::channel(1);
        let (stop_server, server_stopped) = oneshot::channel();
        let activity = inner.activity.clone();
        let admission = inner.activity.admit().expect("root hook admission");
        let observer = tokio::spawn(drive_claude_observer(
            inner,
            generation.observation(),
            receiver,
            Box::pin(async move {
                let _ = server_stopped.await;
            }),
        ));
        let (response, response_receiver) = oneshot::channel();
        requests
            .send(ClaudeHookRequest {
                value: json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": "root-session",
                    "agent_id": "agent-1",
                    "agent_type": "Explore",
                    "transcript_path": root.path().join("root-session.jsonl"),
                    "cwd": root.path(),
                }),
                admission,
                response,
            })
            .await
            .expect("root hook");
        assert_eq!(
            response_receiver.await.expect("root hook response"),
            StatusCode::NO_CONTENT
        );

        stop_server.send(()).expect("inject server exit");
        observer.await.expect("observer task");
        assert_eq!(
            activity.latest_observation(),
            Some(TerminalAgentActivityObservation {
                state: activity.snapshot(),
                epoch: 0,
                kind: TerminalAgentActivityObservationKind::Unavailable,
            }),
            "observer cleanup must publish unavailable before it awaits reconciliation",
        );
        let snapshot = projection
            .snapshot(&ActivityScopeRef::Terminal {
                thread_id: "thread-server-exit".to_owned(),
                terminal_id: "terminal-server-exit".to_owned(),
            })
            .await
            .expect("retained activity");
        assert_eq!(snapshot.actors.len(), 1);
        assert_eq!(
            snapshot.actors[0].status,
            ActivityLifecycle::Interrupted,
            "unexpected sink termination must not leave activity live"
        );
    }

    #[tokio::test]
    async fn dormant_shutdown_reconciliation_does_not_publish_interruptions() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let generation = TerminalObserverGeneration::new(
            "thread-dormant-shutdown".to_owned(),
            "terminal-dormant-shutdown".to_owned(),
        );
        let root = tempfile::tempdir().expect("observer resources");
        let generation_dir = root.path().join("generation");
        std::fs::create_dir(&generation_dir).expect("generation directory");
        let inner = ClaudeObserverInner {
            resources: Arc::new(ClaudeObserverResources {
                listener: Mutex::new(None),
                overlay_path: generation_dir.join("settings.json"),
                runtime_dir: root.path().to_path_buf(),
                generation_dir,
                cleaned: AtomicBool::new(false),
            }),
            publisher: TerminalGenerationActivityPublisher::new(
                generation,
                projection.clone(),
                Arc::new(tokio::sync::Mutex::new(())),
            ),
            provider_instance_id: "claudeAgent".to_owned(),
            token: Arc::new("unused".to_owned()),
            correlation: Arc::new("unused".to_owned()),
            spawned: AtomicBool::new(true),
            activity: Arc::new(TerminalAgentActivityControl::enabled()),
            listener_lifecycle: AtomicU64::new(pack_claude_listener_lifecycle(
                ClaudeListenerLifecycle {
                    ready: true,
                    epoch: 1,
                },
            )),
        };
        let mut actors = HashMap::new();
        let actor = ActivityActorSummary::try_new(
            "agent-1",
            None,
            "Explore",
            None,
            Some("claude"),
            ActivityLifecycle::Running,
            None,
            "2026-07-30T00:00:00Z",
            "2026-07-30T00:00:00Z",
            None,
        )
        .expect("activity actor");
        actors.insert(actor.id.clone(), actor);
        let _ = inner.activity.transition_state(false);

        interrupt_claude_activity(&inner, &actors, &HashMap::new(), 0).await;

        assert!(
            projection
                .snapshot(&ActivityScopeRef::Terminal {
                    thread_id: "thread-dormant-shutdown".to_owned(),
                    terminal_id: "terminal-dormant-shutdown".to_owned(),
                })
                .await
                .is_err(),
            "dormant shutdown reconciliation must not publish interruptions",
        );
    }
}
