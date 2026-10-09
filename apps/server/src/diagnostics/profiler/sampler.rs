//! The profiler's process sampler. Every 5 s it refreshes only BiBCode's own processes on a
//! blocking thread, reads each one's private memory, feeds the 1-minute medians to the leak
//! and CPU rules, and emits one `process.sample` event.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use serde_json::json;
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
use crate::diagnostics::{NativeProcessSampler, ProcessAttributionRegistry, RegistrationSource};

const BASE_INTERVAL: Duration = Duration::from_secs(5);
const MAX_INTERVAL: Duration = Duration::from_secs(60);
const SLOW_TICK: Duration = Duration::from_millis(50);
const FAST_TICKS_TO_SPEED_UP: u32 = 10;
const REDISCOVER_EVERY: Duration = Duration::from_secs(60);
/// A build under a terminal can spawn hundreds of processes; the nearest ones are kept.
const MAX_DESCENDANTS: usize = 128;

pub struct SamplerDetector {
    registry: ProcessAttributionRegistry,
}

impl SamplerDetector {
    pub fn new(registry: ProcessAttributionRegistry) -> Self {
        Self { registry }
    }
}

impl Detector for SamplerDetector {
    fn name(&self) -> &'static str {
        "sampler"
    }

    fn start(&self, cancel: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(run(self.registry.clone(), cancel))
    }
}

async fn run(registry: ProcessAttributionRegistry, cancel: CancellationToken) {
    let sampler = Arc::new(Mutex::new(Sampler::new(registry, std::process::id())));
    let mut interval = BASE_INTERVAL;
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
            Ok(elapsed) => interval = next_interval(interval, elapsed, &mut fast_ticks),
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
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(interval) => {}
        }
    }
}

/// Doubles the interval after a slow tick, up to 60 s, and halves it back toward 5 s after
/// ten fast ticks in a row.
fn next_interval(current: Duration, tick: Duration, fast_ticks: &mut u32) -> Duration {
    if tick > SLOW_TICK {
        *fast_ticks = 0;
        return (current * 2).min(MAX_INTERVAL);
    }
    *fast_ticks += 1;
    if *fast_ticks < FAST_TICKS_TO_SPEED_UP {
        return current;
    }
    *fast_ticks = 0;
    (current / 2).max(BASE_INTERVAL)
}

struct Sampler {
    /// Long-lived: CPU% is the difference from the previous refresh.
    system: System,
    registry: ProcessAttributionRegistry,
    self_pid: Pid,
    /// Each descendant's start time at rediscovery, to tell it from a later holder of its pid.
    descendants: HashMap<Pid, u64>,
    last_rediscovery: Option<Instant>,
    /// Keyed by pid and start time, so a reused pid starts a fresh series.
    series: HashMap<(Pid, u64), Series>,
    /// Minutes count from here, on the monotonic clock, never from wall time.
    origin: Instant,
    cpus: f64,
}

impl Sampler {
    fn new(registry: ProcessAttributionRegistry, self_pid: u32) -> Self {
        Self {
            system: System::new(),
            registry,
            self_pid: Pid::from_u32(self_pid),
            descendants: HashMap::new(),
            last_rediscovery: None,
            series: HashMap::new(),
            origin: Instant::now(),
            cpus: std::thread::available_parallelism().map_or(1.0, |n| n.get() as f64),
        }
    }

