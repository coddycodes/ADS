use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bibcode_server::{ServerConfig, ServerHandle, ServerRuntime};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use rusqlite::Connection;
use serde_json::{Value, json};
use snow::TransportState;
use tempfile::TempDir;
use tokio::{
    net::{TcpSocket, TcpStream},
    sync::Semaphore,
    time::timeout,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;
#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

const NOISE_NK_PARAMS: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
const MAX_CIPHERTEXT_BYTES: usize = 65_535;
const MAX_CHUNK_BYTES: usize = 65_518;
const RECORD_FINAL: u8 = 0;
const RECORD_CONTINUATION: u8 = 1;
/// Established buckets stop growing at the server's pre-auth soft cap; the
/// slots above it are reserved for fresh buckets' first connections.
const E2EE_PREAUTH_GLOBAL_SOFT_CAP: usize = 24;
const E2EE_MAX_ESTABLISHED_CONNECTIONS_PER_PRINCIPAL: usize = 32;
const E2EE_INBOUND_BUFFER_BUDGET_BYTES_PER_PRINCIPAL: usize = 64 * 1024 * 1024;
const TOKEN_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:bibcode:params:oauth:token-type:environment-bootstrap";

static TEST_PERMIT: Semaphore = Semaphore::const_new(1);

type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start_server(temp: &TempDir) -> ServerHandle {
    let config = ServerConfig::new(temp.path()).with_bind("127.0.0.1", 0);
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
    ServerRuntime::start(config).await.expect("server starts")
}

fn ws_url(address: SocketAddr, path: &str) -> String {
    format!("ws://{address}{path}")
}

fn http_url(handle: &ServerHandle, path: &str) -> String {
    format!("http://{}{}", handle.local_addr(), path)
}

fn read_host_public_key(root: &Path) -> Vec<u8> {
    let record = std::fs::read(
        root.join("userdata")
            .join("secrets")
            .join("host-identity-x25519.bin"),
    )
    .expect("persisted host identity");
    assert_eq!(record.len(), 64);
    record[32..].to_vec()
}

async fn open_socket(address: SocketAddr) -> TestSocket {
    connect_async(ws_url(address, "/ws-e2ee"))
        .await
        .expect("open /ws-e2ee")
        .0
}

async fn open_socket_from(address: SocketAddr, source_ip: Ipv4Addr) -> TestSocket {
    let tcp = TcpSocket::new_v4().expect("IPv4 client socket");
    tcp.bind(SocketAddr::new(IpAddr::V4(source_ip), 0))
        .expect("bind loopback source address");
    let stream = tcp.connect(address).await.expect("connect from source IP");
    client_async(
        ws_url(address, "/ws-e2ee").into_client_request().unwrap(),
        MaybeTlsStream::Plain(stream),
    )
    .await
    .expect("open /ws-e2ee from source IP")
    .0
}

fn initiator(host_key: &[u8]) -> snow::HandshakeState {
    snow::Builder::new(NOISE_NK_PARAMS.parse().expect("Noise parameters"))
        .remote_public_key(host_key)
        .expect("host key")
        .build_initiator()
        .expect("Noise initiator")
}

async fn noise_connect(address: SocketAddr, host_key: &[u8]) -> (TestSocket, TransportState) {
    noise_connect_on(open_socket(address).await, host_key).await
}

async fn noise_connect_from(
    address: SocketAddr,
    host_key: &[u8],
    source_ip: Ipv4Addr,
) -> (TestSocket, TransportState) {
    noise_connect_on(open_socket_from(address, source_ip).await, host_key).await
}

async fn noise_connect_on(mut socket: TestSocket, host_key: &[u8]) -> (TestSocket, TransportState) {
    let mut initiator = initiator(host_key);
    let mut message_a = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    let len = initiator
        .write_message(&[], &mut message_a)
        .expect("write message A");
    message_a.truncate(len);
    socket
        .send(Message::Binary(message_a.into()))
        .await
        .expect("send message A");
    let frame = timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("message B timeout")
        .expect("socket remains open")
        .expect("message B frame");
    let Message::Binary(message_b) = frame else {
        panic!("expected binary message B, got {frame:?}");
    };
    let mut payload = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    assert_eq!(
        initiator
            .read_message(&message_b, &mut payload)
            .expect("read message B"),
        0
    );
    (
        socket,
        initiator
            .into_transport_mode()
            .expect("Noise transport mode"),
    )
}

fn encrypt_record(transport: &mut TransportState, flag: u8, chunk: &[u8]) -> Vec<u8> {
    let mut record = Vec::with_capacity(chunk.len() + 1);
    record.push(flag);
    record.extend_from_slice(chunk);
    let mut frame = vec![0_u8; record.len() + 16];
    let len = transport
        .write_message(&record, &mut frame)
        .expect("encrypt record");
    frame.truncate(len);
    frame
}

async fn send_encrypted(socket: &mut TestSocket, transport: &mut TransportState, plaintext: &[u8]) {
    let mut chunks = plaintext.chunks(MAX_CHUNK_BYTES).peekable();
    if chunks.peek().is_none() {
        let frame = encrypt_record(transport, RECORD_FINAL, &[]);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .expect("send empty encrypted message");
        return;
    }
    while let Some(chunk) = chunks.next() {
        let flag = if chunks.peek().is_some() {
            RECORD_CONTINUATION
        } else {
            RECORD_FINAL
        };
        let frame = encrypt_record(transport, flag, chunk);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .expect("send encrypted record");
    }
}

async fn send_encrypted_continuations(
    socket: &mut TestSocket,
    transport: &mut TransportState,
    bytes: usize,
) {
    let chunk = vec![b'x'; MAX_CHUNK_BYTES];
    let mut remaining = bytes;
    while remaining > 0 {
        let length = remaining.min(chunk.len());
        let frame = encrypt_record(transport, RECORD_CONTINUATION, &chunk[..length]);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .expect("send encrypted continuation");
        remaining -= length;
    }
}

/// Waits until the server has read every frame sent on `socket` before this call. The server
/// reads a socket's frames in order and takes each record's inbound budget before it reads the
/// next, so the pong to a ping sent now proves the earlier records hold their budget.
async fn wait_for_server_intake(socket: &mut TestSocket) {
    const PROBE: &[u8] = b"intake probe";
    socket
        .send(Message::Ping(PROBE.to_vec().into()))
        .await
        .expect("send intake probe");
    timeout(Duration::from_secs(10), async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Pong(payload))) if payload.as_ref() == PROBE => return,
                Some(Ok(_)) => {}
                other => panic!("socket ended before the intake probe's pong: {other:?}"),
            }
        }
    })
    .await
    .expect("the server reads the earlier frames");
}

