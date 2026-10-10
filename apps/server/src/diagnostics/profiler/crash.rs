//! Native crash capture.
//!
//! An in-process `crash-handler` hook is attached once, first thing in `main`, and never
//! detached: on Linux a detach restores the signal handlers that existed at attach time, which
//! drops the ones JavaScriptCore installs later, and detaching can itself panic (upstream PR
//! #118). On a fault the hook hands the crash context to a monitor child over `minidumper`'s
//! IPC, and the child writes the minidump from outside the crashed address space. The child is
//! this same binary re-executed as `--crash-monitor <socket> <dir> <parent-pid>`.
//!
//! The profiler toggle only starts and stops the child ([`MonitorDetector`]). With no child the
//! hook returns `Handled(false)`, and the OS crash path (systemd-coredump, WER) runs as before.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use crash_handler::{CrashContext, CrashEventResult, CrashHandler};
use minidumper::{Client, LoopAction, MinidumpBinary, ServerHandler, SocketName};
use serde_json::{Value, json};
use tokio::{sync::watch, task::JoinHandle, time::MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::{os_records, profiler_dir, runtime::Detector, store};
use crate::{
    config::ServerConfig,
    data_root::{DataRootRequest, DataRootSource, resolve_data_root},
};

const MONITOR_FLAG: &str = "--crash-monitor";
/// Set by the parent; the monitor names the crashed process with it in each sidecar.
const PROCESS_KIND_ENV: &str = "BIBCODE_CRASH_MONITOR_PROCESS_KIND";
/// A release-build validation hook for the runbooks: exactly `1` at startup makes the process
/// fault on purpose 5 s after its monitor is armed.
const CRASH_TEST_ENV: &str = "BIBCODE_PROFILER_CRASH_TEST";
const CRASH_TEST_DELAY: Duration = Duration::from_secs(5);
const CAPABILITIES_FILE: &str = "capabilities.json";
#[cfg(target_os = "linux")]
const YAMA_SCOPE: &str = "/proc/sys/kernel/yama/ptrace_scope";
const PING_EVERY: Duration = Duration::from_secs(10);
const PING_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The monitor drops a connection idle this long; the pings keep a live parent's open.
const STALE_TIMEOUT: Duration = Duration::from_secs(30);
const PARENT_CHECK_EVERY: Duration = Duration::from_secs(2);
const MAX_DUMPS: usize = 20;
/// The `send_message` kind that carries the parent's newest event lines.
const LAST_EVENTS: u32 = 1;
const LAST_EVENTS_LINES: usize = 50;
const LAST_EVENTS_MAX_BYTES: usize = 64 * 1024;
/// The most one message carries. Linux sends datagrams, which arrive whole. macOS and Windows
/// use a stream socket, where minidumper reads a message with a single `recv` and a short read
/// desyncs the protocol, so a message there must fit the smallest default socket buffer
/// (8 KiB on macOS) and is sent only right after a ping has drained the connection.
const MESSAGE_MAX_BYTES: usize = if cfg!(target_os = "linux") {
    LAST_EVENTS_MAX_BYTES
} else {
    4 * 1024
};
/// `minidump-writer` does not implement Windows arm64, so dumps stay off there until a native
/// runtime test passes.
const DUMPS_DISABLED: bool = cfg!(all(windows, target_arch = "aarch64"));

/// Read by the crash callback, which must neither block nor allocate.
static ARMED: AtomicBool = AtomicBool::new(false);
/// The connection to the monitor. The callback only `try_lock`s it; everyone else holds the
/// lock just long enough to clone or swap the `Arc`, never across I/O.
static CLIENT: Mutex<Option<Arc<Client>>> = Mutex::new(None);
/// The attached hook, never dropped: dropping it would detach. `None` if attaching failed.
static HANDLER: OnceLock<Option<CrashHandler>> = OnceLock::new();
static CRASH_TEST: AtomicBool = AtomicBool::new(false);
/// Ends the monitor's server loop.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Runs the monitor when this process was started as one and returns its exit code. Call it
/// first thing in `main`: the monitor must never reach Tauri, the single-instance plugin or a
/// Tokio runtime.
pub fn dispatch_crash_monitor() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.as_os_str() != OsStr::new(MONITOR_FLAG) {
        return None;
    }
    let rest: Vec<OsString> = args.collect();
    Some(parse_monitor_args(&rest).map_or(2, run_monitor))
}

/// Installs the crash hook, once per process and for its whole life. Call it right after
/// [`dispatch_crash_monitor`], before anything else installs signal handlers: JavaScriptCore's
/// then sit on top and chain real crashes down to this one.
pub fn attach() {
    if DUMPS_DISABLED {
        return;
    }
    HANDLER.get_or_init(|| {
        CRASH_TEST.store(
            crash_test_requested(std::env::var_os(CRASH_TEST_ENV).as_deref()),
            Ordering::Relaxed,
        );
        // SAFETY: `on_crash` runs in a compromised context. It reads an atomic, clones a handle
        // under a lock it only tries, and sends the context; it neither allocates nor logs.
        CrashHandler::attach(unsafe { crash_handler::make_crash_event(on_crash) }).ok()
    });
}

