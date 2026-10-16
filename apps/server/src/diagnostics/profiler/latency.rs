//! Per-method RPC latency (Decision 7): rolling windows, the slow-request rule, the in-flight
//! hang check and the join log that client reports are matched against.
//!
//! The request path only times and records. Findings are raised by `LatencyDetector` on its
//! 5 s tick, which is what lets five slow methods in one tick collapse into a single
//! `rpc.saturation` finding; an `rpc.slow` finding therefore lags its request by up to 5 s.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        LazyLock, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::{
    cite, emit,
    findings::{FindingInput, Severity},
    record_finding,
    runtime::Detector,
    store::EventId,
};

pub const BUCKET_GROWTH: f64 = 1.12;
pub const BUCKETS: usize = 100;
/// Carries a per-request correlation ID set by the client. A trace ID cannot be the join key:
/// one trace spans several RPCs (for example repeated `uploads.append` calls).
pub const CORRELATION_HEADER: &str = "x-bibcode-profiler-rid";
/// The outcome a stream records, timed to its first item. Event streams may wait any time for
/// their first item, so streams are log-only: no `rpc.slow` finding and no hang check.
pub const FIRST_ITEM: &str = "first-item";
const MAX_CORRELATION_ID_BYTES: usize = 64;
const SLOT_SECS: u64 = 300;
const SLOTS: u64 = 6;
const BASELINE_MIN_SAMPLES: u64 = 100;
const WARN_MS: f64 = 1_000.0;
const SEVERE_MS: f64 = 10_000.0;
const JOIN_LOG_CAPACITY: usize = 4096;
const TICK: Duration = Duration::from_secs(5);
const HANG_AFTER: Duration = Duration::from_secs(10);
const LONG_RUNNING_HANG_AFTER: Duration = Duration::from_secs(300);
const SATURATION_METHODS: usize = 5;
/// On Windows, where `Instant` counts suspended time, a detector tick this late means the
/// machine slept, and a request spanning it is not measured. Elsewhere `Instant` stops during
/// suspend, so a late tick is a runtime stall, which is exactly what the hang check reports.
const SUSPEND_GAP_MS: u64 = 30_000;

/// Log-scale bucket `i` holds `[1.12^i, 1.12^(i+1))` ms; bucket 0 also holds everything up to
/// 1 ms, and the last one everything above about 75 s.
pub fn bucket_of(ms: f64) -> usize {
    if ms <= 1.0 {
        return 0;
    }
    ((ms.ln() / BUCKET_GROWTH.ln()) as usize).min(BUCKETS - 1)
}

fn bucket_upper(i: usize) -> f64 {
    BUCKET_GROWTH.powi(i as i32 + 1)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    #[default]
    None,
    /// Over `max(3 × p95, floor)`: an `rpc.timing` event only, never a finding, in this PR.
    RelativeLogOnly,
    Warn,
    Severe,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RelativeLogOnly => "relative",
            Self::Warn => "warn",
            Self::Severe => "severe",
        }
    }
}

pub fn judge(ms: f64, p95: Option<f64>, samples: u64, interactive: bool) -> Tier {
    if ms >= SEVERE_MS {
        return Tier::Severe;
    }
    if ms >= WARN_MS {
        return Tier::Warn;
    }
    let floor = if interactive { 250.0 } else { 1_000.0 };
    match p95 {
        Some(p95) if samples >= BASELINE_MIN_SAMPLES && ms > (3.0 * p95).max(floor) => {
            Tier::RelativeLogOnly
        }
        _ => Tier::None,
    }
}

/// External work (network, provider CLIs, subprocesses, LLM calls, setup hooks) whose
/// duration says little about the server. These methods still emit `rpc.timing`, but never
/// raise `rpc.slow` or count toward `rpc.saturation`, and their hang budget is 300 s.
pub fn is_long_running(method: &str) -> bool {
    matches!(
        method,
        "vcs.clone"
            // A preview tunnel lives as long as its page; a write waits on the dev server.
            | "preview.tunnelOpen"
            | "preview.tunnelWrite"
            | "sourceControl.cloneRepository"
            | "sourceControl.publishRepository"
            | "sourceControl.lookupRepository"
            | "server.discoverSourceControl"
            | "vcs.pull"
            | "vcs.generateCommitMessage"
            | "git.resolvePullRequest"
            | "git.preparePullRequestThread"
            | "gitManager.commit"
            | "gitManager.getRemoteTags"
            | "gitManager.listPullRequests"
            | "pullRequests.checkout"
            | "pullRequests.runAction"
            | "pullRequests.list"
            | "pullRequests.get"
            | "pullRequests.getTimeline"
            | "pullRequests.getCommits"
            | "pullRequests.getChecks"
            | "pullRequests.getFiles"
            | "pullRequests.getContext"
            | "pullRequests.getVocabulary"
            | "pullRequests.getCreateDefaults"
            | "updater.check"
            | "updater.install"
            | "server.updateProvider"
            | "server.refreshProviders"
            | "server.refreshProviderUsage"
            // Served from a cache, but a cold one waits on the provider's probe (#107).
            | "server.getProviderCapabilities"
            // Saving settings re-probes the providers, which runs OpenCode's `--version` (#115).
            | "server.updateSettings"
            | "worktree.createManaged"
            // `git worktree remove --force` deletes the whole tree, `node_modules` and `target`
            // included.
            | "worktree.remove"
            | "agentSessions.import"
    )
}

