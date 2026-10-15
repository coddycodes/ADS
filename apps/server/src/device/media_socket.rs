//! `GET /api/device-media`: a plain WebSocket that carries one device's H.264 frames (spec
//! §4.2–4.3). It takes no cookie or bearer credential: the first message is a single-use
//! ticket from `device.createMediaTicket`, never the URL. Rejections close with code 4400
//! and a reason the client acts on:
//!
//! | Reason | Cause |
//! | --- | --- |
//! | `expired` | the ticket expired, is unknown or malformed, or did not arrive within 5 s |
//! | `reused` | the ticket was already used |
//! | `wrongChannel` | the ticket was minted over E2EE |
//! | `wrongDevice` | the ticket's device is no longer known |
//! | `limit` | the device already has four media sockets |
//! | `disabled` | device previews were turned off |
//! | `stopped` | the device stopped, failed, or is not ready |
//! | `failed` | the device's video stream failed |
//! | `unauthorized` | the ticket's session was revoked or expired |
//! | `slow` | the client accepted no frame for 10 s |
//!
//! Before the upgrade, connections are admitted against the same global, per-peer, and
//! per-network pre-auth budgets as the E2EE route, from a separate pool so media sockets
//! cannot starve E2EE handshakes; a refused one gets `503` and is never upgraded.

use std::{net::IpAddr, time::Duration};

