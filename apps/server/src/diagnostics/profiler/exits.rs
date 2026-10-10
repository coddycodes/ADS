//! Unrequested deaths of the processes BiBCode runs (shells, provider CLIs, git) and of the
//! app window's web content process.
//!
//! Exit paths check [`watching`] before building anything, so with the profiler off an exit
//! costs one read lock.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Map, Value, json};

use super::{
    amend_finding, emit,
    findings::{FindingInput, Occurrence, Severity},
    os_records, record_finding,
    redact::redact_message,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{os_records::OsCrashRecord, redact::redact_path};

/// How far from a death an OS report may be written and still be taken as its report.
const REPORT_WINDOW: Duration = Duration::from_secs(60);
/// How long a stop asked for counts: the process must exit within it.
const STOP_REQUEST_MEMORY: Duration = Duration::from_secs(60);
const STOP_REQUESTS_KEPT: usize = 64;

/// Processes someone is stopping on purpose: BiBCode itself, or the user from the process
/// list.
static STOP_REQUESTS: Mutex<StopRequests> = Mutex::new(StopRequests::new());

/// How a process was asked to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// Killed or shut down: whatever exit follows was asked for.
    Ended,
    /// Interrupted: a process may handle an interrupt and keep running, so a fault it dies of
    /// afterwards is still its own.
    Interrupted,
}

/// Stops asked for: pid, when, and how; oldest first.
#[derive(Default)]
struct StopRequests(VecDeque<(u32, Instant, Stop)>);

impl StopRequests {
    const fn new() -> Self {
        Self(VecDeque::new())
    }

    fn note(&mut self, pid: u32, stop: Stop) {
        if self.0.len() == STOP_REQUESTS_KEPT {
            self.0.pop_front();
        }
        self.0.push_back((pid, Instant::now(), stop));
    }

    fn withdraw(&mut self, pid: u32) {
        if let Some(index) = self.0.iter().rposition(|(noted, ..)| *noted == pid) {
            self.0.remove(index);
        }
    }

    /// The latest stop asked for `pid` within the last minute, and not before `started`: a
    /// stop asked earlier was for another process that had the same pid.
    fn latest(&self, pid: u32, started: Option<Instant>) -> Option<Stop> {
        self.0
            .iter()
            .rev()
            .find(|(noted, at, _)| {
                *noted == pid
                    && at.elapsed() < STOP_REQUEST_MEMORY
                    && started.is_none_or(|started| *at >= started)
            })
            .map(|(.., stop)| *stop)
    }
}

/// Runs `apply` on the stop requests a test captures on this thread, or on the process's.
fn with_stop_requests<T>(apply: impl Fn(&mut StopRequests) -> T) -> T {
    #[cfg(test)]
    if let Some(result) = CAPTURED.with(|captured| {
        captured
            .borrow_mut()
            .as_mut()
            .map(|captured| apply(&mut captured.stops))
    }) {
        return result;
    }
    apply(&mut STOP_REQUESTS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// How a process's exit came about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass {
    Requested,
    Clean,
    NonZero(i32),
    /// A Unix signal, or a Windows exception code (`0xC…`, such as an access violation).
    Signal {
        signal: i32,
        likely_oom: bool,
    },
}

/// Which unrequested exits count as crashes where a process is watched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watch {
    /// Signal and non-zero exits: the process should not end on its own.
    Failures,
    /// Signal exits only: non-zero is normal for git.
    Signals,
    /// Signals people do not send from a terminal. In one, Ctrl-C, Ctrl-\, `kill` and a
    /// closed terminal end processes on purpose, and `exit 1` is ordinary.
    CrashSignals,
}

/// A watched process that exited.
pub struct Exited<'a> {
    /// `terminal`, `provider` or `command`.
    pub kind: &'a str,
    /// What findings name it: `shell`, a provider driver or a command's executable.
    pub label: &'a str,
    /// Its executable; OS crash records are matched by its file name.
    pub exe: &'a str,
    pub pid: u32,
    pub uptime: Duration,
}

