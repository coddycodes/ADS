//! Live input for a simulator, planned without FFI: which HID action each validated event
//! becomes (spec §5), the DTUHID XPC messages and legacy Indigo messages for those actions
//! (spec §0, S1), and the [`HidSink`] that sends them. `coresim::HidClient` is the real sink.

use std::ffi::CStr;

use futures_util::future::BoxFuture;

use super::{keymap::us_usage, pixels::IosOrientation, pixels::Turn};
use crate::device::{
    DeviceOpError,
    catalog::Orientation,
    input::{DeviceInputEvent, DeviceKey, KeyAction, TouchAction},
};

/// Keys and text need DTUHID; legacy keyboard input is dropped (spec §0).
pub(crate) const KEYBOARD_NOT_READY: &str =
    "The simulator is not accepting keyboard input yet. Try again in a few seconds.";
/// A send the simulator did not accept.
pub(crate) const INPUT_FAILED: &str = "The simulator did not accept the input. Try again; if it keeps failing, stop the device and start it again.";
/// A Purple rotation the simulator did not accept.
pub(crate) const ROTATE_FAILED: &str = "The simulator did not rotate. Try again; if it keeps failing, stop the device and start it again.";

/// The DTUHID service every message goes to, and its `featureIdentifier`.
pub(crate) const DIGITIZER_SERVICE: &CStr = c"com.apple.coredevice.feature.remote.hid.digitizer";

const SHIFT: u32 = 0xE1;
const COMMAND: u32 = 0xE3;
const KEY_V: u32 = 0x19;
/// HID consumer page, where the simulator's buttons live.
const CONSUMER_PAGE: u64 = 0x0C;

/// What to send for one validated event.
#[derive(PartialEq)]
pub(crate) enum HidAction {
    /// Ratios in the portrait framebuffer's space.
    Touch {
        phase: TouchPhase,
        x: f64,
        y: f64,
    },
    Button {
        button: SimButton,
        down: bool,
    },
    /// A HID keyboard usage (page 0x07).
    Key {
        usage: u32,
        down: bool,
    },
    /// Text no US key types: `simctl pbcopy`, then Cmd+V.
    Paste {
        text: String,
    },
}

/// Never prints pasted text, only its length.
impl std::fmt::Debug for HidAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Touch { phase, x, y } => formatter
                .debug_struct("Touch")
                .field("phase", phase)
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::Button { button, down } => formatter
                .debug_struct("Button")
                .field("button", button)
                .field("down", down)
                .finish(),
            Self::Key { usage, down } => formatter
                .debug_struct("Key")
                .field("usage", usage)
                .field("down", down)
                .finish(),
            Self::Paste { text } => formatter
                .debug_struct("Paste")
                .field("len", &text.chars().count())
                .finish(),
        }
    }
}

impl HidAction {
    /// Keys and pastes go through the keyboard, which only DTUHID delivers.
    pub(crate) fn needs_keyboard(&self) -> bool {
        matches!(self, Self::Key { .. } | Self::Paste { .. })
    }

    /// The key or button this presses (`true`) or releases (`false`); `None` for a touch
    /// or a paste.
    pub(crate) fn press(&self) -> Option<(Press, bool)> {
        match *self {
            Self::Key { usage, down } => Some((Press::Key(usage), down)),
            Self::Button { button, down } => Some((Press::Button(button), down)),
            Self::Touch { .. } | Self::Paste { .. } => None,
        }
    }
}

/// A key or button the guest may hold down.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum Press {
    /// A HID keyboard usage (page 0x07).
    Key(u32),
    Button(SimButton),
}

impl Press {
    /// What lifts it.
    pub(crate) fn release(self) -> HidAction {
        match self {
            Self::Key(usage) => HidAction::Key { usage, down: false },
            Self::Button(button) => HidAction::Button {
                button,
                down: false,
            },
        }
    }

