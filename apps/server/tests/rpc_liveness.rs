//! Slow-link regression harness (connection-liveness design, "Validation").
//!
//! A real server sits behind an in-process throttling TCP proxy. The proxy
//! reads from the server only as fast as the link rate, through a small
//! upstream receive buffer, so the server's socket writes block as on a real
//! bottleneck (backpressure). The test clients follow the item-1 client rule:
//! every inbound WebSocket message is proof of life, an RPC `Ping` goes out
//! after 10 s without inbound data, and the link is declared dead after 30 s.
//!
//! The transfer matrix takes about two minutes (8 MiB at 64 KiB/s is 128 s);
//! all matrix trials run concurrently against one server. The config snapshot
//! trial takes about 32 s (256 KiB at 8 KiB/s), with both framings concurrent.

use std::{
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use coddy_studio_server::{RpcRegistry, ServerConfig, ServerHandle, ServerRuntime};
use futures_util::{SinkExt, Stream, StreamExt, future::join_all};
use serde_json::{Value, json};
use snow::TransportState;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    time::{Instant, sleep, sleep_until, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WebSocketError, Message, client::IntoClientRequest, http::HeaderValue},
};

const KIB: u64 = 1024;
const MIB: usize = 1024 * 1024;
const PROBE_AFTER: Duration = Duration::from_secs(10);
const DEAD_AFTER: Duration = Duration::from_secs(30);
const NOISE_NK_PARAMS: &str = "Noise_NK_25519_ChaChaPoly_SHA256";
const MAX_CIPHERTEXT_BYTES: usize = 65_535;
const MAX_CHUNK_BYTES: usize = 65_518;
const MAX_LOGICAL_BYTES: usize = 64 * MIB;
const MAX_RECORDS: usize = 2_048;
const PROXY_CHUNK: usize = 4 * 1024;
const PROXY_UPSTREAM_RECEIVE_BUFFER: u32 = 64 * 1024;
/// Absolute bound for one trial, connection setup included. It matches the
/// matrix's 600 s completion assertion, so a server that keeps answering
/// Pings but never sends the Exit fails the test instead of hanging it.
const TRIAL_DEADLINE: Duration = Duration::from_secs(600);

type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    PlainSplit,
    PlainLegacy,
    Encrypted,
}

#[derive(Clone, Copy, Debug)]
struct Trial {
    mode: Mode,
    rate: u64,
    bytes: usize,
}

#[derive(Debug)]
enum Outcome {
    Completed(Duration),
    Dead(Duration),
    Closed(Duration),
}

/// Starts an unauthenticated loopback server with `fixture.bytes`; `extend`
/// registers any further fixture methods.
async fn start_server_with(temp: &TempDir, extend: impl FnOnce(&mut RpcRegistry)) -> ServerHandle {
    let mut registry = RpcRegistry::empty();
    registry.register_unary("fixture.bytes", |request, _cancellation| async move {
        let bytes = request.payload["bytes"]
            .as_u64()
            .and_then(|bytes| usize::try_from(bytes).ok())
            .expect("bytes");
        Ok(json!({ "data": "x".repeat(bytes) }))
    });
    extend(&mut registry);
    ServerRuntime::start_with_registry(
        ServerConfig::new(temp.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
        registry,
    )
    .await
    .expect("server starts")
}

async fn start_server(temp: &TempDir) -> ServerHandle {
    start_server_with(temp, |_| {}).await
}

fn host_public_key(root: &Path) -> Vec<u8> {
    let record = std::fs::read(
        root.join("userdata")
            .join("secrets")
            .join("host-identity-x25519.bin"),
    )
    .expect("persisted host identity");
    record[32..].to_vec()
}

/// Paces server-to-client bytes at `rate` and forwards client-to-server bytes
/// as they come. `freeze(true)` stops both directions without closing.
struct ThrottleProxy {
    address: SocketAddr,
    frozen: Arc<AtomicBool>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl ThrottleProxy {
    async fn start(target: SocketAddr, rate: u64) -> Self {
        Self::start_duplex(target, rate, 0).await
    }
    async fn start_duplex(target: SocketAddr, rate: u64, up_rate: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listens");
        let address = listener.local_addr().expect("proxy address");
        let frozen = Arc::new(AtomicBool::new(false));
        let accept_frozen = Arc::clone(&frozen);
        let accept_task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept() => { let Ok((client, _)) = result else { break; }; connections.spawn(relay(client, target, rate, up_rate, Arc::clone(&accept_frozen))); }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            address,
            frozen,
            accept_task,
        }
    }

    fn freeze(&self, frozen: bool) {
        self.frozen.store(frozen, Ordering::Relaxed);
    }
}

impl Drop for ThrottleProxy {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn wait_while_frozen(frozen: &AtomicBool) -> bool {
    let mut waited = false;
    while frozen.load(Ordering::Relaxed) {
        waited = true;
        sleep(Duration::from_millis(20)).await;
    }
    waited
}

async fn paced_copy<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    rate: u64,
    frozen: Arc<AtomicBool>,
) {
    let mut buffer = vec![0_u8; PROXY_CHUNK];
    let mut next_send = Instant::now();
    loop {
        if wait_while_frozen(&frozen).await {
            next_send = Instant::now();
        }
        let read = match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if rate != 0 {
            next_send =
                next_send.max(Instant::now()) + Duration::from_secs_f64(read as f64 / rate as f64);
            sleep_until(next_send).await;
        }
        if wait_while_frozen(&frozen).await {
            next_send = Instant::now();
        }
        if writer.write_all(&buffer[..read]).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
}
async fn relay(
    client: TcpStream,
    target: SocketAddr,
    down_rate: u64,
    up_rate: u64,
    frozen: Arc<AtomicBool>,
) {
    let upstream = TcpSocket::new_v4().expect("upstream socket");
    upstream
        .set_recv_buffer_size(PROXY_UPSTREAM_RECEIVE_BUFFER)
        .expect("small upstream receive buffer");
    let Ok(server) = upstream.connect(target).await else {
        return;
    };
    let (client_read, client_write) = client.into_split();
    let (server_read, server_write) = server.into_split();
    tokio::select! {
        () = paced_copy(client_read, server_write, up_rate, frozen.clone()) => {},
        () = paced_copy(server_read, client_write, down_rate, frozen) => {},
    }
}

enum Framing {
    Whole,
    Records,
    Encrypted(Box<TransportState>),
}

async fn connect_plain(proxy: SocketAddr, chunked: bool) -> TestSocket {
    let mut request = format!("ws://{proxy}/ws")
        .into_client_request()
        .expect("WebSocket request");
    if chunked {
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_static("coddy-studio.rpc.chunked.v1"),
        );
    }
    let (socket, response) = connect_async(request).await.expect("WebSocket connects");
    assert_eq!(
        response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok()),
        chunked.then_some("coddy-studio.rpc.chunked.v1"),
    );
    socket
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
    while let Some(chunk) = chunks.next() {
        let flag = if chunks.peek().is_some() { 0x01 } else { 0x00 };
        let frame = encrypt_record(transport, flag, chunk);
        socket
            .send(Message::Binary(frame.into()))
            .await
            .expect("send encrypted record");
    }
}