/// Background methods get the 1 s relative floor instead of 250 ms. Independent of
/// `is_long_running`, which governs findings.
pub fn is_background(method: &str) -> bool {
    [
        "git.",
        "vcs.",
        "worktree.",
        "pullRequests.",
        "server.refresh",
    ]
    .iter()
    .any(|prefix| method.starts_with(prefix))
        || method == "projects.searchEntries"
}

#[derive(Clone)]
struct Slot {
    key: u64,
    counts: [u32; BUCKETS],
}

/// A ring of six 5-minute slots keyed by `now_secs / 300`: the current slot and the five
/// before it, which form the baseline.
#[derive(Clone)]
pub struct LatencyWindow {
    slots: [Slot; SLOTS as usize],
    /// `(slot key, samples, p95)`. Writes only reach the current slot, so the baseline that
    /// excludes it changes only when the key rolls; the hot path reads this instead of
    /// summing 500 counters.
    cached: Option<(u64, u64, Option<f64>)>,
}

impl Default for LatencyWindow {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| Slot {
                key: 0,
                counts: [0; BUCKETS],
            }),
            cached: None,
        }
    }
}

impl LatencyWindow {
    pub fn record(&mut self, ms: f64, now_secs: u64) {
        let key = now_secs / SLOT_SECS;
        let slot = &mut self.slots[(key % SLOTS) as usize];
        if slot.key != key {
            // A slot holding an older key has aged out of the window.
            *slot = Slot {
                key,
                counts: [0; BUCKETS],
            };
        }
        let count = &mut slot.counts[bucket_of(ms)];
        *count = count.saturating_add(1);
    }

    pub fn p95_excluding_current(&self, now_secs: u64) -> Option<f64> {
        self.compute_baseline(now_secs / SLOT_SECS).1
    }

    /// The samples behind `p95_excluding_current`, which the current slot is not part of.
    pub fn samples(&self, now_secs: u64) -> u64 {
        self.compute_baseline(now_secs / SLOT_SECS).0
    }

    fn baseline(&mut self, now_secs: u64) -> (u64, Option<f64>) {
        let key = now_secs / SLOT_SECS;
        match self.cached {
            Some((cached_key, samples, p95)) if cached_key == key => (samples, p95),
            _ => {
                let (samples, p95) = self.compute_baseline(key);
                self.cached = Some((key, samples, p95));
                (samples, p95)
            }
        }
    }

    /// Sums the five slots before `current` and walks them to the 95th percentile.
    fn compute_baseline(&self, current: u64) -> (u64, Option<f64>) {
        let mut totals = [0u64; BUCKETS];
        for slot in &self.slots {
            if slot.key < current && current - slot.key < SLOTS {
                for (total, count) in totals.iter_mut().zip(slot.counts) {
                    *total += u64::from(count);
                }
            }
        }
        let samples: u64 = totals.iter().sum();
        if samples == 0 {
            return (0, None);
        }
        let rank = samples.saturating_mul(95).div_ceil(100);
        let mut seen = 0;
        for (bucket, total) in totals.iter().enumerate() {
            seen += total;
            if seen >= rank {
                return (samples, Some(bucket_upper(bucket)));
            }
        }
        (samples, Some(bucket_upper(BUCKETS - 1)))
    }

    /// The counts of slot `key`, unless it has been overwritten or holds nothing.
    fn slot(&self, key: u64) -> Option<&[u32; BUCKETS]> {
        let slot = &self.slots[(key % SLOTS) as usize];
        (slot.key == key && slot.counts.iter().any(|&count| count > 0)).then_some(&slot.counts)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinEntry {
    pub correlation_id: String,
    pub method: String,
    pub server_ms: f64,
    /// 0 until the session loop records a frame-receipt instant.
    pub queued_ms: f64,
}

/// The last 4096 correlated requests, oldest first.
#[derive(Default)]
pub struct JoinLog {
    pub(crate) entries: VecDeque<JoinEntry>,
}

impl JoinLog {
    pub fn push(&mut self, correlation_id: String, method: String, server_ms: f64, queued_ms: f64) {
        if self.entries.len() == JOIN_LOG_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(JoinEntry {
            correlation_id,
            method,
            server_ms,
            queued_ms,
        });
    }

    /// The newest entry for `correlation_id`.
    pub fn find(&self, correlation_id: &str) -> Option<JoinEntry> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.correlation_id == correlation_id)
            .cloned()
    }
}

/// The worst absolute-tier result of one `(method, outcome)` since the last tick.
#[derive(Clone, Copy, Debug, Default)]
struct Slow {
    count: u64,
    max_ms: f64,
    tier: Tier,
    /// The `rpc.timing` event of the slowest call, which the finding cites.
    slowest_event: Option<EventId>,
}