async fn recv_encrypted(socket: &mut TestSocket, transport: &mut TransportState) -> Vec<u8> {
    let mut assembled = Vec::new();
    loop {
        let frame = timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("encrypted message timeout")
            .expect("socket remains open")
            .expect("encrypted frame");
        match frame {
            Message::Binary(frame) => {
                let mut record = vec![0_u8; MAX_CIPHERTEXT_BYTES];
                let len = transport
                    .read_message(&frame, &mut record)
                    .expect("decrypt record");
                assert!(len > 0, "record contains a flag byte");
                assembled.extend_from_slice(&record[1..len]);
                match record[0] {
                    RECORD_FINAL => return assembled,
                    RECORD_CONTINUATION => {}
                    flag => panic!("unknown record flag {flag}"),
                }
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected encrypted binary frame, got {other:?}"),
        }
    }
}

async fn recv_encrypted_json(socket: &mut TestSocket, transport: &mut TransportState) -> Value {
    serde_json::from_slice(&recv_encrypted(socket, transport).await)
        .expect("encrypted JSON message")
}

async fn pair_inside_channel(
    handle: &ServerHandle,
    root: &Path,
    pairing: &str,
) -> (TestSocket, TransportState, Value) {
    let host_key = read_host_public_key(root);
    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    let auth = json!({ "type": "e2ee_auth", "pairing": pairing });
    send_encrypted(&mut socket, &mut transport, auth.to_string().as_bytes()).await;
    let reply = recv_encrypted_json(&mut socket, &mut transport).await;
    (socket, transport, reply)
}

async fn mint_e2ee_credential(handle: &ServerHandle, root: &Path, pairing: &str) -> String {
    let (mut socket, mut transport, reply) = pair_inside_channel(handle, root, pairing).await;
    let credential = reply["credential"]
        .as_str()
        .expect("minted E2EE credential")
        .to_owned();
    // Mirror the client contract: confirm only when the server says the
    // delivery is guarded.
    if reply["pairingConfirmationRequired"] == Value::Bool(true) {
        confirm_pairing(&mut socket, &mut transport, "2").await;
    }
    socket.close(None).await.expect("close pairing socket");
    credential
}

async fn confirm_pairing(
    socket: &mut TestSocket,
    transport: &mut TransportState,
    request_id: &str,
) {
    send_encrypted(
        socket,
        transport,
        json!({
            "_tag": "Request",
            "id": request_id,
            "tag": "auth.confirmPairing",
            "payload": {},
            "headers": []
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    let response = recv_encrypted_json(socket, transport).await;
    assert_eq!(response["_tag"], "Exit");
    assert_eq!(response["requestId"], request_id);
    assert_eq!(response["exit"]["_tag"], "Success", "{response}");
    assert_eq!(response["exit"]["value"], json!({}));
}

fn latest_auth_session_delivery_state(root: &Path) -> String {
    Connection::open(root.join("userdata/state.sqlite"))
        .expect("open server database")
        .query_row(
            "SELECT delivery_state FROM auth_sessions ORDER BY issued_at DESC, session_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("latest auth session delivery state")
}

fn latest_off_host_auth_session_delivery_state(root: &Path) -> String {
    Connection::open(root.join("userdata/state.sqlite"))
        .expect("open server database")
        .query_row(
            "SELECT delivery_state FROM auth_sessions \
             WHERE reach = 'another-device' AND off_host = 1 \
             ORDER BY issued_at DESC, session_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("latest off-host auth session delivery state")
}

fn latest_off_host_auth_session_delivery_state_row(root: &Path) -> Option<(String, bool)> {
    Connection::open(root.join("userdata/state.sqlite"))
        .expect("open server database")
        .query_row(
            "SELECT delivery_state, revoked_at IS NOT NULL FROM auth_sessions \
             WHERE reach = 'another-device' AND off_host = 1 \
             ORDER BY issued_at DESC, session_id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()
}

async fn open_authenticated_bearer_socket(
    handle: &ServerHandle,
    host_key: &[u8],
    credential: &str,
) -> (TestSocket, TransportState, Value) {
    let (mut socket, mut transport) = noise_connect(handle.local_addr(), host_key).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "type": "e2ee_auth", "bearer": credential })
            .to_string()
            .as_bytes(),
    )
    .await;
    let reply = recv_encrypted_json(&mut socket, &mut transport).await;
    (socket, transport, reply)
}

async fn open_authenticated_bearer_socket_from(
    handle: &ServerHandle,
    host_key: &[u8],
    credential: &str,
    source_ip: Ipv4Addr,
) -> (TestSocket, TransportState, Value) {
    let (mut socket, mut transport) =
        noise_connect_from(handle.local_addr(), host_key, source_ip).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "type": "e2ee_auth", "bearer": credential })
            .to_string()
            .as_bytes(),
    )
    .await;
    let reply = recv_encrypted_json(&mut socket, &mut transport).await;
    (socket, transport, reply)
}

async fn assert_get_config(socket: &mut TestSocket, transport: &mut TransportState) {
    send_encrypted(
        socket,
        transport,
        json!({
            "_tag": "Request",
            "id": "1",
            "tag": "server.getConfig",
            "payload": {},
            "headers": []
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    let response = recv_encrypted_json(socket, transport).await;
    assert_eq!(response["_tag"], "Exit");
    assert_eq!(response["requestId"], "1");
    assert_eq!(response["exit"]["_tag"], "Success");
}

async fn next_close_code(socket: &mut TestSocket) -> Option<u16> {
    loop {
        let frame = timeout(Duration::from_secs(3), socket.next())
            .await
            .expect("close timeout");
        match frame {
            Some(Ok(Message::Close(close))) => {
                return close.map(|frame| u16::from(frame.code));
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_))) => {}
            Some(Ok(Message::Text(_))) => {}
            Some(Err(_)) | None => return None,
            Some(Ok(Message::Frame(_))) => {}
        }
    }
}

async fn exchange_plain_token(client: &Client, handle: &ServerHandle, credential: &str) -> String {
    let response = client
        .post(http_url(handle, "/oauth/token"))
        .form(&[
            ("grant_type", TOKEN_GRANT_TYPE),
            ("subject_token", credential),
            ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
            ("requested_token_type", ACCESS_TOKEN_TYPE),
        ])
        .send()
        .await
        .expect("plain token exchange");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("token JSON")["access_token"]
        .as_str()
        .expect("access token")
        .to_owned()
}

async fn create_off_host_pairing(handle: &ServerHandle) -> (String, String) {
    let client = Client::new();
    let startup = handle.startup_access().expect("startup pairing");
    let admin = exchange_plain_token(&client, handle, &startup.credential).await;
    let response = client
        .post(http_url(handle, "/api/auth/pairing-offer"))
        .bearer_auth(&admin)
        .json(&json!({
            "name": "Off-host client",
            "endpoint": "http://192.168.1.20:3773",
            "reach": "another-device",
        }))
        .send()
        .await
        .expect("off-host pairing offer");
    assert_eq!(response.status(), StatusCode::OK);
    let code = response.json::<Value>().await.expect("pairing offer JSON")["code"]
        .as_str()
        .expect("pairing offer code")
        .to_owned();
    let payload = bibcode_server::auth_pairing_code::decode_pairing_code(&code)
        .expect("pairing code decodes");
    (admin, payload.token)
}

async fn share_state(handle: &ServerHandle, admin: &str) -> Value {
    let response = Client::new()
        .get(http_url(handle, "/api/auth/share-state"))
        .bearer_auth(admin)
        .send()
        .await
        .expect("share state request");
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.expect("share state JSON")
}

async fn plain_ws_request(address: SocketAddr, bearer: &str) -> Result<TestSocket, StatusCode> {
    let mut request = ws_url(address, "/ws")
        .into_client_request()
        .expect("plain WebSocket request");
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {bearer}")).expect("bearer header"),
    );
    match connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            Err(StatusCode::from_u16(response.status().as_u16()).expect("HTTP status"))
        }
        Err(error) => panic!("unexpected WebSocket error: {error}"),
    }
}

