//! The profiler handle is process-global, and every `ProfilerRuntime::start` competes for
//! it. The scenarios therefore run one after another inside a single `#[tokio::test]`, each
//! runtime shut down (which uninstalls its handle) before the next one starts.

use std::{
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use bibcode_server::diagnostics::{
    ProcessAttributionRegistry,
    profiler::{
        self, client, crash,
        findings::{FindingInput, Severity},
        latency,
        runtime::{Detector, ProfilerRuntime},
        sampler::SamplerDetector,
    },
};
use futures_util::FutureExt;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Counts {
    live: Arc<AtomicI64>,
    /// The most instances ever live at once.
    max: Arc<AtomicI64>,
    starts: Arc<AtomicI64>,
}

impl Counts {
    fn get(counter: &AtomicI64) -> i64 {
        counter.load(Ordering::SeqCst)
    }
}

/// Counts its instances; each one takes `.1` to wind down once cancelled.
struct CountingDetector(Counts, Duration);

impl Detector for CountingDetector {
    fn name(&self) -> &'static str {
        "counting"
    }

    fn start(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let (counts, wind_down) = (self.0.clone(), self.1);
        counts.starts.fetch_add(1, Ordering::SeqCst);
        let live = counts.live.fetch_add(1, Ordering::SeqCst) + 1;
        counts.max.fetch_max(live, Ordering::SeqCst);
        tokio::spawn(async move {
            cancel.cancelled().await;
            tokio::time::sleep(wind_down).await;
            counts.live.fetch_sub(1, Ordering::SeqCst);
        })
    }
}

fn crash_finding() -> FindingInput {
    FindingInput {
        kind: "crash.unclean-exit",
        severity: Severity::Error,
        source: "server".into(),
        fingerprint_fields: vec!["server".into()],
        title: "Previous run did not exit cleanly".into(),
        summary: "no OS crash record".into(),
        evidence: serde_json::json!({}),
    }
}

#[tokio::test]
async fn profiler_runtime_scenarios_share_the_process_global_handle_in_sequence() {
    rapid_toggles_never_run_two_detector_sets().await;
    without_a_crash_hook_no_monitor_starts().await;
    // From here on this process has its crash hook, as every BiBCode binary does.
    crash::attach();
    rapid_toggles_never_leave_a_crash_monitor_behind().await;
    #[cfg(unix)]
    a_stuck_runtime_still_replaces_a_hung_monitor().await;
    status_reports_findings_and_disk_use().await;
    a_second_runtime_on_the_same_directory_is_degraded().await;
    a_dropped_runtime_stops_its_detectors_before_releasing_the_directory().await;
    a_repeated_shutdown_waits_for_a_cancelled_one().await;
    an_unreadable_findings_file_reports_failing_storage().await;
    a_failed_startup_compaction_reports_failing_storage().await;
    the_sampler_records_the_server_process().await;
    turning_the_profiler_off_writes_the_open_minute().await;
    a_client_batch_is_joined_to_the_server_timing().await;
}

async fn rapid_toggles_never_run_two_detector_sets() {
    let dir = tempfile::tempdir().unwrap();
    let counts = Counts::default();
    let (tx, rx) = watch::channel(false);
    // Each detector takes 50 ms to wind down, so a stop is still in flight when the next
    // start is requested.
    let runtime = ProfilerRuntime::start(
        dir.path(),
        rx,
        vec![Box::new(CountingDetector(
            counts.clone(),
            Duration::from_millis(50),
        ))],
    );
    for on in [true, false, true, false, true] {
        tx.send(on).unwrap();
        // Let the supervisor act on every flip instead of only the last one.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        Counts::get(&counts.starts) >= 2,
        "the flips were applied as a stop followed by a start"
    );
    assert_eq!(Counts::get(&counts.max), 1, "two detector sets ran at once");
    assert_eq!(Counts::get(&counts.live), 1, "the last flip turned it on");
    assert_eq!(runtime.status()["enabled"], true);
    assert!(runtime.status()["recordingSince"].is_string());
    tx.send(false).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(Counts::get(&counts.live), 0);
    assert_eq!(Counts::get(&counts.max), 1);
    assert_eq!(runtime.status()["enabled"], false);
    assert!(runtime.status()["recordingSince"].is_null());
    runtime.shutdown().await;
}