fn on_crash(context: &CrashContext) -> CrashEventResult {
    if !ARMED.load(Ordering::Acquire) {
        return CrashEventResult::Handled(false);
    }
    // Only an `Arc` clone happens under the lock: a hung monitor may stall this thread, but
    // never the detector, which replaces that monitor and so releases this thread.
    let Some(client) = CLIENT.try_lock().ok().and_then(|slot| slot.clone()) else {
        return CrashEventResult::Handled(false);
    };
    let dumped = client.request_dump(context).is_ok();
    // Never dropped here: if the detector let go of it meanwhile, this is the last reference,
    // and freeing it is not safe in a compromised context. The process ends anyway.
    std::mem::forget(client);
    // Elsewhere a handled crash still reaches the OS record (systemd-coredump, ReportCrash).
    // Windows ends the process without Windows Error Reporting once the filter handles it, and
    // the monitor acknowledges a request even when writing the dump failed, so there the
    // crash stays unhandled after the dump and WER keeps its record.
    CrashEventResult::Handled(dumped && !cfg!(windows))
}

/// The monitor child's arguments, after its executable.
pub fn monitor_args(socket: &Path, dir: &Path, parent_pid: u32) -> Vec<OsString> {
    vec![
        MONITOR_FLAG.into(),
        socket.into(),
        dir.into(),
        parent_pid.to_string().into(),
    ]
}

#[derive(Debug, PartialEq, Eq)]
struct MonitorArgs {
    socket: PathBuf,
    /// The profiler directory: dumps go to its `crashes/`.
    dir: PathBuf,
    parent_pid: u32,
}

/// The arguments after `--crash-monitor`.
fn parse_monitor_args(args: &[OsString]) -> Option<MonitorArgs> {
    let [socket, dir, parent_pid] = args else {
        return None;
    };
    Some(MonitorArgs {
        socket: socket.into(),
        dir: dir.into(),
        parent_pid: parent_pid.to_str()?.parse().ok()?,
    })
}

fn run_monitor(args: MonitorArgs) -> i32 {
    // The parent stays covered while it shuts down gracefully; the monitor leaves once the
    // parent is gone instead. Its own process group already keeps a terminal's Ctrl-C away.
    #[cfg(unix)]
    // SAFETY: only signal dispositions change, before any other thread exists.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    let Ok(mut server) = minidumper::Server::with_name(SocketName::Path(&args.socket)) else {
        return 1;
    };
    if !watch_parent(args.parent_pid) {
        return 1;
    }
    let handler = Handler::new(&args.dir, args.parent_pid, monitor_process_kind());
    // In minidumper 0.11 only `SHUTDOWN` or a handler's `Exit` end the loop: the stale timeout
    // drops idle connections, and a disconnect continues. It has to keep serving, because the
    // parent reconnects after a dump (a Linux `Client` serves one, upstream issue #99).
    match server.run(Box::new(handler), &SHUTDOWN, Some(STALE_TIMEOUT)) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// Sets `SHUTDOWN` once the parent is gone. Returns false if the parent cannot be watched,
/// so the monitor never outlives it.
#[cfg(unix)]
fn watch_parent(parent_pid: u32) -> bool {
    let Ok(parent) = libc::pid_t::try_from(parent_pid) else {
        return false;
    };
    std::thread::Builder::new()
        .name("bibcode-crash-parent-watch".into())
        .spawn(move || {
            while !parent_gone(parent) {
                std::thread::sleep(PARENT_CHECK_EVERY);
            }
            SHUTDOWN.store(true, Ordering::Relaxed);
        })
        .is_ok()
}

/// Reparenting proves the parent died, even when its pid has been reused since.
#[cfg(unix)]
fn parent_gone(parent: libc::pid_t) -> bool {
    // SAFETY: `getppid` and a signal-0 `kill` take plain values and touch no memory.
    let reparented = unsafe { libc::getppid() } != parent;
    // SAFETY: as above.
    let missing = unsafe { libc::kill(parent, 0) } == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    reparented || missing
}

/// Sets `SHUTDOWN` once the parent is gone. Returns false if the parent cannot be watched,
/// so the monitor never outlives it.
#[cfg(windows)]
fn watch_parent(parent_pid: u32) -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::{
        Foundation::WAIT_TIMEOUT,
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };

    // Opened while the parent waits for this monitor to connect, so the handle is that process
    // even if its pid is reused later.
    // SAFETY: `OpenProcess` takes plain values; a null handle is checked below.
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
    if raw.is_null() {
        return false;
    }
    // SAFETY: `raw` is a valid handle that nothing else owns.
    let parent = unsafe { OwnedHandle::from_raw_handle(raw) };
    let wait_ms = u32::try_from(PARENT_CHECK_EVERY.as_millis()).unwrap_or(u32::MAX);
    std::thread::Builder::new()
        .name("bibcode-crash-parent-watch".into())
        .spawn(move || {
            // SAFETY: `parent` stays open for as long as this thread waits on it.
            while unsafe { WaitForSingleObject(parent.as_raw_handle(), wait_ms) } == WAIT_TIMEOUT {}
            SHUTDOWN.store(true, Ordering::Relaxed);
        })
        .is_ok()
}

