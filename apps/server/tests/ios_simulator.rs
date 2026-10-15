//! Opt-in tests against this Mac's real iOS simulators, through CoreSimulator loaded in the
//! test process. They run only with `BIBCODE_IOS_SIM_TESTS=1`:
//!
//! ```sh
//! BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator -- --nocapture
//! ```
//!
//! Each test boots one `Shutdown` iPhone simulator (`BIBCODE_IOS_SIM_UDID`, or the first one
//! listed), and shuts it down again at the end, even when it fails, then checks that `simctl`
//! reports it `Shutdown`.
//!
//! - `BIBCODE_IOS_SIM_FRAMES_DIR`: also write the JPEGs and PNGs received there, and the
//!   first H.264 config and key as `first.h264` (Annex B, for `ffprobe`), to look at them.
#![cfg(target_os = "macos")]

use std::{
    process::Command,
    time::{Duration, Instant},
};

use bibcode_server::device::{
    DeviceManager, DeviceOpError,
    catalog::{AvailabilityKind, DeviceState, Orientation, Platform},
    frames::{FrameSubscription, JpegFrame},
    h264_sps_dimensions_for_tests as sps_dimensions,
    input::{DeviceInputEvent, TouchAction},
    ios_host_manager_for_tests,
    media::{MediaChannel, MediaFramesForTests},
    video::UnitKind,
};
use tokio::sync::watch;

fn enabled() -> bool {
    std::env::var("BIBCODE_IOS_SIM_TESTS").as_deref() == Ok("1")
}

/// One simulator test at a time: they may pick the same simulator.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `udid`'s state as `simctl list` reports it ("Shutdown", "Booted", …).
fn simctl_state(udid: &str) -> Option<String> {
    let output = Command::new("/usr/bin/xcrun")
        .args(["simctl", "list", "devices", "-j"])
        .output()
        .ok()?;
    let listing: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    listing["devices"]
        .as_object()?
        .values()
        .flat_map(|devices| devices.as_array().into_iter().flatten())
        .find(|device| device["udid"] == udid)
        .and_then(|device| device["state"].as_str().map(str::to_owned))
}

