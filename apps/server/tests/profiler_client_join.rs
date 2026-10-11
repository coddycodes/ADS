//! A client report through a real session with the profiler on: the request's correlation
//! header puts it in the join log, and the batch that names it raises a finding with the
//! server's share of the wait. The profiler's handle is process-global, so this binary runs
//! one production server.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;
#[path = "support/websocket_frames.rs"]
mod websocket_frames;

use std::time::Duration;

use bibcode_server::{RpcExit, ServerConfig, ServerMessage, ServerRuntime};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use websocket_frames::next_frame_past_heartbeat;

type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[tokio::test]
async fn a_client_batch_is_joined_to_a_request_it_made_in_the_same_session() {
    let temp = TempDir::new().expect("temporary directory");
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({ "enableProfiler": true }),
    );
    let handle = ServerRuntime::start(config)
        .await
        .expect("production server starts");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects")
        .0;
    let mut ids = 1_u32..;
    let mut id = move || ids.next().expect("ids never run out").to_string();

    let mut status = Value::Null;
    for _ in 0..100 {
        status = success(
            request(
                &mut socket,
                &id(),
                "server.getProfilerStatus",
                json!({}),
                &[],
            )
            .await,
        );
        if status["enabled"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status["enabled"], true, "the saved setting turns it on");

    // The collector's correlation header joins this request to the server's timing.
    success(
        request(
            &mut socket,
            &id(),
            "server.getConfig",
            json!({}),
            &[("x-bibcode-profiler-rid", "itest:1")],
        )
        .await,
    );
    let batch = json!({
        "clientId": "itest",
        "capabilities": ["heartbeat"],
        "windowMs": 10_000,
        "records": [{
            "correlationId": "itest:1",
            "method": "server.getConfig",
            "rttMs": 1_500.0,
            "renderMs": 10.0,
            "startedAtMs": 1.0,
        }],
        "histograms": [],
        "stalls": { "count": 0, "totalMs": 0, "maxMs": 0 },
        "interactions": { "count": 0, "maxMs": 0 },
        "jsHeapBytes": null,
    });
    let answer = success(
        request(
            &mut socket,
            &id(),
            "server.reportProfilerClientBatch",
            batch.clone(),
            &[],
        )
        .await,
    );
    assert_eq!(answer, json!({ "accepted": true }));
    // The same client again at once is refused, with nothing written.
    let again = success(
        request(
            &mut socket,
            &id(),
            "server.reportProfilerClientBatch",
            batch,
            &[],
        )
        .await,
    );
    assert_eq!(
        again,
        json!({ "accepted": false, "reason": "too-frequent" })
    );

    let mut slow = None;
    for _ in 0..100 {
        let status = success(
            request(
                &mut socket,
                &id(),
                "server.getProfilerStatus",
                json!({}),
                &[],
            )
            .await,
        );
        slow = status["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|finding| finding["kind"] == "rpc.slow" && finding["source"] == "client")
            .cloned();
        if slow.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let slow = slow.expect("a client rpc.slow finding");
    assert_eq!(slow["count"], 1, "the refused batch recorded nothing");
    let summary = slow["summary"].as_str().unwrap();
    // Joined: the wait splits into the server's part and the network's.
    assert!(
        summary.contains("server ") && summary.contains("network "),
        "{summary}"
    );

    let _ = socket.close(None).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
}

async fn request(
    socket: &mut TestSocket,
    id: &str,
    tag: &str,
    payload: Value,
    headers: &[(&str, &str)],
) -> ServerMessage {
    let message = json!({
        "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": headers,
    });
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .expect("request sends");
    let reply = tokio::time::timeout(Duration::from_secs(10), next_frame_past_heartbeat(socket))
        .await
        .unwrap_or_else(|_| panic!("no {tag} response"))
        .expect("socket remains open")
        .expect("valid socket message");
    let Message::Text(text) = reply else {
        panic!("expected a text message")
    };
    serde_json::from_str(&text).expect("valid server message")
}

fn success(message: ServerMessage) -> Value {
    match message {
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } => value,
        other => panic!("expected a successful RPC response, got {other:?}"),
    }
}
