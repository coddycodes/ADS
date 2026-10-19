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

/// The upright picture's size in `orientation` of a device whose native display (in its
/// natural orientation, `Portrait`) is `width` x `height`: a landscape screen swaps the
/// axes, as the emulator's rotated video does (native verification item 3). Touches are not
/// mapped here: each backend turns the upright picture's points into what its input path
/// expects.
pub(crate) fn oriented_size(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Portrait | Orientation::ReversePortrait => (width, height),
        Orientation::Landscape | Orientation::ReverseLandscape => (height, width),
    }
}

/// Whether every character of `text` is US-typeable: printable ASCII, Return (`\n`, `\r`),
/// and Tab. iOS types this set as US keys and pastes anything else. Android refuses text
/// outside it, because the emulator's proto promises only printable ASCII in
/// `KeyboardEvent.text`; Return and Tab are sent as text anyway, and are not yet confirmed
/// on a real AVD. If Android turns out to drop either, narrow Android's own check in
/// `android::input::send_batch`, not this set, which iOS's type-or-paste choice shares. The
/// client's `isUsTypeable` (`apps/web/src/components/device/deviceKeyMap.ts`) is the same
/// set, and its test uses the same vectors as this one's.
pub(crate) fn is_us_typeable(text: &str) -> bool {
    text.chars()
        .all(|c| matches!(c, ' '..='~' | '\n' | '\r' | '\t'))
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
    /// catalog only changes when the screen did. Returns the orientation the device ended
    /// in, which differs from `orientation` when the screen kept its own.
    pub async fn rotate(
        &self,
        device_id: &str,
        orientation: Orientation,
    ) -> Result<Orientation, DeviceOpError> {
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

    /// The set `isUsTypeable` in `deviceKeyMap.ts` accepts, with the same vectors as its
    /// test: every printable ASCII character, Return, and Tab. Anything else would be dropped
    /// by the Android emulator without a word.
    #[test]
    fn us_typeable_text_is_printable_ascii_with_return_and_tab() {
        let printable: String = (0x20u8..=0x7e).map(char::from).collect();
        assert!(is_us_typeable(&printable));
        assert!(is_us_typeable("line one\nline two\r\n\tindented"));
        assert!(is_us_typeable(""));
        for refused in [
            "h\u{e9}llo",
            "\u{e9}",
            "\u{65e5}\u{672c}",
            "\u{1f600}",
            "a\u{a0}b",
            "\u{7f}",
            "\u{0}",
            "\u{1b}",
            "\u{8}",
            "\u{2028}",
        ] {
            assert!(!is_us_typeable(refused), "{refused:?}");
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
            input::MAX_ANDROID_TEXT_PER_BATCH,
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

    fn texts_of(events: &[proto::InputEvent]) -> Vec<&str> {
        events
            .iter()
            .map(|event| keyboard(event).text.as_str())
            .collect()
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
        let inputs = fixture.controller.wait_inputs(6).await;
        assert_eq!(touch_pressure(&inputs[0]), 1);
        assert_eq!(touch_xy(&inputs[1]), (648, 1200));
        assert_eq!(touch_pressure(&inputs[2]), 0);
        assert_eq!(key_of(&inputs[3]), ("GoBack", 0 /* keydown */));
        // Text is typed a character per send (`android::input::TEXT_CHARS_PER_SEND`).
        assert_eq!(texts_of(&inputs[4..]), ["h", "i"]);
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

    /// The copy a refused batch shows (`android::input::UNTYPEABLE_TEXT`); the client shows the
    /// server's message as it is.
    const ASCII_ONLY: &str = "This device can type only plain ASCII text. Accented letters, emoji, and other characters can't be sent from here yet.";

    /// Review finding P2: the emulator's `KeyboardEvent.text` drops most characters outside
    /// printable ASCII (`emulator_controller.proto`) while the call succeeds. A batch that
    /// carries one is refused whole, with a typed `DeviceInputError`, and none of it reaches
    /// the emulator. `ok` after each refusal proves the stream saw nothing before it: the
    /// stream is ordered, so an event that had been sent would arrive ahead of `ok`.
    #[tokio::test]
    async fn refuses_text_the_emulator_would_drop() {
        for refused in ["héllo", "日本語", "ok 😀", "tab\u{7f}del", "caf\u{e9}"] {
            let fixture = ManagerFixture::ready_device().await;
            let error = fixture
                .manager
                .input(
                    &fixture.id,
                    vec![DeviceInputEvent::Text {
                        text: refused.into(),
                    }],
                )
                .await
                .unwrap_err();
            assert_eq!(
                error,
                DeviceOpError::InvalidInput(ASCII_ONLY.into()),
                "{refused:?}"
            );
            let rpc = error.to_rpc_error();
            assert_eq!(rpc["_tag"], "DeviceInputError");
            assert_eq!(rpc["message"], ASCII_ONLY);
            // The refusal never echoes what was typed.
            assert!(!rpc["message"].as_str().unwrap().contains(refused));
            fixture
                .manager
                .input(
                    &fixture.id,
                    vec![DeviceInputEvent::Text { text: "ok".into() }],
                )
                .await
                .unwrap();
            let inputs = fixture.controller.wait_inputs(2).await;
            assert_eq!(texts_of(&inputs), ["o", "k"], "{refused:?}");
        }
    }

    /// The refusal is all or nothing: the touch before the text is not sent either, so no
    /// pointer is left down on the device.
    #[tokio::test]
    async fn a_mixed_batch_with_untypeable_text_is_refused_whole() {
        let fixture = ManagerFixture::ready_device().await;
        let error = fixture
            .manager
            .input(
                &fixture.id,
                vec![
                    touch(TouchAction::Down, 0.5, 0.5),
                    DeviceInputEvent::Text {
                        text: "héllo".into(),
                    },
                    touch(TouchAction::Up, 0.5, 0.5),
                ],
            )
            .await
            .unwrap_err();
        assert_eq!(error, DeviceOpError::InvalidInput(ASCII_ONLY.into()));
        fixture
            .manager
            .input(
                &fixture.id,
                vec![DeviceInputEvent::Text { text: "ok".into() }],
            )
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(2).await;
        assert_eq!(texts_of(&inputs), ["o", "k"]);
    }

    #[tokio::test]
    async fn printable_ascii_with_newline_and_tab_arrives_in_order() {
        let fixture = ManagerFixture::ready_device().await;
        let text = "Hello, World! ~{}[]\\|\"'`\nline two\r\n\tindented";
        fixture
            .manager
            .input(
                &fixture.id,
                vec![DeviceInputEvent::Text { text: text.into() }],
            )
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(text.len()).await;
        assert_eq!(texts_of(&inputs).concat(), text);
    }

    /// The longest paste Android takes, 100 characters, arrives whole and in order, a
    /// character per send.
    #[tokio::test]
    async fn a_paste_of_a_hundred_characters_arrives_whole_and_in_order() {
        let fixture = ManagerFixture::ready_device().await;
        let text = pasted(MAX_ANDROID_TEXT_PER_BATCH);
        fixture
            .manager
            .input(
                &fixture.id,
                vec![DeviceInputEvent::Text { text: text.clone() }],
            )
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(text.len()).await;
        let sent = texts_of(&inputs);
        assert_eq!(sent.len(), text.len());
        assert_eq!(sent.concat(), text);
    }

    /// The copy a batch with too much text shows (`android::input::TOO_MUCH_TEXT`).
    const TOO_MUCH_TEXT: &str =
        "This device can paste up to 100 characters at a time. Paste longer text in parts.";

    /// Android would drop the tail of a longer paste without a word (the guest drops keys
    /// queued past 10 s), so it is refused whole, with a typed `DeviceInputError`, and the
    /// touch in the same batch is not sent either: `ok` afterwards arrives alone.
    #[tokio::test]
    async fn a_paste_over_a_hundred_characters_is_refused_whole() {
        let fixture = ManagerFixture::ready_device().await;
        let error = fixture
            .manager
            .input(
                &fixture.id,
                vec![
                    touch(TouchAction::Down, 0.5, 0.5),
                    DeviceInputEvent::Text {
                        text: pasted(MAX_ANDROID_TEXT_PER_BATCH + 1),
                    },
                ],
            )
            .await
            .unwrap_err();
        assert_eq!(error, DeviceOpError::InvalidInput(TOO_MUCH_TEXT.into()));
        let rpc = error.to_rpc_error();
        assert_eq!(rpc["_tag"], "DeviceInputError");
        assert_eq!(rpc["message"], TOO_MUCH_TEXT);
        fixture
            .manager
            .input(
                &fixture.id,
                vec![DeviceInputEvent::Text { text: "ok".into() }],
            )
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(2).await;
        assert_eq!(texts_of(&inputs), ["o", "k"]);
    }

    /// `len` mixed-case characters.
    fn pasted(len: usize) -> String {
        "The Quick BROWN fox, 0123456789! "
            .chars()
            .cycle()
            .take(len)
            .collect()
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
        let ended = fixture
            .manager
            .rotate(&fixture.id, Orientation::Landscape)
            .await
            .unwrap();
        assert_eq!(
            ended,
            Orientation::Portrait,
            "the screen kept its orientation"
        );
        // The turn never showed within the window: the sensor goes back to portrait, so it
        // matches the screen again.
        assert_eq!(
            *fixture.controller.rotations.lock().unwrap(),
            [[0.0, 0.0, 90.0], [0.0, 0.0, 0.0]]
        );
        assert_eq!(fixture.snapshot().orientation, Orientation::Portrait);
        fixture.controller.report_rotation(SkinRotation::Landscape);
        let ended = fixture
            .manager
            .rotate(&fixture.id, Orientation::Landscape)
            .await
            .unwrap();
        assert_eq!(ended, Orientation::Landscape);
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

    /// Review round 2, Important 1: every read of the skin fails and no video runs, so
    /// nothing shows whether the guest turned. The sensor stays where it was sent and the
    /// turn is taken as asked, so the returned orientation, the catalog, and the touch
    /// mapping agree and the client shows no "didn't rotate" note.
    #[tokio::test]
    async fn a_rotate_without_any_evidence_is_taken_as_asked() {
        let fixture = ManagerFixture::ready_device().await;
        fixture.controller.fail_screenshots();
        let ended = fixture
            .manager
            .rotate(&fixture.id, Orientation::ReverseLandscape)
            .await
            .unwrap();
        assert_eq!(ended, Orientation::ReverseLandscape);
        assert_eq!(
            *fixture.controller.rotations.lock().unwrap(),
            [[0.0, 0.0, -90.0]]
        );
        fixture
            .wait_orientation(Orientation::ReverseLandscape)
            .await;
        let snapshot = fixture.snapshot();
        assert_eq!((snapshot.width, snapshot.height), (Some(2400), Some(1080)));
        // Turned clockwise: the picture's top is the panel's left edge, and its left edge
        // is the panel's bottom.
        fixture
            .manager
            .input(&fixture.id, vec![touch(TouchAction::Down, 0.5, 0.25)])
            .await
            .unwrap();
        let inputs = fixture.controller.wait_inputs(1).await;
        assert_eq!(touch_xy(&inputs[0]), (270, 2399 - 1200));
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
