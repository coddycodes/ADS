//! Typed `device.*` RPC over the [`DeviceManager`]. While `enableDevicePreviews` is off, every
//! method fails with `DevicePreviewsDisabledError` before it touches the manager.

use std::future::Future;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::{
    RpcRegistry, RpcResult, RpcStreamChunk,
    device::{
        DeviceManager, DeviceOpError, catalog::Orientation, frames::FrameSubscription,
        input::DeviceInputEvent,
    },
    rpc::RpcSessionContext,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceOpenInput {
    avd_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdInput {
    device_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceInputInput {
    device_id: String,
    events: Vec<DeviceInputEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceRotateInput {
    device_id: String,
    orientation: Orientation,
}

pub fn register_device_rpc(registry: &mut RpcRegistry, devices: DeviceManager) {
    let catalog = devices.clone();
    registry.register_latest_stream("device.subscribeCatalog", move |_request, cancellation| {
        let (sender, receiver) = watch::channel(None);
        if !catalog.enabled() {
            sender.send_replace(Some(Err(DeviceOpError::Disabled.to_rpc_error())));
            return receiver;
        }
        let devices = catalog.clone();
        tokio::spawn(async move {
            let mut source = devices.catalog();
            // The current catalog first, so the client sees discovery in progress.
            publish(&sender, serde_json::to_value(&*source.borrow_and_update()));
            let refresh = devices.refresh();
            tokio::pin!(refresh);
            let mut refreshing = true;
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => break,
                    () = sender.closed() => break,
                    () = &mut refresh, if refreshing => refreshing = false,
                    changed = source.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        publish(&sender, serde_json::to_value(&*source.borrow_and_update()));
                    }
                }
            }
        });
        receiver
    });

    let frames = devices.clone();
    registry.register_latest_stream("device.subscribeFrames", move |request, cancellation| {
        let (sender, receiver) = watch::channel(None);
        let subscription = if frames.enabled() {
            decode::<DeviceIdInput>(request.payload).and_then(|input| {
                frames
                    .subscribe_frames(&input.device_id)
                    .map_err(|error| error.to_rpc_error())
            })
        } else {
            Err(DeviceOpError::Disabled.to_rpc_error())
        };
        let subscription: FrameSubscription = match subscription {
            Ok(subscription) => subscription,
            Err(error) => {
                sender.send_replace(Some(Err(error)));
                return receiver;
            }
        };
        tokio::spawn(async move {
            // Moves the whole subscription, whose guard keeps the producer running; the
            // closure alone would capture only `frames`.
            let mut subscription = subscription;
            // A frame the device already produced is sent at once.
            subscription.frames.mark_changed();
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => break,
                    () = sender.closed() => break,
                    // Errors once the device stops, which ends the stream.
                    changed = subscription.frames.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        // Cloned out so the producer's next send never waits on encoding.
                        let frame = subscription.frames.borrow_and_update().clone();
                        if let Some(frame) = frame {
                            publish(&sender, serde_json::to_value(frame));
                        }
                    }
                }
            }
        });
        receiver
    });

    let tickets = devices.clone();
    registry.register_unary_with_context(
        "device.createMediaTicket",
        move |request, context, _cancellation| {
            let result = create_media_ticket(&tickets, request.payload, &context);
            async move { result }
        },
    );

    register_op(
        registry,
        "device.open",
        &devices,
        |devices, payload| async move {
            let input: DeviceOpenInput = decode(payload)?;
            let device_id = devices.open(&input.avd_name).await.map_err(rpc_error)?;
            Ok(json!({ "deviceId": device_id }))
        },
    );
    register_op(
        registry,
        "device.close",
        &devices,
        |devices, payload| async move {
            let input: DeviceIdInput = decode(payload)?;
            devices.close(&input.device_id).await.map_err(rpc_error)?;
            Ok(Value::Null)
        },
    );
    register_op(
        registry,
        "device.input",
        &devices,
        |devices, payload| async move {
            // A serde message can quote the payload, which may hold typed text.
            let input: DeviceInputInput =
                serde_json::from_value(payload).map_err(|_| redacted_defect())?;
            devices
                .input(&input.device_id, input.events)
                .await
                .map_err(rpc_error)?;
            Ok(Value::Null)
        },
    );
    register_op(
        registry,
        "device.rotate",
        &devices,
        |devices, payload| async move {
            let input: DeviceRotateInput = decode(payload)?;
            devices
                .rotate(&input.device_id, input.orientation)
                .await
                .map_err(rpc_error)?;
            Ok(Value::Null)
        },
    );
}