fn monitor_process_kind() -> String {
    std::env::var(PROCESS_KIND_ENV)
        .ok()
        .filter(|kind| !kind.is_empty())
        .or_else(|| {
            let executable = std::env::current_exe().ok()?;
            Some(executable.file_stem()?.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The monitor's side: names each dump, writes its sidecar, keeps the newest 20, and holds the
/// parent's latest event lines for the next sidecar.
struct Handler {
    crashes: PathBuf,
    parent_pid: u32,
    process_kind: String,
    last_events: Mutex<Vec<u8>>,
    /// The dump being written, deleted again if writing it fails.
    writing: Mutex<Option<PathBuf>>,
}

impl Handler {
    fn new(dir: &Path, parent_pid: u32, process_kind: String) -> Self {
        Self {
            crashes: dir.join(store::CRASHES_DIR),
            parent_pid,
            process_kind,
            last_events: Mutex::default(),
            writing: Mutex::default(),
        }
    }

    fn sidecar(&self, id: &str) -> Value {
        json!({
            "id": id,
            "createdAt": store::now_rfc3339(),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "processKind": self.process_kind,
            "parentPid": self.parent_pid,
            "lastEvents": event_values(&lock(&self.last_events)),
        })
    }
}

impl ServerHandler for Handler {
    fn create_minidump_file(&self) -> io::Result<(File, PathBuf)> {
        let mut crashes = fs::DirBuilder::new();
        crashes.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut crashes, 0o700);
        crashes.create(&self.crashes)?;
        let path = self.crashes.join(format!("{}.dmp", uuid::Uuid::new_v4()));
        let file = create_private(&path)?;
        *lock(&self.writing) = Some(path.clone());
        Ok((file, path))
    }

    fn on_minidump_created(&self, result: Result<MinidumpBinary, minidumper::Error>) -> LoopAction {
        let writing = lock(&self.writing).take();
        match result {
            Ok(binary) => {
                drop(binary.file);
                if let Some(id) = binary.path.file_stem().and_then(OsStr::to_str) {
                    let sidecar = self.sidecar(id).to_string();
                    let _ = create_private(&binary.path.with_extension("json"))
                        .and_then(|mut file| io::Write::write_all(&mut file, sidecar.as_bytes()));
                }
                prune(&self.crashes, MAX_DUMPS);
            }
            Err(_) => {
                if let Some(path) = writing {
                    let _ = fs::remove_file(path);
                }
            }
        }
        LoopAction::Continue
    }

    fn on_message(&self, kind: u32, buffer: Vec<u8>) {
        if kind == LAST_EVENTS {
            *lock(&self.last_events) = buffer;
        }
    }
}

/// A new file only this user may read, whatever the umask: a dump holds raw process memory,
/// prompts and tokens included.
fn create_private(path: &Path) -> io::Result<File> {
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// The event lines a parent sent, as JSON, capped as the parent caps them.
fn event_values(bytes: &[u8]) -> Vec<Value> {
    let start = bytes.len().saturating_sub(LAST_EVENTS_MAX_BYTES);
    let values: Vec<Value> = bytes[start..]
        .split(|&byte| byte == b'\n')
        // A cut-off first line.
        .skip(usize::from(start > 0))
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    let excess = values.len().saturating_sub(LAST_EVENTS_LINES);
    values.into_iter().skip(excess).collect()
}

/// Keeps the `keep` newest dumps in `crashes`. Each older dump is deleted before its sidecar,
/// so a failed delete never leaves a dump without one.
fn prune(crashes: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(crashes) else {
        return;
    };
    let mut dumps: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "dmp"))
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    dumps.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, dump) in dumps.into_iter().skip(keep) {
        if fs::remove_file(&dump).is_ok() {
            let _ = fs::remove_file(dump.with_extension("json"));
        }
    }
}

/// Keeps a monitor child running while the profiler is on: spawns it, connects, and arms the
/// hook; then every 10 s pings it, replacing a child that died and reconnecting a connection
/// that did (a Linux `Client` serves one dump). Cancelling disarms the hook and kills the
/// child. It records in `capabilities.json` what this host can capture.
#[derive(Clone)]
pub struct MonitorDetector {
    /// The profiler directory: dumps go to its `crashes/`, `capabilities.json` beside them.
    dir: PathBuf,
    /// Names the process in each sidecar: `server`, `desktop` or `crash-smoke`.
    process_kind: &'static str,
    /// The binary to re-execute; this process's own unless a test sets it.
    executable: Option<PathBuf>,
}

impl MonitorDetector {
    pub fn new(dir: PathBuf, process_kind: &'static str) -> Self {
        Self {
            dir,
            process_kind,
            executable: None,
        }
    }

    /// Re-executes `executable` instead of this process's binary. A test harness cannot be the
    /// monitor; every BiBCode binary can, since each dispatches `--crash-monitor` first.
    #[must_use]
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = Some(executable);
        self
    }
}

