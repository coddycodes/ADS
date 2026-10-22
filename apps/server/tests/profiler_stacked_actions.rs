//! Git stacked actions through `git.runStackedAction` reach the profiler (#158): each
//! terminal outcome is a `git.stacked-action` event, and a step that keeps failing in one
//! worktree raises `git.stacked-action-failed`. The profiler handle is process-global, so the
//! scenario runs on one runtime.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;
#[path = "support/websocket_frames.rs"]
mod websocket_frames;

use std::{fs, path::Path, process::Command, sync::Arc, time::Duration};

use bibcode_server::{
    RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    diagnostics::profiler::{findings::fingerprint, profiler_dir, runtime::ProfilerRuntime},
    production::git_vcs::{GitVcsRpcServices, register_git_vcs_rpc},
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{sync::watch, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use websocket_frames::next_frame_past_heartbeat;

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const RPC_DEADLINE: Duration = Duration::from_secs(60);
const COMMIT_MESSAGE: &str = "secret-commit-subject-158";

#[tokio::test]
async fn stacked_action_outcomes_reach_the_profiler() {
    let state = TempDir::new().expect("profiler state");
    let (_enabled, setting) = watch::channel(true);
    let runtime = ProfilerRuntime::start(state.path(), setting, vec![]);
    timeout(RPC_DEADLINE, async {
        while runtime.status()["enabled"] != true {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the profiler turns on");

    let server_dir = TempDir::new().expect("server directory");
    let repository = TempDir::new().expect("repository");
    init_repository(repository.path());
    let cwd = repository.path().to_string_lossy().into_owned();

    let mut registry = RpcRegistry::empty();
    let hosts = Arc::new(bibcode_server::source_control::ProviderHosts::default());
    let git =
        Arc::new(bibcode_server::git::GitRepository::default().with_provider_hosts(hosts.clone()));
    register_git_vcs_rpc(
        &mut registry,
        GitVcsRpcServices::with_repository(git, hosts)
            .with_hosting_executable_dir_for_integration_test(
                hermetic_providers::missing_hosting_executable_dir(server_dir.path()),
            ),
    );
    let config = ServerConfig::new(server_dir.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    let handle = ServerRuntime::start_with_registry(config, registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    // A commit that succeeds: an event, no finding. Its message must never be recorded.
    fs::write(repository.path().join("file.txt"), "change\n").expect("write change");
    let committed = run_action(
        &mut socket,
        "1",
        json!({ "actionId": "a1", "cwd": cwd, "action": "commit", "commitMessage": COMMIT_MESSAGE }),
    )
    .await;
    assert_eq!(committed["kind"], "action_finished", "{committed}");

    // A request BiBCode refuses before any Git effect: an event, never a finding.
    let rejected = run_action(
        &mut socket,
        "2",
        json!({ "actionId": "a2", "cwd": cwd, "action": "create_pr" }),
    )
    .await;
    assert_eq!(rejected["kind"], "action_failed", "{rejected}");

    // Push with no origin fails in Git. Twice is not yet a pattern.
    for id in ["3", "4"] {
        let failed = run_action(
            &mut socket,
            id,
            json!({ "actionId": format!("push-{id}"), "cwd": cwd, "action": "push" }),
        )
        .await;
        assert_eq!(failed["kind"], "action_failed", "{failed}");
    }
    let finding_id = id(
        "git.stacked-action-failed",
        &[
            "push",
            "push",
            "git-failed",
            "GitVcsDriver.pushCurrentBranch",
        ],
    );
    assert!(finding(&runtime, &finding_id).is_none());

    // The third failure of the same step in the same worktree raises it; a fourth repeats it.
    for id in ["5", "6"] {
        run_action(
            &mut socket,
            id,
            json!({ "actionId": format!("push-{id}"), "cwd": cwd, "action": "push" }),
        )
        .await;
    }
    let raised = finding(&runtime, &finding_id).expect("git.stacked-action-failed finding");
    assert_eq!(raised["severity"], "warn");
    assert_eq!(raised["count"], 2);
    assert_eq!(
        finding_kinds(&runtime)
            .iter()
            .filter(|kind| kind.starts_with("git."))
            .count(),
        1,
        "the refused request raises nothing"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    runtime.shutdown().await;

    let events = events(state.path());
    let outcomes: Vec<&Value> = events
        .iter()
        .filter(|event| event["kind"] == "git.stacked-action")
        .collect();
    assert_eq!(outcomes.len(), 6, "{outcomes:?}");
    let payload = |event: &Value| {
        event
            .get("payload")
            .cloned()
            .unwrap_or_else(|| event.clone())
    };
    let committed = payload(outcomes[0]);
    assert_eq!(committed["action"], "commit");
    assert_eq!(committed["ok"], true);
    assert!(committed["phase"].is_null());
    let rejected = payload(outcomes[1]);
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["reason"], "rejected");
    let pushed = payload(outcomes[2]);
    assert_eq!(pushed["ok"], false);
    assert_eq!(pushed["phase"], "push");
    assert_eq!(pushed["reason"], "git-failed");
    assert_eq!(pushed["operation"], "GitVcsDriver.pushCurrentBranch");
    assert!(pushed["durationMs"].is_u64());

    let persisted = persisted(state.path(), &finding_id).expect("persisted finding");
    let evidence = &persisted["evidence"];
    assert_eq!(evidence["phase"], "push");
    assert!(evidence["repeatsInWindow"].as_u64().unwrap() >= 3);
    assert!(evidence["eventIds"].is_array(), "{evidence}");
    assert!(evidence["message"].is_string());
    assert!(evidence["worktree"].is_string());

    // Nothing the user wrote, and nothing Git printed, leaves the server.
    let recorded = fs::read_dir(profiler_dir(state.path()))
        .expect("profiler directory")
        .filter_map(|entry| fs::read_to_string(entry.expect("entry").path()).ok())
        .collect::<String>();
    assert!(!recorded.contains(COMMIT_MESSAGE));
    assert!(!recorded.contains("does not appear to be a git repository"));
}

fn init_repository(cwd: &Path) {
    let hooks = cwd.join(".hooks-empty");
    for args in [
        vec!["init", "--quiet", "-b", "main"],
        vec!["config", "user.name", "BiBCode Test"],
        vec!["config", "user.email", "bibcode@example.invalid"],
        vec!["config", "commit.gpgSign", "false"],
        vec!["config", "core.autocrlf", "false"],
        vec!["config", "core.hooksPath", &hooks.to_string_lossy()],
    ] {
        git(cwd, &args);
    }
    fs::create_dir(&hooks).expect("empty hooks directory");
    fs::write(cwd.join("file.txt"), "base\n").expect("write base");
    git(cwd, &["add", "file.txt"]);
    git(cwd, &["commit", "--quiet", "-m", "base"]);
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .expect("git starts");
    assert!(status.success(), "git {args:?} failed");
}

/// Runs one stacked action to its end and returns its terminal event.
async fn run_action(socket: &mut TestSocket, id: &str, payload: Value) -> Value {
    send(
        socket,
        json!({ "_tag": "Request", "id": id, "tag": "git.runStackedAction", "payload": payload, "headers": [] }),
    )
    .await;
    let mut terminal = Value::Null;
    loop {
        match next_message(socket).await {
            ServerMessage::Chunk { values, .. } => {
                if let Some(last) = values.into_iter().rfind(|value| {
                    value["kind"] == "action_finished" || value["kind"] == "action_failed"
                }) {
                    terminal = last;
                }
                send(socket, json!({ "_tag": "Ack", "requestId": id })).await;
            }
            ServerMessage::Exit { .. } => return terminal,
            _ => {}
        }
    }
}

async fn send(socket: &mut TestSocket, message: Value) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .expect("send WebSocket message");
}

async fn next_message(socket: &mut TestSocket) -> ServerMessage {
    let frame = timeout(RPC_DEADLINE, next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected a text WebSocket message, got {frame:?}");
    };
    serde_json::from_str(&text).expect("valid server RPC message")
}

fn id(kind: &str, fields: &[&str]) -> String {
    let fields: Vec<String> = fields.iter().map(|field| (*field).to_owned()).collect();
    format!("{kind}-{}", fingerprint(kind, &fields))
}

fn finding(runtime: &ProfilerRuntime, id: &str) -> Option<Value> {
    runtime.status()["findings"]
        .as_array()?
        .iter()
        .find(|finding| finding["id"] == id)
        .cloned()
}

fn finding_kinds(runtime: &ProfilerRuntime) -> Vec<String> {
    runtime.status()["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|finding| finding["kind"].as_str().map(str::to_owned))
        .collect()
}

/// The finding's last line in `findings.ndjson`, evidence included.
fn persisted(state: &Path, id: &str) -> Option<Value> {
    fs::read_to_string(profiler_dir(state).join("findings.ndjson"))
        .ok()?
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|finding| finding["id"] == id)
}

/// Every event line; shutdown flushed the writer.
fn events(state: &Path) -> Vec<Value> {
    fs::read_dir(profiler_dir(state))
        .expect("profiler directory")
        .filter_map(|entry| {
            let path = entry.expect("entry").path();
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("events-"))
                .then(|| fs::read_to_string(path).ok())
                .flatten()
        })
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .collect()
}
