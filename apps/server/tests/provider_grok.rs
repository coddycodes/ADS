use coddy_studio_server::provider::grok;

use std::{path::PathBuf, time::Duration};

use grok::{
    AcpConnectionConfig, AcpJsonRpcConnection, GrokSessionOptions, GrokSessionRuntime,
    build_snapshot_from_probe,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, duplex},
    sync::mpsc,
    time::timeout,
};

#[test]
fn grok_helper_outputs_match_fixtures() {
    let probe_fixture = fixture("probe-snapshot.json");
    assert_eq!(
        serde_json::to_value(build_snapshot_from_probe(
            probe_fixture["versionStdout"].as_str().expect("version"),
            probe_fixture["exitCode"].as_i64().expect("exit") as i32,
            &probe_fixture["modelState"],
            &probe_fixture["customModels"]
                .as_array()
                .expect("custom models")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ))
        .expect("snapshot json"),
        probe_fixture["expected"]
    );
}

#[tokio::test]
async fn grok_runtime_matches_user_input_and_cancel_traces() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = GrokSessionRuntime::new(
        GrokSessionOptions {
            thread_id: "grok-thread-1".to_owned(),
            cwd: "/tmp/project".to_owned(),
            mcp_servers: vec![json!({
                "type": "http",
                "name": "coddy-studio",
                "url": "http://127.0.0.1:3773/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer secret" }],
            })],
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "default".to_owned(),
        },
        connection.clone(),
        incoming,
    );

    peer.expect_request("initialize")
        .respond(json!({ "protocolVersion": 1 }));
    peer.expect_request("authenticate")
        .respond(json!({ "status": "ok" }));
    peer.expect_request("session/create")
        .expect_params(json!({
            "cwd": "/tmp/project",
            "mcpServers": [{
                "type": "http",
                "name": "coddy-studio",
                "url": "http://127.0.0.1:3773/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer secret" }],
            }],
        }))
        .respond(json!({
            "sessionId": "grok-session-1",
            "modes": {
                "currentModeId": "code",
                "availableModes": [
                    { "id": "code", "name": "Agent" },
                    { "id": "ask", "name": "Ask" }
                ]
            }
        }));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "grok-session-1", "modeId": "ask" }))
        .respond(json!({}));
    peer.expect_request("session/set_model")
        .respond(json!({ "modelId": "grok-build" }));
    peer.expect_request("session/prompt")
        .expect_params(json!({
            "sessionId": "grok-session-1",
            "prompt": [
                { "type": "text", "text": "ask before continuing" },
                { "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" }
            ]
        }))
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 2001,
            "method": "_x.ai/ask_user_question",
            "params": {
                "questions": [
                    {
                        "question": "Which scope should Grok use?",
                        "options": [
                            { "label": "Workspace", "description": "Use the current workspace" },
                            { "label": "Session", "description": "Only use this session" }
                        ]
                    }
                ]
            }
        }))
        .emit_notification(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hello from grok" }
                }
            }
        }))
        .expect_response_after_result(json!({
            "jsonrpc": "2.0",
            "id": 2001,
            "result": {
                "outcome": "accepted",
                "answers": {
                    "Which scope should Grok use?": ["Workspace"]
                }
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));
    peer.expect_request("session/prompt")
        .delay_before_emits(100)
        .emit_notification(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "late after cancel" }
                }
            }
        }))
        .respond(json!({ "stopReason": "cancelled" }));

    let peer_task = tokio::spawn(peer.run());
    runtime.start().await.expect("start");
    runtime.set_model("grok-build").await.expect("set model");

    let send_runtime = runtime.clone();
    let send_turn = tokio::spawn(async move {
        send_runtime
            .send_turn(
                Some("ask before continuing"),
                vec![json!({ "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" })],
            )
            .await
    });
    let first = runtime.next_event().await.expect("session started");
    assert_eq!(first.event_type, "session.started");
    let second = runtime.next_event().await.expect("thread started");
    assert_eq!(second.event_type, "thread.started");
    let first_turn_id = send_turn.await.expect("first join").expect("first turn");
    let mut requested = runtime.collect_events(4).await;
    let (assistant_chunk_index, assistant_chunk) = requested
        .iter()
        .enumerate()
        .find(|(_, event)| event.event_type == "content.delta")
        .expect("Grok assistant chunk");
    assert_eq!(assistant_chunk.item_id, None);
    assert_eq!(assistant_chunk.payload["delta"], json!("hello from grok"));
    assert!(requested[assistant_chunk_index + 1..].iter().any(|event| {
        event.event_type == "turn.completed" && event.turn_id == assistant_chunk.turn_id
    }));
    normalize_turn_ids(&mut requested, &first_turn_id, "turn-3");
    assert_eq!(
        requested,
        stable_fixture("trace-user-input-before-response.json")
    );

    runtime
        .respond_to_user_input(
            "user-input:2001",
            json!({ "Which scope should Grok use?": "Workspace" }),
        )
        .await
        .expect("respond");
    let mut resolved = runtime.collect_events(1).await;
    normalize_turn_ids(&mut resolved, &first_turn_id, "turn-3");
    assert_eq!(
        resolved,
        stable_fixture("trace-user-input-after-response.json")
    );

    let cancel_runtime = runtime.clone();
    let mut cancel_turn = tokio::spawn(async move {
        cancel_runtime
            .send_turn(Some("cancel before the late update"), vec![])
            .await
    });
    let cancel_started = runtime.next_event().await.expect("cancel started");
    let cancel_turn_id = cancel_started.turn_id.clone().expect("cancel turn id");
    let returned_cancel_turn_id = timeout(Duration::from_millis(30), &mut cancel_turn)
        .await
        .expect("send_turn should return before the delayed provider response")
        .expect("cancel join")
        .expect("cancel turn");
    assert_eq!(returned_cancel_turn_id, cancel_turn_id);
    tokio::time::sleep(Duration::from_millis(10)).await;
    runtime
        .interrupt_turn(&cancel_turn_id)
        .await
        .expect("interrupt");
    let mut cancelled = vec![cancel_started.stable_view()];
    cancelled.extend(runtime.collect_events(1).await);
    normalize_turn_ids(&mut cancelled, &cancel_turn_id, "turn-8");
    assert_eq!(cancelled, stable_fixture("trace-cancel.json"));

    peer_task.await.expect("peer");
    let failed_turn = runtime
        .send_turn(Some("provider is closed"), Vec::new())
        .await
        .expect("failed request still starts a tracked turn");
    let mut observed_failure = false;
    for _ in 0..3 {
        let event = timeout(Duration::from_secs(1), runtime.next_event())
            .await
            .expect("closed-provider event timeout")
            .expect("closed-provider event");
        if event.turn_id.as_deref() == Some(failed_turn.as_str())
            && event.payload["state"] == json!("failed")
        {
            observed_failure = true;
            break;
        }
    }
    assert!(observed_failure);
}