impl Detector for MonitorDetector {
    fn name(&self) -> &'static str {
        "crash-monitor"
    }

    fn start(&self, cancel: CancellationToken) -> JoinHandle<()> {
        let detector = self.clone();
        // On a thread and a runtime of its own: a fault on an application worker that waits
        // on a hung monitor must not stall the heartbeat that replaces that monitor.
        tokio::task::spawn_blocking(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(keep_monitor(detector, cancel)),
                Err(error) => tracing::warn!(%error, "could not start the crash monitor"),
            }
        })
    }
}

async fn keep_monitor(detector: MonitorDetector, cancel: CancellationToken) {
    if DUMPS_DISABLED {
        record_capability(&detector.dir, "disabled").await;
        return;
    }
    if let Err(error) = secure_crashes_dir(&detector.dir).await {
        tracing::warn!(
            %error,
            "native crash dumps are unavailable: their directory cannot be made private"
        );
        record_capability(&detector.dir, "unavailable").await;
        return;
    }
    let mut monitor = None;
    let mut failing = false;
    let mut ticks = tokio::time::interval(PING_EVERY);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = ticks.tick() => {}
        }
        match keep_alive(&detector, &mut monitor, &cancel).await {
            Round::Live => failing = false,
            Round::Failed(reason) => {
                // Once per outage: where the monitor can never start, every round fails.
                if !failing {
                    tracing::warn!("{reason}; retrying every 10 s");
                }
                failing = true;
            }
            Round::Impossible => {
                tracing::warn!(
                    "native crash dumps are unavailable: the monitor may not ptrace this process"
                );
                break;
            }
        }
    }
    let client = disarm();
    if let Some((monitor, heartbeat)) = monitor {
        monitor.stop().await;
        drop(heartbeat);
    }
    drop(client);
}

/// Makes `crashes/` readable by this user alone before any dump is written into it, since a
/// dump holds raw process memory. The monitor also creates each file 0600 on Unix; on Windows
/// the files inherit this directory's protected ACL.
async fn secure_crashes_dir(dir: &Path) -> io::Result<()> {
    let crashes = dir.join(store::CRASHES_DIR);
    tokio::fs::create_dir_all(&crashes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&crashes, std::fs::Permissions::from_mode(0o700)).await?;
    }
    #[cfg(windows)]
    crate::auth::secure_windows_path(&crashes, true).await?;
    Ok(())
}

/// What one round of [`keep_alive`] left behind.
enum Round {
    /// A monitor is connected, or the round was cancelled.
    Live,
    /// No monitor is connected; the next round tries again.
    Failed(String),
    /// Dumps are impossible on this host, which ends the detector.
    Impossible,
}

/// One round: leaves a live monitor connected. A monitor that fails its heartbeat, by dying,
/// dropping a connection or not answering within 5 s, is replaced, connections and all. The
/// slot holds the monitor with the connection only the detector pings it on; crash requests
/// go over the connection in `CLIENT`, so a ping's reply can never reach one.
async fn keep_alive(
    detector: &MonitorDetector,
    slot: &mut Option<(Monitor, Arc<Client>)>,
    cancel: &CancellationToken,
) -> Round {
    if let Some(heartbeat) = slot.as_ref().map(|(_, heartbeat)| heartbeat.clone())
        && let Some(client) = current_client()
    {
        let dir = detector.dir.clone();
        // The event lines also keep the crash connection from going stale. A hung monitor
        // blocks both calls: the timeout abandons them, and killing the monitor frees the
        // thread.
        let beat = tokio::task::spawn_blocking(move || {
            heartbeat.ping().is_ok()
                && client
                    .send_message(LAST_EVENTS, last_event_lines(&dir, MESSAGE_MAX_BYTES))
                    .is_ok()
        });
        match tokio::time::timeout(PING_TIMEOUT, until_cancelled(cancel, beat)).await {
            // Answered, or cancelled.
            Ok(Some(true) | None) => return Round::Live,
            Ok(Some(false)) | Err(_) => {
                tracing::warn!("the crash monitor stopped answering; starting another");
            }
        }
    }
    let client = disarm();
    if let Some((monitor, heartbeat)) = slot.take() {
        monitor.stop().await;
        drop(heartbeat);
    }
    drop(client);
    let monitor = match Monitor::spawn(detector) {
        Ok(monitor) => monitor,
        Err(error) => {
            record_capability(&detector.dir, "unavailable").await;
            return Round::Failed(format!("could not start the crash monitor: {error}"));
        }
    };
    let client = connect(&monitor.socket, cancel).await;
    let heartbeat = match client {
        Some(_) => connect(&monitor.socket, cancel).await,
        None => None,
    };
    let (client, heartbeat) = match (client, heartbeat) {
        (Some(client), Some(heartbeat)) => (client, heartbeat),
        unconnected => {
            monitor.stop().await;
            drop(unconnected);
            if cancel.is_cancelled() {
                return Round::Live;
            }
            record_capability(&detector.dir, "unavailable").await;
            return Round::Failed("the crash monitor did not accept a connection".to_owned());
        }
    };
    let (dir, pid) = (detector.dir.clone(), monitor.pid);
    *slot = Some((monitor, Arc::new(heartbeat)));
    // Short, and never abandoned: the disarm that follows a cancel must not run before it.
    match tokio::task::spawn_blocking(move || arm(client, pid, &dir)).await {
        Ok(true) => Round::Live,
        _ => Round::Impossible,
    }
}