    /// Takes one sample. Returns how long it took without the rediscovery, which runs on its
    /// own 60 s cadence whatever the interval and so must not drive the throttle.
    fn tick(&mut self, interval: Duration) -> Duration {
        let started = Instant::now();
        let mut rediscovery = Duration::ZERO;
        if self
            .last_rediscovery
            .is_none_or(|at| at.elapsed() >= REDISCOVER_EVERY)
        {
            self.descendants = descendants_of(self.self_pid);
            self.last_rediscovery = Some(started);
            rediscovery = started.elapsed();
        }
        let registered = self.registered_kinds();
        let mut tracked = vec![self.self_pid];
        tracked.extend(self.descendants.keys());
        tracked.extend(registered.keys());
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

        let at_secs = self.origin.elapsed().as_secs();
        let enabled = is_enabled();
        let mut procs = Vec::new();
        let mut live = HashSet::new();
        for pid in tracked {
            let Some(process) = self.system.process(pid) else {
                continue;
            };
            // A descendant that exited since rediscovery may have handed its pid on.
            if !registered.contains_key(&pid)
                && self
                    .descendants
                    .get(&pid)
                    .is_some_and(|started| *started != process.start_time())
            {
                continue;
            }
            let kind = if pid == self.self_pid {
                "server"
            } else {
                registered
                    .get(&pid)
                    .copied()
                    .unwrap_or_else(|| exe_kind(&exe_name(process)))
            };
            let (private_bytes, metric) = match private_bytes(pid.as_u32()) {
                Some(bytes) => (bytes, "private"),
                None => (process.memory(), "rss"),
            };
            let key = (pid, process.start_time());
            live.insert(key);
            let series = self.series.entry(key).or_default();
            // The kind may change: a provider can be registered after rediscovery found it.
            series.kind = kind;
            if series.label.is_empty() {
                series.label = process_label(process);
            }
            let (leak, cpu_sustained) = series.push(
                MemorySample {
                    at_secs,
                    private_bytes,
                },
                f64::from(process.cpu_usage()) / 100.0,
            );
            if !enabled {
                continue;
            }
            if let Some(finding) =
                leak.and_then(|verdict| leak_finding(series, verdict, pid, private_bytes, metric))
            {
                record_finding(finding);
            }
            if cpu_sustained {
                record_finding(cpu_finding(series, pid));
            }
            procs.push(json!({
                "pid": pid.as_u32(),
                "kind": kind,
                "label": series.label,
                "cpu": round1(f64::from(process.cpu_usage()) / self.cpus),
                "rssBytes": process.memory(),
                "privateBytes": private_bytes,
                "metric": metric,
            }));
        }
        self.series.retain(|key, _| live.contains(key));
        let elapsed = started.elapsed();
        if enabled {
            emit(
                "process.sample",
                json!({
                    "procs": procs,
                    // Wall time of the blocking tick: an upper bound on its CPU.
                    "selfCpuMs": round1(elapsed.as_secs_f64() * 1000.0),
                    "intervalMs": u64::try_from(interval.as_millis()).unwrap_or(u64::MAX),
                }),
            );
        }
        elapsed.saturating_sub(rediscovery)
    }

    /// Kinds of the registered processes still held by the process that registered them. A
    /// registration outlives its process until the owner drops it, so the pid may since
    /// name another process.
    fn registered_kinds(&self) -> HashMap<Pid, &'static str> {
        self.registry
            .registered_sources()
            .into_iter()
            .filter(|(identity, _)| {
                NativeProcessSampler::process_identity(identity.pid)
                    .is_ok_and(|current| current == *identity)
            })
            .map(|(identity, source)| {
                let kind = match source {
                    RegistrationSource::Provider => "provider",
                    RegistrationSource::Terminal => "terminal",
                    RegistrationSource::DesktopUi => "webview",
                    RegistrationSource::Helper => "helper",
                };
                (Pid::from_u32(identity.pid), kind)
            })
            .collect()
    }
}

/// One process's samples within the current minute, and the rules its medians feed.
#[derive(Default)]
struct Series {
    kind: &'static str,
    label: String,
    pending: Vec<MemorySample>,
    pending_cpu: Vec<f64>,
    leak: LeakDetector,
    cpu: CpuDetector,
    cpu_sustained: bool,
}

impl Series {
    /// Adds one sample. A sample from a new minute first closes the previous one: its
    /// medians feed the rules, and the result is a leak verdict, if any, and whether CPU
    /// has just become sustained (true once per run, not once per minute).
    fn push(
        &mut self,
        sample: MemorySample,
        cpu_fraction_of_one_core: f64,
    ) -> (Option<LeakVerdict>, bool) {
        let mut closed = (None, false);
        if let Some(first) = self.pending.first()
            && first.at_secs / 60 != sample.at_secs / 60
        {
            let minute = first.at_secs / 60;
            let mut memory: Vec<f64> = self
                .pending
                .iter()
                .map(|sample| sample.private_bytes as f64)
                .collect();
            closed.0 = self.leak.push_minute(minute, median(&mut memory) as u64);
            let sustained = self.cpu.push(median(&mut self.pending_cpu), minute * 60);
            closed.1 = sustained && !self.cpu_sustained;
            self.cpu_sustained = sustained;
            self.pending.clear();
            self.pending_cpu.clear();
        }
        self.pending.push(sample);
        self.pending_cpu.push(cpu_fraction_of_one_core);
        closed
    }

    fn name(&self) -> String {
        format!("{}:{}", self.kind, self.label)
    }
}