#[tokio::test]
async fn pairing_bootstrap_inside_the_channel_serves_get_config() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let descriptor = Client::new()
        .get(http_url(&handle, "/.well-known/bibcode/environment"))
        .send()
        .await
        .expect("descriptor request")
        .json::<Value>()
        .await
        .expect("descriptor JSON");

    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup.credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");
    assert!(
        reply["credential"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    );
    assert_eq!(reply["environmentId"], descriptor["environmentId"]);
    assert_eq!(reply["storageInstanceId"], descriptor["storageInstanceId"]);
    // The administrative startup grant is not an off-host share grant, so the
    // server delivers it immediately with no confirmation requirement.
    assert_eq!(reply.get("pairingConfirmationRequired"), None);
    assert_get_config(&mut socket, &mut transport).await;
}

#[tokio::test]
async fn off_host_pairing_requires_confirmation_even_without_a_client_flag() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let (_admin, pairing) = create_off_host_pairing(&handle).await;
    // The auth message carries no confirmation field at all: the server
    // decides from the consumed grant, so a client cannot opt out of the
    // durable-delivery guard by omitting a flag.
    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &pairing).await;
    let credential = reply["credential"]
        .as_str()
        .expect("minted credential")
        .to_owned();

    assert_eq!(reply["pairingConfirmationRequired"], true);
    assert_eq!(
        latest_off_host_auth_session_delivery_state(temp.path()),
        "pending-pairing"
    );

    let host_key = read_host_public_key(temp.path());
    let (mut pending, _pending_transport, pending_reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credential).await;
    assert_eq!(
        pending_reply,
        json!({ "type": "e2ee_error", "code": "unauthorized" })
    );
    let _ = pending.close(None).await;

    confirm_pairing(&mut socket, &mut transport, "2").await;
    assert_eq!(
        latest_off_host_auth_session_delivery_state(temp.path()),
        "active"
    );
    let (mut confirmed, mut confirmed_transport, confirmed_reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credential).await;
    assert_eq!(confirmed_reply, json!({ "type": "e2ee_authenticated" }));
    assert_get_config(&mut confirmed, &mut confirmed_transport).await;
}

