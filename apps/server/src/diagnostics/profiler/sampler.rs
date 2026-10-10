//! The profiler's process sampler. Every 5 s it refreshes only BiBCode's own processes on a
//! blocking thread and reads each one's private memory. Each process's samples become
//! 1-minute medians: they feed the leak and CPU rules, and are written once a minute, with
//! the minute's maxima, as a `process.sample` event.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::{Arc, LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{
    emit,
    findings::{FindingInput, Severity},
    is_enabled,
    leak::{CpuDetector, LeakDetector, LeakVerdict, MemorySample, median},
    record_finding,
    redact::redact_command,
    runtime::Detector,
};
use crate::diagnostics::{
    NativeProcessSampler, ProcessAttributionRegistry, ProcessIdentity, RegistrationSource,
};

const BASE_INTERVAL: Duration = Duration::from_secs(5);
const MINUTE: Duration = Duration::from_secs(60);
const MAX_INTERVAL: Duration = Duration::from_secs(60);
const SLOW_TICK: Duration = Duration::from_millis(50);
const FAST_TICKS_TO_SPEED_UP: u32 = 10;
const REDISCOVER_EVERY: Duration = Duration::from_secs(60);
/// A build under a terminal can spawn hundreds of processes; the nearest ones are kept.
const MAX_DESCENDANTS: usize = 128;

/// A process's latest sample, for a report about its death.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LastSample {
    /// `None` where private memory cannot be read and only RSS is known.
    pub(crate) private_bytes: Option<u64>,
    pub(crate) rss_bytes: u64,
    pub(crate) at: Instant,
}

/// Each tracked process's latest sample, by pid, while the sampler runs.
static LAST_SAMPLES: LazyLock<Mutex<LastSamples>> = LazyLock::new(Default::default);

/// A process that left the tracked set stays for one more tick: its death is reported a
/// moment after the sampler last saw it.
#[derive(Default)]
struct LastSamples {
    current: HashMap<u32, LastSample>,
    departed: HashMap<u32, LastSample>,
}

impl LastSamples {
    fn publish(&mut self, live: HashMap<u32, LastSample>) {
        let mut departed = std::mem::replace(&mut self.current, live);
        departed.retain(|pid, _| !self.current.contains_key(pid));
        self.departed = departed;
    }

    fn get(&self, pid: u32) -> Option<LastSample> {
        self.current
            .get(&pid)
            .or_else(|| self.departed.get(&pid))
            .copied()
    }
}

/// The latest sample of `pid`, while the sampler runs.
pub(crate) fn last_sample(pid: u32) -> Option<LastSample> {
    lock(&LAST_SAMPLES).get(pid)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Forgets the samples when the sampler stops, however it stops.
struct ForgetSamples;

impl Drop for ForgetSamples {
    fn drop(&mut self) {
        *lock(&LAST_SAMPLES) = LastSamples::default();
    }
}

pub struct SamplerDetector {
    registry: ProcessAttributionRegistry,
    interval: Duration,
    minute: Duration,
}

impl SamplerDetector {
    pub fn new(registry: ProcessAttributionRegistry) -> Self {
        Self::with_timing(registry, BASE_INTERVAL, MINUTE)
    }

    /// A shorter sampling interval and detector minute, so a test sees minutes close.
    #[doc(hidden)]
    pub fn with_timing(
        registry: ProcessAttributionRegistry,
        interval: Duration,
        minute: Duration,
    ) -> Self {
        Self {
            registry,
            interval,
            minute,
        }
    }
}

impl Detector for SamplerDetector {
    fn name(&self) -> &'static str {
        "sampler"
    }

    fn start(&self, cancel: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(run(
            self.registry.clone(),
            self.interval,
            self.minute,
            cancel,
        ))
    }
}

