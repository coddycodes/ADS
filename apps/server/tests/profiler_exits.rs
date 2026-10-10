//! Subprocess and webview deaths recorded through the profiler's process-global handle. The
//! handle is shared, so the scenarios run in sequence on one runtime.

use std::{path::Path, process::ExitStatus, time::Duration};

use bibcode_server::diagnostics::profiler::{
    exits::{self, Exited, Watch},
    findings::fingerprint,
    profiler_dir,
    runtime::ProfilerRuntime,
};
use serde_json::Value;
use tokio::sync::watch;

#[cfg(unix)]
const CRASH: &str = "SIGSEGV";
#[cfg(windows)]
const CRASH: &str = "0xC0000005";

#[cfg(unix)]
fn crashed() -> ExitStatus {
    std::os::unix::process::ExitStatusExt::from_raw(11)
}

#[cfg(windows)]
fn crashed() -> ExitStatus {
    std::os::windows::process::ExitStatusExt::from_raw(0xC000_0005)
}

#[cfg(unix)]
fn exit_code(code: i32) -> ExitStatus {
    std::os::unix::process::ExitStatusExt::from_raw(code << 8)
}

#[cfg(windows)]
fn exit_code(code: u32) -> ExitStatus {
    std::os::windows::process::ExitStatusExt::from_raw(code)
}

#[tokio::test]
async fn unrequested_deaths_are_recorded_while_the_profiler_is_on() {
    let state = tempfile::tempdir().unwrap();
    let (toggle, setting) = watch::channel(true);
    let runtime = ProfilerRuntime::start(state.path(), setting, vec![]);
    eventually(|| runtime.status()["enabled"] == true).await;

    let git = Exited {
        kind: "command",
        label: "git",
        exe: "/usr/bin/git",
        pid: 4_000_000_001,
        uptime: Duration::from_secs(90),
    };
    exits::report_exit(Watch::Signals, &git, &crashed(), false);
    let crash_id = id("subprocess.crash", &["command", "git", CRASH]);
    let crash = finding(&runtime, &crash_id).expect("subprocess.crash finding");
    assert_eq!(crash["severity"], "error");
    assert!(crash["title"].as_str().unwrap().contains("git"));

    // A requested stop, a process the user stopped from the process list, and an exit code
    // the watch ignores: no occurrence.
    exits::report_exit(Watch::Signals, &git, &crashed(), true);
    exits::note_stop_requested(4_000_000_002);
    let stopped = Exited {
        pid: 4_000_000_002,
        ..git
    };
    exits::report_exit(Watch::Signals, &stopped, &crashed(), false);
    exits::report_exit(Watch::Signals, &git, &exit_code(3), false);
    assert_eq!(finding(&runtime, &crash_id).unwrap()["count"], 1);

    let codex = Exited {
        kind: "provider",
        label: "codex",
        ..git
    };
    exits::report_exit(Watch::Failures, &codex, &exit_code(3), false);
    let failed = finding(
        &runtime,
        &id("subprocess.crash", &["provider", "codex", "exit 3"]),
    )
    .expect("a provider that exits 3 by itself");
    assert_eq!(failed["severity"], "warn");

    exits::report_webview_terminated("Crashed", None);
    let webview = finding(&runtime, &id("crash.webview", &["Crashed"])).expect("crash.webview");
    assert_eq!(webview["severity"], "error");
    assert!(runtime.status()["lastCrashAt"].is_string());

    let failure_reports = tempfile::tempdir().unwrap();
    std::fs::create_dir(failure_reports.path().join("reports")).unwrap();
    std::fs::write(failure_reports.path().join("reports/f00d.dmp"), b"MDMP").unwrap();
    exits::report_webview_failed(
        "render-process-exited",
        "crashed",
        -1_073_741_819,
        Some(failure_reports.path().to_path_buf()),
    );
    let failed_id = id("crash.webview", &["render-process-exited: crashed"]);
    assert!(finding(&runtime, &failed_id).is_some());
    // The dump is looked up off the reporting thread and added to the evidence.
    eventually(|| {
        persisted(state.path(), &failed_id).is_some_and(|f| f["evidence"]["dump"] == "f00d.dmp")
    })
    .await;

    #[cfg(unix)]
    a_terminal_that_dies_of_a_signal_is_recorded_and_a_closed_one_is_not(&runtime).await;

    toggle.send(false).unwrap();
    eventually(|| runtime.status()["enabled"] == false).await;
    let gh = Exited { label: "gh", ..git };
    exits::report_exit(Watch::Signals, &gh, &crashed(), false);
    exits::report_webview_terminated("ExceededMemoryLimit", None);

    // Once BiBCode received a termination signal, what dies dies with it, though the profiler
    // is on. Process-global and final, so it comes last.
    toggle.send(true).unwrap();
    eventually(|| runtime.status()["enabled"] == true).await;
    exits::note_terminating();
    let node = Exited {
        kind: "provider",
        label: "claudeAgent",
        ..git
    };
    exits::report_exit(Watch::Failures, &node, &exit_code(143), false);
    exits::report_webview_terminated("Crashed after termination", None);
    let terminated_id = id("subprocess.crash", &["provider", "claudeAgent", "exit 143"]);
    assert!(finding(&runtime, &terminated_id).is_none());
    runtime.shutdown().await;

    let events = events(state.path());
    let exit_events: Vec<&Value> = events
        .iter()
        .filter(|event| event["kind"] == "subprocess.exit")
        .collect();
    assert!(
        exit_events
            .iter()
            .any(|event| event["processKind"] == "command"
                && event["label"] == "git"
                && event["pid"] == 4_000_000_001_u32
                && event["outcome"] == CRASH),
        "{exit_events:?}"
    );
    assert!(
        !exit_events
            .iter()
            .any(|event| event["label"] == "gh" || event["label"] == "claudeAgent"),
        "{exit_events:?}"
    );
    let crash = persisted(state.path(), &crash_id).unwrap();
    assert_eq!(crash["evidence"]["pid"], 4_000_000_001_u32);
    assert_eq!(crash["evidence"]["uptimeMs"], 90_000);
    assert_eq!(crash["evidence"]["likelyOom"], false);
    let failure = persisted(state.path(), &failed_id).unwrap();
    assert_eq!(failure["evidence"]["exitCode"], -1_073_741_819);
    assert_eq!(
        failure["count"], 1,
        "the dump is evidence, not an occurrence"
    );
    assert!(
        persisted(
            state.path(),
            &id("subprocess.crash", &["command", "gh", CRASH])
        )
        .is_none()
    );
    assert!(persisted(state.path(), &id("crash.webview", &["ExceededMemoryLimit"])).is_none());
    assert!(persisted(state.path(), &terminated_id).is_none());
    assert!(
        persisted(
            state.path(),
            &id("crash.webview", &["Crashed after termination"])
        )
        .is_none()
    );
}

