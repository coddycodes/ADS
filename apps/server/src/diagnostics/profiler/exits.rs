//! Unrequested deaths of the processes Coddy Studio runs (shells, provider CLIs, git) and of the
//! app window's web content process.
//!
//! Exit paths check [`watching`] before building anything, so with the profiler off an exit
//! costs one read lock.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{
        LazyLock, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Map, Value, json};

use super::{
    amend_finding, cite, emit,
    findings::{FindingInput, Occurrence, Severity},
    record_finding,
    redact::redact_message,
    sampler::LastSample,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{
    os_records::{self, OsCrashRecord},
    redact::redact_path,
};

/// How long after a death an OS report may still be written and taken as its report.
const REPORT_WINDOW: Duration = Duration::from_secs(60);
/// How long before a death an OS report may have been written: clocks and file times are
/// coarse, and the death is noticed a moment after it happened.
const REPORT_LEAD: Duration = Duration::from_secs(5);
/// How long a stop asked for counts: the process must exit within it.
const STOP_REQUEST_MEMORY: Duration = Duration::from_secs(60);
const STOP_REQUESTS_KEPT: usize = 64;

/// Processes someone is stopping on purpose: Coddy Studio itself, or the user from the process
/// list.
static STOP_REQUESTS: Mutex<StopRequests> = Mutex::new(StopRequests::new());

/// Set once Coddy Studio received a termination signal: what dies from then on dies with it.
static TERMINATING: AtomicBool = AtomicBool::new(false);

/// A delayed lookup (a core dump harvest, a crash report poll or a minidump listing), started
/// with its finding's claim.
type Lookup = Box<dyn FnOnce(LookupClaim) + Send>;

/// The findings with a delayed lookup running, each with the newest one waiting to run after
/// it: a crash loop runs one at a time, and its latest occurrence still gets its record.
type Lookups = HashMap<String, Option<Lookup>>;

static LOOKUPS: LazyLock<Mutex<Lookups>> = LazyLock::new(Default::default);

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
    fn latest(&self, pid: u32, started: Instant) -> Option<Stop> {
        self.0
            .iter()
            .rev()
            .find(|(noted, at, _)| {
                *noted == pid && at.elapsed() < STOP_REQUEST_MEMORY && *at >= started
            })
            .map(|(.., stop)| *stop)
    }
}

