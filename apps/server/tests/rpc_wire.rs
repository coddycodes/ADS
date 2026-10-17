use std::{path::PathBuf, time::Duration};

use bibcode_server::{
    ACTIVE_RPC_METHODS, CauseItem, ClientMessage, MethodMode, RequestId, RpcExit, RpcRegistry,
    ServerConfig, ServerMessage, ServerRuntime, WireMessage,
};
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{process::Command, sync::mpsc, time::timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const HUGE_REQUEST_ID: &str = "900719925474099312345";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    methods: Vec<ManifestMethod>,
    stream_method_count: usize,
    expected_top_level_stream_shapes: usize,
    expected_orchestration_event_shapes: usize,
    stream_shape_fixtures: Vec<String>,
    typed_failure_fixtures: Vec<String>,
    stale_method_identifiers: Vec<String>,
    fixtures: Vec<String>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
struct ManifestMethod {
    name: String,
    mode: MethodMode,
}

#[test]
fn canonical_effect_fixtures_round_trip_without_losing_request_ids() {
    let fixture_directory = fixture_directory();
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(fixture_directory.join("manifest.json"))
            .expect("RPC manifest fixture"),
    )
    .expect("valid RPC manifest");

    for fixture in &manifest.fixtures {
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(fixture_directory.join(fixture)).expect("RPC fixture"),
        )
        .expect("valid RPC fixture");
        let decoded: WireMessage =
            serde_json::from_value(expected.clone()).expect("fixture decodes in Rust");
        let encoded = serde_json::to_value(decoded).expect("fixture re-encodes in Rust");
        assert_eq!(encoded, expected, "fixture mismatch: {fixture}");
    }

    let request: ClientMessage = serde_json::from_value(json!({
        "_tag": "Request",
        "id": HUGE_REQUEST_ID,
        "tag": "server.getConfig",
        "payload": {},
        "headers": []
    }))
    .expect("large request id");
    assert_eq!(
        request.request_id().map(RequestId::as_str),
        Some(HUGE_REQUEST_ID)
    );
}

#[test]
fn rust_registry_matches_the_active_typescript_rpc_group() {
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(fixture_directory().join("manifest.json"))
            .expect("RPC manifest fixture"),
    )
    .expect("valid RPC manifest");
    let rust_methods = ACTIVE_RPC_METHODS
        .iter()
        .map(|method| ManifestMethod {
            name: method.name.to_owned(),
            mode: method.mode,
        })
        .collect::<Vec<_>>();

    assert_eq!(rust_methods, manifest.methods);
    assert_eq!(rust_methods.len(), 151);
    let rust_stream_count = rust_methods
        .iter()
        .filter(|method| method.mode == MethodMode::Stream)
        .count();
    assert_eq!(rust_stream_count, 22);
    assert_eq!(rust_stream_count, manifest.stream_method_count);
    assert_eq!(manifest.expected_top_level_stream_shapes, 76);
    assert_eq!(manifest.expected_orchestration_event_shapes, 24);
    assert_eq!(manifest.stream_shape_fixtures.len(), 76);
    assert_eq!(manifest.typed_failure_fixtures.len(), 325);
    assert_eq!(
        manifest.stale_method_identifiers,
        ["projects.add", "projects.list", "projects.remove"]
    );
}

