# iOS Simulator device previews — design

Status: approved in conversation 2026-10-15; spec awaiting review.
Issue: [#70](https://github.com/mubeda/BibCode/issues/70), item 5.
Builds on: `docs/superpowers/specs/2026-10-14-android-device-previews-design.md`
(historical; verify against `apps/server/src/device/` before reuse).

## Goal

With **Device previews** on and Xcode installed on a macOS environment server,
a user on any client can list the server's iOS simulators, boot one, see it
live in the Devices pane, tap, type, press hardware buttons, rotate (if the
spike proves it possible), and shut it down. Agents get the same `device_*`
tools as Android. Android and iOS share one device manager, one pane, one
media socket, and one setting.

What the user decided:

- **Scope: parity with Android** — live H.264 view, touch, keys, text,
  rotation, the JPEG fallback, and the agent tools.
- **Hosts: any macOS server**, local or remote over SSH or the relay,
  including headless Macs with no GUI login.
- **Approach A: Apple's private simulator frameworks**, loaded at runtime by
  the `bibcode` binary. No sidecar process.
- **Delivery: on the same branch** as Android
  (`feat/android-device-previews`), shipped in one PR.

## Probes run on 2026-10-15 (Xcode 27.0, iOS 26.5 and 27.0 runtimes)

- `simctl io recordVideo` writes only to a file or URL. Asking for stdout fails
  with "rendering to standard out is no longer supported". No public live
  video exists.
- `simctl io screenshot --type=jpeg` takes about 500 ms per frame at
  1206×2622 (about 2 fps). This is good enough for the view-only fallback, but
  not for a live view.
- `SimulatorKit.framework` lives in
  `Xcode.app/Contents/SharedFrameworks/`. It exports the
  `IndigoHIDMessageFor*` C functions (mouse, keyboard, button, scroll,
  arbitrary HID) and the class `SimulatorKit.SimDeviceLegacyHIDClient`.
  `CoreSimulator.framework` lives in `/Library/Developer/PrivateFrameworks/`.

## Architecture decision record

| Option                                        | Video                                 | Input                         | Headless  | Fragility                     | Verdict    |
| --------------------------------------------- | ------------------------------------- | ----------------------------- | --------- | ----------------------------- | ---------- |
| **A. Private CoreSimulator + SimulatorKit**   | IOSurface → VideoToolbox H.264, 60 fps | Indigo HID messages           | expected (spike S1) | may break on Xcode updates | **chosen** |
| B. XCUITest runner (WebDriverAgent-style)     | screenshot stream, 10–15 fps          | XCTest events, ~100 ms latency | yes      | low                           | rejected: second media path, xcodebuild start-up, Xcode project in repo |
| C. `simctl` only                              | ~2 fps screenshots                    | none                          | yes       | none                          | rejected as the main path; kept as the view-only fallback |
| `idb_companion`                               | —                                     | —                             | —         | —                             | rejected: native helper sidecar, forbidden by `AGENTS.md` |

Approach A fails soft. Every private class, selector, and symbol is checked
before use. If any is missing, iOS falls back to option C and says why.

## 0. Spikes (plan task 1, before any production code)

The plan's first task runs throwaway probes and writes the results into this
section. A failed spike changes the design section it names before
implementation starts.

- **S1 — headless.** From a plain SSH session to a Mac with nobody logged in
  at the console, boot a simulator, register the screen callbacks, and send a
  tap. If the callbacks or HID need a GUI session, then on such hosts iOS
  reports view-only mode with the reason (§6). The rest of the design stands.
- **S2 — rotation.** Find a private call that changes the simulated device's
  orientation, such as a SimDevice orientation selector, a purple/GS event, or
  an Indigo HID message. If none works, iOS shows Rotate disabled (§5).
- **S3 — framebuffer.** Confirm on Xcode 27 the screen-callback registration
  on the device's IO ports (the path `idb`'s `FBFramebuffer` uses), the
  IOSurface pixel format, and its size after rotation.

