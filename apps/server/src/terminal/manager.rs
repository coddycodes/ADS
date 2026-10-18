use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, LazyLock, mpsc},
    time::Duration,
};

#[cfg(test)]
use std::sync::Mutex as StdMutex;

use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::model::{TerminalConsoleTheme, terminal_console_theme_from_env};
use super::{
    PortablePtyBackend, PtyBackend, PtyExit, PtyProcess, PtySpawnInput, TerminalAttachInput,
    TerminalEvent, TerminalLaunchCommand, TerminalMetadataEvent, TerminalOpenInput,
    TerminalRemovalReason, TerminalRestartInput, TerminalSessionSnapshot, TerminalSize,
    TerminalStatus, TerminalSummary, history::TerminalHistory,
};
use crate::{
    diagnostics::{
        AttributionKind, AttributionScope, NativeProcessSampler, ProcessAttributionRegistry,
        ProcessRegistration, ProcessRegistrationError, ProcessRegistrationMetadata, ProcessSampler,
        RegistrationSource, build_descendant_entries,
        profiler::exits::{self, Exited, Watch},
    },
    process::{Platform, ProcessCleanupReport, ShellCandidate, resolve_shell_candidates},
    provider_terminal::{
        PreparedTerminalObserver, TerminalAgentActivityTransition, TerminalLaunchPreparation,
        TerminalLaunchPreparationInput, TerminalLaunchPreparer, TerminalObserverCancellationReason,
        TerminalObserverGeneration, TerminalObserverGenerationLease,
    },
};

const DEFAULT_SUBPROCESS_POLL_INTERVAL: Duration = Duration::from_secs(1);
const OBSERVER_CALLBACK_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_PREPARATION_EXECUTION_BUDGET: Duration = Duration::from_secs(1);
/// The native form of a provider command may need a cold capability probe, which OpenCode's
/// allows 900 ms; giving up sooner would launch a command the installed version rejects.
const NATIVE_COMMAND_TIMEOUT: Duration = MAX_PREPARATION_EXECUTION_BUDGET;
const MAX_ISOLATED_OBSERVER_CALLBACKS: usize = 8;
const MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS: usize = 16;
const MAX_BLOCKING_THREADS_PER_ISOLATED_OBSERVER_CALLBACK: usize = 1;
const OBSERVER_CALLBACK_JOIN_REAPER_POLL_INTERVAL: Duration = Duration::from_millis(1);
const OBSERVER_WORKER_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const OBSERVER_WORKER_ABORT_TIMEOUT: Duration = Duration::from_millis(50);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);
/// Reads published after an exit: twice the PTY channel's 1024, as lag notices count too.
const MAX_DRAINED_OUTPUT_CHUNKS: usize = 2_048;
/// The most output one publication gathers from reads already waiting.
const MAX_COALESCED_OUTPUT_BYTES: usize = 64 * 1024;
const TERMINAL_CLOSE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_TERMINAL_LABEL_LENGTH: usize = 128;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SubprocessInspection {
    pub has_running_subprocess: bool,
    pub child_command_label: Option<String>,
    pub process_ids: Vec<u32>,
}

pub trait TerminalSubprocessInspector: std::fmt::Debug + Send + Sync {
    fn inspect(
        &self,
        terminal_pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<SubprocessInspection, String>> + Send + '_>>;
}

pub(crate) type TerminalProcessExitCallback =
    Arc<dyn Fn(PathBuf) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Debug, Default)]
struct NativeTerminalSubprocessInspector {
    sampler: NativeProcessSampler,
}

impl TerminalSubprocessInspector for NativeTerminalSubprocessInspector {
    fn inspect(
        &self,
        terminal_pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<SubprocessInspection, String>> + Send + '_>> {
        Box::pin(async move {
            if terminal_pid == 0 {
                return Ok(SubprocessInspection::default());
            }

            let rows = self
                .sampler
                .sample()
                .await
                .map_err(|error| error.to_string())?;
            let descendants = build_descendant_entries(&rows, terminal_pid);
            let Some(first_child) = descendants.iter().find(|entry| entry.depth == 0) else {
                return Ok(SubprocessInspection::default());
            };

            let mut process_ids = Vec::with_capacity(descendants.len() + 1);
            process_ids.push(terminal_pid);
            process_ids.extend(descendants.iter().map(|entry| entry.pid));

            Ok(SubprocessInspection {
                has_running_subprocess: true,
                child_command_label: normalize_child_command_name(&first_child.command)
                    .map(|label| truncate_terminal_label(&label)),
                process_ids,
            })
        })
    }
}

#[derive(Clone)]
pub struct TerminalManagerOptions {
    pub history_line_limit: usize,
    pub event_capacity: usize,
    pub preferred_shell: Option<String>,
    pub subprocess_poll_interval: Duration,
    pub subprocess_inspector: Option<Arc<dyn TerminalSubprocessInspector>>,
    pub launch_preparer: Option<Arc<dyn TerminalLaunchPreparer>>,
    /// Server-owned variables merged into every spawn's environment after the caller's, so
    /// they win. Called with the thread id once per process start, never for an open that
    /// finds the terminal running.
    pub session_env: Option<TerminalSessionEnv>,
}

/// See [`TerminalManagerOptions::session_env`].
pub type TerminalSessionEnv = Arc<
    dyn for<'a> Fn(
            &'a str,
            &'a mut std::collections::BTreeMap<String, String>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + Sync,
>;

impl std::fmt::Debug for TerminalManagerOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalManagerOptions")
            .field("history_line_limit", &self.history_line_limit)
            .field("event_capacity", &self.event_capacity)
            .field("preferred_shell", &self.preferred_shell)
            .field("subprocess_poll_interval", &self.subprocess_poll_interval)
            .field(
                "subprocess_inspector",
                &self.subprocess_inspector.as_ref().map(|_| "configured"),
            )
            .field(
                "launch_preparer",
                &self.launch_preparer.as_ref().map(|_| "configured"),
            )
            .field(
                "session_env",
                &self.session_env.as_ref().map(|_| "configured"),
            )
            .finish()
    }
}

impl TerminalManagerOptions {
    /// Wraps a [`Self::session_env`] hook; the bound lets a closure infer its signature.
    pub fn session_env_hook<F>(hook: F) -> TerminalSessionEnv
    where
        F: for<'a> Fn(
                &'a str,
                &'a mut std::collections::BTreeMap<String, String>,
            ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
            + Send
            + Sync
            + 'static,
    {
        Arc::new(hook)
    }
}

impl Default for TerminalManagerOptions {
    fn default() -> Self {
        Self {
            history_line_limit: 5_000,
            event_capacity: 512,
            preferred_shell: None,
            subprocess_poll_interval: DEFAULT_SUBPROCESS_POLL_INTERVAL,
            subprocess_inspector: None,
            launch_preparer: None,
            session_env: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum TerminalError {
    #[error("terminal manager is shut down")]
    Shutdown,
    #[error("terminal publication was cancelled")]
    PublicationCancelled,
    #[error("terminal cwd does not exist: {0}")]
    CwdNotFound(String),
    #[error("terminal cwd is not a directory: {0}")]
    CwdNotDirectory(String),
    #[error("unknown terminal thread: {thread_id}, terminal: {terminal_id}")]
    NotFound {
        thread_id: String,
        terminal_id: String,
    },
    #[error("terminal is not running for thread: {thread_id}, terminal: {terminal_id}")]
    NotRunning {
        thread_id: String,
        terminal_id: String,
    },
    #[error("failed to spawn terminal; attempted {attempted:?}: {message}")]
    Spawn {
        attempted: Vec<String>,
        message: String,
    },
    #[error("terminal I/O failed: {0}")]
    Io(String),
    #[error(transparent)]
    Input(#[from] super::input::TerminalInputError),
    #[error("terminal processes did not exit before cleanup timed out")]
    Close,
    #[error("worktree removal is in progress for terminal thread: {thread_id}")]
    WorktreeRemovalInProgress { thread_id: String },
}

struct SizeOwner {
    claim: String,
    attachment: uuid::Uuid,
}

struct Session {
    generation: Arc<SessionGeneration>,
    thread_id: String,
    terminal_id: String,
    cwd: String,
    worktree_path: Option<String>,
    status: TerminalStatus,
    pid: Option<u32>,
    history: TerminalHistory,
    exit_code: Option<i32>,
    exit_signal: Option<i32>,
    console_theme: Option<TerminalConsoleTheme>,
    label: String,
    has_running_subprocess: bool,
    child_command_label: Option<String>,
    updated_at: String,
    sequence: u64,
    cols: u16,
    rows: u16,
    size_owner: Option<SizeOwner>,
    first_attachment_grant_used: bool,
    osc_color_responder_active: bool,
    process: Option<Arc<dyn PtyProcess>>,
    attribution_registration: Option<ProcessRegistration>,
    observer: Option<PreparedObserverHandle>,
    private_output: StreamingSecretRedactor,
    /// Sticky once set: a host thread's center panel that every client adopts.
    center_panel: bool,
    /// Client-supplied command the current process was launched with (never
    /// the prepared one with private env); `None` for a default shell.
    command: Option<TerminalLaunchCommand>,
    /// The executable the current process was spawned from, and when.
    executable: String,
    started: std::time::Instant,
}

/// What a replacement process keeps from the session it replaces.
#[derive(Default)]
struct SessionCarryOver {
    center_panel: bool,
}

type SessionKey = (String, String);
type SharedSession = Arc<Mutex<Session>>;

#[derive(Clone)]
pub(crate) struct TerminalSessionIdentity {
    key: SessionKey,
    session: SharedSession,
    generation: Arc<SessionGeneration>,
    process: Arc<dyn PtyProcess>,
}

impl TerminalSessionIdentity {
    pub(super) fn input_key(&self) -> (String, String) {
        self.key.clone()
    }

    pub(super) fn input_cancellation(&self) -> CancellationToken {
        self.generation.cancellation.clone()
    }

    pub(super) async fn write_input(
        &self,
        data: &str,
        cancelled: &CancellationToken,
        connection_closed: &CancellationToken,
    ) -> Result<(), TerminalError> {
        let _publication = tokio::select! {
            biased;
            () = cancelled.cancelled() => return Err(TerminalError::PublicationCancelled),
            () = connection_closed.cancelled() => return Err(TerminalError::PublicationCancelled),
            () = self.generation.cancellation.cancelled() => return Err(TerminalError::PublicationCancelled),
            guard = self.generation.publication.lock() => guard,
        };
        let session = self.session.lock().await;
        if cancelled.is_cancelled()
            || connection_closed.is_cancelled()
            || self.generation.is_invalidated()
            || self
                .generation
                .closing
                .load(std::sync::atomic::Ordering::Acquire)
            || !Arc::ptr_eq(&session.generation, &self.generation)
            || session.status != TerminalStatus::Running
            || !session
                .process
                .as_ref()
                .is_some_and(|process| Arc::ptr_eq(process, &self.process))
        {
            return Err(TerminalError::NotRunning {
                thread_id: self.key.0.clone(),
                terminal_id: self.key.1.clone(),
            });
        }
        drop(session);
        self.process.write(data).map_err(TerminalError::Io)
    }
}

#[derive(Clone)]
struct StreamingSecretRedactor {
    secrets: Arc<Vec<String>>,
    pending: String,
    gap_redaction_bytes: usize,
}

impl StreamingSecretRedactor {
    fn new(mut secrets: Vec<String>) -> Self {
        secrets.retain(|secret| !secret.is_empty());
        secrets.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        secrets.dedup();
        Self {
            secrets: Arc::new(secrets),
            pending: String::new(),
            gap_redaction_bytes: 0,
        }
    }

    fn push(&mut self, chunk: &str) -> String {
        let mut boundary = 0;
        if self.gap_redaction_bytes > 0 {
            for (index, character) in chunk.char_indices() {
                if self.gap_redaction_bytes == 0 {
                    break;
                }
                boundary = index + character.len_utf8();
                self.gap_redaction_bytes = self
                    .gap_redaction_bytes
                    .saturating_sub(character.len_utf8());
            }
        }
        let mut output = if boundary > 0 {
            "[redacted]".to_owned()
        } else {
            String::new()
        };
        self.pending.push_str(&chunk[boundary..]);
        output.push_str(&self.process(false));
        output
    }

    fn note_gap(&mut self) -> String {
        let withheld_prefix = !self.pending.is_empty();
        self.pending.clear();
        self.gap_redaction_bytes = self
            .secrets
            .first()
            .map_or(0, |secret| secret.len().saturating_sub(1));
        if withheld_prefix {
            "[redacted]".to_owned()
        } else {
            String::new()
        }
    }

    fn finish(&mut self) -> String {
        self.gap_redaction_bytes = 0;
        self.process(true)
    }

    fn process(&mut self, end_of_stream: bool) -> String {
        let input = std::mem::take(&mut self.pending);
        let mut output = String::new();
        let mut index = 0;
        while index < input.len() {
            let remaining = &input[index..];
            if self
                .secrets
                .iter()
                .any(|secret| secret.len() > remaining.len() && secret.starts_with(remaining))
            {
                if end_of_stream {
                    output.push_str("[redacted]");
                } else {
                    self.pending.push_str(remaining);
                }
                break;
            }
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| remaining.starts_with(secret.as_str()))
            {
                output.push_str("[redacted]");
                index += secret.len();
                continue;
            }
            let character = remaining
                .chars()
                .next()
                .expect("non-empty redaction remainder");
            output.push(character);
            index += character.len_utf8();
        }
        output
    }
}

impl std::fmt::Debug for StreamingSecretRedactor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamingSecretRedactor")
            .field("secret_count", &self.secrets.len())
            .field("pending_bytes", &self.pending.len())
            .field("gap_redaction_bytes", &self.gap_redaction_bytes)
            .finish()
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("thread_id", &self.thread_id)
            .field("terminal_id", &self.terminal_id)
            .field("status", &self.status)
            .field("pid", &self.pid)
            .field("sequence", &self.sequence)
            .field("observer", &self.observer)
            .field("private_output", &self.private_output)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct PreparedObserverHandle {
    inner: Arc<PreparedObserverState>,
}

struct PreparedObserverState {
    observer: Arc<dyn PreparedTerminalObserver>,
    generation: TerminalObserverGeneration,
    callback_isolation: ObserverCallbackIsolation,
}

impl PreparedObserverHandle {
    fn new(
        observer: Box<dyn PreparedTerminalObserver>,
        generation: TerminalObserverGeneration,
        callback_isolation: ObserverCallbackIsolation,
    ) -> Self {
        Self {
            inner: Arc::new(PreparedObserverState {
                observer: Arc::from(observer),
                generation,
                callback_isolation,
            }),
        }
    }

    async fn on_spawned(&self, pid: u32, generation: TerminalObserverGenerationLease) -> bool {
        if self.inner.generation.cancellation_reason().is_some() {
            return false;
        }
        let state = self.inner.clone();
        run_observer_setup(state.callback_isolation.clone(), "on_spawned", move || {
            if state.generation.cancellation_reason().is_some() {
                return false;
            }
            state
                .observer
                .on_spawned(pid, generation, state.generation.worker_context());
            true
        })
        .await
        .unwrap_or(false)
    }

    async fn is_ready_for_on_spawned(&self) -> bool {
        if self.inner.generation.cancellation_reason().is_some() {
            return false;
        }
        let state = self.inner.clone();
        run_observer_setup(
            state.callback_isolation.clone(),
            "is_ready_for_on_spawned",
            move || {
                state.generation.cancellation_reason().is_none()
                    && state.observer.is_ready_for_on_spawned()
            },
        )
        .await
        .unwrap_or(false)
    }

    async fn cancel(&self, reason: TerminalObserverCancellationReason) {
        self.inner.generation.request_cancellation(reason);
        self.inner
            .generation
            .shutdown_workers(
                OBSERVER_WORKER_SHUTDOWN_TIMEOUT,
                OBSERVER_WORKER_ABORT_TIMEOUT,
            )
            .await;
    }

    async fn set_agent_activity_enabled(&self, enabled: bool) -> TerminalAgentActivityTransition {
        if self.inner.generation.cancellation_reason().is_some() {
            return TerminalAgentActivityTransition::default();
        }
        let state = self.inner.clone();
        let callback_timeout = if enabled {
            state
                .observer
                .agent_activity_enable_ack_timeout()
                .map(|timeout| timeout.saturating_add(OBSERVER_CALLBACK_TIMEOUT))
                .unwrap_or(OBSERVER_CALLBACK_TIMEOUT)
        } else {
            OBSERVER_CALLBACK_TIMEOUT
        };
        run_observer_callback(
            state.callback_isolation.clone(),
            "set_agent_activity_enabled",
            callback_timeout,
            async move {
                state
                    .observer
                    .set_agent_activity_enabled(
                        enabled,
                        state.generation.observation(),
                        state.generation.worker_context(),
                    )
                    .await
            },
        )
        .await
        .unwrap_or(TerminalAgentActivityTransition {
            failed: 1,
            ..TerminalAgentActivityTransition::default()
        })
    }
}

#[derive(Clone, Debug)]
struct ObserverCallbackIsolation {
    slots: Arc<tokio::sync::Semaphore>,
    global_slots: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    joined_callback_thread: Arc<StdMutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    #[cfg(test)]
    callback_thread_exit_hook: Arc<StdMutex<Option<Arc<ObserverCallbackThreadExitTestHook>>>>,
}

struct ObserverCallbackThreadJoin {
    callback: &'static str,
    thread: std::thread::JoinHandle<()>,
    manager_permit: tokio::sync::OwnedSemaphorePermit,
    global_permit: tokio::sync::OwnedSemaphorePermit,
    #[cfg(test)]
    joined_callback_thread: Option<tokio::sync::oneshot::Sender<()>>,
}

struct ObserverCallbackJoinReaper {
    sender: mpsc::SyncSender<ObserverCallbackThreadJoin>,
}

#[cfg(test)]
#[derive(Debug)]
struct ObserverCallbackThreadExitTestHook {
    reached: Arc<tokio::sync::Semaphore>,
    release: Arc<(StdMutex<bool>, std::sync::Condvar)>,
}

#[cfg(test)]
struct ObserverCallbackThreadExitGuard(Arc<ObserverCallbackThreadExitTestHook>);

#[cfg(test)]
std::thread_local! {
    static OBSERVER_CALLBACK_THREAD_EXIT_GUARD:
        std::cell::RefCell<Option<ObserverCallbackThreadExitGuard>> =
            const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
impl Default for ObserverCallbackThreadExitTestHook {
    fn default() -> Self {
        Self {
            reached: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new((StdMutex::new(false), std::sync::Condvar::new())),
        }
    }
}

#[cfg(test)]
impl ObserverCallbackThreadExitTestHook {
    async fn wait_until_reached(&self) {
        self.reached
            .acquire()
            .await
            .expect("callback thread exit hook remains open")
            .forget();
    }

    fn block_before_exit(&self) {
        self.reached.add_permits(1);
        let mut released = self
            .release
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*released {
            released = self
                .release
                .1
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn release(&self) {
        *self
            .release
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.release.1.notify_all();
    }
}

#[cfg(test)]
impl Drop for ObserverCallbackThreadExitGuard {
    fn drop(&mut self) {
        self.0.block_before_exit();
    }
}

impl ObserverCallbackThreadJoin {
    fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    fn join(self) {
        if self.thread.join().is_err() {
            tracing::warn!(
                callback = self.callback,
                "provider terminal observer callback thread panicked"
            );
        }
        drop(self.manager_permit);
        drop(self.global_permit);
        #[cfg(test)]
        if let Some(joined_callback_thread) = self.joined_callback_thread {
            let _ = joined_callback_thread.send(());
        }
    }
}

impl ObserverCallbackJoinReaper {
    fn start() -> Option<Self> {
        let (sender, receiver) = mpsc::sync_channel::<ObserverCallbackThreadJoin>(
            MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS,
        );
        std::thread::Builder::new()
            .name("terminal-observer-callback-join-reaper".to_owned())
            .spawn(move || Self::run(receiver))
            .ok()?;
        Some(Self { sender })
    }

    fn submit(
        &self,
        callback: ObserverCallbackThreadJoin,
    ) -> Result<(), ObserverCallbackThreadJoin> {
        self.sender.try_send(callback).map_err(|error| match error {
            mpsc::TrySendError::Full(callback) | mpsc::TrySendError::Disconnected(callback) => {
                callback
            }
        })
    }

    fn run(receiver: mpsc::Receiver<ObserverCallbackThreadJoin>) {
        let mut callbacks: Vec<ObserverCallbackThreadJoin> =
            Vec::with_capacity(MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS);
        loop {
            let mut index = 0;
            while index < callbacks.len() {
                if callbacks[index].is_finished() {
                    callbacks.swap_remove(index).join();
                } else {
                    index += 1;
                }
            }

            let callback = if callbacks.is_empty() {
                match receiver.recv() {
                    Ok(callback) => callback,
                    Err(_) => return,
                }
            } else {
                match receiver.recv_timeout(OBSERVER_CALLBACK_JOIN_REAPER_POLL_INTERVAL) {
                    Ok(callback) => callback,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        for callback in callbacks {
                            callback.join();
                        }
                        return;
                    }
                }
            };

            if callbacks.len() == MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS {
                tracing::error!(
                    callback = callback.callback,
                    capacity = MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS,
                    "provider terminal observer callback join reaper capacity was exceeded"
                );
                callback.join();
            } else {
                callbacks.push(callback);
            }
        }
    }
}

impl Default for ObserverCallbackIsolation {
    fn default() -> Self {
        Self {
            slots: Arc::new(tokio::sync::Semaphore::new(MAX_ISOLATED_OBSERVER_CALLBACKS)),
            global_slots: GLOBAL_OBSERVER_CALLBACK_SLOTS.clone(),
            #[cfg(test)]
            joined_callback_thread: Arc::new(StdMutex::new(None)),
            #[cfg(test)]
            callback_thread_exit_hook: Arc::new(StdMutex::new(None)),
        }
    }
}

#[cfg(test)]
impl ObserverCallbackIsolation {
    fn with_slots(
        slots: Arc<tokio::sync::Semaphore>,
        global_slots: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            slots,
            global_slots,
            joined_callback_thread: Arc::new(StdMutex::new(None)),
            callback_thread_exit_hook: Arc::new(StdMutex::new(None)),
        }
    }

    fn callback_thread_exit_hook(&self) -> Arc<ObserverCallbackThreadExitTestHook> {
        let hook = Arc::new(ObserverCallbackThreadExitTestHook::default());
        assert!(
            self.callback_thread_exit_hook
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .replace(hook.clone())
                .is_none(),
            "only one callback thread exit can be observed at a time"
        );
        hook
    }

    fn callback_thread_joined(&self) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        assert!(
            self.joined_callback_thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .replace(sender)
                .is_none(),
            "only one callback thread join can be observed at a time"
        );
        receiver
    }

    fn take_callback_thread_joined(&self) -> Option<tokio::sync::oneshot::Sender<()>> {
        self.joined_callback_thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    fn take_callback_thread_exit_hook(&self) -> Option<Arc<ObserverCallbackThreadExitTestHook>> {
        self.callback_thread_exit_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

static GLOBAL_OBSERVER_CALLBACK_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS,
        ))
    });
static GLOBAL_OBSERVER_CALLBACK_JOIN_REAPER: LazyLock<Option<ObserverCallbackJoinReaper>> =
    LazyLock::new(ObserverCallbackJoinReaper::start);

enum IsolatedCallbackResult<T> {
    Completed(T),
    Panicked,
    RuntimeFailed(String),
}

async fn run_observer_callback<T>(
    isolation: ObserverCallbackIsolation,
    callback: &'static str,
    execution_budget: Duration,
    future: impl Future<Output = T> + Send + 'static,
) -> Option<T>
where
    T: Send + 'static,
{
    run_isolated_observer_callback(isolation, callback, execution_budget, move || {
        // Every admitted asynchronous callback owns one isolation thread and
        // at most one Tokio blocking-pool thread. With the process-global
        // admission cap, callbacks are therefore bounded to 32
        // manager-created threads plus the one process-wide join reaper.
        // Trusted Rust callbacks can still create unmanaged raw std::threads;
        // this is not a sandbox.
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(MAX_BLOCKING_THREADS_PER_ISOLATED_OBSERVER_CALLBACK)
            .build()
        {
            Ok(runtime) => {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    runtime.block_on(future)
                })) {
                    Ok(value) => IsolatedCallbackResult::Completed(value),
                    Err(_) => IsolatedCallbackResult::Panicked,
                }
            }
            Err(error) => IsolatedCallbackResult::RuntimeFailed(error.to_string()),
        }
    })
    .await
}

async fn run_observer_setup<T>(
    isolation: ObserverCallbackIsolation,
    callback: &'static str,
    setup: impl FnOnce() -> T + Send + 'static,
) -> Option<T>
where
    T: Send + 'static,
{
    run_isolated_observer_callback(isolation, callback, OBSERVER_CALLBACK_TIMEOUT, move || {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(setup)) {
            Ok(value) => IsolatedCallbackResult::Completed(value),
            Err(_) => IsolatedCallbackResult::Panicked,
        }
    })
    .await
}

async fn run_isolated_observer_callback<T>(
    isolation: ObserverCallbackIsolation,
    callback: &'static str,
    execution_budget: Duration,
    invoke: impl FnOnce() -> IsolatedCallbackResult<T> + Send + 'static,
) -> Option<T>
where
    T: Send + 'static,
{
    let Some(join_reaper) = GLOBAL_OBSERVER_CALLBACK_JOIN_REAPER.as_ref() else {
        tracing::warn!(
            callback,
            "failed to start provider terminal observer callback join reaper"
        );
        return None;
    };
    let manager_permit = match tokio::time::timeout(
        OBSERVER_CALLBACK_TIMEOUT,
        isolation.slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => {
            tracing::warn!(callback, "provider terminal observer isolation is closed");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                callback,
                timeout_ms = OBSERVER_CALLBACK_TIMEOUT.as_millis(),
                capacity = MAX_ISOLATED_OBSERVER_CALLBACKS,
                "provider terminal observer callback admission timed out"
            );
            return None;
        }
    };
    let global_permit = match tokio::time::timeout(
        OBSERVER_CALLBACK_TIMEOUT,
        isolation.global_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => {
            tracing::warn!(
                callback,
                "global provider terminal observer isolation is closed"
            );
            return None;
        }
        Err(_) => {
            tracing::warn!(
                callback,
                timeout_ms = OBSERVER_CALLBACK_TIMEOUT.as_millis(),
                capacity = MAX_GLOBAL_ISOLATED_OBSERVER_CALLBACKS,
                "global provider terminal observer callback admission timed out"
            );
            return None;
        }
    };
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    #[cfg(test)]
    let callback_thread_exit_hook = isolation.take_callback_thread_exit_hook();
    let thread = std::thread::Builder::new()
        .name(format!("terminal-observer-{callback}"))
        .spawn(move || {
            #[cfg(test)]
            if let Some(callback_thread_exit_hook) = callback_thread_exit_hook {
                OBSERVER_CALLBACK_THREAD_EXIT_GUARD.with(|guard| {
                    guard
                        .borrow_mut()
                        .replace(ObserverCallbackThreadExitGuard(callback_thread_exit_hook));
                });
            }
            let _ = result_sender.send(invoke());
        });
    if let Err(error) = thread {
        tracing::warn!(callback, %error, "failed to start provider terminal observer isolation");
        return None;
    }
    let thread = thread.expect("checked observer callback thread spawn result");
    #[cfg(test)]
    let joined_callback_thread = isolation.take_callback_thread_joined();
    let callback_join = ObserverCallbackThreadJoin {
        callback,
        thread,
        manager_permit,
        global_permit,
        #[cfg(test)]
        joined_callback_thread,
    };
    if let Err(callback_join) = join_reaper.submit(callback_join) {
        tracing::error!(
            callback,
            "provider terminal observer callback join reaper stopped; joining inline"
        );
        callback_join.join();
    }
    match tokio::time::timeout(execution_budget, result_receiver).await {
        Ok(Ok(IsolatedCallbackResult::Completed(value))) => Some(value),
        Ok(Ok(IsolatedCallbackResult::Panicked)) => {
            tracing::warn!(callback, "provider terminal observer callback panicked");
            None
        }
        Ok(Ok(IsolatedCallbackResult::RuntimeFailed(error))) => {
            tracing::warn!(callback, %error, "provider terminal observer runtime failed");
            None
        }
        Ok(Err(_)) => {
            tracing::warn!(
                callback,
                "provider terminal observer isolation ended without a result"
            );
            None
        }
        Err(_) => {
            tracing::warn!(
                callback,
                timeout_ms = execution_budget.as_millis(),
                "provider terminal observer callback timed out"
            );
            None
        }
    }
}

impl std::fmt::Debug for PreparedObserverHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PreparedObserverHandle")
            .field(&self.inner.observer.diagnostic_label())
            .finish()
    }
}

#[derive(Debug)]
struct ClosedSessionNotification {
    thread_id: String,
    terminal_id: String,
    sequence: u64,
}

#[derive(Debug, Default)]
struct ClosedSessions {
    report: ProcessCleanupReport,
    notifications: Vec<ClosedSessionNotification>,
}

/// Owns a newly spawned PTY until a registered, supervised session takes responsibility for it.
struct UncommittedPtyProcess {
    process: Option<Arc<dyn PtyProcess>>,
    _operation: Arc<tokio::sync::OwnedMutexGuard<()>>,
}

impl UncommittedPtyProcess {
    async fn spawn(
        backend: Arc<dyn PtyBackend>,
        input: PtySpawnInput,
        operation: Arc<tokio::sync::OwnedMutexGuard<()>>,
    ) -> Result<Self, String> {
        // The blocking task must return the owner, not a bare process: dropping
        // the join while spawn is in flight must still kill its late result.
        // Retain the key's operation lock until that cleanup has run as well.
        tokio::task::spawn_blocking(move || {
            backend.spawn(&input).map(|process| Self {
                process: Some(process),
                _operation: operation,
            })
        })
        .await
        .map_err(|_| "terminal spawn task failed".to_owned())?
    }

    fn process(&self) -> Arc<dyn PtyProcess> {
        self.process.as_ref().expect("uncommitted process").clone()
    }

    fn commit(mut self) {
        self.process = None;
    }