#[tokio::test]
async fn bearer_form_reconnect_works_with_the_in_channel_credential() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let (mut first, mut first_transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup.credential).await;
    let credential = reply["credential"]
        .as_str()
        .expect("minted credential")
        .to_owned();
    confirm_pairing(&mut first, &mut first_transport, "2").await;
    first.close(None).await.expect("close first connection");

    let host_key = read_host_public_key(temp.path());
    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "type": "e2ee_auth", "bearer": credential })
            .to_string()
            .as_bytes(),
    )
    .await;
    assert_eq!(
        recv_encrypted_json(&mut socket, &mut transport).await,
        json!({ "type": "e2ee_authenticated" })
    );
    assert_get_config(&mut socket, &mut transport).await;
}

#[tokio::test]
async fn delivered_pairing_session_stays_pending_until_confirm_rpc() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let (admin, pairing) = create_off_host_pairing(&handle).await;
    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &pairing).await;

    assert_eq!(reply["type"], "e2ee_authenticated");
    assert_eq!(
        latest_off_host_auth_session_delivery_state(temp.path()),
        "pending-pairing"
    );
    assert_eq!(
        share_state(&handle, &admin).await["desiredExposure"],
        "wide"
    );
    confirm_pairing(&mut socket, &mut transport, "2").await;
    assert_eq!(
        latest_off_host_auth_session_delivery_state(temp.path()),
        "active"
    );
    confirm_pairing(&mut socket, &mut transport, "3").await;
    assert_eq!(
        share_state(&handle, &admin).await["desiredExposure"],
        "wide"
    );
}

#[tokio::test]
async fn closing_before_confirm_revokes_the_pending_session() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let (_admin, pairing) = create_off_host_pairing(&handle).await;
    let (mut socket, _transport, reply) = pair_inside_channel(&handle, temp.path(), &pairing).await;
    let credential = reply["credential"]
        .as_str()
        .expect("minted credential")
        .to_owned();
    socket.close(None).await.expect("close bootstrap socket");

    // The delivery-guard revocation is scheduled when the connection drops;
    // wait for it durably instead of racing the reconnect against it.
    timeout(Duration::from_secs(2), async {
        loop {
            if latest_off_host_auth_session_delivery_state_row(temp.path())
                .is_some_and(|(state, revoked)| state == "pending-pairing" && revoked)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unconfirmed session is durably revoked after the socket closes");

    let host_key = read_host_public_key(temp.path());
    let (mut reconnect, _reconnect_transport, reconnect_reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credential).await;
    assert_eq!(
        reconnect_reply,
        json!({ "type": "e2ee_error", "code": "unauthorized" })
    );
    let _ = reconnect.close(None).await;
}

#[tokio::test]
async fn confirmed_pairing_session_survives_disconnect_and_restart_cleanup() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let (_admin, pairing) = create_off_host_pairing(&handle).await;
    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &pairing).await;
    let credential = reply["credential"]
        .as_str()
        .expect("minted credential")
        .to_owned();
    confirm_pairing(&mut socket, &mut transport, "2").await;
    socket.close(None).await.expect("close confirmed socket");
    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("server shutdown timeout")
        .expect("server joins");

    let restarted = start_server(&temp).await;
    assert_eq!(latest_auth_session_delivery_state(temp.path()), "active");
    let host_key = read_host_public_key(temp.path());
    let (mut reconnect, mut reconnect_transport, reconnect_reply) =
        open_authenticated_bearer_socket(&restarted, &host_key, &credential).await;
    assert_eq!(reconnect_reply, json!({ "type": "e2ee_authenticated" }));
    assert_get_config(&mut reconnect, &mut reconnect_transport).await;
}

#[tokio::test]
async fn cli_minted_offer_pairs_and_the_session_survives_a_restart() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_bibcode"))
        .args(["pairing", "offer", "--base-dir"])
        .arg(temp.path())
        .args(["--endpoint", "http://192.168.1.20:3773", "--json"])
        .output()
        .expect("run pairing offer");
    assert!(
        output.status.success(),
        "pairing offer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let offer: Value = serde_json::from_slice(&output.stdout).expect("offer JSON");
    let payload = bibcode_server::auth_pairing_code::decode_pairing_code(
        offer["code"].as_str().expect("code"),
    )
    .expect("pairing code decodes");
    assert_eq!(
        payload.host_key,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(read_host_public_key(temp.path()))
    );

    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &payload.token).await;
    assert_eq!(
        reply["pairingConfirmationRequired"],
        Value::Bool(true),
        "{reply}"
    );
    let credential = reply["credential"]
        .as_str()
        .expect("minted credential")
        .to_owned();
    confirm_pairing(&mut socket, &mut transport, "2").await;
    socket.close(None).await.expect("close confirmed socket");
    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("server shutdown timeout")
        .expect("server joins");

    let restarted = start_server(&temp).await;
    let host_key = read_host_public_key(temp.path());
    let (mut reconnect, mut reconnect_transport, reconnect_reply) =
        open_authenticated_bearer_socket(&restarted, &host_key, &credential).await;
    assert_eq!(reconnect_reply, json!({ "type": "e2ee_authenticated" }));
    assert_get_config(&mut reconnect, &mut reconnect_transport).await;
}