/// The monitor serves the crash hook. Where none is attached, as in this test binary until it
/// calls `crash::attach()`, no monitor starts and dumps are recorded as unavailable.
async fn without_a_crash_hook_no_monitor_starts() {
    let dir = tempfile::tempdir().unwrap();
    let profiler_dir = profiler::profiler_dir(dir.path());
    let (_tx, rx) = watch::channel(true);
    let detector = crash::MonitorDetector::new(profiler_dir.clone(), "server")
        .with_executable(env!("CARGO_BIN_EXE_bibcode").into());
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![Box::new(detector)]);
    let expected = if cfg!(all(windows, target_arch = "aarch64")) {
        "disabled"
    } else {
        "unavailable"
    };
    wait_until("dumps are recorded as unavailable", || {
        native_dumps(&profiler_dir) == expected
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(monitor_children().is_empty(), "a monitor started");
    assert_eq!(native_dumps(&profiler_dir), expected);
    runtime.shutdown().await;
}

/// Review Focus 2: off, on, off in quick succession ends with no monitor child. The test
/// binary cannot be the monitor, so the detector re-executes `bibcode`, whose `main`
/// dispatches `--crash-monitor` first, as every BiBCode binary does.
async fn rapid_toggles_never_leave_a_crash_monitor_behind() {
    let dir = tempfile::tempdir().unwrap();
    let profiler_dir = profiler::profiler_dir(dir.path());
    // Left readable by others, as an older build or a umask might leave it.
    std::fs::create_dir_all(profiler_dir.join("crashes")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let readable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(profiler_dir.join("crashes"), readable).unwrap();
    }
    let (tx, rx) = watch::channel(true);
    let detector = crash::MonitorDetector::new(profiler_dir.clone(), "server")
        .with_executable(env!("CARGO_BIN_EXE_bibcode").into());
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![Box::new(detector)]);
    let disabled = cfg!(all(windows, target_arch = "aarch64"));
    let expected = if disabled { "disabled" } else { "available" };
    wait_until("the monitor is connected", || {
        native_dumps(&profiler_dir) == expected
            && monitor_children().len() == usize::from(!disabled)
    })
    .await;
    #[cfg(unix)]
    if !disabled {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(profiler_dir.join("crashes"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "dumps hold raw process memory");
    }

    for on in [false, true, false] {
        tx.send(on).unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    wait_until("the last flip stops the monitor", || {
        monitor_children().is_empty()
    })
    .await;
    // Nothing restarts it later.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(monitor_children().is_empty());
    runtime.shutdown().await;
}

/// The detector must replace a monitor that stopped answering even while the runtime that
/// started it is stuck, as it is when its only worker faulted and waits on that monitor.
/// This test's runtime has one thread, so blocking it stands in for that fault.
#[cfg(unix)]
async fn a_stuck_runtime_still_replaces_a_hung_monitor() {
    let dir = tempfile::tempdir().unwrap();
    let profiler_dir = profiler::profiler_dir(dir.path());
    let (_tx, rx) = watch::channel(true);
    let detector = crash::MonitorDetector::new(profiler_dir.clone(), "server")
        .with_executable(env!("CARGO_BIN_EXE_bibcode").into());
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![Box::new(detector)]);
    wait_until("the monitor is connected", || {
        native_dumps(&profiler_dir) == "available" && monitor_children().len() == 1
    })
    .await;
    let hung = monitor_children()[0];
    // SAFETY: a plain signal to this test's own child.
    assert_eq!(
        unsafe { libc::kill(hung.as_u32() as libc::pid_t, libc::SIGSTOP) },
        0
    );
    // The next heartbeat is at most 10 s away and gives up after 5 s.
    std::thread::sleep(Duration::from_secs(20));
    let children = monitor_children();
    // SAFETY: as above; a stopped monitor cannot notice anything by itself.
    unsafe { libc::kill(hung.as_u32() as libc::pid_t, libc::SIGKILL) };
    assert!(
        children.len() == 1 && children[0] != hung,
        "the stopped monitor was not replaced: {children:?} (stopped: {hung})"
    );
    runtime.shutdown().await;
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting until {what}");
}

fn native_dumps(profiler_dir: &std::path::Path) -> String {
    std::fs::read(profiler_dir.join("capabilities.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|capabilities| capabilities["nativeDumps"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Live `--crash-monitor` children of this process.
fn monitor_children() -> Vec<Pid> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_cmd(UpdateKind::Always),
    );
    let this = Pid::from_u32(std::process::id());
    system
        .processes()
        .values()
        .filter(|process| {
            process.parent() == Some(this)
                && process.cmd().iter().any(|arg| arg == "--crash-monitor")
        })
        .map(sysinfo::Process::pid)
        .collect()
}

async fn status_reports_findings_and_disk_use() {
    let dir = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![]);
    tokio::time::sleep(Duration::from_millis(50)).await;
    profiler::record_finding(crash_finding());
    let status = runtime.status();
    assert_eq!(status["findings"][0]["kind"], "crash.unclean-exit");
    assert!(status["lastCrashAt"].is_string());
    assert_eq!(status["capabilities"]["storage"], "ok");
    assert_eq!(status["capabilities"]["nativeDumps"], "unavailable");
    runtime.shutdown().await;
    let findings =
        std::fs::read_to_string(profiler::profiler_dir(dir.path()).join("findings.ndjson"))
            .unwrap();
    assert!(findings.contains("crash.unclean-exit"));
}

async fn a_second_runtime_on_the_same_directory_is_degraded() {
    let dir = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(true);
    let first = ProfilerRuntime::start(dir.path(), rx.clone(), vec![]);
    let second = ProfilerRuntime::start(dir.path(), rx.clone(), vec![]);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let degraded = second.status();
    assert_eq!(
        degraded["capabilities"]["storage"], "locked",
        "another process holds the directory: nothing failed to write"
    );
    assert_eq!(degraded["enabled"], false);
    assert_eq!(first.status()["capabilities"]["storage"], "ok");
    assert_eq!(first.status()["enabled"], true);

    profiler::record_finding(crash_finding());
    assert_eq!(
        first.status()["findings"][0]["kind"],
        "crash.unclean-exit",
        "the first runtime keeps recording"
    );
    assert_eq!(second.status()["findings"], serde_json::json!([]));

    second.shutdown().await;
    let third = ProfilerRuntime::start(dir.path(), rx, vec![]);
    assert_eq!(
        third.status()["capabilities"]["storage"],
        "locked",
        "the degraded runtime's shutdown must not release the first runtime's lock"
    );
    third.shutdown().await;
    first.shutdown().await;
}

/// A server start that fails after creating the runtime drops it without `shutdown`.
async fn a_dropped_runtime_stops_its_detectors_before_releasing_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let counts = Counts::default();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(
        dir.path(),
        rx.clone(),
        vec![Box::new(CountingDetector(
            counts.clone(),
            Duration::from_millis(100),
        ))],
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(Counts::get(&counts.live), 1);
    // Dropped on a thread outside the Tokio runtime, which must still run the teardown.
    std::thread::spawn(move || drop(runtime)).join().unwrap();
    // Teardown is still running: `start` waits for the lock instead of degrading.
    let next = start_off_the_workers(dir.path(), rx).await;
    assert_eq!(
        Counts::get(&counts.live),
        0,
        "the old detector set stopped before the directory was released"
    );
    assert_takes_over(next).await;
}

/// The update quiesce stops waiting at its deadline, and the server's cleanup then calls
/// `shutdown` again: that call must return only once teardown is complete.
async fn a_repeated_shutdown_waits_for_a_cancelled_one() {
    let dir = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(dir.path(), rx.clone(), vec![]);
    // Poll shutdown exactly once, then drop it, as a cancelled caller does.
    assert!(runtime.shutdown().now_or_never().is_none());
    runtime.shutdown().await;
    assert_takes_over(ProfilerRuntime::start(dir.path(), rx, vec![])).await;
}

async fn an_unreadable_findings_file_reports_failing_storage() {
    let dir = tempfile::tempdir().unwrap();
    // A directory in its place: opening or reading it fails, so the findings are not loaded.
    std::fs::create_dir_all(profiler::profiler_dir(dir.path()).join("findings.ndjson")).unwrap();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![]);
    assert_eq!(runtime.status()["capabilities"]["storage"], "failing");
    runtime.shutdown().await;
}

