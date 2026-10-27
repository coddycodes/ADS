use coddy_studio_server::provider::{OptionRefusal, cursor};

use std::{path::PathBuf, time::Duration};

use cursor::{
    AcpConnectionConfig, AcpJsonRpcConnection, CursorSessionOptions, CursorSessionRuntime,
    build_capabilities_from_config_options, discover_models_from_list_available_models,
    parse_about_output, parse_cli_config_channel, parse_version_date, resolve_acp_base_model_id,
    resolve_acp_config_updates,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, duplex},
    sync::mpsc,
    time::timeout,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureManifest {
    fixtures: Vec<String>,
}

#[test]
fn cursor_fixture_manifest_is_complete() {
    let manifest: FixtureManifest =
        serde_json::from_value(fixture("manifest.json")).expect("manifest");
    assert!(!manifest.fixtures.is_empty());
}

#[test]
fn cursor_helper_outputs_match_fixtures() {
    let about_fixture = fixture("about-authenticated.json");
    assert_eq!(
        serde_json::to_value(parse_about_output(
            about_fixture["code"].as_i64().expect("code") as i32,
            about_fixture["stdout"].as_str().expect("stdout"),
            about_fixture["stderr"].as_str().expect("stderr"),
        ))
        .expect("about json"),
        about_fixture["parsed"]
    );

    let capabilities_fixture = fixture("capabilities.json");
    assert_eq!(
        build_capabilities_from_config_options(&capabilities_fixture["options"]),
        capabilities_fixture["expected"]
    );

    let config_updates_fixture = fixture("config-updates.json");
    assert_eq!(
        serde_json::to_value(
            resolve_acp_config_updates(
                &config_updates_fixture["options"],
                &config_updates_fixture["updates"],
            )
            .expect("config updates")
        )
        .expect("updates json"),
        config_updates_fixture["expected"]
    );

    let discovery_fixture = fixture("model-discovery.json");
    assert_eq!(
        serde_json::to_value(
            discover_models_from_list_available_models(
                &discovery_fixture["response"],
                &discovery_fixture["customModels"]
                    .as_array()
                    .expect("custom models")
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            )
            .expect("models"),
        )
        .expect("models json"),
        discovery_fixture["expected"]
    );

    assert_eq!(parse_version_date("2026.04.08-c4e73a3"), Some(20260408));
    assert_eq!(
        parse_cli_config_channel(r#"{ "channel": "lab" }"#).as_deref(),
        Some("lab")
    );
    assert_eq!(
        resolve_acp_base_model_id("gpt-5.4[reasoning=medium,context=272k]"),
        "gpt-5.4"
    );
}

#[test]
fn cursor_config_updates_reject_stale_or_malformed_descriptors_before_rpc() {
    let fast = json!({
        "id": "fast",
        "category": "model_config",
        "type": "select",
        "currentValue": "false",
        "options": [{ "value": "false" }, { "value": "true" }]
    });
    assert!(
        resolve_acp_config_updates(
            &json!([fast]),
            &json!([{ "id": "fastMode", "value": true }]),
        )
        .is_ok()
    );
    // A request the session can't satisfy is refused in words naming the option by the label its
    // descriptor shows: the session's name for it, or Coddy Studio's default.
    let refusal = |options: Value, updates: Value| {
        resolve_acp_config_updates(&options, &updates)
            .expect_err("the update is refused")
            .option_refusal()
            .map(str::to_owned)
    };
    assert_eq!(
        refusal(json!([]), json!([{ "id": "fastMode", "value": true }])).as_deref(),
        Some("Fast is not supported by the selected model.")
    );
    assert_eq!(
        refusal(
            json!([{ "id": "context", "category": "model_config", "type": "select", "options": [{ "value": "272k" }] }]),
            json!([{ "id": "contextWindow", "value": "1m" }]),
        )
        .as_deref(),
        Some("Context 1m is not supported by the selected model.")
    );
    assert_eq!(
        refusal(
            json!([{ "id": "fast", "name": "Turbo", "category": "model_config", "type": "select", "options": [{ "value": "true" }] }]),
            json!([{ "id": "fastMode", "value": "yes" }]),
        )
        .as_deref(),
        Some("Turbo must be on or off.")
    );
    // A session configuration Coddy Studio can't work with is protocol detail the user can't act on,
    // so its refusal stays generic, with the next step the user can take.
    const UNUSABLE_OPTIONS: &str = "Coddy Studio can't apply these options to the selected model. Choose another model, or turn these options off.";
    for options in [
        json!([{ "id": "fast", "category": "model", "type": "select", "options": [{ "value": "true" }] }]),
        json!([{ "id": "fast", "category": "model_config", "type": "boolean", "options": [{ "value": "true" }] }]),
    ] {
        assert_eq!(
            refusal(options, json!([{ "id": "fastMode", "value": true }])).as_deref(),
            Some(UNUSABLE_OPTIONS)
        );
    }
    assert_eq!(
        refusal(
            json!([{ "id": "reasoning", "category": "thought_level", "type": "boolean", "options": [{ "value": "high" }] }]),
            json!([{ "id": "reasoning", "value": "high" }]),
        )
        .as_deref(),
        Some(UNUSABLE_OPTIONS)
    );
}

#[tokio::test]
async fn cursor_runtime_matches_approval_and_cancel_traces() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-thread-1".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: vec![json!({
                "type": "http",
                "name": "coddy-studio",
                "url": "http://127.0.0.1:3773/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer secret" }],
            })],
        },
        connection.clone(),
        incoming,
    );

    peer.expect_request("initialize")
        .respond(json!({ "protocolVersion": 1 }));
    peer.expect_request("authenticate")
        .respond(json!({ "status": "ok" }));
    peer.expect_request("session/new")
        .expect_params(json!({
            "cwd": "/tmp/project",
            "mcpServers": [{
                "type": "http",
                "name": "coddy-studio",
                "url": "http://127.0.0.1:3773/mcp",
                "headers": [{ "name": "Authorization", "value": "Bearer secret" }],
            }],
        }))
        .respond(json!({ "sessionId": "cursor-session-1" }));
    peer.expect_request("session/prompt")
        .expect_params(json!({
            "sessionId": "cursor-session-1",
            "prompt": [
                { "type": "text", "text": "run a tool call" },
                { "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" }
            ]
        }))
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 1001,
            "method": "session/request_permission",
            "params": {
                "toolCall": { "title": "`cat server/package.json`" },
                "options": [
                    { "optionId": "allow-once", "kind": "allow_once" },
                    { "optionId": "allow-always", "kind": "allow_always" },
                    { "optionId": "reject-once", "kind": "reject_once" }
                ]
            }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 1001,
            "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tool-call-1",
                    "title": "Terminal",
                    "status": "in_progress",
                    "rawInput": { "command": ["cat", "server/package.json"] }
                }
            }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "tool-call-1",
                    "title": "Terminal",
                    "status": "completed",
                    "rawInput": { "command": ["cat", "server/package.json"] }
                }
            }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "hello from mock" }
                }
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));
    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 1002,
            "method": "session/request_permission",
            "params": {
                "toolCall": { "title": "`cat server/package.json`" },
                "options": [
                    { "optionId": "allow-once", "kind": "allow_once" },
                    { "optionId": "allow-always", "kind": "allow_always" },
                    { "optionId": "reject-once", "kind": "reject_once" }
                ]
            }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 1002,
            "result": { "outcome": { "outcome": "cancelled" } }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": "ignored after cancel" }
                }
            }
        }))
        .expect_notification("session/cancel")
        .respond(json!({ "stopReason": "cancelled" }));

    let peer_task = tokio::spawn(peer.run());
    let started_session_id = runtime.start().await.expect("start");
    assert_eq!(started_session_id, "cursor-session-1");

    let approval_runtime = runtime.clone();
    let mut approval_task = tokio::spawn(async move {
        approval_runtime
            .send_turn(
                Some("run a tool call"),
                vec![json!({ "type": "image", "data": "aW1hZ2U=", "mimeType": "image/png" })],
            )
            .await
    });
    let mut approval_events = runtime.collect_events(4).await;
    let approval_turn = timeout(Duration::from_millis(100), &mut approval_task)
        .await
        .expect("send_turn should return while approval is pending")
        .expect("approval join")
        .expect("approval turn");
    runtime
        .respond_to_request("approval:1001", "accept")
        .await
        .expect("approval response");
    approval_events.extend(runtime.collect_events(5).await);
    let (assistant_chunk_index, assistant_chunk) = approval_events
        .iter()
        .enumerate()
        .find(|(_, event)| event.event_type == "content.delta")
        .expect("Cursor assistant chunk");
    assert_eq!(assistant_chunk.item_id, None);
    assert_eq!(assistant_chunk.payload["delta"], json!("hello from mock"));
    assert!(
        approval_events[assistant_chunk_index + 1..]
            .iter()
            .any(|event| {
                event.event_type == "turn.completed" && event.turn_id == assistant_chunk.turn_id
            })
    );
    normalize_turn_ids(&mut approval_events, &approval_turn, "turn-3");
    assert_eq!(approval_events, stable_fixture("trace-approval.json"));

    let cancel_runtime = runtime.clone();
    let mut cancel_task = tokio::spawn(async move {
        cancel_runtime
            .send_turn(Some("cancel this turn"), vec![])
            .await
    });
    let cancel_started = runtime.next_event().await.expect("cancel started");
    assert_eq!(cancel_started.event_type, "turn.started");
    let cancel_turn_id = cancel_started.turn_id.clone().expect("cancel turn id");
    let request_opened = runtime.next_event().await.expect("request opened");
    assert_eq!(request_opened.event_type, "request.opened");
    let returned_cancel_turn_id = timeout(Duration::from_millis(100), &mut cancel_task)
        .await
        .expect("send_turn should return while permission is pending")
        .expect("cancel join")
        .expect("cancel turn");
    assert_eq!(returned_cancel_turn_id, cancel_turn_id);
    runtime.interrupt_turn().await.expect("interrupt");
    let mut cancel_events = vec![cancel_started.stable_view(), request_opened.stable_view()];
    cancel_events.extend(runtime.collect_events(2).await);
    normalize_turn_ids(&mut cancel_events, &cancel_turn_id, "turn-10");
    assert_eq!(cancel_events, stable_fixture("trace-cancel.json"));

    peer_task.await.expect("peer");
    runtime
        .interrupt_turn()
        .await
        .expect("interrupt without an active turn is a no-op");
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
async fn cursor_runtime_applies_the_selected_model_after_session_creation() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-model-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "gpt-5.4".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize")
        .respond(json!({ "protocolVersion": 1 }));
    peer.expect_request("authenticate")
        .respond(json!({ "status": "ok" }));
    peer.expect_request("session/new").respond(json!({
        "sessionId": "cursor-model-session",
        "configOptions": [{ "id": "model", "category": "model" }],
        "modes": {
            "currentModeId": "ask",
            "availableModes": [
                { "id": "ask", "name": "Ask" },
                { "id": "code", "name": "Agent" },
                { "id": "architect", "name": "Plan" }
            ]
        }
    }));
    peer.expect_request("session/set_config_option")
        .respond(json!({ "configOptions": [{ "id": "model", "category": "model" }] }));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-model-session", "modeId": "code" }))
        .respond(json!({}));
    peer.expect_request("session/set_config_option")
        .respond(json!({ "configOptions": [{ "id": "model", "category": "model" }] }));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-model-session", "modeId": "architect" }))
        .respond(json!({}));
    let peer_task = tokio::spawn(peer.run());

    assert_eq!(
        runtime.start().await.expect("start"),
        "cursor-model-session"
    );
    runtime.set_model("gpt-5.5").await.expect("switch model");
    runtime
        .set_interaction_mode("plan")
        .await
        .expect("switch interaction mode");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_runtime_starts_a_new_session_when_the_saved_one_cannot_be_loaded() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-lost-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: Some("cursor-gone".to_owned()),
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize")
        .respond(json!({ "protocolVersion": 1 }));
    peer.expect_request("authenticate")
        .respond(json!({ "status": "ok" }));
    peer.expect_request("session/load")
        .expect_params(json!({ "sessionId": "cursor-gone" }))
        .respond_error(json!({ "code": -32602, "message": "Session not found" }));
    peer.expect_request("session/new")
        .expect_params(json!({ "cwd": "/tmp/project", "mcpServers": [] }))
        .respond(json!({ "sessionId": "cursor-fresh" }));
    let peer_task = tokio::spawn(peer.run());

    assert_eq!(runtime.start().await.expect("start"), "cursor-fresh");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_runtime_keeps_the_saved_session_when_load_fails_transiently() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-busy-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: Some("cursor-kept".to_owned()),
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize")
        .respond(json!({ "protocolVersion": 1 }));
    peer.expect_request("authenticate")
        .respond(json!({ "status": "ok" }));
    peer.expect_request("session/load")
        .expect_params(json!({ "sessionId": "cursor-kept" }))
        .respond_error(json!({ "code": -32603, "message": "Internal error" }));
    peer.expect_no_request();
    let peer_task = tokio::spawn(peer.run());

    let error = runtime
        .start()
        .await
        .expect_err("a transient load error must not replace the session");
    assert!(error.to_string().contains("Internal error"), "{error}");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_fast_mode_uses_acp_config_update() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-fast-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new").respond(json!({
        "sessionId": "s1",
        "configOptions": [{
            "id": "fast",
            "category": "model_config",
            "type": "select",
            "currentValue": "false",
            "options": [{ "value": "false" }, { "value": "true" }]
        }],
    }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({
            "configOptions": [{
                "id": "fast",
                "category": "model_config",
                "type": "select",
                "currentValue": "true",
                "options": [{ "value": "false" }, { "value": "true" }]
            }],
        }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond(json!({
            "configOptions": [{
                "id": "fast",
                "category": "model_config",
                "type": "select",
                "currentValue": "false",
                "options": [{ "value": "false" }, { "value": "true" }]
            }],
        }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("fast config update");
    runtime
        .set_options(Vec::new())
        .await
        .expect("omitting fast mode restores the advertised baseline");

    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_empty_options_compensate_if_a_baseline_reset_fails() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-baseline-compensation-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let baseline = json!([
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "false", "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context", "category": "model_config", "type": "select",
            "currentValue": "272k", "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    let fast_enabled = json!([
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "true", "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context", "category": "model_config", "type": "select",
            "currentValue": "272k", "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    let active = json!([
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "true", "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context", "category": "model_config", "type": "select",
            "currentValue": "1m", "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    let context_reset = json!([
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "true", "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context", "category": "model_config", "type": "select",
            "currentValue": "272k", "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": baseline }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": fast_enabled }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "context", "value": "1m" }))
        .respond(json!({ "configOptions": active.clone() }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "context", "value": "272k" }))
        .respond(json!({ "configOptions": context_reset }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond_error(json!({ "code": -32000, "message": "baseline reset rejected" }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "context", "value": "1m" }))
        .respond(json!({ "configOptions": active }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime
        .set_options(vec![
            json!({ "id": "fastMode", "value": true }),
            json!({ "id": "contextWindow", "value": "1m" }),
        ])
        .await
        .expect("activate non-default options");
    assert!(runtime.set_options(Vec::new()).await.is_err());

    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_model_switch_uses_target_baselines_and_revalidates_active_options() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-model-option-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let config_options = |model: &str, fast: Option<&str>| {
        let mut options =
            vec![json!({ "id": "model", "category": "model", "currentValue": model })];
        if let Some(fast) = fast {
            options.push(json!({
                "id": "fast", "category": "model_config", "type": "select",
                "currentValue": fast, "options": [{ "value": "false" }, { "value": "true" }]
            }));
        }
        options
    };
    let old_baseline = config_options("old", Some("false"));
    let old_active = config_options("old", Some("true"));
    let target_advertisement = vec![
        json!({ "id": "model", "category": "model", "currentValue": "target" }),
        json!({
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "true",
            "options": [
                { "value": "false", "isDefault": true },
                { "value": "true" }
            ]
        }),
    ];
    let target_active = target_advertisement.clone();
    let target_reset = config_options("target", Some("false"));
    let unsupported_target = config_options("no-fast", None);

    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": old_baseline.clone() }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": old_active }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "model", "value": "target" }))
        .respond(json!({ "configOptions": target_advertisement }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": target_active }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond(json!({ "configOptions": target_reset }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "model", "value": "no-fast" }))
        .respond(json!({ "configOptions": unsupported_target }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "model", "value": "old" }))
        .respond(json!({ "configOptions": old_baseline }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": config_options("old", Some("true")) }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("enable fast on old model");
    runtime
        .set_model("target")
        .await
        .expect("switch to compatible target");
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("reapply fast against target capabilities");
    runtime
        .set_options(Vec::new())
        .await
        .expect("clear uses the target model baseline");
    runtime
        .set_model("no-fast")
        .await
        .expect("switch to target without fast");
    let refused = runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect_err("the target model does not advertise fast");
    // The model does not advertise the option, so a retry of the same turn cannot succeed.
    assert_eq!(
        refused.option_refusal(),
        Some("Fast is not supported by the selected model."),
        "{refused}"
    );
    runtime
        .set_model("old")
        .await
        .expect("restore the previous model");
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("restore the previous active options");

    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_default_model_restoration_uses_the_advertised_provider_value() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-default-model-restore-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let default_options = json!([
        {
            "id": "model", "category": "model", "type": "select",
            "currentValue": "cursor-auto",
            "options": [
                { "value": "cursor-auto", "isDefault": true },
                { "value": "target" }
            ]
        },
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "false", "options": [{ "value": "false" }, { "value": "true" }]
        }
    ]);
    let target_options = json!([
        {
            "id": "model", "category": "model", "type": "select",
            "currentValue": "target",
            "options": [
                { "value": "cursor-auto", "isDefault": true },
                { "value": "target" }
            ]
        }
    ]);
    let default_fast_enabled = json!([
        {
            "id": "model", "category": "model", "type": "select",
            "currentValue": "cursor-auto",
            "options": [
                { "value": "cursor-auto", "isDefault": true },
                { "value": "target" }
            ]
        },
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "true", "options": [{ "value": "false" }, { "value": "true" }]
        }
    ]);
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": default_options.clone() }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "model", "value": "target" }))
        .respond(json!({ "configOptions": target_options }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "model", "value": "cursor-auto" }))
        .respond(json!({ "configOptions": default_options.clone() }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": default_fast_enabled }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond(json!({ "configOptions": default_options }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime.set_model("target").await.expect("switch to target");
    runtime
        .set_model("default")
        .await
        .expect("restore Cursor's advertised default model");
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("restored model acknowledgement replaces current options");
    runtime
        .set_options(Vec::new())
        .await
        .expect("restored model acknowledgement replaces baseline options");
    timeout(Duration::from_secs(1), peer_task)
        .await
        .expect("default restoration RPC")
        .expect("peer");
}

#[tokio::test]
async fn cursor_default_model_restoration_rejects_a_fabricated_default() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-missing-default-model-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let initial_options = json!([{
        "id": "model", "category": "model", "type": "select",
        "options": [{ "value": "first-listed" }, { "value": "target" }]
    }]);
    let target_options = json!([{
        "id": "model", "category": "model", "type": "select",
        "currentValue": "target",
        "options": [{ "value": "first-listed" }, { "value": "target" }]
    }]);
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": initial_options }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "model", "value": "target" }))
        .respond(json!({ "configOptions": target_options }));
    peer.expect_no_request();
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime.set_model("target").await.expect("switch to target");
    let refused = runtime
        .set_model("default")
        .await
        .expect_err("a session without a reversible default refuses it");
    // The session advertises no default, so switching to it is refused for good, in plain words.
    assert_eq!(
        refused.option_refusal(),
        Some("This session can't switch to the default model. Choose another model."),
        "{refused}"
    );
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_option_failure_compensates_acknowledged_updates() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-compensation-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let initial = json!([
        {
            "id": "fast",
            "category": "model_config",
            "type": "select",
            "currentValue": "false",
            "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context",
            "category": "model_config",
            "type": "select",
            "currentValue": "272k",
            "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    let fast_enabled = json!([
        {
            "id": "fast",
            "category": "model_config",
            "type": "select",
            "currentValue": "true",
            "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context",
            "category": "model_config",
            "type": "select",
            "currentValue": "272k",
            "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": initial }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": fast_enabled }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "context", "value": "1m" }))
        .respond_error(json!({ "code": -32000, "message": "context rejected" }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond(json!({ "configOptions": initial }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": fast_enabled }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    let rolled_back = runtime
        .set_options(vec![
            json!({ "id": "fastMode", "value": true }),
            json!({ "id": "contextWindow", "value": "1m" }),
        ])
        .await
        .expect_err("the rejected update fails after compensation");
    // Cursor rejected an advertised value and the update was rolled back, so a retry may succeed.
    assert_eq!(rolled_back.option_refusal(), None, "{rolled_back}");
    assert!(
        rolled_back
            .to_string()
            .starts_with("Cursor option update failed and was rolled back: "),
        "{rolled_back}"
    );
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("compensated acknowledgement must become the next local snapshot");

    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_malformed_config_acknowledgement_is_compensated() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-malformed-ack-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let initial = json!([{
        "id": "fast",
        "category": "model_config",
        "type": "select",
        "currentValue": "false",
        "options": [{ "value": "false" }, { "value": "true" }]
    }]);
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": initial }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({}));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond(json!({ "configOptions": initial }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    assert!(
        runtime
            .set_options(vec![json!({ "id": "fastMode", "value": true })])
            .await
            .is_err()
    );
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_failed_compensation_latches_configuration_uncertain() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-uncertain-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    let initial = json!([
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "false", "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context", "category": "model_config", "type": "select",
            "currentValue": "272k", "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    let fast_enabled = json!([
        {
            "id": "fast", "category": "model_config", "type": "select",
            "currentValue": "true", "options": [{ "value": "false" }, { "value": "true" }]
        },
        {
            "id": "context", "category": "model_config", "type": "select",
            "currentValue": "272k", "options": [{ "value": "272k" }, { "value": "1m" }]
        }
    ]);
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "s1", "configOptions": initial }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "true" }))
        .respond(json!({ "configOptions": fast_enabled }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "context", "value": "1m" }))
        .respond_error(json!({ "code": -32000, "message": "context rejected" }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({ "sessionId": "s1", "configId": "fast", "value": "false" }))
        .respond_error(json!({ "code": -32000, "message": "rollback rejected" }));
    peer.expect_no_request();
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    assert!(
        runtime
            .set_options(vec![
                json!({ "id": "fastMode", "value": true }),
                json!({ "id": "contextWindow", "value": "1m" }),
            ])
            .await
            .is_err()
    );
    assert!(runtime.set_options(Vec::new()).await.is_err());
    assert!(
        runtime
            .set_options(vec![json!({ "id": "fastMode", "value": true })])
            .await
            .is_err()
    );
    assert!(
        runtime
            .send_turn(Some("must not send"), Vec::new())
            .await
            .is_err()
    );
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_runtime_leaves_the_default_model_to_cursor() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-default-model-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "default".to_owned(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new").respond(json!({
        "sessionId": "cursor-default-model-session",
        "configOptions": [{ "id": "model", "category": "model" }],
        "modes": { "currentModeId": "ask", "availableModes": [{ "id": "ask", "name": "Ask" }, { "id": "code", "name": "Agent" }] }
    }));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-default-model-session", "modeId": "code" }))
        .respond(json!({}));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_full_access_auto_approves_permissions() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-auto-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
            model: String::new(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "cursor-auto-session" }));
    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 3001,
            "method": "session/request_permission",
            "params": {
                "options": [
                    { "optionId": "once", "kind": "allow_once" },
                    { "optionId": "always", "kind": "allow_always" }
                ]
            }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 3001,
            "result": { "outcome": { "outcome": "selected", "optionId": "always" } }
        }))
        .respond(json!({ "stopReason": "end_turn" }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime
        .send_turn(Some("run"), Vec::new())
        .await
        .expect("send");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_extension_requests_emit_plan_todos_and_questions() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-extension-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "plan".to_owned(),
            model: String::new(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new")
        .respond(json!({ "sessionId": "cursor-extension-session" }));
    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 4001,
            "method": "cursor/create_plan",
            "params": { "plan": "# Ship it" }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 4001,
            "result": { "accepted": true }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "cursor/update_todos",
            "params": {
                "todos": [{ "content": "Run tests", "status": "in_progress" }],
                "merge": false
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));
    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 4002,
            "method": "cursor/ask_question",
            "params": {
                "questions": [{
                    "id": "scope",
                    "prompt": "Which scope?",
                    "options": [{ "id": "workspace", "label": "Workspace" }]
                }]
            }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 4002,
            "result": { "answers": { "scope": "Workspace" } }
        }))
        .respond(json!({ "stopReason": "end_turn" }));
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime
        .send_turn(Some("plan"), Vec::new())
        .await
        .expect("send");
    let events = runtime.collect_events(5).await;
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "turn.proposed.completed")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "turn.plan.updated")
    );
    runtime
        .send_turn(Some("question"), Vec::new())
        .await
        .expect("send question");
    let mut request_id = None;
    for _ in 0..4 {
        let event = timeout(Duration::from_secs(2), runtime.next_event())
            .await
            .expect("extension event timeout")
            .expect("extension event");
        if event.event_type == "user-input.requested" {
            request_id = event.request_id;
            break;
        }
    }
    let request_id = request_id.expect("question request");
    runtime
        .respond_to_user_input(&request_id, json!({ "scope": "Workspace" }))
        .await
        .expect("answer extension question");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn cursor_runtime_covers_resume_permission_and_notification_edges() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-edge-thread".to_owned(),
            cwd: "/tmp/cursor-edge".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "default".to_owned(),
            model: "gpt-edge".to_owned(),
            resume_session_id: Some("cursor-resumed".to_owned()),
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );

    assert!(
        runtime
            .send_turn(Some("before start"), Vec::new())
            .await
            .expect_err("turns require a provider session")
            .to_string()
            .contains("has not started")
    );
    assert!(
        runtime
            .respond_to_request("missing", "accept")
            .await
            .expect_err("unknown approvals must fail")
            .to_string()
            .contains("Unknown pending request id missing")
    );
    assert!(
        runtime
            .respond_to_user_input("missing-input", json!({}))
            .await
            .expect_err("unknown prompts must fail")
            .to_string()
            .contains("Unknown pending request id missing-input")
    );

    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate")
        .expect_params(json!({ "methodId": "cursor_login" }))
        .respond(json!({}));
    peer.expect_request("session/load")
        .expect_params(json!({ "sessionId": "cursor-resumed" }))
        .respond(json!({
            "configOptions": [{ "id": "model", "category": "model" }],
            "modes": {
                "currentModeId": "code",
                "availableModes": [
                    { "id": "architect", "name": "Plan" },
                    { "id": "ask", "name": "Ask" },
                    { "id": "code", "name": "Agent" }
                ]
            }
        }));
    peer.expect_request("session/set_config_option")
        .expect_params(json!({
            "sessionId": "cursor-resumed",
            "configId": "model",
            "value": "gpt-edge",
        }))
        .respond(json!({
            "configOptions": [{ "id": "model-next", "category": "model" }]
        }));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-resumed", "modeId": "ask" }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-resumed", "modeId": "code" }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-resumed", "modeId": "architect" }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-resumed", "modeId": "code" }))
        .respond(json!({}));
    peer.expect_request("session/set_mode")
        .expect_params(json!({ "sessionId": "cursor-resumed", "modeId": "ask" }))
        .respond(json!({}));

    for (wire_id, expected_option) in [(5001, "always"), (5002, "reject")] {
        peer.expect_request("session/prompt")
            .emit_request(cursor_permission_request(wire_id))
            .expect_response(json!({
                "jsonrpc": "2.0",
                "id": wire_id,
                "result": {
                    "outcome": { "outcome": "selected", "optionId": expected_option }
                }
            }))
            .respond(json!({ "stopReason": "end_turn" }));
    }

    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 5003,
            "method": "x.ai/ask_user_question",
            "params": {
                "params": {
                    "questions": [
                        { "question": "Targets", "options": [] },
                        {}
                    ]
                }
            }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 5003,
            "result": {
                "outcome": "accepted",
                "answers": {
                    "array": ["server", "desktop"],
                    "string": ["all"],
                    "ignored": []
                }
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));

    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 5004,
            "method": "_x.ai/ask_user_question",
            "params": { "questions": [] }
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 5004,
            "result": { "outcome": "cancelled" }
        }))
        .respond(json!({ "stopReason": "end_turn" }));

    peer.expect_request("session/prompt")
        .emit_stderr("cursor edge warning")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 5005,
            "method": "cursor/create_plan",
            "params": { "plan": "" }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "cursor/update_todos",
            "params": {
                "todos": [
                    { "content": "  " },
                    { "title": "Queued task", "status": "queued" },
                    { "content": "Finished task", "status": "completed" }
                ]
            }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": { "sessionUpdate": "plan" }
            }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "update": {
                    "sessionUpdate": "tool_call",
                    "title": "Fallback command"
                }
            }
        }))
        .emit_notification_after_response(json!({
            "jsonrpc": "2.0",
            "method": "ignored/notification",
            "params": {}
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 5005,
            "result": { "accepted": true }
        }))
        .respond(json!({}));

    peer.expect_request("session/prompt")
        .emit_request(json!({
            "jsonrpc": "2.0",
            "id": 5006,
            "method": "unsupported/request",
            "params": {}
        }))
        .expect_response(json!({
            "jsonrpc": "2.0",
            "id": 5006,
            "error": {
                "code": -32601,
                "message": "Method not found: unsupported/request",
                "data": null
            }
        }))
        .respond(json!({ "stopReason": "end_turn" }));

    let peer_task = tokio::spawn(peer.run());
    assert_eq!(runtime.start().await.expect("resume"), "cursor-resumed");
    runtime.collect_events(2).await;
    runtime
        .set_runtime_mode("full-access")
        .await
        .expect("full access mode");
    runtime
        .set_interaction_mode("plan")
        .await
        .expect("plan mode");
    runtime
        .set_interaction_mode("default")
        .await
        .expect("default mode");
    runtime
        .set_runtime_mode("approval-required")
        .await
        .expect("approval mode");

    for (wire_id, decision) in [(5001, "acceptForSession"), (5002, "decline")] {
        let turn_id = runtime
            .send_turn(Some("permission"), Vec::new())
            .await
            .expect("permission turn");
        let request_id = format!("approval:{wire_id}");
        next_cursor_event_matching(&runtime, |event| {
            event.event_type == "request.opened"
                && event.request_id.as_deref() == Some(request_id.as_str())
        })
        .await;
        runtime
            .respond_to_request(&request_id, decision)
            .await
            .expect("permission decision");
        next_cursor_event_matching(&runtime, |event| {
            event.event_type == "turn.completed"
                && event.turn_id.as_deref() == Some(turn_id.as_str())
        })
        .await;
    }

    let normalized_turn = runtime
        .send_turn(Some("normalize answers"), Vec::new())
        .await
        .expect("input turn");
    let requested = next_cursor_event_matching(&runtime, |event| {
        event.request_id.as_deref() == Some("user-input:5003")
    })
    .await;
    assert_eq!(requested.payload["questions"][0]["id"], "Targets");
    assert_eq!(requested.payload["questions"][1]["id"], "");
    runtime
        .respond_to_user_input(
            "user-input:5003",
            json!({
                "array": ["server", "desktop"],
                "string": "all",
                "ignored": 42,
            }),
        )
        .await
        .expect("normalized response");
    next_cursor_event_matching(&runtime, |event| {
        event.event_type == "turn.completed"
            && event.turn_id.as_deref() == Some(normalized_turn.as_str())
    })
    .await;

    let cancelled_input_turn = runtime
        .send_turn(Some("cancel prompt"), Vec::new())
        .await
        .expect("cancel input turn");
    next_cursor_event_matching(&runtime, |event| {
        event.request_id.as_deref() == Some("user-input:5004")
    })
    .await;
    runtime
        .respond_to_request("user-input:5004", "cancel")
        .await
        .expect("generic cancellation");
    next_cursor_event_matching(&runtime, |event| {
        event.event_type == "turn.completed"
            && event.turn_id.as_deref() == Some(cancelled_input_turn.as_str())
    })
    .await;

    let notification_turn = runtime
        .send_turn(Some("notifications"), Vec::new())
        .await
        .expect("notification turn");
    let notification_events = collect_cursor_turn(&runtime, &notification_turn).await;
    assert!(notification_events.iter().any(|event| {
        event.event_type == "runtime.warning"
            && event.payload["message"] == json!("cursor edge warning")
    }));
    assert!(notification_events.iter().any(|event| {
        event.event_type == "turn.proposed.completed"
            && event.payload["planMarkdown"]
                == json!("# Plan\n\n(Cursor did not supply plan text.)")
    }));
    assert!(notification_events.iter().any(|event| {
        event.event_type == "item.updated" && event.payload["detail"] == json!("Fallback command")
    }));

    let unknown_turn = runtime
        .send_turn(Some("unknown"), Vec::new())
        .await
        .expect("unknown turn");
    next_cursor_event_matching(&runtime, |event| {
        event.event_type == "turn.completed"
            && event.turn_id.as_deref() == Some(unknown_turn.as_str())
    })
    .await;
    peer_task.await.expect("peer");
    let exited =
        next_cursor_event_matching(&runtime, |event| event.event_type == "session.exited").await;
    assert!(exited.payload["reason"].as_str().is_some());
}

