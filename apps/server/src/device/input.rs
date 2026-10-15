//! Touch, key, and text input and rotation for a running device.

use std::{collections::BTreeMap, time::Duration};

use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_util::task::TaskTracker;

use super::{
    DeviceError, DeviceManager, DeviceOpError,
    android::grpc::{
        EmulatorClient,
        proto::{self, image_format::ImgFormat, rotation::SkinRotation},
    },
    catalog::Orientation,
};

/// The most events one `device.input` batch may carry.
pub(crate) const MAX_INPUT_EVENTS: usize = 64;
/// The most characters one text event may carry.
pub(crate) const MAX_TEXT_CHARS: usize = 1_000;
/// How long one event may wait for room in the emulator's input stream.
const INPUT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// How often `rotate` reads the orientation the emulator reports, and how far apart.
const ROTATION_POLLS: usize = 5;
const ROTATION_POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DeviceInputEvent {
    /// `x` and `y` are normalized to `0..1` in the device's current orientation.
    Touch {
        action: TouchAction,
        pointer_id: u32,
        x: f64,
        y: f64,
    },
    Key {
        key: DeviceKey,
        action: KeyAction,
    },
    Text {
        text: String,
    },
}

/// Never prints text content, only its length.
impl std::fmt::Debug for DeviceInputEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Touch {
                action,
                pointer_id,
                x,
                y,
            } => formatter
                .debug_struct("Touch")
                .field("action", action)
                .field("pointer_id", pointer_id)
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::Key { key, action } => formatter
                .debug_struct("Key")
                .field("key", key)
                .field("action", action)
                .finish(),
            Self::Text { text } => formatter
                .debug_struct("Text")
                .field("len", &text.chars().count())
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TouchAction {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyAction {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceKey {
    Back,
    Home,
    AppSwitch,
    Power,
    VolumeUp,
    VolumeDown,
    Enter,
    Backspace,
    Delete,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

/// The W3C `KeyboardEvent.key` value the emulator maps to `key`.
pub(crate) fn w3c_key(key: DeviceKey) -> &'static str {
    match key {
        DeviceKey::Back => "GoBack",
        DeviceKey::Home => "GoHome",
        DeviceKey::AppSwitch => "AppSwitch",
        DeviceKey::Power => "Power",
        DeviceKey::VolumeUp => "AudioVolumeUp",
        DeviceKey::VolumeDown => "AudioVolumeDown",
        DeviceKey::Enter => "Enter",
        DeviceKey::Backspace => "Backspace",
        DeviceKey::Delete => "Delete",
        DeviceKey::Tab => "Tab",
        DeviceKey::ArrowUp => "ArrowUp",
        DeviceKey::ArrowDown => "ArrowDown",
        DeviceKey::ArrowLeft => "ArrowLeft",
        DeviceKey::ArrowRight => "ArrowRight",
    }
}

/// The display size in `orientation` of a device whose native (portrait) display is
/// `width` x `height`. Assumes the emulator swaps the display's axes when it rotates
/// (native verification item 3).
pub(crate) fn oriented_size(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Portrait | Orientation::ReversePortrait => (width, height),
        Orientation::Landscape | Orientation::ReverseLandscape => (height, width),
    }
}

/// Display pixels for a point normalized to `0..1` in the current orientation, clamped to the
/// display. Assumes touches use the current orientation's display pixels (native
/// verification item 3).
pub(crate) fn to_device_pixels(
    x: f64,
    y: f64,
    width: u32,
    height: u32,
    orientation: Orientation,
) -> (i32, i32) {
    let (width, height) = oriented_size(width, height, orientation);
    let axis = |value: f64, size: u32| {
        // Saturating float-to-int casts; the clamp keeps the value inside the display.
        let pixel = (value.clamp(0.0, 1.0) * f64::from(size)) as u32;
        i32::try_from(pixel.min(size.saturating_sub(1))).unwrap_or(i32::MAX)
    };
    (axis(x, width), axis(y, height))
}

/// The `setPhysicalModel(ROTATION)` vector that tilts the virtual device toward
/// `orientation` (native verification item 2).
pub(crate) fn rotation_vector(orientation: Orientation) -> [f32; 3] {
    match orientation {
        Orientation::Portrait => [0.0, 0.0, 0.0],
        Orientation::Landscape => [0.0, 0.0, -90.0],
        Orientation::ReversePortrait => [0.0, 0.0, 180.0],
        Orientation::ReverseLandscape => [0.0, 0.0, 90.0],
    }
}

/// The orientation an image's `format.rotation` reports; `None` for an unknown value.
pub(crate) fn reported_orientation(rotation: i32) -> Option<Orientation> {
    Some(match SkinRotation::try_from(rotation).ok()? {
        SkinRotation::Portrait => Orientation::Portrait,
        SkinRotation::Landscape => Orientation::Landscape,
        SkinRotation::ReversePortrait => Orientation::ReversePortrait,
        SkinRotation::ReverseLandscape => Orientation::ReverseLandscape,
    })
}

/// A device's long-lived input stream and the pointers it holds down.
pub(super) struct InputState {
    sink: mpsc::Sender<proto::InputEvent>,
    /// Each pointer that is down and where it last was, released when the stream reopens.
    pressed: BTreeMap<u32, (i32, i32)>,
}

impl InputState {
    #[cfg(all(test, unix))]
    pub(super) fn closed(&self) -> bool {
        self.sink.is_closed()
    }
}

fn validate(events: &[DeviceInputEvent]) -> Result<(), DeviceOpError> {
    let invalid = |message: &str| Err(DeviceOpError::InvalidInput(message.to_owned()));
    if events.is_empty() {
        return invalid("Send at least one input event.");
    }
    if events.len() > MAX_INPUT_EVENTS {
        return invalid("Send at most 64 input events at a time.");
    }
    for event in events {
        match event {
            DeviceInputEvent::Touch {
                pointer_id, x, y, ..
            } => {
                if !x.is_finite() || !y.is_finite() {
                    return invalid("Touch positions must be numbers between 0 and 1.");
                }
                if i32::try_from(*pointer_id).is_err() {
                    return invalid("Touch pointer ids must be below 2147483648.");
                }
            }
            DeviceInputEvent::Text { text } if text.chars().count() > MAX_TEXT_CHARS => {
                return invalid("Text input is limited to 1,000 characters. Send it in parts.");
            }
            DeviceInputEvent::Key { .. } | DeviceInputEvent::Text { .. } => {}
        }
    }
    Ok(())
}

fn touch_event(x: i32, y: i32, pointer_id: u32, pressure: i32) -> proto::InputEvent {
    proto::InputEvent {
        r#type: Some(proto::input_event::Type::TouchEvent(proto::TouchEvent {
            touches: vec![proto::Touch {
                x,
                y,
                identifier: i32::try_from(pointer_id).unwrap_or(i32::MAX),
                pressure,
                expiration: proto::touch::EventExpiration::NeverExpire as i32,
                ..Default::default()
            }],
            ..Default::default()
        })),
    }
}