/// How `status` came about. `requested` is true when BiBCode or the user asked the process
/// to stop: whatever its status, it did not die on its own.
pub fn classify_exit(status: &ExitStatus, requested: bool) -> ExitClass {
    if requested {
        return ExitClass::Requested;
    }
    if status.success() {
        return ExitClass::Clean;
    }
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
        return ExitClass::Signal {
            signal,
            likely_oom: signal == libc::SIGKILL,
        };
    }
    match status.code() {
        // An NTSTATUS error (0xC…): the process died of an exception.
        #[cfg(windows)]
        Some(code) if code as u32 >= 0xC000_0000 => ExitClass::Signal {
            signal: code,
            likely_oom: false,
        },
        Some(code) => ExitClass::NonZero(code),
        // A stopped or continued process has not exited.
        None => ExitClass::Clean,
    }
}

/// Whether exits are reported. Call sites check it before they build a report.
pub fn watching() -> bool {
    // Lib tests share the process-global profiler with tests that turn it on, so only a test
    // capturing exits on its own thread watches them.
    #[cfg(test)]
    {
        CAPTURED.with(|captured| captured.borrow().is_some())
    }
    #[cfg(not(test))]
    {
        super::is_enabled()
    }
}

/// Reports a watched process's exit when `watch` counts it as a crash: a `subprocess.exit`
/// event and a `subprocess.crash` finding. Does nothing while the profiler is off.
pub fn report_exit(watch: Watch, process: &Exited<'_>, status: &ExitStatus, requested: bool) {
    if !watching() {
        return;
    }
    let class = classify_exit(status, requested);
    if !counts(watch, class) {
        return;
    }
    let started = Instant::now().checked_sub(process.uptime);
    match with_stop_requests(|requests| requests.latest(process.pid, started)) {
        Some(Stop::Ended) => return,
        Some(Stop::Interrupted) if !faulted(class) => return,
        Some(Stop::Interrupted) | None => {}
    }
    let label = redact_message(process.label);
    let (kind, uptime) = (process.kind, human(process.uptime));
    let (outcome, likely_oom, severity, title, summary) = match class {
        ExitClass::Signal {
            signal,
            likely_oom: true,
        } => (
            signal_name(signal),
            true,
            Severity::Error,
            format!("{label} was killed, likely for using too much memory"),
            format!(
                "The {kind} process {label} got SIGKILL after {uptime}. BiBCode did not send it; \
                 the kernel's OOM killer or systemd-oomd usually does."
            ),
        ),
        ExitClass::Signal { signal, .. } => {
            let name = signal_name(signal);
            (
                name.clone(),
                false,
                Severity::Error,
                format!("{label} died with {name}"),
                format!(
                    "The {kind} process {label} died with {name} after {uptime}; BiBCode did \
                     not stop it."
                ),
            )
        }
        ExitClass::NonZero(code) => (
            format!("exit {code}"),
            false,
            Severity::Warn,
            format!("{label} exited with code {code}"),
            format!(
                "The {kind} process {label} exited with code {code} after {uptime}; BiBCode did \
                 not stop it."
            ),
        ),
        ExitClass::Requested | ExitClass::Clean => return,
    };
    let fields = vec![kind.to_owned(), label.clone(), outcome.clone()];
    #[cfg(test)]
    if captured(&fields) {
        return;
    }
    let uptime_ms = u64::try_from(process.uptime.as_millis()).unwrap_or(u64::MAX);
    emit(
        "subprocess.exit",
        json!({
            "processKind": kind,
            "label": label,
            "pid": process.pid,
            "outcome": outcome,
            "likelyOom": likely_oom,
            "uptimeMs": uptime_ms,
        }),
    );
    let occurrence = record_finding(FindingInput {
        kind: "subprocess.crash",
        severity,
        source: "server".into(),
        fingerprint_fields: fields,
        title,
        summary,
        evidence: json!({
            "pid": process.pid,
            "outcome": outcome,
            "likelyOom": likely_oom,
            "uptimeMs": uptime_ms,
        }),
    });
    #[cfg(target_os = "linux")]
    if let (Some(occurrence), ExitClass::Signal { signal, .. }) = (occurrence, class)
        && dumps_core(signal)
    {
        attach_coredump(occurrence, process);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = occurrence;
}

/// Records that `pid` is about to be stopped on purpose, by BiBCode or by the user from the
/// process list, so the exit that follows within a minute is not reported. Call it before
/// the stop: the exit can be observed before the stopping call returns.
pub fn note_stop_requested(pid: u32) {
    with_stop_requests(|requests| requests.note(pid, Stop::Ended));
}

/// Records that the user is about to interrupt `pid` from the process list. A process may
/// handle the interrupt and keep running, so only a fault it dies of afterwards is reported.
pub fn note_interrupt_requested(pid: u32) {
    with_stop_requests(|requests| requests.note(pid, Stop::Interrupted));
}

/// Withdraws the latest stop noted for `pid`: the stop was not carried out, so a death that
/// follows is the process's own.
pub fn withdraw_stop_request(pid: u32) {
    with_stop_requests(|requests| requests.withdraw(pid));
}

/// The app window's web content process ended: a `crash.webview` finding. On macOS, the
/// crash report written for it is looked for during the next minute and its frames added.
pub fn report_webview_terminated(reason: &str) {
    let occurrence = record_webview(reason, Map::new());
    #[cfg(target_os = "macos")]
    if let Some(occurrence) = occurrence {
        attach_web_content_report(occurrence);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = occurrence;
}

/// WebView2's `ProcessFailed`: a `crash.webview` finding with the exit code and the name of
/// the minidump WebView2 wrote to `report_dir`, if any.
pub fn report_webview_failed(reason: &str, exit_code: i32, report_dir: Option<PathBuf>) {
    let occurrence = record_webview(
        reason,
        Map::from_iter([("exitCode".to_owned(), exit_code.into())]),
    );
    let (Some(occurrence), Some(dir)) = (occurrence, report_dir) else {
        return;
    };
    let at = SystemTime::now();
    // Not on the UI thread that reports the failure: the folder may sit on a slow disk.
    let lookup = std::thread::Builder::new()
        .name("bibcode-webview-dump".into())
        .spawn(move || {
            if let Some(dump) = newest_dump(&dir, at) {
                amend_finding(
                    &occurrence,
                    Map::from_iter([("dump".to_owned(), dump.into())]),
                );
            }
        });
    if let Err(error) = lookup {
        tracing::debug!(%error, "webview minidump lookup did not start");
    }
}

fn record_webview(reason: &str, mut evidence: Map<String, Value>) -> Option<Occurrence> {
    let reason = redact_message(reason);
    evidence.insert("reason".to_owned(), reason.clone().into());
    record_finding(FindingInput {
        kind: "crash.webview",
        severity: Severity::Error,
        source: "server".into(),
        fingerprint_fields: vec![reason.clone()],
        title: "The app window's web content process ended".to_owned(),
        summary: format!(
            "The process that renders the app window ended ({reason}); the window may stay \
             blank until it reloads."
        ),
        evidence: Value::Object(evidence),
    })
}

/// Whether the exit is a fault: a crash signal, or a Windows exception other than Ctrl-C's.
fn faulted(class: ExitClass) -> bool {
    let ExitClass::Signal { signal, .. } = class else {
        return false;
    };
    #[cfg(unix)]
    {
        [
            libc::SIGILL,
            libc::SIGTRAP,
            libc::SIGABRT,
            libc::SIGBUS,
            libc::SIGFPE,
            libc::SIGSEGV,
            libc::SIGSYS,
        ]
        .contains(&signal)
    }
    #[cfg(not(unix))]
    {
        !sent_by_people(signal)
    }
}

fn counts(watch: Watch, class: ExitClass) -> bool {
    match (watch, class) {
        (_, ExitClass::Requested | ExitClass::Clean) => false,
        (Watch::Failures, _) => true,
        (Watch::Signals, ExitClass::Signal { .. }) => true,
        (Watch::CrashSignals, ExitClass::Signal { signal, .. }) => !sent_by_people(signal),
        (Watch::Signals | Watch::CrashSignals, ExitClass::NonZero(_)) => false,
    }
}

/// Signals people end processes with: Ctrl-C, Ctrl-\, `kill`, and the hangup of a closed
/// terminal; on Windows, Ctrl-C's `STATUS_CONTROL_C_EXIT`.
fn sent_by_people(signal: i32) -> bool {
    #[cfg(unix)]
    {
        [
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGKILL,
            libc::SIGTERM,
        ]
        .contains(&signal)
    }
    #[cfg(not(unix))]
    {
        signal as u32 == 0xC000_013A
    }
}

/// `SIGSEGV`, or a Windows exception code such as `0xC0000005`.
fn signal_name(signal: i32) -> String {
    #[cfg(unix)]
    {
        let name = match signal {
            libc::SIGHUP => "SIGHUP",
            libc::SIGINT => "SIGINT",
            libc::SIGQUIT => "SIGQUIT",
            libc::SIGILL => "SIGILL",
            libc::SIGTRAP => "SIGTRAP",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS => "SIGBUS",
            libc::SIGFPE => "SIGFPE",
            libc::SIGKILL => "SIGKILL",
            libc::SIGUSR1 => "SIGUSR1",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGUSR2 => "SIGUSR2",
            libc::SIGPIPE => "SIGPIPE",
            libc::SIGALRM => "SIGALRM",
            libc::SIGTERM => "SIGTERM",
            libc::SIGSYS => "SIGSYS",
            libc::SIGXCPU => "SIGXCPU",
            libc::SIGXFSZ => "SIGXFSZ",
            _ => return format!("signal {signal}"),
        };
        name.to_owned()
    }
    #[cfg(not(unix))]
    {
        format!("{:#010X}", signal as u32)
    }
}

fn human(uptime: Duration) -> String {
    match uptime.as_secs() {
        secs @ 0..120 => format!("{secs} s"),
        secs @ 120..7200 => format!("{} min", secs / 60),
        secs => format!("{} h", secs / 3600),
    }
}

/// An OS crash record as evidence, as the previous-run crash finding has it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn os_record_evidence(record: &OsCrashRecord) -> Map<String, Value> {
    let frames: Vec<String> = record
        .frames
        .iter()
        .map(|frame| redact_path(frame))
        .collect();
    Map::from_iter([
        ("source".to_owned(), record.source.into()),
        (
            "signalOrCode".to_owned(),
            record.signal_or_code.clone().into(),
        ),
        (
            "module".to_owned(),
            record.module.as_deref().map(redact_path).into(),
        ),
        ("frames".to_owned(), frames.into()),
        ("rawRef".to_owned(), redact_path(&record.raw_ref).into()),
    ])
}

/// Signals whose default action dumps core, so systemd-coredump keeps a record.
#[cfg(target_os = "linux")]
fn dumps_core(signal: i32) -> bool {
    [
        libc::SIGQUIT,
        libc::SIGILL,
        libc::SIGTRAP,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGFPE,
        libc::SIGSEGV,
        libc::SIGSYS,
        libc::SIGXCPU,
        libc::SIGXFSZ,
    ]
    .contains(&signal)
}

/// systemd-coredump writes its record after the process has died.
#[cfg(target_os = "linux")]
const COREDUMP_POLL: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const COREDUMP_WAIT: Duration = Duration::from_secs(5);

/// Adds the systemd-coredump record of a subprocess that dumped core, matched by its pid, its
/// executable and its lifetime. Best effort, off the caller's path, given up after 5 s.
#[cfg(target_os = "linux")]
fn attach_coredump(occurrence: Occurrence, process: &Exited<'_>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let started_at = time::OffsetDateTime::now_utc() - process.uptime;
    let (pid, exe) = (process.pid, process.exe.to_owned());
    runtime.spawn(async move {
        let run = super::sentinel::SessionRecord {
            state: String::new(),
            pid,
            boot_id: super::sentinel::boot_id(),
            exe,
            version: String::new(),
            started_at: started_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            oom_kill_baseline: None,
        };
        let record = tokio::time::timeout(COREDUMP_WAIT, async {
            loop {
                tokio::time::sleep(COREDUMP_POLL).await;
                if let Some(record) = os_records::harvest_previous_crash(&run).await {
                    return record;
                }
            }
        })
        .await;
        if let Ok(record) = record {
            amend_finding(&occurrence, os_record_evidence(&record));
        }
    });
}

/// ReportCrash writes a web content process's report some seconds after it died.
#[cfg(target_os = "macos")]
const REPORT_POLL: Duration = Duration::from_secs(5);

/// Looks for the web content process's crash report during the next minute and adds its
/// crashed thread's frames.
#[cfg(target_os = "macos")]
fn attach_web_content_report(occurrence: Occurrence) {
    let Some(dir) = dirs::home_dir().map(|home| home.join("Library/Logs/DiagnosticReports")) else {
        return;
    };
    let at = SystemTime::now();
    let lookup = std::thread::Builder::new()
        .name("bibcode-webview-report".into())
        .spawn(move || {
            let deadline = Instant::now() + REPORT_WINDOW;
            loop {
                if let Some(record) = os_records::macos::web_content_report(&dir, at, REPORT_WINDOW)
                {
                    amend_finding(&occurrence, os_record_evidence(&record));
                    return;
                }
                if Instant::now() >= deadline {
                    return;
                }
                std::thread::sleep(REPORT_POLL);
            }
        });
    if let Err(error) = lookup {
        tracing::debug!(%error, "web content crash report lookup did not start");
    }
}

/// The newest minidump in WebView2's failure report folder, or in Crashpad's `reports` folder
/// inside it, written within a minute of `at`. Older dumps there belong to other failures.
fn newest_dump(dir: &Path, at: SystemTime) -> Option<String> {
    [dir.to_path_buf(), dir.join("reports")]
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter(|entry| {
            Path::new(&entry.file_name())
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dmp"))
        })
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.file_name())))
        .filter(|(written, _)| os_records::within(*written, at, REPORT_WINDOW))
        .max_by_key(|(written, _)| *written)
        .map(|(_, name)| name.to_string_lossy().into_owned())
}