async fn a_failed_startup_compaction_reports_failing_storage() {
    let dir = tempfile::tempdir().unwrap();
    let profiler_dir = profiler::profiler_dir(dir.path());
    std::fs::create_dir_all(profiler_dir.join("findings.ndjson.tmp")).unwrap();
    std::fs::write(profiler_dir.join("findings.ndjson"), b"").unwrap();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![]);
    for _ in 0..200 {
        if runtime.status()["capabilities"]["storage"] == "failing" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(runtime.status()["capabilities"]["storage"], "failing");
    runtime.shutdown().await;
}

/// The production sampler under the runtime, with 250 ms ticks and a 500 ms minute: the
/// first closed minute reaches the events file as a `process.sample` event for this process.
async fn the_sampler_records_the_server_process() {
    let dir = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(
        dir.path(),
        rx,
        vec![Box::new(SamplerDetector::with_timing(
            ProcessAttributionRegistry::new(),
            Duration::from_millis(250),
            Duration::from_millis(500),
        ))],
    );
    let sample = first_process_sample(runtime, dir.path()).await;
    // The widest interval of the minute: the throttle may have doubled it.
    assert!(sample["intervalMs"].as_u64() >= Some(250));
    assert!(sample["selfCpuMs"].is_number());
    let server = server_process(&sample);
    assert!(server["privateMaxBytes"].as_u64() >= server["privateBytes"].as_u64());
    assert!(server["rssMaxBytes"].as_u64() >= server["rssBytes"].as_u64());
    assert!(server["cpuMax"].as_f64() >= server["cpu"].as_f64());
}