#[tokio::test]
async fn no_downgrade_e2ee_credential_is_rejected_on_plain_surfaces() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let client = Client::new();
    let startup = handle.startup_access().expect("startup pairing");
    let plain_credential = exchange_plain_token(&client, &handle, &startup.credential).await;
    let pairing_response = client
        .post(http_url(&handle, "/api/auth/pairing-token"))
        .bearer_auth(&plain_credential)
        .json(&json!({ "label": "e2ee test" }))
        .send()
        .await
        .expect("pairing request");
    assert_eq!(pairing_response.status(), StatusCode::OK);
    let pairing = pairing_response
        .json::<Value>()
        .await
        .expect("pairing JSON")["credential"]
        .as_str()
        .expect("pairing credential")
        .to_owned();
    let (_socket, _transport, reply) = pair_inside_channel(&handle, temp.path(), &pairing).await;
    let e2ee_credential = reply["credential"].as_str().expect("e2ee credential");

    let session = client
        .get(http_url(&handle, "/api/auth/session"))
        .bearer_auth(e2ee_credential)
        .send()
        .await
        .expect("e2ee session request");
    assert_eq!(session.status(), StatusCode::OK);
    assert_eq!(
        session.json::<Value>().await.expect("session JSON")["authenticated"],
        false
    );
    assert_eq!(
        client
            .post(http_url(&handle, "/api/auth/websocket-ticket"))
            .bearer_auth(e2ee_credential)
            .send()
            .await
            .expect("e2ee ticket request")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        plain_ws_request(handle.local_addr(), e2ee_credential)
            .await
            .expect_err("e2ee credential cannot open plain WebSocket"),
        StatusCode::UNAUTHORIZED
    );

    let plain_session = client
        .get(http_url(&handle, "/api/auth/session"))
        .bearer_auth(&plain_credential)
        .send()
        .await
        .expect("plain session request");
    assert_eq!(plain_session.status(), StatusCode::OK);
    assert_eq!(
        plain_session
            .json::<Value>()
            .await
            .expect("plain session JSON")["authenticated"],
        true
    );
    assert_eq!(
        client
            .post(http_url(&handle, "/api/auth/websocket-ticket"))
            .bearer_auth(&plain_credential)
            .send()
            .await
            .expect("plain ticket request")
            .status(),
        StatusCode::OK
    );
    let mut plain_socket = plain_ws_request(handle.local_addr(), &plain_credential)
        .await
        .expect("plain credential opens plain WebSocket");
    plain_socket.close(None).await.expect("close plain socket");
}

#[tokio::test]
async fn bad_pairing_token_gets_e2ee_error_unauthorized_and_stays_unconsumed() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup_credential = handle
        .startup_access()
        .expect("startup pairing")
        .credential
        .clone();
    let host_key = read_host_public_key(temp.path());
    let (mut bad_socket, mut bad_transport) = noise_connect(handle.local_addr(), &host_key).await;
    send_encrypted(
        &mut bad_socket,
        &mut bad_transport,
        br#"{"type":"e2ee_auth","pairing":"nope"}"#,
    )
    .await;
    assert_eq!(
        recv_encrypted_json(&mut bad_socket, &mut bad_transport).await,
        json!({ "type": "e2ee_error", "code": "unauthorized" })
    );
    let _ = next_close_code(&mut bad_socket).await;

    let (dropped, _transport) = noise_connect(handle.local_addr(), &host_key).await;
    drop(dropped);
    let (_socket, _transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup_credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");
}

#[tokio::test]
async fn bad_bearer_gets_e2ee_error_unauthorized() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let host_key = read_host_public_key(temp.path());
    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        br#"{"type":"e2ee_auth","bearer":"garbage"}"#,
    )
    .await;
    assert_eq!(
        recv_encrypted_json(&mut socket, &mut transport).await,
        json!({ "type": "e2ee_error", "code": "unauthorized" })
    );
    let _ = next_close_code(&mut socket).await;
}

#[tokio::test]
async fn wrong_host_key_closes_with_4403() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let wrong_keypair = snow::Builder::new(NOISE_NK_PARAMS.parse().expect("Noise parameters"))
        .generate_keypair()
        .expect("wrong keypair");
    let mut initiator = initiator(&wrong_keypair.public);
    let mut socket = open_socket(handle.local_addr()).await;
    let mut message_a = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    let len = initiator
        .write_message(&[], &mut message_a)
        .expect("wrong-key message A");
    message_a.truncate(len);
    socket
        .send(Message::Binary(message_a.into()))
        .await
        .expect("send wrong-key message A");
    assert_eq!(next_close_code(&mut socket).await, Some(4403));
}

#[tokio::test]
async fn non_empty_handshake_payload_is_rejected() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let host_key = read_host_public_key(temp.path());
    let mut initiator = initiator(&host_key);
    let mut socket = open_socket(handle.local_addr()).await;
    let mut message_a = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    let len = initiator
        .write_message(b"x", &mut message_a)
        .expect("non-empty message A");
    message_a.truncate(len);
    socket
        .send(Message::Binary(message_a.into()))
        .await
        .expect("send non-empty message A");
    assert_eq!(next_close_code(&mut socket).await, None);
}

#[tokio::test]
async fn oversized_binary_frame_closes_the_connection() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let (mut socket, _transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup.credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");
    socket
        .send(Message::Binary(vec![0_u8; MAX_CIPHERTEXT_BYTES + 1].into()))
        .await
        .expect("send oversized frame");
    let outcome = timeout(
        Duration::from_secs(3),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("oversized authenticated frame reaches a terminal outcome");
    assert!(matches!(
        outcome,
        None | Some(Ok(Message::Close(_))) | Some(Err(_))
    ));
}

#[tokio::test]
async fn oversized_pre_auth_websocket_message_is_rejected() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let mut socket = open_socket(handle.local_addr()).await;

    socket
        .send(Message::Binary(vec![0_u8; MAX_CIPHERTEXT_BYTES + 1].into()))
        .await
        .expect("send oversized pre-auth message");

    assert_eq!(
        next_close_code(&mut socket).await,
        None,
        "an oversized frame tears the connection down without a graceful coded close"
    );
}

