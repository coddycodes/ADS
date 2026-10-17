//! Touch, key, and text input and rotation for a running device.

use serde::Deserialize;

use super::{DeviceManager, DeviceOpError, catalog::Orientation};

/// The most events one `device.input` batch may carry.
pub(crate) const MAX_INPUT_EVENTS: usize = 64;
/// The most characters one text event may carry.
pub(crate) const MAX_TEXT_CHARS: usize = 1_000;

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

/// The upright picture's size in `orientation` of a device whose native (portrait) display
/// is `width` x `height`: a landscape screen swaps the axes, as the emulator's rotated video
/// does (native verification item 3). Touches are not mapped here: each backend turns the
/// upright picture's points into what its input path expects.
pub(crate) fn oriented_size(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Portrait | Orientation::ReversePortrait => (width, height),
        Orientation::Landscape | Orientation::ReverseLandscape => (height, width),
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
impl DeviceManager {
    /// Forwards `events`, in order, to a `Ready` device. The whole batch is checked before
    /// anything is sent.
    pub async fn input(
        &self,
        device_id: &str,
        events: Vec<DeviceInputEvent>,
    ) -> Result<(), DeviceOpError> {
        validate(&events)?;
        let (device, orientation) = self.inner.running(device_id)?;
        device.input(events, orientation).await
    }

    /// Asks the device to turn toward `orientation`, then follows what it reports: the
    /// catalog only changes when the screen did.
    pub async fn rotate(
        &self,
        device_id: &str,
        orientation: Orientation,
    ) -> Result<(), DeviceOpError> {
        let (device, _) = self.inner.running(device_id)?;
        let inner = &self.inner;
        device
            .rotate(orientation, &|reported, width, height| {
                inner.set_reported_orientation(device_id, reported, width, height);
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landscape_pictures_swap_the_native_axes() {
        // The emulator's rotated video measured 1280x576 for a 576x1280 portrait stream.
        for (orientation, size) in [
            (Orientation::Portrait, (1080, 2400)),
            (Orientation::ReversePortrait, (1080, 2400)),
            (Orientation::Landscape, (2400, 1080)),
            (Orientation::ReverseLandscape, (2400, 1080)),
        ] {
            assert_eq!(
                oriented_size(1080, 2400, orientation),
                size,
                "{orientation:?}"
            );
        }
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
    use std::time::Duration;

    use super::*;
    use crate::device::{
        DeviceOpError,
        android::{
            grpc::proto::{self, rotation::SkinRotation},
            testing::{ManagerFixture, wait_until},
        },
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
            Some(&[0.0, 0.0, 90.0])
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
        // A touch on the upright landscape picture reaches the emulator in unrotated panel
        // pixels: turned counter-clockwise, the picture's top is the panel's right edge.
        fixture
            .manager
            .input(&fixture.id, vec![touch(TouchAction::Down, 0.5, 0.25)])
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(1).await;
        assert_eq!(touch_xy(&inputs[0]), (1079 - 270, 1200));
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