fn key_event(event: proto::KeyboardEvent) -> proto::InputEvent {
    proto::InputEvent {
        r#type: Some(proto::input_event::Type::KeyEvent(event)),
    }
}

/// Sends `event` on the device's input stream, opening it first if it is missing or
/// closed. A reopened stream first releases every pointer the old one left down.
async fn send(
    client: &EmulatorClient,
    tasks: &TaskTracker,
    state: &mut Option<InputState>,
    event: proto::InputEvent,
) -> Result<(), DeviceError> {
    let event = match state.as_ref() {
        Some(open) if !open.sink.is_closed() => match deliver(&open.sink, event).await {
            Ok(()) => return Ok(()),
            Err(Undelivered::Closed(event)) => event,
            Err(Undelivered::Failed(error)) => return Err(error),
        },
        _ => event,
    };
    reopen_into(client.input_sink(tasks)?, state, event).await
}

/// Reopens on `sink` and stores the result. The old state stays until the reopen finishes,
/// so a call dropped mid-reopen leaves every pointer it held for the next reopen to release.
async fn reopen_into(
    sink: mpsc::Sender<proto::InputEvent>,
    state: &mut Option<InputState>,
    event: proto::InputEvent,
) -> Result<(), DeviceError> {
    let pressed = state
        .as_ref()
        .map(|previous| previous.pressed.clone())
        .unwrap_or_default();
    let (reopened, sent) = reopen(sink, pressed, event).await;
    *state = Some(reopened);
    sent
}

