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
//! - `BIBCODE_IOS_SIM_FRAMES_DIR`: also write the JPEGs received there, and the first H.264
//!   config and key as `first.h264` (Annex B, for `ffprobe`), to look at them.
//! - `BIBCODE_IOS_SIM_ROTATE_TOOL`: a command run as `<tool> <udid> purple-rotate <n>` that
//!   turns the simulator (spike S2's probe), until BiBCode rotates simulators itself.
#![cfg(target_os = "macos")]

use std::{
    process::Command,
    time::{Duration, Instant},
};

use bibcode_server::device::{
    DeviceManager,
    catalog::{AvailabilityKind, DeviceState, Orientation, Platform},
    frames::{FrameSubscription, JpegFrame},
    h264_sps_dimensions_for_tests as sps_dimensions, ios_host_manager_for_tests,
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

    /// Closes the device and checks that `simctl` reports the simulator `Shutdown`.
    async fn stop(self) {
        let started = Instant::now();
        self.manager.close(&self.device_id).await.expect("close");
        let stopped = self.snapshot();
        assert_eq!(stopped.state, DeviceState::Stopped, "{stopped:?}");
        self.manager.shutdown().await;
        let deadline = Instant::now() + Duration::from_secs(30);
        while simctl_state(&self.udid).as_deref() != Some("Shutdown") {
            assert!(Instant::now() < deadline, "{} did not shut down", self.udid);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        println!("{} Shutdown after {:?}", self.udid, started.elapsed());
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
async fn live_frames_turn_with_the_simulator() {
    if !enabled() {
        return;
    }
    let Ok(tool) = std::env::var("BIBCODE_IOS_SIM_ROTATE_TOOL") else {
        println!("BIBCODE_IOS_SIM_ROTATE_TOOL is not set; skipping the rotation check");
        return;
    };
    let _serial = ONE_AT_A_TIME.lock().await;
    let harness = IosHarness::start().await;
    // The iPhone home screen does not rotate; Safari does.
    let launched = Command::new("/usr/bin/xcrun")
        .args(["simctl", "launch", &harness.udid, "com.apple.mobilesafari"])
        .status()
        .expect("simctl launch");
    assert!(launched.success());
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut subscription = harness.subscribe();
    let rotate = |value: &str| {
        let status = Command::new(&tool)
            .args([harness.udid.as_str(), "purple-rotate", value])
            .status()
            .expect("rotate tool");
        assert!(status.success());
    };
    // Purple 4 → uiOrientation 3 (spec §0).
    rotate("4");
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
    // Purple 3 → uiOrientation 4.
    rotate("3");
    let reverse = harness
        .frame_where(&mut subscription, Duration::from_secs(10), |frame| {
            frame.orientation == Orientation::ReverseLandscape
        })
        .await;
    save(&reverse, "reverse-landscape");
    assert!(reverse.width > reverse.height);
    rotate("1");
    let portrait = harness
        .frame_where(&mut subscription, Duration::from_secs(10), |frame| {
            frame.orientation == Orientation::Portrait
        })
        .await;
    save(&portrait, "portrait");
    assert!(portrait.height > portrait.width);
    drop(subscription);
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
    assert_eq!((w, h), harness.screen_size());
    if let Ok(dir) = std::env::var("BIBCODE_IOS_SIM_FRAMES_DIR") {
        // Config then key as an Annex B file, for `ffprobe`.
        let path = std::path::Path::new(&dir).join("first.h264");
        std::fs::write(&path, [&units[0].data[..], &units[1].data[..]].concat()).expect("write");
        println!("wrote {}", path.display());
    }
    // Whatever else the screen sends for two seconds, to see the rate it runs at.
    let window = Instant::now();
    let (mut count, mut bytes, mut last_pts) = (0u32, 0usize, units[1].pts_us);
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
        bytes += frame.len();
    }
    println!("{count} more units ({bytes} bytes) in 2 s on a quiet screen");
    if let Ok(tool) = std::env::var("BIBCODE_IOS_SIM_ROTATE_TOOL") {
        // The iPhone home screen does not rotate; Safari does.
        let launched = Command::new("/usr/bin/xcrun")
            .args(["simctl", "launch", &harness.udid, "com.apple.mobilesafari"])
            .status()
            .expect("simctl launch");
        assert!(launched.success());
        tokio::time::sleep(Duration::from_secs(3)).await;
        let rotate = |value: &str| {
            let status = Command::new(&tool)
                .args([harness.udid.as_str(), "purple-rotate", value])
                .status()
                .expect("rotate tool");
            assert!(status.success());
        };
        let turning = Instant::now();
        // Purple 4 → uiOrientation 3 (spec §0): landscape, so the picture is 2622x1206.
        rotate("4");
        let (units, bytes) = config_of_size(&mut media, (h, w)).await;
        let elapsed = turning.elapsed();
        println!(
            "landscape config after {elapsed:?}; {units} units, {bytes} bytes while turning ({:.1} Mbit/s)",
            bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6
        );
        rotate("1");
        config_of_size(&mut media, (w, h)).await;
        println!("portrait config again");
    } else {
        println!("BIBCODE_IOS_SIM_ROTATE_TOOL is not set; skipping the rotated stream check");
    }
    drop(media);
    harness.stop().await;
}
