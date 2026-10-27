//! `agentSessions.scan` and `agentSessions.import` against real transcripts in temporary
//! Claude and Codex homes, through the production RPC registry.

use std::{path::Path, time::Duration};

use coddy_studio_server::{
    CauseItem, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    orchestration::{EngineOptions, OrchestrationEngine},
    persistence::{Database, ProviderSessionRuntime, run_migrations},
    production::{
        agent_sessions_rpc::register_agent_sessions_rpc,
        orchestration_rpc::register_orchestration_rpc,
    },
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const CLAUDE_ID: &str = "0f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
const CODEX_ID: &str = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
const RESPONSE_DEADLINE: Duration = Duration::from_secs(10);

fn write_jsonl(path: &Path, records: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, text + "\n").unwrap();
}

async fn request(socket: &mut TestSocket, id: &str, tag: &str, payload: Value) -> RpcExit {
    socket
        .send(Message::Text(
            json!({"_tag":"Request","id":id,"tag":tag,"payload":payload,"headers":[]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let message = timeout(RESPONSE_DEADLINE, next_frame_past_heartbeat(socket))
        .await
        .expect("response before the deadline")
        .expect("socket open")
        .expect("valid frame");
    let Message::Text(text) = message else {
        panic!("expected a text frame, got {message:?}");
    };
    match serde_json::from_str(&text).expect("server message") {
        ServerMessage::Exit { request_id, exit } => {
            assert_eq!(request_id.as_str(), id);
            exit
        }
        other => panic!("expected an exit, got {other:?}"),
    }
}

async fn success(socket: &mut TestSocket, id: &str, tag: &str, payload: Value) -> Value {
    match request(socket, id, tag, payload).await {
        RpcExit::Success { value } => value.unwrap_or(Value::Null),
        other => panic!("{tag} failed: {other:?}"),
    }
}

#[tokio::test]
async fn imports_cli_sessions_as_threads_that_resume_them() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("repo");
    let other = temp.path().join("other");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let workspace = std::fs::canonicalize(workspace).unwrap();
    let claude_home = temp.path().join("claude-home");
    let codex_home = temp.path().join("codex-home");
    // Both homes point into the temporary directory so no test reads the developer's own.
    let settings_root = temp.path().join("state");
    std::fs::create_dir_all(&settings_root).unwrap();
    std::fs::write(
        settings_root.join("settings.json"),
        json!({
            "providerInstances": {
                "claudeAgent": {
                    "driver": "claudeAgent",
                    "environment": [{ "name": "CLAUDE_CONFIG_DIR", "value": claude_home }]
                },
                "codex": { "driver": "codex", "config": { "homePath": codex_home } }
            }
        })
        .to_string(),
    )
    .unwrap();
    write_jsonl(
        &claude_home.join(format!("projects/-repo/{CLAUDE_ID}.jsonl")),
        &[
            json!({"type":"user","sessionId":CLAUDE_ID,"cwd":workspace,"timestamp":"2026-10-07T10:00:00.000Z","message":{"role":"user","content":"Explain the build"}}),
            json!({"type":"user","sessionId":CLAUDE_ID,"isSidechain":true,"message":{"content":"subagent"}}),
            json!({"type":"assistant","sessionId":CLAUDE_ID,"timestamp":"2026-10-07T10:00:05.000Z","message":{"model":"claude-opus-4-8","content":[{"type":"text","text":"It uses Cargo."}]}}),
        ],
    );
    let elsewhere = "1f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
    write_jsonl(
        &claude_home.join(format!("projects/-other/{elsewhere}.jsonl")),
        &[
            json!({"type":"user","sessionId":elsewhere,"cwd":other,"message":{"content":"Elsewhere"}}),
        ],
    );
    write_jsonl(
        &codex_home.join(format!(
            "sessions/2026/10/07/rollout-2026-10-07T09-00-00-{CODEX_ID}.jsonl"
        )),
        &[
            json!({"type":"session_meta","timestamp":"2026-10-07T09:00:00.000Z","payload":{"id":CODEX_ID,"cwd":workspace}}),
            json!({"type":"turn_context","payload":{"model":"gpt-5.4","cwd":workspace}}),
            json!({"type":"response_item","timestamp":"2026-10-07T09:00:01.000Z","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Add a test"}]}}),
            json!({"type":"event_msg","timestamp":"2026-10-07T09:00:01.000Z","payload":{"type":"user_message","message":"Add a test"}}),
            json!({"type":"response_item","timestamp":"2026-10-07T09:00:09.000Z","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Added."}]}}),
        ],
    );

    let database = Database::open_in_memory().await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database, EngineOptions::default())
        .await
        .unwrap();
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create","commandId":"create-project","projectId":"project",
                "title":"Repo","workspaceRoot":workspace,"createdAt":"2026-10-01T00:00:00.000Z"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc(&mut registry, engine.clone());
    register_agent_sessions_rpc(&mut registry, engine.clone(), settings_root);
    let handle = ServerRuntime::start_with_registry(
        ServerConfig::new(temp.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
        registry,
    )
    .await
    .unwrap();
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .unwrap()
        .0;
    let claude_thread = format!("import:claudeAgent:{CLAUDE_ID}");
    let codex_thread = format!("import:codex:{CODEX_ID}");
    // An import interrupted after it created the Claude thread is not imported yet.
    engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create","commandId":"interrupted-import","threadId":claude_thread,
                "projectId":"project","title":"Explain the build",
                "modelSelection":{"instanceId":"claudeAgent","model":"claude-opus-4-8"},
                "runtimeMode":"full-access","interactionMode":"default","branch":null,
                "createdAt":"2026-10-07T10:00:00.000Z"
            }))
            .unwrap(),
        )
        .await
        .unwrap();

    let scanned = success(
        &mut socket,
        "1",
        "agentSessions.scan",
        json!({"projectId":"project"}),
    )
    .await;
    assert_eq!(scanned["truncated"], json!(false));
    let mut candidates = scanned["candidates"].as_array().unwrap().clone();
    candidates.iter_mut().for_each(|candidate| {
        candidate.as_object_mut().unwrap().remove("lastActiveAt");
    });
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    for (provider, session_id, title) in [
        ("claudeAgent", CLAUDE_ID, "Explain the build"),
        ("codex", CODEX_ID, "Add a test"),
    ] {
        assert!(
            candidates.contains(&json!({
                "provider":provider,"sessionId":session_id,"title":title,
                "messageCount":2,"alreadyImported":false
            })),
            "{candidates:?}"
        );
    }

    let sessions = json!({"projectId":"project","sessions":[
        {"provider":"claudeAgent","sessionId":CLAUDE_ID},
        {"provider":"codex","sessionId":CODEX_ID},
        {"provider":"claudeAgent","sessionId":elsewhere}
    ]});
    let imported = success(&mut socket, "2", "agentSessions.import", sessions.clone()).await;
    assert_eq!(
        imported["imported"],
        json!([
            {"sessionId":CLAUDE_ID,"threadId":claude_thread},
            {"sessionId":CODEX_ID,"threadId":codex_thread}
        ])
    );
    assert_eq!(
        imported["skipped"],
        json!([{"sessionId":elsewhere,"reason":"The session was not run in this project's folder."}])
    );

    let repositories = engine.repositories();
    for (thread_id, provider, model, adapter, cursor, payload, texts) in [
        (
            &claude_thread,
            "claudeAgent",
            "claude-opus-4-8",
            "claude-stream-json",
            json!({"sessionId":CLAUDE_ID}),
            json!({"transport":"stream-json"}),
            [
                ("user", "Explain the build"),
                ("assistant", "It uses Cargo."),
            ],
        ),
        (
            &codex_thread,
            "codex",
            "gpt-5.4",
            "codex-app-server",
            json!({"threadId":CODEX_ID}),
            json!({"model":"gpt-5.4","cwd":workspace}),
            [("user", "Add a test"), ("assistant", "Added.")],
        ),
    ] {
        let thread = repositories
            .get_thread(thread_id.clone())
            .await
            .unwrap()
            .expect("imported thread");
        assert_eq!(thread.project_id, "project");
        assert_eq!(
            thread.model_selection,
            json!({"instanceId":provider,"model":model})
        );
        let messages = repositories
            .list_messages_by_thread(thread_id.clone())
            .await
            .unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| (message.role.as_str(), message.text.as_str()))
                .collect::<Vec<_>>(),
            texts
        );
        assert!(messages.iter().all(|message| message.turn_id.is_none()));
        let runtime = repositories
            .get_provider_session_runtime(thread_id.clone())
            .await
            .unwrap()
            .expect("runtime row");
        assert_eq!(runtime.status, "suspended");
        assert_eq!(runtime.provider_name, provider);
        assert_eq!(runtime.provider_instance_id.as_deref(), Some(provider));
        assert_eq!(runtime.adapter_key, adapter);
        assert_eq!(runtime.resume_cursor, Some(cursor));
        assert_eq!(runtime.runtime_payload, Some(payload));
    }

    // A session another Coddy Studio thread runs is neither offered nor imported, and an
    // interrupted import whose thread started its own conversation keeps that conversation.
    let owned_id = "2f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
    let used_id = "3f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40";
    for (id, prompt) in [(owned_id, "Owned prompt"), (used_id, "Used prompt")] {
        write_jsonl(
            &claude_home.join(format!("projects/-repo/{id}.jsonl")),
            &[json!({"type":"user","sessionId":id,"cwd":workspace,"message":{"content":prompt}})],
        );
    }
    let used_thread = format!("import:claudeAgent:{used_id}");
    for (command_id, thread) in [
        ("owner-create", "owner-thread"),
        ("used-create", used_thread.as_str()),
    ] {
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.create","commandId":command_id,"threadId":thread,
                    "projectId":"project","title":"Thread",
                    "modelSelection":{"instanceId":"claudeAgent","model":"claude-opus-4-8"},
                    "runtimeMode":"full-access","interactionMode":"default","branch":null,
                    "createdAt":"2026-10-07T10:00:00.000Z"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
    }
    for (thread, cursor) in [
        ("owner-thread", owned_id),
        (used_thread.as_str(), "fresh-conversation"),
    ] {
        repositories
            .upsert_provider_session_runtime(ProviderSessionRuntime {
                thread_id: thread.to_owned(),
                provider_name: "claudeAgent".to_owned(),
                provider_instance_id: Some("claudeAgent".to_owned()),
                adapter_key: "claude-stream-json".to_owned(),
                runtime_mode: "full-access".to_owned(),
                status: "suspended".to_owned(),
                last_seen_at: "2026-10-07T10:00:00.000Z".to_owned(),
                resume_cursor: Some(json!({"sessionId":cursor})),
                runtime_payload: None,
            })
            .await
            .unwrap();
    }
    let listed = success(
        &mut socket,
        "8",
        "agentSessions.scan",
        json!({"projectId":"project"}),
    )
    .await;
    let listed_ids = listed["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["sessionId"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(listed_ids.iter().any(|id| id == used_id), "{listed_ids:?}");
    assert!(
        !listed_ids.iter().any(|id| id == owned_id),
        "{listed_ids:?}"
    );
    let refused = success(
        &mut socket,
        "9",
        "agentSessions.import",
        json!({"projectId":"project","sessions":[
            {"provider":"claudeAgent","sessionId":owned_id},
            {"provider":"claudeAgent","sessionId":used_id}
        ]}),
    )
    .await;
    assert_eq!(refused["imported"], json!([]));
    assert_eq!(
        refused["skipped"],
        json!([
            {"sessionId":owned_id,"reason":"This session already belongs to a Coddy Studio thread."},
            {"sessionId":used_id,"reason":"This session's thread was used before its import finished, so it keeps its own conversation."}
        ])
    );
    assert_eq!(
        repositories
            .get_provider_session_runtime(used_thread.clone())
            .await
            .unwrap()
            .unwrap()
            .resume_cursor,
        Some(json!({"sessionId":"fresh-conversation"}))
    );

    let repeated = success(&mut socket, "3", "agentSessions.import", sessions).await;
    assert_eq!(repeated["imported"], json!([]));
    assert_eq!(
        repeated["skipped"].as_array().unwrap()[..2],
        [
            json!({"sessionId":CLAUDE_ID,"reason":"Already imported."}),
            json!({"sessionId":CODEX_ID,"reason":"Already imported."})
        ]
    );
    assert_eq!(
        repositories
            .list_messages_by_thread(claude_thread.clone())
            .await
            .unwrap()
            .len(),
        2
    );
    let rescanned = success(
        &mut socket,
        "4",
        "agentSessions.scan",
        json!({"projectId":"project"}),
    )
    .await;
    let claude = rescanned["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["sessionId"] == CLAUDE_ID)
        .unwrap();
    assert_eq!(claude["alreadyImported"], json!(true));
    assert_eq!(claude["threadId"], json!(claude_thread));

    // A deleted import keeps its ID, so it is reported instead of failing thread creation.
    engine
        .dispatch(
            serde_json::from_value(
                json!({"type":"thread.delete","commandId":"delete-import","threadId":claude_thread}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let after_delete = success(
        &mut socket,
        "6",
        "agentSessions.scan",
        json!({"projectId":"project"}),
    )
    .await;
    let claude = after_delete["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["sessionId"] == CLAUDE_ID)
        .unwrap();
    assert_eq!(claude["alreadyImported"], json!(true));
    assert!(claude.get("threadId").is_none());
    let reimported = success(
        &mut socket,
        "7",
        "agentSessions.import",
        json!({"projectId":"project","sessions":[{"provider":"claudeAgent","sessionId":CLAUDE_ID}]}),
    )
    .await;
    assert_eq!(
        reimported["skipped"],
        json!([{"sessionId":CLAUDE_ID,"reason":"This session was imported before and its thread was deleted."}])
    );

    match request(
        &mut socket,
        "5",
        "agentSessions.scan",
        json!({"projectId":"missing"}),
    )
    .await
    {
        RpcExit::Failure { cause } => match &cause[..] {
            [CauseItem::Fail { error }] => {
                assert_eq!(error["_tag"], json!("AgentSessionsError"));
                assert_eq!(error["message"], json!("The project no longer exists."));
            }
            other => panic!("unexpected cause {other:?}"),
        },
        other => panic!("expected a failure, got {other:?}"),
    }

    handle.shutdown();
    handle.join().await.unwrap();
    engine.shutdown().await;
}