impl Slow {
    /// `event_id` is the call's `rpc.timing` event, `None` when the full queue dropped it. A
    /// dropped slowest call stays uncited: no faster call stands in for it.
    fn add(&mut self, ms: f64, tier: Tier, event_id: Option<EventId>) {
        if self.count == 0 || ms > self.max_ms {
            self.slowest_event = event_id;
        }
        self.count += 1;
        self.max_ms = self.max_ms.max(ms);
        self.tier = self.tier.max(tier);
    }
}

type MethodKey = (String, &'static str);

/// Decision 3: one mutex per map, not sharded. Each critical section is a hash lookup and a
/// counter increment plus a cached baseline read (recomputed once per 5-minute slot), well
/// under a microsecond; keys are allocated before locking. Only the 5 s tick walks a map.
#[derive(Default)]
pub struct LatencyBook {
    pub(crate) windows: Mutex<HashMap<MethodKey, LatencyWindow>>,
    /// Absolute-tier results parked for the detector's next tick.
    slow: Mutex<HashMap<MethodKey, Slow>>,
    pub(crate) joins: Mutex<JoinLog>,
    /// Outbound reply bytes by method and 5-minute slot, drained into `rpc.bytes`.
    bytes: Mutex<HashMap<(String, u64), ByteCounts>>,
}

#[derive(Default)]
struct ByteCounts {
    messages: u64,
    bytes: u64,
    max_bytes: u64,
}

/// Counts one reply of `method` that left with `bytes` encoded bytes (#89: which methods
/// fill a slow remote link).
pub fn record_outbound_bytes(method: &str, bytes: usize) {
    if !profiling() {
        return;
    }
    let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
    let key = (method.to_owned(), now_secs() / SLOT_SECS);
    let mut counts = lock(&global_latency().bytes);
    let counts = counts.entry(key).or_default();
    counts.messages += 1;
    counts.bytes = counts.bytes.saturating_add(bytes);
    counts.max_bytes = counts.max_bytes.max(bytes);
}

impl LatencyBook {
    /// Records one timed request. `queued_ms` reaches the join log only: the event reports
    /// `queuedMs: null` because no frame-receipt instant exists in the session loop yet.
    pub fn observe(
        &self,
        method: &str,
        outcome: &'static str,
        ms: f64,
        queued_ms: f64,
        correlation_id: Option<&str>,
    ) {
        if let Some(id) = correlation_id {
            let (id, joined_method) = (id.to_owned(), method.to_owned());
            lock(&self.joins).push(id, joined_method, ms, queued_ms);
        }
        let key = (method.to_owned(), outcome);
        let now = now_secs();
        let (samples, p95) = {
            let mut windows = lock(&self.windows);
            let window = windows.entry(key).or_default();
            window.record(ms, now);
            window.baseline(now)
        };
        let tier = judge(ms, p95, samples, !is_background(method));
        if tier == Tier::None {
            return;
        }
        let event_id = emit(
            "rpc.timing",
            json!({
                "method": method,
                "outcome": outcome,
                "ms": ms,
                "queuedMs": null,
                "tier": tier.name(),
                "p95Ms": p95,
                "samples": samples,
                "correlationId": correlation_id,
            }),
        );
        if tier >= Tier::Warn && outcome != FIRST_ITEM && !is_long_running(method) {
            let key = (method.to_owned(), outcome);
            let mut slow = lock(&self.slow);
            slow.entry(key).or_default().add(ms, tier, event_id);
        }
    }

    pub fn find_join(&self, correlation_id: &str) -> Option<JoinEntry> {
        lock(&self.joins).find(correlation_id)
    }

    /// One `rpc.histogram` payload per `(method, outcome)` with samples in slot `key`. The
    /// counts are copied under the lock and formatted after it.
    fn histograms(&self, key: u64) -> Vec<Value> {
        let slots: Vec<(String, &'static str, [u32; BUCKETS])> = lock(&self.windows)
            .iter()
            .filter_map(|((method, outcome), window)| {
                window
                    .slot(key)
                    .map(|counts| (method.clone(), *outcome, *counts))
            })
            .collect();
        slots
            .into_iter()
            .map(|(method, outcome, counts)| {
                let buckets: Vec<[u64; 2]> = (0u64..)
                    .zip(counts)
                    .filter(|&(_, count)| count > 0)
                    .map(|(bucket, count)| [bucket, u64::from(count)])
                    .collect();
                json!({
                    "method": method,
                    "outcome": outcome,
                    "slotSecs": SLOT_SECS,
                    "bucketGrowth": BUCKET_GROWTH,
                    "samples": counts.iter().map(|&count| u64::from(count)).sum::<u64>(),
                    "buckets": buckets,
                })
            })
            .collect()
    }
}

impl LatencyBook {
    #[cfg(test)]
    pub(crate) fn outbound_bytes_for_test(&self, key: u64) -> Vec<Value> {
        self.outbound_bytes(key)
    }

    /// One `rpc.bytes` payload per method for each slot before `key`, removed as returned.
    fn outbound_bytes(&self, key: u64) -> Vec<Value> {
        let mut finished = Vec::new();
        lock(&self.bytes).retain(|(method, slot), counts| {
            if *slot >= key {
                return true;
            }
            finished.push(json!({
                "method": method,
                "slotSecs": SLOT_SECS,
                "slotStartSecs": slot * SLOT_SECS,
                "messages": counts.messages,
                "bytes": counts.bytes,
                "maxBytes": counts.max_bytes,
            }));
            false
        });
        finished
    }
}

pub fn global_latency() -> &'static LatencyBook {
    static BOOK: LazyLock<LatencyBook> = LazyLock::new(LatencyBook::default);
    &BOOK
}

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Monotonic seconds for slot keys, so wall-clock steps never age or revive a slot.
fn now_secs() -> u64 {
    START.elapsed().as_secs()
}

fn ms_since_start(at: Instant) -> u64 {
    u64::try_from(at.saturating_duration_since(*START).as_millis()).unwrap_or(u64::MAX)
}

/// The detector's last tick in `ms_since_start` terms, or 0 while no detector runs.
static LAST_TICK_MS: AtomicU64 = AtomicU64::new(0);
/// When observation last restarted: a recording started, or the machine woke up.
static FENCE_MS: AtomicU64 = AtomicU64::new(0);

/// Whether a request started at `started_ms` spans a gap in observation: it started before the
/// last fence, or the detector heartbeat is overdue because the machine has just woken up.
fn spans_suspend(started_ms: u64, now_ms: u64, last_tick_ms: u64, fence_ms: u64) -> bool {
    started_ms < fence_ms || (last_tick_ms != 0 && slept(last_tick_ms, now_ms))
}

/// See `SUSPEND_GAP_MS`: only Windows can tell a sleep from a stall this way.
fn slept(last_tick_ms: u64, now_ms: u64) -> bool {
    cfg!(windows) && now_ms.saturating_sub(last_tick_ms) > SUSPEND_GAP_MS
}

/// A request in flight across a stop and start, or across a sleep, belongs to no recording:
/// its timer is discarded and it is never reported as hung.
fn fence(fence_ms: u64) {
    FENCE_MS.store(fence_ms, Ordering::Relaxed);
    mute_before(&mut lock(&IN_FLIGHT), fence_ms);
}

fn mute_before(in_flight: &mut HashMap<u64, InFlight>, fence_ms: u64) {
    for request in in_flight.values_mut() {
        if ms_since_start(request.started) < fence_ms {
            request.reported = true;
        }
    }
}

/// The parked slow results as findings: one `rpc.slow` per `(method, outcome)`, or a single
/// `rpc.saturation` when five or more distinct methods were slow in the same tick.
fn slow_findings(slow: HashMap<MethodKey, Slow>) -> Vec<FindingInput> {
    let methods: HashSet<&str> = slow.keys().map(|(method, _)| method.as_str()).collect();
    if methods.len() >= SATURATION_METHODS {
        let severity = severity(slow.values().map(|parked| parked.tier).max());
        let method_count = methods.len();
        let mut evidence: Vec<Value> = slow
            .iter()
            .map(|((method, outcome), parked)| {
                json!({
                    "method": method,
                    "outcome": outcome,
                    "count": parked.count,
                    "maxMs": parked.max_ms,
                })
            })
            .collect();
        evidence.sort_by(|a, b| a["method"].as_str().cmp(&b["method"].as_str()));
        return vec![FindingInput {
            kind: "rpc.saturation",
            severity,
            source: "server".into(),
            fingerprint_fields: vec!["server".into()],
            title: format!("{method_count} RPC methods were slow at once"),
            summary: format!(
                "{method_count} methods took over 1 s on the server within one 5 s interval, \
                 which points at a saturated server rather than one slow handler."
            ),
            evidence: json!({ "slowPart": "server", "methods": evidence }),
        }];
    }
    slow.into_iter()
        .map(|((method, outcome), parked)| {
            let secs = parked.max_ms / 1000.0;
            FindingInput {
                kind: "rpc.slow",
                severity: severity(Some(parked.tier)),
                source: "server".into(),
                title: format!("{method} took {secs:.1} s"),
                summary: format!(
                    "{count} {outcome} call(s) took over 1 s on the server within 5 s; the \
                     slowest took {secs:.1} s.",
                    count = parked.count,
                ),
                evidence: cite(
                    json!({
                        "method": method,
                        "outcome": outcome,
                        "slowPart": "server",
                        "count": parked.count,
                        "maxMs": parked.max_ms,
                        "tier": parked.tier.name(),
                    }),
                    parked.slowest_event,
                ),
                fingerprint_fields: vec![method, outcome.to_owned(), "server".into()],
            }
        })
        .collect()
}

fn severity(tier: Option<Tier>) -> Severity {
    if tier == Some(Tier::Severe) {
        Severity::Error
    } else {
        Severity::Warn
    }
}

struct InFlight {
    method: String,
    started: Instant,
    hang_after: Duration,
    reported: bool,
}

impl InFlight {
    fn new(method: &str, started: Instant) -> Self {
        Self {
            method: method.to_owned(),
            started,
            hang_after: if is_long_running(method) {
                LONG_RUNNING_HANG_AFTER
            } else {
                HANG_AFTER
            },
            reported: false,
        }
    }
}

/// Unary requests running while the profiler is on, for the hang check.
static IN_FLIGHT: LazyLock<Mutex<HashMap<u64, InFlight>>> = LazyLock::new(Default::default);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);

