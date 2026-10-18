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