use axum::{
    extract::ws::{CloseFrame, Message, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{DeviceManager, media::MediaChannel};
use crate::{auth::AuthService, rpc::E2eePreauthAdmission};

/// How long a socket may take to send its ticket.
pub(crate) const TICKET_WAIT: Duration = Duration::from_secs(5);
/// How long one frame may wait for the client before the socket closes as `slow`.
pub(crate) const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a close frame may take; the socket is dropped after it either way.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Inbound messages are only the ticket and `{"type":"keyframe"}`.
const MAX_INBOUND_BYTES: usize = 1024;
const READ_BUFFER_BYTES: usize = 4 * 1024;
const WRITE_BUFFER_BYTES: usize = 16 * 1024;
const CLOSE_CODE: u16 = 4400;

/// What the media route needs: the devices, the sessions tickets are bound to, and its own
/// pre-auth admission pool.
#[derive(Clone)]
pub(crate) struct MediaSocketState {
    pub devices: DeviceManager,
    pub auth: AuthService,
    pub admission: E2eePreauthAdmission,
    /// [`SEND_STALL_TIMEOUT`] outside tests.
    pub send_stall: Duration,
}

impl MediaSocketState {
    pub(crate) fn new(devices: DeviceManager, auth: AuthService) -> Self {
        Self {
            devices,
            auth,
            admission: E2eePreauthAdmission::new(),
            send_stall: SEND_STALL_TIMEOUT,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Inbound {
    Ticket { ticket: String },
    Keyframe,
}

/// Admits and upgrades a plain media socket from `peer_ip` (see
/// `crate::rpc::effective_preauth_peer`); the ticket is checked once it is open.
pub(crate) fn upgrade_plain(
    state: MediaSocketState,
    peer_ip: IpAddr,
    upgrade: WebSocketUpgrade,
) -> Response {
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
        .max_message_size(MAX_INBOUND_BYTES)
        .max_frame_size(MAX_INBOUND_BYTES)
        .read_buffer_size(READ_BUFFER_BYTES)
        .write_buffer_size(WRITE_BUFFER_BYTES)
        .on_upgrade(move |socket| serve(socket, state, MediaChannel::Plain, lease))
}

async fn serve<S>(
    mut socket: S,
    state: MediaSocketState,
    channel: MediaChannel,
    lease: crate::rpc::E2eePreauthLease,
) where
    S: Sink<Message, Error = axum::Error> + Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let ticket = match tokio::time::timeout(TICKET_WAIT, socket.next()).await {
        Err(_) => {
            tracing::warn!("device media socket sent no ticket in time");
            return close(socket, "expired").await;
        }
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str(&text) {
            Ok(Inbound::Ticket { ticket }) => ticket,
            // The parse error could quote the ticket, so it is not logged.
            _ => {
                tracing::warn!("device media socket's first message was not a ticket");
                return close(socket, "expired").await;
            }
        },
        Ok(Some(Ok(_))) => {
            tracing::warn!("device media socket's first message was not a ticket");
            return close(socket, "expired").await;
        }
        // The client left before it sent anything.
        Ok(None | Some(Err(_))) => return,
    };
    let (device_id, principal) = match state.devices.redeem_media_ticket(&ticket, channel) {
        Ok(redeemed) => redeemed,
        Err(error) => {
            let reason = error.close_reason();
            tracing::warn!(reason, "device media ticket refused");
            return close(socket, reason).await;
        }
    };
    drop(ticket);
    // A redeemed ticket authenticates the socket; it leaves the pre-auth pool.
    drop(lease);
    // The session the ticket was minted for must still be live, and the socket ends with
    // it, as `/ws` does.
    let Some(expires_at_ms) = state.auth.active_session_expiry(&principal).await else {
        tracing::warn!("device media ticket's session is no longer active");
        return close(socket, "unauthorized").await;
    };
    let session_shutdown = CancellationToken::new();
    let Ok(connection_guard) = state
        .auth
        .mark_connected_guard(&principal, session_shutdown.clone())
        .await
    else {
        tracing::warn!("device media ticket's session is no longer active");
        return close(socket, "unauthorized").await;
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
                close(socket, reason).await;
            }
        }
        Err(error) => {
            let reason = error.close_reason();
            tracing::warn!(reason, "device media socket could not join the stream");
            close(socket, reason).await;
        }
    }
    session_shutdown.cancel();
    let _ = expiration_guard.await;
    connection_guard.close().await;
}

/// Sends frames until the stream or the session ends, or a frame waits longer than
/// `send_stall` (`Some` close reason), or the client leaves (`None`).
async fn stream<S>(
    socket: &mut S,
    mut subscriber: super::media::MediaSubscriber,
    devices: &DeviceManager,
    session_shutdown: &CancellationToken,
    send_stall: Duration,
) -> Option<&'static str>
where
    S: Sink<Message, Error = axum::Error> + Stream<Item = Result<Message, axum::Error>> + Unpin,
{
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
            inbound = socket.next() => match inbound {
                Some(Ok(Message::Text(text))) => {
                    if matches!(serde_json::from_str(&text), Ok(Inbound::Keyframe)) {
                        subscriber.request_keyframe();
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
                Some(Ok(_)) => {}
            },
            frame = subscriber.next_frame() => {
                let Some(frame) = frame else {
                    return Some(end_reason(&subscriber));
                };
                // A stalled client must not keep the socket, or its slot, past the end of
                // its stream or session.
                tokio::select! {
                    () = closed.cancelled() => return Some(end_reason(&subscriber)),
                    () = session_shutdown.cancelled() => return Some("unauthorized"),
                    sent = tokio::time::timeout(send_stall, socket.send(Message::Binary(frame))) => {
                        match sent {
                            Ok(Ok(())) => {}
                            Ok(Err(_)) => return None,
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
                FakeScrcpySource, ManagerFixture, config_unit, delta_unit, key_unit, kill_process,
                process_alive, wait_until,
            },
        },
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
    }

    impl Harness {
        async fn start() -> Self {
            let source =
                FakeScrcpySource::spawn(vec![config_unit(), key_unit(1), delta_unit(2)]).await;
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
            let state = MediaSocketState::new(fixture.manager.clone(), auth.clone());
            let app = Router::new()
                .route(
                    "/api/device-media",
                    get(
                        |State(state): State<MediaSocketState>, upgrade: WebSocketUpgrade| async move {
                            upgrade_plain(state, IpAddr::from([127, 0, 0, 1]), upgrade)
                        },
                    ),
                )
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind media route");
            let url = format!(
                "ws://{}/api/device-media",
                listener.local_addr().expect("media route address")
            );
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
                &mut StalledSocket,
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
                &mut StalledSocket,
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