/// Runs `apply` on the stop requests a test captures on this thread, or on the process's.
fn with_stop_requests<T>(apply: impl Fn(&mut StopRequests) -> T) -> T {
    #[cfg(test)]
    if let Some(result) = captured(|captured| apply(&mut captured.stops)) {
        return result;
    }
    apply(&mut STOP_REQUESTS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Runs `apply` on the lookups a test captures on this thread, or on the process's.
fn with_lookups<T>(mut apply: impl FnMut(&mut Lookups) -> T) -> T {
    #[cfg(test)]
    if let Some(result) = captured(|captured| apply(&mut captured.lookups)) {
        return result;
    }
    apply(&mut LOOKUPS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// A finding's running delayed lookup. Dropped when the lookup ends, which starts the one
/// waiting.
struct LookupClaim(String);

impl Drop for LookupClaim {
    fn drop(&mut self) {
        let next = with_lookups(|lookups| {
            let next = lookups.get_mut(&self.0).and_then(Option::take);
            if next.is_none() {
                lookups.remove(&self.0);
            }
            next
        });
        if let Some(next) = next {
            next(Self(std::mem::take(&mut self.0)));
        }
    }
}

/// Starts `lookup` for the finding `id`. While one of its lookups runs, `lookup` waits to run
/// next instead, in place of any lookup already waiting: that one's older occurrence can no
/// longer take a record.
fn run_lookup(id: String, lookup: impl FnOnce(LookupClaim) + Send + 'static) {
    let mut waiting = Some(Box::new(lookup) as Lookup);
    let queued = with_lookups(|lookups| match lookups.get_mut(&id) {
        Some(next) => {
            *next = waiting.take();
            true
        }
        None => {
            lookups.insert(id.clone(), None);
            false
        }
    });
    if !queued && let Some(lookup) = waiting {
        lookup(LookupClaim(id));
    }
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

/// Which unrequested exits count as crashes where a process is watched. A deliberate signal
/// (SIGTERM, SIGHUP, SIGINT, Windows' Ctrl-C) never does: Coddy Studio sends none of them itself
/// except an interrupt it notes, so one comes from a logout, a service stop or someone's
/// `kill`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watch {
    /// Signal and non-zero exits: the process should not end on its own.
    Failures,
    /// Signal exits only: non-zero is normal for git.
    Signals,
    /// Signals people do not send from a terminal. In one, Ctrl-\ and `kill -9` end
    /// processes on purpose too, and `exit 1` is ordinary.
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
    /// When it started, or when Coddy Studio began watching it; its uptime ends at the report.
    pub started: Instant,
}

/// The web content process that ended, when the desktop knows it.
pub struct WebProcess<'a> {
    pub pid: u32,
    /// Its executable; on Linux its core dump is matched by the file name.
    pub exe: &'a str,
}

/// How `status` came about. `requested` is true when Coddy Studio or the user asked the process
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
        // An NTSTATUS error (0xC…): the process died of an exception. `exit(-1)` and the
        // other small negative codes at the top of the range are ordinary exits.
        #[cfg(windows)]
        Some(code) if (0xC000_0000..0xFFFF_FF00).contains(&(code as u32)) => ExitClass::Signal {
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
        captured(|_| ()).is_some()
    }
    #[cfg(not(test))]
    {
        super::is_enabled()
    }
}

/// Records that Coddy Studio received a termination signal. Processes that die from then on die
/// with it, so none of their deaths is reported.
pub fn note_terminating() {
    #[cfg(test)]
    if captured(|captured| captured.terminating = true).is_some() {
        return;
    }
    TERMINATING.store(true, Ordering::Relaxed);
}

fn terminating() -> bool {
    #[cfg(test)]
    if let Some(terminating) = captured(|captured| captured.terminating) {
        return terminating;
    }
    TERMINATING.load(Ordering::Relaxed)
}

/// Reports a watched process's exit when `watch` counts it as a crash: a `subprocess.exit`
/// event and a `subprocess.crash` finding. Does nothing while the profiler is off, or once
/// Coddy Studio is terminating.
pub fn report_exit(watch: Watch, process: &Exited<'_>, status: &ExitStatus, requested: bool) {
    if terminating() || !watching() {
        return;
    }
    let class = classify_exit(status, requested);
    if !counts(watch, class) {
        return;
    }
    // Stops are compared with the start itself, not one rebuilt from the uptime with a
    // second clock read: a preemption between the two reads would move it past a stop noted
    // right after the start.
    let started = process.started;
    let uptime_duration = started.elapsed();
    match with_stop_requests(|requests| requests.latest(process.pid, started)) {
        Some(Stop::Ended) => return,
        Some(Stop::Interrupted) if !faulted(class) => return,
        Some(Stop::Interrupted) | None => {}
    }
    let label = redact_message(process.label);
    let (kind, uptime) = (process.kind, human(uptime_duration));
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
                "The {kind} process {label} got SIGKILL after {uptime}. Coddy Studio did not send it; \
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
                    "The {kind} process {label} died with {name} after {uptime}; Coddy Studio did \
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
                "The {kind} process {label} exited with code {code} after {uptime}; Coddy Studio did \
                 not stop it."
            ),
        ),
        ExitClass::Requested | ExitClass::Clean => return,
    };
    let uptime_ms = u64::try_from(uptime_duration.as_millis()).unwrap_or(u64::MAX);
    let mut evidence = Map::from_iter([
        ("pid".to_owned(), process.pid.into()),
        ("outcome".to_owned(), outcome.clone().into()),
        ("likelyOom".to_owned(), likely_oom.into()),
        ("uptimeMs".to_owned(), uptime_ms.into()),
    ]);
    // A sample from before the process started is of an earlier process with its pid.
    if let Some(sample) = last_sample(process.pid)
        && sample.at >= started
    {
        evidence.extend(sample_evidence(&sample));
    }
    let event_id = emit_event(
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
    let occurrence = record(FindingInput {
        kind: "subprocess.crash",
        severity,
        source: "server".into(),
        fingerprint_fields: vec![kind.to_owned(), label, outcome],
        title,
        summary,
        evidence: cite(Value::Object(evidence), event_id),
    });
    #[cfg(target_os = "linux")]
    if let (Some(occurrence), ExitClass::Signal { signal, .. }) = (occurrence, class)
        && dumps_core(signal)
    {
        let started_at = time::OffsetDateTime::now_utc() - uptime_duration;
        attach_coredump(occurrence, process.pid, process.exe, started_at);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = occurrence;
}

/// Records that `pid` is about to be stopped on purpose, by Coddy Studio or by the user from the
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

/// The app window's web content process ended: a `crash.webview` finding. With the process
/// the desktop last saw, its OS record is looked up: on Linux the core dump, on macOS the
/// crash report written during the next minute.
pub fn report_webview_terminated(reason: &str, process: Option<WebProcess<'_>>) {
    let reason = redact_message(reason);
    let occurrence = record_webview(
        &reason,
        "The app window's web content process ended",
        &format!(
            "The process that renders the app window ended ({reason}); the window may stay \
             blank until it reloads."
        ),
        Map::new(),
    );
    let Some(occurrence) = occurrence else {
        return;
    };
    // WebKitGTK kills a process over its memory limit with SIGKILL, which dumps no core.
    #[cfg(target_os = "linux")]
    if let Some(process) = process.filter(|_| reason == "Crashed") {
        let started_at = time::OffsetDateTime::now_utc() - REPORT_WINDOW;
        attach_coredump(occurrence, process.pid, process.exe, started_at);
    }
    #[cfg(target_os = "macos")]
    attach_web_content_report(occurrence, process.map(|process| process.pid));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (occurrence, process);
}

/// WebView2's `ProcessFailed`: a `crash.webview` finding that names the failure's `kind`
/// (`render-process-unresponsive`, `gpu-process-exited`, …), with the exit code and the name
/// of the minidump WebView2 wrote to `report_dir`, if any.
pub fn report_webview_failed(
    kind: &str,
    reason: &str,
    exit_code: i32,
    report_dir: Option<PathBuf>,
) {
    let (kind, reason) = (redact_message(kind), redact_message(reason));
    let (title, what) = webview_failure(&kind);
    let occurrence = record_webview(
        &format!("{kind}: {reason}"),
        title,
        &format!("{what} ({reason})."),
        Map::from_iter([
            ("kind".to_owned(), kind.clone().into()),
            ("exitCode".to_owned(), exit_code.into()),
        ]),
    );
    let (Some(occurrence), Some(dir)) = (occurrence, report_dir) else {
        return;
    };
    let at = SystemTime::now();
    run_lookup(occurrence.id.clone(), move |claim| {
        // Not on the UI thread that reports the failure: the folder may sit on a slow disk.
        let lookup = std::thread::Builder::new()
            .name("coddy-studio-webview-dump".into())
            .spawn(move || {
                let _claim = claim;
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
    });
}

/// The title and the account of a WebView2 failure of `kind`.
fn webview_failure(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "render-process-unresponsive" => (
            "The app window stopped responding",
            "The process that renders the app window stopped responding",
        ),
        "render-process-exited" => (
            "The app window's web content process ended",
            "The process that renders the app window ended; the window may stay blank until \
             it reloads",
        ),
        "browser-process-exited" => (
            "The app window's browser process ended",
            "WebView2's browser process, which hosts the app window's web content, ended",
        ),
        "frame-render-process-exited" => (
            "A frame in the app window lost its renderer",
            "The process that renders a frame inside the app window ended",
        ),
        "gpu-process-exited" => (
            "The app window's GPU process ended",
            "WebView2's GPU process ended; the window redraws once it restarts",
        ),
        "utility-process-exited" => (
            "A utility process of the app window ended",
            "A WebView2 utility process of the app window ended",
        ),
        _ => (
            "A process of the app window ended",
            "A WebView2 process of the app window ended",
        ),
    }
}

/// Records a `crash.webview` finding, unless Coddy Studio is terminating and its webview with it.
fn record_webview(
    reason: &str,
    title: &str,
    summary: &str,
    mut evidence: Map<String, Value>,
) -> Option<Occurrence> {
    if terminating() {
        return None;
    }
    evidence.insert("reason".to_owned(), reason.into());
    record(FindingInput {
        kind: "crash.webview",
        severity: Severity::Error,
        source: "server".into(),
        fingerprint_fields: vec![reason.to_owned()],
        title: title.to_owned(),
        summary: summary.to_owned(),
        evidence: Value::Object(evidence),
    })
}

fn record(input: FindingInput) -> Option<Occurrence> {
    #[cfg(test)]
    if let Some(occurrence) = captured(|captured| {
        let id = format!(
            "{}-{}",
            input.kind,
            super::findings::fingerprint(input.kind, &input.fingerprint_fields)
        );
        captured.findings.push(CapturedFinding {
            fields: input.fingerprint_fields.clone(),
            title: input.title.clone(),
            evidence: input.evidence.clone(),
        });
        Occurrence { id, count: 1 }
    }) {
        return Some(occurrence);
    }
    record_finding(input)
}

fn emit_event(kind: &'static str, payload: Value) -> Option<super::store::EventId> {
    #[cfg(test)]
    if let Some(id) = captured(|captured| {
        captured.events.push((kind, payload.clone()));
        super::store::EventId::new(0, captured.events.len() as u64 - 1)
    }) {
        return Some(id);
    }
    emit(kind, payload)
}

/// The sampler's latest sample of `pid`, if it sampled the process.
fn last_sample(pid: u32) -> Option<LastSample> {
    #[cfg(test)]
    if let Some(sample) = captured(|captured| captured.samples.get(&pid).copied()) {
        return sample;
    }
    super::sampler::last_sample(pid)
}

fn sample_evidence(sample: &LastSample) -> Map<String, Value> {
    let ago_ms = u64::try_from(sample.at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut evidence = Map::from_iter([
        ("lastRssBytes".to_owned(), sample.rss_bytes.into()),
        ("sampledAgoMs".to_owned(), ago_ms.into()),
    ]);
    if let Some(private_bytes) = sample.private_bytes {
        evidence.insert("lastPrivateBytes".to_owned(), private_bytes.into());
    }
    evidence
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
        !deliberate(signal)
    }
}

fn counts(watch: Watch, class: ExitClass) -> bool {
    match (watch, class) {
        (_, ExitClass::Requested | ExitClass::Clean) => false,
        (_, ExitClass::Signal { signal, .. }) if deliberate(signal) => false,
        // 128 + SIGHUP, SIGINT or SIGTERM: the process caught that signal and exited with the
        // shell's code for it, as a provider CLI does after a targeted kill.
        (_, ExitClass::NonZero(129 | 130 | 143)) => false,
        (Watch::Failures, _) => true,
        (Watch::Signals, ExitClass::Signal { .. }) => true,
        (Watch::CrashSignals, ExitClass::Signal { signal, .. }) => !typed_in_a_terminal(signal),
        (Watch::Signals | Watch::CrashSignals, ExitClass::NonZero(_)) => false,
    }
}

/// Signals that end a process on purpose: a polite stop, a hangup or an interrupt; on
/// Windows, Ctrl-C's `STATUS_CONTROL_C_EXIT`.
fn deliberate(signal: i32) -> bool {
    #[cfg(unix)]
    {
        [libc::SIGHUP, libc::SIGINT, libc::SIGTERM].contains(&signal)
    }
    #[cfg(not(unix))]
    {
        signal as u32 == 0xC000_013A
    }
}

/// Signals a terminal's user sends besides the deliberate ones: Ctrl-\ and `kill -9`.
fn typed_in_a_terminal(signal: i32) -> bool {
    #[cfg(unix)]
    {
        [libc::SIGQUIT, libc::SIGKILL].contains(&signal)
    }
    #[cfg(not(unix))]
    {
        let _ = signal;
        false
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

/// Adds the systemd-coredump record of a process that dumped core, matched by its pid, its
/// executable and its records since `started_at`. Best effort, off the caller's path, given
/// up after 5 s; one per finding at a time.
#[cfg(target_os = "linux")]
fn attach_coredump(occurrence: Occurrence, pid: u32, exe: &str, started_at: time::OffsetDateTime) {
    #[cfg(test)]
    if captured(|captured| captured.harvests.push((pid, exe.to_owned()))).is_some() {
        return;
    }
    let exe = exe.to_owned();
    run_lookup(occurrence.id.clone(), move |claim| {
        spawn_lookup(async move {
            let _claim = claim;
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
                unit: None,
                exit: None,
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
    });
}

/// Runs a lookup on a thread and runtime of its own. Callers include the desktop's UI thread,
/// which has no runtime, and a lookup that waited is started by the one before it as that one
/// ends, when the runtime it ran on may be ending too.
#[cfg(target_os = "linux")]
fn spawn_lookup(lookup: impl std::future::Future<Output = ()> + Send + 'static) {
    let thread = std::thread::Builder::new()
        .name("coddy-studio-crash-record".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(lookup),
                Err(error) => tracing::debug!(%error, "crash record lookup has no runtime"),
            }
        });
    if let Err(error) = thread {
        tracing::debug!(%error, "crash record lookup did not start");
    }
}

/// ReportCrash writes a web content process's report some seconds after it died.
#[cfg(target_os = "macos")]
const REPORT_POLL: Duration = Duration::from_secs(5);

/// Looks for the web content process's crash report during the next minute and adds its
/// crashed thread's frames: the report of `pid` when the desktop knew it.
#[cfg(target_os = "macos")]
fn attach_web_content_report(occurrence: Occurrence, pid: Option<u32>) {
    let Some(dir) = dirs::home_dir().map(|home| home.join("Library/Logs/DiagnosticReports")) else {
        return;
    };
    let now = SystemTime::now();
    let earliest = now - REPORT_LEAD;
    let latest = now + REPORT_WINDOW;
    // A lookup that waited behind another still ends a minute after this termination.
    let deadline = Instant::now() + REPORT_WINDOW;
    run_lookup(occurrence.id.clone(), move |claim| {
        let lookup = std::thread::Builder::new()
            .name("coddy-studio-webview-report".into())
            .spawn(move || {
                let _claim = claim;
                loop {
                    if let Some(record) =
                        os_records::macos::web_content_report(&dir, pid, earliest, latest)
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
    });
}

/// The newest minidump in WebView2's failure report folder, or in Crashpad's `reports` folder
/// inside it, written from a few seconds before `at` to a minute after. Older dumps there
/// belong to other failures.
fn newest_dump(dir: &Path, at: SystemTime) -> Option<String> {
    let (earliest, latest) = (at - REPORT_LEAD, at + REPORT_WINDOW);
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
        .filter(|(written, _)| (earliest..=latest).contains(written))
        .max_by_key(|(written, _)| *written)
        .map(|(_, name)| name.to_string_lossy().into_owned())
}

/// Runs `local` on what a test captures on this thread; `None` when it captures nothing.
#[cfg(test)]
fn captured<T>(local: impl FnOnce(&mut Captured) -> T) -> Option<T> {
    CAPTURED.with(|captured| captured.borrow_mut().as_mut().map(local))
}

/// A finding a test captured.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct CapturedFinding {
    pub(crate) fields: Vec<String>,
    pub(crate) title: String,
    pub(crate) evidence: Value,
}

/// What a test captures on its thread. Lib tests cannot turn the process-global profiler on
/// without affecting one another, and their fake pids and fingerprints repeat across tests,
/// so the state reports consult stays on their thread too.
#[cfg(test)]
#[derive(Default)]
struct Captured {
    findings: Vec<CapturedFinding>,
    /// The events emitted, with their kinds; event `n` has the id `00000000-<n>`.
    events: Vec<(&'static str, Value)>,
    /// The pid and executable of each core dump harvest started.
    #[cfg(target_os = "linux")]
    harvests: Vec<(u32, String)>,
    stops: StopRequests,
    lookups: Lookups,
    samples: std::collections::HashMap<u32, LastSample>,
    terminating: bool,
}

#[cfg(test)]
thread_local! {
    static CAPTURED: std::cell::RefCell<Option<Captured>> = const { std::cell::RefCell::new(None) };
}

/// Captures the findings reported on this thread instead of recording them, with the state
/// reports consult, until dropped.
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
        self.take_findings()
            .into_iter()
            .map(|finding| finding.fields)
            .collect()
    }

    pub(crate) fn take_findings(&self) -> Vec<CapturedFinding> {
        captured(|captured| std::mem::take(&mut captured.findings)).unwrap_or_default()
    }

    /// The events emitted so far; event `n` had the id `00000000-<n>`.
    pub(crate) fn take_events(&self) -> Vec<(&'static str, Value)> {
        captured(|captured| std::mem::take(&mut captured.events)).unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn take_harvests(&self) -> Vec<(u32, String)> {
        captured(|captured| std::mem::take(&mut captured.harvests)).unwrap_or_default()
    }

    /// Stands in for the sampler's latest sample of `pid`.
    pub(crate) fn sample(&self, pid: u32, sample: LastSample) {
        captured(|captured| captured.samples.insert(pid, sample));
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
            started: Instant::now() - Duration::from_secs(3),
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

    /// A fault: SIGSEGV, or an access violation.
    fn crashed() -> ExitStatus {
        #[cfg(unix)]
        {
            status(libc::SIGSEGV)
        }
        #[cfg(windows)]
        {
            status(0xC000_0005)
        }
    }

    /// `exit(code)`.
    fn exited(code: u8) -> ExitStatus {
        #[cfg(unix)]
        {
            status(i32::from(code) << 8)
        }
        #[cfg(windows)]
        {
            status(u32::from(code))
        }
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
        // TerminateProcess(…, 1), as Coddy Studio and the process list stop processes.
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(1), false),
            ExitClass::NonZero(1)
        ));
        assert!(matches!(
            classify_exit(&ExitStatus::from_raw(0xBFFF_FFFF), false),
            ExitClass::NonZero(_)
        ));
        // `exit(-1)` and `exit(-2)`.
        assert_eq!(
            classify_exit(&ExitStatus::from_raw(0xFFFF_FFFF), false),
            ExitClass::NonZero(-1)
        );
        assert_eq!(
            classify_exit(&ExitStatus::from_raw(0xFFFF_FFFE), false),
            ExitClass::NonZero(-2)
        );
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
            // A logout, a service stop, a `kill` or an interrupt: never a crash.
            for deliberate in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
                assert!(!counts(watch, signal(deliberate)), "{watch:?} {deliberate}");
            }
        }
        assert!(counts(Watch::Failures, ExitClass::NonZero(1)));
        // 128 + SIGHUP, SIGINT or SIGTERM: a process that caught the signal and exited with
        // the shell's code for it, as node does after a targeted kill.
        for deliberate in [129, 130, 143] {
            assert!(
                !counts(Watch::Failures, ExitClass::NonZero(deliberate)),
                "{deliberate}"
            );
        }
        assert!(counts(Watch::Failures, ExitClass::NonZero(137)));
        assert!(!counts(Watch::Signals, ExitClass::NonZero(1)));
        assert!(!counts(Watch::CrashSignals, ExitClass::NonZero(130)));
        for typed in [libc::SIGQUIT, libc::SIGKILL] {
            assert!(counts(Watch::Failures, signal(typed)));
            assert!(counts(Watch::Signals, signal(typed)));
            assert!(!counts(Watch::CrashSignals, signal(typed)), "{typed}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn ctrl_c_is_never_a_crash() {
        let code = |code: u32| ExitClass::Signal {
            signal: code as i32,
            likely_oom: false,
        };
        for watch in [Watch::Failures, Watch::Signals, Watch::CrashSignals] {
            assert!(counts(watch, code(0xC000_0005)));
            assert!(!counts(watch, code(0xC000_013A)));
        }
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
        report_exit(
            Watch::Failures,
            &git(4_000_000_006),
            &status(libc::SIGTERM),
            false,
        );
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
        note_stop_requested(4_000_000_100);
        report_exit(Watch::Signals, &git(4_000_000_100), &crashed(), false);
        assert!(captured.take().is_empty());
        report_exit(Watch::Signals, &git(4_000_000_101), &crashed(), false);
        assert_eq!(captured.take().len(), 1, "another process's stop");
        // A stop that failed leaves the process's own death reportable.
        withdraw_stop_request(4_000_000_100);
        report_exit(Watch::Signals, &git(4_000_000_100), &crashed(), false);
        assert_eq!(captured.take().len(), 1);

        // A stop asked before the process started was for an earlier one with its pid.
        note_stop_requested(4_000_000_102);
        std::thread::sleep(Duration::from_millis(20));
        let replacement = Exited {
            started: Instant::now() - Duration::from_millis(5),
            ..git(4_000_000_102)
        };
        report_exit(Watch::Signals, &replacement, &crashed(), false);
        assert_eq!(captured.take().len(), 1, "the pid's new process crashed");
    }

    /// A stop noted right after the process started still excuses its exit when the report
    /// runs late, as it does when the reporting thread is preempted.
    #[test]
    fn a_stop_noted_just_after_the_start_excuses_an_exit_reported_late() {
        let captured = CapturedExits::start();
        let started = Instant::now();
        note_stop_requested(4_000_000_103);
        std::thread::sleep(Duration::from_millis(20));
        let stopped = Exited {
            started,
            ..git(4_000_000_103)
        };
        report_exit(Watch::Signals, &stopped, &crashed(), false);
        assert!(captured.take().is_empty(), "Coddy Studio stopped it");
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
        report_exit(Watch::Failures, &codex, &status(130 << 8), false);
        assert!(
            captured.take().is_empty(),
            "it ended because it was interrupted"
        );
        report_exit(Watch::Failures, &codex, &status(libc::SIGSEGV), false);
        assert_eq!(captured.take(), [["provider", "codex", "SIGSEGV"]]);
    }

    /// A node CLI exits 143 or 1 when the SIGTERM that stops Coddy Studio reaches it too.
    #[test]
    fn nothing_is_reported_once_coddy_studio_is_terminating() {
        let captured = CapturedExits::start();
        let claude = Exited {
            kind: "provider",
            label: "claudeAgent",
            ..git(4_000_000_300)
        };
        report_exit(Watch::Failures, &claude, &exited(1), false);
        assert_eq!(captured.take().len(), 1, "before the termination signal");

        note_terminating();
        report_exit(Watch::Failures, &claude, &exited(143), false);
        report_exit(Watch::Failures, &claude, &exited(1), false);
        report_exit(Watch::Signals, &git(4_000_000_301), &crashed(), false);
        report_webview_terminated("Crashed", None);
        assert!(captured.take().is_empty());
    }

    #[test]
    fn the_last_memory_sample_is_evidence() {
        let captured = CapturedExits::start();
        captured.sample(
            4_000_000_310,
            LastSample {
                private_bytes: Some(512 << 20),
                rss_bytes: 600 << 20,
                at: Instant::now(),
            },
        );
        report_exit(Watch::Signals, &git(4_000_000_310), &crashed(), false);
        report_exit(Watch::Signals, &git(4_000_000_311), &crashed(), false);
        let findings = captured.take_findings();
        let sampled = &findings[0].evidence;
        assert_eq!(sampled["lastPrivateBytes"], 512_u64 << 20);
        assert_eq!(sampled["lastRssBytes"], 600_u64 << 20);
        assert!(
            sampled["sampledAgoMs"]
                .as_u64()
                .is_some_and(|ago| ago < 10_000)
        );
        assert_eq!(sampled["pid"], 4_000_000_310_u32);
        assert!(findings[1].evidence.get("lastRssBytes").is_none());

        // The pid's sample is a second old, and this process started 10 ms ago.
        captured.sample(
            4_000_000_312,
            LastSample {
                private_bytes: None,
                rss_bytes: 1,
                at: Instant::now() - Duration::from_secs(1),
            },
        );
        let young = Exited {
            started: Instant::now() - Duration::from_millis(10),
            ..git(4_000_000_312)
        };
        report_exit(Watch::Signals, &young, &crashed(), false);
        let findings = captured.take_findings();
        assert!(findings[0].evidence.get("lastRssBytes").is_none());
    }

    /// The finding cites the `subprocess.exit` event that reports the same death.
    #[test]
    fn a_crash_finding_cites_its_exit_event() {
        let captured = CapturedExits::start();
        report_exit(Watch::Signals, &git(4_000_000_320), &crashed(), false);
        let findings = captured.take_findings();
        let events = captured.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "subprocess.exit");
        assert_eq!(findings[0].evidence["eventIds"], json!(["00000000-0"]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn core_dumps_are_harvested_for_the_crashed_process() {
        let captured = CapturedExits::start();
        let exe = "/usr/lib/webkit2gtk-4.1/WebKitWebProcess";
        report_webview_terminated(
            "Crashed",
            Some(WebProcess {
                pid: 4_000_000_400,
                exe,
            }),
        );
        // Killed for its memory use: a SIGKILL leaves no core dump to harvest.
        report_webview_terminated(
            "ExceededMemoryLimit",
            Some(WebProcess {
                pid: 4_000_000_403,
                exe,
            }),
        );
        report_exit(Watch::Signals, &git(4_000_000_401), &crashed(), false);
        report_exit(
            Watch::Signals,
            &git(4_000_000_402),
            &status(libc::SIGUSR1),
            false,
        );
        assert_eq!(
            captured.take_harvests(),
            [
                (4_000_000_400, exe.to_owned()),
                (4_000_000_401, "/usr/bin/git".to_owned())
            ],
            "only processes that dumped core"
        );
        assert_eq!(
            captured.take(),
            [
                vec!["Crashed"],
                vec!["ExceededMemoryLimit"],
                vec!["command", "git", "SIGSEGV"],
                vec!["command", "git", "SIGUSR1"]
            ]
        );
    }

    #[test]
    fn a_finding_runs_one_delayed_lookup_at_a_time_then_its_newest() {
        let _captured = CapturedExits::start();
        let started = std::sync::Arc::new(Mutex::new(Vec::new()));
        let lookup = |name: &'static str| {
            let started = std::sync::Arc::clone(&started);
            move |claim: LookupClaim| started.lock().unwrap().push((name, claim))
        };
        let names = || {
            started
                .lock()
                .unwrap()
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
        };
        run_lookup("crash-a".into(), lookup("first"));
        run_lookup("crash-a".into(), lookup("second"));
        run_lookup("crash-a".into(), lookup("third"));
        run_lookup("crash-b".into(), lookup("other"));
        assert_eq!(names(), ["first", "other"], "one at a time per finding");

        let first = started.lock().unwrap().remove(0);
        drop(first);
        assert_eq!(
            names(),
            ["other", "third"],
            "the newest waiting one runs next"
        );

        let third = started.lock().unwrap().pop();
        drop(third);
        run_lookup("crash-a".into(), lookup("fourth"));
        assert_eq!(names(), ["other", "fourth"], "none running, so it starts");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_waiting_harvest_runs_once_the_one_before_it_ends() {
        // Not captured: the claims are dropped on the lookups' own threads.
        let id = "subprocess.crash-harvest-hand-off-test".to_owned();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (ran, runs) = std::sync::mpsc::channel();
        run_lookup(id.clone(), move |claim| {
            spawn_lookup(async move {
                let _claim = claim;
                let _ = released.recv();
            });
        });
        let second = ran.clone();
        run_lookup(id, move |claim| {
            spawn_lookup(async move {
                let _claim = claim;
                let _ = second.send("second");
            });
        });
        release.send(()).unwrap();
        assert_eq!(runs.recv_timeout(Duration::from_secs(5)), Ok("second"));
    }

    #[test]
    fn webview2_failures_are_named_after_their_kind() {
        let captured = CapturedExits::start();
        report_webview_failed("render-process-unresponsive", "unresponsive", 0, None);
        report_webview_failed("gpu-process-exited", "crashed", -1_073_741_819, None);
        report_webview_failed("render-process-exited", "crashed", -1_073_741_819, None);
        let findings = captured.take_findings();
        let titles: Vec<&str> = findings
            .iter()
            .map(|finding| finding.title.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "The app window stopped responding",
                "The app window's GPU process ended",
                "The app window's web content process ended"
            ]
        );
        assert_eq!(
            findings[0].fields,
            ["render-process-unresponsive: unresponsive"]
        );
        assert_eq!(findings[1].evidence["kind"], "gpu-process-exited");
        assert_eq!(findings[1].evidence["exitCode"], -1_073_741_819);
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
        write(reports.join("older.dmp"), Duration::from_secs(3));
        write(reports.join("newest.dmp"), Duration::from_secs(1));
        write(reports.join("before.dmp"), Duration::from_secs(20));
        write(dir.path().join("stale.dmp"), Duration::from_secs(3600));
        write(reports.join("notes.txt"), Duration::ZERO);
        assert_eq!(newest_dump(dir.path(), at).as_deref(), Some("newest.dmp"));
        assert_eq!(newest_dump(&dir.path().join("missing"), at), None);
        std::fs::remove_file(reports.join("newest.dmp")).unwrap();
        std::fs::remove_file(reports.join("older.dmp")).unwrap();
        assert_eq!(
            newest_dump(dir.path(), at),
            None,
            "dumps from before the failure are other failures'"
        );
    }
}