/// A media ticket bound to the caller's session and to the transport this request came on,
/// so a plain socket cannot redeem a ticket minted over E2EE.
fn create_media_ticket(
    devices: &DeviceManager,
    payload: Value,
    context: &RpcSessionContext,
) -> RpcResult {
    if !devices.enabled() {
        return Err(DeviceOpError::Disabled.to_rpc_error());
    }
    let input: DeviceIdInput = decode(payload)?;
    // Tickets are bound to a session; a server without auth has none, so the client shows
    // still images at once.
    let principal = context
        .principal()
        .ok_or_else(|| DeviceOpError::MediaUnavailable.to_rpc_error())?;
    let (ticket, expires_at_ms) = devices
        .create_media_ticket(&input.device_id, &principal.session_id, context.channel())
        .map_err(rpc_error)?;
    Ok(json!({ "ticket": ticket, "expiresAtMs": expires_at_ms }))
}

/// Registers a unary method that is refused while device previews are off.
fn register_op<F, Fut>(
    registry: &mut RpcRegistry,
    name: &'static str,
    devices: &DeviceManager,
    op: F,
) where
    F: Fn(DeviceManager, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = RpcResult> + Send + 'static,
{
    let devices = devices.clone();
    registry.register_unary(name, move |request, _cancellation| {
        let enabled = devices.enabled();
        let operation = op(devices.clone(), request.payload);
        async move {
            if !enabled {
                return Err(DeviceOpError::Disabled.to_rpc_error());
            }
            operation.await
        }
    });
}

fn rpc_error(error: DeviceOpError) -> Value {
    error.to_rpc_error()
}

fn publish(sender: &watch::Sender<Option<RpcStreamChunk>>, value: serde_json::Result<Value>) {
    let chunk = value
        .map(|value| vec![value])
        .map_err(|error| defect(&error));
    sender.send_replace(Some(chunk));
}

/// Payloads are validated by the client's schema, so one that does not decode is a defect.
fn decode<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T, Value> {
    serde_json::from_value(payload).map_err(|error| defect(&error))
}

/// A decode failure for a payload whose values must not be quoted.
fn redacted_defect() -> Value {
    json!({ "_tag": "Defect", "message": "device.input payload does not match the contract" })
}

fn defect(error: &serde_json::Error) -> Value {
    json!({ "_tag": "Defect", "message": error.to_string() })
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

    use super::*;
    use crate::{
        ServerConfig, ServerHandle, ServerRuntime,
        device::{catalog::DeviceState, testing::ManagerFixture},
    };

    type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    struct Harness {
        handle: ServerHandle,
        socket: Socket,
        next_id: u64,
        pending: Vec<Value>,
        _directory: tempfile::TempDir,
    }

    impl Harness {
        async fn start(devices: DeviceManager) -> Self {
            let mut registry = RpcRegistry::empty();
            register_device_rpc(&mut registry, devices);
            let directory = tempfile::tempdir().expect("server directory");
            let handle = ServerRuntime::start_with_registry(
                ServerConfig::new(directory.path())
                    .with_bind("127.0.0.1", 0)
                    .with_unsafe_no_auth(),
                registry,
            )
            .await
            .expect("server");
            let socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
                .await
                .expect("WebSocket")
                .0;
            Self {
                handle,
                socket,
                next_id: 1,
                pending: Vec::new(),
                _directory: directory,
            }
        }

        async fn request(&mut self, tag: &str, payload: Value) -> String {
            let id = self.next_id.to_string();
            self.next_id += 1;
            self.send(json!({
                "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [],
            }))
            .await;
            id
        }

        async fn send(&mut self, message: Value) {
            self.socket
                .send(Message::Text(message.to_string().into()))
                .await
                .expect("send");
        }

        /// The next message for request `id`, within 20 s. Every chunk is acknowledged on
        /// arrival, and messages for other requests wait in `pending`.
        async fn next(&mut self, id: &str) -> Value {
            if let Some(index) = self
                .pending
                .iter()
                .position(|message| message["requestId"] == id)
            {
                return self.pending.remove(index);
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            loop {
                let frame = tokio::time::timeout_at(deadline, self.socket.next())
                    .await
                    .unwrap_or_else(|_| panic!("no response for request {id} within 20 s"))
                    .expect("socket open")
                    .expect("frame");
                let Message::Text(text) = frame else {
                    continue;
                };
                let message: Value = serde_json::from_str(&text).expect("response JSON");
                if message["_tag"] == "Chunk" {
                    let request_id = message["requestId"].clone();
                    self.send(json!({ "_tag": "Ack", "requestId": request_id }))
                        .await;
                }
                if message["requestId"] == id {
                    return message;
                }
                self.pending.push(message);
            }
        }

        async fn call(&mut self, tag: &str, payload: Value) -> Result<Value, Value> {
            let id = self.request(tag, payload).await;
            exit(self.next(&id).await)
        }

        /// The first streamed value of `id` that satisfies `accept`.
        async fn chunk_until(&mut self, id: &str, accept: impl Fn(&Value) -> bool) -> Value {
            loop {
                let message = self.next(id).await;
                assert_eq!(message["_tag"], "Chunk", "{message}");
                let value = message["values"][0].clone();
                if accept(&value) {
                    return value;
                }
            }
        }

        async fn stop(mut self) {
            self.socket.close(None).await.expect("close socket");
            self.handle.shutdown();
            self.handle.join().await.expect("server joins");
        }
    }

    fn exit(message: Value) -> Result<Value, Value> {
        assert_eq!(message["_tag"], "Exit", "{message}");
        match message["exit"]["_tag"].as_str() {
            Some("Success") => Ok(message["exit"]["value"].clone()),
            Some("Failure") => Err(message["exit"]["cause"][0]["error"].clone()),
            other => panic!("unexpected exit: {other:?}"),
        }
    }

    #[tokio::test]
    async fn catalog_streams_discovery_and_device_states() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        let mut harness = Harness::start(fixture.manager.clone()).await;
        let catalog = harness.request("device.subscribeCatalog", json!({})).await;
        let ready = harness
            .chunk_until(&catalog, |value| value["availability"]["kind"] == "ready")
            .await;
        assert_eq!(ready["avds"][0]["name"], "Pixel");
        let opened = harness
            .call("device.open", json!({ "avdName": "Pixel" }))
            .await
            .expect("device opens");
        let id = opened["deviceId"].as_str().expect("device id").to_owned();
        let device = harness
            .chunk_until(&catalog, |value| value["devices"][0]["state"] == "ready")
            .await;
        assert_eq!(device["devices"][0]["deviceId"], id.as_str());
        assert_eq!(device["devices"][0]["orientation"], "portrait");
        assert_eq!(
            harness
                .call("device.open", json!({ "avdName": "Pixel" }))
                .await
                .unwrap_err()["_tag"],
            "DeviceLimitError"
        );
        fixture.wait_state(&id, DeviceState::Ready).await;
        harness
            .call("device.close", json!({ "deviceId": id }))
            .await
            .expect("device closes");
        harness
            .chunk_until(&catalog, |value| value["devices"][0]["state"] == "stopped")
            .await;
        harness.stop().await;
        fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn frames_input_and_rotation_reach_the_device() {
        let fixture = ManagerFixture::ready_device().await;
        let mut harness = Harness::start(fixture.manager.clone()).await;
        let frames = harness
            .request("device.subscribeFrames", json!({ "deviceId": fixture.id }))
            .await;
        let frame = harness
            .chunk_until(&frames, |value| value["jpegBase64"].is_string())
            .await;
        assert_eq!(
            (frame["width"].as_u64(), frame["height"].as_u64()),
            (Some(720), Some(1600))
        );
        assert_eq!(frame["orientation"], "portrait");

        harness
            .call(
                "device.input",
                json!({
                    "deviceId": fixture.id,
                    "events": [
                        { "type": "touch", "action": "down", "pointerId": 0, "x": 0.5, "y": 0.25 },
                        { "type": "key", "key": "back", "action": "down" },
                        { "type": "text", "text": "hello" },
                    ],
                }),
            )
            .await
            .expect("input is forwarded");
        assert_eq!(fixture.controller.wait_inputs(3).await.len(), 3);

        let invalid = harness
            .call(
                "device.input",
                json!({ "deviceId": fixture.id, "events": [] }),
            )
            .await
            .unwrap_err();
        assert_eq!(invalid["_tag"], "DeviceInputError");
        // A malformed payload is a defect whose message never quotes the payload.
        let malformed = harness
            .call(
                "device.input",
                json!({
                    "deviceId": fixture.id,
                    "events": [{ "type": "touch", "action": "down", "pointerId": "secret", "x": 0, "y": 0 }],
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(malformed["_tag"], "Defect");
        assert!(!malformed.to_string().contains("secret"), "{malformed}");

        harness
            .call(
                "device.rotate",
                json!({ "deviceId": fixture.id, "orientation": "landscape" }),
            )
            .await
            .expect("rotation is requested");
        assert_eq!(
            fixture.controller.rotations.lock().unwrap().last(),
            Some(&[0.0, 0.0, -90.0])
        );
        fixture.controller.fail_rotation();
        let failed = harness
            .call(
                "device.rotate",
                json!({ "deviceId": fixture.id, "orientation": "portrait" }),
            )
            .await
            .unwrap_err();
        assert_eq!(failed["_tag"], "DeviceEmulatorError");
        assert!(
            failed["message"]
                .as_str()
                .unwrap()
                .contains("did not rotate")
        );

        // Closing the device ends the frame stream.
        harness
            .call("device.close", json!({ "deviceId": fixture.id }))
            .await
            .expect("device closes");
        loop {
            let message = harness.next(&frames).await;
            if message["_tag"] == "Exit" {
                assert!(exit(message).is_ok());
                break;
            }
        }
        let missing = harness
            .request("device.subscribeFrames", json!({ "deviceId": "missing" }))
            .await;
        let error = exit(harness.next(&missing).await).unwrap_err();
        assert_eq!(error["_tag"], "DeviceNotFoundError");
        harness.stop().await;
        fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn media_tickets_are_bound_to_the_session_and_channel() {
        use crate::{
            auth::{AuthService, ClientMetadata, SessionTransport},
            device::media::{MediaChannel, TicketError},
        };
        let fixture = ManagerFixture::ready_device().await;
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("device-rpc-test-bootstrap")
            .expect("auth configuration");
        let auth = AuthService::new(&config, vec![7_u8; 32]);
        let issued = auth
            .exchange_bootstrap(
                "device-rpc-test-bootstrap",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("authorized session");
        let session = issued.principal.session_id.clone();
        let context = RpcSessionContext::authenticated(issued.principal, auth);
        let payload = json!({ "deviceId": fixture.id });

        let plain = create_media_ticket(&fixture.manager, payload.clone(), &context)
            .expect("ticket over a plain connection");
        let ticket = plain["ticket"].as_str().expect("ticket");
        assert!(plain["expiresAtMs"].as_i64().expect("expiry") > 0);
        assert_eq!(
            fixture
                .manager
                .redeem_media_ticket(ticket, MediaChannel::Plain),
            Ok((fixture.id.clone(), session))
        );

        let e2ee = create_media_ticket(
            &fixture.manager,
            payload.clone(),
            &context.clone().with_channel(MediaChannel::E2ee),
        )
        .expect("ticket over E2EE");
        assert_eq!(
            fixture
                .manager
                .redeem_media_ticket(e2ee["ticket"].as_str().unwrap(), MediaChannel::Plain),
            Err(TicketError::WrongChannel)
        );

        let anonymous = create_media_ticket(
            &fixture.manager,
            payload,
            &RpcSessionContext::unauthenticated(),
        )
        .unwrap_err();
        assert_eq!(anonymous["_tag"], "DeviceMediaUnavailableError");
        let missing =
            create_media_ticket(&fixture.manager, json!({ "deviceId": "missing" }), &context)
                .unwrap_err();
        assert_eq!(missing["_tag"], "DeviceNotFoundError");
        fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn every_method_is_refused_while_disabled() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        fixture.enabled.send(false).unwrap();
        let mut harness = Harness::start(fixture.manager.clone()).await;
        for (method, payload) in [
            ("device.subscribeCatalog", json!({})),
            ("device.subscribeFrames", json!({ "deviceId": "d" })),
            ("device.open", json!({ "avdName": "Pixel" })),
            ("device.close", json!({ "deviceId": "d" })),
            (
                "device.input",
                json!({ "deviceId": "d", "events": [{ "type": "text", "text": "x" }] }),
            ),
            (
                "device.rotate",
                json!({ "deviceId": "d", "orientation": "landscape" }),
            ),
            ("device.createMediaTicket", json!({ "deviceId": "d" })),
        ] {
            let error = harness.call(method, payload).await.unwrap_err();
            assert_eq!(error["_tag"], "DevicePreviewsDisabledError", "{method}");
            assert!(
                error["message"].as_str().unwrap().contains("Settings"),
                "{method}"
            );
        }
        // The setting is checked before the payload is decoded.
        let error = harness
            .call("device.subscribeFrames", json!({ "deviceId": 7 }))
            .await
            .unwrap_err();
        assert_eq!(error["_tag"], "DevicePreviewsDisabledError");
        harness.stop().await;
    }
}