#[tokio::test]
async fn cursor_runtime_rejects_new_sessions_without_an_identifier() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CursorSessionRuntime::new(
        CursorSessionOptions {
            thread_id: "cursor-invalid-thread".to_owned(),
            cwd: "/tmp/cursor-invalid".to_owned(),
            runtime_mode: "approval-required".to_owned(),
            interaction_mode: "default".to_owned(),
            model: String::new(),
            resume_session_id: None,
            mcp_servers: Vec::new(),
        },
        connection,
        incoming,
    );
    peer.expect_request("initialize").respond(json!({}));
    peer.expect_request("authenticate").respond(json!({}));
    peer.expect_request("session/new").respond(json!({}));
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

fn cursor_permission_request(wire_id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": wire_id,
        "method": "session/request_permission",
        "params": {
            "toolCall": { "title": "`edge command`" },
            "options": [
                { "optionId": "once", "kind": "allow_once" },
                { "optionId": "always", "kind": "allow_always" },
                { "optionId": "reject", "kind": "reject_once" }
            ]
        }
    })
}

async fn collect_cursor_turn(
    runtime: &CursorSessionRuntime,
    turn_id: &str,
) -> Vec<cursor::CursorRuntimeEvent> {
    let mut events = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(5), runtime.next_event())
            .await
            .expect("cursor event timeout")
            .expect("cursor event");
        let completed =
            event.event_type == "turn.completed" && event.turn_id.as_deref() == Some(turn_id);
        events.push(event);
        if completed {
            return events;
        }
    }
}

