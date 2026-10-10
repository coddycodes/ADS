//! The profiler handle and the panic hook are process-global, so the scenarios run one after
//! another inside a single `#[tokio::test]`, each runtime shut down before the next starts.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use bibcode_server::diagnostics::profiler::{profiler_dir, runtime::ProfilerRuntime, sentinel};
use serde_json::{Value, json};
use tokio::sync::watch;

#[tokio::test]
async fn sentinel_and_panic_scenarios_share_the_process_global_handle_in_sequence() {
    a_run_that_never_shut_down_is_classified_with_the_profiler_off().await;
    a_run_on_another_boot_ended_with_the_system().await;
    a_dump_of_the_previous_run_is_its_native_crash().await;
    a_dump_this_run_survived_is_one_finding_across_scans_and_restarts().await;
    a_panic_is_recorded_with_module_offsets_and_keeps_unwinding().await;
    only_the_directory_owner_marks_its_session_clean().await;
}

/// Review Focus 5: the first start after a crash, with the toggle off.
async fn a_run_that_never_shut_down_is_classified_with_the_profiler_off() {
    let state = tempfile::tempdir().unwrap();
    // The current boot and a pid no OS record matches: no OS record, no OOM baseline.
    write_running_session(state.path(), sentinel::boot_id());
    // Another process's dump, written during the run: not this run's crash.
    write_sidecar(state.path(), "other-process", 4_000_000_001, 30);
    let (tx, rx) = watch::channel(false);
    let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);

    let finding = wait_for_finding(&runtime, "crash.unclean-exit").await;
    assert_eq!(finding["state"], "open");
    assert_eq!(runtime.status()["enabled"], false);
    tx.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let status = runtime.status();
    assert_eq!(status["enabled"], true);
    assert!(
        kinds(&status).contains(&"crash.unclean-exit".to_owned()),
        "the finding is there once the user turns the profiler on"
    );
    assert!(status["lastCrashAt"].is_string());

    let session = read_session(state.path());
    assert_eq!(session["state"], "running");
    assert_eq!(session["pid"], std::process::id());
    runtime.end_session().await;
    assert_eq!(read_session(state.path())["state"], "clean");
    let findings = std::fs::read_to_string(dir(state.path()).join("findings.ndjson")).unwrap();
    assert!(
        findings.contains("crash.unclean-exit"),
        "the finding was persisted"
    );

    // An orderly shutdown leaves nothing to classify on the next start.
    let (_tx, rx) = watch::channel(false);
    let next = ProfilerRuntime::start(state.path(), rx, vec![]);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(kinds(&next.status()), vec!["crash.unclean-exit".to_owned()]);
    assert_eq!(next.status()["findings"][0]["count"], 1);
    next.shutdown().await;
}

/// The crash monitor's dump of the previous run, found on the next start, is a `crash.native`
/// finding that names the dump.
async fn a_dump_of_the_previous_run_is_its_native_crash() {
    let state = tempfile::tempdir().unwrap();
    write_running_session(state.path(), sentinel::boot_id());
    write_sidecar(state.path(), "previous-run-dump", 4_000_000_000, 30);
    let (_tx, rx) = watch::channel(false);
    let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);
    let finding = wait_for_finding(&runtime, "crash.native").await;
    assert_eq!(finding["severity"], "error");
    assert_eq!(kinds(&runtime.status()), vec!["crash.native".to_owned()]);
    runtime.shutdown().await;
    let native = persisted(state.path(), "crash.native");
    assert_eq!(native["evidence"]["crashId"], "previous-run-dump");
    assert_eq!(native["evidence"]["source"], "minidump");
    assert_eq!(native["evidence"]["pid"], 4_000_000_000_u32);
}