async fn run(
    registry: ProcessAttributionRegistry,
    base: Duration,
    minute: Duration,
    cancel: CancellationToken,
) {
    let _forget = ForgetSamples;
    let sampler = Arc::new(Mutex::new(Sampler::new(
        registry,
        std::process::id(),
        minute,
    )));
    let mut interval = base;
    let mut fast_ticks = 0;
    loop {
        let shared = sampler.clone();
        let tick = tokio::task::spawn_blocking(move || {
            shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .tick(interval)
        });
        match tick.await {
            Ok(elapsed) => interval = next_interval(interval, elapsed, base, &mut fast_ticks),
            Err(error) => {
                if error.is_panic() {
                    tracing::warn!(
                        "profiler sampler panicked; sampling stops until the next toggle"
                    );
                }
                return;
            }
        }
        tokio::select! {
            () = cancel.cancelled() => break,
            () = tokio::time::sleep(interval) => {}
        }
    }
    // A toggle-off keeps the profiler enabled until its detectors return, so the open
    // minute is still written; a capture shorter than a minute is not lost.
    let _ = tokio::task::spawn_blocking(move || {
        sampler
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush();
    })
    .await;
}

/// Doubles the interval after a slow tick, up to 60 s, and halves it back toward `base`
/// after ten fast ticks in a row.
fn next_interval(
    current: Duration,
    tick: Duration,
    base: Duration,
    fast_ticks: &mut u32,
) -> Duration {
    if tick > SLOW_TICK {
        *fast_ticks = 0;
        return (current * 2).min(MAX_INTERVAL);
    }
    *fast_ticks += 1;
    if *fast_ticks < FAST_TICKS_TO_SPEED_UP {
        return current;
    }
    *fast_ticks = 0;
    (current / 2).max(base)
}

struct Sampler {
    /// Long-lived: CPU% is the difference from the previous refresh.
    system: System,
    registry: ProcessAttributionRegistry,
    self_pid: Pid,
    /// Each descendant's identity at rediscovery. Identities hold the kernel's raw start
    /// time, which a wall-clock step cannot move; sysinfo's start time adds the boot time
    /// each `System` reads for itself, so two `System`s disagree after a step.
    descendants: HashMap<Pid, ProcessIdentity>,
    last_rediscovery: Option<Instant>,
    /// Keyed by identity, so a reused pid starts a fresh series.
    series: HashMap<ProcessIdentity, Series>,
    /// Minutes count from here, on the monotonic clock, never from wall time.
    origin: Instant,
    /// The detector minute: 60 s outside tests.
    minute_len: Duration,
    /// The open minute, and the sampler's own cost in it: tick wall time and the widest
    /// interval.
    minute: Option<u64>,
    /// Closed minutes waiting for the event that closes the open minute: those of processes
    /// that exited during it, then at the boundary every live process's.
    closed: Vec<ProcessMinute>,
    self_ms: f64,
    widest_interval: Duration,
    cpus: f64,
}