#[cfg(test)]
fn captured(fields: &[String]) -> bool {
    CAPTURED.with(|captured| match captured.borrow_mut().as_mut() {
        Some(captured) => {
            captured.exits.push(fields.to_vec());
            true
        }
        None => false,
    })
}

/// What a test captures on its thread. Lib tests cannot turn the process-global profiler on
/// without affecting one another, and their fake pids repeat across tests, so the stops they
/// note stay on their thread too.
#[cfg(test)]
#[derive(Default)]
struct Captured {
    /// The fingerprint fields of the exits reported.
    exits: Vec<Vec<String>>,
    stops: StopRequests,
}

#[cfg(test)]
thread_local! {
    static CAPTURED: std::cell::RefCell<Option<Captured>> = const { std::cell::RefCell::new(None) };
}

/// Captures the exits reported on this thread instead of recording them, and keeps the
/// stops noted on it, until dropped.
#[cfg(test)]
pub(crate) struct CapturedExits;

#[cfg(test)]
impl CapturedExits {
    pub(crate) fn start() -> Self {
        CAPTURED.with(|captured| *captured.borrow_mut() = Some(Captured::default()));
        Self
    }

    /// The fingerprint fields reported so far: kind, label and signal or exit code.
    pub(crate) fn take(&self) -> Vec<Vec<String>> {
        CAPTURED.with(|captured| {
            captured
                .borrow_mut()
                .as_mut()
                .map(|captured| std::mem::take(&mut captured.exits))
                .unwrap_or_default()
        })
    }
}