/// A capture shorter than a minute: turning the profiler off writes the open minute.
async fn turning_the_profiler_off_writes_the_open_minute() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(
        dir.path(),
        rx,
        vec![Box::new(SamplerDetector::new(
            ProcessAttributionRegistry::new(),
        ))],
    );
    // Let the supervisor start the sampler; its first tick completes before it sees the stop.
    tokio::time::sleep(Duration::from_millis(50)).await;
    tx.send(false).unwrap();
    let sample = first_process_sample(runtime, dir.path()).await;
    assert_eq!(sample["intervalMs"], 5000);
    server_process(&sample);
}

/// Waits for the first written event, shuts the runtime down, and returns the first
/// `process.sample` line. The writer flushes every 2 s, and teardown uninstalls the handle
/// before the sampler stops, so the sample must be written before shutdown.
async fn first_process_sample(
    runtime: ProfilerRuntime,
    state_dir: &std::path::Path,
) -> serde_json::Value {
    for _ in 0..200 {
        if runtime.status()["eventCount"].as_u64() > Some(0) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    runtime.shutdown().await;
    std::fs::read_dir(profiler::profiler_dir(state_dir))
        .unwrap()
        .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok())
        .find(|line| line["kind"] == "process.sample")
        .expect("a process.sample event was written")
}