#[tokio::test]
async fn grok_runtime_covers_resume_modes_permissions_and_protocol_edges() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = GrokSessionRuntime::new_with_auth_and_resume(
        GrokSessionOptions {
            thread_id: "grok-thread-resumed".to_owned(),
            cwd: "/tmp/resumed-project".to_owned(),
            mcp_servers: Vec::new(),
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "default".to_owned(),
        },
        connection,
        incoming,
        "oauth".to_owned(),
        Some("grok-session-resumed".to_owned()),
    );

    assert!(
        runtime
            .set_model("before-start")
            .await
            .expect_err("model changes require a provider session")
            .to_string()
            .contains("has not started")
    );
    assert!(
        runtime
            .interrupt_turn("before-start")
            .await
            .expect_err("interrupts require a provider session")
            .to_string()
            .contains("has not started")
    );
    assert!(
        runtime
            .respond_to_request("missing", "accept")
            .await
            .expect_err("unknown approvals must be rejected")
            .to_string()
            .contains("Unknown pending request id missing")
    );
    assert!(
        runtime
            .respond_to_user_input("missing-input", json!({}))
            .await
            .expect_err("unknown prompts must be rejected")
            .to_string()
            .contains("Unknown pending request id missing-input")
    );

    peer.expect_request("initialize")
        .respond(json!({ "protocolVersion": 1 }));
    peer.expect_request("authenticate")
        .expect_params(json!({ "methodId": "oauth" }))
        .respond(json!({ "status": "ok" }));
    peer.expect_request("session/load")
        .expect_params(json!({
            "sessionId": "grok-session-resumed",
            "cwd": "/tmp/resumed-project",
        }))
        .respond(json!({
            "modes": {
                "currentModeId": "code",
                "availableModes": [
                    { "id": "architect", "name": "Plan" },
                    { "id": "ask", "name": "Ask" },
                    { "id": "code", "name": "Agent" }
                ]
            }
        }));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "grok-session-resumed", "modeId": "ask" }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "grok-session-resumed", "modeId": "code" }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({
            "sessionId": "grok-session-resumed",
            "modeId": "architect",
        }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "grok-session-resumed", "modeId": "code" }))
        .respond(json!({}));

    peer.expect_request("session/prompt")
        .emit_stderr("provider warning")
        .emit_notification(json!({
            "jsonrpc": "2.0",
            "method": "unrelated/notification",
            "params": {},
        }))
        .emit_notification(json!({
            "jsonrpc": "2.0",
            "method": "_x.ai/session/prompt_complete",
            "params": {},
        }))
        .emit_notification(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "plan",
                    "entries": [{ "content": "Run the tests", "status": "pending" }]
                }
            },
        }))
        .emit_request(permission_request(
            2101,
            "`cargo test`",
            permission_options(),
        ))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 2101,
            "result": {
                "outcome": { "outcome": "selected", "optionId": "always" }
            }
        }))
        .respond(json!({}));

    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "grok-session-resumed", "modeId": "ask" }))
        .respond(json!({}));
    for (wire_id, title, expected_option) in [
        (2102, "`persist command`", Some("always")),
        (2103, "`one command`", Some("once")),
        (2104, "`reject command`", Some("reject")),
        (2105, "`cancel command`", None),
    ] {
        let expected_outcome = expected_option.map_or_else(
            || json!({ "outcome": "cancelled" }),
            |option_id| json!({ "outcome": "selected", "optionId": option_id }),
        );
        peer.expect_request("session/prompt")
            .emit_request(permission_request(wire_id, title, permission_options()))
            .expect_response(json!({
                "jsonrpc": "2.0",
                "id": wire_id,
                "result": { "outcome": expected_outcome }
            }))
            .respond(json!({ "stopReason": "end_turn" }));
    }

    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 2201,
            "method": "x.ai/ask_user_question",
            "params": {
                "params": {
                    "questions": [
                        { "id": "targets", "question": "Which targets?", "options": [] },
                        {},
                    ]
                }
            },
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 2201,
            "result": {
                "outcome": "accepted",
                "answers": {
                    "targets": ["server", "desktop"],
                    "notes": ["run all"],
                    "ignored": []
                }
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));

    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 2301,
            "method": "unsupported/request",
            "params": {},
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 2301,
            "error": {
                "code": -32601,
                "message": "Method not found: unsupported/request",
                "data": null,
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));

    let peer_task = tokio::spawn(peer.run());
    assert_eq!(
        runtime.start().await.expect("resumed runtime should start"),
        "grok-session-resumed"
    );
    assert_eq!(
        runtime.collect_events(2).await,
        vec![
            grok::GrokRuntimeEventStableView {
                event_type: "session.started".to_owned(),
                thread_id: "grok-thread-resumed".to_owned(),
                turn_id: None,
                item_id: None,
                request_id: None,
                payload: json!({}),
            },
            grok::GrokRuntimeEventStableView {
                event_type: "thread.started".to_owned(),
                thread_id: "grok-thread-resumed".to_owned(),
                turn_id: None,
                item_id: None,
                request_id: None,
                payload: json!({}),
            },
        ]
    );

    runtime
        .set_runtime_mode("full-access")
        .await
        .expect("full access mode");
    runtime
        .set_interaction_mode("plan")
        .await
        .expect("plan interaction mode");
    runtime
        .set_interaction_mode("default")
        .await
        .expect("default interaction mode");

    let automatic_turn = runtime
        .send_turn(None, Vec::new())
        .await
        .expect("automatic approval turn");
    let automatic_events = collect_until_turn_completed(&runtime, &automatic_turn).await;
    assert!(
        automatic_events
            .iter()
            .any(|event| event.event_type == "runtime.warning"
                && event.payload["message"] == json!("provider warning"))
    );
    assert!(automatic_events.iter().any(|event| {
        event.event_type == "turn.plan.updated"
            && event.payload["entries"][0]["content"] == json!("Run the tests")
    }));
    assert!(
        automatic_events
            .iter()
            .all(|event| event.event_type != "request.opened")
    );

    runtime
        .set_runtime_mode("approval-required")
        .await
        .expect("approval mode");
    for (wire_id, decision) in [
        (2102, "acceptForSession"),
        (2103, "accept"),
        (2104, "decline"),
        (2105, "cancel"),
    ] {
        let turn_id = runtime
            .send_turn(Some("permission"), Vec::new())
            .await
            .expect("manual permission turn");
        let request_id = format!("approval:{wire_id}");
        let opened = next_event_matching(&runtime, |event| {
            event.event_type == "request.opened"
                && event.request_id.as_deref() == Some(request_id.as_str())
        })
        .await;
        assert_eq!(
            opened.payload["detail"],
            json!(match wire_id {
                2102 => "persist command",
                2103 => "one command",
                2104 => "reject command",
                _ => "cancel command",
            })
        );
        runtime
            .respond_to_request(&request_id, decision)
            .await
            .expect("permission response");
        let resolved = next_event_matching(&runtime, |event| {
            event.event_type == "request.resolved"
                && event.request_id.as_deref() == Some(request_id.as_str())
        })
        .await;
        assert_eq!(resolved.payload["decision"], json!(decision));
        next_event_matching(&runtime, |event| {
            event.event_type == "turn.completed"
                && event.turn_id.as_deref() == Some(turn_id.as_str())
        })
        .await;
    }

    let input_turn = runtime
        .send_turn(Some("ask"), Vec::new())
        .await
        .expect("user input turn");
    let input_request = next_event_matching(&runtime, |event| {
        event.event_type == "user-input.requested"
            && event.request_id.as_deref() == Some("user-input:2201")
    })
    .await;
    assert_eq!(input_request.payload["questions"][0]["id"], "targets");
    assert_eq!(input_request.payload["questions"][1]["id"], "");
    runtime
        .respond_to_user_input(
            "user-input:2201",
            json!({
                "targets": ["server", "desktop"],
                "notes": "run all",
                "ignored": 42,
            }),
        )
        .await
        .expect("user input response");
    next_event_matching(&runtime, |event| {
        event.event_type == "user-input.resolved"
            && event.request_id.as_deref() == Some("user-input:2201")
    })
    .await;
    next_event_matching(&runtime, |event| {
        event.event_type == "turn.completed"
            && event.turn_id.as_deref() == Some(input_turn.as_str())
    })
    .await;

    let unknown_turn = runtime
        .send_turn(Some("unknown request"), Vec::new())
        .await
        .expect("unknown request turn");
    next_event_matching(&runtime, |event| {
        event.event_type == "turn.completed"
            && event.turn_id.as_deref() == Some(unknown_turn.as_str())
    })
    .await;
    peer_task.await.expect("peer");
    let exited = next_event_matching(&runtime, |event| event.event_type == "session.exited").await;
    assert!(exited.payload["reason"].as_str().is_some());
}