impl Sampler {
    fn new(registry: ProcessAttributionRegistry, self_pid: u32, minute_len: Duration) -> Self {
        Self {
            system: System::new(),
            registry,
            self_pid: Pid::from_u32(self_pid),
            descendants: HashMap::new(),
            last_rediscovery: None,
            series: HashMap::new(),
            origin: Instant::now(),
            minute_len,
            minute: None,
            closed: Vec::new(),
            self_ms: 0.0,
            widest_interval: Duration::ZERO,
            cpus: std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64),
        }
    }

    /// Takes one sample, and writes the `process.sample` event of a minute that closes.
    /// Returns how long it took without the rediscovery, which runs on its own 60 s cadence
    /// whatever the interval and so must not drive the throttle.
    fn tick(&mut self, interval: Duration) -> Duration {
        let started = Instant::now();
        let mut rediscovery = Duration::ZERO;
        if self
            .last_rediscovery
            .is_none_or(|at| at.elapsed() >= REDISCOVER_EVERY)
        {
            self.descendants = descendants_of(self.self_pid)
                .into_iter()
                .filter_map(|(pid, parent)| {
                    let record = NativeProcessSampler::process_record(pid.as_u32()).ok()?;
                    // A pid reused since the scan has another parent, unless BiBCode started
                    // that process too.
                    (record.ppid == parent.as_u32()).then_some((pid, record.identity))
                })
                .collect();
            self.last_rediscovery = Some(started);
            rediscovery = started.elapsed();
        }
        let registered = self.registered_kinds();
        let mut tracked = vec![self.self_pid];
        tracked.extend(self.descendants.keys());
        tracked.extend(
            registered
                .keys()
                .map(|identity| Pid::from_u32(identity.pid)),
        );
        let mut seen = HashSet::new();
        tracked.retain(|pid| seen.insert(*pid));
        // sysinfo forgets a process only when a refresh finds it exited, so pids that left the
        // tracked set are refreshed too. A live one (a daemon reparented away) would stay for
        // good, so past a bound the `System` starts over, losing one sample of CPU history.
        let lingering: Vec<Pid> = self
            .system
            .processes()
            .keys()
            .filter(|pid| !seen.contains(pid))
            .copied()
            .collect();
        let mut refresh = tracked.clone();
        if lingering.len() > MAX_DESCENDANTS {
            self.system = System::new();
        } else {
            refresh.extend(lingering);
        }
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&refresh),
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                // Linux names are cut to 15 bytes; kinds and labels come from the executable.
                .with_exe(UpdateKind::OnlyIfNotSet)
                .without_tasks(),
        );

        let elapsed_since_origin = self.origin.elapsed();
        let minute =
            u64::try_from(elapsed_since_origin.as_nanos() / self.minute_len.as_nanos().max(1))
                .unwrap_or(u64::MAX);
        let enabled = is_enabled();
        let mut live = HashSet::new();
        let mut samples = HashMap::new();
        for pid in tracked {
            let Some(process) = self.system.process(pid) else {
                continue;
            };
            let Ok(identity) = NativeProcessSampler::process_identity(pid.as_u32()) else {
                continue;
            };
            let kind = if pid == self.self_pid {
                "server"
            } else if let Some(kind) = registered.get(&identity) {
                kind
            } else if self.descendants.get(&pid) == Some(&identity) {
                exe_kind(&exe_name(process))
            } else {
                // The pid has passed to another process since it was found.
                continue;
            };
            let (private_bytes, metric) = match private_bytes(pid.as_u32()) {
                Some(bytes) => (bytes, "private"),
                None => (process.memory(), "rss"),
            };
            samples.insert(
                pid.as_u32(),
                LastSample {
                    private_bytes: (metric == "private").then_some(private_bytes),
                    rss_bytes: process.memory(),
                    at: Instant::now(),
                },
            );
            live.insert(identity);
            let series = self.series.entry(identity).or_default();
            // The kind may change: a provider can be registered after rediscovery found it.
            series.kind = kind;
            if series.label.is_empty() {
                series.label = process_label(process);
            }
            let Some(closed) = series.push(
                minute,
                MemorySample {
                    at_secs: elapsed_since_origin.as_secs(),
                    private_bytes,
                },
                process.memory(),
                f64::from(process.cpu_usage()) / 100.0,
                metric,
            ) else {
                continue;
            };
            let closed = ProcessMinute {
                pid: identity.pid,
                kind: series.kind,
                label: series.label.clone(),
                minute: closed,
            };
            if enabled {
                closed.record_findings();
            }
            self.closed.push(closed);
        }
        // A process that exited still reports the part of the minute it was sampled in.
        let departed: Vec<ProcessIdentity> = self
            .series
            .keys()
            .filter(|identity| !live.contains(identity))
            .copied()
            .collect();
        for identity in departed {
            let Some(mut series) = self.series.remove(&identity) else {
                continue;
            };
            let minute = series.close();
            let closed = ProcessMinute {
                pid: identity.pid,
                kind: series.kind,
                label: series.label,
                minute,
            };
            if enabled {
                closed.record_findings();
            }
            self.closed.push(closed);
        }
        lock(&LAST_SAMPLES).publish(samples);
        let elapsed = started.elapsed();
        if self.minute.is_some_and(|open| open != minute) {
            self.write_minute(enabled);
        }
        self.minute = Some(minute);
        self.self_ms += elapsed.as_secs_f64() * 1000.0;
        self.widest_interval = self.widest_interval.max(interval);
        elapsed.saturating_sub(rediscovery)
    }

    /// Writes the open minute as it stands, when recording stops. Its partial medians feed
    /// the rules like any closed minute, but their verdicts are not recorded: the rules'
    /// state ends with the sampler.
    fn flush(&mut self) {
        for (identity, series) in &mut self.series {
            self.closed.push(ProcessMinute {
                pid: identity.pid,
                kind: series.kind,
                label: series.label.clone(),
                minute: series.close(),
            });
        }
        self.series.clear();
        if !self.closed.is_empty() {
            self.write_minute(is_enabled());
        }
    }

    /// Writes the closed minutes as one `process.sample` event, and starts the sampler's own
    /// tally afresh.
    fn write_minute(&mut self, enabled: bool) {
        let closed = std::mem::take(&mut self.closed);
        if enabled {
            emit(
                "process.sample",
                json!({
                    "procs": closed.iter().map(|process| process.to_json(self.cpus)).collect::<Vec<_>>(),
                    // Wall time of the minute's blocking ticks: an upper bound on their CPU.
                    "selfCpuMs": round1(self.self_ms),
                    "intervalMs": u64::try_from(self.widest_interval.as_millis())
                        .unwrap_or(u64::MAX),
                }),
            );
        }
        self.self_ms = 0.0;
        self.widest_interval = Duration::ZERO;
    }

    /// The kind of each registered identity. A registration outlives its process until the
    /// owner drops it, so its pid may since name another process, or another registration.
    fn registered_kinds(&self) -> HashMap<ProcessIdentity, &'static str> {
        self.registry
            .registered_sources()
            .into_iter()
            .map(|(identity, source)| {
                let kind = match source {
                    RegistrationSource::Provider => "provider",
                    RegistrationSource::Terminal => "terminal",
                    RegistrationSource::DesktopUi => "webview",
                    RegistrationSource::Helper => "helper",
                };
                (identity, kind)
            })
            .collect()
    }
}