## 1. Server: backend split (`apps/server/src/device/`)

The device module splits into a platform-neutral manager and two backends:

- `device/` (platform-neutral): device slots, the lifecycle state machine, the
  cancel-safe detached stop, the setting gate and shutdown, media tickets,
  `FrameHub` (JPEG) and `MediaHub` (H.264 fan-out), and the catalog `watch`.
- `device/android/`: today's `sdk`, `emulator`, `grpc`, `scrcpy` and input
  mapping, moved without behavior change.
- `device/ios/`: `simctl` (lifecycle and listing), `coresim` (all `unsafe`
  FFI), `encoder` (VideoToolbox), and `hid` (Indigo messages). Compiled only
  on `cfg(target_os = "macos")`; other targets register no iOS backend.

An internal trait describes a backend:

```rust
trait DeviceBackend: Send + Sync + 'static {
    fn platform(&self) -> DevicePlatform;
    /// Availability and images; cheap enough for the 30 s catalog refresh.
    async fn discover(&self) -> PlatformCatalog;
    async fn launch(&self, image_id: &str, cancel: CancellationToken)
        -> Result<Box<dyn RunningDevice>, DeviceOpError>;
}

trait RunningDevice: Send + Sync {
    async fn snapshot_jpeg(&self, max_width: u32) -> Result<JpegFrame, DeviceError>;
    /// Starts the H.264 source. A reset callback asks for a fresh key frame.
    async fn start_video(&self) -> Result<VideoSource, DeviceError>;
    async fn input(&self, events: &[DeviceInputEvent]) -> Result<(), DeviceError>;
    async fn rotate(&self, to: DeviceOrientation) -> Result<(), DeviceError>;
    /// Completes when the device ends for any reason; carries the reason.
    fn exited(&self) -> BoxFuture<'static, ExitReason>;
    async fn stop(self: Box<Self>);
}
```

The exact signatures are the plan's to settle. The constraints are:

- Android passes every existing test unchanged after it moves behind the
  trait.
- The manager never names an Android or iOS type.

Limits are **one running device per platform**: one Android and one iOS device
may run together. The existing `DeviceLimitError` applies per platform.

## 2. Contracts (`packages/contracts/src/device.ts`)

Nothing has shipped, so fields are renamed in place without compatibility
aliases.

- `DevicePlatform = "android" | "ios"`.
- `DeviceCatalog`:
  `{ platforms: Array<{ platform, availability, images: Array<{ id, displayName, inUseElsewhere?: true }> }>, devices }`.
  The top-level `availability` and `avds` fields are removed.
- `DeviceSnapshot` gains `platform` and `canRotate: boolean` (§5), and
  `avdName` becomes `imageId`.
- `DeviceOpenInput`: `{ platform, imageId }`.
- `DeviceAvailabilityKind` adds `xcodeMissing`, `xcodeSetupIncomplete`,
  `noSimulators`, `xcodeUnsupported` and `viewOnlyHeadless`. The kind `noAvds`
  stays Android-only.
- The new tagged error `DeviceBusyError { imageId, message }` covers a
  simulator already booted outside BibCode.