#[cfg(test)]
impl Drop for CapturedExits {
    fn drop(&mut self) {
        CAPTURED.with(|captured| *captured.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(pid: u32) -> Exited<'static> {
        Exited {
            kind: "command",
            label: "git",
            exe: "/usr/bin/git",
            pid,
            uptime: Duration::from_secs(3),
        }
    }

    #[cfg(unix)]
    fn status(raw: i32) -> ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(raw)
    }

    #[cfg(windows)]
    fn status(raw: u32) -> ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(raw)
    }

    #[cfg(unix)]
    #[test]
    fn classifies_unix_exits() {
        use std::os::unix::process::ExitStatusExt;
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(0), false),
            ExitClass::Clean
        ));
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(9), false),
            ExitClass::Signal {
                signal: 9,
                likely_oom: true
            }
        ));
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(9), true),
            ExitClass::Requested
        ));
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(11), false),
            ExitClass::Signal {
                signal: 11,
                likely_oom: false
            }
        ));
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(1 << 8), false),
            ExitClass::NonZero(1)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn classifies_windows_exits() {
        use std::os::windows::process::ExitStatusExt;
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(0), false),
            ExitClass::Clean
        ));
        assert_eq!(
            classify_exit(&ExitStatus::from_raw(0xC000_0005), false),
            ExitClass::Signal {
                signal: 0xC000_0005_u32 as i32,
                likely_oom: false
            }
        );
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(0xC000_0005), true),
            ExitClass::Requested
        ));
        // TerminateProcess(…, 1), as BiBCode and the process list stop processes.
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(1), false),
            ExitClass::NonZero(1)
        ));
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(0xBFFF_FFFF), false),
            ExitClass::NonZero(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn each_watch_counts_its_own_exits() {
        let signal = |signal| ExitClass::Signal {
            signal,
            likely_oom: signal == libc::SIGKILL,
        };
        for watch in [Watch::Failures, Watch::Signals, Watch::CrashSignals] {
            assert!(!counts(watch, ExitClass::Requested));
            assert!(!counts(watch, ExitClass::Clean));
            assert!(counts(watch, signal(libc::SIGSEGV)));
            assert!(counts(watch, signal(libc::SIGUSR1)));
        }
        assert!(counts(Watch::Failures, ExitClass::NonZero(1)));
        assert!(!counts(Watch::Signals, ExitClass::NonZero(1)));
        assert!(!counts(Watch::CrashSignals, ExitClass::NonZero(130)));
        for sent in [
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGKILL,
            libc::SIGTERM,
        ] {
            assert!(counts(Watch::Signals, signal(sent)));
            assert!(!counts(Watch::CrashSignals, signal(sent)), "{sent}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn ctrl_c_is_not_a_terminal_crash() {
        let code = |code: u32| ExitClass::Signal {
            signal: code as i32,
            likely_oom: false,
        };
        assert!(counts(Watch::CrashSignals, code(0xC000_0005)));
        assert!(counts(Watch::Signals, code(0xC000_013A)));
        assert!(!counts(Watch::CrashSignals, code(0xC000_013A)));
        assert!(faulted(code(0xC000_0005)));
        assert!(!faulted(code(0xC000_013A)));
    }

    #[test]
    fn signals_and_codes_are_named() {
        #[cfg(unix)]
        {
            assert_eq!(signal_name(libc::SIGSEGV), "SIGSEGV");
            assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
            assert_eq!(signal_name(99), "signal 99");
        }
        #[cfg(windows)]
        assert_eq!(signal_name(0xC000_0005_u32 as i32), "0xC0000005");
    }

    #[cfg(unix)]
    #[test]
    fn reported_exits_carry_kind_label_and_signal() {
        let captured = CapturedExits::start();
        report_exit(Watch::Signals, &git(4_000_000_001), &status(11), false);
        report_exit(Watch::Signals, &git(4_000_000_002), &status(11), true);
        report_exit(Watch::Signals, &git(4_000_000_003), &status(1 << 8), false);
        report_exit(Watch::Failures, &git(4_000_000_004), &status(3 << 8), false);
        report_exit(Watch::Signals, &git(4_000_000_005), &status(0), false);
        assert_eq!(
            captured.take(),
            [["command", "git", "SIGSEGV"], ["command", "git", "exit 3"]]
        );
    }

    #[cfg(windows)]
    #[test]
    fn reported_exits_carry_kind_label_and_code() {
        let captured = CapturedExits::start();
        report_exit(
            Watch::Signals,
            &git(4_000_000_001),
            &status(0xC000_0005),
            false,
        );
        report_exit(Watch::Signals, &git(4_000_000_002), &status(1), false);
        assert_eq!(captured.take(), [["command", "git", "0xC0000005"]]);
    }

    #[test]
    fn an_exit_after_a_requested_stop_is_not_reported() {
        let captured = CapturedExits::start();
        #[cfg(unix)]
        let crashed = status(11);
        #[cfg(windows)]
        let crashed = status(0xC000_0005);
        note_stop_requested(4_000_000_100);
        report_exit(Watch::Signals, &git(4_000_000_100), &crashed, false);
        assert!(captured.take().is_empty());
        report_exit(Watch::Signals, &git(4_000_000_101), &crashed, false);
        assert_eq!(captured.take().len(), 1, "another process's stop");
        // A stop that failed leaves the process's own death reportable.
        withdraw_stop_request(4_000_000_100);
        report_exit(Watch::Signals, &git(4_000_000_100), &crashed, false);
        assert_eq!(captured.take().len(), 1);

        // A stop asked before the process started was for an earlier one with its pid.
        note_stop_requested(4_000_000_102);
        std::thread::sleep(Duration::from_millis(20));
        let replacement = Exited {
            uptime: Duration::from_millis(5),
            ..git(4_000_000_102)
        };
        report_exit(Watch::Signals, &replacement, &crashed, false);
        assert_eq!(captured.take().len(), 1, "the pid's new process crashed");
    }

    #[cfg(unix)]
    #[test]
    fn an_interrupt_excuses_exits_but_not_a_later_fault() {
        let captured = CapturedExits::start();
        let codex = Exited {
            kind: "provider",
            label: "codex",
            ..git(4_000_000_200)
        };
        note_interrupt_requested(4_000_000_200);
        report_exit(Watch::Failures, &codex, &status(libc::SIGINT), false);
        report_exit(Watch::Failures, &codex, &status(130 << 8), false);
        assert!(
            captured.take().is_empty(),
            "it ended because it was interrupted"
        );
        report_exit(Watch::Failures, &codex, &status(libc::SIGSEGV), false);
        assert_eq!(captured.take(), [["provider", "codex", "SIGSEGV"]]);
    }

    #[test]
    fn the_newest_recent_minidump_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let reports = dir.path().join("reports");
        std::fs::create_dir(&reports).unwrap();
        let at = SystemTime::now();
        let write = |path: PathBuf, age: Duration| {
            std::fs::write(&path, b"MDMP").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at - age)
                .unwrap();
        };
        write(reports.join("older.dmp"), Duration::from_secs(20));
        write(reports.join("newest.dmp"), Duration::from_secs(2));
        write(dir.path().join("stale.dmp"), Duration::from_secs(3600));
        write(reports.join("notes.txt"), Duration::ZERO);
        assert_eq!(newest_dump(dir.path(), at).as_deref(), Some("newest.dmp"));
        assert_eq!(newest_dump(&dir.path().join("missing"), at), None);
        std::fs::remove_file(reports.join("newest.dmp")).unwrap();
        std::fs::remove_file(reports.join("older.dmp")).unwrap();
        assert_eq!(
            newest_dump(dir.path(), at),
            None,
            "old dumps are not this one"
        );
    }
}
