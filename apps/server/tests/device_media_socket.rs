//! `GET /api/device-media` and `GET /api/device-media-e2ee` through the full production
//! runtime, with device previews on unless a test says otherwise. Test builds find no Android
//! SDK (`SdkEnv::from_process` under `hermetic-test-guard`), so no real emulator runs. The
//! sockets admit only a ticket sent as their first message; a URL query or cookie is never
//! read. Paths that need a running device or a minted ticket (streaming, `reused`,
//! `wrongChannel`, `limit`, `disabled`, E2EE streaming) run against a fake SDK and
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

const NOISE_NK_PARAMS: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
const MAX_CIPHERTEXT_BYTES: usize = 65_535;
const RECORD_FINAL: u8 = 0;
const RECORD_CONTINUATION: u8 = 1;
/// The media route's Noise prologue; the TS client sends the same bytes.
const MEDIA_PROLOGUE: &[u8] = b"bibcode-device-media/1";

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start() -> (ServerHandle, TempDir) {
    start_with_previews(true).await
}

async fn start_with_previews(enabled: bool) -> (ServerHandle, TempDir) {
    let temp = TempDir::new().expect("server state");
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        serde_json::json!({ "enableDevicePreviews": enabled }),
    );
    let handle = ServerRuntime::start(config).await.expect("server starts");
    (handle, temp)
}

#[tokio::test]
async fn both_routes_refuse_before_upgrade_while_previews_are_off() {
    let (handle, _temp) = start_with_previews(false).await;
    for route in ["device-media", "device-media-e2ee"] {
        match connect_async(format!("ws://{}/api/{route}", handle.local_addr())).await {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), 503, "{route}");
            }
            other => panic!("expected a refusal before the upgrade, got {other:?}"),
        }
    }
    handle.shutdown();
    handle.join().await.expect("server joins");
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

fn host_public_key(root: &std::path::Path) -> Vec<u8> {
    let record = std::fs::read(
        root.join("userdata")
            .join("secrets")
            .join("host-identity-x25519.bin"),
    )
    .expect("persisted host identity");
    record[32..].to_vec()
}

/// Opens the E2EE media route and sends Noise_NK message A pinned to `host_key`.
async fn send_message_a(handle: &ServerHandle, host_key: &[u8]) -> (Socket, snow::HandshakeState) {
    send_message_a_to(handle, "/api/device-media-e2ee", host_key, MEDIA_PROLOGUE).await
}

/// Opens `path` and sends Noise_NK message A pinned to `host_key` under `prologue`.
async fn send_message_a_to(
    handle: &ServerHandle,
    path: &str,
    host_key: &[u8],
    prologue: &[u8],
) -> (Socket, snow::HandshakeState) {
    let (mut socket, _) = connect_async(format!("ws://{}{path}", handle.local_addr()))
        .await
        .expect("socket upgrades");
    let mut initiator = snow::Builder::new(NOISE_NK_PARAMS.parse().expect("Noise parameters"))
        .remote_public_key(host_key)
        .expect("host key")
        .prologue(prologue)
        .expect("prologue")
        .build_initiator()
        .expect("Noise initiator");
    let mut message_a = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    let len = initiator
        .write_message(&[], &mut message_a)
        .expect("message A");
    message_a.truncate(len);
    socket
        .send(Message::Binary(message_a.into()))
        .await
        .expect("send message A");
    (socket, initiator)
}

/// Completes the handshake with the server's own key.
async fn handshake(
    handle: &ServerHandle,
    root: &std::path::Path,
) -> (Socket, snow::TransportState) {
    let (mut socket, mut initiator) = send_message_a(handle, &host_public_key(root)).await;
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("message B within 5 s");
    let Some(Ok(Message::Binary(message_b))) = message else {
        panic!("expected message B, got {message:?}");
    };
    let mut payload = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    assert_eq!(
        initiator
            .read_message(&message_b, &mut payload)
            .expect("message B"),
        0
    );
    (
        socket,
        initiator.into_transport_mode().expect("transport mode"),
    )
}

