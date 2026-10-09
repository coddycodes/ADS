//! `bibcode profiler` reads the profiler directory with no server running.

use std::{fs, path::Path, process::Command};

use serde_json::Value;
use tempfile::TempDir;

const FINDING: &str = r#"{"id":"rpc.slow-aaaaaaaaaaaa","fingerprint":"aaaaaaaaaaaa","kind":"rpc.slow","severity":"warn","firstSeen":"2020-01-01T10:00:00Z","lastSeen":"2020-01-01T10:05:00Z","count":3,"source":"server","title":"Slow request","summary":"s","state":"open","evidence":{"ms":1200}}"#;
const EVENTS: &str = concat!(
    r#"{"v":1,"seq":0,"sid":"aaaa1111","ts":"2020-01-01T10:00:00Z","kind":"rpc.slow","source":"server","ms":1200}"#,
    "\n",
    r#"{"v":1,"seq":1,"sid":"aaaa1111","ts":"2020-01-01T10:00:01Z","kind":"process.sample","source":"server"}"#,
    "\n",
    r#"{"v":1,"seq":2,"sid":"aaaa1111","ts":"2020-01-01T10:0"#,
);

/// A data root whose `userdata/profiler` holds one finding and a few events, and nothing else.
fn seeded_root() -> TempDir {
    let root = TempDir::new().expect("temporary data root");
    let profiler = root.path().join("userdata/profiler");
    fs::create_dir_all(&profiler).expect("profiler directory");
    fs::write(profiler.join("findings.ndjson"), format!("{FINDING}\n")).expect("findings");
    fs::write(profiler.join("events-000000.ndjson"), EVENTS).expect("events");
    root
}

fn bibcode(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bibcode"))
        .arg("profiler")
        .args(args)
        .arg("--base-dir")
        .arg(root)
        // A developer shell must not move the state directory to `dev/`.
        .env_remove("VITE_DEV_SERVER_URL")
        .env_remove("BIBCODE_HOME")
        .output()
        .expect("run bibcode profiler")
}

fn json_stdout(output: &std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "bibcode profiler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("one JSON document on stdout")
}

#[test]
fn status_reads_the_directory_while_no_server_runs() {
    let root = seeded_root();
    let before = fs::read_dir(root.path().join("userdata/profiler"))
        .expect("profiler directory")
        .count();

    let status = json_stdout(&bibcode(root.path(), &["status", "--json"]));

    assert_eq!(status["enabled"], false);
    assert_eq!(status["findings"][0]["kind"], "rpc.slow");
    assert_eq!(status["eventCount"], 2);
    assert_eq!(status["capabilities"]["storage"], "ok");
    // Reading creates nothing, not even a lock file.
    let after = fs::read_dir(root.path().join("userdata/profiler"))
        .expect("profiler directory")
        .count();
    assert_eq!(after, before);
}

#[test]
fn findings_events_and_crashes_print_json_arrays() {
    let root = seeded_root();

    let findings = json_stdout(&bibcode(
        root.path(),
        &["findings", "--json", "--since", "1h"],
    ));
    assert_eq!(findings.as_array().map(Vec::len), Some(0), "{findings}");
    let findings = json_stdout(&bibcode(
        root.path(),
        &[
            "findings",
            "--json",
            "--since",
            "2020-01-01T10:00:00Z",
            "--kind",
            "rpc.*",
        ],
    ));
    assert_eq!(findings[0]["evidence"]["ms"], 1200);
    // The torn third event line is skipped.
    let events = json_stdout(&bibcode(root.path(), &["events", "--json", "--limit", "5"]));
    assert_eq!(events.as_array().map(Vec::len), Some(2));
    assert_eq!(events[1]["kind"], "process.sample");
    let crashes = json_stdout(&bibcode(root.path(), &["crashes", "--json"]));
    assert_eq!(crashes, Value::Array(Vec::new()));
}

#[test]
fn misuse_fails_with_a_hint_and_unshipped_commands_stay_hidden() {
    let root = seeded_root();

    for (args, hint) in [
        (&["events", "--follow"][..], "--duration"),
        (
            &["events", "--since", "yesterday"][..],
            "30s, 10m, 1h or 2d",
        ),
        (
            &["events", "--follow", "--duration", "11m"][..],
            "at most 10m",
        ),
        (&["mcp"][..], "not available yet"),
    ] {
        let output = bibcode(root.path(), args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(hint), "{args:?}: {stderr}");
    }
    let help = bibcode(root.path(), &["--help"]);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("findings") && !help.contains("symbolicate") && !help.contains("mcp"));
}