/// A dump this process survived is found while it runs, once: rescans and restarts find the
/// same dump linked already.
async fn a_dump_this_run_survived_is_one_finding_across_scans_and_restarts() {
    let state = tempfile::tempdir().unwrap();
    let (tx, rx) = watch::channel(false);
    let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);
    write_sidecar(state.path(), "survived", std::process::id(), 0);
    // Turning the profiler on scans at once.
    tx.send(true).unwrap();
    let finding = wait_for_finding(&runtime, "crash.native").await;
    assert_eq!(finding["count"], 1);
    for on in [false, true] {
        tx.send(on).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let natives = |status: &Value| -> Vec<Value> {
        status["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|finding| finding["kind"] == "crash.native")
            .cloned()
            .collect()
    };
    let rescanned = natives(&runtime.status());
    assert_eq!(rescanned.len(), 1);
    assert_eq!(rescanned[0]["count"], 1, "a rescan links nothing again");
    runtime.end_session().await;
    assert_eq!(
        persisted(state.path(), "crash.native")["evidence"]["crashId"],
        "survived"
    );

    let (_tx, rx) = watch::channel(true);
    let next = ProfilerRuntime::start(state.path(), rx, vec![]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let restarted = natives(&next.status());
    assert_eq!(restarted.len(), 1);
    assert_eq!(restarted[0]["count"], 1, "a restart links nothing again");
    next.shutdown().await;
}

async fn a_run_on_another_boot_ended_with_the_system() {
    let state = tempfile::tempdir().unwrap();
    write_running_session(state.path(), Some("another-boot".into()));
    let (_tx, rx) = watch::channel(false);
    let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);
    wait_for_finding(&runtime, "crash.system").await;
    runtime.shutdown().await;
}

async fn a_panic_is_recorded_with_module_offsets_and_keeps_unwinding() {
    let state = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(false);
    let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);
    let home = dirs::home_dir().unwrap().to_string_lossy().into_owned();

    let result = std::panic::catch_unwind(|| panic!("boom {home}/secret"));
    assert!(result.is_err(), "the hook let the panic unwind");
    // On disk before the hook returned, so a panic that aborts the process is kept too.
    let panic = events(state.path())
        .into_iter()
        .rev()
        .find(|line| line["kind"] == "panic")
        .expect("a panic event, recorded with the profiler off");
    let message = panic["message"].as_str().unwrap();
    assert!(message.contains("boom ~/secret"), "{message}");
    assert!(!message.contains(&home));
    assert!(panic["thread"].is_string());
    assert!(
        panic["location"]
            .as_str()
            .unwrap()
            .contains("profiler_sentinel.rs")
    );
    // The build that made the frames, whose release symbols name them.
    assert_eq!(panic["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(panic["os"], std::env::consts::OS);
    assert_eq!(panic["arch"], std::env::consts::ARCH);
    let frames = panic["frames"].as_array().unwrap();
    assert!(!frames.is_empty());
    for frame in frames {
        assert!(frame["module"].is_string(), "{frame}");
        assert!(frame["offset"].is_u64(), "{frame}");
    }
    let this_test = std::env::current_exe().unwrap();
    let this_test = this_test.file_name().unwrap().to_string_lossy();
    // Windows names a module after its PDB, as its symbol files are.
    let is_this_test =
        |module: &str| module == this_test || (cfg!(windows) && module.ends_with(".pdb"));
    assert!(
        frames.iter().any(|frame| {
            frame["module"].as_str().is_some_and(is_this_test) && frame["debugId"].is_string()
        }),
        "frames in this binary carry its debug ID: {frames:?}"
    );

    // Every panic also raises a `crash.panic` finding, off the panicking thread. The same
    // panic again is a repeat of that finding, not a new one.
    let panic_here = || std::panic::catch_unwind(|| panic!("again"));
    assert!(panic_here().is_err());
    let finding = wait_for_finding(&runtime, "crash.panic").await;
    assert_eq!(finding["severity"], "error");
    assert!(
        finding["title"]
            .as_str()
            .unwrap()
            .starts_with("Panic at apps/server/tests/profiler_sentinel.rs:"),
        "{finding}"
    );
    assert!(panic_here().is_err());
    for _ in 0..200 {
        if runtime.status()["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["kind"] == "crash.panic" && finding["count"] == 2)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = runtime.status();
    let panics: Vec<&Value> = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["kind"] == "crash.panic")
        .collect();
    assert_eq!(panics.len(), 2, "one per site: the first panic and `again`");
    assert!(
        panics.iter().any(|finding| finding["count"] == 2),
        "the repeat counted: {panics:?}"
    );
    runtime.shutdown().await;
    // The finding cites the panic event it was raised from.
    let panic_ids: Vec<Value> = events(state.path())
        .into_iter()
        .filter(|line| line["kind"] == "panic")
        .map(|line| line["eventId"].clone())
        .collect();
    let findings = std::fs::read_to_string(dir(state.path()).join("findings.ndjson")).unwrap();
    let cited: Vec<Value> = findings
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|finding| finding["kind"] == "crash.panic")
        .map(|finding| finding["evidence"]["eventIds"][0].clone())
        .collect();
    assert!(!cited.is_empty());
    assert!(
        cited
            .iter()
            .all(|id| id.is_string() && panic_ids.contains(id)),
        "{cited:?} not among {panic_ids:?}"
    );
}

/// Only an orderly end marks the session clean, and it does so before the directory is
/// released: whoever takes the directory next never reads a stale `running` record.
async fn only_the_directory_owner_marks_its_session_clean() {
    let state = tempfile::tempdir().unwrap();
    let (_tx, rx) = watch::channel(false);
    let first = ProfilerRuntime::start(state.path(), rx.clone(), vec![]);
    let observer = {
        let state = state.path().to_path_buf();
        std::thread::spawn(move || {
            let lock = std::fs::File::options()
                .write(true)
                .open(dir(&state).join(".lock"))
                .unwrap();
            while lock.try_lock().is_err() {
                std::thread::sleep(Duration::from_millis(1));
            }
            let seen = read_session(&state)["state"].clone();
            lock.unlock().unwrap();
            seen
        })
    };
    first.end_session().await;
    assert_eq!(
        observer.join().unwrap(),
        "clean",
        "seen as the lock came free"
    );

    let next = ProfilerRuntime::start(state.path(), rx.clone(), vec![]);
    let degraded = ProfilerRuntime::start(state.path(), rx, vec![]);
    degraded.end_session().await;
    assert_eq!(
        read_session(state.path())["state"],
        "running",
        "a degraded runtime does not touch the owner's session"
    );
    // A plain shutdown (a failed start, a cancelled quiesce) is not an orderly end.
    next.shutdown().await;
    assert_eq!(read_session(state.path())["state"], "running");
}

fn dir(state: &Path) -> PathBuf {
    profiler_dir(state)
}

fn write_running_session(state: &Path, boot_id: Option<String>) {
    let started_at = time::OffsetDateTime::now_utc() - time::Duration::minutes(1);
    let record = json!({
        "state": "running",
        "pid": 4_000_000_000_u32,
        "bootId": boot_id,
        "exe": "bibcode-sentinel-test",
        "version": "0.0.0",
        "startedAt": started_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        "oomKillBaseline": null,
    });
    std::fs::create_dir_all(dir(state)).unwrap();
    std::fs::write(dir(state).join("session.json"), record.to_string()).unwrap();
}

/// A crash monitor sidecar for a dump of `pid`, written `ago_secs` seconds ago.
fn write_sidecar(state: &Path, id: &str, pid: u32, ago_secs: i64) {
    let created_at = time::OffsetDateTime::now_utc() - time::Duration::seconds(ago_secs);
    let sidecar = json!({
        "id": id,
        "createdAt": created_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
        "version": "0.0.0",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "processKind": "server",
        "parentPid": pid,
        "bootId": sentinel::boot_id(),
        "lastEvents": [],
    });
    let crashes = dir(state).join("crashes");
    std::fs::create_dir_all(&crashes).unwrap();
    std::fs::write(crashes.join(format!("{id}.json")), sidecar.to_string()).unwrap();
}

/// The last line `findings.ndjson` holds for the finding of `kind`.
fn persisted(state: &Path, kind: &str) -> Value {
    std::fs::read_to_string(dir(state).join("findings.ndjson"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|finding| finding["kind"] == kind)
        .unwrap_or_else(|| panic!("no persisted {kind} finding"))
}

fn read_session(state: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir(state).join("session.json")).unwrap()).unwrap()
}

fn kinds(status: &Value) -> Vec<String> {
    status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The classification runs in the background, bounded by the 5 s harvest timeout.
async fn wait_for_finding(runtime: &ProfilerRuntime, kind: &str) -> Value {
    for _ in 0..200 {
        let status = runtime.status();
        if let Some(finding) = status["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|finding| finding["kind"] == kind)
        {
            return finding.clone();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no {kind} finding: {}", runtime.status());
}

fn events(state: &Path) -> Vec<Value> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir(state))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("events-"))
        })
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<Value>>()
        })
        .collect()
}