/// A `Shutdown` iPhone simulator to boot.
fn pick_simulator() -> String {
    if let Ok(udid) = std::env::var("BIBCODE_IOS_SIM_UDID") {
        return udid;
    }
    let output = Command::new("/usr/bin/xcrun")
        .args(["simctl", "list", "devices", "available", "-j"])
        .output()
        .expect("simctl list");
    let listing: serde_json::Value = serde_json::from_slice(&output.stdout).expect("listing");
    listing["devices"]
        .as_object()
        .expect("devices")
        .iter()
        .filter(|(runtime, _)| runtime.contains(".iOS-"))
        .flat_map(|(_, devices)| devices.as_array().into_iter().flatten())
        .find(|device| {
            device["state"] == "Shutdown"
                && device["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("iPhone"))
        })
        .and_then(|device| device["udid"].as_str().map(str::to_owned))
        .expect("a Shutdown iPhone simulator")
}

/// Shuts the simulator down when dropped, so a failed assertion never leaves it running.
struct ShutdownOnDrop(String);

impl Drop for ShutdownOnDrop {
    fn drop(&mut self) {
        if simctl_state(&self.0).as_deref() != Some("Shutdown") {
            let _ = Command::new("/usr/bin/xcrun")
                .args(["simctl", "shutdown", &self.0])
                .status();
        }
    }
}

struct IosHarness {
    manager: DeviceManager,
    _enabled: watch::Sender<bool>,
    udid: String,
    device_id: String,
    _shutdown: ShutdownOnDrop,
}

impl IosHarness {
    /// Boots a `Shutdown` iPhone through the device manager and waits until it is ready.
    async fn start() -> Self {
        let udid = pick_simulator();
        assert_eq!(
            simctl_state(&udid).as_deref(),
            Some("Shutdown"),
            "{udid} must be Shutdown before the test boots it"
        );
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_test_writer()
            .try_init();
        let (manager, enabled) = ios_host_manager_for_tests();
        manager.refresh_now().await;
        let availability = manager.catalog().borrow().platforms[0].availability.clone();
        println!("iOS availability: {availability:?}");
        assert_eq!(
            availability.kind,
            AvailabilityKind::Ready,
            "the guard goes live"
        );
        let shutdown = ShutdownOnDrop(udid.clone());
        let started = Instant::now();
        let device_id = manager.open(Platform::Ios, &udid).await.expect("open");
        let ready = match manager
            .wait_ready(&device_id, Duration::from_secs(240))
            .await
        {
            Ok(ready) => ready,
            Err(error) => panic!(
                "the simulator did not boot: {error:?}: {:?}",
                manager.catalog().borrow().devices
            ),
        };
        println!(
            "{udid} ready in {:?}: {:?}x{:?} {:?}",
            started.elapsed(),
            ready.width,
            ready.height,
            ready.orientation
        );
        Self {
            manager,
            _enabled: enabled,
            udid,
            device_id,
            _shutdown: shutdown,
        }
    }

    fn subscribe(&self) -> FrameSubscription {
        self.manager
            .subscribe_frames(&self.device_id)
            .expect("subscribe")
    }

    /// The first frame of `subscription` that `done` accepts, within `timeout`.
    async fn frame_where(
        &self,
        subscription: &mut FrameSubscription,
        timeout: Duration,
        done: impl Fn(&JpegFrame) -> bool,
    ) -> JpegFrame {
        tokio::time::timeout(timeout, async {
            loop {
                if let Some(frame) = subscription.frames.borrow_and_update().clone()
                    && done(&frame)
                {
                    return frame;
                }
                subscription
                    .frames
                    .changed()
                    .await
                    .expect("producer running");
            }
        })
        .await
        .expect("a frame in time")
    }

    /// The first frame, and the subscription it came on.
    async fn first_jpeg(&self, timeout: Duration) -> (JpegFrame, FrameSubscription) {
        let mut subscription = self.subscribe();
        let frame = self.frame_where(&mut subscription, timeout, |_| true).await;
        save(&frame, "first");
        (frame, subscription)
    }

    fn snapshot(&self) -> bibcode_server::device::catalog::DeviceSnapshot {
        self.manager
            .catalog()
            .borrow()
            .devices
            .iter()
            .find(|device| device.device_id == self.device_id)
            .cloned()
            .expect("device in catalog")
    }

    /// Closes the device and checks that `simctl` reports the simulator `Shutdown`. A
    /// device that already failed stays failed.
    async fn stop(self) {
        let started = Instant::now();
        let ended = self.snapshot().state == DeviceState::Failed;
        self.manager.close(&self.device_id).await.expect("close");
        let stopped = self.snapshot();
        let expected = if ended {
            DeviceState::Failed
        } else {
            DeviceState::Stopped
        };
        assert_eq!(stopped.state, expected, "{stopped:?}");
        self.manager.shutdown().await;
        let deadline = Instant::now() + Duration::from_secs(30);
        while simctl_state(&self.udid).as_deref() != Some("Shutdown") {
            assert!(Instant::now() < deadline, "{} did not shut down", self.udid);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        println!("{} Shutdown after {:?}", self.udid, started.elapsed());
    }
}

/// An RGB picture.
struct Rgb {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

impl Rgb {
    fn decode(png: &[u8]) -> Self {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .expect("a PNG");
        let mut pixels = vec![0; reader.output_buffer_size().expect("a size")];
        let info = reader.next_frame(&mut pixels).expect("a PNG frame");
        pixels.truncate(info.buffer_size());
        let channels = info.color_type.samples();
        let pixels = pixels
            .chunks(channels)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        Self {
            pixels,
            width: info.width,
            height: info.height,
        }
    }

    fn at(&self, x: u32, y: u32) -> &[u8] {
        let offset = ((y * self.width + x) * 3) as usize;
        &self.pixels[offset..offset + 3]
    }

    /// Nearest-neighbour scaling to `width` x `height`.
    fn resized(&self, width: u32, height: u32) -> Self {
        let pixels = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                self.at(x * self.width / width, y * self.height / height)
                    .to_vec()
            })
            .collect();
        Self {
            pixels,
            width,
            height,
        }
    }

    fn half_turn(&self) -> Self {
        let pixels = self
            .pixels
            .chunks(3)
            .rev()
            .flat_map(<[u8]>::to_vec)
            .collect();
        Self {
            pixels,
            width: self.width,
            height: self.height,
        }
    }

    /// The mean absolute difference per channel.
    fn mean_difference(&self, other: &Self) -> f64 {
        assert_eq!((self.width, self.height), (other.width, other.height));
        let total: u64 = self
            .pixels
            .iter()
            .zip(&other.pixels)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum();
        total as f64 / self.pixels.len() as f64
    }

    /// The share of pixels with a channel more than 24 apart.
    fn changed_share(&self, other: &Self) -> f64 {
        if (self.width, self.height) != (other.width, other.height) {
            return 1.0;
        }
        let changed = self
            .pixels
            .chunks(3)
            .zip(other.pixels.chunks(3))
            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 24))
            .count();
        changed as f64 / (self.pixels.len() / 3) as f64
    }
}