/// The requests past their hang budget (10 s, or 300 s for long-running methods) that were
/// not reported yet, now marked reported.
fn overdue(in_flight: &mut HashMap<u64, InFlight>, now: Instant) -> Vec<(String, Duration)> {
    in_flight
        .values_mut()
        .filter_map(|request| {
            let elapsed = now.saturating_duration_since(request.started);
            (!request.reported && elapsed > request.hang_after).then(|| {
                request.reported = true;
                (request.method.clone(), elapsed)
            })
        })
        .collect()
}

fn hang_finding(method: &str, elapsed: Duration) -> FindingInput {
    let secs = elapsed.as_secs();
    FindingInput {
        kind: "rpc.hang",
        severity: Severity::Error,
        source: "server".into(),
        fingerprint_fields: vec![method.to_owned(), "server".into()],
        title: format!("{method} still running after {secs} s"),
        summary: format!(
            "A {method} request has run on the server for {secs} s without completing."
        ),
        evidence: json!({ "method": method, "slowPart": "server", "elapsedSecs": secs }),
    }
}

/// The correlation header's value, when present and at most 64 bytes. A longer value is
/// treated as absent: a truncated ID could join the wrong request.
pub(super) fn correlation_id(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(CORRELATION_HEADER))
        .filter(|(_, value)| !value.is_empty() && value.len() <= MAX_CORRELATION_ID_BYTES)
        .map(|(_, value)| value.clone())
}

