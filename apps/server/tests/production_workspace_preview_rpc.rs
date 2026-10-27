use std::{path::Path, time::Duration};

use coddy_studio_server::{
    ClientMessage, RequestId, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    mcp,
    preview::PreviewManager,
    workspace::{WorkspaceRpc, WorkspaceService},
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use coddy_studio_server::production::workspace_preview::{
    WorkspacePreviewRpcServices, register_workspace_preview_rpc,
};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn test_config(temp: &TempDir) -> ServerConfig {
    ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
}

async fn send_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, value: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send WebSocket message");
}

async fn next_server_message<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = timeout(Duration::from_secs(2), next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = message else {
        panic!("expected text WebSocket message, got {message:?}");
    };
    serde_json::from_str(&text).expect("valid server RPC message")
}

async fn request<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        json!({
            "_tag": "Request",
            "id": id,
            "tag": tag,
            "payload": payload,
            "headers": []
        }),
    )
    .await;
    next_server_message(socket).await
}

#[tokio::test]
async fn concrete_workspace_and_preview_handlers_run_through_effect_transport() {
    let temp = TempDir::new().expect("temporary base directory");
    tokio::fs::write(temp.path().join("existing.txt"), "hello")
        .await
        .expect("fixture");
    let mut registry = RpcRegistry::empty();
    register_workspace_preview_rpc(
        &mut registry,
        WorkspacePreviewRpcServices::new(
            WorkspaceRpc::new(WorkspaceService::default()),
            PreviewManager::new(),
            mcp::preview_automation::PreviewAutomationBroker::new(),
        ),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let cwd = path_string(temp.path());

    let created = request(
        &mut socket,
        "1",
        "projects.createEntry",
        json!({"cwd": cwd, "relativePath": "created.txt", "kind": "file"}),
    )
    .await;
    assert!(matches!(
        created,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } if value == json!({"relativePath": "created.txt"})
    ));

    let opened = request(
        &mut socket,
        "2",
        "preview.open",
        json!({"threadId": "thread-1", "url": "localhost:4173"}),
    )
    .await;
    let tab_id = match opened {
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } => {
            assert_eq!(value["threadId"], "thread-1");
            assert_eq!(value["navStatus"]["_tag"], "Loading");
            value["tabId"].as_str().expect("tab id").to_owned()
        }
        message => panic!("unexpected preview.open response: {message:?}"),
    };

    let resized = request(
        &mut socket,
        "4",
        "preview.resize",
        json!({
            "threadId": "thread-1",
            "tabId": tab_id.clone(),
            "viewport": {
                "_tag": "preset",
                "presetId": "iphone-se",
                "width": 375,
                "height": 667
            }
        }),
    )
    .await;
    assert!(matches!(
        resized,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } if value["viewport"]["presetId"] == "iphone-se"
            && value["viewport"].get("preset_id").is_none()
    ));

    let listed = request(
        &mut socket,
        "3",
        "preview.list",
        json!({"threadId": "thread-1"}),
    )
    .await;
    assert!(matches!(
        listed,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } if value["sessions"][0]["tabId"] == tab_id
    ));

    let assigned_unary_methods = [
        "assets.createUrl",
        "filesystem.browse",
        "projects.createEntry",
        "projects.deleteEntry",
        "projects.duplicateEntry",
        "projects.listEntries",
        "projects.readFile",
        "projects.renameEntry",
        "projects.searchEntries",
        "projects.writeFile",
        "review.getDiffPreview",
        "preview.close",
        "preview.list",
        "preview.navigate",
        "preview.open",
        "preview.refresh",
        "preview.reportStatus",
        "preview.resize",
        "previewAutomation.focusHost",
        "previewAutomation.respond",
    ];
    for (index, method) in assigned_unary_methods.into_iter().enumerate() {
        let response = request(&mut socket, &(100 + index).to_string(), method, json!({})).await;
        match response {
            ServerMessage::Exit {
                exit: RpcExit::Failure { cause },
                ..
            } => assert!(
                !cause.is_empty(),
                "{method} returned an empty failure cause"
            ),
            ServerMessage::Exit {
                exit: RpcExit::Success { .. },
                ..
            } => {}
            message => panic!("unexpected response for {method}: {message:?}"),
        }
    }

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn project_entries_stream_starts_with_a_resync_signal() {
    let temp = TempDir::new().expect("temporary base directory");
    tokio::fs::write(temp.path().join("existing.txt"), "hello")
        .await
        .expect("fixture");
    let mut registry = RpcRegistry::empty();
    register_workspace_preview_rpc(
        &mut registry,
        WorkspacePreviewRpcServices::new(
            WorkspaceRpc::new(WorkspaceService::default()),
            PreviewManager::new(),
            mcp::preview_automation::PreviewAutomationBroker::new(),
        ),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let cwd = path_string(temp.path());

    send_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": "11",
            "tag": "subscribeProjectEntries",
            "payload": { "cwd": cwd },
            "headers": []
        }),
    )
    .await;
    match next_server_message(&mut socket).await {
        ServerMessage::Chunk { values, .. } => {
            assert_eq!(values, vec![json!({ "cwd": cwd })]);
        }
        message => panic!("expected initial project-entry resync chunk, got {message:?}"),
    }

    send_json(
        &mut socket,
        serde_json::to_value(ClientMessage::Ack {
            request_id: RequestId::try_from("11").expect("request id"),
        })
        .expect("ack"),
    )
    .await;
    send_json(
        &mut socket,
        serde_json::to_value(ClientMessage::Interrupt {
            request_id: RequestId::try_from("11").expect("request id"),
        })
        .expect("interrupt"),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit { .. }
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn preview_event_stream_uses_contract_camel_case_keys() {
    let temp = TempDir::new().expect("temporary base directory");
    let preview = PreviewManager::new();
    let mut registry = RpcRegistry::empty();
    register_workspace_preview_rpc(
        &mut registry,
        WorkspacePreviewRpcServices::new(
            WorkspaceRpc::new(WorkspaceService::default()),
            preview.clone(),
            mcp::preview_automation::PreviewAutomationBroker::new(),
        ),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    send_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": "31",
            "tag": "subscribePreviewEvents",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    // A unary round trip after the subscribe request: the subscription is live once it answers.
    assert!(matches!(
        request(
            &mut socket,
            "32",
            "preview.list",
            json!({"threadId": "thread-1"})
        )
        .await,
        ServerMessage::Exit { .. }
    ));

    let opened = preview
        .open("thread-1", Some("http://localhost:5173"))
        .await
        .expect("open");
    preview
        .resize(
            "thread-1",
            &opened.tab_id,
            coddy_studio_server::preview::PreviewViewportSetting::Preset {
                preset_id: "iphone-se".to_owned(),
                width: 375,
                height: 667,
            },
        )
        .await
        .expect("resize");
    preview
        .request_open("thread-1", "http://localhost:5173/docs")
        .await
        .expect("request open");

    let mut values = Vec::new();
    while values.len() < 3 {
        match next_server_message(&mut socket).await {
            ServerMessage::Chunk { values: chunk, .. } => {
                values.extend(chunk);
                send_json(
                    &mut socket,
                    serde_json::to_value(ClientMessage::Ack {
                        request_id: RequestId::try_from("31").expect("request id"),
                    })
                    .expect("ack"),
                )
                .await;
            }
            message => panic!("expected a preview event chunk, got {message:?}"),
        }
    }
    assert_eq!(values[0]["type"], "opened");
    assert_eq!(values[0]["threadId"], "thread-1");
    assert_eq!(values[0]["tabId"], opened.tab_id.as_str());
    assert_eq!(values[1]["type"], "resized");
    assert_eq!(values[1]["snapshot"]["viewport"]["presetId"], "iphone-se");
    assert!(values[1]["snapshot"]["viewport"].get("preset_id").is_none());
    assert_eq!(values[2]["type"], "openRequested");
    assert_eq!(values[2]["threadId"], "thread-1");
    assert!(values[2]["requestId"].is_string());
    assert!(values[2]["createdAt"].is_string());

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn preview_automation_connect_stream_is_bounded_and_cancellable() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    register_workspace_preview_rpc(
        &mut registry,
        WorkspacePreviewRpcServices::new(
            WorkspaceRpc::new(WorkspaceService::default()),
            PreviewManager::new(),
            mcp::preview_automation::PreviewAutomationBroker::new(),
        ),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    send_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": "9",
            "tag": "previewAutomation.connect",
            "payload": {
                "clientId": "desktop-1",
                "environmentId": "local",
                "supportedOperations": ["open", "snapshot"]
            },
            "headers": []
        }),
    )
    .await;
    let connected = next_server_message(&mut socket).await;
    let connection_id = match connected {
        ServerMessage::Chunk { values, .. }
            if values.len() == 1 && values[0]["type"] == "connected" =>
        {
            values[0]["connectionId"]
                .as_str()
                .expect("connection id")
                .to_owned()
        }
        message => panic!("unexpected connect response: {message:?}"),
    };

    send_json(
        &mut socket,
        serde_json::to_value(ClientMessage::Ack {
            request_id: RequestId::try_from("9").expect("request id"),
        })
        .expect("ack"),
    )
    .await;
    let focused = request(
        &mut socket,
        "10",
        "previewAutomation.focusHost",
        json!({
            "clientId": "desktop-1",
            "environmentId": "local",
            "connectionId": connection_id,
            "focused": true
        }),
    )
    .await;
    assert!(matches!(
        focused,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    ));
    send_json(
        &mut socket,
        serde_json::to_value(ClientMessage::Interrupt {
            request_id: RequestId::try_from("9").expect("request id"),
        })
        .expect("interrupt"),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { .. },
            ..
        }
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}
