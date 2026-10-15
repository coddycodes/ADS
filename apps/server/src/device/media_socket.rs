//! `GET /api/device-media` and `GET /api/device-media-e2ee`: WebSockets that carry one
//! device's H.264 frames (spec §4.2–4.3). They take no cookie or bearer credential: the
//! first message is a single-use ticket from `device.createMediaTicket`, never the URL.
//!
//! The plain route sends the ticket as a text message and frames as binary messages. The
//! E2EE route, for profiles that pin the host key, first runs Noise_NK with that key as
//! `/ws-e2ee` does, under the prologue [`DEVICE_MEDIA_NOISE_PROLOGUE`]; from then on every
//! message, the ticket included, is a logical message in encrypted records (the `/ws-e2ee`
//! record format), and only a ticket minted over E2EE is accepted. A client that pinned
//! another key or prologue is closed with code 4403 `host-identity`.
//!
//! A plain socket has 5 s for its ticket; an E2EE socket has 10 s for the handshake and the
//! ticket together.
//!
//! Rejections close with code 4400 and a reason the client acts on:
//!
//! | Reason | Cause |
//! | --- | --- |
//! | `expired` | the ticket expired, is unknown or malformed, or did not arrive in time |
//! | `reused` | the ticket was already used |
//! | `wrongChannel` | the ticket was minted over the other channel |
//! | `wrongDevice` | the ticket's device is no longer known |
//! | `limit` | the device already has four media sockets |
//! | `disabled` | device previews were turned off |
//! | `stopped` | the device stopped, failed, or is not ready |
//! | `failed` | the device's video stream failed |
//! | `unauthorized` | the ticket's session was revoked or expired |
//! | `slow` | the client accepted no frame for 10 s |
//!
//! Before the upgrade, connections to either route are admitted against the same global,
//! per-peer, and per-network pre-auth budgets as `/ws-e2ee`, from one pool of their own so
//! media sockets cannot starve RPC handshakes; a refused one gets `503` and is never
//! upgraded. A socket leaves the pool once its ticket is redeemed.

use std::{net::IpAddr, time::Duration};