/// Times one request from handler start while the profiler is on. Dropping it unfinished (a
/// cancelled or panicked handler) only takes it out of the in-flight registry.
pub(crate) struct RequestTimer {
    started: Instant,
    correlation_id: Option<String>,
    in_flight: Option<u64>,
}

impl RequestTimer {
    /// Times a unary request and registers it for the hang check. `None`, with nothing read,
    /// inserted or allocated, while the profiler is off.
    pub(crate) fn unary(method: &str, headers: &[(String, String)]) -> Option<Self> {
        if !profiling() {
            return None;
        }
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let request = InFlight::new(method, started);
        lock(&IN_FLIGHT).insert(id, request);
        Some(Self {
            started,
            correlation_id: correlation_id(headers),
            in_flight: Some(id),
        })
    }

    /// Times a stream to its first item; see `FIRST_ITEM`.
    pub(crate) fn stream(headers: &[(String, String)]) -> Option<Self> {
        profiling().then(|| Self {
            started: Instant::now(),
            correlation_id: correlation_id(headers),
            in_flight: None,
        })
    }

    /// Records the request, unless the profiler is off, was restarted while it ran, or the
    /// machine slept while it ran.
    pub(crate) fn finish(self, method: &str, outcome: &'static str) {
        if !profiling() {
            return;
        }
        let now = Instant::now();
        // The heartbeat first: a fresh one guarantees the fence published before it is seen.
        let last_tick = LAST_TICK_MS.load(Ordering::Acquire);
        let fence = FENCE_MS.load(Ordering::Relaxed);
        if spans_suspend(
            ms_since_start(self.started),
            ms_since_start(now),
            last_tick,
            fence,
        ) {
            return;
        }
        let ms = now.saturating_duration_since(self.started).as_secs_f64() * 1000.0;
        global_latency().observe(method, outcome, ms, 0.0, self.correlation_id.as_deref());
    }
}

impl Drop for RequestTimer {
    fn drop(&mut self) {
        if let Some(id) = self.in_flight {
            lock(&IN_FLIGHT).remove(&id);
        }
    }
}

/// The slots that finished since slot `summarized` was current and are still in the window at
/// slot `key`: one per tick normally, several after a late tick.
fn finished_slots(summarized: u64, key: u64) -> std::ops::Range<u64> {
    summarized.max(key.saturating_sub(SLOTS - 1))..key
}

/// Raises the parked `rpc.slow` findings (or one `rpc.saturation`), flags hung requests and
/// emits one `rpc.histogram` per method for each finished 5-minute slot.
pub struct LatencyDetector;

impl Detector for LatencyDetector {
    fn name(&self) -> &'static str {
        "latency"
    }

    fn start(&self, cancel: CancellationToken) -> JoinHandle<()> {
        // Published before spawning, so a request that spans the last stop cannot finish into
        // this recording before the task first runs. `max(1)`: a heartbeat of 0 means none.
        let started = ms_since_start(Instant::now()).max(1);
        fence(started);
        let book = global_latency();
        // Results parked before the profiler was last switched off are stale.
        lock(&book.slow).clear();
        lock(&book.bytes).clear();
        tokio::spawn(async move {
            let mut summarized = now_secs() / SLOT_SECS;
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            LAST_TICK_MS.store(started, Ordering::Release);
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        LAST_TICK_MS.store(0, Ordering::Relaxed);
                        return;
                    }
                    _ = ticks.tick() => {}
                }
                let now = ms_since_start(Instant::now()).max(1);
                if slept(LAST_TICK_MS.load(Ordering::Relaxed), now) {
                    // Whatever ran across the gap measured the sleep, not the server.
                    fence(now);
                }
                // After the fence, so a timer that sees this heartbeat also sees the fence.
                LAST_TICK_MS.store(now, Ordering::Release);
                let slow = std::mem::take(&mut *lock(&book.slow));
                let hung = overdue(&mut lock(&IN_FLIGHT), Instant::now());
                for finding in slow_findings(slow) {
                    record_finding(finding);
                }
                for (method, elapsed) in hung {
                    record_finding(hang_finding(&method, elapsed));
                }
                let key = now_secs() / SLOT_SECS;
                for slot in finished_slots(summarized, key) {
                    for payload in book.histograms(slot) {
                        emit("rpc.histogram", payload);
                    }
                }
                for payload in book.outbound_bytes(key) {
                    emit("rpc.bytes", payload);
                }
                summarized = key;
            }
        })
    }
}