/// Connects to a starting monitor, retrying every 100 ms for up to 5 s.
async fn connect(socket: &Path, cancel: &CancellationToken) -> Option<Client> {
    let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
    loop {
        let name = socket.to_path_buf();
        // On macOS this waits for the monitor's acknowledgment, so a hung monitor blocks it:
        // the deadline bounds the wait, and the caller then kills the monitor.
        let attempt =
            tokio::task::spawn_blocking(move || Client::with_name(SocketName::Path(&name)).ok());
        match tokio::time::timeout_at(deadline, until_cancelled(cancel, attempt)).await {
            Ok(Some(Some(client))) => return Some(client),
            Ok(Some(None)) => {}
            // Cancelled, or past the deadline.
            Ok(None) | Err(_) => return None,
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return None,
            () = tokio::time::sleep(CONNECT_RETRY) => {}
        }
    }
}

/// The task's output, or `None` once `cancel` fires first (or the task panicked).
async fn until_cancelled<T>(cancel: &CancellationToken, task: JoinHandle<T>) -> Option<T> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        result = task => result.ok(),
    }
}

/// Hands the connection to the crash hook and records that dumps are available. On Linux it
/// first checks that the monitor will be allowed to ptrace this process; if not, nothing is
/// armed and dumps are recorded as unavailable.
fn arm(client: Client, monitor_pid: u32, dir: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        if !ptrace_permitted(monitor_pid) {
            write_capabilities(dir, "unavailable");
            return false;
        }
        if let Some(Some(handler)) = HANDLER.get() {
            handler.set_ptracer(Some(monitor_pid));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = monitor_pid;
    let client = Arc::new(client);
    let _ = client.send_message(LAST_EVENTS, last_event_lines(dir, MESSAGE_MAX_BYTES));
    let replaced = lock(&CLIENT).replace(client);
    drop(replaced);
    ARMED.store(true, Ordering::Release);
    write_capabilities(dir, "available");
    if CRASH_TEST.swap(false, Ordering::Relaxed) {
        start_crash_test();
    }
    true
}

/// Whether the monitor may ptrace this process, which the Linux dump writer needs. Without
/// Yama, classic ptrace rules apply. Scope 3 forbids ptrace, and scope 2 allows it only with
/// `CAP_SYS_PTRACE`, which the monitor inherits from this process. Below that, the probe must
/// be able to name the monitor as this process's ptracer (`PR_SET_PTRACER`, which scope 1
/// requires); it withdraws the grant at once, as crash-handler grants it again for the moment
/// of a crash.
#[cfg(target_os = "linux")]
fn ptrace_permitted(monitor_pid: u32) -> bool {
    let Ok(scope) = fs::read_to_string(YAMA_SCOPE) else {
        return true;
    };
    if ptrace_scope_blocks(&scope, has_cap_sys_ptrace()) {
        return false;
    }
    let none: libc::c_ulong = 0;
    // SAFETY: `prctl` with integer arguments only.
    let granted = unsafe {
        libc::prctl(
            libc::PR_SET_PTRACER,
            libc::c_ulong::from(monitor_pid),
            none,
            none,
            none,
        )
    } == 0;
    // SAFETY: as above; 0 withdraws the grant.
    unsafe { libc::prctl(libc::PR_SET_PTRACER, none, none, none, none) };
    granted
}

#[cfg(target_os = "linux")]
fn ptrace_scope_blocks(scope: &str, cap_sys_ptrace: bool) -> bool {
    match scope.trim().parse::<u32>() {
        Ok(2) => !cap_sys_ptrace,
        Ok(scope) => scope >= 3,
        Err(_) => false,
    }
}

/// `CAP_SYS_PTRACE` (capability 19) in this process's effective set.
#[cfg(target_os = "linux")]
fn has_cap_sys_ptrace() -> bool {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            let mask = status
                .lines()
                .find_map(|line| line.strip_prefix("CapEff:"))?;
            u64::from_str_radix(mask.trim(), 16).ok()
        })
        .is_some_and(|mask| mask & (1 << 19) != 0)
}

fn current_client() -> Option<Arc<Client>> {
    lock(&CLIENT).clone()
}

/// Disarms the hook and hands back its connection. Callers drop it only once the monitor is
/// gone: on Windows, dropping a connection waits for the monitor to close its end.
fn disarm() -> Option<Arc<Client>> {
    ARMED.store(false, Ordering::Release);
    lock(&CLIENT).take()
}