#[tokio::test]
async fn grok_runtime_rejects_new_sessions_without_a_session_identifier() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = GrokSessionRuntime::new(
        GrokSessionOptions {
            thread_id: "grok-thread-invalid".to_owned(),
            cwd: "/tmp/invalid-project".to_owned(),
            mcp_servers: Vec::new(),
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "default".to_owned(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/create").respond(json!({}));
    let peer_task = tokio::spawn(peer.run());

    assert!(
        runtime
            .start()
            .await
            .expect_err("missing session identifiers must fail")
            .to_string()
            .contains("has not started")
    );
    peer_task.await.expect("peer");
}

fn permission_options() -> Vec<Value> {
    vec![
        json!({ "kind": "allow_once", "optionId": "once" }),
        json!({ "kind": "allow_always", "optionId": "always" }),
        json!({ "kind": "reject_once", "optionId": "reject" }),
    ]
}

fn permission_request(wire_id: u64, title: &str, options: Vec<Value>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": wire_id,
        "method": "session/request_permission",
        "params": {
            "options": options,
            "toolCall": { "title": title },
        },
    })
}

async fn collect_until_turn_completed(
    runtime: &GrokSessionRuntime,
    turn_id: &str,
) -> Vec<grok::GrokRuntimeEvent> {
    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(5), runtime.next_event())
            .await
            .expect("event timeout")
            .expect("runtime event");
        let completed =
            event.event_type == "turn.completed" && event.turn_id.as_deref() == Some(turn_id);
        events.push(event);
        if completed {
            return events;
        }
    }
}