    /// Shift, Control, Option, and Command, either side (usages 0xE0–0xE7).
    pub(crate) fn is_modifier(self) -> bool {
        matches!(self, Self::Key(0xE0..=0xE7))
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum TouchPhase {
    Down,
    Move,
    Up,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum SimButton {
    Home,
    Lock,
    VolumeUp,
    VolumeDown,
}

impl SimButton {
    /// The button's usage on the HID consumer page.
    fn usage(self) -> u64 {
        match self {
            Self::Home => 0x40,
            Self::Lock => 0x30,
            Self::VolumeUp => 0xE9,
            Self::VolumeDown => 0xEA,
        }
    }
}

/// Tracks the single active pointer between batches: iOS input has one finger (spec §5).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PointerState {
    active: Option<u32>,
}

/// A ratio in the current orientation, mapped into the portrait framebuffer, which is where
/// the simulator takes touches (spec §0's table). Each mapping undoes the turn that shows
/// the frame upright (`pixels::upright_rgb`).
fn to_portrait(x: f64, y: f64, orientation: Orientation) -> (f64, f64) {
    let (u, v) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    match IosOrientation::from_orientation(orientation).turn() {
        Turn::None => (u, v),
        // The UI's top lies along the surface's left edge (`uiOrientation` 3).
        Turn::Clockwise => (v, 1.0 - u),
        // The UI's top lies along the surface's right edge (`uiOrientation` 4).
        Turn::CounterClockwise => (1.0 - v, u),
        Turn::Half => (1.0 - u, 1.0 - v),
    }
}

/// What a device key presses on iOS.
enum KeyTarget {
    Button(SimButton),
    /// A HID keyboard usage.
    Usage(u32),
}

fn key_target(key: DeviceKey) -> Result<KeyTarget, DeviceOpError> {
    let invalid = |message: &str| Err(DeviceOpError::InvalidInput(message.to_owned()));
    Ok(match key {
        DeviceKey::Back => return invalid("iOS has no Back key."),
        DeviceKey::AppSwitch => return invalid("iOS has no Recents key."),
        DeviceKey::Home => KeyTarget::Button(SimButton::Home),
        DeviceKey::Power => KeyTarget::Button(SimButton::Lock),
        DeviceKey::VolumeUp => KeyTarget::Button(SimButton::VolumeUp),
        DeviceKey::VolumeDown => KeyTarget::Button(SimButton::VolumeDown),
        DeviceKey::Enter => KeyTarget::Usage(0x28),
        DeviceKey::Backspace => KeyTarget::Usage(0x2A),
        DeviceKey::Delete => KeyTarget::Usage(0x4C),
        DeviceKey::Tab => KeyTarget::Usage(0x2B),
        DeviceKey::ArrowRight => KeyTarget::Usage(0x4F),
        DeviceKey::ArrowLeft => KeyTarget::Usage(0x50),
        DeviceKey::ArrowDown => KeyTarget::Usage(0x51),
        DeviceKey::ArrowUp => KeyTarget::Usage(0x52),
    })
}

/// A text event: all US-typeable, it is typed as key down/up pairs (Shift around shifted
/// characters, `\r\n` as one Return); otherwise it is pasted whole, one pasteboard write
/// per event, so a later paste never replaces text iOS has not read yet (spec §5).
fn plan_text(text: &str, plan: &mut Vec<HidAction>) {
    if !text.chars().all(|c| us_usage(c).is_some()) {
        plan.push(HidAction::Paste {
            text: text.to_owned(),
        });
        return;
    }
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        let Some((usage, shift)) = us_usage(c) else {
            continue;
        };
        if shift {
            plan.push(HidAction::Key {
                usage: SHIFT,
                down: true,
            });
        }
        plan.push(HidAction::Key { usage, down: true });
        plan.push(HidAction::Key { usage, down: false });
        if shift {
            plan.push(HidAction::Key {
                usage: SHIFT,
                down: false,
            });
        }
    }
}

/// What to send for one validated batch, in order. A batch with a key iOS lacks is refused
/// whole, before the pointer state changes. Touches from any pointer but the active one are
/// dropped, as is a second `down`; `cancel` lifts the pointer like `up`.
pub(crate) fn plan_events(
    events: &[DeviceInputEvent],
    orientation: Orientation,
    pointer: &mut PointerState,
) -> Result<Vec<HidAction>, DeviceOpError> {
    for event in events {
        if let DeviceInputEvent::Key { key, .. } = event {
            key_target(*key)?;
        }
    }
    let mut plan = Vec::with_capacity(events.len());
    for event in events {
        match event {
            DeviceInputEvent::Touch {
                action,
                pointer_id,
                x,
                y,
            } => {
                let phase = match (action, pointer.active) {
                    (TouchAction::Down, None) => {
                        pointer.active = Some(*pointer_id);
                        TouchPhase::Down
                    }
                    (TouchAction::Move, Some(active)) if active == *pointer_id => TouchPhase::Move,
                    (TouchAction::Up | TouchAction::Cancel, Some(active))
                        if active == *pointer_id =>
                    {
                        pointer.active = None;
                        TouchPhase::Up
                    }
                    _ => continue,
                };
                let (x, y) = to_portrait(*x, *y, orientation);
                plan.push(HidAction::Touch { phase, x, y });
            }
            DeviceInputEvent::Key { key, action } => {
                let down = *action == KeyAction::Down;
                plan.push(match key_target(*key)? {
                    KeyTarget::Button(button) => HidAction::Button { button, down },
                    KeyTarget::Usage(usage) => HidAction::Key { usage, down },
                });
            }
            DeviceInputEvent::Text { text } => plan_text(text, &mut plan),
        }
    }
    Ok(plan)
}

/// Cmd+V, sent after `simctl pbcopy`.
pub(crate) fn paste_keys() -> [HidAction; 4] {
    [
        HidAction::Key {
            usage: COMMAND,
            down: true,
        },
        HidAction::Key {
            usage: KEY_V,
            down: true,
        },
        HidAction::Key {
            usage: KEY_V,
            down: false,
        },
        HidAction::Key {
            usage: COMMAND,
            down: false,
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// DTUHID messages

/// An XPC value described in Rust, converted to `xpc_object_t` only when sent.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum XpcValue {
    U64(u64),
    Double(f64),
    Bool(bool),
    Str(&'static CStr),
    Dict(Vec<(&'static CStr, XpcValue)>),
}

fn envelope(message_type: &'static CStr, barrier: bool, payload: XpcValue) -> XpcValue {
    XpcValue::Dict(vec![
        (c"messageType", XpcValue::Str(message_type)),
        (c"isBarrier", XpcValue::Bool(barrier)),
        (c"featureIdentifier", XpcValue::Str(DIGITIZER_SERVICE)),
        (c"payload", payload),
    ])
}

/// HID `state`: 1 down, 2 up.
fn state(down: bool) -> XpcValue {
    XpcValue::U64(if down { 1 } else { 2 })
}

/// The DTUHID message for `action`; `None` for a paste, which the caller turns into keys.
pub(crate) fn dtu_message(action: &HidAction) -> Option<XpcValue> {
    Some(match action {
        HidAction::Touch { phase, x, y } => envelope(
            c"IndigoDigitizerEvent",
            false,
            XpcValue::Dict(vec![
                (
                    c"pointOne",
                    XpcValue::Dict(vec![
                        (c"x", XpcValue::Double(*x)),
                        (c"y", XpcValue::Double(*y)),
                    ]),
                ),
                (
                    c"eventType",
                    XpcValue::U64(match phase {
                        TouchPhase::Down => 0,
                        TouchPhase::Move => 1,
                        TouchPhase::Up => 2,
                    }),
                ),
                (c"edge", XpcValue::U64(0)),
                (c"target", XpcValue::U64(0)),
            ]),
        ),
        HidAction::Button { button, down } => envelope(
            c"IndigoButtonEvent",
            false,
            XpcValue::Dict(vec![
                (c"usagePage", XpcValue::U64(CONSUMER_PAGE)),
                (c"usageCode", XpcValue::U64(button.usage())),
                (c"state", state(*down)),
            ]),
        ),
        HidAction::Key { usage, down } => envelope(
            c"IndigoKeyboardButtonEvent",
            false,
            XpcValue::Dict(vec![
                (c"usageCode", XpcValue::U64(u64::from(*usage))),
                (c"state", state(*down)),
            ]),
        ),
        HidAction::Paste { .. } => return None,
    })
}

/// The barrier that proves `dtuhidd` is alive: a no-op key-up sent with a reply.
pub(crate) fn liveness_message() -> XpcValue {
    envelope(
        c"IndigoKeyboardButtonEvent",
        true,
        XpcValue::Dict(vec![
            (c"usageCode", XpcValue::U64(0)),
            (c"state", XpcValue::U64(2)),
        ]),
    )
}

// ---------------------------------------------------------------------------------------------
// Legacy Indigo messages

/// A legacy `SimDeviceLegacyHIDClient` message: touch and buttons only, since legacy
/// keyboard input is dropped on current CoreSimulator (spec §0).
#[derive(Debug, PartialEq)]
pub(crate) enum IndigoMessage {
    /// `IndigoHIDMessageForMouseNSEvent`, re-enveloped by [`single_touch`].
    Touch { x: f64, y: f64, down: bool },
    /// `IndigoHIDMessageForButton(source, direction, 0x33)`: home 0, lock 1.
    Button { source: i32, down: bool },
    /// `IndigoHIDMessageForHIDArbitrary(0x32, 0x0C, usage, direction)`.
    Consumer { usage: u32, down: bool },
}

/// The legacy message for `action`; `None` for keys and pastes.
pub(crate) fn indigo_message(action: &HidAction) -> Option<IndigoMessage> {
    Some(match action {
        // idb drags through Indigo as repeated touch-downs.
        HidAction::Touch { phase, x, y } => IndigoMessage::Touch {
            x: *x,
            y: *y,
            down: *phase != TouchPhase::Up,
        },
        HidAction::Button {
            button: SimButton::Home,
            down,
        } => IndigoMessage::Button {
            source: 0,
            down: *down,
        },
        HidAction::Button {
            button: SimButton::Lock,
            down,
        } => IndigoMessage::Button {
            source: 1,
            down: *down,
        },
        HidAction::Button { button, down } => IndigoMessage::Consumer {
            usage: u32::try_from(button.usage()).unwrap_or_default(),
            down: *down,
        },
        HidAction::Key { .. } | HidAction::Paste { .. } => return None,
    })
}

/// The size of a single-touch Indigo message.
pub(crate) const SINGLE_TOUCH_LEN: usize = 0x140;
const TOUCH_PAYLOAD: usize = 0x90;
const TOUCH_STRUCT: usize = 0x70;

/// Re-envelopes the multi-touch `builder` message as idb's 0x140-byte single-touch message,
/// with the ratio point and `timestamp` (`mach_absolute_time`). Spec §0, steps 1–5; every
/// offset is from the start of the message, little-endian. `builder` must be at least
/// 0xA0 bytes.
pub(crate) fn single_touch(builder: &[u8], x: f64, y: f64, timestamp: u64) -> Vec<u8> {
    let mut message = vec![0u8; SINGLE_TOUCH_LEN];
    message[0x18..0x1C].copy_from_slice(&(TOUCH_PAYLOAD as u32).to_le_bytes());
    message[0x1C] = 2;
    message[0x20..0x24].copy_from_slice(&0xBu32.to_le_bytes());
    message[0x24..0x2C].copy_from_slice(&timestamp.to_le_bytes());
    message[0x30..0x30 + TOUCH_STRUCT].copy_from_slice(&builder[0x30..0x30 + TOUCH_STRUCT]);
    message[0x3C..0x44].copy_from_slice(&x.to_le_bytes());
    message[0x44..0x4C].copy_from_slice(&y.to_le_bytes());
    message.copy_within(0x20..0x20 + TOUCH_PAYLOAD, 0x20 + TOUCH_PAYLOAD);
    message[0xC0..0xC4].copy_from_slice(&1u32.to_le_bytes());
    message[0xC4..0xC8].copy_from_slice(&2u32.to_le_bytes());
    message
}

// ---------------------------------------------------------------------------------------------
// Rotation

/// The Purple message's size; its buffer is 112 bytes for 8-byte alignment.
pub(crate) const PURPLE_MESSAGE_SIZE: u32 = 108;

/// The Purple GSEvent that sets the UI orientation to `value`, for `PurpleWorkspacePort`
/// (spec §0, S2), as 32-bit words: a `mach_msg_header_t` with a `COPY_SEND` remote right
/// to `port` and `msgh_id` 0x7B, GSEvent type `50 | 0x20000`, and a 4-byte record holding
/// `value`. Every other byte is zero.
pub(crate) fn purple_orientation_message(port: u32, value: u32) -> [u32; 28] {
    let mut words = [0u32; 28];
    let mut set = |offset: usize, value: u32| words[offset / 4] = value;
    set(0x00, 0x13); // msgh_bits: MACH_MSG_TYPE_COPY_SEND for the remote port
    set(0x04, PURPLE_MESSAGE_SIZE);
    set(0x08, port);
    set(0x14, 0x7B);
    set(0x18, 50 | 0x2_0000);
    set(0x48, 4);
    set(0x4C, value);
    words
}

// ---------------------------------------------------------------------------------------------
// Sending

/// Why a send failed.
#[derive(Debug, PartialEq)]
pub(crate) enum HidError {
    /// A key with no live DTUHID connection.
    KeyboardUnavailable,
    /// The device is stopping; nothing more is sent.
    Released,
    /// The simulator did not take it; the detail is for the log.
    Failed(String),
}

/// Sends input and rotations to one running simulator. `coresim::HidClient` is the real one;
/// tests record what they get.
pub(crate) trait HidSink: Send + Sync + 'static {
    /// Whether keys can be sent now. When they cannot, a new DTUHID handshake starts if none
    /// is running.
    fn keyboard_ready(&self) -> bool;
    /// Sends one action and waits until the simulator took it. Never a paste.
    fn send(&self, action: HidAction) -> BoxFuture<'_, Result<(), HidError>>;
    /// Sends the Purple orientation `value` (spec §0, S2).
    fn rotate(&self, value: u32) -> BoxFuture<'_, Result<(), HidError>>;
    /// Closes the connections; every later call returns [`HidError::Released`] without FFI.
    fn release(&self) -> BoxFuture<'_, ()>;
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;

    use super::*;
    use crate::device::{
        DeviceOpError,
        catalog::Orientation,
        input::{DeviceInputEvent, DeviceKey, KeyAction, TouchAction},
    };

    fn touch(action: TouchAction, id: u32, x: f64, y: f64) -> DeviceInputEvent {
        DeviceInputEvent::Touch {
            action,
            pointer_id: id,
            x,
            y,
        }
    }

    fn plan_one(event: DeviceInputEvent, orientation: Orientation) -> Vec<HidAction> {
        plan_events(&[event], orientation, &mut PointerState::default()).unwrap()
    }

    #[test]
    fn only_the_first_pointer_is_sent() {
        let mut pointer = PointerState::default();
        let plan = plan_events(
            &[
                touch(TouchAction::Down, 0, 0.5, 0.5),
                touch(TouchAction::Down, 1, 0.2, 0.2),
                touch(TouchAction::Move, 1, 0.3, 0.3),
                touch(TouchAction::Up, 0, 0.5, 0.6),
            ],
            Orientation::Portrait,
            &mut pointer,
        )
        .unwrap();
        assert_eq!(
            plan,
            vec![
                HidAction::Touch {
                    phase: TouchPhase::Down,
                    x: 0.5,
                    y: 0.5
                },
                HidAction::Touch {
                    phase: TouchPhase::Up,
                    x: 0.5,
                    y: 0.6
                },
            ]
        );
    }

    #[test]
    fn the_pointer_is_tracked_across_batches_and_cancel_is_up() {
        let mut pointer = PointerState::default();
        let down = plan_events(
            &[touch(TouchAction::Down, 7, 0.1, 0.2)],
            Orientation::Portrait,
            &mut pointer,
        )
        .unwrap();
        assert_eq!(down.len(), 1);
        let rest = plan_events(
            &[
                touch(TouchAction::Move, 7, 0.3, 0.4),
                touch(TouchAction::Cancel, 7, 0.3, 0.4),
                touch(TouchAction::Move, 7, 0.9, 0.9),
            ],
            Orientation::Portrait,
            &mut pointer,
        )
        .unwrap();
        assert_eq!(
            rest,
            vec![
                HidAction::Touch {
                    phase: TouchPhase::Move,
                    x: 0.3,
                    y: 0.4
                },
                HidAction::Touch {
                    phase: TouchPhase::Up,
                    x: 0.3,
                    y: 0.4
                },
            ]
        );
        // A new pointer may go down once the first is up.
        let again = plan_events(
            &[touch(TouchAction::Down, 2, 2.0, -1.0)],
            Orientation::Portrait,
            &mut pointer,
        )
        .unwrap();
        assert_eq!(
            again,
            vec![HidAction::Touch {
                phase: TouchPhase::Down,
                x: 1.0,
                y: 0.0
            }]
        );
    }

    /// Spec §0's table, per `uiOrientation`: `landscape` is 4, so (u, v) → (1 − v, u);
    /// `reverseLandscape` is 3, so (u, v) → (v, 1 − u); upside-down is (1 − u, 1 − v).
    #[test]
    fn landscape_ratios_map_to_portrait_space() {
        let at = |orientation| plan_one(touch(TouchAction::Down, 0, 0.25, 0.5), orientation);
        let down = |x, y| {
            vec![HidAction::Touch {
                phase: TouchPhase::Down,
                x,
                y,
            }]
        };
        assert_eq!(at(Orientation::Portrait), down(0.25, 0.5));
        assert_eq!(at(Orientation::Landscape), down(0.5, 0.25));
        assert_eq!(at(Orientation::ReverseLandscape), down(0.5, 0.75));
        assert_eq!(at(Orientation::ReversePortrait), down(0.75, 0.5));
    }

    #[test]
    fn mixed_text_is_pasted_whole() {
        let plan = plan_one(
            DeviceInputEvent::Text {
                text: "Hi é😀!".into(),
            },
            Orientation::Portrait,
        );
        assert_eq!(
            plan,
            [HidAction::Paste {
                text: "Hi é😀!".into()
            }]
        );
    }

    #[test]
    fn us_text_is_typed_as_keys() {
        let plan = plan_one(
            DeviceInputEvent::Text {
                text: "Hi !".into(),
            },
            Orientation::Portrait,
        );
        let key = |usage, down| HidAction::Key { usage, down };
        assert_eq!(
            plan,
            [
                key(0xE1, true),
                key(0x0B, true),
                key(0x0B, false),
                key(0xE1, false),
                key(0x0C, true),
                key(0x0C, false),
                key(0x2C, true),
                key(0x2C, false),
                key(0xE1, true),
                key(0x1E, true),
                key(0x1E, false),
                key(0xE1, false),
            ]
        );
    }

    #[test]
    fn crlf_is_one_return() {
        let plan = plan_one(
            DeviceInputEvent::Text {
                text: "a\r\nb".into(),
            },
            Orientation::Portrait,
        );
        let returns = plan
            .iter()
            .filter(|a| {
                **a == HidAction::Key {
                    usage: 0x28,
                    down: true,
                }
            })
            .count();
        assert_eq!(returns, 1);
    }

    #[test]
    fn keys_map_to_buttons_and_usages() {
        let key =
            |key, action| plan_one(DeviceInputEvent::Key { key, action }, Orientation::Portrait);
        assert_eq!(
            key(DeviceKey::Home, KeyAction::Down),
            [HidAction::Button {
                button: SimButton::Home,
                down: true
            }]
        );
        assert_eq!(
            key(DeviceKey::Power, KeyAction::Up),
            [HidAction::Button {
                button: SimButton::Lock,
                down: false
            }]
        );
        assert_eq!(
            key(DeviceKey::VolumeUp, KeyAction::Down),
            [HidAction::Button {
                button: SimButton::VolumeUp,
                down: true
            }]
        );
        assert_eq!(
            key(DeviceKey::VolumeDown, KeyAction::Down),
            [HidAction::Button {
                button: SimButton::VolumeDown,
                down: true
            }]
        );
        let usage = |k| match key(k, KeyAction::Down)[..] {
            [HidAction::Key { usage, down: true }] => usage,
            ref other => panic!("{other:?}"),
        };
        assert_eq!(usage(DeviceKey::Enter), 0x28);
        assert_eq!(usage(DeviceKey::Backspace), 0x2A);
        assert_eq!(usage(DeviceKey::Delete), 0x4C);
        assert_eq!(usage(DeviceKey::Tab), 0x2B);
        assert_eq!(usage(DeviceKey::ArrowRight), 0x4F);
        assert_eq!(usage(DeviceKey::ArrowLeft), 0x50);
        assert_eq!(usage(DeviceKey::ArrowDown), 0x51);
        assert_eq!(usage(DeviceKey::ArrowUp), 0x52);
    }

    #[test]
    fn android_only_keys_are_refused() {
        let mut pointer = PointerState::default();
        let back = DeviceInputEvent::Key {
            key: DeviceKey::Back,
            action: KeyAction::Down,
        };
        assert!(matches!(
            plan_events(&[back], Orientation::Portrait, &mut pointer),
            Err(DeviceOpError::InvalidInput(m)) if m == "iOS has no Back key."
        ));
        // A refused batch changes no pointer state.
        let batch = [
            touch(TouchAction::Down, 0, 0.5, 0.5),
            DeviceInputEvent::Key {
                key: DeviceKey::AppSwitch,
                action: KeyAction::Up,
            },
        ];
        assert!(matches!(
            plan_events(&batch, Orientation::Portrait, &mut pointer),
            Err(DeviceOpError::InvalidInput(m)) if m == "iOS has no Recents key."
        ));
        let down = plan_events(
            &[touch(TouchAction::Down, 3, 0.5, 0.5)],
            Orientation::Portrait,
            &mut pointer,
        )
        .unwrap();
        assert_eq!(down.len(), 1);
    }

    #[test]
    fn paste_ends_with_command_v() {
        assert_eq!(
            paste_keys(),
            [
                HidAction::Key {
                    usage: 0xE3,
                    down: true
                },
                HidAction::Key {
                    usage: 0x19,
                    down: true
                },
                HidAction::Key {
                    usage: 0x19,
                    down: false
                },
                HidAction::Key {
                    usage: 0xE3,
                    down: false
                },
            ]
        );
    }

    #[test]
    fn debug_output_never_contains_pasted_text() {
        let printed = format!(
            "{:?}",
            HidAction::Paste {
                text: "hunter2".into()
            }
        );
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    fn entry<'a>(value: &'a XpcValue, key: &CStr) -> &'a XpcValue {
        match value {
            XpcValue::Dict(entries) => entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value)
                .unwrap_or_else(|| panic!("no {key:?} in {value:?}")),
            other => panic!("not a dictionary: {other:?}"),
        }
    }

    fn keys(value: &XpcValue) -> Vec<&CStr> {
        match value {
            XpcValue::Dict(entries) => entries.iter().map(|(name, _)| *name).collect(),
            other => panic!("not a dictionary: {other:?}"),
        }
    }

    fn assert_envelope(message: &XpcValue, message_type: &'static CStr, barrier: bool) {
        assert_eq!(
            keys(message),
            [
                c"messageType",
                c"isBarrier",
                c"featureIdentifier",
                c"payload"
            ]
        );
        assert_eq!(entry(message, c"messageType"), &XpcValue::Str(message_type));
        assert_eq!(entry(message, c"isBarrier"), &XpcValue::Bool(barrier));
        assert_eq!(
            entry(message, c"featureIdentifier"),
            &XpcValue::Str(c"com.apple.coredevice.feature.remote.hid.digitizer")
        );
    }

    #[test]
    fn dtuhid_messages_carry_the_spike_payloads() {
        let touch = dtu_message(&HidAction::Touch {
            phase: TouchPhase::Move,
            x: 0.25,
            y: 0.75,
        })
        .unwrap();
        assert_envelope(&touch, c"IndigoDigitizerEvent", false);
        let payload = entry(&touch, c"payload");
        assert_eq!(
            keys(payload),
            [c"pointOne", c"eventType", c"edge", c"target"]
        );
        assert_eq!(
            entry(payload, c"pointOne"),
            &XpcValue::Dict(vec![
                (c"x", XpcValue::Double(0.25)),
                (c"y", XpcValue::Double(0.75))
            ])
        );
        assert_eq!(entry(payload, c"eventType"), &XpcValue::U64(1));
        assert_eq!(entry(payload, c"edge"), &XpcValue::U64(0));
        assert_eq!(entry(payload, c"target"), &XpcValue::U64(0));
        let phase = |phase| {
            let message = dtu_message(&HidAction::Touch {
                phase,
                x: 0.0,
                y: 0.0,
            })
            .unwrap();
            entry(entry(&message, c"payload"), c"eventType").clone()
        };
        assert_eq!(phase(TouchPhase::Down), XpcValue::U64(0));
        assert_eq!(phase(TouchPhase::Up), XpcValue::U64(2));

        let button = dtu_message(&HidAction::Button {
            button: SimButton::VolumeDown,
            down: false,
        })
        .unwrap();
        assert_envelope(&button, c"IndigoButtonEvent", false);
        assert_eq!(
            entry(&button, c"payload"),
            &XpcValue::Dict(vec![
                (c"usagePage", XpcValue::U64(0x0C)),
                (c"usageCode", XpcValue::U64(0xEA)),
                (c"state", XpcValue::U64(2)),
            ])
        );
        let usage = |button| {
            let message = dtu_message(&HidAction::Button { button, down: true }).unwrap();
            entry(entry(&message, c"payload"), c"usageCode").clone()
        };
        assert_eq!(usage(SimButton::Home), XpcValue::U64(0x40));
        assert_eq!(usage(SimButton::Lock), XpcValue::U64(0x30));
        assert_eq!(usage(SimButton::VolumeUp), XpcValue::U64(0xE9));

        let key = dtu_message(&HidAction::Key {
            usage: 0x04,
            down: true,
        })
        .unwrap();
        assert_envelope(&key, c"IndigoKeyboardButtonEvent", false);
        assert_eq!(
            entry(&key, c"payload"),
            &XpcValue::Dict(vec![
                (c"usageCode", XpcValue::U64(0x04)),
                (c"state", XpcValue::U64(1))
            ])
        );
        assert_eq!(dtu_message(&HidAction::Paste { text: "x".into() }), None);

        let liveness = liveness_message();
        assert_envelope(&liveness, c"IndigoKeyboardButtonEvent", true);
        assert_eq!(
            entry(&liveness, c"payload"),
            &XpcValue::Dict(vec![
                (c"usageCode", XpcValue::U64(0)),
                (c"state", XpcValue::U64(2))
            ])
        );
    }

    #[test]
    fn legacy_messages_cover_touch_and_buttons_only() {
        assert_eq!(
            indigo_message(&HidAction::Touch {
                phase: TouchPhase::Down,
                x: 0.1,
                y: 0.2
            }),
            Some(IndigoMessage::Touch {
                x: 0.1,
                y: 0.2,
                down: true
            })
        );
        // idb drags through Indigo as repeated downs.
        assert_eq!(
            indigo_message(&HidAction::Touch {
                phase: TouchPhase::Move,
                x: 0.3,
                y: 0.2
            }),
            Some(IndigoMessage::Touch {
                x: 0.3,
                y: 0.2,
                down: true
            })
        );
        assert_eq!(
            indigo_message(&HidAction::Touch {
                phase: TouchPhase::Up,
                x: 0.3,
                y: 0.2
            }),
            Some(IndigoMessage::Touch {
                x: 0.3,
                y: 0.2,
                down: false
            })
        );
        assert_eq!(
            indigo_message(&HidAction::Button {
                button: SimButton::Home,
                down: true
            }),
            Some(IndigoMessage::Button {
                source: 0,
                down: true
            })
        );
        assert_eq!(
            indigo_message(&HidAction::Button {
                button: SimButton::Lock,
                down: false
            }),
            Some(IndigoMessage::Button {
                source: 1,
                down: false
            })
        );
        assert_eq!(
            indigo_message(&HidAction::Button {
                button: SimButton::VolumeUp,
                down: true
            }),
            Some(IndigoMessage::Consumer {
                usage: 0xE9,
                down: true
            })
        );
        assert_eq!(
            indigo_message(&HidAction::Key {
                usage: 4,
                down: true
            }),
            None
        );
    }

    #[test]
    fn the_purple_message_matches_the_spike_layout() {
        let words = purple_orientation_message(0x5227, 3);
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        assert_eq!(bytes.len(), 112);
        let at = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        assert_eq!(at(0x00), 0x13);
        assert_eq!(at(0x04), 108);
        assert_eq!(at(0x08), 0x5227);
        assert_eq!(at(0x0C), 0, "no local port");
        assert_eq!(at(0x10), 0, "no voucher");
        assert_eq!(at(0x14), 0x7B);
        assert_eq!(at(0x18), 0x2_0032);
        assert_eq!(at(0x48), 4);
        assert_eq!(at(0x4C), 3);
        let set = [0x00, 0x04, 0x08, 0x14, 0x18, 0x48, 0x4C];
        for offset in (0..112).step_by(4).filter(|offset| !set.contains(offset)) {
            assert_eq!(at(offset), 0, "offset {offset:#x}");
        }
    }

    #[test]
    fn a_legacy_touch_is_re_enveloped_as_a_single_touch() {
        // The builder's 384-byte message, with recognisable touch bytes at 0x30..0xA0.
        let mut builder = vec![0u8; 0x180];
        for (offset, byte) in builder.iter_mut().enumerate().skip(0x30).take(0x70) {
            *byte = offset as u8;
        }
        let message = single_touch(&builder, 0.25, 0.75, 0x1122_3344_5566_7788);
        assert_eq!(message.len(), 0x140);
        assert!(
            message[..0x18].iter().all(|&b| b == 0),
            "the mach header stays zero"
        );
        assert_eq!(
            u32::from_le_bytes(message[0x18..0x1C].try_into().unwrap()),
            0x90
        );
        assert_eq!(message[0x1C], 2);
        assert_eq!(&message[0x1D..0x20], &[0, 0, 0]);
        assert_eq!(
            u32::from_le_bytes(message[0x20..0x24].try_into().unwrap()),
            0xB
        );
        assert_eq!(
            u64::from_le_bytes(message[0x24..0x2C].try_into().unwrap()),
            0x1122_3344_5566_7788
        );
        assert_eq!(&message[0x2C..0x30], &[0, 0, 0, 0]);
        assert_eq!(
            f64::from_le_bytes(message[0x3C..0x44].try_into().unwrap()),
            0.25
        );
        assert_eq!(
            f64::from_le_bytes(message[0x44..0x4C].try_into().unwrap()),
            0.75
        );
        assert_eq!(&message[0x30..0x3C], &builder[0x30..0x3C]);
        assert_eq!(&message[0x4C..0xA0], &builder[0x4C..0xA0]);
        assert!(message[0xA0..0xB0].iter().all(|&b| b == 0));
        // The payload is repeated at 0xB0 with field1 = 1 and field2 = 2.
        assert_eq!(&message[0xB0..0xC0], &message[0x20..0x30]);
        assert_eq!(
            u32::from_le_bytes(message[0xC0..0xC4].try_into().unwrap()),
            1
        );
        assert_eq!(
            u32::from_le_bytes(message[0xC4..0xC8].try_into().unwrap()),
            2
        );
        assert_eq!(&message[0xC8..0x140], &message[0x38..0xB0]);
    }
}