/// While the profiler is on, a client record is joined to the server's timing by its
/// correlation ID, its events name the client, and a batch over the limits changes nothing.
async fn a_client_batch_is_joined_to_the_server_timing() {
    let dir = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(dir.path(), rx, vec![]);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let events_before = runtime.status()["eventCount"].as_u64().unwrap();
    latency::global_latency().observe("server.getConfig", "success", 900.0, 0.0, Some("c1:7"));
    let record = serde_json::json!({
        "correlationId": "c1:7",
        "method": "server.getConfig",
        "rttMs": 1_500.0,
        "renderMs": 50.0,
        "startedAtMs": 10.0,
    });
    let batch = |records: Vec<serde_json::Value>| {
        serde_json::json!({
            "clientId": "c1",
            "capabilities": ["heartbeat"],
            "windowMs": 10_000,
            "records": records,
            "histograms": [],
            "stalls": { "count": 3, "totalMs": 1_500, "maxMs": 800 },
            "interactions": { "count": 0, "maxMs": 0 },
            "jsHeapBytes": null,
        })
    };

    let accepted = client::ingest_client_batch(batch(vec![record.clone()]), "env-1");
    assert_eq!(accepted, Ok(serde_json::json!({ "accepted": true })));
    let findings = runtime.status()["findings"].clone();
    let slow = findings
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["kind"] == "rpc.slow" && finding["source"] == "client")
        .expect("a client rpc.slow finding");
    assert!(
        slow["summary"]
            .as_str()
            .unwrap()
            .contains("server 900 ms · network 600 ms · render 50 ms"),
        "{slow}"
    );

    let rejected = client::ingest_client_batch(batch(vec![record; 65]), "env-1");
    assert_eq!(rejected.unwrap_err()["_tag"], "ProfilerClientBatchTooLarge");
    assert_eq!(
        runtime.status()["findings"],
        findings,
        "nothing was recorded"
    );

    // The request event, the batch event and the finding's cross-reference.
    for _ in 0..200 {
        if runtime.status()["eventCount"].as_u64() >= Some(events_before + 3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    runtime.shutdown().await;
    let lines: Vec<serde_json::Value> = std::fs::read_dir(profiler::profiler_dir(dir.path()))
        .unwrap()
        .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    let client_events: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|line| line["source"] == "client:c1")
        .collect();
    assert_eq!(
        client_events.len(),
        2,
        "one request and one batch event, none from the rejected batch"
    );
    assert!(
        client_events
            .iter()
            .any(|line| line["kind"] == "client.request"
                && line["serverMs"] == 900.0
                && line["joinedBy"] == "correlationId")
    );
    assert!(
        client_events
            .iter()
            .any(|line| line["kind"] == "client.batch")
    );
    // Each finding cites the event that explains it.
    let event_id = |kind: &str| {
        client_events
            .iter()
            .find(|line| line["kind"] == kind)
            .map(|line| line["eventId"].clone())
            .unwrap()
    };
    let cited = |kind: &str| {
        lines
            .iter()
            .rfind(|line| line["kind"] == kind && line["source"] == "client")
            .map(|finding| finding["evidence"]["eventIds"].clone())
            .unwrap_or_else(|| panic!("no persisted {kind} finding"))
    };
    assert!(event_id("client.request").is_string());
    assert_eq!(
        cited("rpc.slow"),
        serde_json::json!([event_id("client.request")])
    );
    assert_eq!(
        cited("client.stall"),
        serde_json::json!([event_id("client.batch")])
    );
}

/// This process's entry, with the fields every entry carries.
fn server_process(sample: &serde_json::Value) -> &serde_json::Value {
    let server = sample["procs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|process| process["pid"] == std::process::id())
        .expect("the server process is sampled");
    assert_eq!(server["kind"], "server");
    assert!(server["privateBytes"].as_u64() > Some(0));
    assert!(server["rssBytes"].as_u64() > Some(0));
    if cfg!(target_os = "linux") {
        assert_eq!(server["metric"], "private");
    }
    server
}

/// Starts a runtime as production does, off the async workers, so a predecessor's teardown
/// can finish on the runtime while `start` waits for the directory lock.
async fn start_off_the_workers(
    dir: &std::path::Path,
    rx: watch::Receiver<bool>,
) -> ProfilerRuntime {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || ProfilerRuntime::start(&dir, rx, vec![]))
        .await
        .unwrap()
}

/// The new runtime holds the global handle, so global findings reach it.
async fn assert_takes_over(runtime: ProfilerRuntime) {
    assert_eq!(runtime.status()["capabilities"]["storage"], "ok");
    tokio::time::sleep(Duration::from_millis(50)).await;
    profiler::record_finding(crash_finding());
    assert_eq!(
        runtime.status()["findings"][0]["kind"],
        "crash.unclean-exit",
        "the previous runtime no longer holds the global handle"
    );
    runtime.shutdown().await;
}