/// One process's samples within the open minute, and the rules its medians feed.
#[derive(Default)]
struct Series {
    kind: &'static str,
    label: String,
    minute: u64,
    metric: &'static str,
    pending: Vec<MemorySample>,
    pending_rss: Vec<f64>,
    pending_cpu: Vec<f64>,
    leak: LeakDetector,
    cpu: CpuDetector,
    cpu_sustained: bool,
}

/// A closed minute of one process: medians and maxima (CPU as a fraction of one core), and
/// what the rules made of the medians.
struct ClosedMinute {
    metric: &'static str,
    cpu: f64,
    cpu_max: f64,
    rss: u64,
    rss_max: u64,
    private: u64,
    private_max: u64,
    leak: Option<LeakVerdict>,
    /// True once per run above the threshold, not once per minute.
    cpu_sustained: bool,
}

impl Series {
    /// Adds one sample from detector minute `minute`. A sample from a new minute first
    /// closes the open one.
    fn push(
        &mut self,
        minute: u64,
        sample: MemorySample,
        rss_bytes: u64,
        cpu_fraction_of_one_core: f64,
        metric: &'static str,
    ) -> Option<ClosedMinute> {
        let closed = (!self.pending.is_empty() && self.minute != minute).then(|| self.close());
        self.minute = minute;
        self.metric = metric;
        self.pending.push(sample);
        self.pending_rss.push(rss_bytes as f64);
        self.pending_cpu.push(cpu_fraction_of_one_core);
        closed
    }

    fn close(&mut self) -> ClosedMinute {
        let mut private: Vec<f64> = self
            .pending
            .drain(..)
            .map(|sample| sample.private_bytes as f64)
            .collect();
        let (private, private_max) = median_and_max(&mut private);
        let (rss, rss_max) = median_and_max(&mut self.pending_rss);
        let (cpu, cpu_max) = median_and_max(&mut self.pending_cpu);
        let leak = self.leak.push_minute(self.minute, private as u64);
        let sustained = self.cpu.push(cpu, self.minute * 60);
        let cpu_sustained = sustained && !self.cpu_sustained;
        self.cpu_sustained = sustained;
        ClosedMinute {
            metric: self.metric,
            cpu,
            cpu_max,
            rss: rss as u64,
            rss_max: rss_max as u64,
            private: private as u64,
            private_max: private_max as u64,
            leak,
            cpu_sustained,
        }
    }
}

