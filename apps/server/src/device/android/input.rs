//! The emulator's input stream, touch and key mapping, and rotation.

use std::{collections::BTreeMap, time::Duration};

use tokio::sync::mpsc;
use tokio_util::task::TaskTracker;

use super::{
    emulator_failure,
    grpc::{
        EmulatorClient,
        proto::{self, image_format::ImgFormat, rotation::SkinRotation},
    },
};
use crate::device::{
    DeviceError, DeviceOpError,
    catalog::Orientation,
    input::{DeviceInputEvent, DeviceKey, KeyAction, TouchAction, oriented_size, to_device_pixels},
};

/// How long one event may wait for room in the emulator's input stream.
const INPUT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// How often `rotate` reads the orientation the emulator reports, and how far apart.
const ROTATION_POLLS: usize = 5;
const ROTATION_POLL_INTERVAL: Duration = Duration::from_millis(300);

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
pub(crate) struct InputState {
    sink: mpsc::Sender<proto::InputEvent>,
    /// Each pointer that is down and where it last was, released when the stream reopens.
    pressed: BTreeMap<u32, (i32, i32)>,
}

impl InputState {
    #[cfg(test)]
    pub(super) fn closed(&self) -> bool {
        self.sink.is_closed()
    }
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

/// Forwards `events`, in order, on the device's input stream. `native` is the portrait
/// display size and `orientation` the catalog's current one.
pub(super) async fn send_batch(
    client: &EmulatorClient,
    tasks: &TaskTracker,
    input: &tokio::sync::Mutex<Option<InputState>>,
    events: Vec<DeviceInputEvent>,
    (width, height): (u32, u32),
    orientation: Orientation,
) -> Result<(), DeviceOpError> {
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
        send(client, tasks, &mut state, message)
            .await
            .map_err(|error| emulator_failure("input", &error))?;
        if let (Some((pointer_id, position)), Some(open)) = (pointer, state.as_mut()) {
            match position {
                Some(position) => open.pressed.insert(pointer_id, position),
                None => open.pressed.remove(&pointer_id),
            };
        }
    }
    Ok(())
}

/// Asks the emulator to turn toward `orientation`, then follows what it reports: `report`
/// hears each orientation and display size the screen actually took.
pub(super) async fn rotate(
    client: &EmulatorClient,
    (width, height): (u32, u32),
    orientation: Orientation,
    report: &(dyn Fn(Orientation, u32, u32) + Send + Sync),
) -> Result<(), DeviceOpError> {
    client
        .set_rotation(rotation_vector(orientation))
        .await
        .map_err(|error| emulator_failure("rotate", &error))?;
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
            report(reported, width, height);
            if reported == orientation {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