/// Releases every pointer in `pressed` on a fresh `sink`, then sends `event`. Pointers whose
/// release was not delivered stay in the returned state, so the next reopen releases them.
async fn reopen(
    sink: mpsc::Sender<proto::InputEvent>,
    pressed: BTreeMap<u32, (i32, i32)>,
    event: proto::InputEvent,
) -> (InputState, Result<(), DeviceError>) {
    let undelivered = |failure: Undelivered| match failure {
        Undelivered::Closed(_) => DeviceError::Grpc("the input stream closed".into()),
        Undelivered::Failed(error) => error,
    };
    let mut pending = pressed.into_iter();
    while let Some((pointer_id, (x, y))) = pending.next() {
        if let Err(failure) = deliver(&sink, touch_event(x, y, pointer_id, 0)).await {
            let mut pressed: BTreeMap<_, _> = pending.collect();
            pressed.insert(pointer_id, (x, y));
            return (InputState { sink, pressed }, Err(undelivered(failure)));
        }
    }
    let sent = deliver(&sink, event).await.map_err(undelivered);
    (
        InputState {
            sink,
            pressed: BTreeMap::new(),
        },
        sent,
    )
}

enum Undelivered {
    Closed(proto::InputEvent),
    Failed(DeviceError),
}

async fn deliver(
    sink: &mpsc::Sender<proto::InputEvent>,
    event: proto::InputEvent,
) -> Result<(), Undelivered> {
    match tokio::time::timeout(INPUT_SEND_TIMEOUT, sink.send(event)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(mpsc::error::SendError(event))) => Err(Undelivered::Closed(event)),
        Err(_) => Err(Undelivered::Failed(DeviceError::Grpc(format!(
            "the input stream accepted nothing for {INPUT_SEND_TIMEOUT:?}"
        )))),
    }
}

impl DeviceManager {
    /// Forwards `events`, in order, to a `Ready` device. The whole batch is checked before
    /// anything is sent.
    pub async fn input(
        &self,
        device_id: &str,
        events: Vec<DeviceInputEvent>,
    ) -> Result<(), DeviceOpError> {
        validate(&events)?;
        let (client, _, (width, height), orientation) = self.client(device_id)?;
        let input = self.inner.input_state(device_id)?;
        // Held for the whole batch, so concurrent batches never interleave.
        let mut state = input.lock().await;
        for event in events {
            let (message, pointer) = match event {
                DeviceInputEvent::Touch {
                    action,
                    pointer_id,
                    x,
                    y,
                } => {
                    let (x, y) = to_device_pixels(x, y, width, height, orientation);
                    let down = matches!(action, TouchAction::Down | TouchAction::Move);
                    (
                        touch_event(x, y, pointer_id, i32::from(down)),
                        Some((pointer_id, down.then_some((x, y)))),
                    )
                }
                DeviceInputEvent::Key { key, action } => {
                    let event_type = match action {
                        KeyAction::Down => proto::keyboard_event::KeyEventType::Keydown,
                        KeyAction::Up => proto::keyboard_event::KeyEventType::Keyup,
                    };
                    let event = proto::KeyboardEvent {
                        code_type: proto::keyboard_event::KeyCodeType::Usb as i32,
                        event_type: event_type as i32,
                        key: w3c_key(key).to_owned(),
                        ..Default::default()
                    };
                    (key_event(event), None)
                }
                DeviceInputEvent::Text { text } => (
                    key_event(proto::KeyboardEvent {
                        text,
                        ..Default::default()
                    }),
                    None,
                ),
            };
            send(&client, &self.inner.tasks, &mut state, message)
                .await
                .map_err(|error| super::emulator_failure("input", &error))?;
            if let (Some((pointer_id, position)), Some(open)) = (pointer, state.as_mut()) {
                match position {
                    Some(position) => open.pressed.insert(pointer_id, position),
                    None => open.pressed.remove(&pointer_id),
                };
            }
        }
        Ok(())
    }