#[test]
fn request_ids_reject_non_decimal_or_empty_values() {
    for invalid in ["", "-1", "+1", "1.0", " 1", "abc"] {
        assert!(
            RequestId::try_from(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[test]
fn chunks_reject_empty_value_arrays() {
    let error = serde_json::from_value::<ServerMessage>(json!({
        "_tag": "Chunk",
        "requestId": "1",
        "values": []
    }))
    .expect_err("Effect chunks are non-empty");

    assert!(error.to_string().contains("at least one value"));
}

#[tokio::test]
async fn websocket_handles_ping_and_lossless_unary_requests() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_unary("fixture.echo", |request, _cancellation| async move {
        Ok(request.payload)
    });
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    send_json(&mut socket, json!({ "_tag": "Ping" })).await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Pong
    ));

    send_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": HUGE_REQUEST_ID,
            "tag": "fixture.echo",
            "payload": { "value": "echo" },
            "headers": []
        }),
    )
    .await;
    let response = next_server_message(&mut socket).await;
    assert_eq!(
        response.request_id().map(RequestId::as_str),
        Some(HUGE_REQUEST_ID)
    );
    assert!(matches!(
        response,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } if value == json!({ "value": "echo" })
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn unchanged_typescript_effect_client_calls_the_rust_server() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_unary("filesystem.browse", |_request, _cancellation| async move {
        Ok(json!({
            "parentPath": "C:\\fixture",
            "entries": []
        }))
    });
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("packages/client-runtime/scripts/rust-rpc-compat-smoke.ts");

    let status = Command::new("node")
        .arg(script)
        .arg(format!("ws://{}/ws", handle.local_addr()))
        .current_dir(&workspace_root)
        .status()
        .await
        .expect("launch unchanged TypeScript client");

    assert!(status.success(), "TypeScript Effect client exited {status}");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn websocket_accepts_batched_client_messages() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_unary("fixture.echo", |request, _cancellation| async move {
        Ok(request.payload)
    });
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    send_json(
        &mut socket,
        json!([
            { "_tag": "Ping" },
            {
                "_tag": "Request",
                "id": "8",
                "tag": "fixture.echo",
                "payload": { "batched": true },
                "headers": []
            }
        ]),
    )
    .await;

    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Pong
    ));
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } if value == json!({ "batched": true })
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn malformed_frames_and_unknown_methods_return_effect_errors() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(test_config(&temp), RpcRegistry::empty())
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    socket
        .send(Message::Text("not-json".into()))
        .await
        .expect("send malformed frame");
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::ClientProtocolError { .. }
    ));

    send_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": "9",
            "tag": "fixture.unknown",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Defect { defect }
            if defect == json!("Unknown request tag: fixture.unknown")
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stream_waits_for_one_ack_per_chunk() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_stream("fixture.stream", |_request, _cancellation| {
        let (sender, receiver) = mpsc::channel(1);
        sender
            .try_send(Ok(vec![json!({ "sequence": 1 }), json!({ "sequence": 2 })]))
            .expect("batched items");
        receiver
    });
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
            "id": "42",
            "tag": "fixture.stream",
            "payload": {},
            "headers": []
        }),
    )
    .await;

    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Chunk { values, .. }
            if values == vec![json!({ "sequence": 1 }), json!({ "sequence": 2 })]
    ));
    assert!(
        timeout(Duration::from_millis(100), next_server_message(&mut socket))
            .await
            .is_err(),
        "stream completed before the batched Chunk was acknowledged"
    );
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "42" })).await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: None },
            ..
        }
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn eof_still_allows_stream_acknowledgements_before_shutdown() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_stream("fixture.stream", |_request, _cancellation| {
        let (sender, receiver) = mpsc::channel(2);
        sender.try_send(Ok(vec![json!(1)])).expect("first item");
        sender.try_send(Ok(vec![json!(2)])).expect("second item");
        receiver
    });
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
            "id": "10",
            "tag": "fixture.stream",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    let ServerMessage::Chunk { mut values, .. } = next_server_message(&mut socket).await else {
        panic!("the stream starts with a chunk");
    };
    send_json(&mut socket, json!({ "_tag": "Eof" })).await;
    // Every Ack after the Eof still lets the stream go on; queued items may share a chunk.
    loop {
        send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "10" })).await;
        match next_server_message(&mut socket).await {
            ServerMessage::Chunk { values: more, .. } => values.extend(more),
            ServerMessage::Exit {
                exit: RpcExit::Success { value: None },
                ..
            } => break,
            other => panic!("unexpected message {other:?}"),
        }
    }
    assert_eq!(values, vec![json!(1), json!(2)]);

    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn panicking_handlers_return_a_connection_defect_without_leaking_the_request() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_unary("fixture.panic", |_request, _cancellation| async move {
        panic!("fixture panic")
    });
    registry.register_unary("fixture.echo", |request, _cancellation| async move {
        Ok(request.payload)
    });
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
            "id": "11",
            "tag": "fixture.panic",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Defect { defect }
            if defect.as_str().is_some_and(|message| message.contains("fixture panic"))
    ));

    send_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": "12",
            "tag": "fixture.echo",
            "payload": { "released": true },
            "headers": []
        }),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } if value == json!({ "released": true })
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn graceful_shutdown_cancels_open_websocket_sessions() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(test_config(&temp), RpcRegistry::empty())
        .await
        .expect("server starts");
    let (_socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("server shutdown must not wait for an idle WebSocket")
        .expect("server joins");
}