    async fn cleanup(mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        let exit = process.subscribe_exit();
        if let Err(error) = process.kill() {
            tracing::debug!(
                %error,
                pid = process.pid(),
                "failed to kill rejected terminal process"
            );
        }
        if let Err(error) = wait_for_terminal_process_tree_exit(process.clone(), exit).await {
            tracing::debug!(
                %error,
                pid = process.pid(),
                "rejected terminal process did not finish cleanup cleanly"
            );
        }
    }
}

impl Drop for UncommittedPtyProcess {
    fn drop(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        if let Err(error) = process.kill() {
            tracing::debug!(
                %error,
                pid = process.pid(),
                "failed to kill uncommitted terminal process"
            );
        }
    }
}

#[derive(Debug)]
struct SessionGeneration {
    observation: TerminalObserverGeneration,
    observer: std::sync::Mutex<Option<PreparedObserverHandle>>,
    closing: std::sync::atomic::AtomicBool,
    invalidated: std::sync::atomic::AtomicBool,
    cancellation: CancellationToken,
    publication: Mutex<()>,
    startup: Arc<Mutex<()>>,
    output_started: std::sync::atomic::AtomicBool,
    output_stop: CancellationToken,
    output_completed: CancellationToken,
    #[cfg(test)]
    activity_completed: CancellationToken,
    #[cfg(test)]
    output_barrier: std::sync::Mutex<Option<Arc<PublisherBarrier>>>,
}

impl SessionGeneration {
    fn new(key: &SessionKey, observer_runtime: Option<tokio::runtime::Handle>) -> Self {
        Self {
            observation: TerminalObserverGeneration::new_with_runtime(
                key.0.clone(),
                key.1.clone(),
                observer_runtime,
            ),
            observer: std::sync::Mutex::new(None),
            closing: std::sync::atomic::AtomicBool::new(false),
            invalidated: std::sync::atomic::AtomicBool::new(false),
            cancellation: CancellationToken::new(),
            publication: Mutex::new(()),
            startup: Arc::new(Mutex::new(())),
            output_started: std::sync::atomic::AtomicBool::new(false),
            output_stop: CancellationToken::new(),
            output_completed: CancellationToken::new(),
            #[cfg(test)]
            activity_completed: CancellationToken::new(),
            #[cfg(test)]
            output_barrier: std::sync::Mutex::new(None),
        }
    }

    async fn invalidate(&self) {
        self.observation.invalidate().await;
        self.invalidated
            .store(true, std::sync::atomic::Ordering::Release);
        self.cancellation.cancel();
    }

    fn is_invalidated(&self) -> bool {
        self.invalidated.load(std::sync::atomic::Ordering::Acquire)
    }

    fn is_closing(&self) -> bool {
        self.closing.load(std::sync::atomic::Ordering::Acquire)
    }

    fn install_observer(
        &self,
        observer: PreparedObserverHandle,
    ) -> Result<(), PreparedObserverHandle> {
        let mut current = self.observer.lock().expect("terminal observer lock");
        if self.closing.load(std::sync::atomic::Ordering::Acquire)
            || self.is_invalidated()
            || current.is_some()
        {
            return Err(observer);
        }
        *current = Some(observer);
        Ok(())
    }

    fn observer(&self) -> Option<PreparedObserverHandle> {
        self.observer
            .lock()
            .expect("terminal observer lock")
            .clone()
    }

    async fn cancel_observer(&self, reason: TerminalObserverCancellationReason) {
        let observer = self.observer.lock().expect("terminal observer lock").take();
        if let Some(observer) = observer {
            observer.cancel(reason).await;
        } else {
            self.observation.request_cancellation(reason);
            self.observation
                .shutdown_workers(
                    OBSERVER_WORKER_SHUTDOWN_TIMEOUT,
                    OBSERVER_WORKER_ABORT_TIMEOUT,
                )
                .await;
        }
    }

    async fn stop_output(&self) {
        self.output_stop.cancel();
        if self
            .output_started
            .load(std::sync::atomic::Ordering::Acquire)
            && tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, self.output_completed.cancelled())
                .await
                .is_err()
        {
            tracing::warn!(
                timeout_ms = OUTPUT_DRAIN_TIMEOUT.as_millis(),
                "terminal output drain timed out"
            );
        }
    }

    async fn cancel_and_invalidate(&self, reason: TerminalObserverCancellationReason) {
        self.begin_closing();
        self.cancel_observer(reason).await;
        self.invalidate().await;
    }

    fn begin_closing(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn prevent_new_work(&self) {
        self.invalidated
            .store(true, std::sync::atomic::Ordering::Release);
        self.cancellation.cancel();
    }
}

#[derive(Debug)]
struct SessionGenerationRegistry {
    observer_runtime: Option<tokio::runtime::Handle>,
    current: std::sync::Mutex<HashMap<SessionKey, std::sync::Weak<SessionGeneration>>>,
}

impl SessionGenerationRegistry {
    fn new(observer_runtime: Option<tokio::runtime::Handle>) -> Self {
        Self {
            observer_runtime,
            current: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn current(&self, key: &SessionKey) -> Arc<SessionGeneration> {
        let mut current = self.current.lock().expect("terminal generations lock");
        current.retain(|_, generation| generation.strong_count() > 0);
        if let Some(generation) = current.get(key).and_then(std::sync::Weak::upgrade)
            && !generation.is_invalidated()
        {
            return generation;
        }
        let generation = Arc::new(SessionGeneration::new(key, self.observer_runtime.clone()));
        current.insert(key.clone(), Arc::downgrade(&generation));
        generation
    }

    fn peek(&self, key: &SessionKey) -> Option<Arc<SessionGeneration>> {
        self.current
            .lock()
            .expect("terminal generations lock")
            .get(key)
            .and_then(std::sync::Weak::upgrade)
    }

    fn replace(
        &self,
        key: &SessionKey,
    ) -> (Option<Arc<SessionGeneration>>, Arc<SessionGeneration>) {
        let mut current = self.current.lock().expect("terminal generations lock");
        current.retain(|_, generation| generation.strong_count() > 0);
        let displaced = current.remove(key).and_then(|value| value.upgrade());
        let generation = Arc::new(SessionGeneration::new(key, self.observer_runtime.clone()));
        current.insert(key.clone(), Arc::downgrade(&generation));
        (displaced, generation)
    }

    fn remove_matching(
        &self,
        thread_id: &str,
        terminal_id: Option<&str>,
    ) -> Vec<Arc<SessionGeneration>> {
        let mut current = self.current.lock().expect("terminal generations lock");
        current.retain(|_, generation| generation.strong_count() > 0);
        let keys = current
            .keys()
            .filter(|(candidate_thread, candidate_terminal)| {
                candidate_thread == thread_id
                    && terminal_id.is_none_or(|value| candidate_terminal == value)
            })
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| current.remove(&key).and_then(|value| value.upgrade()))
            .collect()
    }

    fn remove_all(&self) -> Vec<Arc<SessionGeneration>> {
        let mut current = self.current.lock().expect("terminal generations lock");
        current
            .drain()
            .filter_map(|(_, generation)| generation.upgrade())
            .collect()
    }

    fn snapshot_live(&self) -> Vec<Arc<SessionGeneration>> {
        let mut current = self.current.lock().expect("terminal generations lock");
        current.retain(|_, generation| generation.strong_count() > 0);
        current
            .values()
            .filter_map(std::sync::Weak::upgrade)
            .filter(|generation| !generation.is_invalidated())
            .collect()
    }

    fn live_observer_count(&self) -> usize {
        let mut current = self.current.lock().expect("terminal generations lock");
        current.retain(|_, generation| generation.strong_count() > 0);
        current
            .values()
            .filter_map(std::sync::Weak::upgrade)
            .filter(|generation| !generation.is_invalidated() && generation.observer().is_some())
            .count()
    }
}

#[derive(Debug, Default)]
struct SessionOperationRegistry {
    operations: std::sync::Mutex<HashMap<SessionKey, std::sync::Weak<Mutex<()>>>>,
}

#[derive(Debug, Default)]
struct WorktreeRemovalRegistry {
    threads: std::sync::Mutex<HashSet<String>>,
    paths: std::sync::Mutex<HashSet<String>>,
}

impl WorktreeRemovalRegistry {
    fn acquire(
        self: &Arc<Self>,
        thread_ids: &[String],
        worktree_path: &Path,
    ) -> Result<WorktreeRemovalGuard, TerminalError> {
        let mut unique = thread_ids
            .iter()
            .filter(|thread_id| !thread_id.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        unique.sort();
        unique.dedup();
        let path_keys = terminal_path_keys(worktree_path);
        let mut threads = self.threads.lock().expect("worktree removal registry lock");
        let mut paths = self
            .paths
            .lock()
            .expect("worktree removal path registry lock");
        if let Some(thread_id) = unique
            .iter()
            .find(|thread_id| threads.contains(thread_id.as_str()))
        {
            return Err(TerminalError::WorktreeRemovalInProgress {
                thread_id: thread_id.clone(),
            });
        }
        if path_keys.iter().any(|candidate| {
            paths.iter().any(|root| {
                terminal_path_is_within(candidate, root) || terminal_path_is_within(root, candidate)
            })
        }) {
            return Err(TerminalError::WorktreeRemovalInProgress {
                thread_id: unique
                    .first()
                    .cloned()
                    .unwrap_or_else(|| display_terminal_path(worktree_path)),
            });
        }
        threads.extend(unique.iter().cloned());
        paths.extend(path_keys.iter().cloned());
        drop(paths);
        drop(threads);
        Ok(WorktreeRemovalGuard {
            registry: self.clone(),
            thread_ids: unique,
            path_keys,
        })
    }

    fn ensure_available(
        &self,
        thread_id: &str,
        cwd: Option<&Path>,
        worktree_path: Option<&Path>,
    ) -> Result<(), TerminalError> {
        let thread_blocked = self
            .threads
            .lock()
            .expect("worktree removal registry lock")
            .contains(thread_id);
        let path_blocked = {
            let paths = self
                .paths
                .lock()
                .expect("worktree removal path registry lock");
            [cwd, worktree_path]
                .into_iter()
                .flatten()
                .flat_map(terminal_path_keys)
                .any(|candidate| {
                    paths
                        .iter()
                        .any(|root| terminal_path_is_within(&candidate, root))
                })
        };
        if thread_blocked || path_blocked {
            return Err(TerminalError::WorktreeRemovalInProgress {
                thread_id: thread_id.to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct WorktreeRemovalGuard {
    registry: Arc<WorktreeRemovalRegistry>,
    thread_ids: Vec<String>,
    path_keys: Vec<String>,
}

impl Drop for WorktreeRemovalGuard {
    fn drop(&mut self) {
        let mut threads = self
            .registry
            .threads
            .lock()
            .expect("worktree removal registry lock");
        for thread_id in &self.thread_ids {
            threads.remove(thread_id);
        }
        drop(threads);
        let mut paths = self
            .registry
            .paths
            .lock()
            .expect("worktree removal path registry lock");
        for path_key in &self.path_keys {
            paths.remove(path_key);
        }
    }
}

fn display_terminal_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn terminal_path_keys(path: &Path) -> Vec<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let lexical = normalize_terminal_path_lexically(&absolute);
    let mut keys = vec![terminal_path_key_from_absolute(&lexical)];
    if let Ok(canonical) = std::fs::canonicalize(&lexical) {
        keys.push(terminal_path_key_from_absolute(&canonical));
    }
    keys.sort();
    keys.dedup();
    keys
}

fn normalize_terminal_path_lexically(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn terminal_path_key_from_absolute(path: &Path) -> String {
    let mut key = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        if let Some(value) = key.strip_prefix("//?/UNC/") {
            key = format!("//{value}");
        } else if let Some(value) = key.strip_prefix("//?/") {
            key = value.to_owned();
        }
    }
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    #[cfg(windows)]
    key.make_ascii_lowercase();
    key
}

fn terminal_path_is_within(candidate: &str, root: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

impl SessionOperationRegistry {
    fn for_key(&self, key: &SessionKey) -> Arc<Mutex<()>> {
        let mut operations = self
            .operations
            .lock()
            .expect("terminal operation registry lock");
        operations.retain(|_, operation| operation.strong_count() > 0);
        if let Some(operation) = operations.get(key).and_then(std::sync::Weak::upgrade) {
            return operation;
        }
        let operation = Arc::new(Mutex::new(()));
        operations.insert(key.clone(), Arc::downgrade(&operation));
        operation
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
#[derive(Debug)]
struct PublisherBarrier {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Session {
    fn size(&self) -> TerminalSize {
        TerminalSize {
            cols: self.cols,
            rows: self.rows,
            size_claim: self.size_owner.as_ref().map(|owner| owner.claim.clone()),
        }
    }

    fn resized(&mut self) -> TerminalEvent {
        let sequence = self.advance();
        TerminalEvent::Resized {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            sequence,
            size: self.size(),
        }
    }

    fn snapshot(&self) -> TerminalSessionSnapshot {
        TerminalSessionSnapshot {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.clone(),
            worktree_path: self.worktree_path.clone(),
            status: self.status,
            pid: self.pid,
            history: self.history.snapshot(),
            exit_code: self.exit_code,
            exit_signal: self.exit_signal,
            console_theme: self.console_theme,
            label: self.display_label(),
            updated_at: self.updated_at.clone(),
            sequence: self.sequence,
            size: Some(self.size()),
            osc_color_responder_active: self.osc_color_responder_active,
            // Only attach can issue the first-attachment grant.
            first_attachment_grant: false,
        }
    }

    fn summary(&self) -> TerminalSummary {
        TerminalSummary {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            cwd: self.cwd.clone(),
            worktree_path: self.worktree_path.clone(),
            status: self.status,
            pid: self.pid,
            exit_code: self.exit_code,
            exit_signal: self.exit_signal,
            console_theme: self.console_theme,
            has_running_subprocess: self.has_running_subprocess,
            label: self.display_label(),
            updated_at: self.updated_at.clone(),
            center_panel: self.center_panel,
            command: self.command.clone(),
        }
    }

    fn carry_over(&self) -> SessionCarryOver {
        SessionCarryOver {
            center_panel: self.center_panel,
        }
    }

    fn display_label(&self) -> String {
        if self.has_running_subprocess
            && let Some(label) = self.child_command_label.as_deref()
        {
            let trimmed = label.trim();
            if !trimmed.is_empty() {
                return truncate_terminal_label(trimmed);
            }
        }
        truncate_terminal_label(&self.label)
    }

    fn advance(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.updated_at = now_iso();
        self.sequence
    }

    fn flush_private_output(&mut self) -> Option<TerminalEvent> {
        let data = self.private_output.finish();
        self.record_private_output(data)
    }

    fn redact_private_output_gap(&mut self) -> Option<TerminalEvent> {
        let data = self.private_output.note_gap();
        self.record_private_output(data)
    }

    fn record_private_output(&mut self, data: String) -> Option<TerminalEvent> {
        if data.is_empty() {
            return None;
        }
        self.history.push(&data);
        let sequence = self.advance();
        Some(TerminalEvent::Output {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            sequence,
            data,
        })
    }
}

struct Inner {
    backend: Arc<dyn PtyBackend>,
    attribution: ProcessAttributionRegistry,
    options: TerminalManagerOptions,
    inspector: Arc<dyn TerminalSubprocessInspector>,
    callback_isolation: ObserverCallbackIsolation,
    lifecycle: Mutex<()>,
    operations: SessionOperationRegistry,
    worktree_removals: Arc<WorktreeRemovalRegistry>,
    generations: SessionGenerationRegistry,
    input: super::input::TerminalInputRegistry,
    size_claim_attachments: std::sync::Mutex<HashMap<SessionKey, HashMap<String, uuid::Uuid>>>,
    attachment_tasks: TaskTracker,
    sessions: RwLock<HashMap<SessionKey, SharedSession>>,
    events: broadcast::Sender<TerminalEvent>,
    metadata: broadcast::Sender<TerminalMetadataEvent>,
    process_exit_callback: std::sync::RwLock<Option<TerminalProcessExitCallback>>,
    cancellation: CancellationToken,
    #[cfg(test)]
    restart_after_exact_cleanup_barrier: std::sync::Mutex<Option<Arc<PublisherBarrier>>>,
}

#[derive(Clone)]
pub struct TerminalManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TerminalManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalManager")
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new(
            Arc::new(PortablePtyBackend),
            TerminalManagerOptions::default(),
        )
    }
}

impl TerminalManager {
    pub fn new(backend: Arc<dyn PtyBackend>, options: TerminalManagerOptions) -> Self {
        Self::with_process_attribution(backend, options, ProcessAttributionRegistry::new())
    }

    pub fn with_process_attribution(
        backend: Arc<dyn PtyBackend>,
        options: TerminalManagerOptions,
        attribution: ProcessAttributionRegistry,
    ) -> Self {
        let (events, _) = broadcast::channel(options.event_capacity.max(16));
        let (metadata, _) = broadcast::channel(options.event_capacity.max(16));
        let inspector = options
            .subprocess_inspector
            .clone()
            .unwrap_or_else(|| Arc::new(NativeTerminalSubprocessInspector::default()));
        Self {
            inner: Arc::new(Inner {
                backend,
                attribution,
                options,
                inspector,
                callback_isolation: ObserverCallbackIsolation::default(),
                lifecycle: Mutex::new(()),
                operations: SessionOperationRegistry::default(),
                worktree_removals: Arc::new(WorktreeRemovalRegistry::default()),
                generations: SessionGenerationRegistry::new(
                    tokio::runtime::Handle::try_current().ok(),
                ),
                input: super::input::TerminalInputRegistry::default(),
                size_claim_attachments: std::sync::Mutex::new(HashMap::new()),
                attachment_tasks: TaskTracker::new(),
                sessions: RwLock::new(HashMap::new()),
                events,
                metadata,
                process_exit_callback: std::sync::RwLock::new(None),
                cancellation: CancellationToken::new(),
                #[cfg(test)]
                restart_after_exact_cleanup_barrier: std::sync::Mutex::new(None),
            }),
        }
    }

    pub(crate) fn process_attribution_registry(&self) -> ProcessAttributionRegistry {
        self.inner.attribution.clone()
    }

    pub(crate) fn set_process_exit_callback(&self, callback: TerminalProcessExitCallback) {
        *self
            .inner
            .process_exit_callback
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(callback);
    }

    pub async fn set_agent_activity_enabled(
        &self,
        enabled: bool,
    ) -> TerminalAgentActivityTransition {
        let observers = {
            let _lifecycle = self.inner.lifecycle.lock().await;
            self.inner
                .generations
                .snapshot_live()
                .into_iter()
                .filter_map(|generation| generation.observer())
                .collect::<Vec<_>>()
        };
        let mut transition = TerminalAgentActivityTransition::default();
        for observer in observers {
            transition.merge(observer.set_agent_activity_enabled(enabled).await);
        }
        transition
    }

    /// Returns live prepared observers retained to resume activity after re-enable.
    #[doc(hidden)]
    #[must_use]
    pub fn agent_activity_restart_descriptor_count_for_integration_test(&self) -> usize {
        self.inner.generations.live_observer_count()
    }

    pub async fn open(
        &self,
        input: TerminalOpenInput,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        self.open_inner(input, None, CancellationToken::new()).await
    }

    pub(crate) async fn open_with_publication_cancellation(
        &self,
        input: TerminalOpenInput,
        publication_cancellation: CancellationToken,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        self.open_inner(input, None, publication_cancellation).await
    }

    pub(crate) async fn open_with_initial_input_and_publication_cancellation(
        &self,
        input: TerminalOpenInput,
        initial_input: String,
        publication_cancellation: CancellationToken,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        self.open_inner(input, Some(initial_input), publication_cancellation)
            .await
    }

    async fn open_inner(
        &self,
        input: TerminalOpenInput,
        initial_input: Option<String>,
        publication_cancellation: CancellationToken,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        let key = (input.thread_id.clone(), input.terminal_id.clone());
        let generation = {
            let _lifecycle = self.inner.lifecycle.lock().await;
            self.inner.worktree_removals.ensure_available(
                &input.thread_id,
                Some(&input.cwd),
                input.worktree_path.as_deref(),
            )?;
            if self.inner.cancellation.is_cancelled() {
                return Err(TerminalError::Shutdown);
            }
            self.inner.generations.current(&key)
        };
        self.start(input, generation, initial_input, publication_cancellation)
            .await
    }

    pub async fn restart(
        &self,
        input: TerminalRestartInput,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        self.restart_with_publication_cancellation(input, CancellationToken::new())
            .await
    }

    pub(crate) async fn restart_with_publication_cancellation(
        &self,
        input: TerminalRestartInput,
        publication_cancellation: CancellationToken,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        let key = (input.thread_id.clone(), input.terminal_id.clone());
        if let Some(generation) = self.inner.generations.peek(&key) {
            generation.observation.request_cancellation_after(
                TerminalObserverCancellationReason::Restarted,
                || {
                    generation.begin_closing();
                    generation.prevent_new_work();
                },
            );
        }
        let operation = self.inner.operations.for_key(&key);
        let operation = Arc::new(operation.lock_owned().await);
        let (displaced, generation, _startup) = {
            let _lifecycle = self.inner.lifecycle.lock().await;
            self.inner.worktree_removals.ensure_available(
                &input.thread_id,
                Some(&input.cwd),
                input.worktree_path.as_deref(),
            )?;
            if self.inner.cancellation.is_cancelled() {
                return Err(TerminalError::Shutdown);
            }
            let (displaced, generation) = self.inner.generations.replace(&key);
            if let Some(previous) = &displaced {
                previous.begin_closing();
            }
            let startup = generation
                .startup
                .clone()
                .try_lock_owned()
                .expect("fresh terminal generation startup lock");
            (displaced, generation, startup)
        };
        if let Some(previous) = displaced {
            previous.stop_output().await;
            previous
                .cancel_and_invalidate(TerminalObserverCancellationReason::Restarted)
                .await;
        }
        #[cfg(test)]
        let restart_barrier = {
            self.inner
                .restart_after_exact_cleanup_barrier
                .lock()
                .expect("restart barrier lock")
                .take()
        };
        #[cfg(test)]
        if let Some(barrier) = restart_barrier {
            barrier.started.notify_one();
            barrier.release.notified().await;
        }
        let previous = self.inner.sessions.read().await.get(&key).cloned();
        let carry_over = match previous {
            Some(previous) => previous.lock().await.carry_over(),
            None => SessionCarryOver::default(),
        };
        let closed = self
            .close_sessions(
                &input.thread_id,
                Some(&input.terminal_id),
                TerminalObserverCancellationReason::Restarted,
            )
            .await;
        log_terminal_cleanup("restart", &closed.report);
        match self
            .start_inner(
                input,
                Some(carry_over),
                generation,
                None,
                publication_cancellation,
                operation.clone(),
            )
            .await
        {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                self.publish_closed_sessions(
                    &closed.notifications,
                    TerminalRemovalReason::Restarted,
                );
                Err(error)
            }
        }
    }

    async fn start(
        &self,
        input: TerminalOpenInput,
        generation: Arc<SessionGeneration>,
        initial_input: Option<String>,
        publication_cancellation: CancellationToken,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        let key = (input.thread_id.clone(), input.terminal_id.clone());
        let operation = self.inner.operations.for_key(&key);
        let operation = Arc::new(operation.lock_owned().await);
        let startup = generation.startup.clone();
        let _startup = startup.lock().await;
        self.start_inner(
            input,
            None,
            generation,
            initial_input,
            publication_cancellation,
            operation,
        )
        .await
    }

    /// `restarted` carries what the replaced session keeps; `None` for open.
    async fn start_inner(
        &self,
        input: TerminalOpenInput,
        restarted: Option<SessionCarryOver>,
        generation: Arc<SessionGeneration>,
        initial_input: Option<String>,
        publication_cancellation: CancellationToken,
        operation: Arc<tokio::sync::OwnedMutexGuard<()>>,
    ) -> Result<TerminalSessionSnapshot, TerminalError> {
        self.inner.worktree_removals.ensure_available(
            &input.thread_id,
            Some(&input.cwd),
            input.worktree_path.as_deref(),
        )?;
        if generation.is_invalidated() {
            return Err(invalidated_creation_error(&input));
        }
        validate_cwd(&input.cwd).await?;
        validate_dimensions(input.cols, input.rows)?;
        if generation.is_invalidated() {
            return Err(invalidated_creation_error(&input));
        }
        let key = (input.thread_id.clone(), input.terminal_id.clone());
        let console_theme = terminal_console_theme_from_env(&input.env);
        let is_restart = restarted.is_some();
        let mut carry_over = restarted.unwrap_or_default();
        let existing = self.inner.sessions.read().await.get(&key).cloned();
        if let Some(existing) = existing {
            let session = existing.lock().await;
            // Open dimensions initialize a new PTY. Existing sessions change
            // size only through resize, which publishes the applied size/claim.
            if session.process.is_some() {
                let snapshot = session.snapshot();
                drop(session);
                if input.center_panel {
                    self.mark_center_panel(&key, &existing).await;
                }
                return Ok(snapshot);
            }
            // Reopening an exited session keeps its center-panel marker.
            carry_over = session.carry_over();
        }
        let mut input = input;
        if let Some(session_env) = self.inner.options.session_env.as_ref() {
            session_env(&input.thread_id, &mut input.env).await;
        }

        // The provider's native form of the command applies to observed and unobserved
        // launches alike, so it replaces the requested one first. Only the spawn uses it: the
        // session keeps the requested command, so a relaunch derives the native form again.
        let mut native_args = None;
        if let (Some(preparer), Some(command)) = (
            self.inner.options.launch_preparer.as_ref(),
            input.command.as_ref(),
        ) && let Some(activity) = command.activity.clone()
        {
            let request = TerminalLaunchPreparationInput {
                executable: command.executable.clone(),
                args: command.args.clone(),
                cwd: input.cwd.clone(),
                worktree_path: input.worktree_path.clone(),
                launch_env: input.env.clone(),
                activity,
                generation: generation.observation.clone(),
            };
            let preparer = preparer.clone();
            if let Some(Some(native)) = run_observer_callback(
                self.inner.callback_isolation.clone(),
                "native_command",
                NATIVE_COMMAND_TIMEOUT,
                async move { preparer.native_command(&request).await },
            )
            .await
            {
                native_args = Some(native.args);
                input.env.extend(native.env);
            }
        }

        let mut private_values = Vec::new();
        let original_command_candidate = input.command.as_ref().map(|command| {
            (
                command.executable.clone(),
                native_args.clone().unwrap_or_else(|| command.args.clone()),
                input.env.clone(),
            )
        });
        let mut command_candidate = original_command_candidate.clone();
        let mut command_was_prepared = false;
        let mut activity_admission = None;
        if let (Some(preparer), Some(command)) = (
            self.inner.options.launch_preparer.as_ref(),
            input.command.as_ref(),
        ) && let Some(activity) = command.activity.clone()
        {
            let preparation = TerminalLaunchPreparationInput {
                executable: command.executable.clone(),
                args: native_args.unwrap_or_else(|| command.args.clone()),
                cwd: input.cwd.clone(),
                worktree_path: input.worktree_path.clone(),
                launch_env: input.env.clone(),
                activity,
                generation: generation.observation.clone(),
            };
            let preparer = preparer.clone();
            let budget_preparer = preparer.clone();
            let budget_input = preparation.clone();
            let execution_budget = run_observer_callback(
                self.inner.callback_isolation.clone(),
                "preparation_execution_budget",
                OBSERVER_CALLBACK_TIMEOUT,
                async move {
                    budget_preparer
                        .preparation_execution_budget(&budget_input)
                        .await
                },
            )
            .await
            .unwrap_or(OBSERVER_CALLBACK_TIMEOUT)
            .min(MAX_PREPARATION_EXECUTION_BUDGET);
            let prepared = match run_observer_callback(
                self.inner.callback_isolation.clone(),
                "prepare",
                execution_budget,
                async move { preparer.prepare(preparation).await },
            )
            .await
            {
                None | Some(TerminalLaunchPreparation::PassThrough) => None,
                Some(TerminalLaunchPreparation::Prepared(prepared)) => Some((prepared, None)),
                Some(TerminalLaunchPreparation::Admitted(prepared, admission)) => {
                    if admission.is_current() {
                        Some((prepared, Some(admission)))
                    } else {
                        drop(prepared);
                        drop(admission);
                        None
                    }
                }
            };
            if let Some((prepared, admission)) = prepared {
                activity_admission = admission;
                let observer = PreparedObserverHandle::new(
                    prepared.observer,
                    generation.observation.clone(),
                    self.inner.callback_isolation.clone(),
                );
                if generation.is_invalidated() {
                    observer
                        .cancel(TerminalObserverCancellationReason::GenerationInvalidated)
                        .await;
                    return Err(invalidated_creation_error(&input));
                }
                if let Some(reserved_key) = prepared.private_env.keys().find(|private_key| {
                    input.env.keys().any(|client_key| {
                        environment_keys_equal(Platform::current(), client_key, private_key)
                    })
                }) {
                    observer
                        .cancel(TerminalObserverCancellationReason::PreparationRejected)
                        .await;
                    return Err(TerminalError::Io(format!(
                        "terminal environment contains a reserved observer key: {reserved_key}"
                    )));
                }
                private_values.extend(
                    prepared
                        .private_env
                        .values()
                        .filter(|value| !value.is_empty())
                        .cloned(),
                );
                private_values.sort_by(|left, right| {
                    right.len().cmp(&left.len()).then_with(|| left.cmp(right))
                });
                private_values.dedup();
                let mut env = input.env.clone();
                env.extend(prepared.private_env);
                command_candidate = Some((prepared.executable, prepared.args, env));
                if let Err(observer) = generation.install_observer(observer) {
                    observer
                        .cancel(TerminalObserverCancellationReason::GenerationInvalidated)
                        .await;
                    return Err(invalidated_creation_error(&input));
                }
                command_was_prepared = true;
            } else {
                generation
                    .observation
                    .request_cancellation(TerminalObserverCancellationReason::PreparationRejected);
                generation
                    .observation
                    .shutdown_workers(
                        OBSERVER_WORKER_SHUTDOWN_TIMEOUT,
                        OBSERVER_WORKER_ABORT_TIMEOUT,
                    )
                    .await;
            }
        }

        let spawn_candidates = if let Some((executable, args, env)) = command_candidate {
            let attempted_label =
                redact_private_values(format!("{executable} {args:?}"), &private_values);
            vec![(
                PtySpawnInput {
                    executable,
                    args,
                    cwd: input.cwd.clone(),
                    cols: input.cols,
                    rows: input.rows,
                    env,
                },
                attempted_label,
            )]
        } else {
            resolve_shell_candidates(
                Platform::current(),
                self.inner.options.preferred_shell.as_deref(),
                &input.env,
            )
            .into_iter()
            .map(|candidate| {
                let attempted = format_shell_candidate(&candidate);
                (
                    PtySpawnInput {
                        executable: candidate.command,
                        args: candidate.args,
                        cwd: input.cwd.clone(),
                        cols: input.cols,
                        rows: input.rows,
                        env: input.env.clone(),
                    },
                    attempted,
                )
            })
            .collect::<Vec<_>>()
        };

        let mut attempted = Vec::new();
        let mut last_error = "no terminal launch candidates were available".to_owned();
        let mut spawned = None;
        for (spawn, attempted_label) in spawn_candidates {
            attempted.push(attempted_label);
            let executable = spawn.executable.clone();
            match UncommittedPtyProcess::spawn(self.inner.backend.clone(), spawn, operation.clone())
                .await
            {
                Ok(process) => {
                    spawned = Some((process, executable));
                    break;
                }
                Err(error) => last_error = redact_private_values(error, &private_values),
            }
        }
        let Some((mut uncommitted_process, mut executable)) = spawned else {
            generation
                .cancel_observer(TerminalObserverCancellationReason::SpawnFailed)
                .await;
            return Err(TerminalError::Spawn {
                attempted,
                message: last_error,
            });
        };
        if publication_cancellation.is_cancelled() {
            generation
                .cancel_observer(TerminalObserverCancellationReason::GenerationInvalidated)
                .await;
            return Err(TerminalError::PublicationCancelled);
        }
        if generation.is_invalidated() || generation.is_closing() {
            generation
                .cancel_observer(TerminalObserverCancellationReason::GenerationInvalidated)
                .await;
            return Err(invalidated_creation_error(&input));
        }
        if command_was_prepared
            && let Some(observer) = generation.observer()
            && !observer.is_ready_for_on_spawned().await
        {
            if generation.is_invalidated() || generation.is_closing() {
                generation
                    .cancel_observer(TerminalObserverCancellationReason::GenerationInvalidated)
                    .await;
                return Err(invalidated_creation_error(&input));
            }
            drop(uncommitted_process);
            generation
                .cancel_observer(TerminalObserverCancellationReason::PreparationRejected)
                .await;
            // Observer cleanup can await while close or restart fences this generation.
            if generation.is_invalidated() || generation.is_closing() {
                return Err(invalidated_creation_error(&input));
            }
            private_values.clear();
            let Some((original, args, env)) = original_command_candidate else {
                return Err(TerminalError::Io(
                    "prepared terminal launch has no original command".to_owned(),
                ));
            };
            let attempted_label = format!("{original} {args:?}");
            attempted.push(attempted_label);
            executable = original.clone();
            uncommitted_process = UncommittedPtyProcess::spawn(
                self.inner.backend.clone(),
                PtySpawnInput {
                    executable: original,
                    args,
                    cwd: input.cwd.clone(),
                    cols: input.cols,
                    rows: input.rows,
                    env,
                },
                operation.clone(),
            )
            .await
            .map_err(|error| TerminalError::Spawn {
                attempted: attempted.clone(),
                message: error,
            })?;
        }
        let process = uncommitted_process.process();
        if let Some(observer) = generation.observer() {
            let completed = observer
                .on_spawned(process.pid(), generation.observation.observation())
                .await;
            if !completed {
                let reason = if generation.is_invalidated() || generation.is_closing() {
                    TerminalObserverCancellationReason::GenerationInvalidated
                } else {
                    TerminalObserverCancellationReason::PreparationRejected
                };
                generation.cancel_observer(reason).await;
                generation.observation.invalidate().await;
            }
        }
        if generation.is_invalidated() || generation.is_closing() {
            generation
                .cancel_observer(TerminalObserverCancellationReason::GenerationInvalidated)
                .await;
            return Err(invalidated_creation_error(&input));
        }
        if let Some(initial_input) = initial_input {
            process.write(&initial_input).map_err(TerminalError::Io)?;
        }
        let label = input
            .command
            .as_ref()
            .and_then(|command| command.label.clone())
            .unwrap_or_else(|| terminal_label(&input.terminal_id));
        let exit = process.subscribe_exit();
        let process_has_exited = exit.borrow().is_some();
        let attribution_registration = if process_has_exited {
            None
        } else if let Some(identity) = process.process_identity() {
            match self.inner.attribution.register_identity(
                identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Terminal,
                    label: label.clone(),
                    source: RegistrationSource::Terminal,
                    driver: None,
                },
            ) {
                Ok(registration) => Some(registration),
                Err(ProcessRegistrationError::Shutdown) => {
                    uncommitted_process.cleanup().await;
                    return Err(TerminalError::Shutdown);
                }
                Err(error @ ProcessRegistrationError::Capacity) => {
                    uncommitted_process.cleanup().await;
                    return Err(TerminalError::Spawn {
                        attempted,
                        message: error.to_string(),
                    });
                }
            }
        } else {
            // The portable backend captures identity at spawn and reaps an already
            // exited root. Its waiter publishes the exit only after cleaning the
            // process group (Unix) or Job (Windows). A missing identity followed by
            // that exit is an ordinary fast exit already cleaned by its owner, so
            // the manager registers nothing and kills nothing.
            let mut exit_observer = exit.clone();
            let exit_reported = matches!(
                tokio::time::timeout(
                    TERMINAL_CLOSE_WAIT_TIMEOUT,
                    exit_observer.wait_for(Option::is_some),
                )
                .await,
                Ok(Ok(_))
            );
            if !exit_reported {
                uncommitted_process.cleanup().await;
                return Err(TerminalError::Spawn {
                    attempted,
                    message: "spawned terminal process has no stable process identity".to_owned(),
                });
            }
            None
        };
        let history = TerminalHistory::new(self.inner.options.history_line_limit);
        debug_assert_eq!(
            history.line_limit(),
            self.inner.options.history_line_limit,
            "session history must retain the manager's configured line limit"
        );
        let session = Arc::new(Mutex::new(Session {
            generation: generation.clone(),
            thread_id: input.thread_id.clone(),
            terminal_id: input.terminal_id.clone(),
            cwd: input.cwd.to_string_lossy().into_owned(),
            worktree_path: input
                .worktree_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            status: TerminalStatus::Running,
            pid: Some(process.pid()),
            history,
            exit_code: None,
            exit_signal: None,
            console_theme,
            label,
            has_running_subprocess: false,
            child_command_label: None,
            updated_at: now_iso(),
            sequence: 1,
            cols: input.cols,
            rows: input.rows,
            size_owner: None,
            first_attachment_grant_used: false,
            osc_color_responder_active: process.osc_color_responder_active(),
            process: Some(process.clone()),
            attribution_registration,
            observer: generation.observer(),
            private_output: StreamingSecretRedactor::new(private_values),
            center_panel: input.center_panel || carry_over.center_panel,
            command: input.command.clone(),
            executable,
            started: std::time::Instant::now(),
        }));
        let _lifecycle = self.inner.lifecycle.lock().await;
        let _publication = generation.publication.lock().await;
        if publication_cancellation.is_cancelled() {
            generation
                .cancel_observer(TerminalObserverCancellationReason::GenerationInvalidated)
                .await;
            return Err(TerminalError::PublicationCancelled);
        }
        self.inner.worktree_removals.ensure_available(
            &input.thread_id,
            Some(&input.cwd),
            input.worktree_path.as_deref(),
        )?;
        if self.inner.cancellation.is_cancelled() {
            generation
                .cancel_observer(TerminalObserverCancellationReason::Shutdown)
                .await;
            return Err(TerminalError::Shutdown);
        }
        if generation.is_invalidated() {
            generation
                .cancel_observer(TerminalObserverCancellationReason::GenerationInvalidated)
                .await;
            return Err(invalidated_creation_error(&input));
        }
        let existing = self.inner.sessions.read().await.get(&key).cloned();
        if let Some(existing) = existing
            && existing.lock().await.process.is_some()
        {
            generation
                .cancel_observer(TerminalObserverCancellationReason::PreparationRejected)
                .await;
            let snapshot = existing.lock().await.snapshot();
            drop(_publication);
            drop(_lifecycle);
            if input.center_panel {
                self.mark_center_panel(&key, &existing).await;
            }
            return Ok(snapshot);
        }
        // Only Restarted resets surviving attachments' sequence cursors.
        // A non-restart reopen leaves the first-attachment grant available.
        let first_attachment_grant_used = is_restart
            && self
                .inner
                .size_claim_attachments
                .lock()
                .expect("terminal attachment claims")
                .get(&key)
                .is_some_and(|claims| !claims.is_empty());
        session.lock().await.first_attachment_grant_used = first_attachment_grant_used;
        self.inner
            .sessions
            .write()
            .await
            .insert(key, session.clone());
        self.supervise(session.clone(), process, exit, generation.clone());
        uncommitted_process.commit();
        // The installed observer is now visible to the lifecycle snapshot used by disable.
        drop(activity_admission);
        let snapshot = session.lock().await.snapshot();
        let event = if is_restart {
            TerminalEvent::Restarted {
                thread_id: input.thread_id,
                terminal_id: input.terminal_id,
                sequence: snapshot.sequence,
                snapshot: snapshot.clone(),
            }
        } else {
            TerminalEvent::Started {
                thread_id: input.thread_id,
                terminal_id: input.terminal_id,
                sequence: snapshot.sequence,
                snapshot: snapshot.clone(),
            }
        };
        let _ = self.inner.events.send(event);
        let _ = self.inner.metadata.send(TerminalMetadataEvent::Upsert {
            terminal: Box::new(session.lock().await.summary()),
        });
        Ok(snapshot)
    }

    /// Sets the sticky center-panel marker on a registered session and
    /// publishes the changed summary. Never clears it.
    async fn mark_center_panel(&self, key: &SessionKey, session: &SharedSession) {
        let generation = session.lock().await.generation.clone();
        let _publication = generation.publication.lock().await;
        let registered = self
            .inner
            .sessions
            .read()
            .await
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, session));
        let mut session = session.lock().await;
        if registered && !session.center_panel {
            session.center_panel = true;
            let _ = self.inner.metadata.send(TerminalMetadataEvent::Upsert {
                terminal: Box::new(session.summary()),
            });
        }
    }

    fn supervise(
        &self,
        session: Arc<Mutex<Session>>,
        process: Arc<dyn PtyProcess>,
        mut exit: tokio::sync::watch::Receiver<Option<PtyExit>>,
        generation: Arc<SessionGeneration>,
    ) {
        let inner = self.inner.clone();
        let mut output = process.subscribe_output();
        let output_session = session.clone();
        let output_cancel = inner.cancellation.child_token();
        let output_inner = inner.clone();
        let output_generation = generation.clone();
        output_generation
            .output_started
            .store(true, std::sync::atomic::Ordering::Release);
        tokio::spawn(async move {
            let _completion = CancelOnDrop(output_generation.output_completed.clone());
            loop {
                let data = tokio::select! {
                    biased;
                    () = output_generation.output_stop.cancelled() => break,
                    () = output_cancel.cancelled() => break,
                    () = output_generation.cancellation.cancelled() => break,
                    result = output.recv() => match result {
                        Ok(data) => data,
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            if !Self::publish_output_gap(
                                &output_inner,
                                &output_session,
                                &output_generation,
                            )
                            .await
                            {
                                return;
                            }
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                };
                // Reads already waiting are published together: one lock and one event per
                // batch instead of per read keeps the publisher ahead of a program that writes
                // in many small pieces.
                let mut data = data;
                let mut lagged = false;
                while data.len() < MAX_COALESCED_OUTPUT_BYTES {
                    match output.try_recv() {
                        Ok(more) => data.push_str(&more),
                        Err(broadcast::error::TryRecvError::Lagged(_)) => {
                            lagged = true;
                            break;
                        }
                        Err(_) => break,
                    }
                }
                #[cfg(test)]
                let barrier = output_generation
                    .output_barrier
                    .lock()
                    .expect("output barrier lock")
                    .clone();
                #[cfg(test)]
                if let Some(barrier) = barrier {
                    barrier.started.notify_one();
                    barrier.release.notified().await;
                }
                if !Self::publish_output_chunk(
                    &output_inner,
                    &output_session,
                    &output_generation,
                    data,
                )
                .await
                {
                    return;
                }
                if lagged
                    && !Self::publish_output_gap(&output_inner, &output_session, &output_generation)
                        .await
                {
                    return;
                }
            }
            for _ in 0..MAX_DRAINED_OUTPUT_CHUNKS {
                let data = match output.try_recv() {
                    Ok(data) => data,
                    Err(broadcast::error::TryRecvError::Lagged(_)) => {
                        if !Self::publish_output_gap(
                            &output_inner,
                            &output_session,
                            &output_generation,
                        )
                        .await
                        {
                            return;
                        }
                        continue;
                    }
                    Err(
                        broadcast::error::TryRecvError::Empty
                        | broadcast::error::TryRecvError::Closed,
                    ) => break,
                };
                if !Self::publish_output_chunk(
                    &output_inner,
                    &output_session,
                    &output_generation,
                    data,
                )
                .await
                {
                    return;
                }
            }
            Self::flush_output_tail(&output_inner, &output_session, &output_generation).await;
        });

        let exit_cancel = inner.cancellation.child_token();
        let exit_inner = inner.clone();
        let exit_session = session.clone();
        let exit_generation = generation.clone();
        tokio::spawn(async move {
            loop {
                let observed_exit = exit.borrow().clone();
                let Some(exit) = observed_exit else {
                    tokio::select! {
                        () = exit_cancel.cancelled() => return,
                        () = exit_generation.cancellation.cancelled() => return,
                        result = exit.changed() => {
                            if result.is_err() {
                                return;
                            }
                        }
                    }
                    continue;
                };
                Self::finalize_process_exit(&exit_inner, &exit_session, &exit_generation, exit)
                    .await;
                return;
            }
        });

        let activity_cancel = inner.cancellation.child_token();
        let activity_session = session;
        let activity_inner = inner.clone();
        let activity_pid = process.pid();
        let activity_generation = generation;
        tokio::spawn(async move {
            #[cfg(test)]
            let _completion = CancelOnDrop(activity_generation.activity_completed.clone());
            if activity_inner.options.subprocess_poll_interval.is_zero() {
                return;
            }
            loop {
                tokio::select! {
                    () = activity_cancel.cancelled() => return,
                    () = activity_generation.cancellation.cancelled() => return,
                    () = tokio::time::sleep(activity_inner.options.subprocess_poll_interval) => {}
                }

                let inspection = match activity_inner.inspector.inspect(activity_pid).await {
                    Ok(inspection) => inspection,
                    Err(error) => {
                        tracing::debug!(%error, pid = activity_pid, "failed to inspect terminal subprocess state");
                        continue;
                    }
                };

                let _publication = activity_generation.publication.lock().await;
                if activity_generation.is_invalidated() {
                    return;
                }
                let activity = {
                    let mut session = activity_session.lock().await;
                    if session.status != TerminalStatus::Running
                        || session.pid != Some(activity_pid)
                    {
                        return;
                    }
                    if session.has_running_subprocess == inspection.has_running_subprocess
                        && session.child_command_label == inspection.child_command_label
                    {
                        None
                    } else {
                        session.has_running_subprocess = inspection.has_running_subprocess;
                        session.child_command_label = inspection.child_command_label.clone();
                        let sequence = session.advance();
                        Some((
                            TerminalEvent::Activity {
                                thread_id: session.thread_id.clone(),
                                terminal_id: session.terminal_id.clone(),
                                sequence,
                                has_running_subprocess: session.has_running_subprocess,
                                label: session.display_label(),
                            },
                            session.summary(),
                        ))
                    }
                };

                if let Some((event, summary)) = activity {
                    let _ = activity_inner.events.send(event);
                    let _ = activity_inner.metadata.send(TerminalMetadataEvent::Upsert {
                        terminal: Box::new(summary),
                    });
                }
            }
        });
    }

    async fn finalize_process_exit(
        inner: &Arc<Inner>,
        session: &SharedSession,
        generation: &Arc<SessionGeneration>,
        exit: PtyExit,
    ) {
        generation.stop_output().await;
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return;
        }
        let registered = {
            let sessions = inner.sessions.read().await;
            let locked_session = session.lock().await;
            sessions
                .get(&(
                    locked_session.thread_id.clone(),
                    locked_session.terminal_id.clone(),
                ))
                .is_some_and(|current| Arc::ptr_eq(current, session))
        };
        if !registered {
            return;
        }
        generation
            .cancel_and_invalidate(TerminalObserverCancellationReason::ProcessExited)
            .await;
        let (event, summary, status_cwd) = {
            let mut session = session.lock().await;
            if exits::watching() {
                report_process_exit(&session);
            }
            session.status = TerminalStatus::Exited;
            session.attribution_registration.take();
            session.observer.take();
            session.pid = None;
            session.process = None;
            session.exit_code = exit.exit_code;
            session.exit_signal = exit.signal;
            session.has_running_subprocess = false;
            session.child_command_label = None;
            let sequence = session.advance();
            (
                TerminalEvent::Exited {
                    thread_id: session.thread_id.clone(),
                    terminal_id: session.terminal_id.clone(),
                    sequence,
                    exit_code: exit.exit_code,
                    exit_signal: exit.signal,
                },
                session.summary(),
                PathBuf::from(
                    session
                        .worktree_path
                        .as_deref()
                        .unwrap_or(session.cwd.as_str()),
                ),
            )
        };
        let _ = inner.events.send(event);
        let _ = inner.metadata.send(TerminalMetadataEvent::Upsert {
            terminal: Box::new(summary),
        });
        drop(_publication);
        let callback = inner
            .process_exit_callback
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(callback) = callback {
            callback(status_cwd).await;
        }
    }

    async fn publish_output_chunk(
        inner: &Inner,
        session: &SharedSession,
        generation: &SessionGeneration,
        data: String,
    ) -> bool {
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return false;
        }
        let event = {
            let mut session = session.lock().await;
            let data = session.private_output.push(&data);
            if data.is_empty() {
                None
            } else {
                session.history.push(&data);
                let sequence = session.advance();
                Some(TerminalEvent::Output {
                    thread_id: session.thread_id.clone(),
                    terminal_id: session.terminal_id.clone(),
                    sequence,
                    data,
                })
            }
        };
        if let Some(event) = event {
            let _ = inner.events.send(event);
        }
        true
    }

    async fn flush_output_tail(
        inner: &Inner,
        session: &SharedSession,
        generation: &SessionGeneration,
    ) {
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return;
        }
        let event = session.lock().await.flush_private_output();
        if let Some(event) = event {
            let _ = inner.events.send(event);
        }
    }

    async fn publish_output_gap(
        inner: &Inner,
        session: &SharedSession,
        generation: &SessionGeneration,
    ) -> bool {
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return false;
        }
        let event = session.lock().await.redact_private_output_gap();
        if let Some(event) = event {
            let _ = inner.events.send(event);
        }
        true
    }

