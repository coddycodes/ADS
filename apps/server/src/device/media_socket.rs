//! `GET /api/device-media`: a plain WebSocket that carries one device's H.264 frames (spec
//! §4.2–4.3). It takes no cookie or bearer credential: the first message is a single-use
//! ticket from `device.createMediaTicket`, never the URL. Rejections close with code 4400
//! and a reason the client acts on.

use std::time::Duration;

use axum::{
    extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    response::Response,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{DeviceManager, media::MediaChannel};
use crate::auth::AuthService;

/// How long a socket may take to send its ticket.
pub(crate) const TICKET_WAIT: Duration = Duration::from_secs(5);
/// Inbound messages are only the ticket and `{"type":"keyframe"}`.
const MAX_INBOUND_BYTES: usize = 1024;
const CLOSE_CODE: u16 = 4400;

/// What the media route needs: the devices, and the sessions tickets are bound to.
#[derive(Clone)]
pub(crate) struct MediaSocketState {
    pub devices: DeviceManager,
    pub auth: AuthService,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Inbound {
    Ticket { ticket: String },
    Keyframe,
}

/// Upgrades a plain media socket; the ticket is checked once it is open.
pub(crate) fn upgrade_plain(state: MediaSocketState, upgrade: WebSocketUpgrade) -> Response {
    upgrade
        .max_message_size(MAX_INBOUND_BYTES)
        .max_frame_size(MAX_INBOUND_BYTES)
        .on_upgrade(move |socket| serve(socket, state, MediaChannel::Plain))
}

async fn serve(mut socket: WebSocket, state: MediaSocketState, channel: MediaChannel) {
    let ticket = match tokio::time::timeout(TICKET_WAIT, socket.recv()).await {
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
            let reason = stream(&mut socket, subscriber, &state.devices, &session_shutdown).await;
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

/// Sends frames until the stream or the session ends (`Some` close reason) or the client
/// leaves (`None`).
async fn stream(
    socket: &mut WebSocket,
    mut subscriber: super::media::MediaSubscriber,
    devices: &DeviceManager,
    session_shutdown: &CancellationToken,
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
                // A stalled client must not keep the socket open after its stream ended.
                tokio::select! {
                    () = closed.cancelled() => return Some(end_reason(&subscriber)),
                    sent = socket.send(Message::Binary(frame)) => if sent.is_err() {
                        return None;
                    },
                }
            }
        }
    }
}

async fn close(mut socket: WebSocket, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: CLOSE_CODE,
            reason: reason.into(),
        })))
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
            let state = MediaSocketState {
                devices: fixture.manager.clone(),
                auth: auth.clone(),
            };
            let app = Router::new()
                .route(
                    "/api/device-media",
                    get(
                        |State(state): State<MediaSocketState>, upgrade: WebSocketUpgrade| async move {
                            upgrade_plain(state, upgrade)
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
