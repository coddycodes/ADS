//! `device.*` RPC through the full production runtime. With `enableDevicePreviews` off (the
//! default), every method is refused before the device manager looks for an Android SDK, so
//! this stays hermetic. The enabled paths run against a fake SDK and emulator in
//! `production::device_rpc`'s unit tests.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

use std::time::Duration;

use bibcode_server::{ServerConfig, ServerRuntime};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[tokio::test]
async fn device_methods_are_refused_while_device_previews_are_off() {
    let temp = TempDir::new().expect("server state");
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    let handle = ServerRuntime::start(config).await.expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    for (id, method, payload) in [
        (
            "1",
            "device.open",
            json!({ "platform": "android", "imageId": "Pixel" }),
        ),
        ("2", "device.subscribeCatalog", json!({})),
        ("3", "device.subscribeFrames", json!({ "deviceId": "d" })),
        ("4", "device.close", json!({ "deviceId": "d" })),
        (
            "5",
            "device.input",
            json!({ "deviceId": "d", "events": [{ "type": "key", "key": "home", "action": "down" }] }),
        ),
        (
            "6",
            "device.rotate",
            json!({ "deviceId": "d", "orientation": "landscape" }),
        ),
        ("7", "device.createMediaTicket", json!({ "deviceId": "d" })),
    ] {
        socket
            .send(Message::Text(
                json!({ "_tag": "Request", "id": id, "tag": method, "payload": payload, "headers": [] })
                    .to_string()
                    .into(),
            ))
            .await
            .expect("send request");
        let message = loop {
            let frame = tokio::time::timeout(Duration::from_secs(20), socket.next())
                .await
                .expect("response within 20 s")
                .expect("socket open")
                .expect("frame");
            if let Message::Text(text) = frame {
                let message: Value = serde_json::from_str(&text).expect("response JSON");
                if message["requestId"] == id {
                    break message;
                }
            }
        };
        assert_eq!(message["_tag"], "Exit", "{method}: {message}");
        assert_eq!(message["exit"]["_tag"], "Failure", "{method}: {message}");
        let error = &message["exit"]["cause"][0]["error"];
        assert_eq!(error["_tag"], "DevicePreviewsDisabledError", "{method}");
        assert!(
            error["message"].as_str().unwrap().contains("Settings"),
            "{method}: {error}"
        );
    }

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}
