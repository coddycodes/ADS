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
    orientation::{OrientationTracker, is_landscape},
};
use crate::device::{
    DeviceError, DeviceOpError,
    catalog::Orientation,
    input::{DeviceInputEvent, DeviceKey, KeyAction, TouchAction, is_us_typeable},
};

/// How long one event may wait for room in the emulator's input stream.
const INPUT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// The most characters one `KeyboardEvent.text` send carries. The emulator types a `text`
/// as one burst of key events, each followed by a sync, and the guest's keyboard buffer
/// holds 64 events: on a live Pixel_8 AVD (emulator 37.2.12.0) ` PASTED-ok`, 32 key events
/// with its Shift presses, was dropped whole (`SYN_DROPPED` in the guest, no error), while
/// 22 to 24 key events arrived. One character is at most four key events (Shift down, the
/// key down and up, Shift up).
const TEXT_CHARS_PER_SEND: usize = 1;

/// The pause between two text sends of one batch. Sends made back to back reach the guest
/// as one burst as well: ten one-character sends of ` PASTED-ok` without a pause were
/// dropped whole, like one `text`. With a pause the guest dropped none, over 1,000
/// characters: 1 ms (about 2.9 ms between sends, as the timer rounds up) and 5 ms (about
/// 7.5 ms) both measured clean. 5 ms leaves the guest's reader seven sends, over 50 ms, to
/// fall behind before its buffer fills.
const TEXT_SEND_INTERVAL: Duration = Duration::from_millis(5);

/// The most text characters one batch may carry. Android drops a key queued for more than
/// 10 s ("Dropped event because it is stale"), and the focused app takes keys one at a time:
/// on the live Pixel_8, Chrome's address bar (with Gboard) took roughly 7 to 12 characters a
/// second, so 300 pasted characters lost their tail whatever the pause (104 arrived at 1 ms,
/// 265 at 50 ms), and a lost Shift release garbled the characters after it. 100 is the
/// measured safe bound in that field, the slowest measured: 100 arrived exactly, twice.
/// Typing is never refused: a batch holds at most 64 events, and typed text is one
/// character per event.
pub(crate) const MAX_ANDROID_TEXT_PER_BATCH: usize = 100;

/// Why a batch with more than [`MAX_ANDROID_TEXT_PER_BATCH`] text characters is refused.
const TOO_MUCH_TEXT: &str =
    "This device can paste up to 100 characters at a time. Paste longer text in parts.";

/// How long [`rotate`] waits for the guest to turn, and how often it reads the orientation
/// back meanwhile. Tests shorten it, so a refusal does not take the whole window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RotationTiming {
    /// How long the guest may take to turn. On a real Pixel_8 AVD the scrcpy display changed
    /// 0.67-0.90 s after `setPhysicalModel`.
    pub(crate) window: Duration,
    /// The pause between reads; the last read starts by `window - poll`.
    pub(crate) poll: Duration,
}

impl Default for RotationTiming {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(2_500),
            poll: Duration::from_millis(250),
        }
    }
}

