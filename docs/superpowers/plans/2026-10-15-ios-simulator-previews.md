# iOS Simulator device previews Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add iOS Simulator previews on macOS servers to the existing Devices pane, with live H.264 video, touch, keys, text and rotation (if possible), next to Android, behind one shared device manager.

**Architecture:** `apps/server/src/device/` splits into a platform-neutral manager and two backends. The manager talks to backends through two traits: `DeviceBackend`, which discovers and launches devices, and `RunningDevice`, which handles frames, video, input, rotation, exit and stop. Android moves behind the traits with no behavior change. The new iOS backend:

- uses `xcrun simctl` to start and stop simulators and to list them;
- loads CoreSimulator and SimulatorKit at runtime through `objc2`;
- reads the framebuffer as an IOSurface;
- encodes video with VideoToolbox, Apple's public H.264 encoder;
- sends input through SimulatorKit's private Indigo HID functions.

Everything after the backend reuses the existing JPEG frames, media socket, E2EE and pane.

**Tech Stack:** Rust (Tokio, `objc2` family, VideoToolbox through `objc2-video-toolbox`), `xcrun simctl`, Effect Schema contracts, React (Vite+).

**Spec:** `docs/superpowers/specs/2026-10-15-ios-simulator-previews-design.md`

## Global Constraints

- No production Node runtime, no Electron and no native helper sidecar. All iOS code runs inside the `bibcode` binary. `idb_companion` is forbidden.
- iOS code compiles only under `cfg(target_os = "macos")`. Linux and Windows builds register no iOS backend, and their catalog has no `ios` platform entry.
- New Rust crates are macOS-only (`[target.'cfg(target_os = "macos")'.dependencies]`):
  - versions are pinned exactly (`=x.y.z`) and are at least two weeks old;
  - each is recorded in `docs/dependency-upgrades/2026-07-17-ledger.json`;
  - `vp run check:dependency-ledger` passes.
- No npm dependencies are added.
- Nothing has shipped, so contract fields are renamed in place without compatibility aliases: `avds` → `platforms[].images`, `avdName` → `imageId`.
- One running device per platform. `DeviceLimitError` is per platform.
- BibCode shuts down only simulators it booted. Simulators booted elsewhere are listed with `inUseElsewhere: true` and refused with `DeviceBusyError`.
- iOS touch is single-pointer. Extra pointers are ignored without an error.
- Logs and profiler records never contain screen content or typed text, only lengths.
- Rust gates use the pinned toolchain:
  - put it on the PATH first: `export PATH=$HOME/.rustup/toolchains/1.98.0-aarch64-apple-darwin/bin:$PATH`;
  - `cargo fmt --all --check`;
  - `cargo clippy -p bibcode-server --all-targets -- -D warnings`;
  - `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`.