- `DeviceKey` is unchanged. iOS maps `home`, `power` (lock), `volumeUp`,
  `volumeDown`, `enter`, `backspace`, `delete`, `tab` and the arrow keys.
  `back` and `appSwitch` on iOS return `DeviceInputError` ("iOS has no Back
  key.").
- No RPC method is added or removed, so the method and stream counts stay at
  158 and 24. Payload fingerprints and fixtures are regenerated.
- The MCP tools `device_list`, `device_open`, `device_screenshot` and
  `device_close` add `platform` to their output. `device_open` takes
  `platform` and `imageId`.

## 3. iOS lifecycle (`device/ios/simctl.rs`, `coresim.rs`)

- **Images.** These are the available simulators from `xcrun simctl list
  devices available -j`, with ID = UDID and display name "iPhone 17 Pro ·
  iOS 27.0". A simulator whose state is not `Shutdown` and that BibCode did
  not boot is listed with `inUseElsewhere: true`.
- **Open.**
  1. Refuse with `DeviceBusyError` unless the simulator is `Shutdown`.
  2. Run `xcrun simctl boot <udid>`, then `xcrun simctl bootstatus <udid> -b`.
     Both are supervised children (`process::supervised`) and share the
     launcher's boot timeout (default 180 s; tests pass a short one).
  3. Attach the CoreSimulator `SimDevice` by UDID, register the screen
     callbacks (§4), and create the HID client (§5).

  The states are `starting`, then `booting`, then `ready`.
- **Stop.** Unregister callbacks, release the HID client, then run
  `xcrun simctl shutdown <udid>` with a 30 s timeout. This uses the same
  detached, cancel-safe stop as Android. The setting turning off and server
  shutdown stop only simulators BibCode booted.
- **Exit detection.** Use CoreSimulator's device-state notification handler
  when probed present, otherwise poll `simctl list -j` every 2 s. An
  unrequested transition out of `Booted` marks the device `failed` with the
  reason.
- **Not reaped after a server crash.** A simulator booted by a server that
  crashed is not reaped on the next start. It shows as `inUseElsewhere`.
  This matches Android's documented behavior.

## 4. iOS video (`coresim.rs`, `encoder.rs`)

- **Screen source.** Register screen callbacks on the device's IO port for
  the main display. Each damage callback yields the framebuffer `IOSurface`.
  No copy is made: a `CVPixelBuffer` wraps the surface
  (`CVPixelBufferCreateWithIOSurface`).
- **Encoder.** A `VTCompressionSession` produces H.264 with real-time on, no
  frame reordering, a 2 s key-frame interval, and a bitrate scaled to pixel
  count. Output conversion:
  - AVCC length-prefixed NAL units become Annex B.
  - SPS and PPS from the format description become a config `VideoUnit`.
  - Key-frame flags and PTS are mapped.

  The output feeds `MediaHub` through the same `mpsc::Receiver<VideoUnit>` plus
  reset callback that Android uses (ruling R4 of the Android plan). A reset
  forces the next frame to be a key frame. A size change (rotation) tears
  down and recreates the session, then emits a new config unit.
- **Pacing.** Frames go out only on damage, capped at 60 fps. Backpressure,
  slow-viewer handling and the JPEG fallback rules are unchanged from Android.
- **JPEG.** `snapshot_jpeg` locks the current surface and encodes it with
  `jpeg-encoder` (already a dependency) at the requested width. In view-only
  mode it uses `xcrun simctl io <udid> screenshot --type=jpeg -` instead.
- **Threading.** Callbacks run on a private serial dispatch queue and hand
  work to Tokio through bounded channels. No Tokio runtime call happens on a
  dispatch thread, and no CoreFoundation object crosses threads unretained.

## 5. iOS input (`hid.rs`)

- **Touch.** Normalized `x`/`y` (0..1, current orientation) go straight into
  the Indigo touch message, which takes screen ratios. **One pointer**:
  - Events whose `pointerId` is not the first active pointer are dropped.
  - A second `down` while one is active is ignored.
  - Multi-touch and pinch are out of scope for iOS v1.
- **Buttons.** `home`, `power`, `volumeUp` and `volumeDown` use
  `IndigoHIDMessageForButton`.
- **Keys and text.**
  - Keys use HID keyboard usages through `IndigoHIDMessageForKeyboardArbitrary`.
  - Text in the US-layout printable set is sent as key down/up pairs, with
    Shift where needed.
  - Any other text goes through `xcrun simctl pbcopy <udid>` followed by
    Cmd+V. The pane notes that iOS may show its paste prompt.
  - The existing per-chunk limit (≤1000 UTF-16 units) still applies.
- **Rotation.** Uses the S2 result. If none was found, `rotate` returns
  `DeviceInputError` ("Rotation isn't available for iOS simulators yet.") and
  the catalog reports `canRotate: false` on the device snapshot. This field is
  added to `DeviceSnapshot`; Android always sets it `true`.
- **Send path.** Messages go through
  `-[SimDeviceLegacyHIDClient sendWithMessage:freeWhenDone:completionQueue:completion:]`.
  Completions resolve a oneshot. A send failure surfaces as `DeviceInputError`
  and is logged at `warn`.

## 6. Availability and the private-API guard

`discover()` computes iOS availability in this order:

1. The server is not macOS: no iOS platform entry at all.
2. `xcode-select -p` or `xcrun simctl help` fails: `xcodeMissing`, "Install
   Xcode, then open it once to finish setup."
3. `simctl` reports the license or first-launch error: `xcodeSetupIncomplete`,
   "Finish Xcode setup: run `sudo xcodebuild -runFirstLaunch`."
4. No available simulators: `noSimulators`, "Create a simulator in Xcode →
   Window → Devices and Simulators."
5. `dlopen` of either framework fails, or any required class, selector or
   symbol is missing: `xcodeUnsupported`, "Xcode <version> changed the
   simulator interfaces BibCode uses. Simulators open as screenshots without
   touch input." Simulators can still be opened. They run in view-only mode:
   `simctl` screenshots only, input returns `DeviceInputError`, and video
   tickets return `DeviceMediaUnavailableError`, so the client uses JPEG at
   once. The debug log names the missing symbol.
6. If S1 found that the GUI session is required and none exists:
   `viewOnlyHeadless`, "Live input needs someone logged in on this Mac.
   Simulators open as screenshots."
7. Otherwise `ready`.

The guard runs once per server process, with its result cached, and again
only when Xcode's path or version changes.

## 7. Client (`apps/web`)

- **Picker.** Images are grouped under **Android** and **iOS** headings. Each
  heading shows its own availability message when not ready, so one platform
  being unavailable never hides the other. `inUseElsewhere` images are listed
  disabled with "Running outside BibCode".
- **Toolbar follows the platform.**
  - Android keeps Back, Home, Recents, Rotate and Paste.
  - iOS shows Home, Lock, Volume up/down, Rotate and Paste.
  - When `canRotate` is false, Rotate is disabled with the tooltip "Rotation
    isn't available for iOS simulators yet".
- **View-only notice.** One line above the canvas gives the reason when the
  iOS platform is `xcodeUnsupported` or `viewOnlyHeadless`. The canvas shows a
  non-interactive cursor and sends no input.
- **Setting.** The **Settings → Diagnostics → Device previews** description
  becomes "Android emulators and, on macOS servers, iOS simulators." There is
  no second toggle.
- **Reuse.** Canvas, video player, JPEG fallback, retries and E2EE media are
  unchanged.

## 8. Errors

| Condition                          | Error / availability          | Message                                                                 |
| ---------------------------------- | ----------------------------- | ----------------------------------------------------------------------- |
| Simulator already booted elsewhere | `DeviceBusyError`             | "This simulator is already running outside BibCode. Shut it down in Simulator, then open it here." |
| Second iOS device                  | `DeviceLimitError`            | "<name> is already running. Close it first."                           |
| Boot fails or times out            | `DeviceEmulatorError`         | Last lines of `simctl` stderr (shared stderr tail)                      |
| Android-only key on iOS            | `DeviceInputError`            | "iOS has no Back key."                                                  |
| Input in view-only mode            | `DeviceInputError`            | "Touch input is off: <reason>."                                         |

## 9. Profiler, crash, and trace coverage

- **Child processes.** `simctl` children (`boot`, `bootstatus`, `shutdown`,
  `list`, `io screenshot`, `pbcopy`) go through `process::supervised` with
  timeouts.
- **Simulators.** Simulators are children of CoreSimulator's launchd service,
  not of BibCode. The backend looks up the simulator's `launchd_sim` PID after
  boot and registers it with
  `ProcessAttributionRegistry::register_identity` (identity
  `ios-simulator`). An unrequested exit calls
  `diagnostics::profiler::exits::report_exit` with `Watch::Failures`.
- **RPC methods.** No new methods are added. `device.close`, `device.input`
  and `device.rotate` stay in `is_long_running`.
- **Events.** `device.lifecycle` gains `platform`. Document it in
  `docs/skills/bibcode-profiler/references/events.md` and the overview's
  Profiler section.
- **Shutdown.** `DeviceManager::shutdown` keeps its quiesce position and now
  stops both backends.
- **Logs and traces.** FFI failures log at `warn` with the selector or symbol
  name. No screen content or typed text is logged.

## 10. Dependencies

These are macOS-only (`[target.'cfg(target_os = "macos")'.dependencies]`),
pinned exactly and recorded in the dependency ledger:

- `objc2`, `objc2-foundation`
- the `objc2` framework crates for IOSurface, CoreVideo, CoreMedia and
  VideoToolbox
- `block2` for completion blocks

The plan picks exact versions that are at least two weeks old. No npm
dependencies are added.

## 11. Testing

- **Platform-neutral manager.** A fake `DeviceBackend` drives:
  - lifecycle;
  - a cancelled close;
  - the per-platform limit;
  - a busy refusal;
  - an unrequested exit;
  - setting-off and shutdown.

  All existing Android tests pass unchanged.
- **iOS, all platforms.** Pure tests cover:
  - `simctl list -j` parsing, including `inUseElsewhere`;
  - AVCC → Annex B and SPS/PPS → config unit, using recorded VideoToolbox
    output fixtures;
  - touch, key and text → Indigo message mapping, including single-pointer
    rules and the clipboard fallback;
  - the availability ordering in §6.
- **Contracts.** Schema tests and regenerated RPC fixtures.
- **Web.** Tests cover:
  - the grouped picker with mixed availability;
  - the per-platform toolbar;
  - the disabled Rotate tooltip;
  - the view-only notice;
  - no input sent in view-only mode.
- **Real simulator (macOS, opt-in).** Integration tests run behind
  `BIBCODE_IOS_SIM_TESTS=1`. They are not in default CI, because `ci.yml`
  notes its macOS matrices queue for hours. The tests check that:
  - boot reaches `ready`;
  - the first key frame decodes to the expected size;
  - a tap on a known point changes the frame;
  - text arrives;
  - close leaves the simulator `Shutdown` with no leftover BibCode children.
- **Native.** The packaged desktop app on macOS goes through open, live
  stream, tap, type, Home, rotate (if S2 passed), close, setting off, and quit.
  Add this to `docs/testing/macos-desktop.md` and the cross-platform
  validation procedure.

## 12. Documentation

- `docs/architecture/overview.md` and the device section: the backend split
  and the iOS backend.
- `docs/user/workspace-ui.md`: iOS in the Devices pane, view-only mode, and
  the single-touch limit.
- Profiler references (§9) and the testing runbooks (§11).
- `CHANGELOG.md` `[Unreleased]`: one entry covering Android and iOS device
  previews.

## 13. Delivery order

1. Spikes S1–S3; record results in §0.
2. Backend split with Android behind the trait (no behavior change) and the
   fake backend tests.
3. Contract changes, fixtures, and client updates for `platform`/`imageId`
   (Android only still).
4. iOS discovery, lifecycle and the private-API guard (view-only path end to
   end).
5. iOS video (VideoToolbox → `MediaHub`).
6. iOS input and rotation.
7. Client: grouped picker, per-platform toolbar, view-only notice.
8. MCP tools, profiler, docs, runbooks, changelog; real-simulator tests and
   native validation on this Mac.

## Out of scope

- Multi-touch and pinch on iOS.
- Physical iOS devices.
- watchOS, tvOS and visionOS simulators.
- Creating or deleting simulators from BibCode.
- Installing apps from the pane.
- Audio.
- Agent input tools (still out of scope for both platforms).
- Reaping simulators left by a crashed server.