/// `None` for `LeakVerdict::None`, which `push_minute` never returns.
fn leak_finding(
    series: &Series,
    verdict: LeakVerdict,
    pid: Pid,
    private_bytes: u64,
    metric: &str,
) -> Option<FindingInput> {
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
    let name = series.name();
    Some(FindingInput {
        kind,
        severity,
        source: "server".into(),
        fingerprint_fields: vec![series.kind.into(), series.label.clone()],
        title: format!("{name} memory grew {slope:.0} MB/h"),
        summary: format!(
            "The private memory of {name} kept growing after warm-up instead of levelling off."
        ),
        evidence: json!({
            "pid": pid.as_u32(),
            "processKind": series.kind,
            "label": series.label,
            "slopeMbPerH": round1(slope),
            "r2": r2.map(round2),
            "privateBytes": private_bytes,
            "metric": metric,
        }),
    })
}

fn cpu_finding(series: &Series, pid: Pid) -> FindingInput {
    let name = series.name();
    FindingInput {
        kind: "cpu.sustained",
        severity: Severity::Warn,
        source: "server".into(),
        fingerprint_fields: vec![series.kind.into(), series.label.clone()],
        title: format!("{name} used over 80% of a core for 5 minutes"),
        summary: format!("{name} stayed above 80% of one CPU core for at least 5 minutes."),
        evidence: json!({
            "pid": pid.as_u32(),
            "processKind": series.kind,
            "label": series.label,
        }),
    }
}

/// BiBCode's descendants and their start times from a full process scan, the nearest kept
/// when there are too many. The scan uses its own `System`, dropped at once: sysinfo keeps a
/// file open for every process it knows, and the long-lived one should know only the
/// tracked few.
fn descendants_of(root: Pid) -> HashMap<Pid, u64> {
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
    let mut found = HashMap::new();
    let mut visited = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        let parent_start = system.process(pid).map_or(0, Process::start_time);
        for child in children.get(&pid).into_iter().flatten() {
            // A child older than its parent belongs to an earlier holder of the parent's pid.
            if child.start_time() < parent_start || !visited.insert(child.pid()) {
                continue;
            }
            if found.len() == MAX_DESCENDANTS {
                return found;
            }
            found.insert(child.pid(), child.start_time());
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
            interval = next_interval(interval, slow, &mut fast_ticks);
            assert_eq!(interval, Duration::from_secs(expected));
        }
        for _ in 0..9 {
            interval = next_interval(interval, fast, &mut fast_ticks);
        }
        assert_eq!(interval, MAX_INTERVAL);
        interval = next_interval(interval, fast, &mut fast_ticks);
        assert_eq!(interval, Duration::from_secs(30));
        for _ in 0..30 {
            interval = next_interval(interval, fast, &mut fast_ticks);
        }
        assert_eq!(interval, BASE_INTERVAL);
    }

    #[test]
    fn minute_medians_feed_the_leak_and_cpu_rules() {
        let mut series = Series::default();
        let mut leaks = Vec::new();
        let mut cpu_alerts = 0;
        // 80 minutes of 5 s samples: 30 MB/h growth with a 400 MB spike once a minute, which
        // the median drops, on a process that keeps a core busy.
        for at_secs in (0..80 * 60).step_by(5) {
            let spike = if at_secs % 60 == 0 { 400 * MB } else { 0 };
            let sample = MemorySample {
                at_secs,
                private_bytes: 500 * MB + at_secs * MB / 120 + spike,
            };
            let (leak, cpu_sustained) = series.push(sample, 0.95);
            leaks.extend(leak);
            cpu_alerts += usize::from(cpu_sustained);
        }
        assert!(matches!(leaks.as_slice(), [LeakVerdict::Suspect { .. }]));
        assert_eq!(
            cpu_alerts, 1,
            "a sustained run raises one finding, not one per minute"
        );
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

    /// The tick refresh over our own process and 20 children. A parallel build can load a
    /// dev machine, so outside CI the budget scales with the load per core.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_tick_over_twenty_children_takes_under_five_ms() {
        struct Children(Vec<std::process::Child>);
        impl Drop for Children {
            fn drop(&mut self) {
                for child in &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let children = Children(
            (0..20)
                .map(|_| {
                    std::process::Command::new("sleep")
                        .arg("30")
                        .spawn()
                        .expect("spawn sleep")
                })
                .collect(),
        );
        let mut sampler = Sampler::new(ProcessAttributionRegistry::new(), std::process::id());
        let started = Instant::now();
        sampler.tick(BASE_INTERVAL);
        let first_tick = started.elapsed();
        let sampled = children
            .0
            .iter()
            .filter(|child| sampler.system.process(Pid::from_u32(child.id())).is_some())
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