use axum::{
    extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::{
    Sink, SinkExt, Stream, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::Deserialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{DeviceManager, media::MediaChannel};
use crate::{
    auth::AuthService,
    rpc::{
        E2EE_HANDSHAKE_TIMEOUT, E2eeChannel, E2eePreauthAdmission, E2eePreauthLease,
        E2eeSessionError, accept_noise_nk, next_binary_frame,
    },
};

/// How long a plain socket may take to send its ticket.
pub(crate) const TICKET_WAIT: Duration = Duration::from_secs(5);
/// The E2EE route's Noise prologue. It binds a handshake to this route, so a media
/// handshake cannot be replayed into `/ws-e2ee` or the reverse; clients must use the same
/// bytes.
pub(crate) const DEVICE_MEDIA_NOISE_PROLOGUE: &[u8] = b"bibcode-device-media/1";
/// How long one frame may wait for the client before the socket closes as `slow`.
pub(crate) const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a close frame may take; the socket is dropped after it either way.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Inbound messages are only the ticket and `{"type":"keyframe"}`; on the E2EE route this
/// bounds the decrypted logical message.
const MAX_INBOUND_BYTES: usize = 1024;
/// The largest E2EE record read: a whole inbound message, its flag byte, and the Noise tag.
/// Noise_NK's message A (48 bytes) fits too.
const MAX_E2EE_INBOUND_RECORD_BYTES: usize = MAX_INBOUND_BYTES + 1 + 16;
const READ_BUFFER_BYTES: usize = 4 * 1024;
const WRITE_BUFFER_BYTES: usize = 16 * 1024;
const CLOSE_CODE: u16 = 4400;

/// What the media routes need: the devices, the sessions tickets are bound to (and the host
/// identity the E2EE route answers with), and their own pre-auth admission pool.
#[derive(Clone)]
pub(crate) struct MediaSocketState {
    pub devices: DeviceManager,
    pub auth: AuthService,
    pub admission: E2eePreauthAdmission,
    /// [`SEND_STALL_TIMEOUT`] outside tests.
    pub send_stall: Duration,
    /// How long an E2EE socket has for the handshake and its ticket together;
    /// [`E2EE_HANDSHAKE_TIMEOUT`] outside tests.
    pub e2ee_preauth: Duration,
}

impl MediaSocketState {
    pub(crate) fn new(devices: DeviceManager, auth: AuthService) -> Self {
        Self {
            devices,
            auth,
            admission: E2eePreauthAdmission::new(),
            send_stall: SEND_STALL_TIMEOUT,
            e2ee_preauth: E2EE_HANDSHAKE_TIMEOUT,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Inbound {
    Ticket { ticket: String },
    Keyframe,
}

/// What the client sent.
enum Received {
    /// One whole message: a text message, or a decrypted logical message.
    Message(Vec<u8>),
    /// A message the route does not act on.
    Ignored,
    /// The client left or broke the framing.
    Gone,
}

/// The framing of one media socket; everything after it is shared by both routes.
trait MediaTransport {
    /// The next client message. Cancel-safe: a partly received message is kept.
    async fn recv(&mut self) -> Received;
    /// Sends one encoded unit; `Err` once the client is gone.
    async fn send(&mut self, frame: Bytes) -> Result<(), ()>;
    /// Sends a close frame with code 4400 and `reason`.
    async fn close(self, reason: &'static str);
}

/// Plain WebSocket messages: text in, binary out.
struct PlainTransport<S>(S);

impl<S> MediaTransport for PlainTransport<S>
where
    S: Sink<Message, Error = axum::Error> + Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    async fn recv(&mut self) -> Received {
        match self.0.next().await {
            Some(Ok(Message::Text(text))) => Received::Message(text.as_bytes().to_vec()),
            Some(Ok(Message::Close(_)) | Err(_)) | None => Received::Gone,
            Some(Ok(_)) => Received::Ignored,
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), ()> {
        self.0.send(Message::Binary(frame)).await.map_err(drop)
    }

    async fn close(self, reason: &'static str) {
        close(self.0, reason).await;
    }
}

/// Noise records on a WebSocket, after the handshake.
struct E2eeTransport {
    writer: SplitSink<WebSocket, Message>,
    reader: SplitStream<WebSocket>,
    channel: E2eeChannel,
}

impl MediaTransport for E2eeTransport {
    async fn recv(&mut self) -> Received {
        loop {
            let Some(record) = next_binary_frame(&mut self.reader).await else {
                return Received::Gone;
            };
            // Continuations accumulate in the channel, so a cancelled read loses nothing.
            match self.channel.decrypt_frame(&record, MAX_INBOUND_BYTES) {
                Ok(Some(message)) => return Received::Message(message),
                Ok(None) => {}
                Err(_) => {
                    tracing::warn!("device media client sent an invalid E2EE record");
                    return Received::Gone;
                }
            }
        }
    }

    async fn send(&mut self, frame: Bytes) -> Result<(), ()> {
        let records = self.channel.encrypt_message(&frame).map_err(|_| {
            tracing::warn!("device media frame could not be encrypted");
        })?;
        for record in records {
            self.writer
                .feed(Message::Binary(record.into()))
                .await
                .map_err(drop)?;
        }
        self.writer.flush().await.map_err(drop)
    }

    async fn close(self, reason: &'static str) {
        close(self.writer, reason).await;
    }
}

/// Admits and upgrades a plain media socket from `peer_ip` (see
/// `crate::rpc::effective_preauth_peer`); the ticket is checked once it is open.
pub(crate) fn upgrade_plain(
    state: MediaSocketState,
    peer_ip: IpAddr,
    upgrade: WebSocketUpgrade,
) -> Response {
    admit_and_upgrade(
        state,
        peer_ip,
        upgrade,
        MAX_INBOUND_BYTES,
        |socket, state, lease| {
            let ticket_deadline = Instant::now() + TICKET_WAIT;
            serve(
                PlainTransport(socket),
                state,
                MediaChannel::Plain,
                lease,
                ticket_deadline,
            )
        },
    )
}

/// Admits and upgrades an E2EE media socket from `peer_ip`; the handshake and then the
/// ticket are checked once it is open.
pub(crate) fn upgrade_e2ee(
    state: MediaSocketState,
    peer_ip: IpAddr,
    upgrade: WebSocketUpgrade,
) -> Response {
    admit_and_upgrade(
        state,
        peer_ip,
        upgrade,
        MAX_E2EE_INBOUND_RECORD_BYTES,
        serve_e2ee,
    )
}

/// Admits a connection from `peer_ip` into the media routes' pre-auth pool, or refuses it
/// with `503` before the upgrade; an admitted socket reads messages of at most
/// `max_inbound` bytes.
fn admit_and_upgrade<F, Fut>(
    state: MediaSocketState,
    peer_ip: IpAddr,
    upgrade: WebSocketUpgrade,
    max_inbound: usize,
    serve: F,
) -> Response
where
    F: FnOnce(WebSocket, MediaSocketState, E2eePreauthLease) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let lease = match state
        .admission
        .try_admit(peer_ip, tokio::time::Instant::now())
    {
        Ok(lease) => lease,
        Err(reason) => {
            tracing::warn!(reason, "device media socket refused before upgrade");
            return (StatusCode::SERVICE_UNAVAILABLE, reason).into_response();
        }
    };
    upgrade
        .max_message_size(max_inbound)
        .max_frame_size(max_inbound)
        .read_buffer_size(READ_BUFFER_BYTES)
        .write_buffer_size(WRITE_BUFFER_BYTES)
        .on_upgrade(move |socket| serve(socket, state, lease))
}

/// Answers the client's Noise_NK handshake under [`DEVICE_MEDIA_NOISE_PROLOGUE`], then
/// serves the socket over the channel. The handshake and the ticket share one pre-auth
/// deadline, as the handshake and credential do on `/ws-e2ee`.
async fn serve_e2ee(socket: WebSocket, state: MediaSocketState, lease: E2eePreauthLease) {
    let deadline = Instant::now() + state.e2ee_preauth;
    let (mut writer, mut reader) = socket.split();
    let handshake = accept_noise_nk(
        &mut writer,
        &mut reader,
        state.auth.host_identity(),
        DEVICE_MEDIA_NOISE_PROLOGUE,
        deadline,
    )
    .await;
    match handshake {
        Ok(channel) => {
            let transport = E2eeTransport {
                writer,
                reader,
                channel,
            };
            serve(transport, state, MediaChannel::E2ee, lease, deadline).await;
        }
        // Already closed with `host-identity`.
        Err(E2eeSessionError::Handshake) => {
            tracing::warn!("device media client pinned another host key or prologue");
        }
        Err(E2eeSessionError::Closed) => {}
        Err(error) => {
            if matches!(error, E2eeSessionError::Timeout) {
                tracing::warn!("device media E2EE handshake timed out");
            } else {
                tracing::warn!("device media E2EE handshake failed");
            }
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, writer.close()).await;
        }
    }
}

/// Redeems the socket's ticket for `channel`, then streams the device until either side
/// ends. `lease` holds the socket's pre-auth slot until the ticket is redeemed, which must
/// arrive before `ticket_deadline`.
async fn serve<T: MediaTransport>(
    mut socket: T,
    state: MediaSocketState,
    channel: MediaChannel,
    lease: E2eePreauthLease,
    ticket_deadline: Instant,
) {
    let ticket = match tokio::time::timeout_at(ticket_deadline, socket.recv()).await {
        Err(_) => {
            tracing::warn!("device media socket sent no ticket in time");
            return socket.close("expired").await;
        }
        Ok(Received::Message(message)) => match serde_json::from_slice(&message) {
            Ok(Inbound::Ticket { ticket }) => ticket,
            // The parse error could quote the ticket, so it is not logged.
            _ => {
                tracing::warn!("device media socket's first message was not a ticket");
                return socket.close("expired").await;
            }
        },
        Ok(Received::Ignored) => {
            tracing::warn!("device media socket's first message was not a ticket");
            return socket.close("expired").await;
        }
        // The client left before it sent a ticket.
        Ok(Received::Gone) => return,
    };
    let (device_id, principal) = match state.devices.redeem_media_ticket(&ticket, channel) {
        Ok(redeemed) => redeemed,
        Err(error) => {
            let reason = error.close_reason();
            tracing::warn!(reason, "device media ticket refused");
            return socket.close(reason).await;
        }
    };
    drop(ticket);
    // A redeemed ticket authenticates the socket; it leaves the pre-auth pool.
    drop(lease);
    // The session the ticket was minted for must still be live, and the socket ends with
    // it, as `/ws` does.
    let Some(expires_at_ms) = state.auth.active_session_expiry(&principal).await else {
        tracing::warn!("device media ticket's session is no longer active");
        return socket.close("unauthorized").await;
    };
    let session_shutdown = CancellationToken::new();
    let Ok(connection_guard) = state
        .auth
        .mark_connected_guard(&principal, session_shutdown.clone())
        .await
    else {
        tracing::warn!("device media ticket's session is no longer active");
        return socket.close("unauthorized").await;
    };
    let expiration_guard =
        crate::http::spawn_session_expiration_guard(expires_at_ms, session_shutdown.clone());
    match state.devices.attach_media(&device_id) {
        Ok(subscriber) => {
            let reason = stream(
                &mut socket,
                subscriber,
                &state.devices,
                &session_shutdown,
                state.send_stall,
            )
            .await;
            if let Some(reason) = reason {
                socket.close(reason).await;
            }
        }
        Err(error) => {
            let reason = error.close_reason();
            tracing::warn!(reason, "device media socket could not join the stream");
            socket.close(reason).await;
        }
    }
    session_shutdown.cancel();
    let _ = expiration_guard.await;
    connection_guard.close().await;
}

/// Sends frames until the stream or the session ends, or a frame waits longer than
/// `send_stall` (`Some` close reason), or the client leaves (`None`).
async fn stream<T: MediaTransport>(
    socket: &mut T,
    mut subscriber: super::media::MediaSubscriber,
    devices: &DeviceManager,
    session_shutdown: &CancellationToken,
    send_stall: Duration,
) -> Option<&'static str> {
    let closed = subscriber.closed.clone();
    let end_reason = |subscriber: &super::media::MediaSubscriber| {
        if subscriber.failed() {
            "failed"
        } else if devices.enabled() {
            "stopped"
        } else {
            "disabled"
        }
    };
    loop {
        tokio::select! {
            biased;
            () = closed.cancelled() => return Some(end_reason(&subscriber)),
            () = session_shutdown.cancelled() => return Some("unauthorized"),
            inbound = socket.recv() => match inbound {
                Received::Message(message) => {
                    if matches!(serde_json::from_slice(&message), Ok(Inbound::Keyframe)) {
                        subscriber.request_keyframe();
                    }
                }
                Received::Gone => return None,
                Received::Ignored => {}
            },
            frame = subscriber.next_frame() => {
                let Some(frame) = frame else {
                    return Some(end_reason(&subscriber));
                };
                // A stalled client must not keep the socket, or its slot, past the end of
                // its stream or session. An E2EE frame's records are sent under one timeout.
                tokio::select! {
                    () = closed.cancelled() => return Some(end_reason(&subscriber)),
                    () = session_shutdown.cancelled() => return Some("unauthorized"),
                    sent = tokio::time::timeout(send_stall, socket.send(frame)) => {
                        match sent {
                            Ok(Ok(())) => {}
                            Ok(Err(())) => return None,
                            Err(_) => {
                                tracing::warn!("device media client accepted no frame in time");
                                return Some("slow");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Sends the close frame, giving up after [`CLOSE_TIMEOUT`] on a client that reads nothing.
async fn close<S>(mut socket: S, reason: &'static str)
where
    S: Sink<Message, Error = axum::Error> + Unpin,
{
    let _ = tokio::time::timeout(
        CLOSE_TIMEOUT,
        socket.send(Message::Close(Some(CloseFrame {
            code: CLOSE_CODE,
            reason: reason.into(),
        }))),
    )
    .await;
}

#[cfg(all(test, unix))]
mod tests {
    use axum::{Router, extract::State, routing::get};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message as ClientMessage,
    };

    use super::*;
    use crate::{
        ServerConfig,
        auth::{ClientMetadata, SessionTransport},
        device::{
            catalog::DeviceState,
            testing::{
                FakeScrcpySource, ManagerFixture, config_unit, delta_unit, key_unit, key_unit_of,
                kill_process, process_alive, wait_until,
            },
        },
        rpc::MAX_E2EE_CIPHERTEXT_BYTES,
    };

    type Client = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    /// A `Ready` device whose scrcpy-server is a fake source, served on the media route.
    struct Harness {
        fixture: ManagerFixture,
        source: FakeScrcpySource,
        device_id: String,
        auth: AuthService,
        session_id: String,
        url: String,
        e2ee_url: String,
    }

    impl Harness {
        async fn start() -> Self {
            Self::start_with(vec![config_unit(), key_unit(1), delta_unit(2)]).await
        }

        async fn start_with(packets: Vec<Vec<u8>>) -> Self {
            Self::start_custom(packets, E2EE_HANDSHAKE_TIMEOUT).await
        }

        /// `e2ee_preauth` bounds the E2EE handshake and ticket together.
        async fn start_custom(packets: Vec<Vec<u8>>, e2ee_preauth: Duration) -> Self {
            let source = FakeScrcpySource::spawn(packets).await;
            let fixture =
                ManagerFixture::with_sdk(|sdk| sdk.avds(&["Pixel"]).adb_forwards_to(source.port))
                    .await;
            let device_id = fixture.manager.open("Pixel").await.expect("open Pixel");
            fixture.wait_state(&device_id, DeviceState::Ready).await;
            let config = ServerConfig::new(".")
                .with_bind("127.0.0.1", 3773)
                .with_desktop("media-socket-test-bootstrap")
                .expect("auth configuration");
            let auth = AuthService::new(&config, vec![7_u8; 32]);
            let issued = auth
                .exchange_bootstrap(
                    "media-socket-test-bootstrap",
                    None,
                    ClientMetadata::default(),
                    None,
                    SessionTransport::Plain,
                )
                .await
                .expect("authorized session");
            let mut state = MediaSocketState::new(fixture.manager.clone(), auth.clone());
            state.e2ee_preauth = e2ee_preauth;
            let app = Router::new()
                .route(
                    "/api/device-media",
                    get(
                        |State(state): State<MediaSocketState>, upgrade: WebSocketUpgrade| async move {
                            upgrade_plain(state, IpAddr::from([127, 0, 0, 1]), upgrade)
                        },
                    ),
                )
                .route(
                    "/api/device-media-e2ee",
                    get(
                        |State(state): State<MediaSocketState>, upgrade: WebSocketUpgrade| async move {
                            upgrade_e2ee(state, IpAddr::from([127, 0, 0, 1]), upgrade)
                        },
                    ),
                )
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind media route");
            let address = listener.local_addr().expect("media route address");
            let url = format!("ws://{address}/api/device-media");
            let e2ee_url = format!("ws://{address}/api/device-media-e2ee");
            tokio::spawn(async move {
                axum::serve(listener, app).await.expect("serve media route");
            });
            Self {
                fixture,
                source,
                device_id,
                auth,
                session_id: issued.principal.session_id,
                url,
                e2ee_url,
            }
        }

        fn ticket(&self, channel: MediaChannel) -> String {
            self.fixture
                .manager
                .create_media_ticket(&self.device_id, &self.session_id, channel)
                .expect("ticket")
                .0
        }

        async fn connect(&self, ticket: &str) -> Client {
            let (mut client, _) = connect_async(&self.url).await.expect("media socket");
            client
                .send(ClientMessage::Text(
                    serde_json::json!({ "type": "ticket", "ticket": ticket })
                        .to_string()
                        .into(),
                ))
                .await
                .expect("send ticket");
            client
        }

        /// Runs Noise_NK pinned to `host_key` on the E2EE route; the close code on refusal.
        async fn handshake(&self, host_key: &[u8]) -> Result<E2eeClient, Option<u16>> {
            self.handshake_with(host_key, DEVICE_MEDIA_NOISE_PROLOGUE, Duration::ZERO)
                .await
        }

        /// Opens the E2EE route, waits `delay`, then runs Noise_NK under `prologue`.
        async fn handshake_with(
            &self,
            host_key: &[u8],
            prologue: &[u8],
            delay: Duration,
        ) -> Result<E2eeClient, Option<u16>> {
            let (mut socket, _) = connect_async(&self.e2ee_url)
                .await
                .expect("E2EE media socket");
            tokio::time::sleep(delay).await;
            let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
                .remote_public_key(host_key)
                .expect("host key")
                .prologue(prologue)
                .expect("prologue")
                .build_initiator()
                .expect("Noise initiator");
            let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
            let len = initiator
                .write_message(&[], &mut message_a)
                .expect("message A");
            message_a.truncate(len);
            socket
                .send(ClientMessage::Binary(message_a.into()))
                .await
                .expect("send message A");
            loop {
                let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
                    .await
                    .expect("a handshake answer within ten seconds");
                match message {
                    Some(Ok(ClientMessage::Binary(message_b))) => {
                        let mut payload = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
                        assert_eq!(
                            initiator
                                .read_message(&message_b, &mut payload)
                                .expect("message B"),
                            0
                        );
                        return Ok(E2eeClient {
                            socket,
                            transport: initiator.into_transport_mode().expect("transport"),
                        });
                    }
                    Some(Ok(ClientMessage::Close(frame))) => {
                        return Err(frame.map(|frame| u16::from(frame.code)));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => return Err(None),
                }
            }
        }

        /// Handshakes with the server's key and sends `ticket` as the first record.
        async fn connect_e2ee(&self, ticket: &str) -> E2eeClient {
            let mut client = self
                .handshake(self.auth.host_identity().public_key_bytes())
                .await
                .expect("E2EE handshake");
            client
                .send(
                    serde_json::json!({ "type": "ticket", "ticket": ticket })
                        .to_string()
                        .as_bytes(),
                )
                .await;
            client
        }
    }

    /// A Noise initiator on the E2EE media route.
    struct E2eeClient {
        socket: Client,
        transport: snow::TransportState,
    }

    impl E2eeClient {
        /// Sends `plaintext` as one final record.
        async fn send(&mut self, plaintext: &[u8]) {
            let mut record = vec![crate::rpc::E2EE_RECORD_FLAG_FINAL];
            record.extend_from_slice(plaintext);
            let mut frame = vec![0_u8; record.len() + 16];
            let len = self
                .transport
                .write_message(&record, &mut frame)
                .expect("encrypt record");
            frame.truncate(len);
            self.socket
                .send(ClientMessage::Binary(frame.into()))
                .await
                .expect("send record");
        }

        /// The next logical message and the number of records it arrived in.
        async fn recv(&mut self) -> (Vec<u8>, usize) {
            let mut message = Vec::new();
            let mut records = 0;
            loop {
                let frame = next_frame(&mut self.socket).await;
                assert!(frame.len() <= MAX_E2EE_CIPHERTEXT_BYTES);
                let mut record = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
                let len = self
                    .transport
                    .read_message(&frame, &mut record)
                    .expect("decrypt record");
                records += 1;
                message.extend_from_slice(&record[1..len]);
                match record[0] {
                    crate::rpc::E2EE_RECORD_FLAG_FINAL => return (message, records),
                    crate::rpc::E2EE_RECORD_FLAG_CONTINUATION => {}
                    flag => panic!("unknown record flag {flag}"),
                }
            }
        }
    }

    /// The next binary frame, within ten seconds.
    async fn next_frame(client: &mut Client) -> Vec<u8> {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), client.next())
                .await
                .expect("a frame within ten seconds")
                .expect("socket open")
                .expect("message");
            match message {
                ClientMessage::Binary(frame) => return frame.to_vec(),
                ClientMessage::Close(frame) => panic!("closed: {frame:?}"),
                _ => {}
            }
        }
    }

    /// Reads until the server closes; the close code and reason.
    async fn close_reason(client: &mut Client) -> (u16, String) {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), client.next())
                .await
                .expect("a close within ten seconds")
                .expect("socket open")
                .expect("message");
            if let ClientMessage::Close(frame) = message {
                let frame = frame.expect("close frame");
                return (u16::from(frame.code), frame.reason.to_string());
            }
        }
    }

    #[tokio::test]
    async fn plain_socket_requires_a_ticket_and_streams_config_first() {
        let harness = Harness::start().await;
        let ticket = harness.ticket(MediaChannel::Plain);
        let mut client = harness.connect(&ticket).await;
        let config = next_frame(&mut client).await;
        assert_eq!(config[0], 0, "config first");
        let key = next_frame(&mut client).await;
        assert_eq!(
            (key[0], u64::from_be_bytes(key[1..9].try_into().unwrap())),
            (1, 1)
        );
        assert_eq!(next_frame(&mut client).await[0], 2);
        // The new socket asked the encoder for a fresh key unit.
        assert_eq!(harness.source.control_bytes().await, [17]);
        client
            .send(ClientMessage::Text(r#"{"type":"keyframe"}"#.into()))
            .await
            .expect("send keyframe request");
        // The last socket leaving stops the session and removes its forward.
        client.close(None).await.expect("close");
        wait_until(|| harness.fixture.sdk.adb_log().contains("forward --remove")).await;
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn bad_ticket_closes_with_reason() {
        let harness = Harness::start().await;
        let ticket = harness.ticket(MediaChannel::Plain);
        let mut first = harness.connect(&ticket).await;
        assert_eq!(next_frame(&mut first).await[0], 0);
        let mut reused = harness.connect(&ticket).await;
        assert_eq!(
            close_reason(&mut reused).await,
            (CLOSE_CODE, "reused".to_owned())
        );
        let e2ee = harness.ticket(MediaChannel::E2ee);
        let mut wrong_channel = harness.connect(&e2ee).await;
        assert_eq!(
            close_reason(&mut wrong_channel).await,
            (CLOSE_CODE, "wrongChannel".to_owned())
        );
        let mut unknown = harness.connect("not-a-ticket").await;
        assert_eq!(
            close_reason(&mut unknown).await,
            (CLOSE_CODE, "expired".to_owned())
        );
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn fifth_socket_is_refused_with_limit() {
        let harness = Harness::start().await;
        let mut held = Vec::new();
        for _ in 0..crate::device::media::MAX_SOCKETS_PER_DEVICE {
            let mut client = harness.connect(&harness.ticket(MediaChannel::Plain)).await;
            assert_eq!(next_frame(&mut client).await[0], 0);
            held.push(client);
        }
        let mut fifth = harness.connect(&harness.ticket(MediaChannel::Plain)).await;
        assert_eq!(
            close_reason(&mut fifth).await,
            (CLOSE_CODE, "limit".to_owned())
        );
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn disabling_closes_media_sockets() {
        let harness = Harness::start().await;
        let mut client = harness.connect(&harness.ticket(MediaChannel::Plain)).await;
        assert_eq!(next_frame(&mut client).await[0], 0);
        let pid = harness.fixture.pid_of(&harness.device_id);
        harness
            .fixture
            .enabled
            .send(false)
            .expect("turn previews off");
        assert_eq!(
            close_reason(&mut client).await,
            (CLOSE_CODE, "disabled".to_owned())
        );
        harness
            .fixture
            .wait_state(&harness.device_id, DeviceState::Stopped)
            .await;
        assert!(!process_alive(pid), "the emulator is reaped");
        wait_until(|| harness.fixture.sdk.adb_log().contains("forward --remove")).await;
        assert!(matches!(
            harness.fixture.manager.create_media_ticket(
                &harness.device_id,
                &harness.session_id,
                MediaChannel::Plain
            ),
            Err(crate::device::DeviceOpError::Disabled)
        ));
    }

    #[tokio::test]
    async fn closing_the_device_closes_media_sockets() {
        let harness = Harness::start().await;
        let mut client = harness.connect(&harness.ticket(MediaChannel::Plain)).await;
        assert_eq!(next_frame(&mut client).await[0], 0);
        harness
            .fixture
            .manager
            .close(&harness.device_id)
            .await
            .expect("close device");
        assert_eq!(
            close_reason(&mut client).await,
            (CLOSE_CODE, "stopped".to_owned())
        );
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_failed_device_closes_media_sockets() {
        let harness = Harness::start().await;
        let mut client = harness.connect(&harness.ticket(MediaChannel::Plain)).await;
        assert_eq!(next_frame(&mut client).await[0], 0);
        kill_process(harness.fixture.pid_of(&harness.device_id));
        assert_eq!(
            close_reason(&mut client).await,
            (CLOSE_CODE, "stopped".to_owned())
        );
        harness
            .fixture
            .wait_state(&harness.device_id, DeviceState::Failed)
            .await;
        wait_until(|| harness.fixture.sdk.adb_log().contains("forward --remove")).await;
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn e2ee_socket_handshakes_and_streams_encrypted_units() {
        const KEY_DATA_BYTES: usize = 70_000;
        let harness = Harness::start_with(vec![
            config_unit(),
            key_unit_of(1, KEY_DATA_BYTES),
            delta_unit(2),
        ])
        .await;
        let ticket = harness.ticket(MediaChannel::E2ee);
        let mut client = harness.connect_e2ee(&ticket).await;
        let (config, records) = client.recv().await;
        assert_eq!((config[0], records), (0, 1), "config first, in one record");
        let (key, records) = client.recv().await;
        assert_eq!(key[0], 1);
        assert_eq!(u64::from_be_bytes(key[1..9].try_into().unwrap()), 1);
        assert_eq!(key.len(), 9 + KEY_DATA_BYTES, "the whole key unit");
        assert!(
            records >= 2,
            "a unit over {} bytes spans records",
            crate::rpc::MAX_E2EE_CHUNK_BYTES
        );
        assert_eq!(client.recv().await.0[0], 2);
        assert_eq!(harness.source.control_bytes().await, [17]);
        // Inbound messages are encrypted too: past the 1 s reset limit, a keyframe request
        // asks the encoder for another key unit.
        tokio::time::sleep(crate::device::media::RESET_MIN_INTERVAL).await;
        client.send(br#"{"type":"keyframe"}"#).await;
        harness.source.wait_control_bytes(&[17, 17]).await;
        client.socket.close(None).await.expect("close");
        wait_until(|| harness.fixture.sdk.adb_log().contains("forward --remove")).await;
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn plain_ticket_is_refused_on_e2ee_route() {
        let harness = Harness::start().await;
        let ticket = harness.ticket(MediaChannel::Plain);
        let mut client = harness.connect_e2ee(&ticket).await;
        assert_eq!(
            close_reason(&mut client.socket).await,
            (CLOSE_CODE, "wrongChannel".to_owned())
        );
        // The refusal used the ticket up.
        let mut plain = harness.connect(&ticket).await;
        assert_eq!(
            close_reason(&mut plain).await,
            (CLOSE_CODE, "reused".to_owned())
        );
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn wrong_host_key_fails_handshake() {
        let harness = Harness::start().await;
        let ticket = harness.ticket(MediaChannel::E2ee);
        let other = crate::auth::HostIdentity::generate_ephemeral();
        assert_eq!(
            harness
                .handshake(other.public_key_bytes())
                .await
                .err()
                .expect("the handshake fails"),
            Some(crate::rpc::E2EE_HOST_IDENTITY_CLOSE_CODE)
        );
        // No ticket was consumed: it still opens the stream over the right key.
        let mut client = harness.connect_e2ee(&ticket).await;
        assert_eq!(client.recv().await.0[0], 0);
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn e2ee_handshake_needs_the_media_prologue() {
        let harness = Harness::start().await;
        assert_eq!(
            harness
                .handshake_with(
                    harness.auth.host_identity().public_key_bytes(),
                    b"",
                    Duration::ZERO
                )
                .await
                .err()
                .expect("the handshake fails"),
            Some(crate::rpc::E2EE_HOST_IDENTITY_CLOSE_CODE)
        );
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn e2ee_handshake_and_ticket_share_one_deadline() {
        let preauth = Duration::from_millis(1500);
        let harness = Harness::start_custom(vec![config_unit(), key_unit(1)], preauth).await;
        let started = std::time::Instant::now();
        // A late but successful handshake leaves only the rest of the deadline for the
        // ticket, not a fresh ticket wait.
        let mut client = harness
            .handshake_with(
                harness.auth.host_identity().public_key_bytes(),
                DEVICE_MEDIA_NOISE_PROLOGUE,
                Duration::from_millis(1200),
            )
            .await
            .expect("E2EE handshake");
        assert_eq!(
            close_reason(&mut client.socket).await,
            (CLOSE_CODE, "expired".to_owned())
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= preauth && elapsed < preauth + Duration::from_secs(1),
            "{elapsed:?}"
        );
        harness.fixture.manager.shutdown().await;
    }

    /// A client that never accepts a frame and never sends anything.
    struct StalledSocket;

    impl Sink<Message> for StalledSocket {
        type Error = axum::Error;

        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn start_send(self: std::pin::Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Pending
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl Stream for StalledSocket {
        type Item = Result<Message, axum::Error>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test]
    async fn a_stalled_socket_closes_as_slow_and_frees_its_slot() {
        let harness = Harness::start().await;
        let subscriber = harness
            .fixture
            .manager
            .attach_media(&harness.device_id)
            .expect("attach");
        let started = std::time::Instant::now();
        let reason = tokio::time::timeout(
            Duration::from_secs(10),
            stream(
                &mut PlainTransport(StalledSocket),
                subscriber,
                &harness.fixture.manager,
                &CancellationToken::new(),
                Duration::from_millis(200),
            ),
        )
        .await
        .expect("the stalled socket ends");
        assert_eq!(reason, Some("slow"));
        assert!(started.elapsed() >= Duration::from_millis(200));
        // Its slot is free again: the hub stopped with its only socket.
        wait_until(|| harness.fixture.sdk.adb_log().contains("forward --remove")).await;
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn revoking_the_session_closes_a_stalled_socket() {
        let harness = Harness::start().await;
        let session_shutdown = CancellationToken::new();
        let _connection = harness
            .auth
            .mark_connected_guard(&harness.session_id, session_shutdown.clone())
            .await
            .expect("live session");
        let subscriber = harness
            .fixture
            .manager
            .attach_media(&harness.device_id)
            .expect("attach");
        let devices = harness.fixture.manager.clone();
        let streaming = tokio::spawn(async move {
            stream(
                &mut PlainTransport(StalledSocket),
                subscriber,
                &devices,
                &session_shutdown,
                SEND_STALL_TIMEOUT,
            )
            .await
        });
        // Let the first frame reach the stalled send.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            harness
                .auth
                .revoke_client("administrator", &harness.session_id)
                .await
                .expect("revoke")
        );
        let reason = tokio::time::timeout(Duration::from_secs(5), streaming)
            .await
            .expect("revocation ends the stalled socket")
            .expect("stream task");
        assert_eq!(reason, Some("unauthorized"));
        harness.fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn revoking_the_session_closes_media_sockets() {
        let harness = Harness::start().await;
        let ticket = harness.ticket(MediaChannel::Plain);
        let mut client = harness.connect(&ticket).await;
        assert_eq!(next_frame(&mut client).await[0], 0);
        assert!(
            harness
                .auth
                .revoke_client("administrator", &harness.session_id)
                .await
                .expect("revoke")
        );
        assert_eq!(
            close_reason(&mut client).await,
            (CLOSE_CODE, "unauthorized".to_owned())
        );
        // A ticket of a revoked session admits nothing.
        let ticket = harness.ticket(MediaChannel::Plain);
        let mut late = harness.connect(&ticket).await;
        assert_eq!(
            close_reason(&mut late).await,
            (CLOSE_CODE, "unauthorized".to_owned())
        );
        harness.fixture.manager.shutdown().await;
    }
}