#[tokio::test]
async fn interrupt_cancels_an_in_flight_request_and_returns_an_interrupt_exit() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut registry = RpcRegistry::empty();
    registry.register_unary("fixture.pending", |_request, cancellation| async move {
        cancellation.cancelled().await;
        Ok(Value::Null)
    });
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
            "id": "77",
            "tag": "fixture.pending",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    send_json(
        &mut socket,
        json!({ "_tag": "Interrupt", "requestId": "77" }),
    )
    .await;

    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause == vec![CauseItem::Interrupt { fiber_id: None }]
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

fn test_config(temp: &TempDir) -> ServerConfig {
    ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts/fixtures/rpc-wire")
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

async fn connect_plain(
    address: std::net::SocketAddr,
    chunked: bool,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Option<String>,
) {
    let mut request = format!("ws://{address}/ws")
        .into_client_request()
        .expect("WebSocket request");
    if chunked {
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static("bibcode.rpc.chunked.v1"),
        );
    }
    let (socket, response) = connect_async(request).await.expect("WebSocket connects");
    let selected = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (socket, selected)
}

fn bytes_registry() -> RpcRegistry {
    let mut registry = RpcRegistry::empty();
    registry.register_unary("fixture.bytes", |request, _cancellation| async move {
        let bytes = request.payload["bytes"]
            .as_u64()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .unwrap_or_default();
        Ok(json!({ "data": "x".repeat(bytes) }))
    });
    registry
}

#[tokio::test]
async fn chunked_subprotocol_splits_large_responses_into_records() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(test_config(&temp), bytes_registry())
        .await
        .expect("server starts");
    let (mut socket, selected) = connect_plain(handle.local_addr(), true).await;
    assert_eq!(selected.as_deref(), Some("bibcode.rpc.chunked.v1"));

    send_json(
        &mut socket,
        json!({ "_tag": "Request", "id": "1", "tag": "fixture.bytes", "payload": { "bytes": 1024 }, "headers": [] }),
    )
    .await;
    let small = timeout(
        Duration::from_secs(2),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("small response")
    .expect("socket open")
    .expect("frame");
    assert!(
        matches!(small, Message::Text(_)),
        "small messages stay whole text frames"
    );

    send_json(
        &mut socket,
        json!({ "_tag": "Request", "id": "2", "tag": "fixture.bytes", "payload": { "bytes": 200 * 1024 }, "headers": [] }),
    )
    .await;
    let mut flags = Vec::new();
    let mut body = Vec::new();
    loop {
        let frame = timeout(
            Duration::from_secs(2),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        .expect("record")
        .expect("socket open")
        .expect("frame");
        let Message::Binary(record) = frame else {
            panic!("large messages arrive as binary records, got {frame:?}");
        };
        flags.push(record[0]);
        body.extend_from_slice(&record[1..]);
        if record[0] == 0x00 {
            break;
        }
    }
    assert!(flags.len() >= 4);
    assert!(flags[..flags.len() - 1].iter().all(|flag| *flag == 0x01));
    let response: Value = serde_json::from_slice(&body).expect("records reassemble");
    assert_eq!(response["requestId"], "2");
    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn clients_without_the_subprotocol_receive_whole_text_frames() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(test_config(&temp), bytes_registry())
        .await
        .expect("server starts");
    let (mut socket, selected) = connect_plain(handle.local_addr(), false).await;
    assert_eq!(selected, None);
    send_json(
        &mut socket,
        json!({ "_tag": "Request", "id": "1", "tag": "fixture.bytes", "payload": { "bytes": 200 * 1024 }, "headers": [] }),
    )
    .await;
    let frame = timeout(
        Duration::from_secs(2),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("response")
    .expect("socket open")
    .expect("frame");
    assert!(matches!(frame, Message::Text(text) if text.len() > 200 * 1024));
    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}