async fn next_cursor_event_matching(
    runtime: &CursorSessionRuntime,
    predicate: impl Fn(&cursor::CursorRuntimeEvent) -> bool,
) -> cursor::CursorRuntimeEvent {
    loop {
        let event = timeout(Duration::from_secs(5), runtime.next_event())
            .await
            .expect("cursor event timeout")
            .expect("cursor event");
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

fn stable_fixture(name: &str) -> Vec<cursor::CursorRuntimeEventStableView> {
    serde_json::from_value(fixture(name)).expect("stable fixture")
}

fn normalize_turn_ids(
    events: &mut [cursor::CursorRuntimeEventStableView],
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
        .join("../../packages/contracts/fixtures/cursor-provider")
}

fn scripted_peer() -> (
    AcpJsonRpcConnection,
    mpsc::UnboundedReceiver<cursor::IncomingEvent>,
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
            error: None,
            emits: Vec::new(),
            emits_after_follow_up: Vec::new(),
            expected_follow_up: None,
            expected_notification: None,
            stderr_messages: Vec::new(),
        });
        self.steps.last_mut().expect("step")
    }

    fn expect_no_request(&mut self) {
        self.steps.push(PeerStep::ExpectNoRequest);
    }

    async fn run(self) {
        let mut reader = BufReader::new(self.stdin_reader);
        let mut writer = self.stdout_writer;
        let mut stderr = self.stderr;
        for step in self.steps {
            match step {
                PeerStep::ExpectRequest {
                    method,
                    expected_params,
                    response,
                    error,
                    emits,
                    emits_after_follow_up,
                    expected_follow_up,
                    expected_notification,
                    stderr_messages,
                } => {
                    let message =
                        read_json_message(&mut reader, &format!("request:{method}")).await;
                    assert_eq!(message["method"], method);
                    if let Some(expected_params) = expected_params {
                        assert_eq!(message["params"], expected_params);
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
                    for emit in emits_after_follow_up {
                        write_json(&mut writer, emit).await;
                    }
                    if let Some(expected_notification) = expected_notification {
                        let notification = read_json_message(
                            &mut reader,
                            &format!("notification:{expected_notification}"),
                        )
                        .await;
                        assert_eq!(notification["method"], expected_notification);
                    }
                    if let Some(result) = response {
                        write_json(
                            &mut writer,
                            json!({ "jsonrpc": "2.0", "id": message["id"].clone(), "result": result }),
                        )
                        .await;
                    } else if let Some(error) = error {
                        write_json(
                            &mut writer,
                            json!({ "jsonrpc": "2.0", "id": message["id"].clone(), "error": error }),
                        )
                        .await;
                    }
                }
                PeerStep::ExpectNoRequest => {
                    assert!(
                        timeout(
                            Duration::from_millis(100),
                            read_json_message(&mut reader, "unexpected request")
                        )
                        .await
                        .is_err(),
                        "Cursor sent an RPC after configuration became uncertain"
                    );
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[allow(clippy::large_enum_variant)]
enum PeerStep {
    ExpectRequest {
        method: String,
        expected_params: Option<Value>,
        response: Option<Value>,
        error: Option<Value>,
        emits: Vec<Value>,
        emits_after_follow_up: Vec<Value>,
        expected_follow_up: Option<Value>,
        expected_notification: Option<String>,
        stderr_messages: Vec<String>,
    },
    ExpectNoRequest,
}

impl PeerStep {
    fn expect_params(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            expected_params, ..
        } = self
        else {
            panic!("expected request step");
        };
        *expected_params = Some(value);
        self
    }

    fn respond(&mut self, result: Value) -> &mut Self {
        let PeerStep::ExpectRequest { response, .. } = self else {
            panic!("expected request step");
        };
        *response = Some(result);
        self
    }

    fn respond_error(&mut self, error: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            error: response, ..
        } = self
        else {
            panic!("expected request step");
        };
        *response = Some(error);
        self
    }

    fn emit_request(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest { emits, .. } = self else {
            panic!("expected request step");
        };
        emits.push(value);
        self
    }

    fn emit_notification_after_response(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            emits_after_follow_up,
            ..
        } = self
        else {
            panic!("expected request step");
        };
        emits_after_follow_up.push(value);
        self
    }

    fn expect_response(&mut self, value: Value) -> &mut Self {
        let PeerStep::ExpectRequest {
            expected_follow_up, ..
        } = self
        else {
            panic!("expected request step");
        };
        *expected_follow_up = Some(value);
        self
    }

    fn expect_notification(&mut self, method: &str) -> &mut Self {
        let PeerStep::ExpectRequest {
            expected_notification,
            ..
        } = self
        else {
            panic!("expected request step");
        };
        *expected_notification = Some(method.to_owned());
        self
    }

    fn emit_stderr(&mut self, message: &str) -> &mut Self {
        let PeerStep::ExpectRequest {
            stderr_messages, ..
        } = self
        else {
            panic!("expected request step");
        };
        stderr_messages.push(message.to_owned());
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