#[cfg(unix)]
async fn a_terminal_that_dies_of_a_signal_is_recorded_and_a_closed_one_is_not(
    runtime: &ProfilerRuntime,
) {
    use bibcode_server::terminal::{
        PortablePtyBackend, TerminalLaunchCommand, TerminalManager, TerminalManagerOptions,
        TerminalOpenInput,
    };

    let root = tempfile::tempdir().unwrap();
    let manager = TerminalManager::new(
        std::sync::Arc::new(PortablePtyBackend),
        TerminalManagerOptions {
            subprocess_poll_interval: Duration::ZERO,
            ..TerminalManagerOptions::default()
        },
    );
    let shell = |terminal: &str, script: &str| {
        let mut input =
            TerminalOpenInput::new("thread", terminal, root.path().to_path_buf(), 80, 24);
        input.command = Some(TerminalLaunchCommand {
            executable: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            label: None,
            activity: None,
            env: None,
        });
        input
    };
    manager
        .open(shell("crashes", "kill -USR1 $$"))
        .await
        .expect("terminal starts");
    let crash_id = id("subprocess.crash", &["terminal", "shell", "SIGUSR1"]);
    eventually(|| finding(runtime, &crash_id).is_some()).await;

    manager
        .open(shell("closed", "sleep 30"))
        .await
        .expect("terminal starts");
    manager
        .close("thread", Some("closed"))
        .await
        .expect("terminal closes");
    manager.shutdown().await;
    let terminal_crashes: Vec<Value> = runtime.status()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["kind"] == "subprocess.crash")
        .filter(|finding| finding["title"].as_str().unwrap().contains("shell"))
        .cloned()
        .collect();
    assert_eq!(terminal_crashes.len(), 1, "{terminal_crashes:?}");
}

fn id(kind: &str, fields: &[&str]) -> String {
    let fields: Vec<String> = fields.iter().map(|field| (*field).to_owned()).collect();
    format!("{kind}-{}", fingerprint(kind, &fields))
}

/// The finding as the status RPC lists it.
fn finding(runtime: &ProfilerRuntime, id: &str) -> Option<Value> {
    runtime.status()["findings"]
        .as_array()?
        .iter()
        .find(|finding| finding["id"] == id)
        .cloned()
}

/// The finding's last line in `findings.ndjson`, evidence included.
fn persisted(state: &Path, id: &str) -> Option<Value> {
    std::fs::read_to_string(profiler_dir(state).join("findings.ndjson"))
        .ok()?
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|finding| finding["id"] == id)
}

fn events(state: &Path) -> Vec<Value> {
    let mut events = Vec::new();
    for entry in std::fs::read_dir(profiler_dir(state)).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("events-"))
        {
            events.extend(
                std::fs::read_to_string(path)
                    .unwrap()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok()),
            );
        }
    }
    events
}

async fn eventually(condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("condition not reached within 10 s");
}