/// `simctl`'s own screenshot of `udid`, upright in the UI orientation.
fn simctl_screenshot(udid: &str) -> Rgb {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("screenshot.png");
    let status = Command::new("/usr/bin/xcrun")
        .args(["simctl", "io", udid, "screenshot", "--type=png"])
        .arg(&path)
        .status()
        .expect("simctl io screenshot");
    assert!(status.success());
    Rgb::decode(&std::fs::read(&path).expect("read screenshot"))
}

impl IosHarness {
    /// Opens a page in Safari, which turns with the device; the iPhone home screen does not.
    async fn open_safari(&self) {
        let status = Command::new("/usr/bin/xcrun")
            .args(["simctl", "openurl", &self.udid, "https://example.com"])
            .status()
            .expect("simctl openurl");
        assert!(status.success());
        tokio::time::sleep(Duration::from_secs(4)).await;
    }

    /// Asks the device to turn and prints what the catalog then reports.
    async fn rotate(&self, orientation: Orientation) {
        let started = Instant::now();
        self.manager
            .rotate(&self.device_id, orientation)
            .await
            .expect("rotate");
        let snapshot = self.snapshot();
        println!(
            "rotate {orientation:?} -> {:?} {:?}x{:?} in {:?}",
            snapshot.orientation,
            snapshot.width,
            snapshot.height,
            started.elapsed()
        );
    }

    /// A down and an up at a point of the current orientation.
    async fn tap(&self, x: f64, y: f64) {
        let touch = |action| DeviceInputEvent::Touch {
            action,
            pointer_id: 0,
            x,
            y,
        };
        self.manager
            .input(&self.device_id, vec![touch(TouchAction::Down)])
            .await
            .expect("touch down");
        tokio::time::sleep(Duration::from_millis(60)).await;
        self.manager
            .input(&self.device_id, vec![touch(TouchAction::Up)])
            .await
            .expect("touch up");
    }

    /// The live screen as RGB, at most `max_width` wide; saved as `name` when asked.
    async fn screen_rgb(&self, max_width: u32, name: &str) -> Rgb {
        let (png, mime_type, _, _) = self
            .manager
            .png_for_tests(&self.device_id, max_width)
            .await
            .expect("a screenshot");
        assert_eq!(mime_type, "image/png", "a live simulator gives PNGs");
        if let Ok(dir) = std::env::var("BIBCODE_IOS_SIM_FRAMES_DIR") {
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            std::fs::write(&path, &png).expect("write screenshot");
            println!("wrote {}", path.display());
        }
        Rgb::decode(&png)
    }