/// One process's closed minute, as the event writes it.
struct ProcessMinute {
    pid: u32,
    kind: &'static str,
    label: String,
    minute: ClosedMinute,
}

impl ProcessMinute {
    fn name(&self) -> String {
        format!("{}:{}", self.kind, self.label)
    }

    fn record_findings(&self) {
        if let Some(finding) = self
            .minute
            .leak
            .and_then(|verdict| leak_finding(self, verdict))
        {
            record_finding(finding);
        }
        if self.minute.cpu_sustained {
            record_finding(cpu_finding(self));
        }
    }

    fn to_json(&self, cpus: f64) -> Value {
        // CPU as a share of every core, like the monitor's figures.
        let cpu = |fraction_of_one_core: f64| round1(fraction_of_one_core * 100.0 / cpus);
        let minute = &self.minute;
        json!({
            "pid": self.pid,
            "kind": self.kind,
            "label": self.label,
            "metric": minute.metric,
            "cpu": cpu(minute.cpu),
            "cpuMax": cpu(minute.cpu_max),
            "rssBytes": minute.rss,
            "rssMaxBytes": minute.rss_max,
            "privateBytes": minute.private,
            "privateMaxBytes": minute.private_max,
        })
    }
}

/// The median and the maximum, emptying `values`.
fn median_and_max(values: &mut Vec<f64>) -> (f64, f64) {
    let middle = median(values);
    // `median` sorts ascending.
    let max = values.last().copied().unwrap_or(0.0);
    values.clear();
    (middle, max)
}

/// `None` for `LeakVerdict::None`, which `push_minute` never returns.
fn leak_finding(process: &ProcessMinute, verdict: LeakVerdict) -> Option<FindingInput> {
    let (kind, severity, slope, r2) = match verdict {
        LeakVerdict::Likely { slope_mb_per_h, r2 } => (
            "memory.leak-likely",
            Severity::Error,
            slope_mb_per_h,
            Some(r2),
        ),
        LeakVerdict::Suspect { slope_mb_per_h } => {
            ("memory.leak-suspect", Severity::Warn, slope_mb_per_h, None)
        }
        LeakVerdict::None => return None,
    };
    let name = process.name();
    Some(FindingInput {
        kind,
        severity,
        source: "server".into(),
        fingerprint_fields: vec![process.kind.into(), process.label.clone()],
        title: format!("{name} memory grew {slope:.0} MB/h"),
        summary: format!(
            "The private memory of {name} kept growing after warm-up instead of levelling off."
        ),
        evidence: json!({
            "pid": process.pid,
            "processKind": process.kind,
            "label": process.label,
            "slopeMbPerH": round1(slope),
            "r2": r2.map(round2),
            "privateBytes": process.minute.private,
            "metric": process.minute.metric,
        }),
    })
}

fn cpu_finding(process: &ProcessMinute) -> FindingInput {
    let name = process.name();
    FindingInput {
        kind: "cpu.sustained",
        severity: Severity::Warn,
        source: "server".into(),
        fingerprint_fields: vec![process.kind.into(), process.label.clone()],
        title: format!("{name} used over 80% of a core for 5 minutes"),
        summary: format!("{name} stayed above 80% of one CPU core for at least 5 minutes."),
        evidence: json!({
            "pid": process.pid,
            "processKind": process.kind,
            "label": process.label,
        }),
    }
}