async fn next_event_matching(
    runtime: &GrokSessionRuntime,
    predicate: impl Fn(&grok::GrokRuntimeEvent) -> bool,
) -> grok::GrokRuntimeEvent {
    loop {
        let event = timeout(Duration::from_secs(5), runtime.next_event())
            .await
            .expect("event timeout")
            .expect("runtime event");
        if predicate(&event) {
            return event;
        }
    }
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_directory().join(name)).expect("fixture file"),
    )
    .expect("valid fixture")
}

fn stable_fixture(name: &str) -> Vec<grok::GrokRuntimeEventStableView> {
    serde_json::from_value(fixture(name)).expect("stable fixture")
}

fn normalize_turn_ids(
    events: &mut [grok::GrokRuntimeEventStableView],
    actual_turn_id: &str,
    stable_turn_id: &str,
) {
    for event in events {
        if event.turn_id.as_deref() == Some(actual_turn_id) {
            event.turn_id = Some(stable_turn_id.to_owned());
        }
    }
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/grok-provider")
}

fn scripted_peer() -> (
    AcpJsonRpcConnection,
    mpsc::UnboundedReceiver<grok::IncomingEvent>,
    ScriptedPeer,
) {
    let (runtime_stdout, peer_stdout) = duplex(16 * 1024);
    let (peer_stdin, runtime_stdin) = duplex(16 * 1024);
    let (peer_stderr, runtime_stderr) = duplex(16 * 1024);
    let (connection, incoming) = AcpJsonRpcConnection::spawn(
        runtime_stdout,
        runtime_stdin,
        runtime_stderr,
        AcpConnectionConfig::default(),
    );
    (
        connection,
        incoming,
        ScriptedPeer::new(peer_stdin, peer_stdout, peer_stderr),
    )
}