    /// Waits up to `timeout` for more than 5% of the screen's pixels to differ from
    /// `before`, and returns the share that did.
    async fn screen_changes_from(&self, before: &Rgb, timeout: Duration, name: &str) -> f64 {
        let deadline = Instant::now() + timeout;
        loop {
            let now = self.screen_rgb(before.width, name).await;
            let changed = before.changed_share(&now);
            if changed > 0.05 {
                return changed;
            }
            assert!(
                Instant::now() < deadline,
                "only {:.1}% of pixels changed",
                changed * 100.0
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Waits up to `timeout` for the device to reach `state`.
    async fn wait_state(
        &self,
        state: DeviceState,
        timeout: Duration,
    ) -> bibcode_server::device::catalog::DeviceSnapshot {
        let deadline = Instant::now() + timeout;
        loop {
            let snapshot = self.snapshot();
            if snapshot.state == state {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "the device did not reach {state:?}: {snapshot:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// One H.264 unit as a media socket receives it.
#[derive(Debug)]
struct MediaUnit {
    kind: UnitKind,
    pts_us: u64,
    data: Vec<u8>,
}

/// The next unit of `media`, decoded from `[kind u8][pts u64 BE][Annex B data]`.
async fn next_unit(media: &mut MediaFramesForTests, timeout: Duration) -> MediaUnit {
    let frame = tokio::time::timeout(timeout, media.next_frame())
        .await
        .expect("a video unit in time")
        .expect("the stream is open");
    let kind = match frame[0] {
        0 => UnitKind::Config,
        1 => UnitKind::Key,
        2 => UnitKind::Delta,
        other => panic!("unknown unit kind {other}"),
    };
    MediaUnit {
        kind,
        pts_us: u64::from_be_bytes(frame[1..9].try_into().expect("pts")),
        data: frame[9..].to_vec(),
    }
}

/// Units up to and including the first config of `size` and the key after it, and their
/// byte count.
async fn config_of_size(media: &mut MediaFramesForTests, size: (u32, u32)) -> (u32, usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut units, mut bytes) = (0u32, 0usize);
    loop {
        assert!(Instant::now() < deadline, "no {size:?} config in time");
        let unit = next_unit(media, Duration::from_secs(10)).await;
        units += 1;
        bytes += unit.data.len();
        if unit.kind == UnitKind::Config && sps_dimensions(&unit.data) == Some(size) {
            let key = next_unit(media, Duration::from_secs(5)).await;
            assert_eq!(key.kind, UnitKind::Key, "config comes before a key");
            return (units + 1, bytes + key.data.len());
        }
    }
}

impl IosHarness {
    /// The portrait screen size the catalog reports.
    fn screen_size(&self) -> (u32, u32) {
        let snapshot = self.snapshot();
        (
            snapshot.width.expect("width"),
            snapshot.height.expect("height"),
        )
    }

    /// Joins the H.264 stream as a media socket would.
    fn media(&self) -> MediaFramesForTests {
        self.manager
            .media_frames_for_tests(&self.device_id)
            .expect("attach media")
    }

    /// The first config and key units of a fresh stream, and the stream.
    async fn first_video_units(&self, timeout: Duration) -> (Vec<MediaUnit>, MediaFramesForTests) {
        let mut media = self.media();
        let config = next_unit(&mut media, timeout).await;
        let key = next_unit(&mut media, timeout).await;
        (vec![config, key], media)
    }
}

/// Writes `frame` to `BIBCODE_IOS_SIM_FRAMES_DIR` when it is set.
fn save(frame: &JpegFrame, name: &str) {
    use base64::Engine as _;
    let Ok(dir) = std::env::var("BIBCODE_IOS_SIM_FRAMES_DIR") else {
        return;
    };
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(&frame.jpeg_base64)
        .expect("base64");
    let path = std::path::Path::new(&dir).join(format!("{name}.jpg"));
    std::fs::write(&path, jpeg).expect("write frame");
    println!("wrote {}", path.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn live_frames_arrive_from_a_booted_simulator() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    let started = Instant::now();
    let (frame, mut subscription) = harness.first_jpeg(Duration::from_secs(20)).await;
    println!(
        "first frame after {:?}: {}x{} {:?}, {} base64 bytes",
        started.elapsed(),
        frame.width,
        frame.height,
        frame.orientation,
        frame.jpeg_base64.len()
    );
    assert!(frame.width >= 300 && frame.height > frame.width);
    assert_eq!(frame.width, 720, "scaled to the JPEG width");
    let snapshot = harness.snapshot();
    assert_eq!(snapshot.orientation, Orientation::Portrait);
    // The portrait framebuffer's size, not a screenshot's.
    assert_eq!((snapshot.width, snapshot.height), (Some(1206), Some(2622)));
    // The stream keeps going on the same subscription: new pixels, or the last frame again
    // after a quiet second.
    let seq = frame.seq;
    let next = harness
        .frame_where(&mut subscription, Duration::from_secs(5), |frame| {
            frame.seq > seq
        })
        .await;
    println!("frame {} followed frame {seq}", next.seq);
    drop(subscription);
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rotation_changes_the_screen_size() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    harness.open_safari().await;
    let mut subscription = harness.subscribe();
    assert!(harness.snapshot().can_rotate, "a live simulator turns");
    harness.rotate(Orientation::Landscape).await;
    let landscape = harness
        .frame_where(&mut subscription, Duration::from_secs(10), |frame| {
            frame.orientation == Orientation::Landscape
        })
        .await;
    save(&landscape, "landscape");
    println!(
        "landscape frame: {}x{} {:?}",
        landscape.width, landscape.height, landscape.orientation
    );
    assert!(landscape.width > landscape.height);
    let snapshot = harness.snapshot();
    assert_eq!(snapshot.orientation, Orientation::Landscape);
    assert_eq!((snapshot.width, snapshot.height), (Some(2622), Some(1206)));
    harness.rotate(Orientation::ReverseLandscape).await;
    let reverse = harness
        .frame_where(&mut subscription, Duration::from_secs(10), |frame| {
            frame.orientation == Orientation::ReverseLandscape
        })
        .await;
    save(&reverse, "reverse-landscape");
    assert!(reverse.width > reverse.height);
    assert_eq!(
        harness.snapshot().orientation,
        Orientation::ReverseLandscape
    );
    harness.rotate(Orientation::Portrait).await;
    let portrait = harness
        .frame_where(&mut subscription, Duration::from_secs(10), |frame| {
            frame.orientation == Orientation::Portrait
        })
        .await;
    save(&portrait, "portrait");
    assert!(portrait.height > portrait.width);
    let snapshot = harness.snapshot();
    assert_eq!((snapshot.width, snapshot.height), (Some(1206), Some(2622)));
    drop(subscription);
    harness.stop().await;
}

/// BiBCode's `landscape` is Android's: the phone turned left (counter-clockwise), so the
/// UI's top lies along the device's right edge. That convention comes from spec §0's
/// observation of which surface edge the UI's top lies along at each `uiOrientation`, which
/// `IosOrientation` encodes. This test checks the chain end to end: `landscape` is
/// reported back as `landscape` at the landscape size, and the frame BiBCode turns upright
/// with that orientation's turn matches `simctl`'s own upright screenshot rather than the
/// same picture upside down, so the turn, and with it the edge, is the real one.
#[tokio::test(flavor = "multi_thread")]
async fn rotation_matches_android_convention() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    harness.open_safari().await;
    harness.rotate(Orientation::Landscape).await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.snapshot().orientation != Orientation::Landscape {
        assert!(
            Instant::now() < deadline,
            "Safari did not turn to landscape"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let snapshot = harness.snapshot();
    assert_eq!((snapshot.width, snapshot.height), (Some(2622), Some(1206)));
    // Let the turn's animation settle before comparing pictures.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let ours = harness.screen_rgb(400, "android-convention-bibcode").await;
    let theirs = simctl_screenshot(&harness.udid);
    println!(
        "simctl screenshot {}x{}, BiBCode frame {}x{}",
        theirs.width, theirs.height, ours.width, ours.height
    );
    assert!(
        theirs.width > theirs.height,
        "simctl's screenshot is upright landscape"
    );
    let theirs = theirs.resized(ours.width, ours.height);
    let upright = ours.mean_difference(&theirs);
    let upside_down = ours.mean_difference(&theirs.half_turn());
    println!("mean difference: upright {upright:.1}, upside down {upside_down:.1}");
    assert!(
        upright * 2.0 < upside_down,
        "the landscape frame matches simctl's upright screenshot"
    );
    // A tap in landscape coordinates lands where the frame shows it: Safari's address
    // field, at the top centre of the landscape page, takes focus.
    harness.tap(0.5, 0.08).await;
    let changed = harness
        .screen_changes_from(&ours, Duration::from_secs(5), "android-convention-tapped")
        .await;
    println!(
        "{:.1}% of pixels changed after a landscape tap",
        changed * 100.0
    );
    harness.rotate(Orientation::Portrait).await;
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tap_changes_the_screen() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    // Let the home screen settle after boot.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let before = harness.screen_rgb(360, "tap-before").await;
    let started = Instant::now();
    // The Settings icon on the default home screen (spec §0); the centre is wallpaper.
    harness.tap(0.848, 0.483).await;
    let changed = harness
        .screen_changes_from(&before, Duration::from_secs(5), "tap-after")
        .await;
    println!(
        "{:.1}% of pixels changed {:?} after the tap",
        changed * 100.0,
        started.elapsed()
    );
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn text_arrives_in_a_text_field() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    harness.open_safari().await;
    let page = harness.screen_rgb(360, "text-page").await;
    // Safari's address bar sits at the bottom of the page on iOS 26; focusing it opens
    // the start page and the keyboard.
    harness.tap(0.47, 0.935).await;
    harness
        .screen_changes_from(&page, Duration::from_secs(5), "text-focused")
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let focused = harness.screen_rgb(360, "text-focused").await;
    let started = Instant::now();
    let text = vec![DeviceInputEvent::Text {
        text: "BiBCode".to_owned(),
    }];
    // The keyboard path needs the DTUHID handshake, which may still run after boot.
    loop {
        match harness
            .manager
            .input(&harness.device_id, text.clone())
            .await
        {
            Ok(()) => break,
            Err(DeviceOpError::InvalidInput(message))
                if message.contains("not accepting keyboard input yet")
                    && started.elapsed() < Duration::from_secs(45) =>
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => panic!("typing failed: {error:?}"),
        }
    }
    println!("typed after {:?}", started.elapsed());
    let changed = harness
        .screen_changes_from(&focused, Duration::from_secs(5), "text-typed")
        .await;
    println!("{:.1}% of pixels changed after typing", changed * 100.0);
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn external_shutdown_fails_device() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    harness
        .manager
        .create_media_ticket(
            &harness.device_id,
            "ios-simulator-test",
            MediaChannel::Plain,
        )
        .expect("a media ticket");
    let (_, media) = harness.first_video_units(Duration::from_secs(20)).await;
    let started = Instant::now();
    let udid = harness.udid.clone();
    let shutdown = std::thread::spawn(move || {
        let status = Command::new("/usr/bin/xcrun")
            .args(["simctl", "shutdown", &udid])
            .status()
            .expect("simctl shutdown");
        (status, Instant::now())
    });
    // Within 5 s of the shutdown starting.
    let failed = harness
        .wait_state(DeviceState::Failed, Duration::from_secs(5))
        .await;
    println!(
        "failed {:?} after simctl shutdown started: {:?}",
        started.elapsed(),
        failed.failure
    );
    let (status, returned) = shutdown.join().expect("the shutdown thread");
    assert!(status.success());
    println!("simctl shutdown took {:?}", returned - started);
    assert_eq!(failed.failure.as_deref(), Some("Shutdown outside BiBCode"));
    let reason = tokio::time::timeout(Duration::from_secs(5), media.end_reason(&harness.manager))
        .await
        .expect("the media stream closes");
    assert_eq!(reason, "stopped");
    harness.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn first_key_frame_decodes_to_the_screen_size() {
    if !enabled() {
        return;
    }
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    // A live simulator accepts media tickets now that it has video.
    harness
        .manager
        .create_media_ticket(
            &harness.device_id,
            "ios-simulator-test",
            MediaChannel::Plain,
        )
        .expect("a media ticket");
    let started = Instant::now();
    let (units, mut media) = harness.first_video_units(Duration::from_secs(20)).await;
    println!(
        "config ({} bytes) and key ({} bytes) after {:?}",
        units[0].data.len(),
        units[1].data.len(),
        started.elapsed()
    );
    assert_eq!(units[0].kind, UnitKind::Config);
    assert_eq!(units[1].kind, UnitKind::Key);
    let (w, h) = sps_dimensions(&units[0].data).expect("an SPS");
    // `00 00 00 01 67 profile constraints level …`
    println!(
        "SPS profile_idc {} level_idc {}",
        units[0].data[5], units[0].data[7]
    );
    assert_eq!((w, h), harness.screen_size());
    if let Ok(dir) = std::env::var("BIBCODE_IOS_SIM_FRAMES_DIR") {
        // Config then key as an Annex B file, for `ffprobe`.
        let path = std::path::Path::new(&dir).join("first.h264");
        std::fs::write(&path, [&units[0].data[..], &units[1].data[..]].concat()).expect("write");
        println!("wrote {}", path.display());
    }
    // Whatever else the screen sends for two seconds, to see the rate it runs at.
    let window = Instant::now();
    let (mut count, mut keys, mut bytes, mut last_pts) = (0u32, 0u32, 0usize, units[1].pts_us);
    while window.elapsed() < Duration::from_secs(2) {
        let Ok(frame) = tokio::time::timeout(Duration::from_millis(500), media.next_frame()).await
        else {
            continue;
        };
        let frame = frame.expect("the stream is open");
        let pts = u64::from_be_bytes(frame[1..9].try_into().expect("pts"));
        assert!(pts >= last_pts, "pts never goes back");
        last_pts = pts;
        count += 1;
        keys += u32::from(frame[0] == 1);
        bytes += frame.len();
    }
    println!("{count} more units ({keys} keys, {bytes} bytes) in the next 2 s");
    harness.open_safari().await;
    let turning = Instant::now();
    harness.rotate(Orientation::Landscape).await;
    // Landscape, so the picture is 2622x1206.
    let (units, bytes) = config_of_size(&mut media, (h, w)).await;
    let elapsed = turning.elapsed();
    println!(
        "landscape config after {elapsed:?}; {units} units, {bytes} bytes while turning ({:.1} Mbit/s)",
        bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6
    );
    harness.rotate(Orientation::Portrait).await;
    config_of_size(&mut media, (w, h)).await;
    println!("portrait config again");
    drop(media);
    harness.stop().await;
}