/// BiBCode's descendants, each with the parent it was found under, from a full process
/// scan, the nearest kept when there are too many. The scan uses its own `System`, dropped at once: sysinfo keeps a
/// file open for every process it knows, and the long-lived one should know only the
/// tracked few.
fn descendants_of(root: Pid) -> Vec<(Pid, Pid)> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    let mut children = HashMap::<Pid, Vec<&Process>>::new();
    for process in system.processes().values() {
        if process.thread_kind().is_none()
            && let Some(parent) = process.parent()
        {
            children.entry(parent).or_default().push(process);
        }
    }
    let mut found = Vec::new();
    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        let parent_start = system.process(pid).map_or(0, Process::start_time);
        for child in children.get(&pid).into_iter().flatten() {
            // A child older than its parent belongs to an earlier holder of the parent's pid.
            // Both start times come from this one `System`, so they share its boot time.
            if child.start_time() < parent_start || !visited.insert(child.pid()) {
                continue;
            }
            if found.len() == MAX_DESCENDANTS {
                return found;
            }
            found.push((child.pid(), pid));
            queue.push_back(child.pid());
        }
    }
    found
}

fn exe_name(process: &Process) -> String {
    process
        .exe()
        .and_then(Path::file_name)
        .unwrap_or(process.name())
        .to_string_lossy()
        .into_owned()
}

/// The executable name only. Quoted, so a name with spaces survives `redact_command`.
fn process_label(process: &Process) -> String {
    redact_command(&format!("\"{}\"", exe_name(process)))
}

fn exe_kind(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    match name.strip_suffix(".exe").unwrap_or(&name) {
        "webkitwebprocess" | "com.apple.webkit.webcontent" | "msedgewebview2" => "webview",
        "git" => "git",
        _ => "other",
    }
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Private memory in bytes: what leak detection tracks, as RSS rises and falls with shared
/// and evictable pages. `None` falls back to RSS.
#[cfg(target_os = "linux")]
fn private_bytes(pid: u32) -> Option<u64> {
    parse_private_bytes(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// `RssAnon` plus `VmSwap` from `/proc/<pid>/status`. Kernel threads have no `RssAnon`.
#[cfg(target_os = "linux")]
fn parse_private_bytes(status: &str) -> Option<u64> {
    let kib = |field: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(field)?
                .trim()
                .strip_suffix("kB")?
                .trim()
                .parse::<u64>()
                .ok()
        })
    };
    let anon = kib("RssAnon:")?;
    Some((anon + kib("VmSwap:").unwrap_or(0)) * 1024)
}