    pub async fn attach(
        &self,
        input: TerminalAttachInput,
    ) -> Result<TerminalAttachment, TerminalError> {
        self.attach_with_publication_cancellation(input, CancellationToken::new())
            .await
    }

    pub(crate) async fn attach_with_publication_cancellation(
        &self,
        input: TerminalAttachInput,
        publication_cancellation: CancellationToken,
    ) -> Result<TerminalAttachment, TerminalError> {
        let events = self.inner.events.subscribe();
        let key = (input.thread_id.clone(), input.terminal_id.clone());
        let request_generation = {
            let _lifecycle = self.inner.lifecycle.lock().await;
            self.inner.worktree_removals.ensure_available(
                &input.thread_id,
                input.cwd.as_deref(),
                input.worktree_path.as_deref(),
            )?;
            if self.inner.cancellation.is_cancelled() {
                return Err(TerminalError::Shutdown);
            }
            self.inner.generations.current(&key)
        };
        let existing = {
            let sessions = self.inner.sessions.read().await;
            sessions.get(&key).cloned()
        };
        let mut session = match existing {
            Some(session) => session,
            None => {
                let cwd = input.cwd.clone().ok_or_else(|| TerminalError::NotFound {
                    thread_id: input.thread_id.clone(),
                    terminal_id: input.terminal_id.clone(),
                })?;
                self.start(
                    TerminalOpenInput {
                        thread_id: input.thread_id.clone(),
                        terminal_id: input.terminal_id.clone(),
                        cwd,
                        worktree_path: input.worktree_path.clone(),
                        cols: input.cols.unwrap_or(120),
                        rows: input.rows.unwrap_or(30),
                        env: input.env.clone(),
                        command: input.command.clone(),
                        center_panel: input.center_panel,
                    },
                    request_generation.clone(),
                    None,
                    publication_cancellation,
                )
                .await?;
                self.inner
                    .sessions
                    .read()
                    .await
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| TerminalError::NotFound {
                        thread_id: input.thread_id.clone(),
                        terminal_id: input.terminal_id.clone(),
                    })?
            }
        };
        tokio::task::yield_now().await;
        let (session_generation, status, current_cols, current_rows) = {
            let session = session.lock().await;
            (
                session.generation.clone(),
                session.status,
                session.cols,
                session.rows,
            )
        };
        if status == TerminalStatus::Running
            && (!Arc::ptr_eq(&request_generation, &session_generation)
                || session_generation.is_invalidated())
        {
            return Err(TerminalError::NotFound {
                thread_id: input.thread_id,
                terminal_id: input.terminal_id,
            });
        }
        if status != TerminalStatus::Running && input.restart_if_not_running {
            let cwd = input.cwd.ok_or_else(|| TerminalError::NotRunning {
                thread_id: input.thread_id.clone(),
                terminal_id: input.terminal_id.clone(),
            })?;
            self.restart(TerminalOpenInput {
                thread_id: input.thread_id.clone(),
                terminal_id: input.terminal_id.clone(),
                cwd,
                worktree_path: input.worktree_path,
                cols: input.cols.unwrap_or(current_cols),
                rows: input.rows.unwrap_or(current_rows),
                env: input.env,
                command: input.command,
                center_panel: input.center_panel,
            })
            .await?;
            session = self
                .require_session(&input.thread_id, &input.terminal_id)
                .await?;
        }
        if input.center_panel {
            self.mark_center_panel(&key, &session).await;
        }
        let session_generation = session.lock().await.generation.clone();
        if self.inner.cancellation.is_cancelled() {
            return Err(TerminalError::Shutdown);
        }
        let _publication = session_generation.publication.lock().await;
        let mut current = session.lock().await;
        if current.status == TerminalStatus::Running && session_generation.is_invalidated() {
            return Err(TerminalError::NotFound {
                thread_id: input.thread_id,
                terminal_id: input.terminal_id,
            });
        }
        let size_claim = input
            .size_claim
            .map(|claim| {
                let id = uuid::Uuid::new_v4();
                // Registration and tracker closure share this synchronous gate.
                // Never hold the global lifecycle lock while waiting for publication.
                let mut attachments = self
                    .inner
                    .size_claim_attachments
                    .lock()
                    .expect("terminal attachment claims");
                if self.inner.cancellation.is_cancelled() || self.inner.attachment_tasks.is_closed()
                {
                    return Err(TerminalError::Shutdown);
                }
                attachments
                    .entry(key.clone())
                    .or_default()
                    .insert(claim.clone(), id);
                let first_attachment_grant =
                    current.size_owner.is_none() && !current.first_attachment_grant_used;
                current.first_attachment_grant_used = true;
                if current.size_owner.is_none() {
                    current.size_owner = Some(SizeOwner {
                        claim: claim.clone(),
                        attachment: id,
                    });
                    let _ = self.inner.events.send(current.resized());
                } else if let Some(owner) = current
                    .size_owner
                    .as_mut()
                    .filter(|owner| owner.claim == claim)
                {
                    // A reconnect may reuse a renderer's claim. Its departed stream
                    // must not clear ownership from this replacement attachment.
                    owner.attachment = id;
                }
                let dropped = CancellationToken::new();
                let inner = self.inner.clone();
                let cleanup_key = key.clone();
                let cleanup_drop = dropped.clone();
                self.inner.attachment_tasks.spawn(async move {
                    tokio::select! {
                        () = inner.cancellation.cancelled() => {},
                        () = cleanup_drop.cancelled() => {
                            Self::release_size_claim(&inner, &cleanup_key, id).await;
                        }
                    }
                });
                Ok((
                    TerminalAttachmentClaim {
                        inner: Arc::downgrade(&self.inner),
                        key,
                        claim,
                        id,
                        dropped,
                    },
                    first_attachment_grant,
                ))
            })
            .transpose()?;
        let (size_claim, first_attachment_grant) = size_claim
            .map_or((None, false), |(claim, first_attachment_grant)| {
                (Some(claim), first_attachment_grant)
            });
        let mut initial = current.snapshot();
        initial.first_attachment_grant = first_attachment_grant;
        Ok(TerminalAttachment {
            inner: Arc::downgrade(&self.inner),
            generation: Some(current.generation.clone()),
            thread_id: input.thread_id,
            terminal_id: input.terminal_id,
            next_sequence: initial.sequence,
            initial,
            events: TerminalEventReceiver::new(events, size_claim.is_some()),
            _size_claim: size_claim,
        })
    }

