//! `GET /api/device-media` through the full production runtime. The socket admits only a
//! ticket sent as its first message; a URL query or cookie is never read. Paths that need a
//! running device (streaming, `reused`, `limit`, `disabled`) run against a fake SDK and
//! scrcpy-server in `device::media_socket`'s unit tests.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

use std::time::{Duration, Instant};

use bibcode_server::{ServerConfig, ServerHandle, ServerRuntime};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

async fn start() -> (ServerHandle, TempDir) {
    let temp = TempDir::new().expect("server state");
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    let handle = ServerRuntime::start(config).await.expect("server starts");
    (handle, temp)
}

/// Reads until the server closes; the close code and reason, or `None` when the connection
/// ended without a close frame.
async fn closed(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<(u16, String)> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(15), socket.next())
            .await
            .expect("the server closes within 15 s");
        match message {
            Some(Ok(Message::Close(frame))) => {
                return frame.map(|frame| (u16::from(frame.code), frame.reason.to_string()));
            }
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

#[tokio::test]
async fn ticket_in_query_or_cookie_is_not_accepted() {
    let (handle, _temp) = start().await;
    let mut request = format!(
        "ws://{}/api/device-media?ticket=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        handle.local_addr()
    )
    .into_client_request()
    .expect("request");
    request.headers_mut().insert(
        "cookie",
        HeaderValue::from_static("ticket=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
    );
    let (mut socket, _) = connect_async(request).await.expect("media socket upgrades");
    let started = Instant::now();
    assert_eq!(
        closed(&mut socket).await,
        Some((4400, "expired".to_owned()))
    );
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "the socket waited for a ticket message: {:?}",
        started.elapsed()
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn unknown_ticket_closes_as_expired() {
    let (handle, _temp) = start().await;
    let (mut socket, _) = connect_async(format!("ws://{}/api/device-media", handle.local_addr()))
        .await
        .expect("media socket upgrades");
    socket
        .send(Message::Text(
            r#"{"type":"ticket","ticket":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#.into(),
        ))
        .await
        .expect("send ticket");
    assert_eq!(
        closed(&mut socket).await,
        Some((4400, "expired".to_owned()))
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn oversized_first_message_is_refused() {
    let (handle, _temp) = start().await;
    let (mut socket, _) = connect_async(format!("ws://{}/api/device-media", handle.local_addr()))
        .await
        .expect("media socket upgrades");
    let started = Instant::now();
    socket
        .send(Message::Text("x".repeat(64 * 1024).into()))
        .await
        .expect("send oversized message");
    // Refused at the frame limit, without waiting out the ticket timeout.
    let _ = closed(&mut socket).await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn connections_beyond_the_preauth_limit_are_refused_before_upgrade() {
    let (handle, _temp) = start().await;
    let request = || {
        let mut request = format!("ws://{}/api/device-media", handle.local_addr())
            .into_client_request()
            .expect("request");
        // As forwarded by a proxy on this machine for one public client.
        request
            .headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.5"));
        request
    };
    // Each holds its pre-auth slot while it waits to send a ticket.
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(connect_async(request()).await.expect("admitted").0);
    }
    match connect_async(request()).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 503);
        }
        other => panic!("expected a refusal before the upgrade, got {other:?}"),
    }
    drop(held);
    handle.shutdown();
    handle.join().await.expect("server joins");
}