#[cfg(target_os = "macos")]
fn private_bytes(pid: u32) -> Option<u64> {
    let pid = i32::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
    // SAFETY: `info` is writable storage of the size the `RUSAGE_INFO_V2` flavor fills.
    let result =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
    // SAFETY: a zero result means the call filled `info`.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(windows)]
fn private_bytes(pid: u32) -> Option<u64> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
        },
    };

    let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).ok()?;
    // SAFETY: a query-only handle, checked for null and closed exactly once below.
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size,
        ..Default::default()
    };
    // SAFETY: `counters` is writable storage of `size` bytes and `handle` is live.
    let read =
        unsafe { GetProcessMemoryInfo(handle, std::ptr::from_mut(&mut counters).cast(), size) };
    // SAFETY: `handle` was opened above and is closed once.
    unsafe { CloseHandle(handle) };
    (read != 0).then_some(counters.PrivateUsage as u64)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn private_bytes(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    const MB: u64 = 1024 * 1024;

    #[test]
    fn slow_ticks_back_off_and_fast_ticks_recover() {
        let (slow, fast) = (Duration::from_millis(80), Duration::from_millis(1));
        let mut fast_ticks = 0;
        let mut interval = BASE_INTERVAL;
        for expected in [10, 20, 40, 60, 60] {
            interval = next_interval(interval, slow, BASE_INTERVAL, &mut fast_ticks);
            assert_eq!(interval, Duration::from_secs(expected));
        }
        for _ in 0..9 {
            interval = next_interval(interval, fast, BASE_INTERVAL, &mut fast_ticks);
        }
        assert_eq!(interval, MAX_INTERVAL);
        interval = next_interval(interval, fast, BASE_INTERVAL, &mut fast_ticks);
        assert_eq!(interval, Duration::from_secs(30));
        for _ in 0..30 {
            interval = next_interval(interval, fast, BASE_INTERVAL, &mut fast_ticks);
        }
        assert_eq!(interval, BASE_INTERVAL);
    }

    #[test]
    fn minute_medians_feed_the_rules_and_maxima_keep_the_spikes() {
        let mut series = Series::default();
        let mut minutes = Vec::new();
        // 80 minutes of 5 s samples: 30 MB/h growth with a 400 MB spike once a minute, on a
        // process that keeps a core busy.
        for at_secs in (0..80 * 60).step_by(5) {
            let spike = if at_secs % 60 == 0 { 400 * MB } else { 0 };
            let sample = MemorySample {
                at_secs,
                private_bytes: 500 * MB + at_secs * MB / 120 + spike,
            };
            minutes.extend(series.push(
                at_secs / 60,
                sample,
                sample.private_bytes,
                0.95,
                "private",
            ));
        }
        let leaks: Vec<_> = minutes.iter().filter_map(|minute| minute.leak).collect();
        assert!(matches!(leaks.as_slice(), [LeakVerdict::Suspect { .. }]));
        assert_eq!(
            minutes.iter().filter(|minute| minute.cpu_sustained).count(),
            1,
            "a sustained run raises one finding, not one per minute"
        );
        // The median drops the spike; the written maximum keeps it.
        assert!(minutes[0].private < 501 * MB);
        assert_eq!(minutes[0].private_max, 900 * MB);
        assert_eq!(minutes[0].rss_max, 900 * MB);
        assert_eq!(minutes[0].metric, "private");
    }

    #[test]
    fn executables_map_to_process_kinds() {
        assert_eq!(exe_kind("WebKitWebProcess"), "webview");
        assert_eq!(exe_kind("msedgewebview2.exe"), "webview");
        assert_eq!(exe_kind("com.apple.WebKit.WebContent"), "webview");
        assert_eq!(exe_kind("git.exe"), "git");
        assert_eq!(exe_kind("node"), "other");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn private_memory_is_anonymous_resident_plus_swap() {
        let status = "Name:\tx\nVmRSS:\t  900 kB\nRssAnon:\t  300 kB\nRssFile:\t  600 kB\nVmSwap:\t   20 kB\n";
        assert_eq!(parse_private_bytes(status), Some(320 * 1024));
        assert_eq!(parse_private_bytes("Name:\tkthreadd\n"), None);
        assert!(private_bytes(std::process::id()).is_some_and(|bytes| bytes > 0));
    }

    /// `sleep 30` children, killed and reaped on drop.
    #[cfg(target_os = "linux")]
    struct Sleepers(Vec<std::process::Child>);

    #[cfg(target_os = "linux")]
    impl Sleepers {
        fn spawn(count: usize) -> Self {
            Self(
                (0..count)
                    .map(|_| {
                        std::process::Command::new("sleep")
                            .arg("30")
                            .spawn()
                            .expect("spawn sleep")
                    })
                    .collect(),
            )
        }

        /// Kills and reaps one child, so its pid leaves `/proc`.
        fn end(&mut self, index: usize) {
            let _ = self.0[index].kill();
            let _ = self.0[index].wait();
        }

        fn identities(&self) -> Vec<ProcessIdentity> {
            self.0
                .iter()
                .map(|child| NativeProcessSampler::process_identity(child.id()).unwrap())
                .collect()
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for Sleepers {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[test]
    fn a_departed_process_keeps_its_last_sample_for_one_more_tick() {
        let sample = |rss_bytes| LastSample {
            private_bytes: Some(rss_bytes / 2),
            rss_bytes,
            at: Instant::now(),
        };
        let mut samples = LastSamples::default();
        samples.publish(HashMap::from([(1, sample(100)), (2, sample(200))]));
        samples.publish(HashMap::from([(1, sample(110))]));
        assert_eq!(samples.get(1).map(|last| last.rss_bytes), Some(110));
        assert_eq!(samples.get(2).map(|last| last.rss_bytes), Some(200));
        samples.publish(HashMap::from([(1, sample(120))]));
        assert_eq!(samples.get(2), None, "gone a tick after it departed");
    }

    #[test]
    fn a_tick_publishes_each_sampled_process() {
        let mut sampler = Sampler::new(
            ProcessAttributionRegistry::new(),
            std::process::id(),
            MINUTE,
        );
        sampler.tick(BASE_INTERVAL);
        // Other tests' samplers publish too, and every one samples this process.
        let own = last_sample(std::process::id()).expect("this process was sampled");
        assert!(own.rss_bytes > 0);
    }

    /// Pid reuse is judged by raw identities on both sides. A Linux identity's start time
    /// counts clock ticks since boot, so a wall-clock step cannot change it. sysinfo's start
    /// time adds the boot time each `System` reads when it is created, so after a step the
    /// rediscovery `System` and the long-lived one disagree about every process.
    #[cfg(target_os = "linux")]
    #[test]
    fn descendants_are_matched_by_raw_identity() {
        let children = Sleepers::spawn(2);
        let mut sampler = Sampler::new(
            ProcessAttributionRegistry::new(),
            std::process::id(),
            MINUTE,
        );
        sampler.tick(BASE_INTERVAL);
        let identities = children.identities();
        for identity in &identities {
            assert_eq!(
                sampler.descendants.get(&Pid::from_u32(identity.pid)),
                Some(identity)
            );
            assert!(sampler.series.contains_key(identity));
        }

        // The first child's pid now names another process.
        sampler
            .descendants
            .get_mut(&Pid::from_u32(identities[0].pid))
            .unwrap()
            .started_at += 1;
        sampler.tick(BASE_INTERVAL);
        assert!(!sampler.series.contains_key(&identities[0]));
        assert!(sampler.series.contains_key(&identities[1]));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_exited_process_keeps_its_partial_minute() {
        let mut children = Sleepers::spawn(2);
        let identities = children.identities();
        let mut sampler = Sampler::new(
            ProcessAttributionRegistry::new(),
            std::process::id(),
            MINUTE,
        );
        sampler.tick(BASE_INTERVAL);
        assert!(sampler.closed.is_empty(), "no minute has closed yet");

        children.end(0);
        sampler.tick(BASE_INTERVAL);
        assert!(!sampler.series.contains_key(&identities[0]));
        // Other tests' children are descendants too and may exit meanwhile.
        let exited = sampler
            .closed
            .iter()
            .find(|process| process.pid == identities[0].pid)
            .expect("the exited child's partial minute is kept");
        assert!(exited.minute.private_max > 0);
        assert!(
            sampler
                .closed
                .iter()
                .all(|process| process.pid != identities[1].pid),
            "a live process's minute stays open"
        );
    }

    /// The tick refresh over our own process and 20 children. A parallel build can load a
    /// dev machine, so outside CI the budget scales with the load per core.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_tick_over_twenty_children_takes_under_five_ms() {
        let children = Sleepers::spawn(20);
        let mut sampler = Sampler::new(
            ProcessAttributionRegistry::new(),
            std::process::id(),
            MINUTE,
        );
        let started = Instant::now();
        sampler.tick(BASE_INTERVAL);
        let first_tick = started.elapsed();
        let sampled = children
            .identities()
            .iter()
            .filter(|identity| sampler.series.contains_key(identity))
            .count();
        assert_eq!(sampled, 20, "every child is found and sampled");

        let mut ticks: Vec<f64> = (0..10)
            .map(|_| sampler.tick(BASE_INTERVAL).as_secs_f64() * 1000.0)
            .collect();
        let median_ms = median(&mut ticks);
        let load = if std::env::var_os("CI").is_some() {
            1.0
        } else {
            let load1: f64 = std::fs::read_to_string("/proc/loadavg")
                .ok()
                .and_then(|text| text.split_whitespace().next()?.parse().ok())
                .unwrap_or(0.0);
            (load1 / sampler.cpus).max(1.0)
        };
        eprintln!(
            "sampler overhead: median tick {median_ms:.3} ms over {} processes, first tick with rediscovery {:.3} ms, load factor {load:.2}",
            sampler.series.len(),
            first_tick.as_secs_f64() * 1000.0
        );
        assert!(
            median_ms < 5.0 * load,
            "median tick {median_ms:.3} ms, budget {:.3} ms",
            5.0 * load
        );
    }
}