#[tokio::test]
async fn preauth_message_cap_is_64kib() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let host_key = read_host_public_key(temp.path());
    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    for (flag, length) in [
        (RECORD_CONTINUATION, 40 * 1024),
        (RECORD_CONTINUATION, 40 * 1024),
        (RECORD_FINAL, 0),
    ] {
        let frame = encrypt_record(&mut transport, flag, &vec![b'a'; length]);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .expect("send pre-auth record");
    }
    assert_eq!(
        next_close_code(&mut socket).await,
        None,
        "exceeding the pre-auth message cap closes without a graceful coded close"
    );
}

#[tokio::test]
async fn authenticated_empty_continuation_is_rejected() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup.credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");

    let frame = encrypt_record(&mut transport, RECORD_CONTINUATION, b"");
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("send empty continuation");
    let outcome = timeout(
        Duration::from_secs(3),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("server rejects invalid fragmentation");
    assert!(matches!(
        outcome,
        None | Some(Ok(Message::Close(_))) | Some(Err(_))
    ));
}

#[tokio::test]
async fn incomplete_authenticated_message_closes_after_ten_seconds_without_progress() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup.credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");

    let frame = encrypt_record(&mut transport, RECORD_CONTINUATION, b"partial");
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("send incomplete encrypted message");
    let outcome = timeout(
        Duration::from_secs(12),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("incomplete-message progress deadline");
    assert!(matches!(
        outcome,
        None | Some(Ok(Message::Close(_))) | Some(Err(_))
    ));
}

#[tokio::test]
async fn idle_authenticated_connection_has_no_reassembly_deadline() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let (mut socket, mut transport, reply) =
        pair_inside_channel(&handle, temp.path(), &startup.credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");

    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_get_config(&mut socket, &mut transport).await;
}

#[tokio::test]
async fn handshake_timeout_closes_the_socket() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let mut socket = open_socket(handle.local_addr()).await;
    let frame = timeout(Duration::from_secs(12), socket.next())
        .await
        .expect("server enforces the ten-second combined deadline");
    assert!(matches!(
        frame,
        None | Some(Ok(Message::Close(_))) | Some(Err(_))
    ));
}

#[tokio::test]
async fn preauth_loopback_forwarder_bypasses_public_peer_cap_until_the_soft_cap() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let host_key = read_host_public_key(temp.path());
    // The forwarder class is exempt from the four-per-peer public cap, but an
    // established bucket still stops at the global soft cap so it cannot
    // drain the fresh-bucket reserve.
    let mut stalled = Vec::with_capacity(E2EE_PREAUTH_GLOBAL_SOFT_CAP);
    for _ in 0..E2EE_PREAUTH_GLOBAL_SOFT_CAP {
        stalled.push(noise_connect(handle.local_addr(), &host_key).await);
    }
    let mut overflow = open_socket(handle.local_addr()).await;
    assert_eq!(next_close_code(&mut overflow).await, Some(1013));

    let (released, _) = stalled.pop().expect("one stalled connection");
    drop(released);
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (mut admitted, _) = noise_connect(handle.local_addr(), &host_key).await;
    admitted
        .close(None)
        .await
        .expect("close admitted connection");
}