async fn connect_encrypted(proxy: SocketAddr, host_key: &[u8]) -> (TestSocket, TransportState) {
    let mut socket = connect_async(format!("ws://{proxy}/ws-e2ee"))
        .await
        .expect("E2EE WebSocket connects")
        .0;
    let mut initiator = snow::Builder::new(NOISE_NK_PARAMS.parse().expect("Noise parameters"))
        .remote_public_key(host_key)
        .expect("host key")
        .build_initiator()
        .expect("Noise initiator");
    let mut message_a = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    let len = initiator
        .write_message(&[], &mut message_a)
        .expect("write message A");
    message_a.truncate(len);
    socket
        .send(Message::Binary(message_a.into()))
        .await
        .expect("send message A");
    let message_b = loop {
        match socket.next().await {
            Some(Ok(Message::Binary(frame))) => break frame,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            other => panic!("expected message B, got {other:?}"),
        }
    };
    let mut payload = vec![0_u8; MAX_CIPHERTEXT_BYTES];
    initiator
        .read_message(&message_b, &mut payload)
        .expect("read message B");
    let mut transport = initiator
        .into_transport_mode()
        .expect("Noise transport mode");
    // The server runs with unsafe_no_auth, so any bearer is accepted.
    send_encrypted(
        &mut socket,
        &mut transport,
        json!({ "type": "e2ee_auth", "bearer": "harness", "features": ["interleave-v1"] })
            .to_string()
            .as_bytes(),
    )
    .await;
    (socket, transport)
}

async fn send_text(socket: &mut TestSocket, framing: &mut Framing, text: &str) {
    match framing {
        Framing::Whole | Framing::Records => socket
            .send(Message::Text(text.to_owned().into()))
            .await
            .expect("send text frame"),
        Framing::Encrypted(transport) => send_encrypted(socket, transport, text.as_bytes()).await,
    }
}

#[derive(Default)]
struct Assembly {
    bytes: Vec<u8>,
    records: usize,
}

/// Turns one WebSocket data frame into zero or one complete RPC message.
fn reassemble(framing: &mut Framing, partial: &mut Assembly, frame: Message) -> Option<Vec<u8>> {
    let record = match (framing, frame) {
        (Framing::Whole | Framing::Records, Message::Text(text)) => {
            return Some(text.as_bytes().to_vec());
        }
        (Framing::Whole, Message::Binary(bytes)) => return Some(bytes.to_vec()),
        (Framing::Records, Message::Binary(bytes)) => bytes.to_vec(),
        (Framing::Encrypted(transport), Message::Binary(bytes)) => {
            let mut plaintext = vec![0_u8; MAX_CIPHERTEXT_BYTES];
            let len = transport
                .read_message(&bytes, &mut plaintext)
                .expect("decrypt record");
            plaintext.truncate(len);
            plaintext
        }
        _ => return None,
    };
    let (&flag, chunk) = record.split_first().expect("record flag");
    assert!(partial.records < MAX_RECORDS, "record cap");
    assert!(
        partial.bytes.len() + chunk.len() <= MAX_LOGICAL_BYTES,
        "message cap"
    );
    match flag {
        0x00 => {
            partial.bytes.extend_from_slice(chunk);
            partial.records = 0;
            Some(std::mem::take(&mut partial.bytes))
        }
        0x01 => {
            assert!(!chunk.is_empty(), "nonempty continuation");
            partial.records += 1;
            partial.bytes.extend_from_slice(chunk);
            None
        }
        0x02 => {
            assert!(
                !chunk.is_empty() && chunk.len() <= MAX_CHUNK_BYTES,
                "control cap"
            );
            Some(chunk.to_vec())
        }
        other => panic!("unknown record flag {other}"),
    }
}

/// Reads until `response_tag` or a terminal Exit for `request_id`, applying
/// the item-1 client rule to each inbound data frame before reassembly.
async fn wait_for_response(
    socket: &mut TestSocket,
    framing: &mut Framing,
    started: Instant,
    request_id: &str,
    response_tag: &str,
) -> Result<(Duration, Value), Outcome> {
    let mut last_inbound = Instant::now();
    let mut pinged = false;
    let mut partial = Assembly::default();
    loop {
        let idle_deadline = last_inbound + if pinged { DEAD_AFTER } else { PROBE_AFTER };
        let frame = tokio::select! {
            frame = socket.next() => frame,
            () = sleep_until(idle_deadline) => {
                if pinged {
                    let close = tokio_tungstenite::tungstenite::protocol::CloseFrame {
                        code: 4408.into(),
                        reason: "liveness timeout".into(),
                    };
                    let _ = timeout(Duration::from_secs(1), socket.close(Some(close))).await;
                    return Err(Outcome::Dead(started.elapsed()));
                }
                send_text(socket, framing, r#"{"_tag":"Ping"}"#).await;
                pinged = true;
                continue;
            }
        };
        let Some(Ok(frame)) = frame else {
            return Err(Outcome::Closed(started.elapsed()));
        };
        if matches!(frame, Message::Close(_)) {
            return Err(Outcome::Closed(started.elapsed()));
        }
        if matches!(&frame, Message::Text(_) | Message::Binary(_)) {
            last_inbound = Instant::now();
            pinged = false;
        }
        let Some(message) = reassemble(framing, &mut partial, frame) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&message) else {
            continue;
        };
        if value["requestId"] == request_id
            && (value["_tag"] == response_tag || value["_tag"] == "Exit")
        {
            return Ok((started.elapsed(), value));
        }
    }
}