/// A monitor child.
struct Monitor {
    child: tokio::process::Child,
    pid: u32,
    socket: PathBuf,
}

impl Monitor {
    fn spawn(detector: &MonitorDetector) -> io::Result<Self> {
        let executable = match &detector.executable {
            Some(executable) => executable.clone(),
            // The running binary itself, even if its path was replaced since: the monitor must
            // be the same build, because the IPC protocol is.
            None if cfg!(target_os = "linux") => PathBuf::from("/proc/self/exe"),
            None => std::env::current_exe()?,
        };
        // Short: macOS caps a socket path at 104 bytes, and its temporary directory is long.
        let id = uuid::Uuid::new_v4().simple().to_string();
        let socket = std::env::temp_dir().join(format!("bibcode-crash-{}.sock", &id[..12]));
        let mut command = tokio::process::Command::new(executable);
        command
            .args(monitor_args(&socket, &detector.dir, std::process::id()))
            .env(PROCESS_KIND_ENV, detector.process_kind)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Its own process group, so a terminal's Ctrl-C reaches only the parent.
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        command.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP
                | windows_sys::Win32::System::Threading::CREATE_NO_WINDOW,
        );
        let child = command.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("the crash monitor exited at once"))?;
        Ok(Self { child, pid, socket })
    }

    /// Kills and reaps the child, and removes the socket a killed monitor leaves behind:
    /// first, because a parent crashing on this monitor ends as soon as the monitor does.
    async fn stop(mut self) {
        let unlinked = tokio::fs::remove_file(&self.socket).await.is_ok();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        if !unlinked {
            let _ = tokio::fs::remove_file(&self.socket).await;
        }
    }
}

/// Records what this host can capture, for the profiler status. OS crash records are
/// available where the previous-run harvest has a facility to read.
fn write_capabilities(dir: &Path, native_dumps: &str) {
    let os_crash_records = match os_records::facility() {
        "no systemd-coredump" | "none" => "unavailable",
        _ => "available",
    };
    let body = json!({ "nativeDumps": native_dumps, "osCrashRecords": os_crash_records });
    let path = dir.join(CAPABILITIES_FILE);
    let staged = dir.join("capabilities.json.tmp");
    // Replaced whole, so the status never reads half a file.
    let written = fs::create_dir_all(dir)
        .and_then(|()| fs::write(&staged, body.to_string()))
        .and_then(|()| fs::rename(&staged, &path));
    if let Err(error) = written {
        tracing::warn!(%error, "could not record the profiler's capabilities");
    }
}

async fn record_capability(dir: &Path, native_dumps: &'static str) {
    let dir = dir.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || write_capabilities(&dir, native_dumps)).await;
}

/// The newest complete lines of the newest events file, oldest first: at most 50 lines and
/// `max_bytes` bytes. The store's collectors redacted them already.
fn last_event_lines(dir: &Path, max_bytes: usize) -> Vec<u8> {
    let newest = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let index = store::event_file_index(&entry.file_name().to_string_lossy())?;
            Some((index, entry.path()))
        })
        .max_by_key(|(index, _)| *index);
    newest
        .and_then(|(_, path)| tail_lines(&path, max_bytes).ok())
        .unwrap_or_default()
}

fn tail_lines(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let start = file
        .metadata()?
        .len()
        .saturating_sub(u64::try_from(max_bytes).unwrap_or(u64::MAX));
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    file.take(u64::try_from(max_bytes).unwrap_or(u64::MAX))
        .read_to_end(&mut tail)?;
    let mut lines: Vec<&[u8]> = tail.split(|&byte| byte == b'\n').collect();
    // What follows the last newline: nothing, or a line still being written.
    lines.pop();
    // A first line that began before `start` is cut off.
    let skip = usize::from(start > 0).max(lines.len().saturating_sub(LAST_EVENTS_LINES));
    let mut kept = Vec::with_capacity(tail.len());
    for line in lines.into_iter().skip(skip) {
        kept.extend_from_slice(line);
        kept.push(b'\n');
    }
    Ok(kept)
}