#[tokio::test]
async fn established_capacity_is_partitioned_by_principal_and_released_on_close() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let client = Client::new();
    let startup = handle.startup_access().expect("startup pairing");
    let admin = exchange_plain_token(&client, &handle, &startup.credential).await;
    let first_pairing = client
        .post(http_url(&handle, "/api/auth/pairing-token"))
        .bearer_auth(&admin)
        .json(&json!({ "label": "first principal" }))
        .send()
        .await
        .expect("first pairing request")
        .json::<Value>()
        .await
        .expect("first pairing JSON")["credential"]
        .as_str()
        .expect("first pairing credential")
        .to_owned();
    let second_pairing = client
        .post(http_url(&handle, "/api/auth/pairing-token"))
        .bearer_auth(&admin)
        .json(&json!({ "label": "second principal" }))
        .send()
        .await
        .expect("second pairing request")
        .json::<Value>()
        .await
        .expect("second pairing JSON")["credential"]
        .as_str()
        .expect("second pairing credential")
        .to_owned();
    let first_credential = mint_e2ee_credential(&handle, temp.path(), &first_pairing).await;
    let second_credential = mint_e2ee_credential(&handle, temp.path(), &second_pairing).await;
    let host_key = read_host_public_key(temp.path());

    let mut first_principal_sockets = Vec::new();
    tokio::time::sleep(Duration::from_secs(2)).await;
    // The fleet-sized forwarder burst admits every sequential pre-auth
    // handshake here without refill pauses; established capacity, not
    // pre-auth rate, is what this test partitions.
    for _ in 0..E2EE_MAX_ESTABLISHED_CONNECTIONS_PER_PRINCIPAL {
        let (socket, transport, reply) = open_authenticated_bearer_socket_from(
            &handle,
            &host_key,
            &first_credential,
            Ipv4Addr::LOCALHOST,
        )
        .await;
        assert_eq!(reply, json!({ "type": "e2ee_authenticated" }));
        first_principal_sockets.push((socket, transport));
    }

    tokio::time::sleep(Duration::from_secs(8)).await;
    let (mut overflow, _transport, reply) = open_authenticated_bearer_socket_from(
        &handle,
        &host_key,
        &first_credential,
        Ipv4Addr::LOCALHOST,
    )
    .await;
    assert_eq!(reply, json!({ "type": "e2ee_error", "code": "protocol" }));
    let _ = next_close_code(&mut overflow).await;

    let (mut second_socket, _transport, reply) = open_authenticated_bearer_socket_from(
        &handle,
        &host_key,
        &second_credential,
        Ipv4Addr::LOCALHOST,
    )
    .await;
    assert_eq!(reply, json!({ "type": "e2ee_authenticated" }));
    second_socket
        .close(None)
        .await
        .expect("close second principal");

    let (mut released, _) = first_principal_sockets
        .pop()
        .expect("first principal socket");
    released
        .close(None)
        .await
        .expect("close first principal socket");
    let mut replacement = timeout(Duration::from_secs(3), async {
        loop {
            let (mut socket, transport, reply) = open_authenticated_bearer_socket_from(
                &handle,
                &host_key,
                &first_credential,
                Ipv4Addr::LOCALHOST,
            )
            .await;
            if reply == json!({ "type": "e2ee_authenticated" }) {
                break (socket, transport);
            }
            let _ = next_close_code(&mut socket).await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closed connection releases principal capacity");
    replacement
        .0
        .close(None)
        .await
        .expect("close replacement socket");
}

#[tokio::test]
async fn inbound_plaintext_capacity_backpressures_by_principal_and_releases_on_close() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let client = Client::new();
    let startup = handle.startup_access().expect("startup pairing");
    let admin = exchange_plain_token(&client, &handle, &startup.credential).await;
    let mut credentials = Vec::new();
    for label in ["inbound first principal", "inbound second principal"] {
        let pairing = client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(&admin)
            .json(&json!({ "label": label }))
            .send()
            .await
            .expect("pairing request")
            .json::<Value>()
            .await
            .expect("pairing JSON")["credential"]
            .as_str()
            .expect("pairing credential")
            .to_owned();
        credentials.push(mint_e2ee_credential(&handle, temp.path(), &pairing).await);
    }
    let host_key = read_host_public_key(temp.path());
    let (mut first_partial, mut first_transport, first_reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credentials[0]).await;
    assert_eq!(first_reply, json!({ "type": "e2ee_authenticated" }));

    send_encrypted_continuations(
        &mut first_partial,
        &mut first_transport,
        E2EE_INBOUND_BUFFER_BUDGET_BYTES_PER_PRINCIPAL,
    )
    .await;
    // Under load the request below could otherwise be served before the budget is used up.
    wait_for_server_intake(&mut first_partial).await;
    // Opened only now: a socket that stays silent while the budget is filled, which takes
    // over a minute on a loaded runner, outlives the server's heartbeat silence limit.
    let (mut waiting, mut waiting_transport, waiting_reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credentials[0]).await;
    assert_eq!(waiting_reply, json!({ "type": "e2ee_authenticated" }));

    send_encrypted(
        &mut waiting,
        &mut waiting_transport,
        json!({
            "_tag": "Request",
            "id": "2",
            "tag": "server.getConfig",
            "payload": {},
            "headers": []
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(100),
            next_frame_past_heartbeat(&mut waiting)
        )
        .await
        .is_err(),
        "principal pressure must backpressure without closing the waiting socket"
    );

    let (mut other_principal, mut other_transport, other_reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credentials[1]).await;
    assert_eq!(other_reply, json!({ "type": "e2ee_authenticated" }));
    assert_get_config(&mut other_principal, &mut other_transport).await;

    first_partial
        .close(None)
        .await
        .expect("close first partial socket");
    let response = recv_encrypted_json(&mut waiting, &mut waiting_transport).await;
    assert_eq!(response["_tag"], "Exit");
    assert_eq!(response["requestId"], "2");
    assert_eq!(response["exit"]["_tag"], "Success");
    assert_get_config(&mut waiting, &mut waiting_transport).await;
}

#[tokio::test]
async fn text_frames_before_handshake_are_rejected() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let mut socket = open_socket(handle.local_addr()).await;
    socket
        .send(Message::Text("plaintext".into()))
        .await
        .expect("send plaintext frame");
    assert_eq!(next_close_code(&mut socket).await, None);
}

#[tokio::test]
async fn minted_pairing_offer_pins_the_host_key_and_opens_the_e2ee_channel() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let admin = exchange_plain_token(&Client::new(), &handle, &startup.credential).await;
    let response = Client::new()
        .post(http_url(&handle, "/api/auth/pairing-offer"))
        .bearer_auth(&admin)
        .json(&json!({
            "endpoint": "http://127.0.0.1:3773",
            "name": "Test Host",
            "reach": "custom",
        }))
        .send()
        .await
        .expect("pairing offer request");
    assert_eq!(response.status(), StatusCode::OK);
    let offer = response.json::<Value>().await.expect("pairing offer JSON");
    let code = offer["code"].as_str().expect("pairing code");
    let payload = bibcode_server::auth_pairing_code::decode_pairing_code(code).expect("decodes");
    assert_eq!(payload.name, "Test Host");
    assert_eq!(
        payload.reach,
        bibcode_server::auth_pairing_code::RemotePairingReach::Custom
    );
    let host_key = URL_SAFE_NO_PAD
        .decode(&payload.host_key)
        .expect("host key base64url");
    assert_eq!(host_key, read_host_public_key(temp.path()));

    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "type": "e2ee_auth", "pairing": payload.token })
            .to_string()
            .as_bytes(),
    )
    .await;
    let authenticated = recv_encrypted_json(&mut socket, &mut transport).await;
    assert_eq!(authenticated["type"], "e2ee_authenticated");
    assert!(
        authenticated["credential"]
            .as_str()
            .is_some_and(|credential| !credential.is_empty())
    );
    assert_eq!(
        authenticated["storageInstanceId"].as_str(),
        Some(payload.storage_instance_id.as_str())
    );
    assert_get_config(&mut socket, &mut transport).await;
}