/// Why a batch with text the emulator would drop is refused. It names no character, and the
/// device is not named: the running device's display name belongs to the manager, not to
/// the backend.
const UNTYPEABLE_TEXT: &str = "This device can type only plain ASCII text. Accented letters, emoji, and other characters can't be sent from here yet.";

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
/// `orientation` (native verification item 2). A positive z turns it counter-clockwise:
/// `-90` gave `ROTATION_270` on a real Pixel_8 AVD, so `Landscape` (`ROTATION_90`) is `+90`.
pub(crate) fn rotation_vector(orientation: Orientation) -> [f32; 3] {
    match orientation {
        Orientation::Portrait => [0.0, 0.0, 0.0],
        Orientation::Landscape => [0.0, 0.0, 90.0],
        Orientation::ReversePortrait => [0.0, 0.0, 180.0],
        Orientation::ReverseLandscape => [0.0, 0.0, -90.0],
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

/// Natural-panel pixels for a point `(u, v)` normalized to `0..1` in the upright picture of
/// a device turned to `orientation`, whose panel in its natural orientation (`Portrait`,
/// whatever the panel's shape) is `width` x `height`. The
/// emulator's `Touch.x`/`y` is "the physical location on the screen", the unrotated panel,
/// whatever the guest's rotation (native verification item 3): a tap at (109, 66) of a
/// 2400x1080 landscape picture arrived as panel (109, 66) before this mapping.
pub(crate) fn to_panel_pixels(
    u: f64,
    v: f64,
    width: u32,
    height: u32,
    orientation: Orientation,
) -> (i32, i32) {
    // Saturating float-to-int casts; the clamps keep the point on the panel.
    let forward =
        |t: f64, n: u32| ((t.clamp(0.0, 1.0) * f64::from(n)) as u32).min(n.saturating_sub(1));
    let reversed = |t: f64, n: u32| n.saturating_sub(1) - forward(t, n);
    let (x, y) = match orientation {
        Orientation::Portrait => (forward(u, width), forward(v, height)),
        // Turned counter-clockwise: the picture's top is the panel's right edge.
        Orientation::Landscape => (reversed(v, width), forward(u, height)),
        Orientation::ReversePortrait => (reversed(u, width), reversed(v, height)),
        // Turned clockwise: the picture's top is the panel's left edge.
        Orientation::ReverseLandscape => (forward(v, width), reversed(u, height)),
    };
    let pixel = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    (pixel(x), pixel(y))
}

/// A device's long-lived input stream and the pointers it holds down.
pub(crate) struct InputState {
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
/// display size and `orientation` the catalog's current one, the orientation the guest
/// last reported. Pointers are kept in panel pixels, so a release after a turn still lands
/// where the pointer went down.
///
/// A batch with text outside [`is_us_typeable`] is refused whole, before anything is sent.
/// The emulator's proto promises only printable ASCII in `KeyboardEvent.text` and warns that
/// "most" other symbols are ignored, yet the call still succeeds: refusing means no
/// character is dropped while the call reports success, and the device sees none of the
/// batch, so the client's own release of a press in it goes out again.
///
/// A batch with more than [`MAX_ANDROID_TEXT_PER_BATCH`] text characters is refused the same
/// way, after the ASCII check: the guest would drop the tail of the paste without a word.
///
/// Text that passes is typed in sends of at most [`TEXT_CHARS_PER_SEND`] characters, in
/// order and [`TEXT_SEND_INTERVAL`] apart, so the guest never sees a burst it drops.
pub(super) async fn send_batch(
    client: &EmulatorClient,
    tasks: &TaskTracker,
    input: &tokio::sync::Mutex<Option<InputState>>,
    events: Vec<DeviceInputEvent>,
    (width, height): (u32, u32),
    orientation: Orientation,
) -> Result<(), DeviceOpError> {
    if events
        .iter()
        .any(|event| matches!(event, DeviceInputEvent::Text { text } if !is_us_typeable(text)))
    {
        return Err(DeviceOpError::InvalidInput(UNTYPEABLE_TEXT.to_owned()));
    }
    let text_chars: usize = events
        .iter()
        .map(|event| match event {
            DeviceInputEvent::Text { text } => text.chars().count(),
            DeviceInputEvent::Touch { .. } | DeviceInputEvent::Key { .. } => 0,
        })
        .sum();
    if text_chars > MAX_ANDROID_TEXT_PER_BATCH {
        return Err(DeviceOpError::InvalidInput(TOO_MUCH_TEXT.to_owned()));
    }
    // Held for the whole batch, so concurrent batches never interleave.
    let mut state = input.lock().await;
    // Whether a text send of this batch went out, so the next one waits its turn.
    let mut typed = false;
    for event in events {
        let (message, pointer) = match event {
            DeviceInputEvent::Touch {
                action,
                pointer_id,
                x,
                y,
            } => {
                let (x, y) = to_panel_pixels(x, y, width, height, orientation);
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
            DeviceInputEvent::Text { text } => {
                send_text(client, tasks, &mut state, &text, &mut typed).await?;
                continue;
            }
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

/// Types `text` as `KeyboardEvent.text` sends of at most [`TEXT_CHARS_PER_SEND`]
/// characters, in order, each [`TEXT_SEND_INTERVAL`] after the batch's previous text send
/// (`typed` says whether there was one). The caller holds the input lock throughout, so no
/// other event lands between them.
async fn send_text(
    client: &EmulatorClient,
    tasks: &TaskTracker,
    state: &mut Option<InputState>,
    text: &str,
    typed: &mut bool,
) -> Result<(), DeviceOpError> {
    let mut sends = 0_usize;
    for chunk in text_chunks(text) {
        if *typed {
            tokio::time::sleep(TEXT_SEND_INTERVAL).await;
        }
        let event = key_event(proto::KeyboardEvent {
            text: chunk,
            ..Default::default()
        });
        send(client, tasks, state, event)
            .await
            .map_err(|error| emulator_failure("input", &error))?;
        *typed = true;
        sends += 1;
    }
    // Counts only: the text itself is never logged.
    tracing::debug!(
        chars = text.chars().count(),
        sends,
        "typed text on the device"
    );
    Ok(())
}

/// `text` in pieces of at most [`TEXT_CHARS_PER_SEND`] characters, in order.
fn text_chunks(text: &str) -> impl Iterator<Item = String> + '_ {
    let mut chars = text.chars();
    std::iter::from_fn(move || {
        let chunk: String = chars.by_ref().take(TEXT_CHARS_PER_SEND).collect();
        (!chunk.is_empty()).then_some(chunk)
    })
}

/// Asks the emulator to turn toward `to`, then follows what the guest shows in `tracker`,
/// and returns the orientation the device ended in. It writes nothing to the catalog: the
/// tracker's orientation reaches it through `orientation_changes` and, while still images
/// run, through each published frame. `sensor` holds the orientation whose vector the
/// sensor was last sent; it stays locked for the whole call, so one device's rotates run
/// one at a time and an older one never undoes a newer one.
///
/// The skin follows the sensor even when the screen refuses the turn (the launcher refuses
/// landscape, phones refuse 180 degrees). A turn the derived orientation has not taken
/// within the window counts as refused only on positive evidence: a read that succeeded
/// (it disagreed, or the call would have returned), or a scrcpy display in the other class
/// than `to`. The sensor then goes back to the vector it held before the press, so it
/// matches the screen and a later app does not turn unexpectedly. A failed read is no such
/// evidence: the poll falls back to the tracker's orientation, which follows the scrcpy
/// display. With no evidence at all the turn is taken as asked: the sensor stays where it
/// was sent, the tracker records the skin at `to`, and `to` is returned. A guest slower
/// than the window has its turn undone, and the user presses again.
pub(super) async fn rotate(
    client: &EmulatorClient,
    tracker: &OrientationTracker,
    sensor: &tokio::sync::Mutex<Orientation>,
    timing: RotationTiming,
    to: Orientation,
) -> Result<Orientation, DeviceOpError> {
    let mut sensor = sensor.lock().await;
    let before = *sensor;
    client
        .set_rotation(rotation_vector(to))
        .await
        .map_err(|error| emulator_failure("rotate", &error))?;
    *sensor = to;
    let deadline = tokio::time::Instant::now() + timing.window;
    let mut read = false;
    loop {
        let skin = read_skin(client).await;
        let derived = match skin {
            Some(skin) => {
                read = true;
                tracker.report_skin(skin)
            }
            None => tracker.current(),
        };
        tracing::debug!(
            ?skin,
            ?derived,
            requested = ?to,
            "device reported its rotation"
        );
        if derived == to {
            return Ok(to);
        }
        if tokio::time::Instant::now() + timing.poll > deadline {
            break;
        }
        tokio::time::sleep(timing.poll).await;
    }
    let refused = read
        || tracker
            .display_turned()
            .is_some_and(|turned| turned != is_landscape(to));
    if !refused {
        tracing::debug!(
            requested = ?to,
            "the device's orientation could not be read; the turn is taken as asked"
        );
        // No read succeeded, and no display contradicts `to`. The sensor stays at `to` and
        // the skin follows the sensor, as a successful read would have shown, so the
        // tracker is told and the result, the catalog, and the touch mapping agree.
        return Ok(tracker.report_skin(to));
    }
    tracing::debug!(
        requested = ?to,
        ?before,
        "the device did not turn; the sensor goes back"
    );
    client
        .set_rotation(rotation_vector(before))
        .await
        .map_err(|error| emulator_failure("rotate back", &error))?;
    *sensor = before;
    // The skin follows the sensor, so it is back at `before` too.
    Ok(tracker.report_skin(before))
}

/// The skin rotation a PNG screenshot reports; `None` when the read fails or has none.
async fn read_skin(client: &EmulatorClient) -> Option<Orientation> {
    match client.screenshot(ImgFormat::Png, 1).await {
        Ok(image) => image
            .format
            .and_then(|format| format.rotation)
            .and_then(|rotation| reported_orientation(rotation.rotation)),
        Err(error) => {
            tracing::debug!(%error, "reading the device's orientation failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys() {
        assert_eq!(w3c_key(DeviceKey::Back), "GoBack");
        assert_eq!(w3c_key(DeviceKey::Home), "GoHome");
        assert_eq!(w3c_key(DeviceKey::VolumeDown), "AudioVolumeDown");
    }

    /// The first native run sent `-90` for `Landscape` and the guest reported `ROTATION_270`
    /// with the cutout on the picture's right (`android-native/13-rotation.txt`): turned
    /// clockwise. `Landscape` is counter-clockwise, so it is `+90`.
    #[test]
    fn landscape_tilts_counter_clockwise() {
        assert_eq!(rotation_vector(Orientation::Portrait), [0.0, 0.0, 0.0]);
        assert_eq!(rotation_vector(Orientation::Landscape), [0.0, 0.0, 90.0]);
        assert_eq!(
            rotation_vector(Orientation::ReversePortrait),
            [0.0, 0.0, 180.0]
        );
        assert_eq!(
            rotation_vector(Orientation::ReverseLandscape),
            [0.0, 0.0, -90.0]
        );
    }

    /// The live readback (`android-rotation/01-rotate-left.txt` and
    /// `03-reverse-landscape.txt`): `+90` gave `ROTATION_90` and skin 1 `LANDSCAPE`, `-90`
    /// gave `ROTATION_270` and skin 3 `REVERSE_LANDSCAPE`, so the skin maps name for name.
    #[test]
    fn reported_rotations_map_name_for_name() {
        for (skin, orientation) in [
            (SkinRotation::Portrait, Orientation::Portrait),
            (SkinRotation::Landscape, Orientation::Landscape),
            (SkinRotation::ReversePortrait, Orientation::ReversePortrait),
            (
                SkinRotation::ReverseLandscape,
                Orientation::ReverseLandscape,
            ),
        ] {
            assert_eq!(
                reported_orientation(skin as i32),
                Some(orientation),
                "{skin:?}"
            );
        }
        assert_eq!(reported_orientation(42), None);
    }

    const PANEL: (u32, u32) = (1080, 2400);

    fn panel(u: f64, v: f64, orientation: Orientation) -> (i32, i32) {
        to_panel_pixels(u, v, PANEL.0, PANEL.1, orientation)
    }

    #[test]
    fn upright_points_map_to_the_unrotated_panel() {
        // (u, v) = (0.25, 0.75) of the upright picture, each orientation's row of the table.
        assert_eq!(panel(0.25, 0.75, Orientation::Portrait), (270, 1800));
        assert_eq!(panel(0.25, 0.75, Orientation::Landscape), (1079 - 810, 600));
        assert_eq!(
            panel(0.25, 0.75, Orientation::ReversePortrait),
            (1079 - 270, 2399 - 1800)
        );
        assert_eq!(
            panel(0.25, 0.75, Orientation::ReverseLandscape),
            (810, 2399 - 600)
        );
    }

    /// The native run's measured tap: (109, 66) of the 2400x1080 landscape picture.
    #[test]
    fn the_measured_landscape_tap_lands_on_the_panel_where_the_picture_shows_it() {
        let (u, v) = (109.0 / 2400.0, 66.0 / 1080.0);
        assert_eq!(panel(u, v, Orientation::Landscape), (1013, 109));
    }

    #[test]
    fn each_orientation_maps_its_corners_to_the_right_panel_corners() {
        let (right, bottom) = (1079, 2399);
        // The picture's top-left, top-right, bottom-left, and bottom-right corners.
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        for (orientation, expected) in [
            (
                Orientation::Portrait,
                [(0, 0), (right, 0), (0, bottom), (right, bottom)],
            ),
            // Counter-clockwise: the picture's top-left is the panel's top-right.
            (
                Orientation::Landscape,
                [(right, 0), (right, bottom), (0, 0), (0, bottom)],
            ),
            (
                Orientation::ReversePortrait,
                [(right, bottom), (0, bottom), (right, 0), (0, 0)],
            ),
            // Clockwise: the picture's top-left is the panel's bottom-left.
            (
                Orientation::ReverseLandscape,
                [(0, bottom), (0, 0), (right, bottom), (right, 0)],
            ),
        ] {
            let mapped = corners.map(|(u, v)| panel(u, v, orientation));
            assert_eq!(mapped, expected, "{orientation:?}");
        }
    }

    /// `Portrait` is the natural orientation, so a landscape-native panel maps unturned.
    #[test]
    fn a_landscape_native_panel_at_rest_maps_touches_unchanged() {
        let tablet = |u, v| to_panel_pixels(u, v, 2560, 1600, Orientation::Portrait);
        assert_eq!(tablet(0.0, 0.0), (0, 0));
        assert_eq!(tablet(0.25, 0.75), (640, 1200));
        assert_eq!(tablet(1.0, 1.0), (2559, 1599));
    }

    #[test]
    fn points_off_the_picture_are_clamped_to_the_panel() {
        assert_eq!(panel(1.2, -0.1, Orientation::Portrait), (1079, 0));
        assert_eq!(panel(1.2, -0.1, Orientation::Landscape), (1079, 2399));
        assert_eq!(panel(1.2, -0.1, Orientation::ReversePortrait), (0, 2399));
        assert_eq!(panel(1.2, -0.1, Orientation::ReverseLandscape), (0, 0));
    }

    use std::sync::Arc;

    use crate::device::android::{
        orientation::DisplayHandle,
        testing::{FAST_ROTATION, FakeController, FakeHandle, wait_until},
    };

    /// Long enough for any fake turn below, which ends the call early.
    const ROOMY_ROTATION: RotationTiming = RotationTiming {
        window: Duration::from_secs(5),
        poll: Duration::from_millis(20),
    };

    /// A fake emulator, a client to it, a tracker for a phone panel, and a sensor at rest.
    async fn emulator() -> (
        FakeHandle,
        EmulatorClient,
        Arc<OrientationTracker>,
        tokio::sync::Mutex<Orientation>,
    ) {
        let fake = FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();
        let tracker = OrientationTracker::new(PANEL, Orientation::Portrait);
        (
            fake,
            client,
            tracker,
            tokio::sync::Mutex::new(Orientation::Portrait),
        )
    }

    /// A running scrcpy session in `tracker` whose display is `width` x `height`.
    fn display(tracker: &Arc<OrientationTracker>, width: u32, height: u32) -> DisplayHandle {
        let display = tracker.start_display();
        display.set(width, height);
        display
    }

    /// Turns the scrcpy display wide after `after`, as the guest does when it turns.
    async fn turn_wide_after(display: &DisplayHandle, after: Duration) {
        tokio::time::sleep(after).await;
        display.set(1280, 576);
    }

    /// The launcher's refusal: the skin follows the sensor to landscape while the guest's
    /// display stays tall, so the turn is undone and portrait is the result.
    #[tokio::test]
    async fn a_turn_the_display_never_takes_puts_the_sensor_back() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.skin_follows_sensor();
        let _display = display(&tracker, 576, 1280);
        let ended = rotate(
            &client,
            &tracker,
            &sensor,
            FAST_ROTATION,
            Orientation::Landscape,
        )
        .await
        .unwrap();
        assert_eq!(ended, Orientation::Portrait);
        assert_eq!(
            *fake.rotations.lock().unwrap(),
            [[0.0, 0.0, 90.0], [0.0, 0.0, 0.0]]
        );
        assert_eq!(tracker.current(), Orientation::Portrait);
        assert_eq!(*sensor.lock().await, Orientation::Portrait);
    }

    /// Chrome's turn: the display becomes wide within the window, so the turn stands.
    #[tokio::test]
    async fn a_turn_the_display_takes_within_the_window_stands() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.skin_follows_sensor();
        let display = display(&tracker, 576, 1280);
        let (ended, ()) = tokio::join!(
            rotate(
                &client,
                &tracker,
                &sensor,
                ROOMY_ROTATION,
                Orientation::Landscape
            ),
            turn_wide_after(&display, Duration::from_millis(50)),
        );
        assert_eq!(ended.unwrap(), Orientation::Landscape);
        assert_eq!(*fake.rotations.lock().unwrap(), [[0.0, 0.0, 90.0]]);
        assert_eq!(tracker.current(), Orientation::Landscape);
    }

    /// Review I2: every read fails on a degraded emulator, but the video shows the guest
    /// turn, so the tracker's orientation decides and the turn is not undone.
    #[tokio::test]
    async fn a_turn_whose_reads_fail_still_follows_the_display() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.fail_screenshots();
        let display = display(&tracker, 576, 1280);
        let (ended, ()) = tokio::join!(
            rotate(
                &client,
                &tracker,
                &sensor,
                ROOMY_ROTATION,
                Orientation::Landscape
            ),
            turn_wide_after(&display, Duration::from_millis(50)),
        );
        assert_eq!(ended.unwrap(), Orientation::Landscape);
        assert_eq!(*fake.rotations.lock().unwrap(), [[0.0, 0.0, 90.0]]);
    }

    /// With every read failing and no video, nothing shows whether the guest turned, so the
    /// sensor is left where it was sent and the turn is taken as asked: the result and the
    /// tracker (and with it the catalog and the touch mapping) agree on it.
    #[tokio::test]
    async fn without_any_evidence_the_turn_is_taken_as_asked() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.fail_screenshots();
        let ended = rotate(
            &client,
            &tracker,
            &sensor,
            FAST_ROTATION,
            Orientation::Landscape,
        )
        .await
        .unwrap();
        assert_eq!(ended, Orientation::Landscape);
        assert_eq!(tracker.current(), Orientation::Landscape);
        assert_eq!(*fake.rotations.lock().unwrap(), [[0.0, 0.0, 90.0]]);
        assert_eq!(*sensor.lock().await, Orientation::Landscape);
    }

    /// Review round 2, Important 1, with video: every read fails and the guest turns wide
    /// for Rotate right. The tracker still holds the skin from before the press, which
    /// would fall back to `Landscape` by the display's shape; the turn is taken as asked,
    /// so the tracker ends at `ReverseLandscape` and the touches are not mirrored.
    #[tokio::test]
    async fn a_turn_whose_reads_fail_ends_at_the_orientation_asked_for() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.fail_screenshots();
        let display = display(&tracker, 576, 1280);
        let (ended, ()) = tokio::join!(
            rotate(
                &client,
                &tracker,
                &sensor,
                FAST_ROTATION,
                Orientation::ReverseLandscape
            ),
            turn_wide_after(&display, Duration::from_millis(50)),
        );
        assert_eq!(ended.unwrap(), Orientation::ReverseLandscape);
        assert_eq!(tracker.current(), Orientation::ReverseLandscape);
        assert_eq!(*fake.rotations.lock().unwrap(), [[0.0, 0.0, -90.0]]);
        assert_eq!(*sensor.lock().await, Orientation::ReverseLandscape);
    }

    /// With every read failing, a video display that stays tall is the evidence of refusal.
    #[tokio::test]
    async fn a_display_that_keeps_its_class_is_evidence_of_refusal() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.fail_screenshots();
        let _display = display(&tracker, 576, 1280);
        let ended = rotate(
            &client,
            &tracker,
            &sensor,
            FAST_ROTATION,
            Orientation::Landscape,
        )
        .await
        .unwrap();
        assert_eq!(ended, Orientation::Portrait);
        assert_eq!(
            *fake.rotations.lock().unwrap(),
            [[0.0, 0.0, 90.0], [0.0, 0.0, 0.0]]
        );
    }

    /// Review M3: an app forced landscape with the sensor at rest, so the tracker shows
    /// landscape. A refused 180-degree turn restores the vector the sensor held (`0`), not
    /// the vector of the orientation shown (`+90`).
    #[tokio::test]
    async fn a_refusal_restores_the_vector_the_sensor_held() {
        let (fake, client, tracker, sensor) = emulator().await;
        let _display = display(&tracker, 1280, 576);
        assert_eq!(tracker.current(), Orientation::Landscape);
        fake.skin_follows_sensor();
        let ended = rotate(
            &client,
            &tracker,
            &sensor,
            FAST_ROTATION,
            Orientation::ReversePortrait,
        )
        .await
        .unwrap();
        assert_eq!(ended, Orientation::Landscape);
        assert_eq!(
            *fake.rotations.lock().unwrap(),
            [[0.0, 0.0, 180.0], [0.0, 0.0, 0.0]]
        );
        assert_eq!(*sensor.lock().await, Orientation::Portrait);
    }

    /// Review M2: Rotate right pressed while a refused Rotate left still waits. The newer
    /// rotate runs only after the older one has put the sensor back, so it is never undone.
    #[tokio::test]
    async fn overlapping_rotates_run_one_at_a_time() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.skin_follows_sensor();
        let display = display(&tracker, 576, 1280);
        let sent = |vector: [f32; 3]| fake.rotations.lock().unwrap().contains(&vector);
        let (older, newer, ()) = tokio::join!(
            rotate(
                &client,
                &tracker,
                &sensor,
                FAST_ROTATION,
                Orientation::Landscape
            ),
            async {
                wait_until(|| sent([0.0, 0.0, 90.0])).await;
                rotate(
                    &client,
                    &tracker,
                    &sensor,
                    ROOMY_ROTATION,
                    Orientation::ReverseLandscape,
                )
                .await
            },
            // The guest turns for Rotate right only.
            async {
                wait_until(|| sent([0.0, 0.0, -90.0])).await;
                display.set(1280, 576);
            },
        );
        assert_eq!(older.unwrap(), Orientation::Portrait);
        assert_eq!(newer.unwrap(), Orientation::ReverseLandscape);
        assert_eq!(
            *fake.rotations.lock().unwrap(),
            [[0.0, 0.0, 90.0], [0.0, 0.0, 0.0], [0.0, 0.0, -90.0]]
        );
        assert_eq!(tracker.current(), Orientation::ReverseLandscape);
    }

    /// Review M5: a refusal whose reset fails says so apart from the first call, and the
    /// sensor is known to be left at the refused vector.
    #[tokio::test]
    async fn a_failed_reset_is_reported_as_its_own_failure() {
        let (fake, client, tracker, sensor) = emulator().await;
        fake.skin_follows_sensor();
        let _display = display(&tracker, 576, 1280);
        let rotating = rotate(
            &client,
            &tracker,
            &sensor,
            FAST_ROTATION,
            Orientation::Landscape,
        );
        let ((), result) = tokio::join!(
            async {
                wait_until(|| !fake.rotations.lock().unwrap().is_empty()).await;
                fake.fail_rotation();
            },
            rotating,
        );
        let error = result.unwrap_err();
        assert!(
            matches!(&error, DeviceOpError::Emulator(message) if message.contains("did not turn back")),
            "{error:?}"
        );
        assert_eq!(*sensor.lock().await, Orientation::Landscape);
    }

    /// What the fake recorded, in order: each text send's text, and `<down>` or `<up>` for a
    /// touch.
    fn recorded(fake: &FakeHandle) -> Vec<String> {
        fake.inputs
            .lock()
            .unwrap()
            .iter()
            .map(|event| match &event.r#type {
                Some(proto::input_event::Type::KeyEvent(key)) => key.text.clone(),
                Some(proto::input_event::Type::TouchEvent(touch)) => {
                    if touch.touches[0].pressure > 0 {
                        "<down>".to_owned()
                    } else {
                        "<up>".to_owned()
                    }
                }
                other => panic!("not a key or a touch: {other:?}"),
            })
            .collect()
    }

    /// Sends `events` through `input` as one batch, for an upright phone panel.
    async fn send_to(
        client: &EmulatorClient,
        input: &tokio::sync::Mutex<Option<InputState>>,
        events: Vec<DeviceInputEvent>,
    ) -> Result<(), DeviceOpError> {
        send_batch(
            client,
            &TaskTracker::new(),
            input,
            events,
            PANEL,
            Orientation::Portrait,
        )
        .await
    }

    fn text(text: &str) -> DeviceInputEvent {
        DeviceInputEvent::Text { text: text.into() }
    }

    fn touch(action: TouchAction) -> DeviceInputEvent {
        DeviceInputEvent::Touch {
            action,
            pointer_id: 0,
            x: 0.5,
            y: 0.5,
        }
    }

    /// A live Pixel_8 dropped a toolbar Paste of ` PASTED-ok` whole, with no error: one
    /// `KeyboardEvent.text` of about 32 key events never arrived. A text event goes out as
    /// sends of at most `TEXT_CHARS_PER_SEND` characters, in order.
    #[tokio::test]
    async fn text_is_sent_in_small_chunks_in_order() {
        let fake = FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();
        let input = tokio::sync::Mutex::new(None);
        let pasted = "The Quick BROWN fox JUMPS over 12 LAZY d";
        assert_eq!(pasted.len(), 40);
        send_to(&client, &input, vec![text(pasted)]).await.unwrap();
        wait_until(|| recorded(&fake).concat().len() >= pasted.len()).await;
        let sent = recorded(&fake);
        assert!(
            sent.iter()
                .all(|chunk| (1..=TEXT_CHARS_PER_SEND).contains(&chunk.chars().count())),
            "{sent:?}"
        );
        assert_eq!(sent.len(), pasted.len().div_ceil(TEXT_CHARS_PER_SEND));
        assert_eq!(sent.concat(), pasted);
    }

    /// A text event's sends stay together where the event was, even with another batch sent
    /// at the same time, and the touches around it are still tracked: nothing is left down.
    #[tokio::test]
    async fn text_chunks_keep_their_place_between_other_events() {
        let fake = FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();
        let input = tokio::sync::Mutex::new(None);
        let batch = |typed: &str| {
            vec![
                touch(TouchAction::Down),
                text(typed),
                touch(TouchAction::Up),
            ]
        };
        let (first, second) = tokio::join!(
            send_to(&client, &input, batch("abcdeFGHIJ")),
            send_to(&client, &input, batch("KLMNOpqrst")),
        );
        first.unwrap();
        second.unwrap();
        let expected = |typed: &str| {
            let chars: Vec<char> = typed.chars().collect();
            let mut events = vec!["<down>".to_owned()];
            events.extend(
                chars
                    .chunks(TEXT_CHARS_PER_SEND)
                    .map(|chunk| chunk.iter().collect::<String>()),
            );
            events.push("<up>".to_owned());
            events
        };
        // Each batch ends with its touch's release, so both are in once two have arrived.
        wait_until(|| {
            recorded(&fake)
                .iter()
                .filter(|event| *event == "<up>")
                .count()
                >= 2
        })
        .await;
        let sent = recorded(&fake);
        let in_order = |a: &str, b: &str| [expected(a), expected(b)].concat();
        assert!(
            sent == in_order("abcdeFGHIJ", "KLMNOpqrst")
                || sent == in_order("KLMNOpqrst", "abcdeFGHIJ"),
            "{sent:?}"
        );
        assert!(input.lock().await.as_ref().unwrap().pressed.is_empty());
    }

    /// A mixed-case ASCII string of `len` characters, with letters, digits, and symbols.
    fn mixed(len: usize) -> String {
        "aB3-cD_eF.gH9iJ kL0mN"
            .chars()
            .cycle()
            .zip(0..len)
            .map(|(c, i)| {
                if i % 7 == 6 {
                    char::from(b'0' + (i % 10) as u8)
                } else {
                    c
                }
            })
            .collect()
    }

    /// A connected client and an input stream nothing drains until the test reads it.
    async fn captured_input(
        room: usize,
    ) -> (
        FakeHandle,
        EmulatorClient,
        tokio::sync::Mutex<Option<InputState>>,
        mpsc::Receiver<proto::InputEvent>,
    ) {
        let fake = FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();
        let (sink, received) = mpsc::channel(room);
        let input = tokio::sync::Mutex::new(Some(InputState {
            sink,
            pressed: BTreeMap::new(),
        }));
        (fake, client, input, received)
    }

    fn sent_text(event: proto::InputEvent) -> String {
        match event.r#type {
            Some(proto::input_event::Type::KeyEvent(key)) => key.text,
            other => panic!("not a text send: {other:?}"),
        }
    }

    /// Back-to-back sends reach the guest as one burst, and a live Pixel_8 dropped every
    /// key of ` PASTED-ok` sent so (`SYN_DROPPED` on its keyboard). The longest text
    /// Android takes goes out a character at a time, in order, `TEXT_SEND_INTERVAL` apart.
    #[tokio::test]
    async fn a_hundred_characters_are_typed_in_order_with_a_pause_between_sends() {
        let (_fake, client, input, mut received) = captured_input(MAX_ANDROID_TEXT_PER_BATCH).await;
        let pasted = mixed(MAX_ANDROID_TEXT_PER_BATCH);
        tokio::time::pause();
        let reading = async {
            let mut sends = Vec::new();
            while sends
                .iter()
                .map(|(_, sent): &(_, String)| sent.len())
                .sum::<usize>()
                < pasted.len()
            {
                let event = received.recv().await.unwrap();
                sends.push((tokio::time::Instant::now(), sent_text(event)));
            }
            sends
        };
        let (typed, sends) = tokio::join!(send_to(&client, &input, vec![text(&pasted)]), reading);
        typed.unwrap();
        assert_eq!(
            sends
                .iter()
                .map(|(_, sent)| sent.as_str())
                .collect::<String>(),
            pasted
        );
        assert_eq!(sends.len(), pasted.len().div_ceil(TEXT_CHARS_PER_SEND));
        for pair in sends.windows(2) {
            let gap = pair[1].0 - pair[0].0;
            assert!(gap >= TEXT_SEND_INTERVAL, "{gap:?}");
        }
    }

    /// Android drops a key queued for more than 10 s, and Chrome's address bar took about
    /// 10 characters a second, so a long paste would lose its tail without a word. More than
    /// `MAX_ANDROID_TEXT_PER_BATCH` characters in one batch, in one text event or several,
    /// are refused before anything is sent. The ASCII refusal still comes first.
    #[tokio::test]
    async fn more_than_a_hundred_characters_in_a_batch_are_refused_before_anything_is_sent() {
        let (_fake, client, input, mut received) = captured_input(256).await;
        let too_much = DeviceOpError::InvalidInput(TOO_MUCH_TEXT.to_owned());
        let over = mixed(MAX_ANDROID_TEXT_PER_BATCH + 1);
        let error = send_to(&client, &input, vec![text(&over)])
            .await
            .unwrap_err();
        assert_eq!(error, too_much);
        let (first, second) = over.split_at(60);
        let error = send_to(&client, &input, vec![text(first), text(second)])
            .await
            .unwrap_err();
        assert_eq!(error, too_much);
        let error = send_to(&client, &input, vec![text("é"), text(&over)])
            .await
            .unwrap_err();
        assert_eq!(
            error,
            DeviceOpError::InvalidInput(UNTYPEABLE_TEXT.to_owned())
        );
        assert!(
            received.try_recv().is_err(),
            "a refused batch sent something"
        );
    }

    /// The length refusal is all or nothing, like the ASCII one: the touch in the batch is
    /// not sent, so no pointer is left down.
    #[tokio::test]
    async fn a_touch_with_too_much_text_is_refused_whole() {
        let (_fake, client, input, mut received) = captured_input(256).await;
        let error = send_to(
            &client,
            &input,
            vec![
                touch(TouchAction::Down),
                text(&mixed(MAX_ANDROID_TEXT_PER_BATCH + 1)),
                touch(TouchAction::Up),
            ],
        )
        .await
        .unwrap_err();
        assert_eq!(error, DeviceOpError::InvalidInput(TOO_MUCH_TEXT.to_owned()));
        assert!(
            received.try_recv().is_err(),
            "a refused batch sent something"
        );
        assert!(input.lock().await.as_ref().unwrap().pressed.is_empty());
    }

    /// Typing is never refused: a batch holds at most 64 events, and typed text is one
    /// character per event.
    #[tokio::test]
    async fn sixty_four_typed_characters_are_accepted() {
        let (_fake, client, input, mut received) = captured_input(256).await;
        let typed = mixed(crate::device::input::MAX_INPUT_EVENTS);
        let events = typed.chars().map(|c| text(&c.to_string())).collect();
        tokio::time::pause();
        send_to(&client, &input, events).await.unwrap();
        let mut sent = String::new();
        while let Ok(event) = received.try_recv() {
            sent.push_str(&sent_text(event));
        }
        assert_eq!(sent, typed);
    }

    /// Review of the chunked text: a stream that stops taking input fails the batch at the
    /// first send that waits out `INPUT_SEND_TIMEOUT`, once, not once per character, and the
    /// rest of the batch is not sent.
    #[tokio::test]
    async fn a_stuck_stream_fails_the_text_after_one_send_timeout() {
        let fake = FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();
        // Room for one send, and nothing ever reads it.
        let (sink, stuck) = mpsc::channel(1);
        let input = tokio::sync::Mutex::new(Some(InputState {
            sink,
            pressed: BTreeMap::new(),
        }));
        tokio::time::pause();
        let started = tokio::time::Instant::now();
        let error = send_to(
            &client,
            &input,
            vec![text("abcdefghij"), touch(TouchAction::Down)],
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DeviceOpError::Emulator(_)), "{error:?}");
        let waited = started.elapsed();
        assert!(
            (INPUT_SEND_TIMEOUT..2 * INPUT_SEND_TIMEOUT).contains(&waited),
            "{waited:?}"
        );
        // Only `a` got in; the touch after the text was never sent or recorded as down.
        assert_eq!(stuck.len(), 1);
        let state = input
            .try_lock()
            .expect("the batch let go of the input lock");
        assert!(state.as_ref().unwrap().pressed.is_empty());
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