    /// Asks the emulator to turn toward `orientation`, then follows what it reports: the
    /// catalog only changes when the screen did.
    pub async fn rotate(
        &self,
        device_id: &str,
        orientation: Orientation,
    ) -> Result<(), DeviceOpError> {
        let (client, _, (width, height), _) = self.client(device_id)?;
        client
            .set_rotation(rotation_vector(orientation))
            .await
            .map_err(|error| super::emulator_failure("rotate", &error))?;
        for poll in 0..ROTATION_POLLS {
            if poll > 0 {
                tokio::time::sleep(ROTATION_POLL_INTERVAL).await;
            }
            let reported = match client.screenshot(ImgFormat::Png, 1).await {
                Ok(image) => image
                    .format
                    .and_then(|format| format.rotation)
                    .and_then(|rotation| reported_orientation(rotation.rotation)),
                Err(error) => {
                    tracing::debug!(%error, "reading the device's orientation failed");
                    None
                }
            };
            if let Some(reported) = reported {
                let (width, height) = oriented_size(width, height, reported);
                self.set_reported_orientation(device_id, reported, width, height);
                if reported == orientation {
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_normalized_coordinates_per_orientation() {
        assert_eq!(
            to_device_pixels(0.5, 0.25, 1080, 2400, Orientation::Portrait),
            (540, 600)
        );
        assert_eq!(
            to_device_pixels(0.5, 0.25, 1080, 2400, Orientation::Landscape),
            (1200, 270)
        );
        assert_eq!(
            to_device_pixels(1.2, -0.1, 1080, 2400, Orientation::Portrait),
            (1079, 0)
        );
    }

    #[test]
    fn maps_keys_rotations_and_reported_orientations() {
        assert_eq!(w3c_key(DeviceKey::Back), "GoBack");
        assert_eq!(w3c_key(DeviceKey::Home), "GoHome");
        assert_eq!(w3c_key(DeviceKey::VolumeDown), "AudioVolumeDown");
        assert_eq!(rotation_vector(Orientation::Landscape), [0.0, 0.0, -90.0]);
        assert_eq!(
            rotation_vector(Orientation::ReverseLandscape),
            [0.0, 0.0, 90.0]
        );
        assert_eq!(
            reported_orientation(SkinRotation::ReverseLandscape as i32),
            Some(Orientation::ReverseLandscape)
        );
        assert_eq!(reported_orientation(42), None);
    }

    #[test]
    fn deserializes_the_wire_shape() {
        let events: Vec<DeviceInputEvent> = serde_json::from_value(serde_json::json!([
            { "type": "touch", "action": "down", "pointerId": 2, "x": 0.5, "y": 0.25 },
            { "type": "key", "key": "appSwitch", "action": "up" },
            { "type": "text", "text": "hi" },
        ]))
        .unwrap();
        assert!(matches!(
            events[0],
            DeviceInputEvent::Touch {
                action: TouchAction::Down,
                pointer_id: 2,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            DeviceInputEvent::Key {
                key: DeviceKey::AppSwitch,
                action: KeyAction::Up
            }
        ));
        assert!(matches!(&events[2], DeviceInputEvent::Text { text } if text == "hi"));
    }

    #[test]
    fn debug_output_never_contains_text() {
        let event = DeviceInputEvent::Text {
            text: "hunter2 secret".into(),
        };
        let printed = format!("{event:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("len: 14"), "{printed}");
    }

    #[tokio::test]
    async fn pointers_whose_release_failed_are_released_by_the_next_reopen() {
        let pressed = BTreeMap::from([(0, (540, 1200)), (1, (10, 20))]);
        // First reopen: the stream breaks before any release is delivered.
        let (broken, closed) = mpsc::channel(8);
        drop(closed);
        let (state, sent) = reopen(broken, pressed, touch_event(1, 1, 2, 1)).await;
        assert!(sent.is_err());
        assert_eq!(state.pressed.len(), 2);
        // Second reopen: both pointers are released before the new event.
        let (sink, mut received) = mpsc::channel(8);
        let (state, sent) = reopen(sink, state.pressed, touch_event(1, 1, 2, 1)).await;
        sent.unwrap();
        assert!(state.pressed.is_empty());
        let mut released = Vec::new();
        for _ in 0..3 {
            let event = received.recv().await.unwrap();
            let Some(proto::input_event::Type::TouchEvent(touch)) = event.r#type else {
                panic!("not a touch");
            };
            released.push((touch.touches[0].identifier, touch.touches[0].pressure));
        }
        assert_eq!(released, vec![(0, 0), (1, 0), (2, 1)]);
    }

    #[tokio::test]
    async fn a_dropped_reopen_keeps_the_pointers_it_held() {
        use futures_util::FutureExt as _;
        let (closed_sink, closed) = mpsc::channel(1);
        drop(closed);
        let mut state = Some(InputState {
            sink: closed_sink,
            pressed: BTreeMap::from([(0, (540, 1200)), (1, (10, 20))]),
        });
        // The first release fills the stream, so the second waits; the call is dropped there.
        let (sink, mut received) = mpsc::channel(1);
        assert!(
            reopen_into(sink, &mut state, touch_event(1, 1, 2, 1))
                .now_or_never()
                .is_none()
        );
        assert_eq!(state.as_ref().unwrap().pressed.len(), 2);
        received.recv().await.unwrap();
        // The next reopen still releases both pointers before its event.
        let (sink, mut received) = mpsc::channel(8);
        reopen_into(sink, &mut state, touch_event(1, 1, 2, 1))
            .await
            .unwrap();
        assert!(state.as_ref().unwrap().pressed.is_empty());
        let mut sent = Vec::new();
        for _ in 0..3 {
            let event = received.recv().await.unwrap();
            let Some(proto::input_event::Type::TouchEvent(touch)) = event.r#type else {
                panic!("not a touch");
            };
            sent.push((touch.touches[0].identifier, touch.touches[0].pressure));
        }
        assert_eq!(sent, vec![(0, 0), (1, 0), (2, 1)]);
    }

    #[test]
    fn rejects_empty_batches_and_non_finite_positions() {
        assert!(matches!(validate(&[]), Err(DeviceOpError::InvalidInput(_))));
        let nan = DeviceInputEvent::Touch {
            action: TouchAction::Down,
            pointer_id: 0,
            x: f64::NAN,
            y: 0.5,
        };
        assert!(matches!(
            validate(&[nan]),
            Err(DeviceOpError::InvalidInput(_))
        ));
        let text = DeviceInputEvent::Text {
            text: "é".repeat(MAX_TEXT_CHARS),
        };
        assert!(validate(&[text]).is_ok());
    }
}

#[cfg(all(test, unix))]
mod device_tests {
    use super::*;
    use crate::device::{
        DeviceOpError,
        android::testing::{ManagerFixture, wait_until},
    };

    fn touch(action: TouchAction, x: f64, y: f64) -> DeviceInputEvent {
        DeviceInputEvent::Touch {
            action,
            pointer_id: 0,
            x,
            y,
        }
    }

    fn only_touch(event: &proto::InputEvent) -> &proto::Touch {
        match &event.r#type {
            Some(proto::input_event::Type::TouchEvent(touch)) => &touch.touches[0],
            other => panic!("not a touch: {other:?}"),
        }
    }

    fn touch_pressure(event: &proto::InputEvent) -> i32 {
        only_touch(event).pressure
    }

    fn touch_xy(event: &proto::InputEvent) -> (i32, i32) {
        let touch = only_touch(event);
        (touch.x, touch.y)
    }

    fn keyboard(event: &proto::InputEvent) -> &proto::KeyboardEvent {
        match &event.r#type {
            Some(proto::input_event::Type::KeyEvent(key)) => key,
            other => panic!("not a key: {other:?}"),
        }
    }

    fn key_of(event: &proto::InputEvent) -> (&str, i32) {
        let key = keyboard(event);
        (key.key.as_str(), key.event_type)
    }

    fn text_of(event: &proto::InputEvent) -> &str {
        &keyboard(event).text
    }

    #[tokio::test]
    async fn forwards_touch_key_and_text_in_order() {
        let fixture = ManagerFixture::ready_device().await;
        fixture
            .manager
            .input(
                &fixture.id,
                vec![
                    touch(TouchAction::Down, 0.5, 0.5),
                    touch(TouchAction::Move, 0.6, 0.5),
                    touch(TouchAction::Up, 0.6, 0.5),
                    DeviceInputEvent::Key {
                        key: DeviceKey::Back,
                        action: KeyAction::Down,
                    },
                    DeviceInputEvent::Text { text: "hi".into() },
                ],
            )
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(5).await;
        assert_eq!(touch_pressure(&inputs[0]), 1);
        assert_eq!(touch_xy(&inputs[1]), (648, 1200));
        assert_eq!(touch_pressure(&inputs[2]), 0);
        assert_eq!(key_of(&inputs[3]), ("GoBack", 0 /* keydown */));
        assert_eq!(text_of(&inputs[4]), "hi");
        // Shutdown waits for the input stream's task, which ends with the device.
        tokio::time::timeout(Duration::from_secs(20), fixture.manager.shutdown())
            .await
            .expect("shutdown ends the input stream");
    }

    #[tokio::test]
    async fn rejects_oversized_batches_and_text() {
        let fixture = ManagerFixture::ready_device().await;
        let many = (0..65)
            .map(|_| touch(TouchAction::Move, 0.1, 0.1))
            .collect();
        assert!(matches!(
            fixture.manager.input(&fixture.id, many).await,
            Err(DeviceOpError::InvalidInput(_))
        ));
        let long = DeviceInputEvent::Text {
            text: "x".repeat(1001),
        };
        assert!(matches!(
            fixture.manager.input(&fixture.id, vec![long]).await,
            Err(DeviceOpError::InvalidInput(_))
        ));
        // Nothing of a refused batch reaches the device.
        assert!(fixture.controller.inputs.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dangling_touch_is_released_on_stream_reset() {
        let fixture = ManagerFixture::ready_device().await;
        fixture
            .manager
            .input(&fixture.id, vec![touch(TouchAction::Down, 0.5, 0.5)])
            .await
            .unwrap();
        fixture.controller.wait_inputs(1).await;
        fixture.controller.break_input_stream().await; // fake drops the stream
        wait_until(|| fixture.manager.input_stream_closed(&fixture.id)).await;
        fixture
            .manager
            .input(&fixture.id, vec![touch(TouchAction::Down, 0.2, 0.2)])
            .await
            .unwrap();
        // New stream: release of pointer 0 where it was, then the new down.
        let inputs = fixture.controller.wait_inputs(2).await;
        assert_eq!(touch_pressure(&inputs[0]), 0);
        assert_eq!(touch_xy(&inputs[0]), (540, 1200));
        assert_eq!(touch_pressure(&inputs[1]), 1);
        assert_eq!(touch_xy(&inputs[1]), (216, 480));
    }

    #[tokio::test]
    async fn rotate_sends_vector_but_catalog_follows_reported_rotation() {
        let fixture = ManagerFixture::ready_device().await;
        fixture.controller.report_rotation(SkinRotation::Portrait); // app refuses to rotate
        fixture
            .manager
            .rotate(&fixture.id, Orientation::Landscape)
            .await
            .unwrap();
        assert_eq!(
            fixture.controller.rotations.lock().unwrap().last(),
            Some(&[0.0, 0.0, -90.0])
        );
        assert_eq!(fixture.snapshot().orientation, Orientation::Portrait);
        fixture.controller.report_rotation(SkinRotation::Landscape);
        fixture
            .manager
            .rotate(&fixture.id, Orientation::Landscape)
            .await
            .unwrap();
        fixture.wait_orientation(Orientation::Landscape).await;
        let snapshot = fixture.snapshot();
        assert_eq!((snapshot.width, snapshot.height), (Some(2400), Some(1080)));
        // Touches still land in the rotated display.
        fixture
            .manager
            .input(&fixture.id, vec![touch(TouchAction::Down, 0.5, 0.25)])
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(1).await;
        assert_eq!(touch_xy(&inputs[0]), (1200, 270));
    }

    #[tokio::test]
    async fn a_failed_emulator_call_is_an_emulator_error() {
        let fixture = ManagerFixture::ready_device().await;
        fixture.controller.fail_rotation();
        let error = fixture
            .manager
            .rotate(&fixture.id, Orientation::Landscape)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, DeviceOpError::Emulator(message) if message.contains("did not rotate")),
            "{error:?}"
        );
        assert_eq!(error.to_rpc_error()["_tag"], "DeviceEmulatorError");
        // The device itself is still running.
        assert_eq!(
            fixture.snapshot().state,
            crate::device::catalog::DeviceState::Ready
        );
    }

    #[tokio::test]
    async fn input_and_rotate_need_a_ready_device() {
        let fixture = ManagerFixture::ready_device().await;
        fixture.manager.close(&fixture.id).await.unwrap();
        assert!(matches!(
            fixture
                .manager
                .input(&fixture.id, vec![touch(TouchAction::Down, 0.5, 0.5)])
                .await,
            Err(DeviceOpError::NotReady(_))
        ));
        assert!(matches!(
            fixture
                .manager
                .rotate(&fixture.id, Orientation::Landscape)
                .await,
            Err(DeviceOpError::NotReady(_))
        ));
    }
}