struct ScriptedPeer {
    stdin_reader: tokio::io::DuplexStream,
    stdout_writer: tokio::io::DuplexStream,
    stderr: tokio::io::DuplexStream,
    steps: Vec<PeerStep>,
}

impl ScriptedPeer {
    fn new(
        stdin_reader: tokio::io::DuplexStream,
        stdout_writer: tokio::io::DuplexStream,
        stderr: tokio::io::DuplexStream,
    ) -> Self {
        Self {
            stdin_reader,
            stdout_writer,
            stderr,
            steps: Vec::new(),
        }
    }

    fn expect_request(&mut self, method: &str) -> &mut PeerStep {
        self.steps.push(PeerStep::ExpectRequest {
            method: method.to_owned(),
            expected_params: None,
            response: None,
            emits: Vec::new(),
            expected_follow_up: None,
            expected_follow_up_after_response: None,
            delay_before_emits_ms: None,
            stderr_messages: Vec::new(),
            emits_after_notification: Vec::new(),
            expected_notification: None,
        });
        self.steps.last_mut().expect("step")
    }

    async fn run(self) {
        let mut reader = BufReader::new(self.stdin_reader);
        let mut writer = self.stdout_writer;
        let mut stderr = self.stderr;
        for step in self.steps {
            let PeerStep::ExpectRequest {
                method,
                expected_params,
                response,
                emits,
                expected_follow_up,
                expected_follow_up_after_response,
                delay_before_emits_ms,
                stderr_messages,
                emits_after_notification,
                expected_notification,
            } = step;
            let message = read_json_message(&mut reader, &format!("request:{method}")).await;
            assert_eq!(message["method"], method);
            if let Some(expected_params) = expected_params {
                assert_eq!(message["params"], expected_params);
            }
            if let Some(delay_before_emits_ms) = delay_before_emits_ms {
                tokio::time::sleep(Duration::from_millis(delay_before_emits_ms)).await;
            }
            for message in stderr_messages {
                write_line(&mut stderr, &message).await;
            }
            for emit in emits {
                write_json(&mut writer, emit).await;
            }
            if let Some(expected_response) = expected_follow_up {
                let follow_up =
                    read_json_message(&mut reader, &format!("follow-up:{method}")).await;
                assert_eq!(follow_up, expected_response);
            }
            if let Some(expected_notification) = expected_notification {
                let notification = read_json_message(
                    &mut reader,
                    &format!("notification:{expected_notification}"),
                )
                .await;
                assert_eq!(notification["method"], expected_notification);
            }
            for emit in emits_after_notification {
                write_json(&mut writer, emit).await;
            }
            if let Some(result) = response {
                write_json(
                    &mut writer,
                    json!({ "jsonrpc": "2.0", "id": message["id"].clone(), "result": result }),
                )
                .await;
            }
            if let Some(expected_response) = expected_follow_up_after_response {
                let follow_up =
                    read_json_message(&mut reader, &format!("post-result:{method}")).await;
                assert_eq!(follow_up, expected_response);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

enum PeerStep {
    ExpectRequest {
        method: String,
        expected_params: Option<Value>,
        response: Option<Value>,
        emits: Vec<Value>,
        expected_follow_up: Option<Value>,
        expected_follow_up_after_response: Option<Value>,
        delay_before_emits_ms: Option<u64>,
        stderr_messages: Vec<String>,
        emits_after_notification: Vec<Value>,
        expected_notification: Option<String>,
    },
}

impl PeerStep {
    fn expect_params(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            expected_params, ..
        } = self;
        *expected_params = Some(value);
        self
    }

    fn respond(&mut self, result: Value) -> &mut Self {
        let PeerStep::ExpectRequest { response, .. } = self;
        *response = Some(result);
        self
    }

    fn emit_request(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest { emits, .. } = self;
        emits.push(value);
        self
    }

    fn emit_notification(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest { emits, .. } = self;
        emits.push(value);
        self
    }

    fn delay_before_emits(&mut self, milliseconds: u64) -> &mut Self {
        let PeerStep::ExpectRequest {
            delay_before_emits_ms,
            ..
        } = self;
        *delay_before_emits_ms = Some(milliseconds);
        self
    }

    fn emit_stderr(&mut self, message: &str) -> &mut Self {
        let PeerStep::ExpectRequest {
            stderr_messages, ..
        } = self;
        stderr_messages.push(message.to_owned());
        self
    }

    fn expect_response(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            expected_follow_up, ..
        } = self;
        *expected_follow_up = Some(value);
        self
    }

    fn expect_response_after_result(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            expected_follow_up_after_response,
            ..
        } = self;
        *expected_follow_up_after_response = Some(value);
        self
    }
}

async fn read_json_message<R>(reader: &mut BufReader<R>, context: &str) -> Value
where
    R: AsyncRead + Unpin,
{
    let line = timeout(Duration::from_secs(5), async {
        let mut buffer = String::new();
        reader.read_line(&mut buffer).await.expect("read line");
        buffer
    })
    .await
    .unwrap_or_else(|_| panic!("message timeout while waiting for {context}"));
    serde_json::from_str(line.trim_end()).expect("valid json")
}

async fn write_json<W>(writer: &mut W, value: Value)
where
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .expect("write json");
    writer.flush().await.expect("flush");
}

async fn write_line<W>(writer: &mut W, value: &str)
where
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .expect("write line");
    writer.flush().await.expect("flush");
}