    pub(crate) async fn begin_input(
        &self,
        connection_id: uuid::Uuid,
        connection_closed: CancellationToken,
        thread_id: &str,
        terminal_id: &str,
        attachment_sequence: u64,
        admission: Option<super::input::TerminalInputAdmission>,
    ) -> Result<super::input::TerminalInputLease, TerminalError> {
        let session = self.require_session(thread_id, terminal_id).await?;
        let current = session.lock().await;
        if current.generation.is_invalidated()
            || current
                .generation
                .closing
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(TerminalError::PublicationCancelled);
        }
        let identity = TerminalSessionIdentity {
            key: (thread_id.to_owned(), terminal_id.to_owned()),
            session: session.clone(),
            generation: current.generation.clone(),
            process: current
                .process
                .clone()
                .filter(|_| current.status == TerminalStatus::Running)
                .ok_or_else(|| TerminalError::NotRunning {
                    thread_id: thread_id.to_owned(),
                    terminal_id: terminal_id.to_owned(),
                })?,
        };
        drop(current);
        self.inner
            .input
            .begin(
                connection_id,
                connection_closed,
                self.inner.cancellation.clone(),
                identity,
                attachment_sequence,
                admission,
            )
            .map_err(TerminalError::from)
    }

    /// Read before resolving a [`super::input::TerminalInputAdmission`] and again once the
    /// session is registered: a change means a re-admission round may have missed it.
    pub(crate) fn input_readmission_epoch(&self) -> u64 {
        self.inner.input.readmission_epoch()
    }

    /// Starts a re-admission round for the input sessions in `scope`: returns the round's
    /// revision and their `(input_id, thread_id)`. Their frames wait for the round's result.
    pub(crate) fn start_input_readmission(
        &self,
        scope: super::input::InputReadmissionScope<'_>,
    ) -> (u64, Vec<(String, String)>) {
        self.inner.input.start_readmission(scope)
    }

    /// Applies round `revision`'s result: replaces the session's admission without
    /// interrupting its input, or seals the session when the new admission could not be taken
    /// (`None`). Results superseded by a newer round are ignored; returns whether it applied.
    pub(crate) fn readmit_input(
        &self,
        input_id: &str,
        revision: u64,
        admission: Option<super::input::TerminalInputAdmission>,
    ) -> bool {
        self.inner.input.readmit(input_id, revision, admission)
    }

    /// Whether the input session is still registered and not sealed.
    pub(crate) fn check_input_open(&self, input_id: &str) -> Result<(), TerminalError> {
        self.inner
            .input
            .check_open(input_id)
            .map_err(TerminalError::from)
    }

    pub(crate) fn prepare_input_write<'a>(
        &self,
        connection_id: uuid::Uuid,
        thread_id: &str,
        terminal_id: &str,
        input_id: &'a str,
        sequence: u64,
        data: &'a str,
    ) -> Result<super::input::PreparedTerminalInput<'a>, TerminalError> {
        self.inner.input.prepare_write(
            connection_id,
            thread_id,
            terminal_id,
            input_id,
            sequence,
            data,
        )
    }

    pub(crate) fn cancel_input(
        &self,
        connection_id: uuid::Uuid,
        thread_id: &str,
        terminal_id: &str,
        input_id: &str,
    ) -> Result<(), TerminalError> {
        self.inner
            .input
            .cancel(connection_id, thread_id, terminal_id, input_id)
            .map_err(TerminalError::from)
    }

    pub async fn write(
        &self,
        thread_id: &str,
        terminal_id: &str,
        data: &str,
    ) -> Result<(), TerminalError> {
        let session = self.require_session(thread_id, terminal_id).await?;
        let generation = session.lock().await.generation.clone();
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return Err(TerminalError::NotFound {
                thread_id: thread_id.to_owned(),
                terminal_id: terminal_id.to_owned(),
            });
        }
        let (process, status) = {
            let session = session.lock().await;
            (session.process.clone(), session.status)
        };
        if status == TerminalStatus::Exited {
            return Ok(());
        }
        let process = process.ok_or_else(|| TerminalError::NotRunning {
            thread_id: thread_id.to_string(),
            terminal_id: terminal_id.to_string(),
        })?;
        process.write(data).map_err(TerminalError::Io)
    }

    pub async fn resize(
        &self,
        thread_id: &str,
        terminal_id: &str,
        cols: u16,
        rows: u16,
        size_claim: Option<String>,
    ) -> Result<(), TerminalError> {
        validate_dimensions(cols, rows)?;
        let Some(session) = self
            .inner
            .sessions
            .read()
            .await
            .get(&(thread_id.to_string(), terminal_id.to_string()))
            .cloned()
        else {
            return Ok(());
        };
        let generation = session.lock().await.generation.clone();
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return Ok(());
        }
        let mut session = session.lock().await;
        let size_owner = match size_claim {
            Some(claim) => {
                let attachments = self
                    .inner
                    .size_claim_attachments
                    .lock()
                    .expect("terminal attachment claims");
                let Some(id) = attachments
                    .get(&(thread_id.to_owned(), terminal_id.to_owned()))
                    .and_then(|claims| claims.get(&claim))
                else {
                    // A queued resize from a departed renderer must not resurrect its claim.
                    return Ok(());
                };
                Some(SizeOwner {
                    claim,
                    attachment: *id,
                })
            }
            None => None,
        };
        let Some(process) = session.process.as_ref() else {
            return Ok(());
        };
        let size_changed = session.cols != cols || session.rows != rows;
        let owner_changed = session.size_owner.as_ref().map(|owner| &owner.claim)
            != size_owner.as_ref().map(|owner| &owner.claim);
        if size_changed {
            process.resize(cols, rows).map_err(TerminalError::Io)?;
        }
        // Applying a claim consumes the grant even if no dimensions changed.
        session.first_attachment_grant_used |= size_owner.is_some();
        if !size_changed && !owner_changed {
            return Ok(());
        }
        session.cols = cols;
        session.rows = rows;
        session.size_owner = size_owner;
        let _ = self.inner.events.send(session.resized());
        Ok(())
    }

    async fn release_size_claim(inner: &Inner, key: &SessionKey, attachment_id: uuid::Uuid) {
        let Some(session) = inner.sessions.read().await.get(key).cloned() else {
            return;
        };
        let generation = session.lock().await.generation.clone();
        let _publication = tokio::select! {
            () = inner.cancellation.cancelled() => return,
            guard = generation.publication.lock() => guard,
        };
        if !inner
            .sessions
            .read()
            .await
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            return;
        }
        let mut session = session.lock().await;
        if session.size_owner.as_ref().map(|owner| owner.attachment) != Some(attachment_id) {
            return;
        }
        session.size_owner = None;
        let _ = inner.events.send(session.resized());
    }

    pub async fn clear(&self, thread_id: &str, terminal_id: &str) -> Result<(), TerminalError> {
        let session = self.require_session(thread_id, terminal_id).await?;
        let generation = session.lock().await.generation.clone();
        let _publication = generation.publication.lock().await;
        if generation.is_invalidated() {
            return Err(TerminalError::NotFound {
                thread_id: thread_id.to_owned(),
                terminal_id: terminal_id.to_owned(),
            });
        }
        let event = {
            let mut session = session.lock().await;
            session.history.clear();
            let sequence = session.advance();
            TerminalEvent::Cleared {
                thread_id: thread_id.to_string(),
                terminal_id: terminal_id.to_string(),
                sequence,
            }
        };
        let _ = self.inner.events.send(event);
        Ok(())
    }

    pub async fn close(
        &self,
        thread_id: &str,
        terminal_id: Option<&str>,
    ) -> Result<(), TerminalError> {
        if let Some(terminal_id) = terminal_id
            && let Some(generation) = self
                .inner
                .generations
                .peek(&(thread_id.to_owned(), terminal_id.to_owned()))
        {
            generation.observation.request_cancellation_after(
                TerminalObserverCancellationReason::Closed,
                || {
                    generation.begin_closing();
                    generation.prevent_new_work();
                },
            );
        }
        let operation = terminal_id.map(|terminal_id| {
            self.inner
                .operations
                .for_key(&(thread_id.to_owned(), terminal_id.to_owned()))
        });
        let _operation = match operation {
            Some(operation) => Some(operation.lock_owned().await),
            None => None,
        };
        let generations = self
            .inner
            .generations
            .remove_matching(thread_id, terminal_id);
        for generation in &generations {
            generation.stop_output().await;
            generation
                .cancel_and_invalidate(TerminalObserverCancellationReason::Closed)
                .await;
        }
        let _lifecycle = self.inner.lifecycle.lock().await;
        let closed = self
            .close_sessions(
                thread_id,
                terminal_id,
                TerminalObserverCancellationReason::Closed,
            )
            .await;
        self.publish_closed_sessions(&closed.notifications, TerminalRemovalReason::Closed);
        log_terminal_cleanup("close", &closed.report);
        if closed.report.failure_count > 0 {
            return Err(TerminalError::Close);
        }
        Ok(())
    }

    pub async fn begin_worktree_removal(
        &self,
        thread_ids: &[String],
        worktree_path: &Path,
    ) -> Result<WorktreeRemovalGuard, TerminalError> {
        let guard = {
            let _lifecycle = self.inner.lifecycle.lock().await;
            if self.inner.cancellation.is_cancelled() {
                return Err(TerminalError::Shutdown);
            }
            let path_keys = terminal_path_keys(worktree_path);
            let sessions = self.inner.sessions.read().await;
            let mut fenced_thread_ids = thread_ids.to_vec();
            for session in sessions.values() {
                let session = session.lock().await;
                let belongs_to_worktree = [
                    session.cwd.as_str(),
                    session.worktree_path.as_deref().unwrap_or(""),
                ]
                .into_iter()
                .filter(|path| !path.is_empty())
                .flat_map(|path| terminal_path_keys(Path::new(path)))
                .any(|candidate| {
                    path_keys
                        .iter()
                        .any(|root| terminal_path_is_within(&candidate, root))
                });
                if belongs_to_worktree {
                    fenced_thread_ids.push(session.thread_id.clone());
                }
            }
            drop(sessions);
            self.inner
                .worktree_removals
                .acquire(&fenced_thread_ids, worktree_path)?
        };
        for thread_id in &guard.thread_ids {
            self.close(thread_id, None).await?;
        }
        Ok(guard)
    }

    /// Stops every live terminal process for a thread while retaining the
    /// exited session snapshots and their bounded transcript history.
    pub async fn quiesce_thread_preserving_history(
        &self,
        thread_id: &str,
    ) -> Result<(), TerminalError> {
        let identities = self.capture_thread_session_identities(thread_id).await;
        self.quiesce_sessions_preserving_history_if_current(identities)
            .await
    }

    pub(crate) async fn capture_thread_session_identities(
        &self,
        thread_id: &str,
    ) -> Vec<TerminalSessionIdentity> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let sessions = {
            let sessions = self.inner.sessions.read().await;
            sessions
                .iter()
                .filter(|((candidate_thread, _), _)| candidate_thread == thread_id)
                .map(|(key, session)| (key.clone(), session.clone()))
                .collect::<Vec<_>>()
        };
        let mut identities = Vec::with_capacity(sessions.len());
        for (key, session) in sessions {
            let (generation, process) = {
                let session = session.lock().await;
                (session.generation.clone(), session.process.clone())
            };
            if let Some(process) = process {
                identities.push(TerminalSessionIdentity {
                    key,
                    session,
                    generation,
                    process,
                });
            }
        }
        identities
    }

    pub(crate) async fn quiesce_sessions_preserving_history_if_current(
        &self,
        identities: Vec<TerminalSessionIdentity>,
    ) -> Result<(), TerminalError> {
        let (targets, mut failed) = {
            let _lifecycle = self.inner.lifecycle.lock().await;
            let mut targets = Vec::with_capacity(identities.len());
            let mut failed = false;
            for identity in identities {
                let registered = {
                    let sessions = self.inner.sessions.read().await;
                    sessions
                        .get(&identity.key)
                        .is_some_and(|current| Arc::ptr_eq(current, &identity.session))
                };
                let generation_is_current = self
                    .inner
                    .generations
                    .peek(&identity.key)
                    .is_some_and(|current| Arc::ptr_eq(&current, &identity.generation));
                let process_is_current = if registered && generation_is_current {
                    let session = identity.session.lock().await;
                    Arc::ptr_eq(&session.generation, &identity.generation)
                        && session
                            .process
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(current, &identity.process))
                } else {
                    false
                };
                if !process_is_current {
                    continue;
                }
                let exit = identity.process.subscribe_exit();
                // Stopped on request: the exit that follows is not a crash.
                exits::note_stop_requested(identity.process.pid());
                match identity.process.kill() {
                    Ok(()) => targets.push((
                        identity.key,
                        identity.session,
                        identity.generation,
                        identity.process,
                        exit,
                    )),
                    Err(error) => {
                        failed = true;
                        exits::withdraw_stop_request(identity.process.pid());
                        tracing::warn!(
                            thread_id = %identity.key.0,
                            terminal_id = %identity.key.1,
                            pid = identity.process.pid(),
                            %error,
                            "failed to stop terminal for unavailable workspace"
                        );
                    }
                }
            }
            (targets, failed)
        };
        for (key, session, generation, process, exit) in targets {
            let observed = exit.clone();
            match wait_for_terminal_process_tree_exit(process, exit).await {
                Ok(()) => {
                    let observed_exit = observed.borrow().clone();
                    if let Some(exit) = observed_exit {
                        Self::finalize_process_exit(&self.inner, &session, &generation, exit).await;
                    }
                }
                Err(error) => {
                    failed = true;
                    tracing::warn!(
                        thread_id = %key.0,
                        terminal_id = %key.1,
                        %error,
                        "terminal quiesce did not finish cleanly"
                    );
                }
            }
        }
        if failed {
            Err(TerminalError::Close)
        } else {
            Ok(())
        }
    }

    async fn close_sessions(
        &self,
        thread_id: &str,
        terminal_id: Option<&str>,
        cancellation_reason: TerminalObserverCancellationReason,
    ) -> ClosedSessions {
        let mut closed = ClosedSessions::default();
        let keys = {
            let sessions = self.inner.sessions.read().await;
            sessions
                .keys()
                .filter(|(candidate_thread, candidate_terminal)| {
                    candidate_thread == thread_id
                        && terminal_id.is_none_or(|value| candidate_terminal == value)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        for key in keys {
            let Some(session) = self.inner.sessions.read().await.get(&key).cloned() else {
                continue;
            };
            let generation = session.lock().await.generation.clone();
            generation.stop_output().await;
            let _publication = generation.publication.lock().await;
            generation.cancel_and_invalidate(cancellation_reason).await;
            let removed = {
                let mut sessions = self.inner.sessions.write().await;
                if sessions
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &session))
                {
                    sessions.remove(&key)
                } else {
                    None
                }
            };
            let Some(session) = removed else {
                continue;
            };
            let (process, sequence) = {
                let mut session = session.lock().await;
                session.attribution_registration.take();
                session.observer.take();
                let process = session.process.take();
                session.status = TerminalStatus::Exited;
                session.pid = None;
                session.has_running_subprocess = false;
                session.child_command_label = None;
                (process, session.advance())
            };
            if let Some(process) = process {
                let exit = process.subscribe_exit();
                match process.kill() {
                    Ok(()) => match wait_for_terminal_process_tree_exit(Arc::clone(&process), exit)
                        .await
                    {
                        Ok(()) => closed.report.record_success(),
                        Err(error) => closed.report.record_failure(format!(
                            "terminal {}/{} process {}: {error}",
                            key.0,
                            key.1,
                            process.pid()
                        )),
                    },
                    Err(error) => closed.report.record_failure(format!(
                        "terminal {}/{} process {}: {error}",
                        key.0,
                        key.1,
                        process.pid()
                    )),
                }
            }
            closed.notifications.push(ClosedSessionNotification {
                thread_id: key.0,
                terminal_id: key.1,
                sequence,
            });
        }
        closed
    }

    fn publish_closed_sessions(
        &self,
        notifications: &[ClosedSessionNotification],
        reason: TerminalRemovalReason,
    ) {
        for notification in notifications {
            let _ = self.inner.events.send(TerminalEvent::Closed {
                thread_id: notification.thread_id.clone(),
                terminal_id: notification.terminal_id.clone(),
                sequence: notification.sequence,
            });
            let _ = self.inner.metadata.send(TerminalMetadataEvent::Remove {
                thread_id: notification.thread_id.clone(),
                terminal_id: notification.terminal_id.clone(),
                reason,
            });
        }
    }

    /// Terminal sessions whose process is starting or running.
    pub async fn live_session_count(&self) -> usize {
        let sessions = self.inner.sessions.read().await;
        let mut live = 0;
        for session in sessions.values() {
            if matches!(
                session.lock().await.summary().status,
                TerminalStatus::Starting | TerminalStatus::Running
            ) {
                live += 1;
            }
        }
        live
    }

    /// `(thread_id, terminal_id, root pid)` of every starting or running session.
    pub async fn live_session_pids(&self) -> Vec<(String, String, u32)> {
        let sessions = self.inner.sessions.read().await;
        let mut live = Vec::new();
        for session in sessions.values() {
            let summary = session.lock().await.summary();
            if matches!(
                summary.status,
                TerminalStatus::Starting | TerminalStatus::Running
            ) && let Some(pid) = summary.pid
            {
                live.push((summary.thread_id, summary.terminal_id, pid));
            }
        }
        live
    }

    pub async fn subscribe_metadata(&self) -> TerminalMetadataAttachment {
        let events = self.inner.metadata.subscribe();
        let sessions = self.inner.sessions.read().await;
        let mut initial = Vec::with_capacity(sessions.len());
        for session in sessions.values() {
            initial.push(session.lock().await.summary());
        }
        initial.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.thread_id.cmp(&right.thread_id))
                .then_with(|| left.terminal_id.cmp(&right.terminal_id))
        });
        TerminalMetadataAttachment { initial, events }
    }

    pub fn subscribe_events(&self) -> TerminalEventReceiver {
        TerminalEventReceiver::new(self.inner.events.subscribe(), false)
    }

    pub async fn shutdown(&self) {
        let report = self.shutdown_with_report().await;
        log_terminal_cleanup("shutdown", &report);
    }

    async fn shutdown_with_report(&self) -> ProcessCleanupReport {
        self.inner.cancellation.cancel();
        {
            let _registrations = self
                .inner
                .size_claim_attachments
                .lock()
                .expect("terminal attachment claims");
            self.inner.attachment_tasks.close();
        }
        self.inner.attachment_tasks.wait().await;
        for generation in self.inner.generations.remove_all() {
            generation.stop_output().await;
            generation
                .cancel_and_invalidate(TerminalObserverCancellationReason::Shutdown)
                .await;
        }
        let _lifecycle = self.inner.lifecycle.lock().await;
        let keys = self
            .inner
            .sessions
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut report = ProcessCleanupReport::default();
        for (thread_id, terminal_id) in keys {
            let closed = self
                .close_sessions(
                    &thread_id,
                    Some(&terminal_id),
                    TerminalObserverCancellationReason::Shutdown,
                )
                .await;
            self.publish_closed_sessions(&closed.notifications, TerminalRemovalReason::Shutdown);
            report.merge(closed.report);
        }
        if let Some(preparer) = self.inner.options.launch_preparer.as_ref() {
            preparer.shutdown().await;
        }
        report
    }

    async fn require_session(
        &self,
        thread_id: &str,
        terminal_id: &str,
    ) -> Result<SharedSession, TerminalError> {
        self.inner
            .sessions
            .read()
            .await
            .get(&(thread_id.to_string(), terminal_id.to_string()))
            .cloned()
            .ok_or_else(|| TerminalError::NotFound {
                thread_id: thread_id.to_string(),
                terminal_id: terminal_id.to_string(),
            })
    }
}

/// Reports a session's process that ended by itself to the profiler. Only crash signals
/// count: in a terminal people end processes with `exit 1`, Ctrl-C or `kill`. BiBCode's own
/// stops invalidate the generation first, or note the stop.
fn report_process_exit(session: &Session) {
    let (Some(pid), Some(status)) = (
        session.pid,
        session
            .process
            .as_ref()
            .and_then(|process| process.exit_status()),
    ) else {
        return;
    };
    let driver = session
        .command
        .as_ref()
        .and_then(|command| command.activity.as_ref())
        .map(|activity| activity.driver_kind.as_str());
    let (kind, label) = driver.map_or(("terminal", "shell"), |driver| ("provider", driver));
    exits::report_exit(
        Watch::CrashSignals,
        &Exited {
            kind,
            label,
            exe: &session.executable,
            pid,
            uptime: session.started.elapsed(),
        },
        &status,
        false,
    );
}

async fn wait_for_terminal_process_tree_exit(
    process: Arc<dyn PtyProcess>,
    mut root_exit: tokio::sync::watch::Receiver<Option<PtyExit>>,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + TERMINAL_CLOSE_WAIT_TIMEOUT;
    let tree_process = Arc::clone(&process);
    let tree_exit = tokio::task::spawn_blocking(move || {
        tree_process.wait_for_process_tree_exit(TERMINAL_CLOSE_WAIT_TIMEOUT)
    })
    .await
    .map_err(|error| format!("process-tree wait task failed: {error}"))??;

    match tree_exit {
        Some(true) => {
            if wait_for_terminal_root_exit(&mut root_exit, deadline).await {
                Ok(())
            } else {
                Err(format!(
                    "process did not finish reaping within {} ms",
                    TERMINAL_CLOSE_WAIT_TIMEOUT.as_millis()
                ))
            }
        }
        Some(false) => Err(format!(
            "process tree did not exit within {} ms",
            TERMINAL_CLOSE_WAIT_TIMEOUT.as_millis()
        )),
        None => {
            if wait_for_terminal_root_exit(&mut root_exit, deadline).await {
                Ok(())
            } else {
                Err(format!(
                    "process did not exit within {} ms",
                    TERMINAL_CLOSE_WAIT_TIMEOUT.as_millis()
                ))
            }
        }
    }
}

async fn wait_for_terminal_root_exit(
    root_exit: &mut tokio::sync::watch::Receiver<Option<PtyExit>>,
    deadline: tokio::time::Instant,
) -> bool {
    root_exit.borrow().is_some()
        || tokio::time::timeout_at(deadline, async {
            while root_exit.borrow().is_none() {
                root_exit.changed().await.map_err(|_| ())?;
            }
            Ok::<(), ()>(())
        })
        .await
        .ok()
        .and_then(Result::ok)
        .is_some()
}

fn log_terminal_cleanup(operation: &'static str, report: &ProcessCleanupReport) {
    if report.failure_count > 0 {
        tracing::warn!(
            operation,
            attempted = report.attempted,
            succeeded = report.succeeded,
            failed = report.failure_count,
            failures = ?report.failures,
            "terminal process-owner cleanup completed with failures"
        );
    }
}

/// Filters size events for public subscriptions and legacy attachments, while
/// claim-carrying attach streams receive them in order with output.
pub struct TerminalEventReceiver {
    events: broadcast::Receiver<TerminalEvent>,
    include_resized: bool,
}

impl TerminalEventReceiver {
    fn new(events: broadcast::Receiver<TerminalEvent>, include_resized: bool) -> Self {
        Self {
            events,
            include_resized,
        }
    }

    fn accepts(&self, event: &TerminalEvent) -> bool {
        self.include_resized || !matches!(event, TerminalEvent::Resized { .. })
    }