- JS gates:
  - `vp check` must pass with zero warnings (main's new gate);
  - `vp run typecheck`;
  - `vp run check:contracts`.
- RPC method count stays at **158** and stream count at **24**. Fixture and fingerprint totals change only by regeneration.
- Copy is exactly as the spec gives it (§6, §7, §8). New user-facing copy is reviewed against `UI.md`.
- Commit trailers on every commit:
  - The model's own `Co-Authored-By: <model> <noreply@anthropic.com>` line.
  - `Claude-Session: https://claude.ai/code/session_013wRcoKGahv3KuuNuNQn2tV`.
- Do not edit `.repos/` or `.codegraph/`. Do not push.

## Review Focus

1. **The simulator dies while it is streaming**, for example when the user quits Simulator.app or runs `simctl shutdown` from a terminal. The device must go `failed` with a reason, the media sockets must close with `stopped`, and no BibCode thread may stay blocked in an FFI callback. *Pinned by:* Task 5 Step 3 (`unrequested_shutdown_fails_the_device`) and Task 10 (real-sim `external_shutdown_fails_device`).
2. **The screen size changes mid-stream (rotation)**: the IOSurface is replaced with new dimensions. The encoder must restart and emit a new config unit before any frame of the new size, and the player must reconfigure. *Pinned by:* Task 7 Step 1 (`size_change_emits_config_before_frames`).
3. **A tap arrives before the first frame, or after close started.** Input on a non-`Ready` device returns `DeviceNotReadyError`, and nothing reaches FFI after the HID client is released. *Pinned by:* Task 8 Step 1 (`input_after_stop_is_refused_without_ffi`).
4. **Text with emoji and non-US characters**, mixed with ASCII in one event. US-typeable runs must go out as keys, and the rest through the clipboard and Cmd+V, in order. *Pinned by:* Task 8 Step 1 (`mixed_text_is_split_in_order`).
5. **Xcode is updated under a running server** to a version without the private symbols. The cached guard must re-probe when the Xcode path or version changes, and new opens must go view-only with the `xcodeUnsupported` message. *Pinned by:* Task 6 Step 1 (`guard_reprobes_when_xcode_changes`).

---

## File map

**Server, platform-neutral (`apps/server/src/device/`)**
- `mod.rs` (modify): the manager over `Vec<Arc<dyn DeviceBackend>>`, slot holds `Arc<dyn RunningDevice>`, per-platform limit, `DeviceBusyError`, and `open(platform, image_id)`.
- `backend.rs` (create): the `DeviceBackend` and `RunningDevice` traits, `PlatformCatalog`, `LaunchFailure`, and `ErasedVideoSource`.
- `catalog.rs` (modify): `Platform`, `PlatformCatalog`, `Image`, the new availability kinds, and `DeviceSnapshot { platform, image_id, can_rotate }`.
- `frames.rs`, `media.rs` (modify): the producer and the hub get their source from `RunningDevice`, not from the gRPC client or adb.
- `input.rs` (modify): keeps the wire types and `validate`. The Android mapping moves to `android/input.rs`.
- `fake_backend.rs` (create, `#[cfg(test)]`): an in-memory backend for the manager tests.

**Server, Android (`apps/server/src/device/android/`)**: `mod.rs` (`AndroidBackend` and `RunningAndroid`), plus `sdk.rs`, `emulator.rs`, `grpc.rs`, `scrcpy.rs` and `input.rs` moved from `device/`. `testing.rs` moves to `android/testing.rs`.

**Server, iOS (`apps/server/src/device/ios/`, macOS only)**
- `mod.rs`: `IosBackend` and `RunningSimulator`.
- `simctl.rs`: runs and parses `xcrun simctl`.
- `guard.rs`: Xcode detection and the private-API probe, cached per Xcode path and version.
- `coresim.rs`: all `unsafe`. Loads the frameworks, finds `SimDevice`, registers the screen callbacks, and creates the HID client.
- `encoder.rs`: the VideoToolbox session and AVCC → Annex B conversion.
- `hid.rs`: maps touch, buttons, keys and text to Indigo messages. The mapping is pure; FFI sends go through `coresim`.
- `keymap.rs`: the US layout table from characters to HID usages.

**Server, callers**: `production/device_rpc.rs`, `production/connect_mcp/device_tools.rs`, `production/runtime.rs` (manager construction), `diagnostics/profiler` (no new methods).

**Contracts**: `packages/contracts/src/device.ts`, `device.test.ts`, regenerated `fixtures/rpc-wire/*`.

**Web (`apps/web/src/components/device/`)**: `DevicePanel.tsx`, `DeviceToolbar.tsx`, `DeviceScreen.tsx`, `deviceMessages.ts`, their tests, and `settings/DevicePreviewsSection.tsx`.

**Docs**:
- The spec's §0.
- `docs/architecture/overview.md` (Device previews).
- `docs/user/workspace-ui.md`.
- `docs/skills/bibcode-profiler/references/events.md`.
- `docs/testing/macos-desktop.md` and `docs/testing/cross-platform-validation.md`.
- `CHANGELOG.md`.

---

### Task 1: Spikes S1–S3 (throwaway probes; record results in the spec)

**Files:**
- Create (scratch, not committed): `$SCRATCH/ios-spike/` (Swift scripts).
- Modify: `docs/superpowers/specs/2026-10-15-ios-simulator-previews-design.md` §0.

**Interfaces:**
- Produces: the confirmed selector, function and struct names that Tasks 6–8 use, written into spec §0 under "Results". Later tasks take names **from §0**, not from this plan, wherever the two differ.

This task is controller-run (or run by one implementer) on this Mac. It needs Xcode 27 and the iPhone 17 Pro simulator `308FD3CD-F46A-44B9-9B68-9A849F337C80`. Reference for the private API shapes: facebook/idb on GitHub, files `FBSimulatorControl/Framebuffer/FBFramebuffer.m`, `FBSimulatorControl/HID/FBSimulatorHID.m` and `FBSimulatorControl/HID/FBSimulatorIndigoHID.m`. Read them over the web; do not vendor them.

- [ ] **Step 1: S3 framebuffer probe.** Write `fb.swift`, which does the following, then run it: `xcrun swiftc -F /Library/Developer/PrivateFrameworks -F /Applications/Xcode.app/Contents/SharedFrameworks fb.swift -o fb && ./fb <udid>` with the simulator booted. Record each confirmed selector name, the pixel format (expected `BGRA`, `'BGRA'` FourCC) and the size.
  1. `dlopen`s `/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator` and `/Applications/Xcode.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit`.
  2. Gets `SimServiceContext.sharedServiceContextForDeveloperDir:error:` (the developer dir from `xcode-select -p`), then `defaultDeviceSetWithError:`, then `devices`, and finds the UDID.
  3. Walks `device.io.ioPorts`. Picks the port whose `descriptor` responds to `framebufferSurface` (or `ioSurface` on older Xcode) and whose `descriptor.state.displayClass == 0`.
  4. Calls `registerCallbackWithUUID:ioSurfacesChangeCallback:` and `registerCallbackWithUUID:damageRectanglesCallback:`, then counts damage callbacks for 5 s while you click in Simulator.app.
  5. Prints the IOSurface width, height and pixel format.

- [ ] **Step 2: S1 input probe (same session).** Extend it to `hid.swift`:
  1. Create `SimulatorKit.SimDeviceLegacyHIDClient(device:)` through `NSClassFromString("SimulatorKit.SimDeviceLegacyHIDClient")` and `initWithDevice:error:`.
  2. Build a touch-down and touch-up at ratio (0.5, 0.5) with `IndigoHIDMessageForMouseNSEvent`. Look it up with `dlsym`; its signature is in idb's `FBSimulatorIndigoHID.m`.
  3. Send each with `sendWithMessage:freeWhenDone:completionQueue:completion:`.

  Confirm a home-screen tap opens an app icon at the centre, and record the exact function signatures.
- [ ] **Step 3: S1 headless.** Run the `hid` binary from `ssh localhost` (System Settings → General → Sharing → Remote Login). Run it twice: once with a GUI session logged in, and once from a fresh SSH login with the console at the login window (log out of the GUI first). Record whether boot, callbacks and taps work in each case. If Remote Login can't be enabled, record "S1 headless: NOT CONFIRMED (no SSH)". The default stays "works", and Task 6's `viewOnlyHeadless` branch stays implemented but unreachable until confirmed.
- [ ] **Step 4: S2 rotation.** Try, in order, and record the first that changes the IOSurface to landscape:
  1. `-[SimDevice setOrientation:]` or a similar selector, found by listing `SimDevice`'s methods with `class_copyMethodList` and filtering for `orient` or `rotat`.
  2. `IndigoHIDMessageForHIDArbitrary` with a device-orientation usage.
  3. `-[SimDevice postDarwinNotification:error:]` of `com.apple.springboard.rotate`, if present in the method list.

  If none works, record "S2: no rotation". Task 8 then implements the disabled path.
- [ ] **Step 5: Write the results into spec §0.** Add a "Results (2026-10-15)" subsection listing:
  - every confirmed class, selector, C function and signature;
  - pixel format;
  - S1 and S2 outcomes.

  Commit it:
```bash
git add docs/superpowers/specs/2026-10-15-ios-simulator-previews-design.md
git commit -m "docs: record the iOS simulator spike results (#70)"
```

---

### Task 2: Backend traits and Android behind them (no behavior change)

**Files:**
- Create: `apps/server/src/device/backend.rs`, `apps/server/src/device/android/mod.rs`.
- Move (`git mv`): `device/{sdk,emulator,grpc,scrcpy}.rs` → `device/android/`; `device/testing.rs` → `device/android/testing.rs`.
- Modify: `device/mod.rs`, `device/frames.rs`, `device/media.rs`, `device/input.rs`, `production/connect_mcp/device_tools.rs`, `production/runtime.rs` (construction only), and `build.rs` if the proto path moves (keep the proto where it is; only the Rust module moves).

**Interfaces:**
- Produces, in `device/backend.rs`:

```rust
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;

use super::{DeviceError, DeviceOpError,
    catalog::{Availability, Image, Orientation, Platform},
    input::DeviceInputEvent, scrcpy_types::VideoUnit};

/// What a backend currently offers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlatformDiscovery {
    pub availability: Availability,
    pub images: Vec<Image>,
}

/// Why a launch did not reach `Ready`; mirrors today's `LaunchError`.
#[derive(Debug)]
pub(crate) enum LaunchFailure {
    Exited(String),
    BootTimeout,
    Spawn(String),
    Unresponsive(String),
    Cancelled,
    /// The image exists but is in use outside BibCode (`DeviceBusyError`).
    Busy,
}

/// One encoded JPEG from a device, before the hub numbers it.
pub(crate) struct RawJpeg { pub jpeg: Vec<u8>, pub width: u32, pub height: u32, pub orientation: Orientation }

/// A PNG for the MCP screenshot tool.
pub(crate) struct Png { pub png: Vec<u8>, pub width: u32, pub height: u32, pub orientation: Orientation }

pub(crate) trait DeviceBackend: Send + Sync + 'static {
    fn platform(&self) -> Platform;
    /// Fresh discovery; the manager caches it for 30 s.
    fn discover(&self) -> BoxFuture<'_, PlatformDiscovery>;
    /// Boots `image_id`, reporting `Booting` through `on_phase`. Stops what it started on
    /// error or cancel before returning.
    fn launch<'a>(
        &'a self,
        image_id: &'a str,
        on_phase: Box<dyn Fn(super::catalog::DeviceState) + Send + Sync>,
        cancel: &'a tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn RunningDevice>, LaunchFailure>>;
}

pub(crate) trait RunningDevice: Send + Sync + 'static {
    /// Native (portrait) display size.
    fn native_size(&self) -> (u32, u32);
    fn can_rotate(&self) -> bool;
    /// A stream of JPEGs at most `max_width` wide; the frame producer reopens it on end.
    fn jpeg_stream(&self, max_width: u32)
        -> BoxFuture<'_, Result<BoxStream<'static, Result<RawJpeg, DeviceError>>, DeviceError>>;
    fn png(&self, max_width: u32) -> BoxFuture<'_, Result<Png, DeviceError>>;
    /// Starts an H.264 source for the media hub.
    fn video(&self) -> BoxFuture<'_, Result<Box<dyn ErasedVideoSource>, DeviceError>>;
    /// Sends one validated batch in order. `orientation` is the catalog's current one.
    fn input(&self, events: Vec<DeviceInputEvent>, orientation: Orientation)
        -> BoxFuture<'_, Result<(), DeviceOpError>>;
    /// Turns toward `to`, calling `report` with each orientation and size the device
    /// reports, never with what was asked.
    fn rotate(&self, to: Orientation, report: &(dyn Fn(Orientation, u32, u32) + Send + Sync))
        -> BoxFuture<'_, Result<(), DeviceOpError>>;
    /// `Some(reason)` once the device ended without being asked; polled every second.
    fn exited(&self) -> Option<String>;
    /// Reports the unrequested exit to the profiler (`report_exit`, `Watch::Failures`).
    fn report_unrequested_exit(&self, reason: &str);
    /// Stops the device and waits until it is gone. Must be safe to run detached.
    fn stop(self: Box<Self>) -> BoxFuture<'static, ()>;
    #[cfg(test)]
    fn pid(&self) -> Option<u32> { None }
}

/// Object-safe form of `media::VideoSource`, implemented for every `VideoSource`.
pub(crate) trait ErasedVideoSource: Send {
    fn next_unit(&mut self) -> BoxFuture<'_, Option<VideoUnit>>;
    fn reset_video(&mut self) -> BoxFuture<'_, Result<(), DeviceError>>;
    fn failed(&self) -> bool;
    fn stop(self: Box<Self>, device_ended: bool) -> BoxFuture<'static, ()>;
}
```

`VideoUnit` and `UnitKind` move from `scrcpy.rs` to a neutral `device/video.rs` (create), so iOS doesn't depend on Android. `media::VideoSource` gets an `impl VideoSource for Box<dyn ErasedVideoSource>`, and a blanket `impl<S: VideoSource + Send + 'static> ErasedVideoSource for S`.

`DeviceManager` keeps its public API for callers. `open(avd_name)` becomes `open(platform, image_id)` in Task 4, not here. In this task the manager holds one `AndroidBackend`; the slot's `running: Option<RunningEmulator>` becomes `running: Option<Arc<dyn RunningDevice>>`. `client()` is deleted. Its callers move to these trait methods:
- `frames.rs` → `jpeg_stream`;
- `input.rs` → `RunningDevice::input`;
- `rotate` → `RunningDevice::rotate`;
- `media.rs attach_media` → `video()`;
- MCP screenshot → `png`.

`stop` takes the device out of its `Arc` with `Arc::try_unwrap`. The slot is the only owner while it is running; the frame, input and media tasks hold `Weak<dyn RunningDevice>` or clone the `Arc` only for one call. If an `Arc` is still shared when stopping, wait up to 5 s for it to become unique, polling every 50 ms, then log a `warn` and stop anyway through a shared `stop_shared(&self)`. The plan's choice is to keep `stop(self: Box<Self>)` and hold devices as `Arc<Box<dyn RunningDevice>>`. The task reviewer accepts any equivalent that keeps a dropped `close` cancel-safe (test `a_dropped_close_still_stops_the_device`).

- [ ] **Step 1: Move files and fix paths.** `git mv` the five Android files into `device/android/` and move `VideoUnit`/`UnitKind` into `device/video.rs`. Update the `use` paths. Run:
  - `cargo check -p bibcode-server --all-targets`. Expected: it compiles.
  - `cargo test -p bibcode-server --lib device::`. Expected: the same pass count as before the move.
  - Commit `refactor(server): move the Android device code into device/android (#70)`.
- [ ] **Step 2: Add `backend.rs` and the erased video source.** Write the traits above. Add this test in `media.rs` tests, then run `cargo test -p bibcode-server --lib device::media` and expect PASS:
```rust
#[tokio::test]
async fn an_erased_source_feeds_the_hub_like_the_concrete_one() {
    let (source, units) = ChannelSource::new();
    let erased: Box<dyn crate::device::backend::ErasedVideoSource> = Box::new(source);
    let hub = MediaHub::new(CancellationToken::new());
    let task = tokio::spawn(run(hub.clone(), async move { Ok(erased) }));
    units.send(config()).await.unwrap();
    units.send(key(1)).await.unwrap();
    // The existing helper that subscribes and reads two frames.
    let frames = read_frames(&hub, 2).await;
    assert_eq!(frames.len(), 2);
    hub.stop();
    task.await.unwrap();
}
```
- [ ] **Step 3: Implement `AndroidBackend` and `RunningAndroid` in `android/mod.rs`.**
  - `RunningAndroid` wraps `RunningEmulator`, the adb path, `InputState`, and the task tracker the input stream needs.
  - Its `jpeg_stream` wraps `EmulatorClient::stream_screenshot`. The per-frame RGB888 → JPEG conversion moves here from `frames.rs`'s `Producer::pump`.
  - Its `input` is today's `DeviceManager::input` loop body, moved verbatim.
  - Its `rotate` is today's rotate, with `report` in place of `set_reported_orientation`.
  - Its `video` is `ScrcpySession::start` boxed.
  - Its `exited` is `helper.exited().map(|status| format!("exited: {status}"))`.
  - Its `report_unrequested_exit` calls `helper.report_unrequested_exit(HELPER_LABEL, …)`.
  - Its `stop` calls `emulator::stop`.
  - `AndroidBackend::discover` maps `sdk::discover` to `PlatformDiscovery`, and `launch` wraps `EmulatorLauncher`, mapping `LaunchError` to `LaunchFailure`.
- [ ] **Step 4: Rewire the manager, frames, media, input and MCP onto the traits.** Delete `DeviceManager::client`. `frames::produce` takes `Weak<dyn RunningDevice>`-style access through a new `Inner::running(device_id) -> Result<(Arc<…>, Orientation), DeviceOpError>`. The test-only `pid()` uses `RunningDevice::pid`.
- [ ] **Step 5: Run all Android tests unchanged.**
  - `cargo test -p bibcode-server --lib device::` and `cargo test -p bibcode-server --test device_rpc --test device_media_socket`. Expected: every test that passed before Step 1 passes. Test code changes are limited to import paths and the removed `client()`. Tests that called `client()` read the same facts from the catalog or the fake controller instead, and the reviewer checks that no assertion was weakened.
  - Then run `cargo clippy -p bibcode-server --all-targets -- -D warnings`.
- [ ] **Step 6: Commit.**
```bash
git add -A apps/server
git commit -m "refactor(server): drive Android devices through a backend trait (#70)"
```

---

### Task 3: Fake backend and multi-platform manager

**Files:**
- Create: `apps/server/src/device/fake_backend.rs` (`#[cfg(test)]`).
- Modify: `device/mod.rs` (`backends: Vec<Arc<dyn DeviceBackend>>`, the per-platform limit, `Busy`), `device/catalog.rs` (`Platform`, `Image`, `PlatformCatalog`).

**Interfaces:**
- Produces:
  - `pub enum Platform { Android, Ios }`, serialized `"android"`/`"ios"`;
  - `pub struct Image { id: String, display_name: String, in_use_elsewhere: bool }`, where `inUseElsewhere` is omitted when false;
  - `pub struct PlatformCatalog { platform, availability, images }`;
  - `DeviceCatalog { platforms: Vec<PlatformCatalog>, devices }`;
  - `DeviceSnapshot { device_id, platform, image_id, state, failure, width, height, orientation, can_rotate }`;
  - `DeviceOpError::Busy { image_id: String }`, which maps to `{"_tag":"DeviceBusyError","imageId":…,"message":"This simulator is already running outside BibCode. Shut it down in Simulator, then open it here."}`;
  - `DeviceOpError::UnknownImage(String)`, renamed from `UnknownAvd`, with the same message for Android. For iOS: "No simulator with id \"{0}\" exists on the server. Choose one from the device list.";
  - `DeviceManager::open(&self, platform: Platform, image_id: &str)`;
  - `DeviceManagerConfig { backends: Vec<Arc<dyn DeviceBackend>>, registry, enabled }`. The Android-specific config moves into `AndroidBackend::new(env, discovery_dirs, registry, grpc_port)`.
- The fake backend's builder:

```rust
pub(crate) struct FakeBackend { /* platform, images, scripted launches, counters */ }
impl FakeBackend {
    pub(crate) fn new(platform: Platform, images: &[&str]) -> Arc<Self>;
    pub(crate) fn busy(self: &Arc<Self>, image: &str);          // launch -> LaunchFailure::Busy
    pub(crate) fn never_boots(self: &Arc<Self>);                 // launch waits for cancel
    pub(crate) fn exit_running(&self, device_index: usize, reason: &str); // flips exited()
    pub(crate) fn stops(&self) -> usize;                         // RunningDevice::stop calls
    pub(crate) fn discover_calls(&self) -> usize;
}
```

- [ ] **Step 1: Write failing manager tests in `mod.rs` that use `FakeBackend`.**
```rust
#[tokio::test]
async fn one_device_per_platform_runs_side_by_side() {
    let android = FakeBackend::new(Platform::Android, &["Pixel"]);
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1", "UDID-2"]);
    let (manager, _enabled) = fake_manager(vec![android.clone(), ios.clone()]);
    let a = manager.open(Platform::Android, "Pixel").await.unwrap();
    let i = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &a, DeviceState::Ready).await;
    wait_state(&manager, &i, DeviceState::Ready).await;
    assert!(matches!(
        manager.open(Platform::Ios, "UDID-2").await,
        Err(DeviceOpError::Limit { running }) if running == "UDID-1"
    ));
    manager.shutdown().await;
    assert_eq!((android.stops(), ios.stops()), (1, 1));
}

#[tokio::test]
async fn a_busy_image_is_refused_and_leaves_no_slot() {
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1"]);
    ios.busy("UDID-1");
    let (manager, _enabled) = fake_manager(vec![ios.clone()]);
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    let failed = wait_state(&manager, &id, DeviceState::Failed).await;
    assert_eq!(failed.failure.as_deref(), Some("in use outside BibCode"));
    assert_eq!(ios.stops(), 0);
}

#[tokio::test]
async fn an_unrequested_exit_fails_only_that_device() {
    let android = FakeBackend::new(Platform::Android, &["Pixel"]);
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1"]);
    let (manager, _enabled) = fake_manager(vec![android.clone(), ios.clone()]);
    let a = manager.open(Platform::Android, "Pixel").await.unwrap();
    let i = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &i, DeviceState::Ready).await;
    ios.exit_running(0, "Shutdown outside BibCode");
    let failed = wait_state(&manager, &i, DeviceState::Failed).await;
    assert_eq!(failed.failure.as_deref(), Some("Shutdown outside BibCode"));
    assert_eq!(wait_state(&manager, &a, DeviceState::Ready).await.state, DeviceState::Ready);
    manager.shutdown().await;
}

#[tokio::test]
async fn the_catalog_lists_each_platform_with_its_own_availability() {
    let android = FakeBackend::new(Platform::Android, &[]); // reports noAvds
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1"]);
    let (manager, _enabled) = fake_manager(vec![android, ios]);
    manager.refresh_now().await;
    let catalog = manager.catalog().borrow().clone();
    let kinds: Vec<_> = catalog.platforms.iter().map(|p| (p.platform, p.availability.kind)).collect();
    assert_eq!(kinds, vec![(Platform::Android, AvailabilityKind::NoAvds), (Platform::Ios, AvailabilityKind::Ready)]);
}

#[tokio::test]
async fn a_dropped_close_still_stops_a_fake_device() {
    use futures_util::FutureExt as _;
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1"]);
    let (manager, _enabled) = fake_manager(vec![ios.clone()]);
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &id, DeviceState::Ready).await;
    assert!(manager.close(&id).now_or_never().is_none());
    wait_state(&manager, &id, DeviceState::Stopped).await;
    assert_eq!(ios.stops(), 1);
}
```
A busy launch fails the slot with failure `"in use outside BibCode"`. The `device.open` RPC turns that into `DeviceBusyError` (Task 4), because the iOS backend checks busy before creating a slot (Task 5). The fake keeps the failure path covered.
- [ ] **Step 2: Run them.** `cargo test -p bibcode-server --lib device::tests`. Expected: FAIL (no `Platform`, no `FakeBackend`).
- [ ] **Step 3: Implement** the catalog types, `FakeBackend`, the per-platform limit (`slots.values().find(|s| s.snapshot.platform == platform && !ended(s.snapshot.state))`), per-backend discovery caches (`HashMap<Platform, Discovery>`), `LaunchFailure::Busy`, and `DeviceOpError::Busy`. Android tests construct the manager through a helper that builds `AndroidBackend` from the existing `FakeSdk`.
- [ ] **Step 4: Run** `cargo test -p bibcode-server --lib device::` and expect all tests to pass, Android included. Then run clippy.
- [ ] **Step 5: Commit** `feat(server): run one device per platform behind the device manager (#70)`.

---

### Task 4: Contracts, fixtures, RPC, MCP and web adapted to platforms (Android only)

**Files:**
- Modify: `packages/contracts/src/device.ts` and `device.test.ts`; `production/device_rpc.rs`; `production/connect_mcp/device_tools.rs` and its tests; `apps/server/tests/device_rpc.rs`; `apps/web/src/components/device/{DevicePanel,DeviceToolbar,DeviceScreen}.tsx` and tests; `deviceMessages.ts`.
- Regenerate: `packages/contracts/fixtures/rpc-wire/*` (`node packages/contracts/scripts/export-rust-rpc-fixtures.ts`).

**Interfaces:**
- Produces, in TypeScript:

```ts
export const DevicePlatform = Schema.Literals(["android", "ios"]);
export const DeviceAvailabilityKind = Schema.Literals([
  "ready", "sdkMissing", "adbMissing", "noAvds", "accelerationUnavailable",
  "xcodeMissing", "xcodeSetupIncomplete", "noSimulators", "xcodeUnsupported", "viewOnlyHeadless",
]);
export const DeviceImage = Schema.Struct({
  id: Schema.String,
  displayName: Schema.String,
  inUseElsewhere: Schema.optionalKey(Schema.Literal(true)),
});
export const DevicePlatformCatalog = Schema.Struct({
  platform: DevicePlatform,
  availability: Schema.Struct({ kind: DeviceAvailabilityKind, message: Schema.String }),
  images: Schema.Array(DeviceImage),
});
export const DeviceSnapshot = Schema.Struct({
  deviceId: Schema.String,
  platform: DevicePlatform,
  imageId: Schema.String,
  state: DeviceState,
  failure: Schema.optionalKey(Schema.String),
  width: Schema.optionalKey(Schema.Number),
  height: Schema.optionalKey(Schema.Number),
  orientation: DeviceOrientation,
  canRotate: Schema.Boolean,
});
export const DeviceCatalog = Schema.Struct({
  platforms: Schema.Array(DevicePlatformCatalog),
  devices: Schema.Array(DeviceSnapshot),
});
export const DeviceOpenInput = Schema.Struct({ platform: DevicePlatform, imageId: Schema.String });
// Added to the DeviceRpcError union:
export class DeviceBusyError extends Schema.TaggedErrorClass<DeviceBusyError>()("DeviceBusyError", {
  imageId: Schema.String,
  message: Schema.String,
}) {}
```
- The MCP `device_list` output becomes `{ platforms: [{ platform, availability, images }], devices }`. `device_open` takes `{ platform: "android" | "ios", imageId }`, and `platform` defaults to `"android"` when omitted, so existing agent prompts still work. That default is a documented input default, not a compatibility alias. Tool descriptions say "Android emulators and iOS simulators (macOS servers)".
- The web pane's selection key is `{ platform, imageId }`.

- [ ] **Step 1: Update `device.test.ts` (failing).** Add these cases, using the hoisted decoders pattern at module scope:
  - decoding a two-platform catalog;
  - `inUseElsewhere`;
  - `canRotate`;
  - `DeviceBusyError`;
  - rejecting a missing `platform`, and rejecting `avds`.

  Run `vp test run packages/contracts/src/device.test.ts --root packages/contracts`. Expected: FAIL.
- [ ] **Step 2: Change `device.ts`.** Run the contract test again and expect PASS.
- [ ] **Step 3: Update the Rust payloads.**
  - `device_rpc.rs` decodes `DeviceOpenInput { platform, image_id }` and calls `devices.open(platform, &image_id)`. `rpc_error` maps `Busy`.
  - In `connect_mcp/device_tools.rs`, `open` reads `platform` (default Android) and `imageId`, and the input schemas and descriptions change.
  - Update the expected JSON in `production/connect_mcp/tests.rs` and `tests/device_rpc.rs`.
- [ ] **Step 4: Regenerate fixtures and run the contracts gate.** Run `node packages/contracts/scripts/export-rust-rpc-fixtures.ts`, then `vp run check:contracts`. Expected: it passes with methods 158 and streams 24. Fingerprints and fixtures change; update the count assertions in `export-rust-rpc-fixtures.test.ts` and `apps/server/tests/rpc_wire.rs` only for typed-failure, fixture and fingerprint totals, which go up by one each for `DeviceBusyError`. Use the generator's printed numbers.
- [ ] **Step 5: Adapt the web pane to the new shape (Android only).**
  - `DevicePanel` reads `catalog.platforms` and flattens the images with their platform.
  - It opens with `{ platform, imageId }`.
  - It compares `device.imageId`.
  - It shows the platform's own availability message.

  `DeviceToolbar` takes `images: ReadonlyArray<{ platform; id; displayName; inUseElsewhere? }>` and `onSelectImage(platform, id)`. Update the existing tests' fixtures to the new shape. Expected behavior is unchanged for Android. Run `vp test run apps/web/src/components/device` and expect PASS.
- [ ] **Step 6: Run the gates.** `vp check`, `vp run typecheck`, `cargo test -p bibcode-server --lib production::connect_mcp`, `cargo test -p bibcode-server --test device_rpc`.
- [ ] **Step 7: Commit** `feat(contracts): describe devices per platform (#70)`.

---

### Task 5: iOS discovery and lifecycle with `simctl` (view-only end to end)

**Files:**
- Create: `device/ios/mod.rs`, `device/ios/simctl.rs`, `device/ios/guard.rs` (Xcode detection only here; the private probe comes in Task 6).
- Modify: `device/mod.rs` (`#[cfg(target_os = "macos")] pub(crate) mod ios;`), `production/runtime.rs` (on macOS, push `IosBackend` into `backends`).

**Interfaces:**
- Consumes: `DeviceBackend`, `RunningDevice`, `PlatformDiscovery`, `LaunchFailure` (Task 2); `Platform`, `Image` (Task 3).
- Produces:

```rust
pub(crate) struct SimctlRunner { xcrun: PathBuf, registry: ProcessAttributionRegistry }
impl SimctlRunner {
    pub(crate) async fn list(&self) -> Result<Vec<SimDevice>, SimctlError>;          // simctl list devices available -j
    pub(crate) async fn boot(&self, udid: &str, timeout: Duration) -> Result<(), SimctlError>; // boot + bootstatus -b
    pub(crate) async fn shutdown(&self, udid: &str) -> Result<(), SimctlError>;    // 30 s timeout
    pub(crate) async fn screenshot_jpeg(&self, udid: &str) -> Result<Vec<u8>, SimctlError>; // io <udid> screenshot --type=jpeg -
    pub(crate) async fn pbcopy(&self, udid: &str, text: &str) -> Result<(), SimctlError>;  // text on stdin
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SimDevice { pub udid: String, pub name: String, pub runtime: String /* "iOS 27.0" */, pub state: SimState }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SimState { Shutdown, Booted, Booting, ShuttingDown, Other }
pub(crate) fn parse_list(json: &[u8]) -> Result<Vec<SimDevice>, SimctlError>; // only iOS runtimes
pub(crate) fn display_name(device: &SimDevice) -> String; // "iPhone 17 Pro · iOS 27.0"
pub(crate) enum XcodeStatus { Missing, SetupIncomplete, Ready { developer_dir: PathBuf, version: String } }
pub(crate) async fn xcode_status(runner: &SimctlRunner) -> XcodeStatus;
```
Every `simctl` call runs through `process::supervised` with a timeout: 15 s for `list`, `screenshot` and `pbcopy`; the boot timeout for `boot` plus `bootstatus`; 30 s for `shutdown`. A failure carries the last stderr lines through `stderr_tail`.

`RunningSimulator` (view-only in this task):
- `jpeg_stream` polls `screenshot_jpeg` every 500 ms. It reads each JPEG's width and height from its SOF0 header with a small parser, `jpeg_size`, in `simctl.rs`.
- `png` returns `DeviceError::Protocol("PNG screenshots need live mode")` until Task 6.

  > The MCP tool falls back to the JPEG frame for iOS. Its result's `mimeType` is `image/jpeg`; `device_tools.rs` handles both.

- `video` returns an error that `attach_media` maps to `MediaAttachError::NotReady`. The client already falls back to JPEG.
- `input` returns `DeviceOpError::InvalidInput("Touch input is off: <reason>.")`.
- `can_rotate` is false.
- `exited` reads an `AtomicBool`/`Mutex<Option<String>>` set by a 2 s `list` poll task, which reports `"Shutdown outside BibCode"` when the state leaves `Booted`.
- `report_unrequested_exit` calls `diagnostics::profiler::exits::report_exit` with `Watch::Failures` and label `ios-simulator`.
- `stop` cancels the poll and runs `shutdown`.

Attribution: after boot, find the `launchd_sim` PID whose command line contains the UDID, using `sysinfo` (already a dependency). Register it with `ProcessAttributionRegistry::register_identity` under the `ios-simulator` identity, and unregister it on stop.

Busy: `IosBackend::launch` lists first. If the UDID's state is not `Shutdown`, it returns `LaunchFailure::Busy`. `DeviceManager::open` checks busy **before** making a slot: it calls a new `DeviceBackend::check_available(&self, image_id) -> BoxFuture<Result<(), LaunchFailure>>`, whose default is `Ok(())`. Android keeps the default. So `device.open` returns `DeviceBusyError` synchronously. Add that method to the trait in this task and update `FakeBackend::busy` to use it, changing Task 3's `a_busy_image…` test to expect `Err(DeviceOpError::Busy { image_id })` from `open`.

- [ ] **Step 1: Write failing pure tests in `simctl.rs`.** Use a recorded fixture at `device/ios/fixtures/simctl-list.json`, captured on this Mac with `xcrun simctl list devices available -j`. Trim it to 3 devices across 2 runtimes, plus one watchOS device that must be filtered out.
```rust
#[test]
fn parses_ios_devices_and_skips_other_platforms() {
    let devices = parse_list(include_bytes!("fixtures/simctl-list.json")).unwrap();
    assert!(devices.iter().all(|d| d.runtime.starts_with("iOS ")));
    let pro = devices.iter().find(|d| d.udid == "308FD3CD-F46A-44B9-9B68-9A849F337C80").unwrap();
    assert_eq!(display_name(pro), "iPhone 17 Pro · iOS 27.0");
    assert_eq!(pro.state, SimState::Shutdown);
}

#[test]
fn reads_jpeg_dimensions_from_sof0() {
    let rgb = vec![0x80u8; 1206 * 2622 * 3];
    let jpeg = crate::device::frames::encode_rgb888(&rgb, 1206, 2622).unwrap();
    assert_eq!(jpeg_size(&jpeg), Some((1206, 2622)));
    assert_eq!(jpeg_size(b"not a jpeg"), None);
}

#[test]
fn maps_xcode_failures_to_availability() {
    assert_eq!(availability_for(&XcodeStatus::Missing, &[]).kind, AvailabilityKind::XcodeMissing);
    assert_eq!(availability_for(&XcodeStatus::SetupIncomplete, &[]).kind, AvailabilityKind::XcodeSetupIncomplete);
    let ready = XcodeStatus::Ready { developer_dir: "/x".into(), version: "27.0".into() };
    assert_eq!(availability_for(&ready, &[]).kind, AvailabilityKind::NoSimulators);
    assert_eq!(
        availability_for(&ready, &[]).message,
        "Create a simulator in Xcode → Window → Devices and Simulators."
    );
}
```
- [ ] **Step 2: Run** `cargo test -p bibcode-server --lib device::ios`. Expected: FAIL.
- [ ] **Step 3: Implement** `simctl.rs`, `guard.rs` (`xcode_status` runs `xcode-select -p` and `xcrun simctl help`; a license or first-launch message in stderr gives `SetupIncomplete`), and `IosBackend`/`RunningSimulator` in view-only mode as described above.
- [ ] **Step 4: Add the external-exit test, using a fake `xcrun` script.** In the `#[cfg(test)]` helper `ios/testing.rs`, write a shell script into a tempdir that answers `list`, `boot`, `bootstatus`, `shutdown` and `io … screenshot` from files. The pattern is the same as `android/testing.rs` `FakeSdk`. The test then:
```rust
#[tokio::test]
async fn unrequested_shutdown_fails_the_device() {
    let sim = FakeXcrun::new().device("UDID-1", "iPhone 17 Pro", "iOS 27.0").write();
    let (manager, _enabled) = ios_manager(&sim);
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &id, DeviceState::Ready).await;
    sim.set_state("UDID-1", "Shutdown");
    let failed = wait_state(&manager, &id, DeviceState::Failed).await;
    assert_eq!(failed.failure.as_deref(), Some("Shutdown outside BibCode"));
    assert!(sim.calls().iter().all(|c| !c.starts_with("shutdown")), "BibCode did not shut it down");
}

#[tokio::test]
async fn a_simulator_booted_elsewhere_is_listed_and_refused() {
    let sim = FakeXcrun::new().device("UDID-1", "iPhone 17 Pro", "iOS 27.0").state("UDID-1", "Booted").write();
    let (manager, _enabled) = ios_manager(&sim);
    manager.refresh_now().await;
    let catalog = manager.catalog().borrow().clone();
    assert!(catalog.platforms[0].images[0].in_use_elsewhere);
    assert!(matches!(manager.open(Platform::Ios, "UDID-1").await, Err(DeviceOpError::Busy { .. })));
    assert!(sim.calls().iter().all(|c| !c.starts_with("boot")));
}

#[tokio::test]
async fn close_shuts_down_only_what_bibcode_booted() {
    let sim = FakeXcrun::new().device("UDID-1", "iPhone 17 Pro", "iOS 27.0").write();
    let (manager, _enabled) = ios_manager(&sim);
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &id, DeviceState::Ready).await;
    manager.close(&id).await.unwrap();
    assert_eq!(sim.calls().iter().filter(|c| c.starts_with("shutdown UDID-1")).count(), 1);
}
```
- [ ] **Step 5: Run** `cargo test -p bibcode-server --lib device::` and expect PASS. Then run clippy, plus a non-macOS compile check. If the target is installed, use `cargo check -p bibcode-server --target x86_64-unknown-linux-gnu`; otherwise rely on CI and note "not run locally".
- [ ] **Step 6: Commit** `feat(server): list, boot and stop iOS simulators on macOS (#70)`.

---

### Task 6: CoreSimulator loader, private-API guard and live JPEG frames

> **Amended after Task 1 (spike results, spec §0 and amended §4).** These amendments override the text below wherever they differ.
> - `Screen::attach` registers the `SimScreen` callbacks instead of the damage and surface-change ones:
>   - `frameCallback` sends `()` on `damage`;
>   - `surfacesChanged` replaces the surface;
>   - `propertiesChanged` publishes `uiOrientation` on a `watch::Sender<Orientation>`. Add `Screen::orientation() -> watch::Receiver<Orientation>`.
> - `read_bgra` honours the surface's bytes-per-row (4864 on the iPhone 17 Pro, not width × 4). It returns pixels **rotated into the current UI orientation**, so landscape returns width > height.
> - The device reports orientation changes to the manager through `RunningDevice::rotate`'s `report` path or an equivalent watch the manager follows. The catalog shows `uiOrientation`.
> - Every Objective-C send to a proxy is gated by `conformsToProtocol:`/`respondsToSelector:` and wrapped in `objc2::exception::catch`. Add a test that the guard reports `xcodeUnsupported` when a gating check fails.
> - Name `IosOrientation::from_ui(u32)` and test it for values 1–4. Map portrait=1 and reversePortrait=2. Task 8's real-simulator test confirms which of 3 and 4 is `landscape`; until then use 3 → `landscape` and 4 → `reverseLandscape`, with a comment pointing at Task 8.

**Files:**
- Create: `device/ios/coresim.rs`.
- Modify: `device/ios/guard.rs`, `device/ios/mod.rs`, `apps/server/Cargo.toml` (macOS deps), `docs/dependency-upgrades/2026-07-17-ledger.json`, `Cargo.lock`.

**Interfaces:**
- Consumes: the class and selector names from spec §0 "Results" (Task 1).
- Produces:

```rust
/// Loaded frameworks and resolved symbols; `None` fields never happen after `load` succeeds.
pub(crate) struct CoreSim { /* dlopen handles, Class pointers, fn pointers */ }
pub(crate) enum GuardResult { Live(Arc<CoreSim>), ViewOnly { kind: AvailabilityKind, missing: String } }
pub(crate) fn probe(developer_dir: &Path) -> GuardResult;           // never panics
pub(crate) struct GuardCache { /* (developer_dir, version) -> GuardResult */ }
impl GuardCache { pub(crate) fn get(&self, status: &XcodeStatus) -> GuardResult; }

pub(crate) struct Screen { /* retained SimDevice, port descriptor, callback UUID */ }
impl Screen {
    /// Registers the surface-change and damage callbacks on a private serial queue. Each
    /// damage event sends `()` on `damage` (capacity 1, `try_send`); a surface change
    /// replaces the current surface under a mutex.
    pub(crate) fn attach(sim: &CoreSim, udid: &str, damage: tokio::sync::mpsc::Sender<()>) -> Result<Self, DeviceError>;
    /// The current surface as BGRA bytes scaled to `max_width`, with its size.
    pub(crate) fn read_bgra(&self, max_width: u32) -> Result<(Vec<u8>, u32, u32), DeviceError>;
    /// The current surface retained, for the encoder (Task 7).
    pub(crate) fn surface(&self) -> Option<IoSurfaceRef>;
    pub(crate) fn size(&self) -> (u32, u32);
}
impl Drop for Screen { /* unregister both callbacks, then release */ }
```

Dependencies (macOS-only, exact pins; choose the newest versions released at least 14 days before the task date, and use matching minor versions across the family):
- `objc2`
- `objc2-foundation`
- `block2`
- `objc2-io-surface`
- `objc2-core-video`
- `objc2-core-media`
- `objc2-video-toolbox`
- `dispatch2`

Use `libc::dlopen`/`dlsym` (`libc` is already a dependency).

`unsafe` rules for `coresim.rs`:
- every `unsafe` block has a `// SAFETY:` comment;
- every Objective-C object is held in `Retained<…>`;
- every callback block is `'static` and captures only `Arc`/`Sender` values;
- nothing in a callback blocks or calls into Tokio except `try_send`.

- [ ] **Step 1: Write failing guard tests.** These pure tests use an injected symbol resolver:
```rust
#[test]
fn a_missing_selector_gives_view_only_with_its_name() {
    let resolver = FakeResolver::complete().without_selector("registerCallbackWithUUID:damageRectanglesCallback:");
    match probe_with(&resolver) {
        GuardResult::ViewOnly { kind, missing } => {
            assert_eq!(kind, AvailabilityKind::XcodeUnsupported);
            assert_eq!(missing, "registerCallbackWithUUID:damageRectanglesCallback:");
        }
        GuardResult::Live(_) => panic!("must not go live"),
    }
}

#[test]
fn guard_reprobes_when_xcode_changes() {
    let cache = GuardCache::with_prober(counting_prober());
    let a = XcodeStatus::Ready { developer_dir: "/X1".into(), version: "27.0".into() };
    let b = XcodeStatus::Ready { developer_dir: "/X1".into(), version: "27.1".into() };
    cache.get(&a); cache.get(&a); cache.get(&b);
    assert_eq!(cache.probes(), 2);
}

#[test]
fn unsupported_message_names_the_xcode_version() {
    assert_eq!(
        unsupported_message("27.1"),
        "Xcode 27.1 changed the simulator interfaces BibCode uses. Simulators open as screenshots without touch input."
    );
}
```
`probe` is `probe_with(&DlsymResolver)`. `FakeResolver` implements a `SymbolResolver` trait with `class(name)`, `responds(class, selector)` and `function(name)`.
- [ ] **Step 2: Run** `cargo test -p bibcode-server --lib device::ios::guard` and expect FAIL.
- [ ] **Step 3: Implement `coresim.rs`** with the §0 names, plus `guard.rs`'s `SymbolResolver`, `probe` and `GuardCache`. `IosBackend::discover` reports the guard's view-only kind and message when it isn't `Live`. If the guard is `Live`, `RunningSimulator::jpeg_stream` changes:
  - it attaches a `Screen`;
  - on each damage event, at most every 66 ms, it reads BGRA, converts to RGB and encodes with `frames::encode_rgb888`, yielding `RawJpeg`;
  - with no damage for 1 s it re-yields the last frame, so a new subscriber gets a picture.

  `png` uses the same BGRA buffer through `png` encoding, if the `png` crate is already in `Cargo.lock`; otherwise return the JPEG and set the MCP `mimeType` as in Task 5.
- [ ] **Step 4: Add a real-simulator test (opt-in).** In `apps/server/tests/ios_simulator.rs`, mark each test `#[cfg(target_os = "macos")]` and skip it unless `BIBCODE_IOS_SIM_TESTS=1`:
```rust
#[tokio::test]
async fn live_frames_arrive_from_a_booted_simulator() {
    if std::env::var("BIBCODE_IOS_SIM_TESTS").as_deref() != Ok("1") { return; }
    let harness = IosHarness::start().await; // picks the first Shutdown iPhone; boots it
    let frame = harness.first_jpeg(std::time::Duration::from_secs(20)).await;
    assert!(frame.width >= 300 && frame.height > frame.width);
    harness.stop().await; // asserts simctl reports Shutdown afterwards
}
```
Run `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator -- --nocapture` on this Mac. Expected: PASS. Paste the output into the task report.
- [ ] **Step 5: Run the gates.** `cargo test -p bibcode-server --lib device::`, clippy, `vp run check:dependency-ledger`.
- [ ] **Step 6: Commit** `feat(server): read iOS simulator frames through CoreSimulator (#70)`.

---

### Task 7: VideoToolbox H.264 source

> **Amended after Task 1.** These amendments override the text below wherever they differ.
> - The surface never changes size on rotation. Before encoding, rotate each `CVPixelBuffer` into the current UI orientation with `VTPixelRotationSession`, using a pixel-buffer pool at the rotated size.
> - The encoder's output size is the rotated size, so an orientation change is a size change. It rebuilds the session, then emits config before key: `size_change_emits_config_before_frames` still pins it.
> - Frames are paced by `frameCallback` (Task 6's `damage` channel), capped at 60 fps.

**Files:**
- Create: `device/ios/encoder.rs`, `device/ios/fixtures/vt-*.bin` (recorded encoder output).
- Modify: `device/ios/mod.rs` (`RunningSimulator::video`).

**Interfaces:**
- Consumes: `Screen::surface`, `Screen::size`, damage events (Task 6); `VideoUnit`/`UnitKind` (`device/video.rs`); `ErasedVideoSource` (Task 2).
- Produces:

```rust
/// Pure: AVCC sample (4-byte big-endian lengths) to Annex B.
pub(crate) fn avcc_to_annex_b(sample: &[u8]) -> Result<Vec<u8>, DeviceError>;
/// Pure: SPS and PPS to one Annex B config unit.
pub(crate) fn config_unit(sps: &[u8], pps: &[u8]) -> VideoUnit;
/// Pure: bitrate for a pixel count (≈ 0.1 bit/px at 60 fps, clamped 2–12 Mbit/s).
pub(crate) fn bitrate_for(width: u32, height: u32) -> u32;

pub(crate) struct SimulatorVideo { /* Screen handle, VTCompressionSession, mpsc::Receiver<VideoUnit>, force_key flag */ }
impl crate::device::media::VideoSource for SimulatorVideo { /* next_unit / reset_video sets force_key / failed / stop */ }
```
Session settings:
- `kVTCompressionPropertyKey_RealTime = true`;
- `AllowFrameReordering = false`;
- `MaxKeyFrameInterval = 120` and `MaxKeyFrameIntervalDuration = 2.0`;
- `ProfileLevel = H264_Main_AutoLevel`;
- `AverageBitRate = bitrate_for(w, h)`.

On each damage event (at most one per 16 ms), wrap the current IOSurface with `CVPixelBufferCreateWithIOSurface` and call `VTCompressionSessionEncodeFrame`. Pass `kVTEncodeFrameOptionKey_ForceKeyFrame` when `force_key` is set. The output callback runs on VideoToolbox's thread and does the following, using `try_send` into a channel of capacity 8; when that fails it sets `force_key` and drops the frame:
- a key sample with format-description changes first emits `config_unit`;
- every sample emits `Key` or `Delta`, with `pts_us` from the presentation timestamp.

A surface size change invalidates the session and builds a new one at the new size. The next frame is then a config unit followed by a key frame.

- [ ] **Step 1: Write failing pure tests.**
```rust
#[test]
fn avcc_lengths_become_start_codes() {
    let avcc = [0, 0, 0, 2, 0x65, 0x88, 0, 0, 0, 1, 0x41];
    assert_eq!(avcc_to_annex_b(&avcc).unwrap(), vec![0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1, 0x41]);
    assert!(avcc_to_annex_b(&[0, 0, 0, 9, 0x65]).is_err());
}

#[test]
fn config_unit_carries_sps_then_pps() {
    let unit = config_unit(&[0x67, 1], &[0x68, 2]);
    assert_eq!(unit.kind, UnitKind::Config);
    assert_eq!(&unit.data[..], &[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]);
}

#[test]
fn bitrate_scales_and_clamps() {
    assert_eq!(bitrate_for(100, 100), 2_000_000);
    assert_eq!(bitrate_for(400, 800), 2_000_000); // 1.92 Mbit/s, clamped up
    assert_eq!(bitrate_for(1000, 1000), 6_000_000); // 0.1 bit/px × 60 fps
    assert_eq!(bitrate_for(1206, 2622), 12_000_000); // 18.97 Mbit/s, clamped down
    assert_eq!(bitrate_for(4000, 4000), 12_000_000);
}

#[test]
fn size_change_emits_config_before_frames() {
    // Pure state machine extracted from the callback: `EncoderState::on_sample`.
    let mut state = EncoderState::default();
    let first = state.on_sample(Sample::key((1206, 2622), b"sps1", b"pps1", b"idr"));
    assert_eq!(kinds(&first), [UnitKind::Config, UnitKind::Key]);
    let delta = state.on_sample(Sample::delta((1206, 2622), b"p"));
    assert_eq!(kinds(&delta), [UnitKind::Delta]);
    let rotated = state.on_sample(Sample::key((2622, 1206), b"sps2", b"pps2", b"idr"));
    assert_eq!(kinds(&rotated), [UnitKind::Config, UnitKind::Key]);
}
```
- [ ] **Step 2: Run** `cargo test -p bibcode-server --lib device::ios::encoder` and expect FAIL.
- [ ] **Step 3: Implement** the pure functions, `EncoderState`, the VideoToolbox session and `SimulatorVideo`. `RunningSimulator::video` returns `Box::new(SimulatorVideo::start(...)?)` when the guard is `Live`, and otherwise the Task 5 error.
- [ ] **Step 4: Run the real-simulator test (opt-in).** Add to `tests/ios_simulator.rs`:
```rust
#[tokio::test]
async fn first_key_frame_decodes_to_the_screen_size() {
    if std::env::var("BIBCODE_IOS_SIM_TESTS").as_deref() != Ok("1") { return; }
    let harness = IosHarness::start().await;
    let units = harness.first_video_units(std::time::Duration::from_secs(20)).await; // via attach_media
    assert_eq!(units[0].kind, UnitKind::Config);
    assert_eq!(units[1].kind, UnitKind::Key);
    let (w, h) = sps_dimensions(&units[0].data).unwrap(); // small SPS parser in the test helper
    assert_eq!((w, h), harness.screen_size());
    harness.stop().await;
}
```
Run it with `BIBCODE_IOS_SIM_TESTS=1` and expect PASS.
- [ ] **Step 5: Run** `cargo test -p bibcode-server --lib device::` and `--test device_media_socket` (unchanged), then clippy.
- [ ] **Step 6: Commit** `feat(server): stream iOS simulators as H.264 through VideoToolbox (#70)`.

---

### Task 8: iOS input, text and rotation

> **Amended after Task 1 (spec §0 and amended §5).** These amendments override the text below wherever they differ.
> - **Send path is DTUHID first.** `HidClient` becomes a `DtuHid` connection:
>   - `-[SimDevice lookup:error:]` for `com.apple.coredevice.feature.remote.hid.digitizer`;
>   - `xpc_endpoint_create_mach_port_4sim`, then `xpc_connection_create_from_endpoint`, then `xpc_connection_enable_sim2host_4sim` before resume;
>   - messages `IndigoDigitizerEvent`, `IndigoButtonEvent` and `IndigoKeyboardButtonEvent` with §0's payload keys;
>   - the barrier liveness check (5 attempts × 4 s reply timeout × 4 s backoff).
>
>   The legacy `SimDeviceLegacyHIDClient` Indigo path is the fallback for touch and buttons only. With no DTUHID, keys and text return `DeviceOpError::InvalidInput("The simulator is not accepting keyboard input yet. Try again in a few seconds.")`.
> - **Buttons** are `IndigoButtonEvent` on usage page 0x0C: Home 0x40, Lock 0x30, Volume up 0xE9, Volume down 0xEA.
> - **Touch ratios** are mapped into portrait-surface space for the current orientation (§0: at `uiOrientation` 3, (u, v) → (v, 1 − u)). Replace the `landscape_ratios_map_to_portrait_space` expectation with the value that rule gives for whichever orientation Task 6 mapped to `landscape`, and add one case per orientation.
> - **Rotation is supported.** Send the Purple GSEvent (`PurpleWorkspacePort`, `msgh_id` 0x7B, type `50 | 0x20000`, the record at 0x48/0x4C, `mach_msg` with a 2000 ms send timeout). `can_rotate()` is `true`.
>   - `rotate_left`/`right` map onto Orientation values. A new real-simulator test, `rotation_matches_android_convention`, sends `landscape`, checks the reported orientation and frame size, and fixes the 3/4 mapping in both `IosOrientation::from_ui` and the Purple value table if Task 6's provisional choice was wrong.
>   - Drop `rotation_changes_the_screen_size`'s "only if S2 found a call" condition.
> - Add a pure test of the DTUHID message builders: keys and nested payload types (u64/double/bool/string) for each message, built as a Rust-side description before conversion to `xpc_object_t`.

**Files:**
- Create: `device/ios/hid.rs`, `device/ios/keymap.rs`.
- Modify: `device/ios/coresim.rs` (`HidClient`), `device/ios/mod.rs` (`input`, `rotate`, `can_rotate`).

**Interfaces:**
- Consumes: the Indigo function signatures and the S2 result from spec §0; `SimctlRunner::pbcopy` (Task 5); `input::validate` (Task 2).
- Produces:

```rust
/// What to send for one validated event; pure.
#[derive(Debug, PartialEq)]
pub(crate) enum HidAction {
    Touch { phase: TouchPhase, x: f64, y: f64 },       // ratios in portrait space
    Button { button: SimButton, down: bool },
    Key { usage: u32, down: bool },                      // HID keyboard usage page 0x07
    Paste { text: String },                              // pbcopy, then Cmd+V
}
#[derive(Debug, PartialEq, Clone, Copy)] pub(crate) enum TouchPhase { Down, Move, Up }
#[derive(Debug, PartialEq, Clone, Copy)] pub(crate) enum SimButton { Home, Lock, VolumeUp, VolumeDown }

/// Tracks the single active pointer between batches.
#[derive(Default)] pub(crate) struct PointerState { active: Option<u32> }

pub(crate) fn plan_events(
    events: &[DeviceInputEvent], orientation: Orientation, pointer: &mut PointerState,
) -> Result<Vec<HidAction>, DeviceOpError>;
pub(crate) fn us_usage(c: char) -> Option<(u32, bool /* shift */)>;  // keymap.rs
pub(crate) struct HidClient { /* retained SimDeviceLegacyHIDClient */ }
impl HidClient {
    pub(crate) fn new(sim: &CoreSim, device: &SimDeviceRef) -> Result<Self, DeviceError>;
    pub(crate) async fn send(&self, action: &HidAction) -> Result<(), DeviceError>; // Paste handled by caller
}
```

Mapping rules:
- **Touch.** Ratios are rotated into portrait space for the current orientation (see `input::to_device_pixels` for the axis rules, using ratios instead of pixels). `Move`/`Up`/`Cancel` from a non-active pointer are dropped, as is a second `Down` while one pointer is active. `Cancel` maps to `Up`.
- **Keys.**
  - `home`, `power`, `volumeUp` and `volumeDown` map to `Button`.
  - `enter` is usage 0x28, `backspace` 0x2A, `delete` 0x4C and `tab` 0x2B. The arrows are 0x4F (right), 0x50 (left), 0x51 (down) and 0x52 (up).
  - `back` and `appSwitch` return `InvalidInput("iOS has no Back key.")` and `InvalidInput("iOS has no Recents key.")`.
- **Text** is split into maximal runs: US-typeable characters (`us_usage` is `Some`) become key down/up pairs with Shift (usage 0xE1) around shifted characters. Other runs become `Paste`. The runs keep their order.
- **Paste.** `pbcopy`, then Cmd (0xE3) down, V (0x19) down, V up, Cmd up.
- **Input in view-only mode** returns `InvalidInput(format!("Touch input is off: {reason}."))`. The reason comes from the availability message without its trailing period.

Rotation: if S2 found a call, `rotate` invokes it. It then polls `Screen::size` five times, 300 ms apart, and calls `report` with the derived orientation and size. Otherwise `rotate` returns `InvalidInput("Rotation isn't available for iOS simulators yet.")` and `can_rotate()` is false.

- [ ] **Step 1: Write failing pure tests in `hid.rs`.**
```rust
fn touch(action: TouchAction, id: u32, x: f64, y: f64) -> DeviceInputEvent {
    DeviceInputEvent::Touch { action, pointer_id: id, x, y }
}

#[test]
fn only_the_first_pointer_is_sent() {
    let mut pointer = PointerState::default();
    let plan = plan_events(&[
        touch(TouchAction::Down, 0, 0.5, 0.5),
        touch(TouchAction::Down, 1, 0.2, 0.2),
        touch(TouchAction::Move, 1, 0.3, 0.3),
        touch(TouchAction::Up, 0, 0.5, 0.6),
    ], Orientation::Portrait, &mut pointer).unwrap();
    assert_eq!(plan, vec![
        HidAction::Touch { phase: TouchPhase::Down, x: 0.5, y: 0.5 },
        HidAction::Touch { phase: TouchPhase::Up, x: 0.5, y: 0.6 },
    ]);
}

#[test]
fn landscape_ratios_map_to_portrait_space() {
    let mut pointer = PointerState::default();
    let plan = plan_events(&[touch(TouchAction::Down, 0, 0.25, 0.5)], Orientation::Landscape, &mut pointer).unwrap();
    // Same axis rule as input::to_device_pixels for Landscape, in ratios.
    assert_eq!(plan, vec![HidAction::Touch { phase: TouchPhase::Down, x: 0.5, y: 0.75 }]);
}

#[test]
fn mixed_text_is_split_in_order() {
    let mut pointer = PointerState::default();
    let plan = plan_events(&[DeviceInputEvent::Text { text: "Hi é😀!".into() }], Orientation::Portrait, &mut pointer).unwrap();
    let paste_at = plan.iter().position(|a| matches!(a, HidAction::Paste { .. })).unwrap();
    assert_eq!(plan[paste_at], HidAction::Paste { text: "é😀".into() });
    // "Hi " before the paste (H needs shift), "!" after it.
    assert!(plan[..paste_at].contains(&HidAction::Key { usage: 0xE1, down: true }));
    assert_eq!(plan.last(), Some(&HidAction::Key { usage: 0xE1, down: false }));
}

#[test]
fn android_only_keys_are_refused() {
    let mut pointer = PointerState::default();
    let back = DeviceInputEvent::Key { key: DeviceKey::Back, action: KeyAction::Down };
    assert!(matches!(
        plan_events(&[back], Orientation::Portrait, &mut pointer),
        Err(DeviceOpError::InvalidInput(m)) if m == "iOS has no Back key."
    ));
}

#[test]
fn us_layout_covers_letters_digits_and_shifted_symbols() {
    assert_eq!(us_usage('a'), Some((0x04, false)));
    assert_eq!(us_usage('A'), Some((0x04, true)));
    assert_eq!(us_usage('1'), Some((0x1E, false)));
    assert_eq!(us_usage('!'), Some((0x1E, true)));
    assert_eq!(us_usage(' '), Some((0x2C, false)));
    assert_eq!(us_usage('é'), None);
}
```
Also add to `ios/mod.rs` tests, using the fake xcrun plus a `HidSink` trait injected in place of `HidClient` for tests:
```rust
#[tokio::test]
async fn input_after_stop_is_refused_without_ffi() {
    let (manager, sim, hid) = live_fake_ios().await; // fake xcrun + recording HidSink
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &id, DeviceState::Ready).await;
    manager.close(&id).await.unwrap();
    let tap = vec![DeviceInputEvent::Touch { action: TouchAction::Down, pointer_id: 0, x: 0.5, y: 0.5 }];
    assert!(matches!(manager.input(&id, tap).await, Err(DeviceOpError::NotReady(DeviceState::Stopped))));
    assert_eq!(hid.sent_after_release(), 0);
    drop(sim);
}
```
- [ ] **Step 2: Run** `cargo test -p bibcode-server --lib device::ios` and expect FAIL.
- [ ] **Step 3: Implement** `keymap.rs`, `hid.rs`, `HidClient` (with §0's signatures), and `RunningSimulator::input` (validate, plan, send in order, `Paste` through `pbcopy` then the key sequence), plus `rotate` and `can_rotate`. Log send failures at `warn` without text content. They map to `DeviceOpError::Emulator("The simulator did not accept the input. Try again; if it keeps failing, stop the device and start it again.")`.
- [ ] **Step 4: Run the real-simulator tests (opt-in).** Add:
  - `a_tap_changes_the_screen`: tap the Settings icon position, then wait up to 5 s for the JPEG to differ by more than 5% of pixels;
  - `text_arrives_in_a_text_field`: open `simctl openurl <udid> https://example.com` in Safari, tap the address bar, type `bibcode`, and check that a screenshot differs. If an OCR-free check is impossible, assert the frame changed;
  - `rotation_changes_the_screen_size`, only if S2 found a call;
  - `external_shutdown_fails_device`: run `xcrun simctl shutdown <udid>` from the test, and expect the device `failed` within 5 s and the media socket closed with `stopped`.

  Run them with `BIBCODE_IOS_SIM_TESTS=1`.
- [ ] **Step 5: Run the gates.** `cargo test -p bibcode-server --lib device::`, clippy.
- [ ] **Step 6: Commit** `feat(server): send touch, keys and text to iOS simulators (#70)`.

---

### Task 9: Client — grouped picker, per-platform toolbar, view-only notice

**Files:**
- Modify: `apps/web/src/components/device/DevicePanel.tsx`, `DeviceToolbar.tsx`, `DeviceScreen.tsx`, `deviceMessages.ts`, `DevicePanel.test.tsx`, `DeviceScreen.test.tsx`, `apps/web/src/components/settings/DevicePreviewsSection.tsx` and its test.

**Interfaces:**
- Consumes: the Task 4 contract shapes.
- Produces: no new exports beyond the props named below.

Behavior:
- **Picker.** The picker (`DeviceToolbar` `Select`) renders a `SelectGroup` per platform, labelled **Android** or **iOS**, in that order, showing only platforms present in the catalog.
  - A platform that isn't ready (other than `xcodeUnsupported` and `viewOnlyHeadless`, which still list images) shows its availability message as a disabled item under its heading.
  - `inUseElsewhere` images are disabled items with the suffix " — Running outside BibCode".
- **Toolbar.** For `platform === "ios"` it shows Home, Lock (`power`), Volume up, Volume down, Rotate and Paste, but not Back or Recents. Android is unchanged.
  - When `device.canRotate === false`, Rotate shows `disabledReason` "Rotation isn't available for iOS simulators yet".
- **View-only notice.** When the device's platform availability kind is `xcodeUnsupported` or `viewOnlyHeadless`, `DeviceScreen` gets `inputDisabledReason={availability.message}`. It renders one line above the canvas with that message, sets `cursor: default`, and sends no pointer, key or paste input.
- **Selection.** The pane's selected image is `{ platform, imageId }`. The shown device is the running device for the selected image; if there is none, it's the newest non-ended device.
- **Setting.** The description in `DevicePreviewsSection` becomes exactly "Android emulators and, on macOS servers, iOS simulators."

- [ ] **Step 1: Write failing tests.** Follow the existing `DevicePanel.test.tsx` render helpers and catalog fixtures, and extend the fixture builder with `platforms`. Add:
```tsx
it("groups images by platform and keeps an unavailable platform's message", async () => {
  renderPanel(catalog({
    platforms: [
      platform("android", "sdkMissing", "Install the Android SDK on the server.", []),
      platform("ios", "ready", "Ready.", [image("UDID-1", "iPhone 17 Pro · iOS 27.0")]),
    ],
  }));
  await openPicker();
  expect(screen.getByRole("group", { name: "Android" })).toHaveTextContent("Install the Android SDK on the server.");
  expect(screen.getByRole("option", { name: "iPhone 17 Pro · iOS 27.0" })).toBeEnabled();
});

it("disables a simulator running outside BibCode", async () => {
  renderPanel(catalog({ platforms: [platform("ios", "ready", "Ready.", [image("UDID-1", "iPhone 17 Pro · iOS 27.0", true)])] }));
  await openPicker();
  expect(screen.getByRole("option", { name: /Running outside BibCode/ })).toHaveAttribute("aria-disabled", "true");
});

it("shows iOS buttons and explains a disabled Rotate", async () => {
  renderPanel(readyIosDevice({ canRotate: false }));
  expect(screen.queryByRole("button", { name: "Back" })).toBeNull();
  expect(screen.getByRole("button", { name: "Home" })).toBeEnabled();
  expect(screen.getByRole("button", { name: /Rotate/ })).toBeDisabled();
  await hover(screen.getByRole("button", { name: /Rotate/ }));
  expect(await screen.findByText("Rotation isn't available for iOS simulators yet")).toBeVisible();
});

it("view-only iOS shows why and sends no input", async () => {
  const { inputCalls } = renderPanel(readyIosDevice({}, "xcodeUnsupported",
    "Xcode 27.1 changed the simulator interfaces BibCode uses. Simulators open as screenshots without touch input."));
  expect(screen.getByText(/Xcode 27.1 changed the simulator interfaces/)).toBeVisible();
  await pointerTap(screen.getByTestId("device-canvas"));
  expect(inputCalls()).toHaveLength(0);
});

it("opens the selected image on its platform", async () => {
  const { openCalls } = renderPanel(catalog({ platforms: [platform("ios", "ready", "Ready.", [image("UDID-1", "iPhone 17 Pro · iOS 27.0")])] }));
  await userEvent.click(screen.getByRole("button", { name: "Start" }));
  expect(openCalls()).toEqual([{ platform: "ios", imageId: "UDID-1" }]);
});
```
Helper names follow what already exists in `DevicePanel.test.tsx`. Add the missing helpers (`platform`, `image`, `readyIosDevice`) to that file.
- [ ] **Step 2: Run** `vp test run apps/web/src/components/device apps/web/src/components/settings/DevicePreviewsSection.test.tsx` and expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same tests and expect PASS. Then run `vp check` (zero warnings) and `vp run typecheck`.
- [ ] **Step 5: Do the `UI.md` review** of the picker grouping, the disabled reasons, the view-only notice and the toolbar differences, and write the findings into the task report. Also do the React best-practices review: if the `vercel-react-best-practices` skill is available, use it; otherwise report "NOT RUN (skill unavailable)" and do a manual check of memoisation, effect dependencies and render work.
- [ ] **Step 6: Commit** `feat(web): show iOS simulators in the Devices pane (#70)`.

---

### Task 10: Profiler, docs, runbooks, changelog and native validation

**Files:**
- Modify:
  - `device/mod.rs` (`record_lifecycle` gains `platform`);
  - `docs/skills/bibcode-profiler/references/events.md`;
  - `docs/architecture/overview.md` (Device previews and Profiler sections);
  - `docs/user/workspace-ui.md`;
  - `docs/testing/macos-desktop.md` and `docs/testing/cross-platform-validation.md`;
  - `CHANGELOG.md`;
  - `apps/server/tests/ios_simulator.rs` (finish the harness).

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write a failing test for the lifecycle event.** The test (in `device/mod.rs`, with the profiler enabled through the existing test hook used by other `profiler::emit` tests):
```rust
#[tokio::test]
async fn lifecycle_events_name_the_platform() {
    let _profiler = crate::diagnostics::profiler::testing::enable();
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1"]);
    let (manager, _enabled) = fake_manager(vec![ios]);
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &id, DeviceState::Ready).await;
    let events = crate::diagnostics::profiler::testing::events("device.lifecycle");
    assert!(events.iter().all(|e| e["platform"] == "ios"));
    assert!(events.iter().all(|e| e.get("imageId").is_none()), "never names the image");
}
```
If the profiler has no `testing` module, use whatever capture the existing profiler event tests use; find it with `rg -n "fn .*emit" apps/server/src/diagnostics/profiler`.
- [ ] **Step 2: Implement** the `platform` field. Run the test and expect PASS.
- [ ] **Step 3: Update the docs.**
  - **Profiler events:** `device.lifecycle` `platform` (`"android" | "ios"`) in `events.md` and the overview's Profiler section, plus the `ios-simulator` process identity and its crash reports.
  - **Architecture overview:** the backend split, the iOS backend, the private-API guard and view-only mode.
  - **`workspace-ui.md`:** iOS in the pane, single touch, the paste prompt, and view-only mode.
  - **Runbooks:**
    - `macos-desktop.md` gets an "iOS Simulator previews" procedure: open, live video, tap, type, Home, rotate (if supported), close, setting off, quit, then check `xcrun simctl list` shows the simulator `Shutdown`. It also runs `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator`.
    - `cross-platform-validation.md` adds iOS to the device previews section.
    - `execution-report-template.md`: review it, and change it only if a new evidence field is needed (`iOS sim tests: pass/fail`).
  - **`CHANGELOG.md`:** add `## [Unreleased]` above `## [v0.9.3]` with a "Device previews" section. It has one entry for Android emulators (with H.264 video and E2EE) and one for iOS simulators on macOS servers (single touch, view-only fallback). Write it in the existing changelog voice (user-facing, plain).
- [ ] **Step 4: Run the full validation.** Record each result in the task report.
  - `cargo fmt --all --check`
  - `cargo clippy -p bibcode-server --all-targets -- -D warnings` and `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`
  - `cargo test -p bibcode-server --lib` and `cargo test -p bibcode-server --tests`. The known macOS failures are `provider_runtime an_exited_provider_process_is_read_without_reaping_it` and the three `server_terminal::input_admission_tests`. Report them as pre-existing.
  - `cargo test -p bibcode-desktop`
  - `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator`
  - `vp check`, `vp run typecheck`, `vp test` (the ChatView "keeps the persisted terminal activity scope pinned…" failure is pre-existing on main), `vp run check:contracts`, `vp run check:dependency-ledger`
- [ ] **Step 5: Native validation on this Mac.**
  1. Build the desktop app the way `docs/testing/macos-desktop.md` says.
  2. Turn on Device previews.
  3. Open the iPhone 17 Pro, check live video, tap Settings, type in Safari's address bar, press Home, rotate (if supported), close, then turn the setting off with a device running and quit with one running.
  4. After each, `xcrun simctl list devices | grep 308FD3CD` must show `Shutdown`, and `pgrep -fl 308FD3CD` must show no BibCode-started process.
  5. Save screenshots and logs in the scratchpad, and summarize them in the task report using the execution report template's fields.
- [ ] **Step 6: Commit** `docs: iOS simulator previews in the overview, runbooks and changelog (#70)`.

---

## Self-review notes (plan author)

**Spec coverage:**

| Spec section | Task |
|---|---|
| §0 spikes | 1 |
| §1 backend split | 2, 3 |
| §2 contracts | 3, 4 |
| §3 lifecycle | 5 |
| §4 video | 6 (JPEG), 7 (H.264) |
| §5 input | 8 |
| §6 guard | 5 (Xcode), 6 (private) |
| §7 client | 4 (shape), 9 (UI) |
| §8 errors | 3 (busy, limit), 5 (boot), 8 (input) |
| §9 profiler | 5 (`report_exit`, attribution), 10 (event, docs) |
| §10 dependencies | 6 |
| §11 testing | each task, plus 10 |
| §12 documentation | 10 |

Out-of-scope items have no task.

**Deliberate deviations from the spec, recorded for the reviewer:**
- **`check_available`.** `DeviceBackend` gains `check_available` (Task 5), so a busy simulator is refused synchronously by `device.open`, as spec §8 requires.
- **MCP `device_open` default.** `platform` defaults to `"android"` when omitted (Task 4). This is a documented input default, not an alias.
- **iOS screenshot format.** MCP `device_screenshot` returns JPEG for iOS when PNG encoding isn't available without a new crate (Tasks 5 and 6).