async fn send_record(
    socket: &mut Socket,
    transport: &mut snow::TransportState,
    flag: u8,
    chunk: &[u8],
) {
    let mut record = vec![flag];
    record.extend_from_slice(chunk);
    let mut frame = vec![0_u8; record.len() + 16];
    let len = transport
        .write_message(&record, &mut frame)
        .expect("encrypt record");
    frame.truncate(len);
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("send record");
}

#[tokio::test]
async fn e2ee_wrong_host_key_fails_the_handshake() {
    let (handle, _temp) = start().await;
    let other = snow::Builder::new(NOISE_NK_PARAMS.parse().expect("Noise parameters"))
        .generate_keypair()
        .expect("another key pair");
    let (mut socket, _) = send_message_a(&handle, &other.public).await;
    assert_eq!(
        closed(&mut socket).await,
        Some((4403, "host-identity".to_owned()))
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn e2ee_unknown_ticket_closes_as_expired() {
    let (handle, temp) = start().await;
    let (mut socket, mut transport) = handshake(&handle, temp.path()).await;
    send_record(
        &mut socket,
        &mut transport,
        RECORD_FINAL,
        br#"{"type":"ticket","ticket":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
    )
    .await;
    assert_eq!(
        closed(&mut socket).await,
        Some((4400, "expired".to_owned()))
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn e2ee_oversized_first_message_is_refused() {
    let (handle, temp) = start().await;
    let (mut socket, mut transport) = handshake(&handle, temp.path()).await;
    let started = Instant::now();
    // Each record fits the socket's frame limit; together they exceed the 1 KiB message cap.
    send_record(
        &mut socket,
        &mut transport,
        RECORD_CONTINUATION,
        &[b'x'; 600],
    )
    .await;
    send_record(&mut socket, &mut transport, RECORD_FINAL, &[b'x'; 600]).await;
    // Dropped at the record layer, not read as a (malformed) ticket.
    assert_eq!(closed(&mut socket).await, None);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn e2ee_handshake_times_out() {
    let (handle, _temp) = start().await;
    let (mut socket, _) = connect_async(format!(
        "ws://{}/api/device-media-e2ee",
        handle.local_addr()
    ))
    .await
    .expect("E2EE media socket upgrades");
    let started = Instant::now();
    let _ = closed(&mut socket).await;
    assert!(
        (Duration::from_secs(9)..Duration::from_secs(11)).contains(&started.elapsed()),
        "{:?}",
        started.elapsed()
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn both_media_routes_share_one_preauth_pool() {
    let (handle, temp) = start().await;
    let request = |path: &str| {
        let mut request = format!("ws://{}{path}", handle.local_addr())
            .into_client_request()
            .expect("request");
        request
            .headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("203.0.113.5"));
        request
    };
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(
            connect_async(request("/api/device-media"))
                .await
                .expect("admitted")
                .0,
        );
    }
    match connect_async(request("/api/device-media-e2ee")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 503);
        }
        other => panic!("expected a refusal before the upgrade, got {other:?}"),
    }
    // RPC handshakes draw from their own pool: `/ws-e2ee` still answers message A.
    let (mut rpc, _) = connect_async(request("/ws-e2ee"))
        .await
        .expect("/ws-e2ee upgrades");
    let mut initiator = snow::Builder::new(NOISE_NK_PARAMS.parse().expect("Noise parameters"))
        .remote_public_key(&host_public_key(temp.path()))
        .expect("host key")
        .build_initiator()
        .expect("Noise initiator");
    let mut message_a = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    let len = initiator
        .write_message(&[], &mut message_a)
        .expect("message A");
    message_a.truncate(len);
    rpc.send(Message::Binary(message_a.into()))
        .await
        .expect("send message A");
    let answer = tokio::time::timeout(Duration::from_secs(5), rpc.next())
        .await
        .expect("an answer within 5 s");
    assert!(matches!(answer, Some(Ok(Message::Binary(_)))), "{answer:?}");
    drop(held);
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn media_prologue_fails_on_ws_e2ee() {
    let (handle, temp) = start().await;
    let (mut socket, _) = send_message_a_to(
        &handle,
        "/ws-e2ee",
        &host_public_key(temp.path()),
        MEDIA_PROLOGUE,
    )
    .await;
    assert_eq!(
        closed(&mut socket).await,
        Some((4403, "host-identity".to_owned()))
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}