/// Reads until the Exit for `request_id`, applying the item-1 client rule.
async fn wait_for_exit(
    socket: &mut TestSocket,
    framing: &mut Framing,
    started: Instant,
    request_id: &str,
    expected_bytes: usize,
) -> Outcome {
    match wait_for_response(socket, framing, started, request_id, "Exit").await {
        Ok((after, value)) => {
            assert_eq!(
                value["exit"]["_tag"], "Success",
                "matching Exit must succeed: {value}"
            );
            let data = value["exit"]["value"]["data"]
                .as_str()
                .expect("fixture data string");
            assert_eq!(data.len(), expected_bytes, "exact requested payload length");
            assert!(
                data.bytes().all(|byte| byte == b'x'),
                "exact fixture payload content"
            );
            Outcome::Completed(after)
        }
        Err(outcome) => outcome,
    }
}

/// Connects through `proxy` and returns the socket and its framing, after
/// the E2EE `e2ee_authenticated` reply for encrypted sockets.
async fn open(mode: Mode, proxy: SocketAddr, host_key: &[u8]) -> (TestSocket, Framing) {
    match mode {
        Mode::PlainSplit => (connect_plain(proxy, true).await, Framing::Records),
        Mode::PlainLegacy => (connect_plain(proxy, false).await, Framing::Whole),
        Mode::Encrypted => {
            let (mut socket, transport) = connect_encrypted(proxy, host_key).await;
            let mut framing = Framing::Encrypted(Box::new(transport));
            let mut partial = Assembly::default();
            loop {
                let frame = socket
                    .next()
                    .await
                    .expect("authenticated reply")
                    .expect("frame");
                if let Some(message) = reassemble(&mut framing, &mut partial, frame) {
                    let reply: Value = serde_json::from_slice(&message).expect("reply JSON");
                    assert_eq!(reply["type"], "e2ee_authenticated");
                    assert_eq!(reply["features"], json!(["interleave-v1"]));
                    break;
                }
            }
            (socket, framing)
        }
    }
}

async fn request_delayed(
    socket: &mut TestSocket,
    framing: &mut Framing,
    id: &str,
    delay_ms: u64,
    bytes: usize,
) {
    let request = json!({
        "_tag": "Request",
        "id": id,
        "tag": "fixture.delayed",
        "payload": { "delayMs": delay_ms, "bytes": bytes },
        "headers": [],
    });
    send_text(socket, framing, &request.to_string()).await;
}

async fn request_bytes(socket: &mut TestSocket, framing: &mut Framing, id: &str, bytes: usize) {
    let request = json!({
        "_tag": "Request",
        "id": id,
        "tag": "fixture.bytes",
        "payload": { "bytes": bytes },
        "headers": [],
    });
    send_text(socket, framing, &request.to_string()).await;
}

/// Runs `future` under [`TRIAL_DEADLINE`] and names what did not finish.
async fn within_trial_deadline<T>(label: &str, future: impl std::future::Future<Output = T>) -> T {
    let started = Instant::now();
    match timeout(TRIAL_DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!(
            "{label} did not finish within {TRIAL_DEADLINE:?} (elapsed {:?})",
            started.elapsed()
        ),
    }
}

