//! `bibcode profiler` reads the profiler directory with no server running.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};
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

/// Text in the files comes from monitored processes, and serde_json leaves DEL, CSI and OSC raw
/// there. Human output replaces them and the bidi controls that reorder text; `--json` writes
/// DEL and the C1 controls as `\u` escapes, and a parser reads back the same text.
#[test]
fn output_carries_nothing_that_drives_or_misleads_a_terminal() {
    // DEL, CSI, OSC, then a right-to-left override and a left-to-right isolate.
    const RAW: &str = "x\u{7f}\u{9b}\u{9d}\u{202e}y\u{2066}z";
    let root = seeded_root();
    let profiler = root.path().join("userdata/profiler");
    let event = json!({
        "v": 1, "seq": 0, "sid": "aaaa1111", "ts": "2020-01-01T10:00:00Z",
        "kind": "rpc.slow", "source": "server", "method": RAW,
    });
    let mut finding: Value = serde_json::from_str(FINDING).expect("seed finding");
    finding["title"] = RAW.into();
    fs::write(profiler.join("events-000000.ndjson"), format!("{event}\n")).expect("events");
    fs::write(profiler.join("findings.ndjson"), format!("{finding}\n")).expect("findings");

    let human = bibcode(root.path(), &["findings"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("UTF-8");
    assert!(human.ends_with(" x3 x????y?z\n"), "{human:?}");
    for (args, field) in [
        (&["events", "--json"][..], "method"),
        (&["findings", "--json"][..], "title"),
    ] {
        let output = bibcode(root.path(), args);
        let records = json_stdout(&output);
        // DEL is the byte 0x7F; a C1 control is the UTF-8 pair 0xC2 0x80 to 0xC2 0x9F.
        assert!(!output.stdout.contains(&0x7f), "{args:?}: raw DEL");
        assert!(
            !output
                .stdout
                .windows(2)
                .any(|pair| pair[0] == 0xc2 && (0x80..=0x9f).contains(&pair[1])),
            "{args:?}: raw C1 control"
        );
        // The data is untouched: only its spelling in the JSON text changed.
        assert_eq!(records[0][field], RAW, "{args:?}");
    }
}

#[test]
fn misuse_fails_with_a_hint_and_help_lists_every_command() {
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
        (&["symbolicate", "aaaa1111:x"][..], "<sid>:<seq>"),
        (&["symbolicate", "no-such-crash"][..], "no crash record"),
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
    assert!(help.contains("findings") && help.contains("symbolicate") && help.contains("mcp"));
}

/// The `.sym` of a module `bibcode` whose `my_fn` covers 0x1000 to 0x1020, all line 42.
const SYM: &str =
    "MODULE linux x86_64 ABC123 bibcode\nFILE 0 src/lib.rs\nFUNC 1000 20 0 my_fn\n1000 20 42 0\n";

/// A data root whose events hold one panic in `my_fn`, and a symbol store beside it.
fn panic_root() -> TempDir {
    let root = seeded_root();
    let panic = json!({
        "v": 1, "seq": 3, "sid": "aaaa1111", "ts": "2020-01-01T10:00:02Z", "kind": "panic",
        "source": "server", "thread": "main", "message": "boom", "location": "src/lib.rs:42",
        "frames": [
            { "module": "bibcode", "debugId": "ABC123", "offset": 0x1010 },
            { "module": "libc.so.6", "debugId": null, "offset": 0x29d90 },
        ],
    });
    fs::write(
        root.path().join("userdata/profiler/events-000001.ndjson"),
        format!("{panic}\n"),
    )
    .expect("panic event");
    let store = root.path().join("symbols/bibcode/ABC123");
    fs::create_dir_all(&store).expect("symbol store");
    fs::write(store.join("bibcode.sym"), SYM).expect("sym file");
    root
}

#[test]
fn symbolicate_names_a_panic_from_a_symbol_store() {
    let root = panic_root();
    let store = root.path().join("symbols");
    let store = store.to_str().expect("UTF-8 path");

    let named = json_stdout(&bibcode(
        root.path(),
        &["symbolicate", "aaaa1111:3", "--symbols", store, "--json"],
    ));
    assert_eq!(
        named["frames"][0],
        json!({
            "module": "bibcode", "offset": 0x1010, "function": "my_fn",
            "file": "src/lib.rs", "line": 42,
        })
    );
    assert!(named["frames"][1]["function"].is_null());

    let human = bibcode(
        root.path(),
        &["symbolicate", "aaaa1111:3", "--symbols", store],
    );
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).expect("UTF-8");
    assert_eq!(
        human,
        "Panic on thread main: boom at src/lib.rs:42\n  0 bibcode!my_fn [src/lib.rs:42]\n  1 libc.so.6+0x29d90\n"
    );
    // Without symbols nothing is named, nothing is downloaded and nothing is written.
    let unnamed = bibcode(root.path(), &["symbolicate", "aaaa1111:3"]);
    assert!(unnamed.status.success());
    assert!(String::from_utf8_lossy(&unnamed.stdout).contains("No symbols named these frames"));
    assert!(!root.path().join("userdata/profiler-symbols").exists());
}

/// What an MCP client sees: one JSON-RPC response per request line on stdout, nothing else,
/// and a clean exit when its input closes.
#[test]
fn mcp_answers_over_stdio_until_its_input_closes() {
    let root = panic_root();
    let mut server = Command::new(env!("CARGO_BIN_EXE_bibcode"))
        .args(["profiler", "mcp", "--base-dir"])
        .arg(root.path())
        .env_remove("VITE_DEV_SERVER_URL")
        .env_remove("BIBCODE_HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start bibcode profiler mcp");
    let requests = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "profiler_findings", "arguments": { "limit": 1 } } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "profiler_symbolicate", "arguments": { "panicSeq": "aaaa1111:3", "symbolsDir": root.path().join("symbols") } } }),
    ];
    {
        let mut input = server.stdin.take().expect("stdin");
        for request in &requests {
            writeln!(input, "{request}").expect("write a request");
        }
    }

    let output = server.wait_with_output().expect("bibcode profiler mcp");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .expect("UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line is one JSON-RPC message"))
        .collect();
    let ids: Vec<&Value> = responses.iter().map(|response| &response["id"]).collect();
    assert_eq!(ids, [&json!(1), &json!(2), &json!(3), &json!(4)]);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "bibcode-profiler"
    );
    assert_eq!(
        responses[1]["result"]["tools"].as_array().map(Vec::len),
        Some(5)
    );
    let findings = &responses[2]["result"]["structuredContent"]["findings"];
    assert_eq!(findings.as_array().map(Vec::len), Some(1));
    assert_eq!(findings[0]["kind"], "rpc.slow");
    assert_eq!(
        responses[3]["result"]["structuredContent"]["frames"][0]["function"],
        "my_fn"
    );
}
