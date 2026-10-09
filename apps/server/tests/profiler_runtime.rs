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

use bibcode_server::diagnostics::profiler::{
    self,
    findings::{FindingInput, Severity},
    runtime::{Detector, ProfilerRuntime},
};
use futures_util::FutureExt;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Counts live instances; each one takes `.1` to wind down once cancelled.
struct CountingDetector(Arc<AtomicI64>, Duration);

impl Detector for CountingDetector {
    fn name(&self) -> &'static str {
        "counting"
    }

    fn start(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        let (live, wind_down) = (self.0.clone(), self.1);
        live.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            cancel.cancelled().await;
            tokio::time::sleep(wind_down).await;
            live.fetch_sub(1, Ordering::SeqCst);
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
    status_reports_findings_and_disk_use().await;
    a_second_runtime_on_the_same_directory_is_degraded().await;
    a_dropped_runtime_stops_its_detectors_before_releasing_the_directory().await;
    a_repeated_shutdown_waits_for_a_cancelled_one().await;
    an_unreadable_findings_file_reports_failing_storage().await;
    a_failed_startup_compaction_reports_failing_storage().await;
}

async fn rapid_toggles_never_run_two_detector_sets() {
    let dir = tempfile::tempdir().unwrap();
    let live = Arc::new(AtomicI64::new(0));
    let (tx, rx) = watch::channel(false);
    let runtime = ProfilerRuntime::start(
        dir.path(),
        rx,
        vec![Box::new(CountingDetector(live.clone(), Duration::ZERO))],
    );
    for on in [true, false, true, false, true] {
        tx.send(on).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(live.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.status()["enabled"], true);
    assert!(runtime.status()["recordingSince"].is_string());
    tx.send(false).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(live.load(Ordering::SeqCst), 0);
    assert_eq!(runtime.status()["enabled"], false);
    assert!(runtime.status()["recordingSince"].is_null());
    runtime.shutdown().await;
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
    assert_eq!(degraded["capabilities"]["storage"], "failing");
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
        "failing",
        "the degraded runtime's shutdown must not release the first runtime's lock"
    );
    third.shutdown().await;
    first.shutdown().await;
}

/// A server start that fails after creating the runtime drops it without `shutdown`.
async fn a_dropped_runtime_stops_its_detectors_before_releasing_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let live = Arc::new(AtomicI64::new(0));
    let (_tx, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(
        dir.path(),
        rx.clone(),
        vec![Box::new(CountingDetector(
            live.clone(),
            Duration::from_millis(100),
        ))],
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(live.load(Ordering::SeqCst), 1);
    // Dropped on a thread outside the Tokio runtime, which must still run the teardown.
    std::thread::spawn(move || drop(runtime)).join().unwrap();
    let next = start_when_unlocked(dir.path(), &rx).await;
    assert_eq!(
        live.load(Ordering::SeqCst),
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

/// Starts a runtime once the previous one has released the directory lock.
async fn start_when_unlocked(dir: &std::path::Path, rx: &watch::Receiver<bool>) -> ProfilerRuntime {
    for _ in 0..100 {
        let runtime = ProfilerRuntime::start(dir, rx.clone(), vec![]);
        if runtime.status()["capabilities"]["storage"] == "ok" {
            return runtime;
        }
        runtime.shutdown().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the previous runtime never released the profiler directory");
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