#[cfg(test)]
thread_local! {
    /// Pins `profiling()` on the current thread. The global handle is shared by every lib
    /// test that starts a server, so session tests pin the gate instead; `#[tokio::test]` runs
    /// a session and its request tasks on the test thread.
    static PINNED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) struct PinnedProfiling;

#[cfg(test)]
impl PinnedProfiling {
    pub(crate) fn new(on: bool) -> Self {
        PINNED.with(|pinned| pinned.set(Some(on)));
        Self
    }
}

#[cfg(test)]
impl Drop for PinnedProfiling {
    fn drop(&mut self) {
        PINNED.with(|pinned| pinned.set(None));
    }
}

#[cfg(test)]
pub(crate) fn in_flight(method: &str) -> bool {
    lock(&IN_FLIGHT)
        .values()
        .any(|request| request.method == method)
}

/// The gate every request hook checks first.
fn profiling() -> bool {
    #[cfg(test)]
    if let Some(pinned) = PINNED.with(std::cell::Cell::get) {
        return pinned;
    }
    super::is_enabled()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_tiers_fire_without_baseline() {
        assert_eq!(judge(1_200.0, None, 0, true), Tier::Warn);
        assert_eq!(judge(10_500.0, None, 0, true), Tier::Severe);
        assert_eq!(judge(400.0, None, 0, true), Tier::None);
    }

    #[test]
    fn relative_tier_needs_100_samples_and_respects_floor() {
        assert_eq!(judge(400.0, Some(100.0), 99, true), Tier::None);
        assert_eq!(judge(400.0, Some(100.0), 100, true), Tier::RelativeLogOnly); // > max(300, 250)
        assert_eq!(judge(240.0, Some(50.0), 500, true), Tier::None); // under 250 ms floor
        assert_eq!(judge(900.0, Some(100.0), 500, false), Tier::None); // under 1 s background floor
    }

    #[test]
    fn p95_excludes_current_slot_and_ages_out() {
        let mut w = LatencyWindow::default();
        for _ in 0..200 {
            w.record(10.0, 0);
        } // slot 0
        for _ in 0..5 {
            w.record(5_000.0, 301);
        } // current slot (1)
        let p95 = w.p95_excluding_current(301).unwrap();
        assert!(p95 < 15.0, "burst must not raise its own threshold: {p95}");
        assert!(
            w.p95_excluding_current(301 + 6 * 300).is_none(),
            "30 min later the window is empty"
        );
    }

    #[test]
    fn join_log_is_bounded() {
        let mut log = JoinLog::default();
        for i in 0..5000 {
            log.push(format!("t{i}"), "m".into(), 1.0, 0.0);
        }
        assert!(log.find("t0").is_none());
        assert!(log.find("t4999").is_some());
    }

    #[test]
    fn samples_count_the_same_slots_as_the_p95() {
        let mut w = LatencyWindow::default();
        for _ in 0..150 {
            w.record(10.0, 0);
        }
        w.record(10.0, 300);
        assert_eq!(
            w.samples(300),
            150,
            "the current slot is not part of the baseline"
        );
        assert_eq!(
            w.samples(5 * 300),
            151,
            "five slots back is still in the window"
        );
        assert_eq!(w.samples(6 * 300), 1, "six slots back has aged out");
    }

    #[test]
    fn background_methods_use_the_background_floor() {
        for method in [
            "git.resolvePullRequest",
            "vcs.refreshStatus",
            "worktree.remove",
            "pullRequests.list",
            "server.refreshProviders",
            "projects.searchEntries",
        ] {
            assert!(is_background(method), "{method}");
        }
        for method in [
            "gitManager.runOperation",
            "server.getConfig",
            "projects.list",
        ] {
            assert!(!is_background(method), "{method}");
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn correlation_id_comes_only_from_its_bounded_header() {
        assert_eq!(
            correlation_id(&headers(&[("X-Bibcode-Profiler-Rid", "c1:7")])).as_deref(),
            Some("c1:7")
        );
        assert_eq!(correlation_id(&headers(&[("traceparent", "00-abc")])), None);
        assert_eq!(correlation_id(&headers(&[(CORRELATION_HEADER, "")])), None);
        let longest = "x".repeat(64);
        assert_eq!(
            correlation_id(&headers(&[(CORRELATION_HEADER, &longest)])),
            Some(longest)
        );
        assert_eq!(
            correlation_id(&headers(&[(CORRELATION_HEADER, &"x".repeat(65))])),
            None,
            "an over-long ID is not joined rather than truncated into a collision"
        );
    }

    #[test]
    fn observe_parks_only_absolute_tiers_of_unary_requests() {
        let book = LatencyBook::default();
        book.observe("test.fast", "success", 5.0, 0.0, None);
        book.observe("test.slow", "success", 1_500.0, 0.0, None);
        book.observe("test.slow", "success", 12_000.0, 0.0, None);
        book.observe("test.stream", FIRST_ITEM, 12_000.0, 0.0, Some("c1:1"));
        let slow = lock(&book.slow);
        assert_eq!(
            slow.len(),
            1,
            "fast requests and stream first items park nothing"
        );
        let parked = &slow[&("test.slow".to_owned(), "success")];
        assert_eq!(
            (parked.count, parked.max_ms, parked.tier),
            (2, 12_000.0, Tier::Severe)
        );
        drop(slow);
        assert_eq!(
            book.find_join("c1:1").map(|entry| entry.method),
            Some("test.stream".to_owned())
        );
    }

    fn parked(
        entries: &[(&str, &'static str, Tier, f64)],
    ) -> HashMap<(String, &'static str), Slow> {
        entries
            .iter()
            .map(|(method, outcome, tier, max_ms)| {
                (
                    ((*method).to_owned(), *outcome),
                    Slow {
                        count: 1,
                        max_ms: *max_ms,
                        tier: *tier,
                        slowest_event: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn fewer_than_five_slow_methods_raise_one_finding_each() {
        // Two outcomes of one method count as one method.
        let findings = slow_findings(parked(&[
            ("a", "success", Tier::Warn, 1_200.0),
            ("a", "failure", Tier::Warn, 1_300.0),
            ("b", "success", Tier::Severe, 10_500.0),
            ("c", "success", Tier::Warn, 1_000.0),
            ("d", "success", Tier::Warn, 1_000.0),
        ]));
        assert_eq!(findings.len(), 5);
        assert!(findings.iter().all(|finding| finding.kind == "rpc.slow"));
        let severe = findings
            .iter()
            .find(|finding| finding.fingerprint_fields[0] == "b")
            .unwrap();
        assert_eq!(severe.severity, Severity::Error);
        assert_eq!(severe.title, "b took 10.5 s");
        assert_eq!(severe.fingerprint_fields, ["b", "success", "server"]);
    }

    /// The finding cites the `rpc.timing` event of the call its `maxMs` describes.
    #[test]
    fn a_slow_finding_cites_the_slowest_calls_timing_event() {
        let mut slow = Slow::default();
        slow.add(1_200.0, Tier::Warn, Some(EventId::new(0xa1, 1)));
        slow.add(3_000.0, Tier::Warn, Some(EventId::new(0xa1, 2)));
        slow.add(1_100.0, Tier::Warn, Some(EventId::new(0xa1, 3)));
        assert_eq!((slow.count, slow.max_ms), (3, 3_000.0));
        let findings = slow_findings(HashMap::from([(("a".to_owned(), "success"), slow)]));
        assert_eq!(findings[0].evidence["eventIds"], json!(["000000a1-2"]));

        // An event the full queue dropped has no id to cite, and a faster call's event does
        // not stand in for it.
        let mut dropped = Slow::default();
        dropped.add(3_000.0, Tier::Warn, None);
        dropped.add(1_200.0, Tier::Warn, Some(EventId::new(0xa1, 4)));
        let findings = slow_findings(HashMap::from([(("a".to_owned(), "success"), dropped)]));
        assert!(findings[0].evidence.get("eventIds").is_none());
    }

    #[test]
    fn five_slow_methods_in_one_tick_raise_one_saturation_finding() {
        let findings = slow_findings(parked(&[
            ("a", "success", Tier::Warn, 1_200.0),
            ("b", "success", Tier::Warn, 1_200.0),
            ("c", "success", Tier::Warn, 1_200.0),
            ("d", "success", Tier::Warn, 1_200.0),
            ("e", "success", Tier::Severe, 11_000.0),
        ]));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "rpc.saturation");
        assert_eq!(findings[0].fingerprint_fields, ["server"]);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(slow_findings(HashMap::new()).is_empty());
    }

    #[test]
    fn a_hung_request_is_reported_once() {
        let now = Instant::now();
        let mut in_flight = HashMap::from([
            (1, InFlight::new("test.hung", now - Duration::from_secs(11))),
            (2, InFlight::new("test.busy", now - Duration::from_secs(9))),
        ]);
        let hung = overdue(&mut in_flight, now);
        assert_eq!(hung.len(), 1);
        assert_eq!(hung[0].0, "test.hung");
        assert!(overdue(&mut in_flight, now).is_empty(), "once per request");
        let finding = hang_finding(&hung[0].0, hung[0].1);
        assert_eq!(finding.kind, "rpc.hang");
        assert_eq!(finding.fingerprint_fields, ["test.hung", "server"]);
    }

    #[test]
    fn a_dropped_timer_leaves_the_in_flight_registry_and_off_registers_nothing() {
        let _profiling = PinnedProfiling::new(true);
        let timer = RequestTimer::unary("test.latency.dropped", &[]).expect("profiling");
        assert!(in_flight("test.latency.dropped"));
        drop(timer); // a cancelled or panicked handler
        assert!(!in_flight("test.latency.dropped"));

        let _profiling = PinnedProfiling::new(false);
        assert!(RequestTimer::unary("test.latency.off", &[]).is_none());
        assert!(RequestTimer::stream(&[]).is_none());
        assert!(!in_flight("test.latency.off"));
    }

    #[test]
    fn requests_spanning_a_suspension_are_not_measured() {
        let gap = SUSPEND_GAP_MS;
        // No detector heartbeat yet (tests, or the first instant after a start).
        assert!(!spans_suspend(100, 200, 0, 0));
        // Heartbeat fresh, no resume since the request started.
        assert!(!spans_suspend(100, 5_000, 4_000, 0));
        // Started before the last fence: a recording start, or the detector seeing a resume.
        assert!(spans_suspend(100, 90_000, 89_000, 80_000));
        assert!(!spans_suspend(80_000, 90_000, 89_000, 80_000));
        // The machine just woke and the detector has not ticked since. Only Windows `Instant`
        // counts suspended time; elsewhere a late heartbeat is a stall worth measuring.
        assert_eq!(spans_suspend(100, 4_000 + gap + 1, 4_000, 0), cfg!(windows));
    }

    #[test]
    fn tier_order_puts_the_log_only_tier_below_the_finding_tiers() {
        // `observe` parks results with `tier >= Tier::Warn`.
        assert!(Tier::None < Tier::RelativeLogOnly);
        assert!(Tier::RelativeLogOnly < Tier::Warn);
        assert!(Tier::Warn < Tier::Severe);
    }

    #[test]
    fn long_running_methods_raise_no_slow_or_saturation_findings() {
        let book = LatencyBook::default();
        book.observe("vcs.clone", "success", 12_000.0, 0.0, None);
        for method in [
            "sourceControl.cloneRepository",
            "vcs.pull",
            "updater.check",
            "pullRequests.list",
            "gitManager.commit",
            "worktree.remove",
            "server.getProviderCapabilities",
        ] {
            book.observe(method, "success", 1_500.0, 0.0, None);
        }
        let parked = std::mem::take(&mut *lock(&book.slow));
        assert!(slow_findings(parked).is_empty());
        assert!(
            lock(&book.windows).contains_key(&("vcs.clone".to_owned(), "success")),
            "still timed"
        );
        assert!(!is_long_running("server.getConfig"));
        assert!(
            !is_long_running("worktree.createPanel"),
            "local orchestration only: a stall there is a server problem"
        );
        // Bounded: probes with their own timeouts, and a merge preview capped at the 30 s
        // git command timeout. A slow one is a slowness worth reporting.
        assert!(!is_long_running("worktree.adopt"));
        assert!(!is_long_running("gitManager.previewMerge"));
    }

    #[test]
    fn a_long_running_request_has_a_300_second_hang_budget() {
        let now = Instant::now();
        let mut in_flight = HashMap::from([
            (1, InFlight::new("vcs.clone", now - Duration::from_secs(11))),
            (
                2,
                InFlight::new("updater.install", now - Duration::from_secs(301)),
            ),
        ]);
        let hung = overdue(&mut in_flight, now);
        assert_eq!(hung.len(), 1);
        assert_eq!(hung[0].0, "updater.install");
    }

    #[test]
    fn a_fence_mutes_only_requests_that_started_before_it() {
        let request = |secs| InFlight::new("test.fence", *START + Duration::from_secs(secs));
        let mut in_flight = HashMap::from([(1, request(1)), (2, request(3))]);
        mute_before(&mut in_flight, 2_000);
        assert!(
            in_flight[&1].reported,
            "spans the fence: never reported as hung"
        );
        assert!(
            !in_flight[&2].reported,
            "admitted after the fence: still checked"
        );
    }

    #[test]
    fn a_late_tick_summarizes_every_finished_slot_still_in_the_window() {
        assert!(finished_slots(3, 3).is_empty());
        assert_eq!(finished_slots(3, 4), 3..4);
        assert_eq!(finished_slots(0, 2), 0..2);
        assert_eq!(finished_slots(0, 10), 5..10, "older slots have aged out");
    }

    #[test]
    fn histograms_summarize_one_slot_sparsely() {
        let book = LatencyBook::default();
        let mut window = LatencyWindow::default();
        window.record(10.0, 0);
        window.record(10.0, 0);
        window.record(2_000.0, 300);
        lock(&book.windows).insert(("test.histogram".to_owned(), "success"), window);
        let summaries = book.histograms(0);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["method"], "test.histogram");
        assert_eq!(summaries[0]["samples"], 2);
        assert_eq!(summaries[0]["buckets"], json!([[bucket_of(10.0), 2]]));
        assert!(book.histograms(7).is_empty());
    }
}