#[tokio::test]
async fn interleave_v1_is_confirmed_only_when_the_client_lists_it() {
    let _permit = TEST_PERMIT.acquire().await.expect("test permit");
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let startup = handle.startup_access().expect("startup pairing");
    let credential = mint_e2ee_credential(&handle, temp.path(), &startup.credential).await;
    let host_key = read_host_public_key(temp.path());

    let (mut socket, mut transport) = noise_connect(handle.local_addr(), &host_key).await;
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "type": "e2ee_auth", "bearer": credential, "features": ["interleave-v1"] })
            .to_string()
            .as_bytes(),
    )
    .await;
    let reply = recv_encrypted_json(&mut socket, &mut transport).await;
    assert_eq!(reply["type"], "e2ee_authenticated");
    assert_eq!(reply["features"], json!(["interleave-v1"]));
    assert_get_config(&mut socket, &mut transport).await;

    let (_socket, _transport, reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credential).await;
    assert_eq!(reply["type"], "e2ee_authenticated");
    assert!(
        reply.get("features").is_none(),
        "no features without the request: {reply}"
    );
}

async fn upload_rpc(
    socket: &mut TestSocket,
    transport: &mut TransportState,
    id: &str,
    method: &str,
    payload: Value,
) -> Value {
    send_encrypted(
        socket,
        transport,
        json!({"_tag":"Request","id":id,"tag":method,"payload":payload,"headers":[]})
            .to_string()
            .as_bytes(),
    )
    .await;
    loop {
        let response = recv_encrypted_json(socket, transport).await;
        if response["requestId"] == id {
            return response;
        }
    }
}
#[tokio::test]
async fn authenticated_staged_upload_ownership_survives_reconnect_and_refuses_foreign_access() {
    let _permit = TEST_PERMIT.acquire().await.unwrap();
    let temp = TempDir::new().unwrap();
    let handle = start_server(&temp).await;
    let client = Client::new();
    // The startup grant is single-use. Exchange it once, then mint distinct
    // pairing grants so these sockets represent genuinely different principals.
    let admin = exchange_plain_token(
        &client,
        &handle,
        &handle.startup_access().unwrap().credential,
    )
    .await;
    let mut credentials = Vec::new();
    for label in ["upload owner", "foreign uploader"] {
        let response = client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(&admin)
            .json(&json!({"label":label}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let grant = response.json::<Value>().await.unwrap()["credential"]
            .as_str()
            .unwrap()
            .to_owned();
        credentials.push(mint_e2ee_credential(&handle, temp.path(), &grant).await);
    }
    let host_key = read_host_public_key(temp.path());
    let (mut first, mut first_transport, reply) =
        open_authenticated_bearer_socket(&handle, &host_key, &credentials[0]).await;
    assert_eq!(reply["type"], "e2ee_authenticated");
    let begun = upload_rpc(&mut first, &mut first_transport, "1", "uploads.begin", json!({"target":{"_tag":"chat-attachment","type":"file","name":"notes.txt","mimeType":"text/plain"},"sizeBytes":3})).await;
    assert_eq!(begun["exit"]["_tag"], "Success", "{begun}");
    let id = begun["exit"]["value"]["uploadId"]
        .as_str()
        .unwrap()
        .to_owned();
    first.close(None).await.unwrap();
    let (mut foreign, mut foreign_transport, _) =
        open_authenticated_bearer_socket(&handle, &host_key, &credentials[1]).await;
    for (method, payload) in [
        ("uploads.get", json!({"uploadId":id})),
        (
            "uploads.append",
            json!({"uploadId":id,"offset":0,"data":"YWJj"}),
        ),
    ] {
        let denied = upload_rpc(&mut foreign, &mut foreign_transport, "3", method, payload).await;
        assert_eq!(denied["exit"]["_tag"], "Failure", "{denied}");
        assert_eq!(denied["exit"]["cause"][0]["error"]["_tag"], "UploadError");
        assert_eq!(denied["exit"]["cause"][0]["error"]["reason"], "not_found");
    }
    let cancel = upload_rpc(
        &mut foreign,
        &mut foreign_transport,
        "4",
        "uploads.cancel",
        json!({"uploadId":id}),
    )
    .await;
    assert_eq!(cancel["exit"]["value"], json!({}));
    let (mut owner, mut owner_transport, _) =
        open_authenticated_bearer_socket(&handle, &host_key, &credentials[0]).await;
    let status = upload_rpc(
        &mut owner,
        &mut owner_transport,
        "5",
        "uploads.get",
        json!({"uploadId":id}),
    )
    .await;
    assert_eq!(status["exit"]["value"]["receivedBytes"], 0);
    let appended = upload_rpc(&mut owner, &mut owner_transport, "6", "uploads.append", json!({"uploadId":id,"offset":0,"data":"YWJj","sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"})).await;
    assert_eq!(appended["exit"]["value"]["receivedBytes"], 3, "{appended}");
    let status = upload_rpc(
        &mut owner,
        &mut owner_transport,
        "7",
        "uploads.get",
        json!({"uploadId":id}),
    )
    .await;
    assert_eq!(status["exit"]["value"]["complete"], true);
    upload_rpc(
        &mut owner,
        &mut owner_transport,
        "8",
        "uploads.cancel",
        json!({"uploadId":id}),
    )
    .await;
    owner.close(None).await.unwrap();
    foreign.close(None).await.unwrap();
    handle.shutdown();
    handle.join().await.unwrap();
}