/// Exactly `"1"` asks for the crash test; anything else, or nothing, is ignored.
fn crash_test_requested(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

/// The runbooks' deliberate crash: 5 s after arming, if still armed.
fn start_crash_test() {
    let _ = std::thread::Builder::new()
        .name("bibcode-crash-test".into())
        .spawn(|| {
            std::thread::sleep(CRASH_TEST_DELAY);
            if ARMED.load(Ordering::Acquire) {
                crash_test_fault();
            }
        });
}

#[inline(never)]
fn crash_test_fault() {
    // A write into the unmapped first page. Not to null itself: debug builds check writes
    // through null pointers and abort before the fault happens.
    // SAFETY: none. The crash test asks for exactly this fault.
    unsafe { std::ptr::dangling_mut::<u8>().write_volatile(1) }
}

/// Runs `detector` while `setting` is on, until the setting's sender is gone. A change stops a
/// running detector before the next start, so two never run at once.
pub async fn follow(detector: impl Detector, mut setting: watch::Receiver<bool>) {
    let mut running: Option<(CancellationToken, JoinHandle<()>)> = None;
    loop {
        let want = *setting.borrow_and_update();
        running = match (want, running) {
            (true, None) => {
                let cancel = CancellationToken::new();
                let task = detector.start(cancel.clone());
                Some((cancel, task))
            }
            (false, Some((cancel, task))) => {
                cancel.cancel();
                let _ = task.await;
                None
            }
            (_, unchanged) => unchanged,
        };
        if setting.changed().await.is_err() {
            break;
        }
    }
    if let Some((cancel, task)) = running {
        cancel.cancel();
        let _ = task.await;
    }
}

/// The desktop host's local environment, read before Tauri starts: its profiler directory and
/// committed `enableProfiler`. The host resolves `BIBCODE_HOME` or `~/.bibcode` as its backend
/// does, and its in-process server never runs with a dev URL, so the state is in `userdata/`.
pub fn desktop_local_profiler() -> Option<(PathBuf, bool)> {
    let home = dirs::home_dir()?;
    let request = match std::env::var_os("BIBCODE_HOME") {
        Some(root) => DataRootRequest::explicit(DataRootSource::Environment, root.into(), home),
        None => DataRootRequest::default(home),
    };
    let state_dir = ServerConfig::new(resolve_data_root(request).ok()?.effective).state_dir();
    let enabled = fs::read(state_dir.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|settings| settings.get("enableProfiler")?.as_bool())
        .unwrap_or(false);
    Some((profiler_dir(&state_dir), enabled))
}

/// For the `crash-smoke` binary: runs a [`MonitorDetector`] for `dir` on a runtime that lives
/// as long as the process, and returns once the hook is armed.
#[doc(hidden)]
pub fn arm_for_test(dir: &Path) {
    let runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("crash-smoke runtime"),
    ));
    let _entered = runtime.enter();
    // Never cancelled: the process ends by crashing or by being killed.
    drop(MonitorDetector::new(dir.to_path_buf(), "crash-smoke").start(CancellationToken::new()));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !ARMED.load(Ordering::Acquire) {
        assert!(
            std::time::Instant::now() < deadline,
            "the crash monitor did not connect"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::atomic::{AtomicI64, Ordering::SeqCst},
    };

    use super::*;

    #[test]
    fn the_monitor_argv_names_the_socket_the_directory_and_the_parent() {
        let socket = Path::new("/tmp/bibcode-crash-0123456789ab.sock");
        let dir = Path::new("/data/userdata/profiler");
        let args = monitor_args(socket, dir, 4242);
        assert_eq!(
            args,
            [
                "--crash-monitor",
                "/tmp/bibcode-crash-0123456789ab.sock",
                "/data/userdata/profiler",
                "4242"
            ]
            .map(OsString::from)
        );
        assert_eq!(
            parse_monitor_args(&args[1..]),
            Some(MonitorArgs {
                socket: socket.to_path_buf(),
                dir: dir.to_path_buf(),
                parent_pid: 4242,
            })
        );
    }

    #[test]
    fn other_argv_is_not_a_monitor() {
        // The test binary's own argv.
        assert_eq!(dispatch_crash_monitor(), None);
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse_monitor_args(&args(&["/s", "/d"])), None);
        assert_eq!(parse_monitor_args(&args(&["/s", "/d", "12", "x"])), None);
        assert_eq!(parse_monitor_args(&args(&["/s", "/d", "pid"])), None);
    }

    #[test]
    fn only_exactly_1_requests_the_crash_test() {
        assert!(crash_test_requested(Some(OsStr::new("1"))));
        for ignored in [None, Some("0"), Some("true"), Some(" 1"), Some("")] {
            assert!(
                !crash_test_requested(ignored.map(OsStr::new)),
                "{ignored:?} must be ignored"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn yama_scope_3_or_2_without_the_capability_disables_ptrace() {
        assert!(ptrace_scope_blocks("3\n", true));
        assert!(ptrace_scope_blocks("2\n", false));
        assert!(!ptrace_scope_blocks("2\n", true));
        for allowed in ["0\n", "1\n", "", "junk"] {
            assert!(!ptrace_scope_blocks(allowed, false), "{allowed:?}");
        }
    }

    fn event_line(seq: usize) -> String {
        format!(r#"{{"v":1,"seq":{seq},"kind":"process.sample"}}"#)
    }

    #[test]
    fn the_monitor_gets_the_newest_complete_event_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("events-000000.ndjson"),
            format!("{}\n", event_line(999)),
        )
        .unwrap();
        let lines: String = (0..60)
            .map(|seq| format!("{}\n", event_line(seq)))
            .collect();
        // A torn last line, still being written.
        std::fs::write(
            dir.path().join("events-000001.ndjson"),
            format!("{lines}{{\"v\":1,\"seq\":60"),
        )
        .unwrap();

        let sent = String::from_utf8(last_event_lines(dir.path(), 64 * 1024)).unwrap();
        let expected: String = (10..60)
            .map(|seq| format!("{}\n", event_line(seq)))
            .collect();
        assert_eq!(
            sent, expected,
            "the last 50 complete lines of the newest file"
        );

        // The byte cap keeps only whole lines.
        let capped = String::from_utf8(last_event_lines(dir.path(), 200)).unwrap();
        assert!(!capped.is_empty() && capped.len() <= 200);
        assert!(capped.ends_with(&format!("{}\n", event_line(59))));
        assert!(capped.lines().all(|line| line.starts_with("{\"v\":1")));

        assert!(last_event_lines(&dir.path().join("missing"), 1024).is_empty());
    }

    fn age(path: &Path, seconds: u64) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn a_dump_gets_a_sidecar_and_the_newest_20_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        for index in 0..20 {
            let dump = crashes.join(format!("old-{index:02}.dmp"));
            std::fs::write(&dump, b"MDMP").unwrap();
            std::fs::write(dump.with_extension("json"), b"{}").unwrap();
            age(&dump, 1000 - index);
        }
        let handler = Handler::new(dir.path(), 4242, "server".to_owned());
        let lines: String = (0..60)
            .map(|seq| format!("{}\n", event_line(seq)))
            .collect();
        handler.on_message(1, lines.into_bytes());
        handler.on_message(7, b"ignored".to_vec());

        let (mut file, path) = handler.create_minidump_file().unwrap();
        assert_eq!(path.parent(), Some(crashes.as_path()));
        file.write_all(b"MDMP").unwrap();
        let action = handler.on_minidump_created(Ok(minidumper::MinidumpBinary {
            file,
            path: path.clone(),
            contents: None,
        }));
        assert!(action == minidumper::LoopAction::Continue);

        let sidecar: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        let id = path.file_stem().unwrap().to_str().unwrap();
        assert_eq!(sidecar["id"], id);
        assert_eq!(sidecar["processKind"], "server");
        assert_eq!(sidecar["parentPid"], 4242);
        assert_eq!(sidecar["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(sidecar["os"], std::env::consts::OS);
        assert_eq!(sidecar["arch"], std::env::consts::ARCH);
        assert!(sidecar["createdAt"].is_string());
        let events = sidecar["lastEvents"].as_array().unwrap();
        assert_eq!(events.len(), 50, "capped at 50 lines");
        assert_eq!(events[0]["seq"], 10);
        assert_eq!(events[49]["seq"], 59);

        // The oldest dump went, with its sidecar; the new one and 19 others stay.
        assert!(!crashes.join("old-00.dmp").exists());
        assert!(!crashes.join("old-00.json").exists());
        let dumps = std::fs::read_dir(&crashes)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "dmp")
            })
            .count();
        assert_eq!(dumps, 20);
        assert!(path.exists());
    }

    /// A dump holds raw process memory: only this user may read it, whatever the umask.
    #[cfg(unix)]
    #[test]
    fn dumps_and_sidecars_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let handler = Handler::new(dir.path(), 1, "server".to_owned());
        let (file, path) = handler.create_minidump_file().unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("crashes")), 0o700);
        handler.on_minidump_created(Ok(minidumper::MinidumpBinary {
            file,
            path: path.clone(),
            contents: None,
        }));
        assert_eq!(mode(&path.with_extension("json")), 0o600);
    }

    #[test]
    fn a_failed_dump_leaves_no_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let handler = Handler::new(dir.path(), 1, "server".to_owned());
        let (_file, path) = handler.create_minidump_file().unwrap();
        assert!(path.exists());
        let action =
            handler.on_minidump_created(Err(minidumper::Error::ProtocolError("ptrace denied")));
        assert!(action == minidumper::LoopAction::Continue);
        assert!(!path.exists());
        assert!(!path.with_extension("json").exists());
    }

    /// Counts its live instances; each takes 20 ms to wind down once cancelled.
    #[derive(Clone, Default)]
    struct Counting {
        live: Arc<AtomicI64>,
        max: Arc<AtomicI64>,
    }

    impl Detector for Counting {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn start(&self, cancel: CancellationToken) -> JoinHandle<()> {
            let counts = self.clone();
            let live = counts.live.fetch_add(1, SeqCst) + 1;
            counts.max.fetch_max(live, SeqCst);
            tokio::spawn(async move {
                cancel.cancelled().await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                counts.live.fetch_sub(1, SeqCst);
            })
        }
    }

    #[tokio::test]
    async fn follow_runs_one_detector_while_the_setting_is_on() {
        let counts = Counting::default();
        let (setting, follower) = watch::channel(false);
        let following = tokio::spawn(follow(counts.clone(), follower));
        for on in [true, false, true, false, true] {
            setting.send(on).unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(counts.live.load(SeqCst), 1, "the last flip turned it on");
        assert_eq!(counts.max.load(SeqCst), 1, "two detectors ran at once");
        drop(setting);
        following.await.unwrap();
        assert_eq!(
            counts.live.load(SeqCst),
            0,
            "with its setting gone, it stops"
        );
    }
}