    pub async fn recv(&mut self) -> Result<TerminalEvent, broadcast::error::RecvError> {
        loop {
            let event = self.events.recv().await?;
            if self.accepts(&event) {
                return Ok(event);
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<TerminalEvent, broadcast::error::TryRecvError> {
        loop {
            let event = self.events.try_recv()?;
            if self.accepts(&event) {
                return Ok(event);
            }
        }
    }
}

struct TerminalAttachmentClaim {
    inner: std::sync::Weak<Inner>,
    key: SessionKey,
    claim: String,
    id: uuid::Uuid,
    dropped: CancellationToken,
}

impl Drop for TerminalAttachmentClaim {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            let mut attachments = inner
                .size_claim_attachments
                .lock()
                .expect("terminal attachment claims");
            if let Some(claims) = attachments.get_mut(&self.key) {
                if claims.get(&self.claim) == Some(&self.id) {
                    claims.remove(&self.claim);
                }
                if claims.is_empty() {
                    attachments.remove(&self.key);
                }
            }
        }
        // The manager-owned worker performs asynchronous publication even when
        // the stream is dropped outside the Tokio runtime's thread.
        self.dropped.cancel();
    }
}

pub struct TerminalAttachment {
    pub initial: TerminalSessionSnapshot,
    inner: std::sync::Weak<Inner>,
    /// The process this attachment last saw; `None` after a restart it was told about.
    generation: Option<Arc<SessionGeneration>>,
    thread_id: String,
    terminal_id: String,
    next_sequence: u64,
    events: TerminalEventReceiver,
    _size_claim: Option<TerminalAttachmentClaim>,
}

/// What a terminal attachment delivers next.
pub enum AttachmentItem {
    Event(TerminalEvent),
    /// The attachment fell further behind than the shared event channel holds, so it may have
    /// missed this terminal's output: the session's current state, for the client to reset
    /// to. Skipping the lost events instead left lines missing from the terminal.
    /// `same_process` is false when the session restarted meanwhile, or may have.
    Resync {
        snapshot: TerminalSessionSnapshot,
        same_process: bool,
    },
}

impl TerminalAttachment {
    pub async fn recv(&mut self) -> Option<AttachmentItem> {
        loop {
            match self.events.recv().await {
                Ok(event) => {
                    if let Some(event) = self.accept(event) {
                        return Some(self.deliver(event).await);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(item) = self.resync().await {
                        return Some(item);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// The next item already waiting, without waiting for one to arrive.
    pub async fn try_recv(&mut self) -> Option<AttachmentItem> {
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    if let Some(event) = self.accept(event) {
                        return Some(self.deliver(event).await);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    if let Some(item) = self.resync().await {
                        return Some(item);
                    }
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => {
                    return None;
                }
            }
        }
    }

    async fn deliver(&mut self, event: TerminalEvent) -> AttachmentItem {
        // The client moves to the restarted process: later resyncs compare against it, when
        // the live session is still that process. After a further restart it is unknown, and
        // the next resync counts as a new process.
        if let TerminalEvent::Restarted { snapshot, .. } = &event {
            self.generation = match self.current_session().await {
                Some(session) => {
                    let session = session.lock().await;
                    (session.pid == snapshot.pid).then(|| session.generation.clone())
                }
                None => None,
            };
        }
        AttachmentItem::Event(event)
    }

    /// The session closed and its event was among the lost ones: say so.
    fn closed(&mut self) -> AttachmentItem {
        self.next_sequence = self.next_sequence.saturating_add(1);
        AttachmentItem::Event(TerminalEvent::Closed {
            thread_id: self.thread_id.clone(),
            terminal_id: self.terminal_id.clone(),
            sequence: self.next_sequence,
        })
    }

    async fn current_session(&self) -> Option<SharedSession> {
        let inner = self.inner.upgrade()?;
        inner
            .sessions
            .read()
            .await
            .get(&(self.thread_id.clone(), self.terminal_id.clone()))
            .cloned()
    }

    fn accept(&mut self, event: TerminalEvent) -> Option<TerminalEvent> {
        if !event.belongs_to(&self.thread_id, &self.terminal_id) {
            return None;
        }
        let sequence = event.sequence();
        // A restarted session owns a fresh sequence space. Its
        // authoritative reset must reach existing attachments even
        // when the prior process had already advanced past it.
        if matches!(event, TerminalEvent::Restarted { .. }) {
            self.next_sequence = sequence;
            return Some(event);
        }
        if sequence > self.next_sequence {
            self.next_sequence = sequence;
            return Some(event);
        }
        None
    }

    /// The session's current state; events it already holds are skipped afterwards. History
    /// and sequence advance together under the session lock, so they agree.
    ///
    /// Everything still queued is published before the snapshot is read, so the snapshot
    /// covers it and it is dropped, a restart included. A lag caused only by other terminals
    /// changes nothing here and yields no resync, so the display and selection stay.
    // ponytail: a restart published between the drain and the read still arrives after the
    // snapshot and resets to its startup state; a per-process restart counter would fence it.
    async fn resync(&mut self) -> Option<AttachmentItem> {
        // A start or restart in progress settles first: between removing the old session and
        // installing the new one the terminal is absent without being closed.
        let inner = self.inner.upgrade()?;
        let operation = inner
            .operations
            .for_key(&(self.thread_id.clone(), self.terminal_id.clone()));
        let _operation = operation.lock().await;
        // Drained under the lock: what is queued now, a restart included, the snapshot covers.
        while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = self.events.try_recv() {}
        let Some(session) = self.current_session().await else {
            return Some(self.closed());
        };
        let (snapshot, generation) = {
            let session = session.lock().await;
            (session.snapshot(), session.generation.clone())
        };
        // Removed while its state was read: that state may already carry the close's sequence,
        // which would then turn the real `Closed` away.
        if !self
            .current_session()
            .await
            .is_some_and(|current| Arc::ptr_eq(&current, &session))
        {
            return Some(self.closed());
        }
        let same_process = self
            .generation
            .as_ref()
            .is_some_and(|seen| Arc::ptr_eq(seen, &generation));
        self.generation = Some(generation);
        if same_process && snapshot.sequence == self.next_sequence {
            return None;
        }
        self.next_sequence = snapshot.sequence;
        Some(AttachmentItem::Resync {
            snapshot,
            same_process,
        })
    }
}

pub struct TerminalMetadataAttachment {
    pub initial: Vec<TerminalSummary>,
    events: broadcast::Receiver<TerminalMetadataEvent>,
}

impl TerminalMetadataAttachment {
    pub async fn recv(&mut self) -> Option<TerminalMetadataEvent> {
        loop {
            match self.events.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

fn invalidated_creation_error(input: &TerminalOpenInput) -> TerminalError {
    TerminalError::NotFound {
        thread_id: input.thread_id.clone(),
        terminal_id: input.terminal_id.clone(),
    }
}

async fn validate_cwd(cwd: &Path) -> Result<(), TerminalError> {
    match tokio::fs::metadata(cwd).await {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(TerminalError::CwdNotDirectory(
            cwd.to_string_lossy().into_owned(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(
            TerminalError::CwdNotFound(cwd.to_string_lossy().into_owned()),
        ),
        Err(error) => Err(TerminalError::Io(error.to_string())),
    }
}

fn validate_dimensions(cols: u16, rows: u16) -> Result<(), TerminalError> {
    if !(1..=1_000).contains(&cols) || !(1..=500).contains(&rows) {
        return Err(TerminalError::Io(format!(
            "invalid terminal size {cols}x{rows}"
        )));
    }
    Ok(())
}

fn terminal_label(terminal_id: &str) -> String {
    terminal_id
        .strip_prefix("term-")
        .filter(|suffix| !suffix.is_empty())
        .map_or_else(
            || terminal_id.to_string(),
            |suffix| format!("Terminal {suffix}"),
        )
}

fn format_shell_candidate(candidate: &ShellCandidate) -> String {
    if candidate.args.is_empty() {
        candidate.command.clone()
    } else {
        format!("{} {}", candidate.command, candidate.args.join(" "))
    }
}

fn truncate_terminal_label(value: &str) -> String {
    let truncated = value
        .chars()
        .take(MAX_TERMINAL_LABEL_LENGTH)
        .collect::<String>();
    if truncated.is_empty() {
        value.to_string()
    } else {
        truncated
    }
}

fn normalize_child_command_name(raw: &str) -> Option<String> {
    let mut trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('(') && trimmed.ends_with(')'))
    {
        trimmed = trimmed[1..trimmed.len() - 1].trim();
    }
    let first_token = trimmed.split_whitespace().next()?.trim();
    if first_token.is_empty() {
        return None;
    }
    let base = first_token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first_token);
    let without_exe = if base.to_ascii_lowercase().ends_with(".exe") {
        &base[..base.len().saturating_sub(4)]
    } else {
        base
    };
    (!without_exe.is_empty()).then(|| without_exe.to_string())
}

fn redact_private_values(mut value: String, private_values: &[String]) -> String {
    for private_value in private_values {
        if !private_value.is_empty() {
            value = value.replace(private_value, "[redacted]");
        }
    }
    value
}

fn environment_keys_equal(platform: Platform, left: &str, right: &str) -> bool {
    match platform {
        Platform::Windows => left.eq_ignore_ascii_case(right),
        Platform::Unix => left == right,
    }
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{
        AttributionKind, AttributionScope, ProcessAttributionRegistry, ProcessRow,
    };
    use crate::{
        activity::{
            ActivityCapabilities, ActivityProjection, ActivityRepository, ProviderActivityMutation,
        },
        persistence::{Database, run_migrations},
        provider_terminal::{TerminalGenerationActivityPublisher, TerminalObserverWorkerContext},
    };
    use std::time::Instant;

    #[derive(Debug, Default)]
    struct ShutdownRecordingPreparer {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl TerminalLaunchPreparer for ShutdownRecordingPreparer {
        fn prepare(
            &self,
            _input: TerminalLaunchPreparationInput,
        ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
            Box::pin(std::future::ready(TerminalLaunchPreparation::PassThrough))
        }

        fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
    }

    #[derive(Debug)]
    struct RegistryObserver;

    impl PreparedTerminalObserver for RegistryObserver {
        fn on_spawned(
            &self,
            _pid: u32,
            _generation: TerminalObserverGenerationLease,
            _workers: TerminalObserverWorkerContext,
        ) {
        }

        fn diagnostic_label(&self) -> &str {
            "registry-observer"
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn observer_callback_slot_is_reusable_after_aborted_owner() {
        let local_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let global_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let isolation =
            ObserverCallbackIsolation::with_slots(local_slots.clone(), global_slots.clone());
        let exited = isolation.callback_thread_joined();
        let exit_hook = isolation.callback_thread_exit_hook();
        let (started_sender, started) = tokio::sync::oneshot::channel();
        let (release, release_receiver) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn({
            let isolation = isolation.clone();
            async move {
                run_observer_callback(
                    isolation,
                    "aborted_owner",
                    Duration::from_secs(5),
                    async move {
                        started_sender.send(()).expect("callback thread started");
                        release_receiver.await.expect("release callback thread");
                    },
                )
                .await
            }
        });
        started.await.expect("callback thread started");

        caller.abort();
        assert!(
            caller
                .await
                .expect_err("aborted callback owner")
                .is_cancelled()
        );
        assert!(local_slots.clone().try_acquire_owned().is_err());
        assert!(global_slots.clone().try_acquire_owned().is_err());
        release.send(()).expect("release callback thread");
        exit_hook.wait_until_reached().await;
        let local_slot_retained_until_thread_exit =
            local_slots.clone().try_acquire_owned().is_err();
        let global_slot_retained_until_thread_exit =
            global_slots.clone().try_acquire_owned().is_err();
        exit_hook.release();
        exited.await.expect("callback thread joined");
        assert!(local_slot_retained_until_thread_exit);
        assert!(global_slot_retained_until_thread_exit);
        assert!(local_slots.try_acquire_owned().is_ok());
        assert!(global_slots.try_acquire_owned().is_ok());

        assert_eq!(
            run_observer_callback(
                isolation,
                "replacement_after_abort",
                Duration::from_secs(1),
                async { "replacement" },
            )
            .await,
            Some("replacement")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn observer_callback_slot_is_reusable_after_callback_panic() {
        let local_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let global_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let isolation =
            ObserverCallbackIsolation::with_slots(local_slots.clone(), global_slots.clone());
        let exited = isolation.callback_thread_joined();
        let exit_hook = isolation.callback_thread_exit_hook();
        let (started_sender, started) = tokio::sync::oneshot::channel();
        let (release, release_receiver) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn({
            let isolation = isolation.clone();
            async move {
                run_observer_callback(
                    isolation,
                    "panicked_callback",
                    Duration::from_secs(1),
                    async move {
                        started_sender.send(()).expect("callback thread started");
                        release_receiver.await.expect("release callback thread");
                        panic!("injected observer callback panic");
                    },
                )
                .await
            }
        });
        started.await.expect("callback thread started");
        assert!(local_slots.clone().try_acquire_owned().is_err());
        assert!(global_slots.clone().try_acquire_owned().is_err());
        release.send(()).expect("release callback thread");
        assert_eq!(caller.await.expect("panicked callback owner"), None);
        exit_hook.wait_until_reached().await;
        let local_slot_retained_until_thread_exit =
            local_slots.clone().try_acquire_owned().is_err();
        let global_slot_retained_until_thread_exit =
            global_slots.clone().try_acquire_owned().is_err();
        exit_hook.release();
        exited.await.expect("callback thread joined");
        assert!(local_slot_retained_until_thread_exit);
        assert!(global_slot_retained_until_thread_exit);
        assert!(local_slots.try_acquire_owned().is_ok());
        assert!(global_slots.try_acquire_owned().is_ok());

        assert_eq!(
            run_observer_callback(
                isolation,
                "replacement_after_panic",
                Duration::from_secs(1),
                async { 7_u8 },
            )
            .await,
            Some(7)
        );
    }

    #[tokio::test]
    async fn hardening_restart_replacement_cancels_exact_displaced_generation_before_invalidation()
    {
        let registry = SessionGenerationRegistry::new(tokio::runtime::Handle::try_current().ok());
        let key = ("thread-race".to_owned(), "terminal-race".to_owned());
        let first = registry.current(&key);
        first.invalidate().await;
        let displaced = registry.current(&key);
        displaced
            .install_observer(PreparedObserverHandle::new(
                Box::new(RegistryObserver),
                displaced.observation.clone(),
                ObserverCallbackIsolation::default(),
            ))
            .expect("observer installation");

        let (exact_displaced, replacement) = registry.replace(&key);
        exact_displaced
            .expect("displaced generation")
            .cancel_and_invalidate(TerminalObserverCancellationReason::Restarted)
            .await;

        assert_eq!(
            displaced.observation.cancellation_reason(),
            Some(TerminalObserverCancellationReason::Restarted),
            "the exact generation displaced by replacement must receive cancellation"
        );
        assert!(!displaced.observation.is_current());
        assert!(replacement.observation.is_current());
    }

    #[tokio::test]
    async fn shutdown_drains_the_owned_launch_preparer_after_terminal_generations() {
        let preparer = Arc::new(ShutdownRecordingPreparer::default());
        let manager = TerminalManager::new(
            Arc::new(HistoryTestBackend::default()),
            TerminalManagerOptions {
                launch_preparer: Some(preparer.clone()),
                ..TerminalManagerOptions::default()
            },
        );

        manager.shutdown().await;
        manager.shutdown().await;

        assert_eq!(preparer.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn hardening_keyed_operation_registry_prunes_dead_entries() {
        let registry = SessionOperationRegistry::default();
        for terminal in 0..128 {
            drop(registry.for_key(&(
                "thread-operation-pruning".to_owned(),
                format!("terminal-{terminal}"),
            )));
        }
        let survivor = registry.for_key(&(
            "thread-operation-pruning".to_owned(),
            "terminal-survivor".to_owned(),
        ));
        assert_eq!(
            registry
                .operations
                .lock()
                .expect("operation registry lock")
                .len(),
            1,
            "dead logical-key locks accumulated in the operation registry"
        );
        drop(survivor);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hardening_publication_lifecycle_blocked_activity_does_not_block_another_key() {
        let root = tempfile::tempdir().expect("temp dir");
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let manager = TerminalManager::new(
            Arc::new(HistoryTestBackend::default()),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let key = (
            "thread-publication".to_owned(),
            "terminal-blocked".to_owned(),
        );
        let generation = manager.inner.generations.current(&key);
        let publisher = TerminalGenerationActivityPublisher::new(
            generation.observation.clone(),
            projection.clone(),
            Arc::new(Mutex::new(())),
        );
        assert!(
            publisher
                .publish_correlated(
                    "codex",
                    Some("codex"),
                    ActivityCapabilities::structured_full(false),
                )
                .await
                .expect("scope publication")
        );
        let (entered, release) = projection.pause_before_publish_for_test(1);
        let entered_wait = entered.notified();
        tokio::pin!(entered_wait);
        let publish = tokio::spawn(async move {
            publisher
                .apply(
                    "event:block",
                    vec![
                        ProviderActivityMutation::upsert_actor(
                            "actor:block",
                            None,
                            "Blocked actor",
                            "running",
                        )
                        .expect("actor mutation"),
                    ],
                    "2026-07-25T12:00:00Z",
                )
                .await
        });
        entered_wait.await;

        let restart_manager = manager.clone();
        let restart_root = root.path().to_path_buf();
        let restart = tokio::spawn(async move {
            restart_manager
                .restart(TerminalOpenInput::new(
                    "thread-publication",
                    "terminal-blocked",
                    restart_root,
                    80,
                    24,
                ))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !generation
                .closing
                .load(std::sync::atomic::Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("restart reached invalidation");

        let unrelated = tokio::time::timeout(Duration::from_secs(1), async {
            manager
                .open(TerminalOpenInput::new(
                    "thread-publication",
                    "terminal-unrelated",
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await?;
            manager
                .attach(TerminalAttachInput::existing(
                    "thread-publication",
                    "terminal-unrelated",
                ))
                .await
        })
        .await;
        release.notify_one();
        publish
            .await
            .expect("publication task")
            .expect("activity publication");
        restart
            .await
            .expect("restart task")
            .expect("restart result");
        manager.shutdown().await;

        assert!(
            matches!(unrelated, Ok(Ok(_))),
            "activity publication held the global terminal lifecycle lock"
        );
    }

    #[derive(Debug)]
    struct HistoryTestPty {
        pid: u32,
        process_identity: Option<crate::diagnostics::ProcessIdentity>,
        exit_on_identity_read: std::sync::Mutex<Option<PtyExit>>,
        exit_after_identity_read: std::sync::Mutex<Option<(Duration, PtyExit)>>,
        output: broadcast::Sender<String>,
        exit: tokio::sync::watch::Sender<Option<PtyExit>>,
        killed: std::sync::atomic::AtomicBool,
        exit_on_kill: std::sync::atomic::AtomicBool,
        /// What `exit_status` reports once the process has exited.
        exit_status: std::sync::Mutex<Option<std::process::ExitStatus>>,
        tree_exit_supported: std::sync::atomic::AtomicBool,
        tree_exited: std::sync::atomic::AtomicBool,
        kill_error: std::sync::Mutex<Option<String>>,
        write_error: std::sync::Mutex<Option<String>>,
        writes: std::sync::Mutex<Vec<String>>,
        resize_error: std::sync::Mutex<Option<String>>,
        resizes: std::sync::Mutex<Vec<(u16, u16)>>,
        output_on_resize: std::sync::Mutex<Option<String>>,
    }

    impl HistoryTestPty {
        fn new(pid: u32) -> Self {
            let (output, _) = broadcast::channel(16);
            let (exit, _) = tokio::sync::watch::channel(None);
            Self {
                pid,
                process_identity: Some(crate::diagnostics::ProcessIdentity { pid, started_at: 0 }),
                exit_on_identity_read: std::sync::Mutex::new(None),
                exit_after_identity_read: std::sync::Mutex::new(None),
                output,
                exit,
                killed: std::sync::atomic::AtomicBool::new(false),
                exit_on_kill: std::sync::atomic::AtomicBool::new(true),
                exit_status: std::sync::Mutex::new(None),
                tree_exit_supported: std::sync::atomic::AtomicBool::new(false),
                tree_exited: std::sync::atomic::AtomicBool::new(true),
                kill_error: std::sync::Mutex::new(None),
                write_error: std::sync::Mutex::new(None),
                writes: std::sync::Mutex::new(Vec::new()),
                resize_error: std::sync::Mutex::new(None),
                resizes: std::sync::Mutex::new(Vec::new()),
                output_on_resize: std::sync::Mutex::new(None),
            }
        }

        fn with_identity(pid: u32) -> Self {
            let mut process = Self::new(pid);
            process.process_identity =
                Some(crate::diagnostics::ProcessIdentity { pid, started_at: 0 });
            process
        }

        fn emit(&self, data: &str) {
            self.output.send(data.to_owned()).expect("output receiver");
        }

        fn is_killed(&self) -> bool {
            self.killed.load(std::sync::atomic::Ordering::Acquire)
        }

        fn writes(&self) -> Vec<String> {
            self.writes.lock().expect("writes lock").clone()
        }

        fn exit(&self, exit_code: i32) {
            self.exit
                .send(Some(PtyExit {
                    exit_code: Some(exit_code),
                    signal: None,
                }))
                .expect("exit receiver");
        }

        fn delay_exit_on_kill(&self) {
            self.exit_on_kill
                .store(false, std::sync::atomic::Ordering::Release);
        }

        fn keep_process_tree_running(&self) {
            self.tree_exit_supported
                .store(true, std::sync::atomic::Ordering::Release);
            self.tree_exited
                .store(false, std::sync::atomic::Ordering::Release);
        }

        fn report_process_tree_exited(&self) {
            self.tree_exit_supported
                .store(true, std::sync::atomic::Ordering::Release);
            self.tree_exited
                .store(true, std::sync::atomic::Ordering::Release);
        }

        fn fail_kill(&self, error: impl Into<String>) {
            *self.kill_error.lock().expect("kill error") = Some(error.into());
        }
    }

    impl PtyProcess for HistoryTestPty {
        fn pid(&self) -> u32 {
            self.pid
        }

        fn process_identity(&self) -> Option<crate::diagnostics::ProcessIdentity> {
            if let Some(exit) = self
                .exit_on_identity_read
                .lock()
                .expect("exit-on-identity-read lock")
                .take()
            {
                self.exit.send_replace(Some(exit));
            }
            if let Some((delay, exit)) = self
                .exit_after_identity_read
                .lock()
                .expect("exit-after-identity-read lock")
                .take()
            {
                let exit_sender = self.exit.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    exit_sender.send_replace(Some(exit));
                });
            }
            self.process_identity
        }

        fn write(&self, data: &str) -> Result<(), String> {
            self.writes
                .lock()
                .expect("writes lock")
                .push(data.to_owned());
            self.write_error
                .lock()
                .expect("write error")
                .clone()
                .map_or(Ok(()), Err)
        }

        fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
            if let Some(error) = self.resize_error.lock().unwrap().clone() {
                return Err(error);
            }
            self.resizes.lock().unwrap().push((cols, rows));
            if let Some(output) = self.output_on_resize.lock().unwrap().as_ref() {
                self.emit(output);
            }
            Ok(())
        }

        fn kill(&self) -> Result<(), String> {
            self.killed
                .store(true, std::sync::atomic::Ordering::Release);
            let result = self
                .kill_error
                .lock()
                .expect("kill error")
                .clone()
                .map_or(Ok(()), Err);
            if result.is_ok() && self.exit_on_kill.load(std::sync::atomic::Ordering::Acquire) {
                self.exit.send_replace(Some(PtyExit {
                    exit_code: None,
                    signal: None,
                }));
            }
            result
        }

        fn wait_for_process_tree_exit(&self, _timeout: Duration) -> Result<Option<bool>, String> {
            if self
                .tree_exit_supported
                .load(std::sync::atomic::Ordering::Acquire)
            {
                Ok(Some(
                    self.tree_exited.load(std::sync::atomic::Ordering::Acquire),
                ))
            } else {
                Ok(None)
            }
        }

        fn subscribe_output(&self) -> broadcast::Receiver<String> {
            self.output.subscribe()
        }

        fn subscribe_exit(&self) -> tokio::sync::watch::Receiver<Option<PtyExit>> {
            self.exit.subscribe()
        }

        fn exit_status(&self) -> Option<std::process::ExitStatus> {
            *self.exit_status.lock().expect("exit status")
        }
    }

    #[tokio::test]
    async fn terminal_snapshot_reports_native_osc_responder_activity_through_restart() {
        let root = tempfile::tempdir().unwrap();
        let manager = TerminalManager::new(
            Arc::new(PortablePtyBackend),
            TerminalManagerOptions {
                preferred_shell: Some(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.to_owned()),
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new("colors", "term", root.path().to_path_buf(), 80, 24);
        let plain = manager.open(input.clone()).await.unwrap();
        assert!(!plain.osc_color_responder_active);
        input.env.insert(
            super::super::osc::OSC_BACKGROUND_ENV.to_owned(),
            "10,20,30".to_owned(),
        );
        let colored = manager.restart(input.clone()).await.unwrap();
        assert!(colored.osc_color_responder_active);
        let attached = manager
            .attach(TerminalAttachInput::existing("colors", "term"))
            .await
            .unwrap();
        assert!(attached.initial.osc_color_responder_active);
        input.env.clear();
        assert!(
            !manager
                .restart(input)
                .await
                .unwrap()
                .osc_color_responder_active
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_legacy_attachments_and_general_events_never_receive_resized() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "compat",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let mut legacy = manager
            .attach(TerminalAttachInput::existing("compat", "term"))
            .await
            .unwrap();
        let mut general = manager.subscribe_events();
        manager
            .resize("compat", "term", 91, 42, None)
            .await
            .unwrap();
        backend.latest().emit("after resize");
        let legacy_event = next_attachment_event(&mut legacy).await;
        assert!(
            matches!(legacy_event, TerminalEvent::Output { ref data, .. } if data == "after resize")
        );
        let general_event = tokio::time::timeout(Duration::from_secs(2), general.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(general_event, TerminalEvent::Output { ref data, .. } if data == "after resize")
        );
        manager.shutdown().await;
    }

    fn size_attachment(claim: &str) -> TerminalAttachInput {
        TerminalAttachInput {
            size_claim: Some(claim.to_owned()),
            ..TerminalAttachInput::existing("sizing", "term")
        }
    }

    async fn size_fixture(
        cols: u16,
        rows: u16,
    ) -> (tempfile::TempDir, Arc<HistoryTestBackend>, TerminalManager) {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                cols,
                rows,
            ))
            .await
            .unwrap();
        (root, backend, manager)
    }

    #[tokio::test]
    async fn live_session_count_counts_open_sessions_until_they_close() {
        let (root, _backend, manager) = size_fixture(80, 24).await;
        assert_eq!(manager.live_session_count().await, 1);
        manager
            .open(TerminalOpenInput::new(
                "sizing",
                "term-2",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        assert_eq!(manager.live_session_count().await, 2);
        manager.close("sizing", Some("term")).await.unwrap();
        assert_eq!(manager.live_session_count().await, 1);
        manager.close("sizing", Some("term-2")).await.unwrap();
        assert_eq!(manager.live_session_count().await, 0);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn live_session_pids_reports_running_sessions_until_they_close() {
        let (_root, _backend, manager) = size_fixture(80, 24).await;
        assert_eq!(
            manager.live_session_pids().await,
            vec![("sizing".to_owned(), "term".to_owned(), 1)]
        );
        manager.close("sizing", Some("term")).await.unwrap();
        assert!(manager.live_session_pids().await.is_empty());
        manager.shutdown().await;
    }

    async fn next_attachment_event(attachment: &mut TerminalAttachment) -> TerminalEvent {
        match tokio::time::timeout(Duration::from_secs(2), attachment.recv())
            .await
            .unwrap()
            .unwrap()
        {
            AttachmentItem::Event(event) => event,
            AttachmentItem::Resync { .. } => panic!("expected an event, not a resync"),
        }
    }

    /// An attachment that falls further behind than the event channel holds resets its client
    /// to the session's state instead of silently skipping output, which left lines missing
    /// from the terminal after a burst.
    #[tokio::test]
    async fn a_lagging_attachment_resyncs_instead_of_skipping_output() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                event_capacity: 16,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "lag",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let mut attachment = manager
            .attach(TerminalAttachInput::existing("lag", "term"))
            .await
            .unwrap();
        let pty = backend.latest();
        for line in 0..100 {
            pty.emit(&format!("line {line}\n"));
            // Each line is published before the next, while the attachment reads nothing.
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let item = tokio::time::timeout(Duration::from_secs(2), attachment.recv())
            .await
            .unwrap()
            .unwrap();
        let AttachmentItem::Resync {
            snapshot,
            same_process: true,
        } = item
        else {
            panic!("a lagging attachment gets the same process's state");
        };
        for line in 0..100 {
            assert!(
                snapshot.history.contains(&format!("line {line}\n")),
                "line {line}"
            );
        }
        // Only what follows the snapshot is delivered after it.
        pty.emit("after");
        let event = loop {
            match tokio::time::timeout(Duration::from_secs(2), attachment.recv())
                .await
                .unwrap()
                .unwrap()
            {
                AttachmentItem::Event(TerminalEvent::Output { data, .. }) => break data,
                AttachmentItem::Event(_) => continue,
                AttachmentItem::Resync { .. } => panic!("one resync"),
            }
        };
        assert_eq!(event, "after");
        manager.shutdown().await;
    }

    /// Reads waiting when the publisher runs go out as one event: publishing them one by one
    /// fell behind a program printing many short lines, and the PTY channel dropped output.
    #[tokio::test]
    async fn reads_already_waiting_are_published_together() {
        let (_root, backend, manager) = size_fixture(80, 24).await;
        let mut attachment = manager.attach(size_attachment("reader")).await.unwrap();
        let pty = backend.latest();
        // No await between them: all ten wait for the publisher.
        for read in 0..10 {
            pty.emit(&format!("[{read}]"));
        }
        let data = loop {
            if let TerminalEvent::Output { data, .. } = next_attachment_event(&mut attachment).await
            {
                break data;
            }
        };
        assert_eq!(data, "[0][1][2][3][4][5][6][7][8][9]");
        manager.shutdown().await;
    }

    /// Another terminal's burst can overflow the shared channel: this terminal missed nothing,
    /// so it gets no resync, which would reset its display and selection.
    #[tokio::test]
    async fn a_lag_caused_by_another_terminal_does_not_resync() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                event_capacity: 16,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "lag",
                "quiet",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let mut attachment = manager
            .attach(TerminalAttachInput::existing("lag", "quiet"))
            .await
            .unwrap();
        manager
            .open(TerminalOpenInput::new(
                "lag",
                "busy",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let busy = backend.latest();
        for line in 0..40 {
            busy.emit(&format!("line {line}\n"));
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            attachment.try_recv().await.is_none(),
            "nothing for the quiet terminal"
        );
        manager.shutdown().await;
    }

    /// A close lost to the lag still reaches the client, which would otherwise show the
    /// terminal running.
    #[tokio::test]
    async fn a_close_lost_to_the_lag_is_still_delivered() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                event_capacity: 16,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "lag",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let mut attachment = manager
            .attach(TerminalAttachInput::existing("lag", "term"))
            .await
            .unwrap();
        let pty = backend.latest();
        for line in 0..40 {
            pty.emit(&format!("line {line}\n"));
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        manager.close("lag", Some("term")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let item = tokio::time::timeout(Duration::from_secs(2), attachment.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            item,
            AttachmentItem::Event(TerminalEvent::Closed { .. })
        ));
        manager.shutdown().await;
    }

    /// After a restart the client saw, a lag caused by another terminal is still nothing to
    /// resync.
    #[tokio::test]
    async fn a_seen_restart_keeps_later_lags_from_resetting_input() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                event_capacity: 16,
                ..TerminalManagerOptions::default()
            },
        );
        let input = TerminalOpenInput::new("lag", "quiet", root.path().to_path_buf(), 80, 24);
        manager.open(input.clone()).await.unwrap();
        let mut attachment = manager
            .attach(TerminalAttachInput::existing("lag", "quiet"))
            .await
            .unwrap();
        manager.restart(input).await.unwrap();
        loop {
            if matches!(
                next_attachment_event(&mut attachment).await,
                TerminalEvent::Restarted { .. }
            ) {
                break;
            }
        }
        manager
            .open(TerminalOpenInput::new(
                "lag",
                "busy",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let busy = backend.latest();
        for line in 0..40 {
            busy.emit(&format!("line {line}\n"));
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            attachment.try_recv().await.is_none(),
            "nothing for the quiet terminal"
        );
        manager.shutdown().await;
    }

    /// A restart hidden in the lag is a new process: the resync must not keep the old one's
    /// pending input.
    #[tokio::test]
    async fn a_resync_across_a_missed_restart_is_a_new_process() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                event_capacity: 16,
                ..TerminalManagerOptions::default()
            },
        );
        let input = TerminalOpenInput::new("lag", "term", root.path().to_path_buf(), 80, 24);
        manager.open(input.clone()).await.unwrap();
        let mut attachment = manager
            .attach(TerminalAttachInput::existing("lag", "term"))
            .await
            .unwrap();
        manager.restart(input).await.unwrap();
        let pty = backend.latest();
        for line in 0..40 {
            pty.emit(&format!("line {line}\n"));
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let item = tokio::time::timeout(Duration::from_secs(2), attachment.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                item,
                AttachmentItem::Resync {
                    same_process: false,
                    ..
                }
            ),
            "the restart's event was lost to the lag, so the resync replaces the process"
        );
        manager.shutdown().await;
    }

    async fn wait_for_attachment_workers(manager: &TerminalManager, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.inner.attachment_tasks.len() != count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("attachment cleanup completed");
    }

    #[tokio::test]
    async fn terminal_resize_publishes_size_before_later_output_and_suppresses_noops() {
        let (_root, backend, manager) = size_fixture(151, 50).await;
        let mut attachment = manager.attach(size_attachment("owner")).await.unwrap();
        assert_eq!(
            attachment.initial.size,
            Some(TerminalSize {
                cols: 151,
                rows: 50,
                size_claim: Some("owner".to_owned())
            })
        );
        *backend.latest().output_on_resize.lock().unwrap() = Some("after resize".to_owned());
        manager
            .resize("sizing", "term", 91, 42, Some("owner".to_owned()))
            .await
            .unwrap();
        let resized = next_attachment_event(&mut attachment).await;
        assert!(
            matches!(&resized, TerminalEvent::Resized { size, .. } if size.cols == 91 && size.rows == 42 && size.size_claim.as_deref() == Some("owner"))
        );
        let output = next_attachment_event(&mut attachment).await;
        assert!(matches!(&output, TerminalEvent::Output { data, .. } if data == "after resize"));
        assert!(output.sequence() > resized.sequence());
        manager
            .resize("sizing", "term", 91, 42, Some("owner".to_owned()))
            .await
            .unwrap();
        backend.latest().emit("after noop");
        assert!(
            matches!(next_attachment_event(&mut attachment).await, TerminalEvent::Output { ref data, .. } if data == "after noop")
        );
        assert_eq!(*backend.latest().resizes.lock().unwrap(), [(91, 42)]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_open_and_attach_spawn_at_requested_size_without_resizing_existing_sessions() {
        let (root, backend, manager) = size_fixture(151, 50).await;
        let mut input = TerminalAttachInput::existing("sizing", "new");
        input.cwd = Some(root.path().to_path_buf());
        input.cols = Some(91);
        input.rows = Some(42);
        input.size_claim = Some("new-owner".to_owned());
        let attachment = manager.attach(input.clone()).await.unwrap();
        assert_eq!(
            (backend.spawns()[0].cols, backend.spawns()[0].rows),
            (151, 50)
        );
        assert_eq!(
            (backend.spawns()[1].cols, backend.spawns()[1].rows),
            (91, 42)
        );
        assert_eq!(
            attachment.initial.size.unwrap(),
            TerminalSize {
                cols: 91,
                rows: 42,
                size_claim: Some("new-owner".to_owned())
            }
        );
        input.terminal_id = "term".to_owned();
        let attached = manager.attach(input).await.unwrap();
        assert_eq!(attached.initial.size.unwrap().cols, 151);
        manager
            .open(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                70,
                20,
            ))
            .await
            .unwrap();
        let reopened = manager
            .attach(TerminalAttachInput::existing("sizing", "term"))
            .await
            .unwrap();
        assert_eq!(reopened.initial.size.unwrap().cols, 151);
        assert_eq!(backend.spawns().len(), 2);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_resize_claim_only_change_publishes_and_is_in_snapshot() {
        let (_root, backend, manager) = size_fixture(80, 24).await;
        let mut attachment = manager.attach(size_attachment("watcher")).await.unwrap();
        let _first = manager.attach(size_attachment("first")).await.unwrap();
        let _second = manager.attach(size_attachment("second")).await.unwrap();
        for claim in [Some("first".to_owned()), Some("second".to_owned()), None] {
            manager
                .resize("sizing", "term", 80, 24, claim.clone())
                .await
                .unwrap();
            assert!(
                matches!(next_attachment_event(&mut attachment).await, TerminalEvent::Resized { ref size, .. } if size.size_claim == claim && (size.cols, size.rows) == (80, 24))
            );
            let snapshot = manager
                .attach(TerminalAttachInput::existing("sizing", "term"))
                .await
                .unwrap()
                .initial;
            assert_eq!(
                snapshot.size.unwrap(),
                TerminalSize {
                    cols: 80,
                    rows: 24,
                    size_claim: claim.clone()
                }
            );
            manager
                .resize("sizing", "term", 80, 24, claim)
                .await
                .unwrap();
            backend.latest().emit("after noop");
            assert!(matches!(
                next_attachment_event(&mut attachment).await,
                TerminalEvent::Output { .. }
            ));
        }
        assert!(backend.latest().resizes.lock().unwrap().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_resize_failure_preserves_size_claim_and_stream() {
        let (_root, backend, manager) = size_fixture(80, 24).await;
        let mut attachment = manager.attach(size_attachment("owner")).await.unwrap();
        let _failed = manager.attach(size_attachment("failed")).await.unwrap();
        *backend.latest().resize_error.lock().unwrap() = Some("resize failed".to_owned());
        assert!(
            manager
                .resize("sizing", "term", 91, 42, Some("failed".to_owned()))
                .await
                .is_err()
        );
        let snapshot = manager
            .attach(TerminalAttachInput::existing("sizing", "term"))
            .await
            .unwrap()
            .initial;
        assert_eq!(
            snapshot.size.unwrap(),
            TerminalSize {
                cols: 80,
                rows: 24,
                size_claim: Some("owner".to_owned())
            }
        );
        backend.latest().emit("unchanged");
        assert!(
            matches!(next_attachment_event(&mut attachment).await, TerminalEvent::Output { ref data, .. } if data == "unchanged")
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_attachment_waiting_for_publication_does_not_block_another_terminal() {
        let (root, _backend, manager) = size_fixture(80, 24).await;
        let generation = manager
            .require_session("sizing", "term")
            .await
            .unwrap()
            .lock()
            .await
            .generation
            .clone();
        let publication = generation.publication.lock().await;
        let attaching = manager.attach(size_attachment("owner"));
        tokio::pin!(attaching);
        assert!(futures_util::poll!(&mut attaching).is_pending()); // attach's initial yield
        assert!(futures_util::poll!(&mut attaching).is_pending()); // publication is held
        let unrelated = tokio::time::timeout(
            Duration::from_secs(2),
            manager.open(TerminalOpenInput::new(
                "other",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            )),
        )
        .await;
        drop(publication);
        assert!(
            unrelated.is_ok(),
            "one terminal's publication blocked another terminal's open"
        );
        drop(attaching.await.unwrap());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_first_attachment_grant_is_used_once_without_surviving_attachments() {
        let (root, _backend, manager) = size_fixture(80, 24).await;
        let legacy = manager
            .attach(TerminalAttachInput::existing("sizing", "term"))
            .await
            .unwrap();
        assert!(!legacy.initial.first_attachment_grant);
        let (first, concurrent) = tokio::join!(
            manager.attach(size_attachment("first")),
            manager.attach(size_attachment("concurrent"))
        );
        let first = first.unwrap();
        let concurrent = concurrent.unwrap();
        let flags = [
            first.initial.first_attachment_grant,
            concurrent.initial.first_attachment_grant,
        ];
        assert_eq!(flags.into_iter().filter(|flag| *flag).count(), 1);
        let flagged = if flags[0] { &first } else { &concurrent };
        let claim = if flags[0] { "first" } else { "concurrent" };
        assert_eq!(
            flagged.initial.size.as_ref().unwrap().size_claim.as_deref(),
            Some(claim)
        );
        drop(first);
        drop(concurrent);
        // Regaining an unowned session must not renew an already used grant.
        let later = manager.attach(size_attachment("later")).await.unwrap();
        assert!(!later.initial.first_attachment_grant);
        drop(later);
        // Only the legacy stream survives. It did not negotiate reply ownership.
        let restarted = manager
            .restart(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        assert!(!restarted.first_attachment_grant);
        let after_restart = manager
            .attach(size_attachment("after-restart"))
            .await
            .unwrap();
        assert!(after_restart.initial.first_attachment_grant);
        let after_first = manager
            .attach(size_attachment("after-first"))
            .await
            .unwrap();
        assert!(!after_first.initial.first_attachment_grant);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_first_attachment_grant_remains_available_after_reopen_with_a_surviving_stream()
     {
        let (root, backend, manager) = size_fixture(80, 24).await;
        let mut survivor = manager.attach(size_attachment("survivor")).await.unwrap();
        backend.latest().exit(0);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    survivor.recv().await.expect("surviving attachment"),
                    AttachmentItem::Event(TerminalEvent::Exited { .. })
                ) {
                    break;
                }
            }
        })
        .await
        .expect("old process exited");

        let mut events = manager.subscribe_events();
        let reopened = manager
            .open(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        assert_eq!(reopened.status, TerminalStatus::Running);
        assert_ne!(reopened.pid, survivor.initial.pid);
        assert!(matches!(
            events.recv().await.unwrap(),
            TerminalEvent::Started { sequence: 1, .. }
        ));
        backend.latest().emit("\x1b[6n\x1b[c");
        assert!(matches!(
            events.recv().await.unwrap(),
            TerminalEvent::Output { .. }
        ));

        // The old stream remains registered, but Started does not reset its
        // sequence cursor. A new attachment must answer the startup history.
        let first = manager
            .attach(size_attachment("first-after-open"))
            .await
            .unwrap();
        assert!(first.initial.first_attachment_grant);
        assert_eq!(first.initial.history, "\x1b[6n\x1b[c");
        assert_eq!(
            first.initial.size.as_ref().unwrap().size_claim.as_deref(),
            Some("first-after-open")
        );
        let later = manager
            .attach(size_attachment("later-after-open"))
            .await
            .unwrap();
        assert!(!later.initial.first_attachment_grant);
        assert_eq!(later.initial.size, first.initial.size);
        drop(survivor);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_first_attachment_grant_is_used_when_claim_attachments_survive_restart() {
        let (root, _backend, manager) = size_fixture(80, 24).await;
        let survivor = manager.attach(size_attachment("survivor")).await.unwrap();
        manager
            .restart(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        // A surviving renderer can answer live queries even before reclaiming.
        let before_resize = manager
            .attach(size_attachment("before-resize"))
            .await
            .unwrap();
        assert!(!before_resize.initial.first_attachment_grant);
        manager
            .resize("sizing", "term", 91, 42, Some("survivor".to_owned()))
            .await
            .unwrap();
        let after_resize = manager
            .attach(size_attachment("after-resize"))
            .await
            .unwrap();
        assert!(!after_resize.initial.first_attachment_grant);
        assert_eq!(
            after_resize
                .initial
                .size
                .as_ref()
                .unwrap()
                .size_claim
                .as_deref(),
            Some("survivor")
        );
        assert_eq!(after_resize.initial.size.as_ref().unwrap().cols, 91);
        drop(survivor);
        drop(before_resize);
        drop(after_resize);
        let later = manager.attach(size_attachment("later")).await.unwrap();
        assert!(!later.initial.first_attachment_grant);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_first_attachment_grant_does_not_override_a_surviving_resize_owner() {
        let (root, _backend, manager) = size_fixture(80, 24).await;
        let _survivor = manager.attach(size_attachment("survivor")).await.unwrap();
        manager
            .restart(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        manager
            .resize("sizing", "term", 91, 42, Some("survivor".to_owned()))
            .await
            .unwrap();
        let later = manager.attach(size_attachment("later")).await.unwrap();
        assert_eq!(
            later.initial.size.as_ref().unwrap().size_claim.as_deref(),
            Some("survivor")
        );
        assert!(!later.initial.first_attachment_grant);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_attach_assigns_one_owner_atomically_before_startup_queries() {
        let (_root, backend, manager) = size_fixture(80, 24).await;
        let (first, second) = tokio::join!(
            manager.attach(size_attachment("first")),
            manager.attach(size_attachment("second"))
        );
        let mut first = first.unwrap();
        let second = second.unwrap();
        let owner = first
            .initial
            .size
            .as_ref()
            .unwrap()
            .size_claim
            .as_deref()
            .unwrap();
        assert!(matches!(owner, "first" | "second"));
        assert_eq!(first.initial.size, second.initial.size);
        assert!(backend.latest().resizes.lock().unwrap().is_empty());
        backend.latest().emit("\x1b[6n\x1b[c");
        assert!(
            matches!(next_attachment_event(&mut first).await, TerminalEvent::Output { ref data, .. } if data == "\x1b[6n\x1b[c")
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_owner_drop_publishes_null_but_mirror_drop_keeps_the_owner() {
        let (_root, backend, manager) = size_fixture(80, 24).await;
        let mut owner = manager.attach(size_attachment("owner")).await.unwrap();
        let mirror = manager.attach(size_attachment("mirror")).await.unwrap();
        drop(mirror);
        wait_for_attachment_workers(&manager, 1).await;
        backend.latest().emit("still owned");
        assert!(matches!(
            next_attachment_event(&mut owner).await,
            TerminalEvent::Output { .. }
        ));
        let mut mirror = manager.attach(size_attachment("mirror")).await.unwrap();
        drop(owner);
        assert!(
            matches!(next_attachment_event(&mut mirror).await, TerminalEvent::Resized { ref size, .. } if size.size_claim.is_none() && (size.cols, size.rows) == (80, 24))
        );
        manager
            .resize("sizing", "term", 91, 42, Some("mirror".to_owned()))
            .await
            .unwrap();
        assert!(
            matches!(next_attachment_event(&mut mirror).await, TerminalEvent::Resized { ref size, .. } if size.size_claim.as_deref() == Some("mirror"))
        );
        manager.shutdown().await;
        assert!(manager.inner.attachment_tasks.is_empty());
    }

    #[tokio::test]
    async fn terminal_departed_attachment_cannot_release_a_reused_claim_or_resize_late() {
        let (_root, backend, manager) = size_fixture(80, 24).await;
        let old = manager.attach(size_attachment("reconnect")).await.unwrap();
        let mut replacement = manager.attach(size_attachment("reconnect")).await.unwrap();
        drop(old);
        wait_for_attachment_workers(&manager, 1).await;
        backend.latest().emit("replacement owns");
        assert!(matches!(
            next_attachment_event(&mut replacement).await,
            TerminalEvent::Output { .. }
        ));
        let mut watcher = manager.attach(size_attachment("watcher")).await.unwrap();
        drop(replacement);
        assert!(
            matches!(next_attachment_event(&mut watcher).await, TerminalEvent::Resized { ref size, .. } if size.size_claim.is_none())
        );
        manager
            .resize("sizing", "term", 10, 10, Some("reconnect".to_owned()))
            .await
            .unwrap();
        backend.latest().emit("late resize ignored");
        assert!(matches!(
            next_attachment_event(&mut watcher).await,
            TerminalEvent::Output { .. }
        ));
        assert!(backend.latest().resizes.lock().unwrap().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_attachment_release_tracks_ownership_across_process_restart() {
        let (root, _backend, manager) = size_fixture(80, 24).await;
        let owner = manager.attach(size_attachment("owner")).await.unwrap();
        let mut watcher = manager.attach(size_attachment("watcher")).await.unwrap();
        manager
            .restart(TerminalOpenInput::new(
                "sizing",
                "term",
                root.path().to_path_buf(),
                91,
                42,
            ))
            .await
            .unwrap();
        assert!(
            matches!(next_attachment_event(&mut watcher).await, TerminalEvent::Restarted { ref snapshot, .. } if snapshot.size.as_ref().unwrap().size_claim.is_none())
        );
        manager
            .resize("sizing", "term", 91, 42, Some("owner".to_owned()))
            .await
            .unwrap();
        assert!(matches!(
            next_attachment_event(&mut watcher).await,
            TerminalEvent::Resized { .. }
        ));
        drop(owner);
        assert!(
            matches!(next_attachment_event(&mut watcher).await, TerminalEvent::Resized { ref size, .. } if size.size_claim.is_none())
        );
        manager.shutdown().await;
    }

    #[derive(Debug)]
    struct HistoryTestBackend {
        processes: std::sync::Mutex<Vec<Arc<HistoryTestPty>>>,
        spawns: std::sync::Mutex<Vec<PtySpawnInput>>,
        fail_spawns: bool,
        expose_process_identity: bool,
        exit_on_identity_read: Option<PtyExit>,
        exit_after_identity_read: Option<(Duration, PtyExit)>,
    }

    impl Default for HistoryTestBackend {
        fn default() -> Self {
            Self {
                processes: std::sync::Mutex::new(Vec::new()),
                spawns: std::sync::Mutex::new(Vec::new()),
                fail_spawns: false,
                expose_process_identity: true,
                exit_on_identity_read: None,
                exit_after_identity_read: None,
            }
        }
    }

    impl HistoryTestBackend {
        fn latest(&self) -> Arc<HistoryTestPty> {
            self.processes
                .lock()
                .expect("processes lock")
                .last()
                .cloned()
                .expect("spawned process")
        }

        fn spawns(&self) -> Vec<PtySpawnInput> {
            self.spawns.lock().expect("spawns lock").clone()
        }
    }

    impl PtyBackend for HistoryTestBackend {
        fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
            self.spawns.lock().expect("spawns lock").push(input.clone());
            if self.fail_spawns {
                return Err("provider spawn failed".to_owned());
            }
            let mut processes = self.processes.lock().expect("processes lock");
            let process = if self.expose_process_identity {
                HistoryTestPty::with_identity(processes.len() as u32 + 1)
            } else {
                let mut process = HistoryTestPty::new(processes.len() as u32 + 1);
                process.process_identity = None;
                process
            };
            *process
                .exit_on_identity_read
                .lock()
                .expect("exit-on-identity-read lock") = self.exit_on_identity_read.clone();
            *process
                .exit_after_identity_read
                .lock()
                .expect("exit-after-identity-read lock") = self.exit_after_identity_read.clone();
            let process = Arc::new(process);
            processes.push(process.clone());
            Ok(process)
        }
    }

    #[tokio::test]
    async fn process_exit_observer_is_not_lost_without_terminal_broadcast_subscribers() {
        let root = tempfile::tempdir().expect("terminal root");
        let worktree = root.path().join("worktree");
        std::fs::create_dir(&worktree).expect("worktree fixture");
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());
        let (observed, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
        manager.set_process_exit_callback(Arc::new(move |cwd| {
            let _ = observed.send(cwd);
            Box::pin(std::future::ready(()))
        }));
        let mut input = TerminalOpenInput::new(
            "thread",
            "structured-command",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.worktree_path = Some(worktree.clone());
        manager.open(input).await.expect("terminal starts");

        backend
            .latest()
            .exit
            .send(Some(PtyExit {
                exit_code: Some(0),
                signal: None,
            }))
            .expect("terminal exit publishes");

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), observed_rx.recv())
                .await
                .expect("process-exit observer deadline")
                .expect("process-exit observer remains installed"),
            worktree
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn retained_process_exit_callback_does_not_hold_terminal_publication() {
        let root = tempfile::tempdir().expect("terminal root");
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());
        let (callback_started, mut callback_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let release_callback = Arc::new(tokio::sync::Semaphore::new(0));
        manager.set_process_exit_callback(Arc::new({
            let release_callback = Arc::clone(&release_callback);
            move |_| {
                let _ = callback_started.send(());
                let release_callback = Arc::clone(&release_callback);
                Box::pin(async move {
                    release_callback
                        .acquire()
                        .await
                        .expect("process-exit callback release remains open")
                        .forget();
                })
            }
        }));
        let input = TerminalOpenInput::new(
            "thread",
            "retained-exit-callback",
            root.path().to_path_buf(),
            80,
            24,
        );
        manager.open(input.clone()).await.expect("terminal starts");
        let session = manager
            .require_session("thread", "retained-exit-callback")
            .await
            .expect("terminal session");
        let generation = session.lock().await.generation.clone();

        backend.latest().exit(0);
        callback_started_rx
            .recv()
            .await
            .expect("retained callback starts");
        let publication_available = generation.publication.try_lock().is_ok();
        if !publication_available {
            release_callback.add_permits(16);
            manager.shutdown().await;
            panic!("process-exit callback retained terminal publication ownership");
        }

        release_callback.add_permits(16);
        manager
            .restart(input)
            .await
            .expect("restart publishes while prior exit callback settles");
        assert_eq!(backend.processes.lock().expect("processes lock").len(), 2);
        manager.shutdown().await;
    }

    async fn ordered_write(
        manager: &TerminalManager,
        connection: uuid::Uuid,
        thread_id: &str,
        terminal_id: &str,
        input_id: &str,
        sequence: u64,
        data: &str,
    ) -> Result<crate::terminal::input::TerminalInputAcknowledgement, TerminalError> {
        manager
            .prepare_input_write(connection, thread_id, terminal_id, input_id, sequence, data)?
            .write()
            .await
    }

    async fn ordered_input_fixture() -> (
        tempfile::TempDir,
        Arc<HistoryTestBackend>,
        TerminalManager,
        uuid::Uuid,
        CancellationToken,
        String,
    ) {
        let root = tempfile::tempdir().expect("root");
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "thread",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("open");
        let connection = uuid::Uuid::new_v4();
        let closed = CancellationToken::new();
        let lease = manager
            .begin_input(connection, closed.clone(), "thread", "term", 0, None)
            .await
            .expect("lease");
        (root, backend, manager, connection, closed, lease.input_id)
    }

    #[tokio::test]
    async fn ordered_input_readmission_keeps_the_session_and_follows_the_new_admission() {
        let (_root, backend, manager, connection, closed, _input_id) =
            ordered_input_fixture().await;
        let admission = |lost: &CancellationToken| {
            Some(super::super::input::TerminalInputAdmission::new(
                lost.clone(),
                (),
                None,
            ))
        };
        let first_lost = CancellationToken::new();
        let lease = manager
            .begin_input(
                connection,
                closed,
                "thread",
                "term",
                1,
                admission(&first_lost),
            )
            .await
            .expect("admitted lease");
        let write = |sequence, data| {
            ordered_write(
                &manager,
                connection,
                "thread",
                "term",
                &lease.input_id,
                sequence,
                data,
            )
        };
        write(0, "a").await.expect("first frame");

        let epoch = manager.input_readmission_epoch();
        let (older, sessions) = manager
            .start_input_readmission(crate::terminal::InputReadmissionScope::Thread("thread"));
        assert_eq!(sessions, [(lease.input_id.clone(), "thread".to_owned())]);
        assert_ne!(manager.input_readmission_epoch(), epoch);
        let (newer, _) = manager
            .start_input_readmission(crate::terminal::InputReadmissionScope::Thread("thread"));
        assert!(
            manager
                .start_input_readmission(crate::terminal::InputReadmissionScope::Thread("other"))
                .1
                .is_empty()
        );

        // A frame waits while a round resolves the workspace it may be refused.
        let mut held = Box::pin(write(1, "b"));
        assert!(futures_util::poll!(&mut held).is_pending());
        let second_lost = CancellationToken::new();
        manager.readmit_input(&lease.input_id, newer, admission(&second_lost));
        held.await.expect("same session after re-admission");
        // An older round's late result neither replaces the newer admission nor seals.
        let stale_lost = CancellationToken::new();
        manager.readmit_input(&lease.input_id, older, admission(&stale_lost));
        manager.readmit_input(&lease.input_id, older, None);
        // Neither the replaced admission's loss nor the stale one concerns the session.
        first_lost.cancel();
        stale_lost.cancel();
        tokio::time::sleep(Duration::from_millis(20)).await;
        write(2, "c").await.expect("still admitted");
        assert_eq!(backend.latest().writes(), ["a", "b", "c"]);

        second_lost.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !manager
                .start_input_readmission(crate::terminal::InputReadmissionScope::Thread("thread"))
                .1
                .is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loss of the current admission seals the session");
        assert!(matches!(
            write(3, "d").await,
            Err(TerminalError::Input(error)) if error.code == "closed"
        ));
        assert_eq!(backend.latest().writes(), ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn ordered_input_project_root_rounds_cover_inheriting_and_resolving_sessions() {
        use crate::terminal::InputReadmissionScope;

        let (root, _backend, manager, connection, closed, _input_id) =
            ordered_input_fixture().await;
        let mut inputs = std::collections::BTreeMap::new();
        for (thread_id, origin) in [
            ("inherits", Some("project-a")),
            ("other-project", Some("project-b")),
            ("own-worktree", None),
            ("resolving", None),
        ] {
            manager
                .open(TerminalOpenInput::new(
                    thread_id,
                    "term",
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .expect("open");
            let lease = manager
                .begin_input(
                    connection,
                    closed.clone(),
                    thread_id,
                    "term",
                    0,
                    Some(super::super::input::TerminalInputAdmission::new(
                        CancellationToken::new(),
                        (),
                        origin.map(str::to_owned),
                    )),
                )
                .await
                .expect("admitted lease");
            inputs.insert(thread_id, lease.input_id);
        }
        // This session's workspace is being resolved again, so its origin is not known yet.
        manager.start_input_readmission(InputReadmissionScope::Thread("resolving"));

        let (_, sessions) =
            manager.start_input_readmission(InputReadmissionScope::InheritedRootOf("project-a"));
        let mut covered = sessions
            .into_iter()
            .map(|(input_id, thread_id)| {
                assert_eq!(inputs[thread_id.as_str()], input_id);
                thread_id
            })
            .collect::<Vec<_>>();
        covered.sort();
        assert_eq!(covered, ["inherits", "resolving"]);

        // A begin's catch-up is a round of its own session, which later project rounds cover.
        let own = &inputs["own-worktree"];
        assert_eq!(
            manager
                .start_input_readmission(InputReadmissionScope::Session(own))
                .1,
            [(own.clone(), "own-worktree".to_owned())]
        );
        let (_, sessions) =
            manager.start_input_readmission(InputReadmissionScope::InheritedRootOf("project-a"));
        assert!(sessions.iter().any(|(input_id, _)| input_id == own));
    }

    #[tokio::test]
    async fn ordered_input_admission_loss_stops_a_frame_before_the_cleanup_task_runs() {
        let (_root, backend, manager, connection, closed, _input_id) =
            ordered_input_fixture().await;
        let lost = CancellationToken::new();
        let lease = manager
            .begin_input(
                connection,
                closed,
                "thread",
                "term",
                1,
                Some(super::super::input::TerminalInputAdmission::new(
                    lost.clone(),
                    (),
                    None,
                )),
            )
            .await
            .expect("admitted lease");
        // On this current-thread runtime the cleanup task cannot run before the write.
        lost.cancel();
        assert!(matches!(
            ordered_write(&manager, connection, "thread", "term", &lease.input_id, 0, "late").await,
            Err(TerminalError::Input(error)) if error.code == "closed"
        ));
        assert!(backend.latest().writes().is_empty());
    }

    #[tokio::test]
    async fn ordered_input_delayed_old_begin_cannot_replace_a_newer_attachment() {
        let (_root, backend, manager, connection, closed, _input_id) =
            ordered_input_fixture().await;
        let old_begin = manager.begin_input(connection, closed.clone(), "thread", "term", 1, None);
        let fresh = manager
            .begin_input(connection, closed, "thread", "term", 2, None)
            .await
            .expect("fresh attachment");
        assert!(
            old_begin.await.is_err(),
            "late old issuance must not supersede fresh lease"
        );
        ordered_write(
            &manager,
            connection,
            "thread",
            "term",
            &fresh.input_id,
            0,
            "fresh input",
        )
        .await
        .expect("fresh lease remains valid");
        assert_eq!(backend.latest().writes(), ["fresh input"]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_reorders_arrivals_before_writing_the_real_pty() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        let later = ordered_write(&manager, connection, "thread", "term", &input_id, 1, "b");
        tokio::pin!(later);
        assert!(futures_util::poll!(&mut later).is_pending());
        assert!(backend.latest().writes().is_empty());
        let first = ordered_write(&manager, connection, "thread", "term", &input_id, 0, "a")
            .await
            .expect("first");
        assert_eq!(first.sequence, 0);
        assert_eq!(later.await.expect("later").sequence, 1);
        assert_eq!(backend.latest().writes(), ["a", "b"]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_failed_prefix_discards_waiting_suffix_and_cannot_replay() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        *backend.latest().write_error.lock().expect("write error") =
            Some("partial PTY write".into());
        let later = ordered_write(
            &manager, connection, "thread", "term", &input_id, 1, "suffix",
        );
        tokio::pin!(later);
        assert!(futures_util::poll!(&mut later).is_pending());
        assert!(
            ordered_write(
                &manager, connection, "thread", "term", &input_id, 0, "prefix"
            )
            .await
            .is_err()
        );
        assert!(later.await.is_err());
        assert!(
            ordered_write(
                &manager, connection, "thread", "term", &input_id, 0, "prefix"
            )
            .await
            .is_err()
        );
        assert_eq!(backend.latest().writes(), ["prefix"]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_rejects_foreign_connection_without_poisoning_owner() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        let other = uuid::Uuid::new_v4();
        assert!(
            ordered_write(&manager, other, "thread", "term", &input_id, 0, "foreign")
                .await
                .is_err()
        );
        assert!(
            manager
                .cancel_input(other, "thread", "term", &input_id)
                .is_err()
        );
        ordered_write(
            &manager, connection, "thread", "term", &input_id, 0, "owner",
        )
        .await
        .expect("owner");
        assert_eq!(backend.latest().writes(), ["owner"]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_disconnect_and_restart_reject_old_leases() {
        let (root, backend, manager, connection, closed, input_id) = ordered_input_fixture().await;
        closed.cancel();
        assert!(
            ordered_write(
                &manager,
                connection,
                "thread",
                "term",
                &input_id,
                0,
                "disconnected"
            )
            .await
            .is_err()
        );
        let connection = uuid::Uuid::new_v4();
        let lease = manager
            .begin_input(
                connection,
                CancellationToken::new(),
                "thread",
                "term",
                0,
                None,
            )
            .await
            .expect("new attachment");
        manager
            .restart(TerminalOpenInput::new(
                "thread",
                "term",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("restart");
        assert!(
            ordered_write(
                &manager,
                connection,
                "thread",
                "term",
                &lease.input_id,
                0,
                "old process"
            )
            .await
            .is_err()
        );
        assert!(backend.latest().writes().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn ordered_input_missing_sequence_expires_and_rejects_late_prefix() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        let later = ordered_write(
            &manager, connection, "thread", "term", &input_id, 1, "suffix",
        );
        tokio::pin!(later);
        assert!(futures_util::poll!(&mut later).is_pending());
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(later.await.is_err());
        assert!(
            ordered_write(&manager, connection, "thread", "term", &input_id, 0, "late")
                .await
                .is_err()
        );
        assert!(backend.latest().writes().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_disconnect_fences_a_frame_waiting_for_pty_publication() {
        let (_root, backend, manager, connection, closed, input_id) = ordered_input_fixture().await;
        let session = manager
            .require_session("thread", "term")
            .await
            .expect("session");
        let generation = session.lock().await.generation.clone();
        let publication = generation.publication.lock().await;
        let write = ordered_write(
            &manager, connection, "thread", "term", &input_id, 0, "unsent",
        );
        tokio::pin!(write);
        assert!(futures_util::poll!(&mut write).is_pending());
        closed.cancel();
        drop(publication);
        assert!(write.await.is_err());
        assert!(backend.latest().writes().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_utf8_byte_limit_preserves_legacy_writer() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        let exact = "😀".repeat(4096);
        ordered_write(&manager, connection, "thread", "term", &input_id, 0, &exact)
            .await
            .expect("exact byte cap");
        let oversized = "😀".repeat(4097);
        assert!(
            matches!(ordered_write(&manager, connection, "thread", "term", &input_id, 1, &oversized).await, Err(TerminalError::Input(error)) if error.code == "capacity")
        );
        assert!(
            ordered_write(
                &manager, connection, "thread", "term", &input_id, 2, "suffix"
            )
            .await
            .is_err()
        );
        manager
            .write("thread", "term", "legacy")
            .await
            .expect("legacy path");
        assert_eq!(backend.latest().writes(), [exact, "legacy".to_owned()]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_duplicate_cannot_be_delivered_twice() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        ordered_write(&manager, connection, "thread", "term", &input_id, 0, "once")
            .await
            .expect("first");
        assert!(
            ordered_write(&manager, connection, "thread", "term", &input_id, 0, "once")
                .await
                .is_err()
        );
        assert!(
            ordered_write(
                &manager, connection, "thread", "term", &input_id, 1, "suffix"
            )
            .await
            .is_err()
        );
        assert_eq!(backend.latest().writes(), ["once"]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn ordered_input_window_is_shared_across_terminals_but_not_connections() {
        let (root, backend, manager, connection, closed, first_id) = ordered_input_fixture().await;
        let first_process = backend.latest();
        manager
            .open(TerminalOpenInput::new(
                "thread",
                "other",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("other terminal");
        let second = manager
            .begin_input(connection, closed, "thread", "other", 0, None)
            .await
            .expect("second lease");
        let payload = "x".repeat(16 * 1024);
        let mut pending = Vec::new();
        for sequence in 1..=8 {
            for (terminal_id, input_id) in [
                ("term", first_id.as_str()),
                ("other", second.input_id.as_str()),
            ] {
                let mut write = Box::pin(ordered_write(
                    &manager,
                    connection,
                    "thread",
                    terminal_id,
                    input_id,
                    sequence,
                    &payload,
                ));
                assert!(futures_util::poll!(&mut write).is_pending());
                pending.push(write);
            }
        }
        assert!(
            matches!(ordered_write(&manager, connection, "thread", "term", &first_id, 9, "overflow").await, Err(TerminalError::Input(error)) if error.code == "capacity")
        );
        let independent_connection = uuid::Uuid::new_v4();
        let independent = manager
            .begin_input(
                independent_connection,
                CancellationToken::new(),
                "thread",
                "term",
                0,
                None,
            )
            .await
            .expect("independent lease");
        ordered_write(
            &manager,
            independent_connection,
            "thread",
            "term",
            &independent.input_id,
            0,
            "independent",
        )
        .await
        .expect("other connection is not blocked");
        assert_eq!(first_process.writes(), ["independent"]);
        assert!(backend.latest().writes().is_empty());
        drop(pending);
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn ordered_input_expired_gap_rejects_prefix_before_waiter_is_polled_again() {
        let (_root, backend, manager, connection, _closed, input_id) =
            ordered_input_fixture().await;
        let later = ordered_write(
            &manager, connection, "thread", "term", &input_id, 1, "suffix",
        );
        tokio::pin!(later);
        assert!(futures_util::poll!(&mut later).is_pending());
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(
            ordered_write(
                &manager,
                connection,
                "thread",
                "term",
                &input_id,
                0,
                "late prefix"
            )
            .await
            .is_err()
        );
        assert!(later.await.is_err());
        assert!(backend.latest().writes().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn open_with_initial_input_submits_setup_before_publishing_the_terminal() {
        let root = tempfile::tempdir().expect("terminal root");
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());

        manager
            .open_with_initial_input_and_publication_cancellation(
                TerminalOpenInput::new(
                    "thread",
                    "setup-install",
                    root.path().to_path_buf(),
                    120,
                    30,
                ),
                "vp install\r".to_owned(),
                CancellationToken::new(),
            )
            .await
            .expect("terminal opens only after setup input is submitted");

        assert_eq!(backend.latest().writes(), ["vp install\r"]);
        assert_eq!(manager.subscribe_metadata().await.initial.len(), 1);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn setup_publication_cancellation_kills_the_uncommitted_process() {
        let root = tempfile::tempdir().expect("terminal root");
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());
        let generation = manager
            .inner
            .generations
            .current(&("thread".to_owned(), "setup-cancelled".to_owned()));
        let publication = generation.publication.lock().await;
        let cancellation = CancellationToken::new();
        let open_manager = manager.clone();
        let open_cancellation = cancellation.clone();
        let cwd = root.path().to_path_buf();
        let open = tokio::spawn(async move {
            open_manager
                .open_with_initial_input_and_publication_cancellation(
                    TerminalOpenInput::new("thread", "setup-cancelled", cwd, 120, 30),
                    "vp install\r".to_owned(),
                    open_cancellation,
                )
                .await
        });
        while backend.processes.lock().expect("processes").is_empty() {
            tokio::task::yield_now().await;
        }

        cancellation.cancel();
        drop(publication);
        assert!(matches!(
            open.await.expect("open task"),
            Err(TerminalError::PublicationCancelled)
        ));
        let process = backend
            .processes
            .lock()
            .expect("processes")
            .first()
            .cloned()
            .expect("spawned process");
        assert!(process.is_killed());
        assert!(
            manager
                .require_session("thread", "setup-cancelled")
                .await
                .is_err()
        );
    }

    #[derive(Debug)]
    struct BlockingSpawnBackend {
        process: Arc<HistoryTestPty>,
        started: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl PtyBackend for BlockingSpawnBackend {
        fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
            if let Some(started) = self.started.lock().expect("started lock").take() {
                started.send(()).expect("spawn-started receiver");
            }
            self.release
                .lock()
                .expect("release lock")
                .recv_timeout(Duration::from_secs(2))
                .expect("spawn release");
            Ok(self.process.clone())
        }
    }

    #[derive(Debug)]
    struct FirstSpawnBlockingBackend {
        processes: std::sync::Mutex<Vec<Arc<HistoryTestPty>>>,
        spawn_count: std::sync::atomic::AtomicUsize,
        first_started: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
        first_release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        second_spawned: tokio::sync::Semaphore,
    }

    impl PtyBackend for FirstSpawnBlockingBackend {
        fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
            let spawn_index = self
                .spawn_count
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if spawn_index == 0 {
                if let Some(started) = self.first_started.lock().expect("started lock").take() {
                    started.send(()).expect("first-spawn receiver");
                }
                self.first_release
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(2))
                    .expect("first-spawn release");
            } else {
                self.second_spawned.add_permits(1);
            }
            let process = Arc::new(HistoryTestPty::new(spawn_index as u32 + 1));
            self.processes
                .lock()
                .expect("processes lock")
                .push(process.clone());
            Ok(process)
        }
    }

    #[derive(Debug)]
    struct ReturningSpawnBackend {
        process: Arc<HistoryTestPty>,
        spawned: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    impl PtyBackend for ReturningSpawnBackend {
        fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
            if let Some(spawned) = self.spawned.lock().expect("spawned lock").take() {
                spawned.send(()).expect("spawned receiver");
            }
            Ok(self.process.clone())
        }
    }

    #[derive(Debug)]
    struct ControllableSubprocessInspector {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        inspection: SubprocessInspection,
    }

    impl TerminalSubprocessInspector for ControllableSubprocessInspector {
        fn inspect(
            &self,
            _terminal_pid: u32,
        ) -> Pin<Box<dyn Future<Output = Result<SubprocessInspection, String>> + Send + '_>>
        {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(self.inspection.clone())
            })
        }
    }

    #[tokio::test]
    async fn close_waits_for_the_terminal_process_to_exit() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());

        manager
            .open(TerminalOpenInput::new(
                "thread-close",
                "term-close",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let process = backend.latest();
        process.delay_exit_on_kill();
        let close_manager = manager.clone();
        let close = tokio::spawn(async move {
            close_manager
                .close("thread-close", Some("term-close"))
                .await
                .unwrap();
        });

        while !process.is_killed() {
            tokio::task::yield_now().await;
        }
        assert!(
            !close.is_finished(),
            "close returned before the killed process released its resources"
        );

        process.exit(0);
        tokio::time::timeout(Duration::from_secs(2), close)
            .await
            .expect("terminal close timed out")
            .expect("terminal close task");
    }

    #[tokio::test]
    async fn close_waits_for_root_reaping_after_the_process_tree_exits() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());

        manager
            .open(TerminalOpenInput::new(
                "thread-tree-reaping",
                "term-tree-reaping",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let process = backend.latest();
        process.delay_exit_on_kill();
        process.report_process_tree_exited();
        let close_manager = manager.clone();
        let close = tokio::spawn(async move {
            close_manager
                .close("thread-tree-reaping", Some("term-tree-reaping"))
                .await
                .unwrap();
        });

        while !process.is_killed() {
            tokio::task::yield_now().await;
        }
        assert!(
            !close.is_finished(),
            "close returned after the process tree exited but before root reaping completed"
        );

        process.exit(0);
        tokio::time::timeout(Duration::from_secs(2), close)
            .await
            .expect("terminal close timed out")
            .expect("terminal close task");
    }

    #[tokio::test]
    async fn close_fails_while_the_terminal_process_tree_is_still_running() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());

        manager
            .open(TerminalOpenInput::new(
                "thread-tree-close",
                "term-tree-close",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        backend.latest().keep_process_tree_running();

        assert!(matches!(
            manager
                .close("thread-tree-close", Some("term-tree-close"))
                .await,
            Err(TerminalError::Close)
        ));
    }

    #[tokio::test]
    async fn close_during_in_flight_subprocess_inspection_does_not_resurrect_metadata() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let inspector = Arc::new(ControllableSubprocessInspector {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            inspection: SubprocessInspection {
                has_running_subprocess: true,
                child_command_label: Some("codex".to_owned()),
                process_ids: vec![1, 2],
            },
        });
        let manager = TerminalManager::new(
            backend,
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::from_millis(1),
                subprocess_inspector: Some(inspector.clone()),
                ..TerminalManagerOptions::default()
            },
        );
        let mut events = manager.subscribe_events();
        let mut metadata = manager.subscribe_metadata().await;

        manager
            .open(TerminalOpenInput::new(
                "thread-race",
                "term-race",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), inspector.started.notified())
            .await
            .expect("subprocess inspection did not start");
        let activity_completed = manager
            .require_session("thread-race", "term-race")
            .await
            .unwrap()
            .lock()
            .await
            .generation
            .activity_completed
            .clone();

        manager
            .close("thread-race", Some("term-race"))
            .await
            .unwrap();

        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .expect("closed event timeout")
                .expect("terminal event sender");
            if matches!(
                event,
                TerminalEvent::Closed {
                    ref thread_id,
                    ref terminal_id,
                    ..
                } if thread_id == "thread-race" && terminal_id == "term-race"
            ) {
                break;
            }
        }
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), metadata.recv())
                .await
                .expect("metadata remove timeout")
                .expect("metadata event sender");
            if matches!(
                event,
                TerminalMetadataEvent::Remove {
                    ref thread_id,
                    ref terminal_id,
                    ..
                } if thread_id == "thread-race" && terminal_id == "term-race"
            ) {
                break;
            }
        }

        inspector.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), activity_completed.cancelled())
            .await
            .expect("activity supervisor did not complete after inspection release");
        let event_after_close = events.try_recv();
        let metadata_after_close = metadata.events.try_recv();
        assert!(
            matches!(
                event_after_close,
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "terminal event emitted after close: {event_after_close:?}"
        );
        assert!(
            matches!(
                metadata_after_close,
                Err(broadcast::error::TryRecvError::Empty)
            ),
            "terminal metadata emitted after close: {metadata_after_close:?}"
        );
    }

    #[tokio::test]
    async fn structured_command_spawns_once_with_exact_program_args_cwd_and_env() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-provider",
            "term-provider",
            root.path().to_path_buf(),
            120,
            30,
        );
        input.env.insert("BIBCODE_TEST".to_owned(), "1".to_owned());
        input.command = Some(crate::terminal::TerminalLaunchCommand {
            executable: "/opt/Provider CLI/codex".to_owned(),
            args: vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()],
            label: Some("Codex Terminal".to_owned()),
            activity: None,
            env: None,
        });

        let first = manager.open(input.clone()).await.unwrap();
        let second = manager.open(input).await.unwrap();

        assert_eq!(first.pid, second.pid);
        assert_eq!(first.label, "Codex Terminal");
        let spawns = backend.spawns();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].executable, "/opt/Provider CLI/codex");
        assert_eq!(
            spawns[0].args,
            vec!["--dangerously-bypass-approvals-and-sandbox"]
        );
        assert_eq!(spawns[0].cwd, root.path());
        assert_eq!(
            spawns[0].env.get("BIBCODE_TEST").map(String::as_str),
            Some("1")
        );
    }

    #[tokio::test]
    async fn attach_creates_a_missing_structured_command_without_shell_fallback() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let attachment = manager
            .attach(TerminalAttachInput {
                center_panel: false,
                size_claim: None,
                thread_id: "thread-provider".to_owned(),
                terminal_id: "term-provider".to_owned(),
                cwd: Some(root.path().to_path_buf()),
                worktree_path: Some(root.path().to_path_buf()),
                cols: Some(90),
                rows: Some(28),
                env: std::collections::BTreeMap::new(),
                restart_if_not_running: false,
                command: Some(crate::terminal::TerminalLaunchCommand {
                    executable: "claude".to_owned(),
                    args: vec!["--dangerously-skip-permissions".to_owned()],
                    label: Some("Claude Terminal".to_owned()),
                    activity: None,
                    env: None,
                }),
            })
            .await
            .unwrap();

        assert_eq!(attachment.initial.label, "Claude Terminal");
        assert_eq!(backend.spawns().len(), 1);
        assert_eq!(backend.spawns()[0].executable, "claude");
    }

    #[tokio::test]
    async fn close_invalidates_an_older_missing_session_attach_before_creation() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut metadata = manager.subscribe_metadata().await;
        let sessions_guard = manager.inner.sessions.write().await;

        let attach_started = Arc::new(tokio::sync::Notify::new());
        let attach_manager = manager.clone();
        let attach_root = root.path().to_path_buf();
        let attach_started_task = attach_started.clone();
        let attach_task = tokio::spawn(async move {
            attach_started_task.notify_one();
            attach_manager
                .attach(TerminalAttachInput {
                    center_panel: false,
                    size_claim: None,
                    thread_id: "thread-attach-close".to_owned(),
                    terminal_id: "term-attach-close".to_owned(),
                    cwd: Some(attach_root.clone()),
                    worktree_path: Some(attach_root),
                    cols: Some(80),
                    rows: Some(24),
                    env: std::collections::BTreeMap::new(),
                    restart_if_not_running: false,
                    command: Some(crate::terminal::TerminalLaunchCommand {
                        executable: "codex".to_owned(),
                        args: vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()],
                        label: Some("Codex Terminal".to_owned()),
                        activity: None,
                        env: None,
                    }),
                })
                .await
        });
        attach_started.notified().await;

        let close_started = Arc::new(tokio::sync::Notify::new());
        let close_manager = manager.clone();
        let close_started_task = close_started.clone();
        let close_task = tokio::spawn(async move {
            close_started_task.notify_one();
            close_manager
                .close("thread-attach-close", Some("term-attach-close"))
                .await
                .unwrap();
        });
        close_started.notified().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager.inner.lifecycle.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close did not acquire the lifecycle lock");

        drop(sessions_guard);
        close_task.await.expect("close task");
        let attach_result = attach_task.await.expect("attach task");

        assert!(
            matches!(attach_result, Err(TerminalError::NotFound { .. })),
            "older attach unexpectedly created a session"
        );
        assert!(backend.spawns().is_empty(), "invalidated attach spawned");
        assert!(
            manager
                .require_session("thread-attach-close", "term-attach-close")
                .await
                .is_err(),
            "invalidated attach registered a session"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), metadata.recv())
                .await
                .is_err(),
            "invalidated attach published terminal metadata"
        );

        let launched = manager
            .open(TerminalOpenInput::new(
                "thread-attach-close",
                "term-attach-close",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("a deliberate later launch must remain valid");
        assert_eq!(launched.status, TerminalStatus::Running);
        assert_eq!(backend.spawns().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_during_spawn_kills_the_invalidated_process_without_registering_it() {
        let root = tempfile::tempdir().unwrap();
        let process = Arc::new(HistoryTestPty::new(41));
        let (spawn_started, spawn_started_rx) = std::sync::mpsc::channel();
        let (spawn_release, spawn_release_rx) = std::sync::mpsc::channel();
        let manager = TerminalManager::new(
            Arc::new(BlockingSpawnBackend {
                process: process.clone(),
                started: std::sync::Mutex::new(Some(spawn_started)),
                release: std::sync::Mutex::new(spawn_release_rx),
            }),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut metadata = manager.subscribe_metadata().await;
        let generation = manager.inner.generations.current(&(
            "thread-spawn-close".to_owned(),
            "term-spawn-close".to_owned(),
        ));
        let attach_manager = manager.clone();
        let attach_root = root.path().to_path_buf();
        let attach_task = tokio::spawn(async move {
            attach_manager
                .attach(TerminalAttachInput {
                    center_panel: false,
                    size_claim: None,
                    thread_id: "thread-spawn-close".to_owned(),
                    terminal_id: "term-spawn-close".to_owned(),
                    cwd: Some(attach_root),
                    worktree_path: None,
                    cols: Some(80),
                    rows: Some(24),
                    env: std::collections::BTreeMap::new(),
                    restart_if_not_running: false,
                    command: None,
                })
                .await
        });
        tokio::task::spawn_blocking(move || {
            spawn_started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("spawn did not start");
        })
        .await
        .expect("spawn-start wait");

        let close_manager = manager.clone();
        let close_task = tokio::spawn(async move {
            close_manager
                .close("thread-spawn-close", Some("term-spawn-close"))
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !generation.is_invalidated() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("close did not invalidate the in-flight generation");
        spawn_release.send(()).expect("release spawn");

        let attach_result = attach_task.await.expect("attach task");
        close_task.await.expect("close task");
        assert!(
            matches!(attach_result, Err(TerminalError::NotFound { .. })),
            "invalidated in-flight spawn unexpectedly attached"
        );
        assert!(process.is_killed(), "invalidated process was not killed");
        assert!(
            manager
                .require_session("thread-spawn-close", "term-spawn-close")
                .await
                .is_err(),
            "invalidated process was registered"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), metadata.recv())
                .await
                .is_err(),
            "invalidated process published terminal metadata"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worktree_removal_fence_invalidates_inflight_spawn_and_rejects_late_reopen() {
        let root = tempfile::tempdir().unwrap();
        let (first_started, first_started_rx) = std::sync::mpsc::channel();
        let (first_release, first_release_rx) = std::sync::mpsc::channel();
        let backend = Arc::new(FirstSpawnBlockingBackend {
            processes: std::sync::Mutex::new(Vec::new()),
            spawn_count: std::sync::atomic::AtomicUsize::new(0),
            first_started: std::sync::Mutex::new(Some(first_started)),
            first_release: std::sync::Mutex::new(first_release_rx),
            second_spawned: tokio::sync::Semaphore::new(0),
        });
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let opening_manager = manager.clone();
        let opening_root = root.path().to_path_buf();
        let opening = tokio::spawn(async move {
            opening_manager
                .open(TerminalOpenInput::new(
                    "thread-worktree-removal",
                    "terminal-inflight",
                    opening_root,
                    80,
                    24,
                ))
                .await
        });
        tokio::task::spawn_blocking(move || {
            first_started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("first spawn did not start");
        })
        .await
        .expect("spawn-start wait");

        let removal = manager
            .begin_worktree_removal(&["unrelated-client-thread".to_owned()], root.path())
            .await
            .expect("worktree removal fence");
        let late_open = manager
            .open(TerminalOpenInput::new(
                "another-unlisted-thread",
                "terminal-late",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await;
        assert!(
            matches!(
                late_open,
                Err(TerminalError::WorktreeRemovalInProgress { .. })
            ),
            "late terminal launch bypassed the worktree-removal fence"
        );

        first_release.send(()).expect("release first spawn");
        assert!(
            matches!(
                opening.await.expect("opening task"),
                Err(TerminalError::NotFound { .. })
                    | Err(TerminalError::WorktreeRemovalInProgress { .. })
            ),
            "in-flight terminal launch survived the path-scoped removal fence"
        );
        assert!(
            backend
                .processes
                .lock()
                .expect("processes lock")
                .first()
                .is_some_and(|process| process.is_killed()),
            "invalidated terminal process was not killed"
        );

        drop(removal);
        manager
            .open(TerminalOpenInput::new(
                "thread-worktree-removal",
                "terminal-after-failure",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("dropping a failed-removal fence permits an explicit later launch");
        assert_eq!(
            backend
                .spawn_count
                .load(std::sync::atomic::Ordering::Acquire),
            2
        );
    }

    #[tokio::test]
    async fn worktree_removal_path_fence_rejects_late_attach_and_restart_without_client_ids() {
        let root = tempfile::tempdir().unwrap();
        let manager = TerminalManager::default();
        manager
            .open(TerminalOpenInput::new(
                "server-known-thread",
                "terminal-existing",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("initial terminal");

        let removal = manager
            .begin_worktree_removal(&["unrelated-client-thread".to_owned()], root.path())
            .await
            .expect("path-scoped worktree removal fence");

        let attach = manager
            .attach(TerminalAttachInput {
                center_panel: false,
                size_claim: None,
                thread_id: "server-known-thread".to_owned(),
                terminal_id: "terminal-existing".to_owned(),
                cwd: None,
                worktree_path: None,
                cols: None,
                rows: None,
                env: std::collections::BTreeMap::new(),
                restart_if_not_running: false,
                command: None,
            })
            .await;
        assert!(matches!(
            attach,
            Err(TerminalError::WorktreeRemovalInProgress { .. })
        ));

        let restart = manager
            .restart(TerminalOpenInput::new(
                "different-thread-id",
                "terminal-restart",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await;
        assert!(matches!(
            restart,
            Err(TerminalError::WorktreeRemovalInProgress { .. })
        ));

        drop(removal);
    }

    #[test]
    fn worktree_removal_registry_collapses_parent_directory_aliases() {
        let root = tempfile::tempdir().expect("fixture root");
        let worktree = root.path().join("worktree");
        let child = worktree.join("child");
        std::fs::create_dir_all(&child).expect("worktree fixture");
        let aliased_worktree = child.join("..");
        let registry = Arc::new(WorktreeRemovalRegistry::default());
        let _guard = registry
            .acquire(&[], &worktree)
            .expect("worktree removal fence");

        assert!(matches!(
            registry.ensure_available("unlisted-thread", Some(&aliased_worktree), None),
            Err(TerminalError::WorktreeRemovalInProgress { .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn worktree_removal_registry_resolves_windows_junction_aliases() {
        let root = tempfile::tempdir().expect("fixture root");
        let worktree = root.path().join("worktree");
        let alias = root.path().join("worktree-alias");
        std::fs::create_dir(&worktree).expect("worktree fixture");
        if let Err(error) = junction::create(&worktree, &alias) {
            eprintln!("skipping junction assertion: Windows denied junction creation: {error}");
            return;
        }
        let registry = Arc::new(WorktreeRemovalRegistry::default());
        let guard = registry
            .acquire(&[], &worktree)
            .expect("worktree removal fence");

        assert!(matches!(
            registry.ensure_available("unlisted-thread", Some(&alias), None),
            Err(TerminalError::WorktreeRemovalInProgress { .. })
        ));

        drop(guard);
        junction::delete(&alias).expect("remove fixture junction");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborting_open_during_spawn_reaps_the_late_process_after_shutdown() {
        let root = tempfile::tempdir().expect("terminal root");
        let process = Arc::new(HistoryTestPty::new(43));
        let mut exit = process.subscribe_exit();
        let (spawn_started, spawn_started_rx) = std::sync::mpsc::channel();
        let (spawn_release, spawn_release_rx) = std::sync::mpsc::channel();
        let manager = TerminalManager::new(
            Arc::new(BlockingSpawnBackend {
                process: process.clone(),
                started: std::sync::Mutex::new(Some(spawn_started)),
                release: std::sync::Mutex::new(spawn_release_rx),
            }),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut metadata = manager.subscribe_metadata().await;
        let open_manager = manager.clone();
        let mut open = tokio::spawn(async move {
            open_manager
                .open(TerminalOpenInput::new(
                    "thread-aborted-spawn",
                    "term-aborted-spawn",
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
        });
        tokio::task::spawn_blocking(move || {
            spawn_started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("spawn started");
        })
        .await
        .expect("spawn waiter");

        open.abort();
        let aborted_before_release =
            tokio::time::timeout(Duration::from_millis(250), &mut open).await;
        // Shutdown and loss of the async caller must not strand the blocking
        // task's eventual process, even when the manager has already gone away.
        manager.shutdown().await;
        assert!(manager.subscribe_metadata().await.initial.is_empty());
        drop(manager);
        spawn_release.send(()).expect("release late spawn");
        if aborted_before_release.is_err() {
            let _ = open.await;
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while exit.borrow().is_none() {
                exit.changed().await.expect("late process exit");
            }
        })
        .await
        .expect("late process was not reaped");
        assert!(process.is_killed(), "late process was not killed");
        assert!(
            !matches!(
                tokio::time::timeout(Duration::from_millis(50), metadata.recv()).await,
                Ok(Some(_))
            ),
            "aborted spawn published terminal metadata"
        );
        assert!(
            aborted_before_release
                .expect("open cancellation must finish while PTY spawn is blocked")
                .expect_err("aborted open")
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn aborting_open_after_spawn_before_registration_kills_the_unowned_process() {
        let root = tempfile::tempdir().unwrap();
        let process = Arc::new(HistoryTestPty::new(42));
        let (spawned, spawned_rx) = tokio::sync::oneshot::channel();
        let manager = TerminalManager::new(
            Arc::new(ReturningSpawnBackend {
                process: process.clone(),
                spawned: std::sync::Mutex::new(Some(spawned)),
            }),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut metadata = manager.subscribe_metadata().await;
        let generation = manager.inner.generations.current(&(
            "thread-aborted-open".to_owned(),
            "term-aborted-open".to_owned(),
        ));
        let publication = generation.publication.lock().await;

        let open_manager = manager.clone();
        let open_task = tokio::spawn(async move {
            open_manager
                .open(TerminalOpenInput::new(
                    "thread-aborted-open",
                    "term-aborted-open",
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), spawned_rx)
            .await
            .expect("spawn did not succeed")
            .expect("spawned sender");
        // The backend's signal precedes the blocking-task handoff. Wait until
        // open owns the result and is blocked on publication before aborting.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager.inner.lifecycle.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("open reached session publication");

        open_task.abort();
        let join_error = open_task
            .await
            .expect_err("aborted open unexpectedly completed");
        assert!(join_error.is_cancelled(), "open task was not cancelled");
        drop(publication);

        assert!(process.is_killed(), "abandoned process was not killed");
        assert!(
            manager
                .require_session("thread-aborted-open", "term-aborted-open")
                .await
                .is_err(),
            "aborted open registered a session"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), metadata.recv())
                .await
                .is_err(),
            "aborted open published terminal metadata"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn hardening_same_key_concurrent_open_spawns_exactly_one_process() {
        let root = tempfile::tempdir().expect("temp dir");
        let (first_started, first_started_rx) = std::sync::mpsc::channel();
        let (first_release, first_release_rx) = std::sync::mpsc::channel();
        let backend = Arc::new(FirstSpawnBlockingBackend {
            processes: std::sync::Mutex::new(Vec::new()),
            spawn_count: std::sync::atomic::AtomicUsize::new(0),
            first_started: std::sync::Mutex::new(Some(first_started)),
            first_release: std::sync::Mutex::new(first_release_rx),
            second_spawned: tokio::sync::Semaphore::new(0),
        });
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let input = TerminalOpenInput::new(
            "thread-concurrent-open",
            "terminal-concurrent-open",
            root.path().to_path_buf(),
            80,
            24,
        );
        let first_manager = manager.clone();
        let first_input = input.clone();
        let first = tokio::spawn(async move { first_manager.open(first_input).await });
        tokio::task::spawn_blocking(move || {
            first_started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("first spawn did not start");
        })
        .await
        .expect("first-spawn wait");

        let second_manager = manager.clone();
        let second = tokio::spawn(async move { second_manager.open(input).await });
        let spawned_twice =
            tokio::time::timeout(Duration::from_millis(250), backend.second_spawned.acquire())
                .await
                .is_ok();
        first_release.send(()).expect("release first spawn");
        first
            .await
            .expect("first open task")
            .expect("first open result");
        second
            .await
            .expect("second open task")
            .expect("second open result");
        manager.shutdown().await;

        assert!(!spawned_twice, "same-key concurrent open spawned twice");
        assert_eq!(
            backend
                .spawn_count
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn hardening_aborted_spawn_holds_key_until_late_process_cleanup() {
        for restart in [false, true] {
            let root = tempfile::tempdir().expect("temp dir");
            let (first_started, first_started_rx) = std::sync::mpsc::channel();
            let (first_release, first_release_rx) = std::sync::mpsc::channel();
            let backend = Arc::new(FirstSpawnBlockingBackend {
                processes: std::sync::Mutex::new(Vec::new()),
                spawn_count: std::sync::atomic::AtomicUsize::new(0),
                first_started: std::sync::Mutex::new(Some(first_started)),
                first_release: std::sync::Mutex::new(first_release_rx),
                second_spawned: tokio::sync::Semaphore::new(0),
            });
            let manager = TerminalManager::new(
                backend.clone(),
                TerminalManagerOptions {
                    subprocess_poll_interval: Duration::ZERO,
                    ..TerminalManagerOptions::default()
                },
            );
            let input = TerminalOpenInput::new(
                "thread-aborted-spawn",
                "terminal-aborted-spawn",
                root.path().to_path_buf(),
                80,
                24,
            );
            let first_manager = manager.clone();
            let first_input = input.clone();
            let first = tokio::spawn(async move { first_manager.open(first_input).await });
            tokio::task::spawn_blocking(move || {
                first_started_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("first spawn started");
            })
            .await
            .expect("first spawn waiter");
            first.abort();
            assert!(first.await.expect_err("aborted open").is_cancelled());

            let replacement_manager = manager.clone();
            let replacement = tokio::spawn(async move {
                if restart {
                    replacement_manager.restart(input).await
                } else {
                    replacement_manager.open(input).await
                }
            });
            let overlapping_spawn =
                tokio::time::timeout(Duration::from_millis(250), backend.second_spawned.acquire())
                    .await
                    .is_ok();
            first_release.send(()).expect("release abandoned spawn");
            replacement
                .await
                .expect("replacement task")
                .expect("replacement terminal");
            let old_process_killed = backend
                .processes
                .lock()
                .expect("processes lock")
                .iter()
                .find(|process| process.pid() == 1)
                .expect("abandoned process")
                .is_killed();
            manager.shutdown().await;
            assert!(!overlapping_spawn, "same-key spawn overlapped after abort");
            assert!(old_process_killed, "abandoned process was not killed");
            assert_eq!(
                backend
                    .spawn_count
                    .load(std::sync::atomic::Ordering::Acquire),
                2
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn hardening_same_key_restart_serializes_exact_cleanup_through_replacement_start() {
        let root = tempfile::tempdir().expect("temp dir");
        let (first_started, first_started_rx) = std::sync::mpsc::channel();
        let (first_release, first_release_rx) = std::sync::mpsc::channel();
        let backend = Arc::new(FirstSpawnBlockingBackend {
            processes: std::sync::Mutex::new(Vec::new()),
            spawn_count: std::sync::atomic::AtomicUsize::new(0),
            first_started: std::sync::Mutex::new(Some(first_started)),
            first_release: std::sync::Mutex::new(first_release_rx),
            second_spawned: tokio::sync::Semaphore::new(0),
        });
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let input = TerminalOpenInput::new(
            "thread-concurrent-restart",
            "terminal-concurrent-restart",
            root.path().to_path_buf(),
            80,
            24,
        );
        let initial_manager = manager.clone();
        let initial_input = input.clone();
        let initial = tokio::spawn(async move { initial_manager.open(initial_input).await });
        tokio::task::spawn_blocking(move || {
            first_started_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("initial spawn did not start");
        })
        .await
        .expect("initial-spawn wait");
        first_release.send(()).expect("release initial spawn");
        initial
            .await
            .expect("initial open task")
            .expect("initial open");

        let barrier = Arc::new(PublisherBarrier {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *manager
            .inner
            .restart_after_exact_cleanup_barrier
            .lock()
            .expect("restart barrier lock") = Some(barrier.clone());
        let first_restart_manager = manager.clone();
        let first_restart_input = input.clone();
        let first_restart =
            tokio::spawn(async move { first_restart_manager.restart(first_restart_input).await });
        tokio::time::timeout(Duration::from_secs(2), barrier.started.notified())
            .await
            .expect("first restart did not reach exact-cleanup barrier");

        let second_restart_manager = manager.clone();
        let second_restart =
            tokio::spawn(async move { second_restart_manager.restart(input).await });
        let spawned_during_stale_cleanup =
            tokio::time::timeout(Duration::from_millis(250), backend.second_spawned.acquire())
                .await
                .is_ok();
        barrier.release.notify_one();
        let first_result = first_restart.await.expect("first restart task");
        let second_result = second_restart.await.expect("second restart task");
        let latest_process = backend
            .processes
            .lock()
            .expect("processes lock")
            .last()
            .cloned()
            .expect("latest process");

        assert!(
            !spawned_during_stale_cleanup,
            "same-key replacement spawned while the prior operation still owned exact cleanup"
        );
        assert!(
            matches!(first_result, Err(TerminalError::NotFound { .. })),
            "the newer same-key restart must supersede the queued replacement"
        );
        second_result.expect("second restart");
        assert!(
            !latest_process.is_killed(),
            "stale same-key cleanup killed the replacement process"
        );
        assert_eq!(
            backend
                .spawn_count
                .load(std::sync::atomic::Ordering::Acquire),
            2
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn stale_output_after_close_and_same_key_reopen_cannot_hide_replacement_output() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let input = TerminalOpenInput::new(
            "thread-output-generation",
            "term-output-generation",
            root.path().to_path_buf(),
            80,
            24,
        );
        manager.open(input.clone()).await.unwrap();
        let old_session = manager
            .require_session("thread-output-generation", "term-output-generation")
            .await
            .unwrap();
        let old_process = backend.latest();
        old_process.emit("old-before-close");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if old_session
                    .lock()
                    .await
                    .history
                    .snapshot()
                    .contains("old-before-close")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old output was not published");
        let old_generation = old_session.lock().await.generation.clone();
        let output_completed = old_generation.output_completed.clone();
        let output_barrier = Arc::new(PublisherBarrier {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *old_generation
            .output_barrier
            .lock()
            .expect("output barrier lock") = Some(output_barrier.clone());
        old_process.emit("stale-after-reopen");
        tokio::time::timeout(Duration::from_secs(2), output_barrier.started.notified())
            .await
            .expect("stale output publisher did not reach the barrier");

        manager
            .close("thread-output-generation", Some("term-output-generation"))
            .await
            .unwrap();
        manager.open(input).await.unwrap();
        let mut replacement = manager
            .attach(TerminalAttachInput::existing(
                "thread-output-generation",
                "term-output-generation",
            ))
            .await
            .unwrap();
        let replacement_process = backend.latest();

        output_barrier.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), output_completed.cancelled())
            .await
            .expect("stale output publisher did not complete");
        replacement_process.emit("replacement-output");

        let received = next_attachment_event(&mut replacement).await;
        assert!(
            matches!(
                received,
                TerminalEvent::Output { ref data, .. } if data == "replacement-output"
            ),
            "replacement attachment accepted stale output: {received:?}"
        );
    }

    #[tokio::test]
    async fn hardening_redaction_lag_conservatively_redacts_a_buffered_secret_prefix() {
        let root = tempfile::tempdir().expect("temp dir");
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        manager
            .open(TerminalOpenInput::new(
                "thread-redaction-lag",
                "terminal-redaction-lag",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("terminal");
        let session = manager
            .require_session("thread-redaction-lag", "terminal-redaction-lag")
            .await
            .expect("session");
        session.lock().await.private_output =
            StreamingSecretRedactor::new(vec!["private-middle-suffix".to_owned()]);
        let process = backend.latest();
        process.emit("private-");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if session.lock().await.private_output.pending == "private-" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prefix buffered");

        let generation = session.lock().await.generation.clone();
        let barrier = Arc::new(PublisherBarrier {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *generation
            .output_barrier
            .lock()
            .expect("output barrier lock") = Some(barrier.clone());
        let started = barrier.started.notified();
        tokio::pin!(started);
        process.emit("");
        started.await;
        process.emit("middle-");
        for _ in 0..32 {
            process.emit("");
        }
        process.emit("suffix");
        *generation
            .output_barrier
            .lock()
            .expect("output barrier lock") = None;
        barrier.release.notify_one();

        let history = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let history = session.lock().await.history.snapshot();
                if !history.is_empty() {
                    break history;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("lag output");
        manager.shutdown().await;

        assert!(!history.contains("private-"), "{history}");
        assert!(!history.contains("middle-"), "{history}");
        assert!(!history.contains("suffix"), "{history}");
        assert!(history.contains("[redacted]"), "{history}");
    }

    #[tokio::test]
    async fn structured_command_failure_does_not_fall_back_to_a_shell_candidate() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            fail_spawns: true,
            ..HistoryTestBackend::default()
        });
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                preferred_shell: Some("/bin/sh".to_owned()),
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-provider",
            "term-provider",
            root.path().to_path_buf(),
            120,
            30,
        );
        input.command = Some(crate::terminal::TerminalLaunchCommand {
            executable: "missing-provider".to_owned(),
            args: vec!["--direct".to_owned()],
            label: Some("Provider Terminal".to_owned()),
            activity: None,
            env: None,
        });

        let error = manager.open(input).await.unwrap_err();

        assert!(matches!(
            error,
            TerminalError::Spawn {
                ref attempted,
                ref message,
            } if attempted == &["missing-provider [\"--direct\"]"]
                && message == "provider spawn failed"
        ));
        let spawns = backend.spawns();
        assert_eq!(spawns.len(), 1);
        assert_eq!(spawns[0].executable, "missing-provider");
        assert_eq!(spawns[0].args, ["--direct"]);
    }

    #[tokio::test]
    async fn structured_command_restart_if_not_running_preserves_the_direct_launch() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let command = crate::terminal::TerminalLaunchCommand {
            executable: "claude".to_owned(),
            args: vec!["--dangerously-skip-permissions".to_owned()],
            label: Some("Claude Terminal".to_owned()),
            activity: None,
            env: None,
        };
        let mut input = TerminalOpenInput::new(
            "thread-provider",
            "term-provider",
            root.path().to_path_buf(),
            90,
            28,
        );
        input.command = Some(command.clone());
        manager.open(input).await.unwrap();

        let mut events = manager.subscribe_events();
        backend
            .latest()
            .exit
            .send(Some(PtyExit {
                exit_code: Some(0),
                signal: None,
            }))
            .unwrap();
        let exited = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(exited, TerminalEvent::Exited { .. }));

        let attachment = manager
            .attach(TerminalAttachInput {
                center_panel: false,
                size_claim: None,
                thread_id: "thread-provider".to_owned(),
                terminal_id: "term-provider".to_owned(),
                cwd: Some(root.path().to_path_buf()),
                worktree_path: Some(root.path().to_path_buf()),
                cols: Some(90),
                rows: Some(28),
                env: std::collections::BTreeMap::new(),
                restart_if_not_running: true,
                command: Some(command),
            })
            .await
            .unwrap();

        assert_eq!(attachment.initial.label, "Claude Terminal");
        let spawns = backend.spawns();
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[1].executable, "claude");
        assert_eq!(spawns[1].args, ["--dangerously-skip-permissions"]);
    }

    #[tokio::test]
    async fn configured_history_survives_output_and_clear_but_restart_starts_fresh() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                history_line_limit: 2,
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let input = TerminalOpenInput::new(
            "thread-history",
            "term-history",
            root.path().to_path_buf(),
            80,
            24,
        );

        manager.open(input.clone()).await.unwrap();
        let original_session = manager
            .require_session("thread-history", "term-history")
            .await
            .unwrap();
        assert_eq!(original_session.lock().await.history.line_limit(), 2);

        let mut events = manager.subscribe_events();
        let process = backend.latest();
        for chunk in ["one\n", "two\n", "three\n"] {
            process.emit(chunk);
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .expect("output timeout")
                .expect("output event");
            assert!(matches!(event, TerminalEvent::Output { data, .. } if data == chunk));
        }

        let attachment = manager
            .attach(TerminalAttachInput::existing(
                "thread-history",
                "term-history",
            ))
            .await
            .unwrap();
        assert_eq!(attachment.initial.history, "two\nthree\n");

        manager
            .clear("thread-history", "term-history")
            .await
            .unwrap();
        let cleared = manager
            .attach(TerminalAttachInput::existing(
                "thread-history",
                "term-history",
            ))
            .await
            .unwrap();
        assert!(cleared.initial.history.is_empty());
        assert_eq!(original_session.lock().await.history.line_limit(), 2);

        let restarted = manager.restart(input).await.unwrap();
        assert!(restarted.history.is_empty());
        let restarted_session = manager
            .require_session("thread-history", "term-history")
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&original_session, &restarted_session));
        assert_eq!(restarted_session.lock().await.history.line_limit(), 2);
        assert_eq!(backend.processes.lock().expect("processes lock").len(), 2);

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn workspace_quiesce_stops_every_process_and_retains_session_history() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        for terminal_id in ["term-one", "term-two"] {
            manager
                .open(TerminalOpenInput::new(
                    "thread-quiesce",
                    terminal_id,
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .unwrap();
        }
        let mut events = manager.subscribe_events();
        for (process, transcript) in backend
            .processes
            .lock()
            .expect("processes")
            .iter()
            .zip(["retained-one\n", "retained-two\n"])
        {
            process.emit(transcript);
        }
        for _ in 0..2 {
            let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
                .await
                .expect("output timeout")
                .expect("output event");
            assert!(matches!(event, TerminalEvent::Output { .. }));
        }

        manager
            .quiesce_thread_preserving_history("thread-quiesce")
            .await
            .expect("quiesce");

        for (terminal_id, transcript) in [
            ("term-one", "retained-one\n"),
            ("term-two", "retained-two\n"),
        ] {
            let attachment = manager
                .attach(TerminalAttachInput::existing("thread-quiesce", terminal_id))
                .await
                .expect("retained terminal attaches");
            assert_eq!(attachment.initial.status, TerminalStatus::Exited);
            assert_eq!(attachment.initial.history, transcript);
        }
        assert!(
            backend
                .processes
                .lock()
                .expect("processes")
                .iter()
                .all(|process| process.is_killed())
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn exact_workspace_quiesce_skips_a_replacement_and_stops_other_captured_sessions() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        for terminal_id in ["term-replaced", "term-retained"] {
            manager
                .open(TerminalOpenInput::new(
                    "thread-exact-quiesce",
                    terminal_id,
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .unwrap();
        }
        let captured = manager
            .capture_thread_session_identities("thread-exact-quiesce")
            .await;
        assert_eq!(captured.len(), 2, "cleanup captures every live session");

        manager
            .restart(TerminalRestartInput {
                center_panel: false,
                thread_id: "thread-exact-quiesce".to_owned(),
                terminal_id: "term-replaced".to_owned(),
                cwd: root.path().to_path_buf(),
                worktree_path: None,
                cols: 80,
                rows: 24,
                env: std::collections::BTreeMap::new(),
                command: None,
            })
            .await
            .expect("recovered replacement starts");
        let processes = backend.processes.lock().expect("processes").clone();
        let replacement = processes.last().expect("replacement process").clone();
        let mut events = manager.subscribe_events();
        replacement.emit("replacement-history\n");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(events.recv().await, Ok(TerminalEvent::Output { data, .. }) if data == "replacement-history\n") {
                    break;
                }
            }
        })
        .await
        .expect("replacement output published");

        manager
            .quiesce_sessions_preserving_history_if_current(captured)
            .await
            .expect("stale exact cleanup");

        assert!(
            !replacement.is_killed(),
            "stale cleanup must not kill the recovered replacement"
        );
        manager
            .write("thread-exact-quiesce", "term-replaced", "still-usable")
            .await
            .expect("replacement remains writable");
        let replacement_attachment = manager
            .attach(TerminalAttachInput::existing(
                "thread-exact-quiesce",
                "term-replaced",
            ))
            .await
            .expect("replacement remains attachable");
        assert_eq!(
            replacement_attachment.initial.status,
            TerminalStatus::Running
        );
        assert_eq!(
            replacement_attachment.initial.history,
            "replacement-history\n"
        );
        let retained_attachment = manager
            .attach(TerminalAttachInput::existing(
                "thread-exact-quiesce",
                "term-retained",
            ))
            .await
            .expect("captured session history remains attachable");
        assert_eq!(retained_attachment.initial.status, TerminalStatus::Exited);
        assert!(processes[1].is_killed());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn workspace_quiesce_attempts_every_process_when_kills_fail() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        for terminal_id in ["term-one", "term-two"] {
            manager
                .open(TerminalOpenInput::new(
                    "thread-quiesce-errors",
                    terminal_id,
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .unwrap();
        }
        let processes = backend.processes.lock().expect("processes").clone();
        for process in &processes {
            process.fail_kill("expected kill failure");
        }

        assert!(matches!(
            manager
                .quiesce_thread_preserving_history("thread-quiesce-errors")
                .await,
            Err(TerminalError::Close)
        ));
        assert!(
            processes.iter().all(|process| process.is_killed()),
            "one failed kill must not prevent later terminals from being signaled"
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn terminal_env_contains_open_url_vars() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let issued = Arc::new(StdMutex::new(Vec::<String>::new()));
        let hook_issued = issued.clone();
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                session_env: Some(TerminalManagerOptions::session_env_hook(
                    move |thread_id, env| {
                        let issued = hook_issued.clone();
                        Box::pin(async move {
                            let mut issued = issued.lock().unwrap();
                            let token = format!("{thread_id}-token-{}", issued.len());
                            issued.push(token.clone());
                            crate::open_url::OpenUrlSession {
                                endpoint: "http://127.0.0.1:3773/api/preview/open-url".to_owned(),
                                token,
                                shim_dir: PathBuf::from("/state/runtime/open-url"),
                            }
                            .apply(env);
                        })
                    },
                )),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input =
            TerminalOpenInput::new("thread-open-url", "term", root.path().to_path_buf(), 80, 24);
        input.env.insert("BROWSER".to_owned(), "firefox".to_owned());
        input
            .env
            .insert("PATH".to_owned(), "/client/bin".to_owned());

        manager.open(input.clone()).await.unwrap();
        // Opening a running terminal again spawns nothing and issues nothing.
        manager.open(input.clone()).await.unwrap();
        manager.restart(input).await.unwrap();

        let spawns = backend.spawns();
        assert_eq!(spawns.len(), 2, "open and restart each spawn once");
        let separator = if cfg!(windows) { ';' } else { ':' };
        for (spawn, token) in spawns.iter().zip(issued.lock().unwrap().iter()) {
            assert_eq!(spawn.env["BIBCODE_OPEN_URL_AUTH"], *token);
            assert_eq!(
                spawn.env["BIBCODE_OPEN_URL_ENDPOINT"],
                "http://127.0.0.1:3773/api/preview/open-url"
            );
            assert_eq!(spawn.env["BROWSER"], "bibcode-open-url");
            assert_eq!(spawn.env["BRAINSTORM_OPEN_CMD"], "bibcode-open-url");
            assert_eq!(
                spawn.env["PATH"],
                format!("/state/runtime/open-url{separator}/client/bin")
            );
        }
        assert_eq!(
            *issued.lock().unwrap(),
            vec![
                "thread-open-url-token-0".to_owned(),
                "thread-open-url-token-1".to_owned()
            ]
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn console_theme_survives_attach_and_updates_on_restart() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend,
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-theme",
            "term-theme",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.env.insert(
            "BIBCODE_WINDOWS_CONSOLE_THEME".to_owned(),
            "light".to_owned(),
        );

        let opened = manager.open(input.clone()).await.unwrap();
        assert_eq!(opened.console_theme, Some(TerminalConsoleTheme::Light));
        let attached = manager
            .attach(TerminalAttachInput::existing("thread-theme", "term-theme"))
            .await
            .unwrap();
        assert_eq!(
            attached.initial.console_theme,
            Some(TerminalConsoleTheme::Light)
        );
        let metadata = manager.subscribe_metadata().await;
        assert_eq!(metadata.initial.len(), 1);
        assert_eq!(
            metadata.initial[0].console_theme,
            Some(TerminalConsoleTheme::Light)
        );
        let mut events = manager.subscribe_events();
        let mut metadata_events = metadata.events;

        input.env.insert(
            "BIBCODE_WINDOWS_CONSOLE_THEME".to_owned(),
            "dark".to_owned(),
        );
        let restarted = manager.restart(input).await.unwrap();
        assert_eq!(restarted.console_theme, Some(TerminalConsoleTheme::Dark));
        assert!(matches!(
            events.recv().await.unwrap(),
            TerminalEvent::Restarted { snapshot, .. }
                if snapshot.console_theme == Some(TerminalConsoleTheme::Dark)
        ));
        assert!(matches!(
            metadata_events.recv().await.unwrap(),
            TerminalMetadataEvent::Upsert { terminal }
                if terminal.console_theme == Some(TerminalConsoleTheme::Dark)
        ));

        manager.shutdown().await;
    }

    #[tokio::test]
    async fn summaries_carry_sticky_center_panel_and_launch_command() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::from_secs(3600),
                ..TerminalManagerOptions::default()
            },
        );
        let summary = async |terminal_id: &str| {
            manager
                .subscribe_metadata()
                .await
                .initial
                .into_iter()
                .find(|summary| summary.terminal_id == terminal_id)
                .expect("terminal summary")
        };
        let mut metadata = manager.subscribe_metadata().await;
        let mut upserts = || {
            std::iter::from_fn(|| metadata.events.try_recv().ok())
                .filter_map(|event| match event {
                    TerminalMetadataEvent::Upsert { terminal } => Some(terminal),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let launch = |executable: &str| TerminalLaunchCommand {
            executable: executable.to_owned(),
            args: vec!["--flag".to_owned()],
            label: Some("AI Terminal".to_owned()),
            activity: None,
            env: Some(std::collections::BTreeMap::from([(
                "PROVIDER_DEFAULT".to_owned(),
                "1".to_owned(),
            )])),
        };

        // A plain open is not a center panel and has no launch command.
        let plain = TerminalOpenInput::new("host", "drawer", root.path().to_path_buf(), 80, 24);
        manager.open(plain.clone()).await.unwrap();
        let opened = summary("drawer").await;
        assert!(!opened.center_panel);
        assert_eq!(opened.command, None);
        let wire = serde_json::to_value(&opened).unwrap();
        assert_eq!(wire["centerPanel"], false);
        assert!(wire.get("command").is_none());
        assert_eq!(upserts().len(), 1);

        // centerPanel=false never publishes or clears; true flips it once.
        manager.open(plain.clone()).await.unwrap();
        assert!(upserts().is_empty());
        let mut marked = plain.clone();
        marked.center_panel = true;
        manager.open(marked.clone()).await.unwrap();
        let flipped = upserts();
        assert_eq!(flipped.len(), 1);
        assert!(flipped[0].center_panel);
        assert!(summary("drawer").await.center_panel);
        manager.open(marked).await.unwrap();
        manager.open(plain).await.unwrap();
        assert!(upserts().is_empty());
        assert!(summary("drawer").await.center_panel);

        // A center panel opened with a command reports both.
        let mut ai = TerminalOpenInput::new("host", "ai", root.path().to_path_buf(), 80, 24);
        ai.center_panel = true;
        ai.command = Some(launch("claude"));
        manager.open(ai.clone()).await.unwrap();
        let opened = summary("ai").await;
        assert!(opened.center_panel);
        assert_eq!(opened.command, Some(launch("claude")));
        assert_eq!(
            serde_json::to_value(&opened).unwrap()["command"],
            serde_json::json!({
                "executable": "claude",
                "args": ["--flag"],
                "label": "AI Terminal",
                "env": { "PROVIDER_DEFAULT": "1" },
            })
        );

        // Restart without a command keeps the marker and reports the shell it
        // spawned, never the previous AI command.
        let mut bare_restart = ai.clone();
        bare_restart.center_panel = false;
        bare_restart.command = None;
        manager.restart(bare_restart).await.unwrap();
        assert_ne!(backend.spawns().last().unwrap().executable, "claude");
        let restarted = summary("ai").await;
        assert!(restarted.center_panel);
        assert_eq!(restarted.command, None);

        // Restart with a new command stores that command.
        let mut new_restart = ai.clone();
        new_restart.center_panel = false;
        new_restart.command = Some(launch("codex"));
        manager.restart(new_restart).await.unwrap();
        assert_eq!(backend.spawns().last().unwrap().executable, "codex");
        let restarted = summary("ai").await;
        assert!(restarted.center_panel);
        assert_eq!(restarted.command, Some(launch("codex")));

        // Reopening an exited center panel without a command keeps the marker
        // and reports the default shell it spawned.
        backend.latest().exit(0);
        tokio::time::timeout(Duration::from_secs(2), async {
            while summary("ai").await.status != TerminalStatus::Exited {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal exit is published");
        manager
            .open(TerminalOpenInput::new(
                "host",
                "ai",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let reopened = summary("ai").await;
        assert_eq!(reopened.status, TerminalStatus::Running);
        assert!(reopened.center_panel);
        assert_ne!(backend.spawns().last().unwrap().executable, "codex");
        assert_eq!(reopened.command, None);

        // An attach that creates the session can mark it as a center panel.
        let mut attach = TerminalAttachInput::existing("host", "attached");
        attach.cwd = Some(root.path().to_path_buf());
        attach.center_panel = true;
        drop(manager.attach(attach).await.unwrap());
        let attached = summary("attached").await;
        assert!(attached.center_panel);
        assert_eq!(attached.command, None);

        // An attach to an existing plain session flips the marker once.
        let mut other = TerminalOpenInput::new("host", "other", root.path().to_path_buf(), 80, 24);
        other.command = Some(launch("claude"));
        manager.open(other).await.unwrap();
        upserts();
        let mut attach = TerminalAttachInput::existing("host", "other");
        attach.center_panel = true;
        drop(manager.attach(attach).await.unwrap());
        let flipped = upserts();
        assert_eq!(flipped.len(), 1);
        assert!(flipped[0].center_panel);
        assert_eq!(flipped[0].command, Some(launch("claude")));

        // Only an explicit close tells clients to drop the tab; shutdown
        // removals keep it so the terminal can relaunch from its tab.
        let mut removals = || {
            std::iter::from_fn(|| metadata.events.try_recv().ok())
                .filter_map(|event| match event {
                    TerminalMetadataEvent::Remove {
                        terminal_id,
                        reason,
                        ..
                    } => Some((terminal_id, reason)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        manager.close("host", Some("other")).await.unwrap();
        assert_eq!(
            removals(),
            vec![("other".to_owned(), TerminalRemovalReason::Closed)]
        );
        manager.shutdown().await;
        let shutdown = removals();
        assert!(!shutdown.is_empty());
        assert!(
            shutdown
                .iter()
                .all(|(_, reason)| *reason == TerminalRemovalReason::Shutdown)
        );
    }

    #[tokio::test]
    async fn manager_covers_live_lifecycle_attachments_and_metadata() {
        let root = tempfile::tempdir().unwrap();
        let manager = TerminalManager::new(
            Arc::new(PortablePtyBackend),
            TerminalManagerOptions {
                history_line_limit: 2,
                preferred_shell: Some("/bin/sh".to_owned()),
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );

        let mut metadata = manager.subscribe_metadata().await;
        assert!(metadata.initial.is_empty());
        assert!(
            manager
                .resize("missing", "missing", 80, 24, None)
                .await
                .is_ok()
        );
        assert!(matches!(
            manager
                .attach(TerminalAttachInput::existing("missing", "missing"))
                .await,
            Err(TerminalError::NotFound { .. })
        ));

        let input = TerminalOpenInput::new(
            "thread-unit",
            "term-unit",
            root.path().to_path_buf(),
            80,
            24,
        );
        let opened = manager.open(input.clone()).await.unwrap();
        assert_eq!(opened.label, "Terminal unit");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), metadata.recv())
                .await
                .unwrap(),
            Some(TerminalMetadataEvent::Upsert { .. })
        ));

        let mut attach_input = TerminalAttachInput::existing("thread-unit", "term-unit");
        attach_input.cols = Some(100);
        attach_input.rows = Some(30);
        let mut attachment = manager.attach(attach_input).await.unwrap();
        manager.write("thread-unit", "term-unit", "").await.unwrap();
        manager
            .resize("thread-unit", "term-unit", 120, 40, None)
            .await
            .unwrap();
        manager.clear("thread-unit", "term-unit").await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), attachment.recv())
                .await
                .unwrap(),
            Some(AttachmentItem::Event(TerminalEvent::Cleared { .. }))
        ));

        let restarted = manager.restart(input).await.unwrap();
        assert_eq!(restarted.status, TerminalStatus::Running);
        manager
            .close("thread-unit", Some("term-unit"))
            .await
            .unwrap();
        manager.shutdown().await;
    }

    fn terminal_claims(
        registry: &ProcessAttributionRegistry,
        pids: &[u32],
    ) -> Vec<crate::diagnostics::ProcessClaim> {
        let rows = pids
            .iter()
            .map(|pid| ProcessRow::fixture(*pid, 0, "shell"))
            .collect::<Vec<_>>();
        registry.bind_and_snapshot(&rows, Instant::now())
    }

    fn attributed_manager(
        backend: Arc<HistoryTestBackend>,
        registry: ProcessAttributionRegistry,
    ) -> TerminalManager {
        TerminalManager::with_process_attribution(
            backend,
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
            registry,
        )
    }

    #[tokio::test]
    async fn terminal_registration_tracks_start_and_exit() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            expose_process_identity: true,
            ..HistoryTestBackend::default()
        });
        let registry = ProcessAttributionRegistry::new();
        let manager = attributed_manager(backend.clone(), registry.clone());
        let opened = manager
            .open(TerminalOpenInput::new(
                "thread-attributed",
                "term-attributed",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let pid = opened.pid.expect("running terminal pid");
        let claims = terminal_claims(&registry, &[pid]);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].scope, AttributionScope::External);
        assert_eq!(claims[0].kind, AttributionKind::Terminal);
        assert_eq!(claims[0].label, opened.label);

        let mut events = manager.subscribe_events();
        backend.latest().exit(0);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    events.recv().await,
                    Ok(TerminalEvent::Exited {
                        thread_id,
                        terminal_id,
                        ..
                    }) if thread_id == "thread-attributed" && terminal_id == "term-attributed"
                ) {
                    break;
                }
            }
        })
        .await
        .expect("terminal exit event");
        assert!(terminal_claims(&registry, &[pid]).is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn frozen_runtime_rejects_and_reaps_a_late_terminal_spawn() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            expose_process_identity: true,
            ..HistoryTestBackend::default()
        });
        let registry = ProcessAttributionRegistry::new();
        let manager = attributed_manager(backend.clone(), registry.clone());
        assert!(registry.freeze_and_snapshot_identities().is_empty());

        let result = manager
            .open(TerminalOpenInput::new(
                "thread-after-freeze",
                "term-after-freeze",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await;

        assert!(matches!(result, Err(TerminalError::Shutdown)));
        let process = backend.latest();
        assert!(process.is_killed(), "rejected late process must be killed");
        assert!(
            process.subscribe_exit().borrow().is_some(),
            "rejected late process must be observed as exited before open returns"
        );
        assert!(
            manager
                .require_session("thread-after-freeze", "term-after-freeze")
                .await
                .is_err(),
            "a rejected process must never be published as a terminal session"
        );
    }

    #[tokio::test]
    async fn full_registry_rejects_and_reaps_an_unattributed_terminal_spawn() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            expose_process_identity: true,
            ..HistoryTestBackend::default()
        });
        let registry = ProcessAttributionRegistry::new();
        let registrations = (0..512_u32)
            .map(|pid| {
                registry
                    .register_identity(
                        crate::diagnostics::ProcessIdentity {
                            pid: pid + 10_000,
                            started_at: u64::from(pid),
                        },
                        ProcessRegistrationMetadata {
                            scope: AttributionScope::External,
                            kind: AttributionKind::Provider,
                            label: "capacity fixture".to_owned(),
                            source: RegistrationSource::Provider,
                            driver: None,
                        },
                    )
                    .expect("fill registry")
            })
            .collect::<Vec<_>>();
        let manager = attributed_manager(backend.clone(), registry);

        let result = manager
            .open(TerminalOpenInput::new(
                "thread-at-capacity",
                "term-at-capacity",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await;

        assert!(matches!(result, Err(TerminalError::Spawn { .. })));
        let process = backend.latest();
        assert!(process.is_killed(), "unattributed process must be killed");
        assert!(
            process.subscribe_exit().borrow().is_some(),
            "unattributed process must be reaped before open returns"
        );
        drop(registrations);
    }

    #[tokio::test]
    async fn exit_during_identity_registration_is_observed_and_releases_the_claim() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            expose_process_identity: true,
            exit_on_identity_read: Some(PtyExit {
                exit_code: Some(17),
                signal: None,
            }),
            ..HistoryTestBackend::default()
        });
        let registry = ProcessAttributionRegistry::new();
        let manager = attributed_manager(backend, registry.clone());
        let mut events = manager.subscribe_events();
        let opened = manager
            .open(TerminalOpenInput::new(
                "thread-exited-during-start",
                "term-exited-during-start",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .unwrap();
        let pid = opened.pid.expect("spawned terminal pid");

        let exit_event = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(event @ TerminalEvent::Exited { .. }) = events.recv().await {
                    break event;
                }
            }
        })
        .await
        .expect("already-observed terminal exit must be supervised");
        assert!(matches!(
            exit_event,
            TerminalEvent::Exited {
                exit_code: Some(17),
                ..
            }
        ));
        assert!(terminal_claims(&registry, &[pid]).is_empty());
        manager.shutdown().await;
    }

    #[cfg(unix)]
    async fn next_exit(events: &mut TerminalEventReceiver, terminal: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(TerminalEvent::Exited { terminal_id, .. }) = events.recv().await
                    && terminal_id == terminal
                {
                    return;
                }
            }
        })
        .await
        .expect("exit published");
    }

    /// A raw Unix wait status: a signal number, or an exit code shifted left by 8.
    #[cfg(unix)]
    fn raw_status(raw: i32) -> std::process::ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(raw)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_and_provider_processes_dying_of_a_crash_signal_are_reported() {
        let captured = crate::diagnostics::profiler::exits::CapturedExits::start();
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        let mut events = manager.subscribe_events();
        let mut codex =
            TerminalOpenInput::new("thread", "codex", root.path().to_path_buf(), 80, 24);
        codex.command = Some(TerminalLaunchCommand {
            executable: "codex".to_owned(),
            args: Vec::new(),
            label: None,
            activity: Some(crate::terminal::ProviderTerminalActivityLaunch {
                driver_kind: "codex".to_owned(),
                provider_instance_id: "codex".to_owned(),
            }),
            env: None,
        });
        let shell = |terminal: &str| {
            TerminalOpenInput::new("thread", terminal, root.path().to_path_buf(), 80, 24)
        };
        // A crash, a provider CLI crash, and exits a terminal's user causes on purpose.
        for (input, status) in [
            (shell("crashed"), raw_status(libc::SIGSEGV)),
            (codex, raw_status(libc::SIGABRT)),
            (shell("interrupted"), raw_status(libc::SIGINT)),
            (shell("killed"), raw_status(libc::SIGKILL)),
            (shell("exit-1"), raw_status(1 << 8)),
        ] {
            let terminal = input.terminal_id.clone();
            manager.open(input).await.expect("terminal starts");
            let process = backend.latest();
            *process.exit_status.lock().unwrap() = Some(status);
            process.exit.send_replace(Some(PtyExit {
                exit_code: status.code().or(Some(1)),
                signal: None,
            }));
            next_exit(&mut events, &terminal).await;
        }

        assert_eq!(
            captured.take(),
            [
                ["terminal", "shell", "SIGSEGV"],
                ["provider", "codex", "SIGABRT"]
            ]
        );
        manager.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminals_bibcode_stops_are_not_reported_whatever_their_exit() {
        let captured = crate::diagnostics::profiler::exits::CapturedExits::start();
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        for thread in ["quiesced", "closed"] {
            manager
                .open(TerminalOpenInput::new(
                    thread,
                    "term",
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .expect("terminal starts");
            // Whatever a kill leaves behind, a stop BiBCode made is not a crash.
            *backend.latest().exit_status.lock().unwrap() = Some(raw_status(libc::SIGSEGV));
        }

        manager
            .quiesce_thread_preserving_history("quiesced")
            .await
            .expect("quiesce");
        manager.close("closed", Some("term")).await.expect("close");
        // Both kills published their exit; let any exit watcher run.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        assert!(captured.take().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_terminal_that_exits_after_identity_read_opens_and_reports_its_exit() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            expose_process_identity: false,
            exit_after_identity_read: Some((
                Duration::from_millis(100),
                PtyExit {
                    exit_code: Some(17),
                    signal: None,
                },
            )),
            ..HistoryTestBackend::default()
        });
        let registry = ProcessAttributionRegistry::new();
        let manager = attributed_manager(backend.clone(), registry.clone());
        let mut events = manager.subscribe_events();
        manager
            .open(TerminalOpenInput::new(
                "thread-exited-after-identity-read",
                "term-exited-after-identity-read",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await
            .expect("an already exited terminal must open");

        let exit_event = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(event @ TerminalEvent::Exited { .. }) = events.recv().await {
                    break event;
                }
            }
        })
        .await
        .expect("exit published after identity read must be supervised");
        assert!(matches!(
            exit_event,
            TerminalEvent::Exited {
                exit_code: Some(17),
                ..
            }
        ));
        let process = backend.latest();
        assert!(
            !process.is_killed(),
            "natural exit must not kill the process tree"
        );
        assert!(terminal_claims(&registry, &[process.pid()]).is_empty());
        manager.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_terminal_without_a_process_identity_is_refused_and_killed() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend {
            expose_process_identity: false,
            ..HistoryTestBackend::default()
        });
        let registry = ProcessAttributionRegistry::new();
        let manager = attributed_manager(backend.clone(), registry.clone());

        let started = tokio::time::Instant::now();
        let result = manager
            .open(TerminalOpenInput::new(
                "thread-missing-identity",
                "term-missing-identity",
                root.path().to_path_buf(),
                80,
                24,
            ))
            .await;

        assert!(started.elapsed() >= TERMINAL_CLOSE_WAIT_TIMEOUT);
        assert!(matches!(
            result,
            Err(TerminalError::Spawn { message, .. })
                if message == "spawned terminal process has no stable process identity"
        ));
        let process = backend.latest();
        assert!(
            process.is_killed(),
            "unattributed live process must be killed"
        );
        assert!(process.subscribe_exit().borrow().is_some());
        assert!(terminal_claims(&registry, &[process.pid()]).is_empty());
        assert!(
            manager
                .require_session("thread-missing-identity", "term-missing-identity")
                .await
                .is_err(),
            "an unattributed live process must never be published"
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_attempts_every_terminal_owner_and_bounds_failures() {
        let root = tempfile::tempdir().unwrap();
        let backend = Arc::new(HistoryTestBackend::default());
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                subprocess_poll_interval: Duration::ZERO,
                ..TerminalManagerOptions::default()
            },
        );
        for index in 0..12 {
            manager
                .open(TerminalOpenInput::new(
                    "thread-cleanup",
                    format!("term-{index}"),
                    root.path().to_path_buf(),
                    80,
                    24,
                ))
                .await
                .expect("terminal opens");
        }
        for (index, process) in backend
            .processes
            .lock()
            .expect("processes lock")
            .iter()
            .enumerate()
        {
            if index != 1 {
                process.fail_kill("界".repeat(500));
            }
        }

        let report = manager.shutdown_with_report().await;

        assert_eq!(report.attempted, 12);
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failure_count, 11);
        assert!(report.failures.len() < report.failure_count);
        assert!(
            report
                .failures
                .iter()
                .all(|failure| failure.chars().count() <= 160)
        );
    }

    #[test]
    fn presentation_helpers_cover_history_and_process_labels() {
        let mut history = TerminalHistory::new(2);
        history.push("one\ntwo\nthree\n");
        history.push("four\n");
        assert_eq!(history.snapshot(), "three\nfour\n");
        let mut cleared = TerminalHistory::new(0);
        cleared.push("ignored");
        assert!(cleared.snapshot().is_empty());

        assert_eq!(
            normalize_child_command_name("[/usr/bin/node.exe --flag]"),
            Some("node".into())
        );
        assert_eq!(
            normalize_child_command_name("( cargo test )"),
            Some("cargo".into())
        );
        assert_eq!(normalize_child_command_name("[]"), None);
        assert_eq!(terminal_label("custom"), "custom");
        assert_eq!(terminal_label("term-"), "term-");
    }
}