async fn run_trial(server: SocketAddr, host_key: &[u8], trial: Trial) -> Outcome {
    let label = format!(
        "slow-link trial {:?} framing at {} B/s for {} bytes",
        trial.mode, trial.rate, trial.bytes
    );
    within_trial_deadline(&label, async {
        let proxy = ThrottleProxy::start(server, trial.rate).await;
        let (mut socket, mut framing) = open(trial.mode, proxy.address, host_key).await;
        let started = Instant::now();
        request_bytes(&mut socket, &mut framing, "1", trial.bytes).await;
        wait_for_exit(&mut socket, &mut framing, started, "1", trial.bytes).await
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_links_finish_transfers_without_a_disconnect() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let host_key = host_public_key(temp.path());
    let server = handle.local_addr();
    let trials: Vec<Trial> = [Mode::PlainSplit, Mode::PlainLegacy, Mode::Encrypted]
        .into_iter()
        .flat_map(|mode| {
            [64 * KIB, 256 * KIB].into_iter().flat_map(move |rate| {
                [MIB, 8 * MIB]
                    .into_iter()
                    .map(move |bytes| Trial { mode, rate, bytes })
            })
        })
        .collect();
    let outcomes = join_all(
        trials
            .iter()
            .map(|trial| run_trial(server, &host_key, *trial)),
    )
    .await;

    let mut failures = Vec::new();
    for (trial, outcome) in trials.iter().zip(&outcomes) {
        let expected_seconds = trial.bytes as f64 / trial.rate as f64;
        let passed = match (trial.mode, outcome) {
            (_, Outcome::Completed(after)) => {
                assert!(
                    *after < Duration::from_secs(600),
                    "completed after {after:?}"
                );
                true
            }
            (_, Outcome::Closed(after)) => {
                failures.push(format!("{trial:?}: peer closed after {after:?}"));
                false
            }
            // A legacy whole frame that needs more than 30 s cannot beat the
            // client rule; it may be declared dead, but never before 27 s.
            (Mode::PlainLegacy, Outcome::Dead(after)) => {
                expected_seconds >= 30.0
                    && *after >= Duration::from_secs(27)
                    && *after <= Duration::from_secs(33)
            }
            _ => false,
        };
        if !passed {
            failures.push(format!("{trial:?} -> {outcome:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "slow-link trials failed:\n{}",
        failures.join("\n")
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

// Exactly 256 KiB of snapshot JSON, above the design's 250 KB minimum.
const CONFIG_SNAPSHOT_BYTES: usize = 256 * 1024;
// Phase 1's supported slow-link target, half the server's writer rate floor.
const CONFIG_SNAPSHOT_RATE: u64 = 8 * KIB;
// Mirrors rpc/transport.rs's whole-message base allowance.
const WRITER_BASE_ALLOWANCE_SECONDS: u64 = 30;
// Mirrors rpc/transport.rs's whole-message size allowance at 16 KiB/s.
const WRITER_FLOOR_BYTES_PER_SECOND: u64 = 16 * KIB;
// 30 s + 256 KiB / 16 KiB/s = 46 s (conservatively omitting the RPC envelope).
const CONFIG_SNAPSHOT_WRITER_DEADLINE: Duration = Duration::from_secs(
    WRITER_BASE_ALLOWANCE_SECONDS + CONFIG_SNAPSHOT_BYTES as u64 / WRITER_FLOOR_BYTES_PER_SECOND,
);

/// Pins the 8 KiB/s establishment floor for current record-framed clients in
/// docs/superpowers/specs/2026-09-26-slow-link-establishment-design.md, Phase 1
/// "Test and live-validation plan": the first snapshot beats the writer deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_link_receives_a_full_config_snapshot_before_the_writer_deadline() {
    let temp = TempDir::new().expect("temporary base directory");
    let mut snapshot = json!({ "version": 1, "type": "snapshot", "config": { "padding": "" } });
    let padding_bytes =
        CONFIG_SNAPSHOT_BYTES - serde_json::to_vec(&snapshot).expect("snapshot JSON").len();
    snapshot["config"]["padding"] = Value::String("x".repeat(padding_bytes));
    let handle = start_server_with(&temp, move |registry| {
        registry.register_stream("fixture.configSnapshot", move |_, cancellation| {
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            let snapshot = snapshot.clone();
            tokio::spawn(async move {
                if sender.send(Ok(vec![snapshot])).await.is_ok() {
                    cancellation.cancelled().await;
                }
                // Keep the stream open after its one snapshot, until interrupted.
                drop(sender);
            });
            receiver
        });
    })
    .await;
    let host_key = host_public_key(temp.path());
    let server = handle.local_addr();
    // PlainLegacy shows no progress for about 32 s, beyond client liveness:
    // the design's pre-liveness-server floor is about 7.6–9.2 KiB/s.
    let trials = [Mode::PlainSplit, Mode::Encrypted].map(|mode| {
        let host_key = &host_key;
        async move {
            let label = format!("{mode:?} config snapshot at {CONFIG_SNAPSHOT_RATE} B/s");
            within_trial_deadline(&label, async {
                let proxy = ThrottleProxy::start(server, CONFIG_SNAPSHOT_RATE).await;
                let (mut socket, mut framing) = open(mode, proxy.address, host_key).await;
                let started = Instant::now();
                send_text(
                    &mut socket,
                    &mut framing,
                    &json!({
                        "_tag": "Request", "id": "1", "tag": "fixture.configSnapshot",
                        "payload": {}, "headers": [],
                    })
                    .to_string(),
                )
                .await;
                let outcome =
                    wait_for_response(&mut socket, &mut framing, started, "1", "Chunk").await;
                if outcome.is_ok() {
                    send_text(
                        &mut socket,
                        &mut framing,
                        r#"{"_tag":"Interrupt","requestId":"1"}"#,
                    )
                    .await;
                }
                let _ = socket.close(None).await;
                (mode, outcome)
            })
            .await
        }
    });
    let outcomes = join_all(trials).await;
    handle.shutdown();
    handle.join().await.expect("server joins");

    let minimum_transfer =
        Duration::from_secs_f64(0.9 * CONFIG_SNAPSHOT_BYTES as f64 / CONFIG_SNAPSHOT_RATE as f64);
    for (mode, outcome) in outcomes {
        let (after, chunk) = outcome.unwrap_or_else(|outcome| {
            panic!("{mode:?}: config snapshot ended before its first Chunk: {outcome:?}")
        });
        assert_eq!(
            chunk["_tag"], "Chunk",
            "{mode:?}: expected a snapshot Chunk, not a terminal Exit"
        );
        let values = chunk["values"].as_array().expect("stream values");
        assert_eq!(values.len(), 1, "{mode:?}: exactly one snapshot");
        let snapshot = &values[0];
        assert_eq!(snapshot["version"], 1, "{mode:?}: snapshot version");
        assert_eq!(snapshot["type"], "snapshot", "{mode:?}: snapshot type");
        assert_eq!(
            serde_json::to_vec(snapshot).expect("snapshot JSON").len(),
            CONFIG_SNAPSHOT_BYTES,
            "{mode:?}: exact padded snapshot size"
        );
        let padding = snapshot["config"]["padding"]
            .as_str()
            .expect("snapshot padding");
        assert_eq!(padding.len(), padding_bytes, "{mode:?}: full padding");
        assert!(
            padding.bytes().all(|byte| byte == b'x'),
            "{mode:?}: exact snapshot padding content"
        );
        assert!(
            after >= minimum_transfer,
            "{mode:?}: throttle did not hold: snapshot arrived in {after:?}, expected at least {minimum_transfer:?}"
        );
        assert!(
            after < CONFIG_SNAPSHOT_WRITER_DEADLINE,
            "{mode:?}: snapshot took {after:?}, exceeding the writer's {CONFIG_SNAPSHOT_WRITER_DEADLINE:?} message deadline at {CONFIG_SNAPSHOT_RATE} B/s"
        );
        eprintln!("{mode:?}: {CONFIG_SNAPSHOT_BYTES}-byte config snapshot arrived in {after:?}");
    }
}

/// At 1 MiB/s a 64 KiB record nominally drains in about 62 ms, under the server's
/// 100 ms backpressure wait, so D2's rule counts no write by rate alone. (On loopback
/// the kernel wakes a blocked writer only after a third of its send buffer drains, so
/// some writes here still wait longer; the Pong path on its own is pinned by
/// `an_idle_client_that_answers_pings_is_kept_past_the_silence_limit`.) The trial pins
/// that a transfer in this band, longer than the 45 s silence limit plus the 5 s check
/// interval, completes exactly without a reap.
const FAST_LINK_RATE: u64 = 1024 * KIB;
const FAST_LINK_BYTES: usize = 52 * MIB;
const REAP_WINDOW: Duration = Duration::from_secs(50);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fast_transfer_longer_than_the_silence_limit_completes_without_a_reap() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_server(&temp).await;
    let host_key = host_public_key(temp.path());
    let server = handle.local_addr();
    // One at a time: each trial holds several copies of its payload.
    for mode in [Mode::PlainSplit, Mode::Encrypted] {
        let trial = Trial {
            mode,
            rate: FAST_LINK_RATE,
            bytes: FAST_LINK_BYTES,
        };
        let outcome = run_trial(server, &host_key, trial).await;
        match outcome {
            Outcome::Completed(after) => assert!(
                after > REAP_WINDOW,
                "{trial:?} must outlast the reap window to prove anything (took {after:?})"
            ),
            other => panic!("{trial:?} ended without its exact payload: {other:?}"),
        }
    }
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn item_one_client_detects_a_frozen_server_within_thirty_three_seconds() {
    let temp = TempDir::new().expect("temporary root");
    let handle = start_server(&temp).await;
    let proxy = ThrottleProxy::start(handle.local_addr(), 1024 * KIB).await;
    let (mut socket, mut framing) = open(Mode::PlainSplit, proxy.address, &[]).await;
    request_bytes(&mut socket, &mut framing, "1", 1024).await;
    assert!(matches!(
        within_trial_deadline(
            "pre-freeze exchange",
            wait_for_exit(&mut socket, &mut framing, Instant::now(), "1", 1024),
        )
        .await,
        Outcome::Completed(_)
    ));
    proxy.freeze(true);
    let frozen_at = Instant::now();
    let result = timeout(
        Duration::from_secs(33),
        wait_for_exit(&mut socket, &mut framing, frozen_at, "999", 0),
    )
    .await
    .expect("client detects silence before 33 s while freeze remains active");
    assert!(proxy.frozen.load(Ordering::Relaxed));
    assert!(
        matches!(result, Outcome::Dead(after)
        if after >= Duration::from_secs(27) && after <= Duration::from_secs(33)),
        "frozen-server result: {result:?}"
    );
    handle.shutdown();
    handle.join().await.expect("server joins");
}

async fn start_observed_server(
    temp: &TempDir,
) -> (
    ServerHandle,
    tokio::sync::mpsc::Receiver<()>,
    tokio::sync::mpsc::Receiver<Instant>,
) {
    let (started_tx, started) = tokio::sync::mpsc::channel(1);
    let (ended_tx, ended) = tokio::sync::mpsc::channel(1);
    let handle = start_server_with(temp, move |registry| {
        // A small response after a delay: the server pushing data at its own pace.
        registry.register_unary("fixture.delayed", |request, _cancellation| async move {
            let delay_ms = request.payload["delayMs"].as_u64().expect("delayMs");
            let bytes = request.payload["bytes"]
                .as_u64()
                .and_then(|bytes| usize::try_from(bytes).ok())
                .expect("bytes");
            sleep(Duration::from_millis(delay_ms)).await;
            Ok(json!({ "data": "x".repeat(bytes) }))
        });
        registry.register_stream("fixture.watch", move |_, cancellation| {
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            let started = started_tx.clone();
            let ended = ended_tx.clone();
            tokio::spawn(async move {
                started.send(()).await.expect("watch started");
                cancellation.cancelled().await;
                sender.closed().await;
                let _ = ended.send(Instant::now()).await;
            });
            receiver
        });
    })
    .await;
    (handle, started, ended)
}

async fn watch_session(socket: &mut TestSocket, framing: &mut Framing) {
    send_text(
        socket,
        framing,
        &json!({
            "_tag": "Request", "id": "100", "tag": "fixture.watch", "payload": {}, "headers": [],
        })
        .to_string(),
    )
    .await;
}

/// A freeze case's setup: an observed server behind a throttling proxy and a
/// socket whose `fixture.watch` subscription is running.
struct WatchedSession {
    _temp: TempDir,
    handle: ServerHandle,
    proxy: ThrottleProxy,
    socket: TestSocket,
    framing: Framing,
    ended: tokio::sync::mpsc::Receiver<Instant>,
}

/// Builds a [`WatchedSession`] under [`TRIAL_DEADLINE`], so a server that
/// upgrades but never starts the subscription fails the test by name.
async fn open_watched_session(mode: Mode, rate: u64) -> WatchedSession {
    let label = format!("{mode:?} watched-session setup at {rate} B/s");
    within_trial_deadline(&label, async {
        let temp = TempDir::new().expect("temporary root");
        let (handle, mut started, ended) = start_observed_server(&temp).await;
        let proxy = ThrottleProxy::start(handle.local_addr(), rate).await;
        let (mut socket, mut framing) =
            open(mode, proxy.address, &host_public_key(temp.path())).await;
        watch_session(&mut socket, &mut framing).await;
        started.recv().await.expect("subscription established");
        WatchedSession {
            _temp: temp,
            handle,
            proxy,
            socket,
            framing,
            ended,
        }
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reader_frozen_mid_transfer_is_dropped_by_the_server() {
    for mode in [Mode::PlainSplit, Mode::Encrypted] {
        let WatchedSession {
            _temp,
            handle,
            proxy,
            mut socket,
            mut framing,
            mut ended,
        } = open_watched_session(mode, 256 * KIB).await;
        request_bytes(&mut socket, &mut framing, "1", 8 * MIB).await;
        sleep(Duration::from_secs(3)).await;
        proxy.freeze(true);
        let frozen_at = Instant::now();
        let closed_at = timeout(Duration::from_secs(33), ended.recv())
            .await
            .expect("session and subscription end within 33 s while frozen")
            .expect("teardown timestamp");
        assert!(proxy.frozen.load(Ordering::Relaxed));
        assert!(closed_at.duration_since(frozen_at) <= Duration::from_secs(33));
        handle.shutdown();
        handle.join().await.expect("server joins");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_client_that_goes_silent_is_reaped_by_the_heartbeat() {
    for mode in [Mode::PlainSplit, Mode::Encrypted] {
        let WatchedSession {
            _temp,
            handle,
            proxy,
            mut socket,
            mut framing,
            mut ended,
        } = open_watched_session(mode, 1024 * KIB).await;
        request_bytes(&mut socket, &mut framing, "1", 1024).await;
        assert!(matches!(
            within_trial_deadline(
                "pre-freeze exchange",
                wait_for_exit(&mut socket, &mut framing, Instant::now(), "1", 1024),
            )
            .await,
            Outcome::Completed(_)
        ));
        proxy.freeze(true);
        let frozen_at = Instant::now();
        let closed_at = timeout(Duration::from_secs(51), ended.recv())
            .await
            .expect("50 s idle bound plus 1 s scheduling tolerance")
            .expect("teardown timestamp");
        assert!(proxy.frozen.load(Ordering::Relaxed));
        assert!(closed_at.duration_since(frozen_at) <= Duration::from_secs(51));
        handle.shutdown();
        handle.join().await.expect("server joins");
    }
}

/// An idle page still receives small frames (the local-server discovery stream sends
/// about one a second). Writes the socket accepts at once prove nothing about a frozen
/// peer, so they must not move the silence origin past the 50 s bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_client_is_reaped_while_the_server_keeps_pushing_small_frames() {
    for mode in [Mode::PlainSplit, Mode::Encrypted] {
        let WatchedSession {
            _temp,
            handle,
            proxy,
            mut socket,
            mut framing,
            mut ended,
        } = open_watched_session(mode, 1024 * KIB).await;
        // One 2.8 KB response a second for 20 s after the freeze.
        for second in 1..=20_u64 {
            request_delayed(
                &mut socket,
                &mut framing,
                &(200 + second).to_string(),
                second * 1_000,
                2_800,
            )
            .await;
        }
        // Ordered delivery: once this answers, the server holds every delayed request.
        request_bytes(&mut socket, &mut framing, "1", 1024).await;
        assert!(matches!(
            within_trial_deadline(
                "pre-freeze exchange",
                wait_for_exit(&mut socket, &mut framing, Instant::now(), "1", 1024),
            )
            .await,
            Outcome::Completed(_)
        ));
        proxy.freeze(true);
        let frozen_at = Instant::now();
        let closed_at = timeout(Duration::from_secs(51), ended.recv())
            .await
            .expect("reaped within 51 s of freeze start while small writes are accepted")
            .expect("teardown timestamp");
        assert!(proxy.frozen.load(Ordering::Relaxed));
        assert!(closed_at.duration_since(frozen_at) <= Duration::from_secs(51));
        handle.shutdown();
        handle.join().await.expect("server joins");
    }
}

/// While an idle client only reads, the server writes nothing and the client sends no
/// request, so its automatic Pongs to the heartbeat Pings are its only inbound frames:
/// they alone must keep the session past the 45 s silence limit. They are also what
/// keeps a fast transfer alive when no data write waits long enough to count.
const PONG_ONLY_WINDOW: Duration = Duration::from_secs(60);
const PONG_ONLY_DEADLINE: Duration = Duration::from_secs(180);
const PONG_ONLY_MIN_PINGS: usize = 4;

/// Exercises the socket reader without sending application messages during the window.
async fn observe_answered_heartbeats<S>(socket: &mut S, mode: Mode)
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    let window = sleep(PONG_ONLY_WINDOW);
    tokio::pin!(window);
    let mut window_elapsed = false;
    let mut pings = 0;
    timeout(PONG_ONLY_DEADLINE, async {
        // A late server check restarts the Ping cadence. Observe enough actual
        // heartbeats as well as the minimum survival window, without demanding
        // a cadence from a process that may have been descheduled.
        while !window_elapsed || pings < PONG_ONLY_MIN_PINGS {
            tokio::select! {
                // Reading flushes the automatic Pong queued for the previous Ping.
                frame = socket.next() => match frame {
                    Some(Ok(Message::Ping(_))) => pings += 1,
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        panic!("{mode:?} session ended while the client answered Pings: {frame:?}");
                    }
                    Some(Ok(_)) => {}
                },
                () = &mut window, if !window_elapsed => window_elapsed = true,
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{mode:?}: heartbeat observation exceeded {PONG_ONLY_DEADLINE:?} (saw {pings} Pings)"
        )
    });
}

#[tokio::test(start_paused = true)]
async fn pong_only_observation_waits_for_heartbeats_after_a_scheduler_stall() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = futures_util::stream::poll_fn(move |cx| receiver.poll_recv(cx));
    let observation = observe_answered_heartbeats(&mut socket, Mode::PlainSplit);
    tokio::pin!(observation);
    assert!(futures_util::poll!(&mut observation).is_pending());
    sender.send(Ok(Message::Ping(Vec::new().into()))).unwrap();
    sender.send(Ok(Message::Ping(Vec::new().into()))).unwrap();
    assert!(futures_util::poll!(&mut observation).is_pending());

    // A stalled process may receive fewer Pings while its wall-clock window expires.
    // Production restarts its heartbeat cadence rather than emitting a catch-up burst.
    tokio::time::advance(Duration::from_secs(70)).await;
    assert!(futures_util::poll!(&mut observation).is_pending());
    sender.send(Ok(Message::Ping(Vec::new().into()))).unwrap();
    assert!(futures_util::poll!(&mut observation).is_pending());
    sender.send(Ok(Message::Ping(Vec::new().into()))).unwrap();
    assert!(futures_util::poll!(&mut observation).is_ready());
}

#[tokio::test(start_paused = true)]
async fn pong_only_observation_keeps_the_full_survival_window() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut socket = futures_util::stream::poll_fn(move |cx| receiver.poll_recv(cx));
    let observation = observe_answered_heartbeats(&mut socket, Mode::Encrypted);
    tokio::pin!(observation);
    assert!(futures_util::poll!(&mut observation).is_pending());
    for _ in 0..PONG_ONLY_MIN_PINGS {
        sender.send(Ok(Message::Ping(Vec::new().into()))).unwrap();
    }
    assert!(futures_util::poll!(&mut observation).is_pending());
    tokio::time::advance(Duration::from_secs(59)).await;
    assert!(futures_util::poll!(&mut observation).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(futures_util::poll!(&mut observation).is_ready());
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "heartbeat observation exceeded")]
async fn pong_only_observation_remains_bounded_when_heartbeats_never_arrive() {
    let mut socket = futures_util::stream::pending();
    observe_answered_heartbeats(&mut socket, Mode::PlainSplit).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_client_that_answers_pings_is_kept_past_the_silence_limit() {
    let cases = [Mode::PlainSplit, Mode::Encrypted].map(|mode| async move {
        let WatchedSession {
            _temp,
            handle,
            proxy: _proxy,
            mut socket,
            mut framing,
            mut ended,
        } = open_watched_session(mode, 1024 * KIB).await;
        observe_answered_heartbeats(&mut socket, mode).await;
        assert!(
            ended.try_recv().is_err(),
            "{mode:?}: not reaped while the client answered Pings"
        );
        request_bytes(&mut socket, &mut framing, "2", 1024).await;
        let result = within_trial_deadline(
            "post-idle exchange",
            wait_for_exit(&mut socket, &mut framing, Instant::now(), "2", 1024),
        )
        .await;
        assert!(
            matches!(result, Outcome::Completed(_)),
            "{mode:?}: {result:?}"
        );
        handle.shutdown();
        handle.join().await.expect("server joins");
    });
    join_all(cases).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_client_silent_for_thirty_seconds_is_kept() {
    let WatchedSession {
        _temp,
        handle,
        proxy,
        mut socket,
        mut framing,
        mut ended,
    } = open_watched_session(Mode::PlainSplit, 1024 * KIB).await;
    proxy.freeze(true);
    sleep(Duration::from_secs(30)).await;
    assert!(ended.try_recv().is_err(), "not reaped under the 45 s limit");
    proxy.freeze(false);
    request_bytes(&mut socket, &mut framing, "2", 1024).await;
    let result = within_trial_deadline(
        "post-thaw exchange",
        wait_for_exit(&mut socket, &mut framing, Instant::now(), "2", 1024),
    )
    .await;
    assert!(matches!(result, Outcome::Completed(_)), "{result:?}");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn proxy_upstream_rate_is_enforced() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = ThrottleProxy::start_duplex(listener.local_addr().unwrap(), 0, 32 * KIB).await;
    let mut sender = TcpStream::connect(proxy.address).await.unwrap();
    let read = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let start = Instant::now();
        let mut bytes = vec![0_u8; 64 * 1024];
        socket.read_exact(&mut bytes).await.unwrap();
        assert!(bytes.iter().all(|byte| *byte == 73));
        start.elapsed()
    });
    sender.write_all(&vec![73; 64 * 1024]).await.unwrap();
    let elapsed = timeout(Duration::from_secs(5), read)
        .await
        .unwrap()
        .unwrap();
    assert!(
        elapsed >= Duration::from_millis(1900),
        "upstream was unpaced: {elapsed:?}"
    );
}

async fn staged_upload_trial(mode: Mode, up_rate: u64) {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use coddy_studio_server::transfer::staging::{UploadClock, UploadLimits, UploadRegistry};
    use sha2::{Digest, Sha256};
    within_trial_deadline("staged upload", async {
        let temp = TempDir::new().unwrap();
        let directory = temp.path().join("staged-chat");
        let uploads = UploadRegistry::new(directory.clone(), UploadLimits::default(), Arc::new(Instant::now) as UploadClock);
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
        let (ended_tx, mut ended_rx) = tokio::sync::mpsc::channel(1);
        let handle = start_server_with(&temp, |registry| {
            coddy_studio_server::production::uploads_rpc::register_uploads_rpc(registry, uploads.clone(), temp.path().join("terminal-pastes"));
            registry.register_stream("fixture.watch", move |_, cancellation| {
                let (sender, receiver) = tokio::sync::mpsc::channel(1);
                let started = started_tx.clone(); let ended = ended_tx.clone();
                tokio::spawn(async move { started.send(()).await.unwrap(); cancellation.cancelled().await; sender.closed().await; let _ = ended.send(Instant::now()).await; });
                receiver
            });
        }).await;
        let proxy = ThrottleProxy::start_duplex(handle.local_addr(), 0, up_rate).await;
        let (mut socket, mut framing) = open(mode, proxy.address, &host_public_key(temp.path())).await;
        watch_session(&mut socket, &mut framing).await;
        started_rx.recv().await.unwrap();
        let bytes: Vec<u8> = (0..3 * MIB).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let digest: String = Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect();
        send_text(&mut socket, &mut framing, &json!({"_tag":"Request","id":"1","tag":"uploads.begin","payload":{"target":{"_tag":"chat-attachment","type":"file","name":"slow.bin","mimeType":"application/octet-stream"},"sizeBytes":bytes.len(),"sha256":digest},"headers":[]}).to_string()).await;
        let (_, begin) = wait_for_response(&mut socket, &mut framing, Instant::now(), "1", "Exit").await.unwrap();
        assert_eq!(begin["exit"]["_tag"], "Success", "{begin}");
        let upload_id = begin["exit"]["value"]["uploadId"].as_str().unwrap().to_owned();
        let start = Instant::now();
        let mut next = 0_usize;
        let mut pending = std::collections::HashSet::new();
        let mut received = 0_u64;
        let mut assembly = Assembly::default();
        let mut last_inbound = Instant::now();
        let mut probe_sent = false;
        let mut responsiveness_ping = None;
        let mut pong_latency = None;
        while received < bytes.len() as u64 {
            while pending.len() < 2 && next < bytes.len() {
                let end = (next + 64 * 1024).min(bytes.len()); let id = format!("{}", 1000 + next);
                send_text(&mut socket, &mut framing, &json!({"_tag":"Request","id":id,"tag":"uploads.append","payload":{"uploadId":upload_id,"offset":next,"data":STANDARD.encode(&bytes[next..end])},"headers":[]}).to_string()).await;
                pending.insert(id); next = end;
            }
            if received > 0 && responsiveness_ping.is_none() {
                responsiveness_ping = Some(Instant::now());
                send_text(&mut socket, &mut framing, r#"{"_tag":"Ping"}"#).await;
            }
            let deadline = last_inbound + if probe_sent { DEAD_AFTER } else { PROBE_AFTER };
            let frame = tokio::select! {
                frame = socket.next() => frame.expect("socket remains live").expect("WebSocket frame"),
                () = sleep_until(deadline) => { assert!(!probe_sent, "client would close 4408 during staged upload"); send_text(&mut socket, &mut framing, r#"{"_tag":"Ping"}"#).await; probe_sent = true; continue; }
            };
            assert!(!matches!(frame, Message::Close(_)), "staged upload closed: {frame:?}");
            if matches!(frame, Message::Text(_) | Message::Binary(_)) { last_inbound = Instant::now(); probe_sent = false; }
            let Some(message) = reassemble(&mut framing, &mut assembly, frame) else { continue; };
            let value: Value = serde_json::from_slice(&message).unwrap();
            if value["_tag"] == "Pong" && let Some(ping) = responsiveness_ping { pong_latency.get_or_insert(ping.elapsed()); }
            if value["_tag"] == "Exit" && pending.remove(value["requestId"].as_str().unwrap_or("")) {
                assert_eq!(value["exit"]["_tag"], "Success", "{value}");
                received = received.max(value["exit"]["value"]["receivedBytes"].as_u64().unwrap());
            }
            assert!(ended_rx.try_recv().is_err(), "server reaped active staged sender");
        }
        let elapsed = start.elapsed();
        assert!(elapsed > Duration::from_secs(45), "upstream pacing did not constrain upload: {elapsed:?}");
        // Two 64KiB base64 chunks at the configured uplink rate, plus 2s for
        // WebSocket headers, scheduler delays and the independent response direction.
        let latency_limit = Duration::from_secs_f64(2.0 * 64.0 * 1024.0 * 4.0 / 3.0 / up_rate as f64) + Duration::from_secs(2);
        assert!(pong_latency.unwrap() <= latency_limit, "Ping delayed behind staged upload: {pong_latency:?}");
        send_text(&mut socket, &mut framing, &json!({"_tag":"Request","id":"2","tag":"uploads.get","payload":{"uploadId":upload_id},"headers":[]}).to_string()).await;
        let (_, get) = wait_for_response(&mut socket, &mut framing, start, "2", "Exit").await.unwrap();
        assert_eq!(get["exit"]["_tag"], "Success", "{get}");
        assert_eq!(get["exit"]["value"]["complete"], true);
        assert_eq!(get["exit"]["value"]["receivedBytes"], bytes.len());
        let stored = tokio::fs::read(directory.join(format!("{upload_id}.upload"))).await.unwrap();
        assert_eq!(stored, bytes);
        let stored_digest: String = Sha256::digest(&stored).iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(stored_digest, digest);
        assert!(ended_rx.try_recv().is_err());
        println!("staged {mode:?}: elapsed={elapsed:?}, pong={pong_latency:?}");
        socket.close(None).await.unwrap(); uploads.shutdown().await;
        handle.shutdown(); handle.join().await.unwrap();
    }).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_three_mib_uploads_survive_plain_and_encrypted_slow_uplinks() {
    tokio::join!(
        staged_upload_trial(Mode::PlainSplit, 64 * KIB),
        staged_upload_trial(Mode::Encrypted, 64 * KIB)
    );
}

#[derive(Debug)]
struct InlineTrialOutcome {
    client_liveness_close: Option<u16>,
    server_reaped: bool,
    elapsed: Duration,
}
async fn inline_upload_trial(mode: Mode, up_rate: u64) -> InlineTrialOutcome {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    within_trial_deadline("inline upload residual", async {
        let temp = TempDir::new().unwrap();
        let (handle, mut started, mut ended) = start_observed_server(&temp).await;
        let proxy = ThrottleProxy::start_duplex(handle.local_addr(), 0, up_rate).await;
        let (mut socket, mut framing) = open(mode, proxy.address, &host_public_key(temp.path())).await;
        watch_session(&mut socket, &mut framing).await;
        started.recv().await.unwrap();
        // This known fixture method accepts a large request and returns a small
        // success; it exercises the same transport framing as an inline turn.
        let request = json!({"_tag":"Request","id":"200","tag":"fixture.bytes","payload":{"bytes":0,"dataUrl":format!("data:application/octet-stream;base64,{}",STANDARD.encode(vec![91; 3 * MIB]))},"headers":[]}).to_string();
        let frames: Vec<Message> = match &mut framing {
            Framing::Whole | Framing::Records => vec![Message::Text(request.into())],
            Framing::Encrypted(transport) => {
                let chunks: Vec<_> = request.as_bytes().chunks(MAX_CHUNK_BYTES).collect();
                chunks.iter().enumerate().map(|(i, chunk)| Message::Binary(encrypt_record(transport, u8::from(i + 1 != chunks.len()), chunk).into())).collect()
            }
        };
        let (mut writer, mut reader) = socket.split();
        let sender = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            for frame in frames { if writer.send(frame).await.is_err() { break; } }
            // Keep the raw peer open, even when the monitor would close 4408,
            // to measure the server's heartbeat/reap decision independently.
            sleep(Duration::from_secs(100)).await;
        }));
        let start = Instant::now();
        let mut last_inbound = start;
        let mut client_liveness_close = None;
        let mut assembly = Assembly::default();
        let mut server_reaped = false;
        loop {
            let frame = tokio::select! {
                _ = ended.recv() => { server_reaped = true; break; }
                () = sleep_until(last_inbound + DEAD_AFTER), if client_liveness_close.is_none() => { client_liveness_close = Some(4408); continue; }
                frame = reader.next() => frame,
            };
            let Some(Ok(frame)) = frame else { server_reaped = true; break; };
            if matches!(frame, Message::Close(_)) { server_reaped = true; break; }
            if matches!(frame, Message::Text(_) | Message::Binary(_)) { last_inbound = Instant::now(); }
            let Some(message) = reassemble(&mut framing, &mut assembly, frame) else { continue; };
            let value: Value = serde_json::from_slice(&message).unwrap();
            if value["requestId"] == "200" && value["_tag"] == "Exit" { assert_eq!(value["exit"]["_tag"], "Success"); break; }
        }
        let outcome = InlineTrialOutcome { client_liveness_close, server_reaped, elapsed: start.elapsed() };
        println!("inline {mode:?}: {outcome:?}");
        drop(sender); drop(reader); handle.shutdown(); handle.join().await.unwrap();
        outcome
    }).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inline_three_mib_upload_records_the_existing_residual() {
    let (plain, encrypted) = tokio::join!(
        inline_upload_trial(Mode::PlainSplit, 64 * KIB),
        inline_upload_trial(Mode::Encrypted, 64 * KIB)
    );
    assert!(plain.server_reaped);
    assert_eq!(plain.client_liveness_close, Some(4408));
    assert!(encrypted.elapsed > Duration::from_secs(30));
    // Record, rather than prescribe, Noise's independent client/server result.
    println!(
        "inline client close: {:?}; server reaped: {}",
        encrypted.client_liveness_close, encrypted.server_reaped
    );
}
