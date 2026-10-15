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
| **A. Private CoreSimulator + SimulatorKit**   | IOSurface → VideoToolbox H.264, 60 fps | DTUHID over XPC; Indigo fallback | expected (spike S1) | may break on Xcode updates | **chosen** |
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

### Results (2026-10-15)

Environment: macOS 27.0.1, Xcode 27.0, CoreSimulator **1171.7**, iPhone 17 Pro
on the iOS **26.5** runtime, booted with `simctl` and Simulator.app **not
running**. **Only the iOS 26.5 runtime was exercised.** Every result below is
UNCONFIRMED on the installed iOS 27.0 runtime. The probes were scratch Objective-C programs that `dlopen` the two
frameworks and use `objc_msgSend` and `dlsym`. Later tasks take names from
this subsection. Anything marked UNCONFIRMED was not observed here.

**Rules that apply to every call**

- The IO ports, descriptors and screen properties are `ROCKRemoteProxy` /
  `ROCKImmutableProxy` objects:
  - KVC (`valueForKey:`) raises on them. Send each selector directly.
  - An unknown selector raises `NSInvalidArgumentException`. For example,
    `ioSurface` raises on Xcode 27.
  - An Objective-C exception must never unwind through Rust frames. Call
    only the selectors listed below, and gate them with
    `conformsToProtocol:` / `respondsToSelector:`.
- `respondsToSelector:` was accurate for the display descriptors in this
  probe, but idb warns that proxies may claim selectors they do not
  implement.

**Lookup (CoreSimulator, `/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator`)**

- `+[SimServiceContext sharedServiceContextForDeveloperDir:error:]`
  (`@32@0:8@16^@24`) takes `(NSString *, NSError **)` and returns `id`. The
  developer dir is `xcode-select -p`.
- `-[SimServiceContext defaultDeviceSetWithError:]` (`@24@0:8^@16`) returns
  `id`.
- `-[SimDeviceSet devices]` (`@16@0:8`) returns `NSArray<SimDevice *>`. Match on
  `-[SimDevice UDID]` (`@16@0:8`, an `NSUUID`). `-[SimDevice state]`
  (`Q16@0:8`): 3 means booted.
- `-[SimDevice io]` (`@16@0:8`) returns a `SimDeviceIOClient`. Call
  `-[SimDeviceIOClient ioPorts]` (`@16@0:8`), which returns an `NSArray` of
  `SimDeviceIOPortInterface` proxies.
- `-[port descriptor]` (`@16@0:8`) returns the descriptor. The main display is
  the descriptor that meets all three tests:
  1. It conforms to `SimScreen`, `SimDisplayIOSurfaceRenderable` and
     `SimDisplayRenderable`.
  2. It responds to `framebufferSurface`.
  3. `-[descriptor state]` returns a `SimDisplayDescriptorState` whose
     `displayClass` (`S`, unsigned short) is `0`.

  The iPhone 17 Pro has one class-0 display and two class-1 displays.
- `SimDisplayDescriptorState`:
  - `defaultWidthForDisplay` (`I`) is 1206.
  - `defaultHeightForDisplay` (`I`) is 2622.
  - `defaultPixelFormat` (`I`) is `'BGRA'` (0x42475241).

**S3: framebuffer (confirmed)**

- `-[descriptor framebufferSurface]` (`@16@0:8`) returns an `IOSurface`
  (toll-free bridged `IOSurfaceRef`) with these properties:
  - 1206×2622 pixels, pixel format `'BGRA'` (0x42475241).
  - 4864 bytes per row (padded, not width×4), 4 bytes per element, and 0
    planes (non-planar).
  - An allocation of 12,763,136 bytes.

  `maskedFramebufferSurface` has the same shape, with corners and notch
  masked. Do not use it.
- **Preferred callbacks (`SimScreen`):**
  `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:`
  has the encoding `v56@0:8@16@24@?32@?40@?48`. Its arguments are:
  - `NSUUID *`
  - `dispatch_queue_t`
  - `void (^)(void)` for frames
  - `void (^)(id fb, id masked)` for surfaces
  - `void (^)(id props)` for properties

  The runtime encodes each block only as `@?`, so the block parameter types
  above are inferred from idb's headers and from what the probe received.

  How they behaved:
  - `surfacesChanged` fired once, immediately, with the current surface.
  - `frameCallback` fired 130 times in 5 s during an app launch.
  - All three callbacks run on the supplied queue.
  - Unregister with `unregisterScreenCallbacksWithUUID:` (`v24@0:8@16`).
  - Correction (Task 6): CoreSimulator sends the blocks over XPC and raises
    `NSInternalInconsistencyException` ("Block is missing signature field")
    for a block without a signature. Give each block its signature:
    `v8@?0`, `v24@?0@8@16` and `v16@?0@8`.
- **Old-style fallback (`SimDisplayRenderable` /
  `SimDisplayIOSurfaceRenderable`):**
  - `registerCallbackWithUUID:damageRectanglesCallback:` (`v32@0:8@16@?24`)
    fired 158 times in 5 s. The array was **always empty**, so it is a bare
    per-frame tick, not geometry. It fired 6 times in 3 s while idle, on a
    non-main framework thread.
  - `registerCallbackWithUUID:ioSurfacesChangeCallback:` (`v32@0:8@16@?24`)
    did **not** fire at registration.
  - Unregister with `unregisterDamageRectanglesCallbackWithUUID:` and
    `unregisterIOSurfacesChangeCallbackWithUUID:`.
  - §4 should use the `SimScreen` callbacks and read `framebufferSurface`
    once at start and on each `surfacesChanged`. A frame callback does not
    carry the surface.
- **Size after rotation:** the IOSurface **stays 1206×2622 in portrait** (same
  surface ID). The guest draws rotated content into it. The rotation is
  reported in two ways:
  - `propertiesChangedCallback` fires.
  - `-[SimScreenProperties uiOrientation]` (`I`) changes.

  `SimScreenProperties` comes from `-[descriptor screenProperties]` or from the
  callback argument. It has these selectors:
  - `uiOrientation`
  - `pixelSize` (`{CGSize=dd}`, 1206×2622)
  - `screenID` (1)
  - `uniqueId` (`"PurpleMain"`)
  - `name` (`"LCD"`)
  - `seed`
  - `dotPitch` (460)
  - `screenType`

  `simctl io screenshot` returns 2622×1206 in landscape, so it is already
  rotated. To send a landscape view, the encoder must rotate per
  `uiOrientation`, or the client must.

**S1: input (confirmed with a GUI session logged in and Simulator.app not running)**

- **Legacy Indigo path (SimulatorKit,
  `/Applications/Xcode.app/Contents/SharedFrameworks/SimulatorKit.framework/SimulatorKit`):**
  - `NSClassFromString(@"SimulatorKit.SimDeviceLegacyHIDClient")`:
    - Initialize with `-initWithDevice:error:` (`@32@0:8@16^@24`).
    - Send with `-sendWithMessage:freeWhenDone:completionQueue:completion:`
      (`v44@0:8^{IndigoHIDMessageStruct=…}16B24@28@?36`). Its arguments are
      `(IndigoMessage *, BOOL, dispatch_queue_t, void (^)(NSError *))`.
    - Completions arrived with `nil` errors.
    - Also present: `-initWithDevice:sessionResetQueue:error:sessionResetHandler:`
      and `-resetHIDSession`.
  - C builders (exported, `dlsym`). Each returns a `malloc`'d `IndigoMessage *`:
    - `IndigoHIDMessageForMouseNSEvent(CGPoint *p1, CGPoint *p2OrNULL, uint32_t target, NSEventType type, CGSize size, uint32_t edge)`
      - Use target `0x32`, type 1 for down and 2 for up, size `{1,1}` (so
        `p` is a ratio), and edge 0.
      - It returns a 384-byte multi-payload message (`innerSize` 0xA0,
        eventType 2, eventKind 0xB).
      - The builder's touch fields:

        | Field                    | Down     | Up       |
        | ------------------------ | -------- | -------- |
        | `field1` (0x30)          | 0x400002 | 0x400002 |
        | `field2` (0x34)          | 1        | 1        |
        | `eventMask` (0x38)       | 0x3      | 0x3      |
        | `range` (0x64)           | 1        | 0        |
        | `touch` (0x68)           | 1        | 0        |
      - The probe re-enveloped it as idb's 320-byte (0x140) single-touch
        message. All offsets are bytes from the **start of the message**, in
        both the builder's buffer and the new one, and multi-byte fields are
        little-endian:
        1. In the builder's buffer, write the ratio point as doubles:
           `xRatio` at 0x3C and `yRatio` at 0x44.
        2. `calloc` 0x140 bytes. The mach header at 0x00–0x17 stays **all
           zero**. `sendWithMessage:` filled in whatever it needs; the probe
           wrote nothing there.
        3. Write `innerSize` (u32) 0x90 at 0x18, `eventType` (u8) 2 at 0x1C,
           and `eventKind` (u32) 0xB at 0x20. Write `mach_absolute_time()`
           (u64, at a 4-byte-aligned offset) at 0x24. Leave 0x1D–0x1F and
           0x2C–0x2F zero.
        4. Copy 0x70 bytes from **source offset 0x30 to destination offset
           0x30**. This is the touch struct and includes `xRatio` and
           `yRatio`. Then `free` the builder's buffer.
        5. Copy the 0x90-byte payload at 0x20–0xAF to 0xB0–0x13F. In the copy,
           set `field1` (u32 at 0xC0) to 1 and `field2` (u32 at 0xC4) to 2.
        6. Send 0x140 bytes with `freeWhenDone:YES`.
    - `IndigoHIDMessageForButton(int32_t source, int32_t direction, int32_t target)`
      - Sources: home is 0x0 and lock is 0x1, both tested. Side is 0xBB8,
        which comes from idb and was not tested.
      - Direction: 1 is down, 2 is up. Target: 0x33.
      - The message is 192 bytes.
    - `IndigoHIDMessageForHIDArbitrary(int32_t target, uint32_t usagePage, uint32_t usage, int32_t direction)`
      - Use target 0x32.
    - `IndigoHIDMessageForKeyboardArbitrary(int32_t keyCode, int32_t direction)`
  - Results:

    | Input                                                                     | Result                         | How it was verified                                         |
    | ------------------------------------------------------------------------- | ------------------------------ | ----------------------------------------------------------- |
    | Touch on the Settings icon at ratio (0.848, 0.483)                        | **Works**, 5 of 5 cycles       | Settings opened. The screen centre is empty wallpaper, so it was not used. |
    | Home button (source 0)                                                    | **Works**, 5 of 5 cycles       | Returned to the home screen.                                |
    | Lock button (source 1)                                                    | **Works**                      | The device locked.                                          |
    | Volume up via `HIDArbitrary(0x32, 0x0C, 0xE9)`                            | **Works**: one step per press  | `sim_volume` in `data/var/run/simulatoraudio/audiosettings.plist` went 6→12→18. Each step is about 100/16. |
    | `KeyboardArbitrary`, HID usages 0x04–0x08 or HIToolbox code 0x02          | **Dropped**: no text appeared  | A focused search field showed no text.                      |
- **DTUHID path (`dtuhidd` in the guest, in-process XPC with no helper):**
  - Symbols (`dlsym(RTLD_DEFAULT)`, libxpc):
    - `xpc_object_t xpc_endpoint_create_mach_port_4sim(mach_port_t, uint64_t, uint64_t)`
      takes `(port, 0, 0)`.
    - `xpc_connection_t xpc_connection_create_from_endpoint(xpc_object_t)`.
    - `void xpc_connection_enable_sim2host_4sim(xpc_connection_t)`, called
      before resume.
  - Get the port from `-[SimDevice lookup:error:]` (`I32@0:8@16^@24`, which
    returns `mach_port_t`). The service is
    `com.apple.coredevice.feature.remote.hid.digitizer`.
  - Connect in this order:
    1. `xpc_endpoint_create_mach_port_4sim(port, 0, 0)`
    2. `xpc_connection_create_from_endpoint`
    3. `xpc_connection_enable_sim2host_4sim`
    4. `xpc_connection_set_event_handler`
    5. `xpc_connection_resume`
  - **One connection carries all three message types.** Touch, buttons and
    keys all went over the digitizer-service connection.
    `featureIdentifier` is always
    `"com.apple.coredevice.feature.remote.hid.digitizer"`.
  - Each probe run opened its own connection and sent one gesture. A single
    long-lived connection used across many gestures was not tested.
  - Every message is an XPC dictionary with these keys:
    - `messageType` (string)
    - `isBarrier` (bool)
    - `featureIdentifier` (string, the service name)
    - `payload` (dictionary)
  - The liveness check is
    `IndigoKeyboardButtonEvent {usageCode: u64 0, state: u64 2}`, sent with
    `isBarrier = true` through `xpc_connection_send_message_with_reply`. The
    reply was an empty dictionary on the first attempt. The device had
    been booted for minutes by then.
  - A successful port lookup does not prove liveness. `dtuhidd` is
    demand-launched, and idb documents it aborting early in boot. The port
    still exists after that, and sends to it are silently discarded.
    Connecting must round-trip the barrier and retry with backoff after boot.
    idb makes 5 attempts, each with a 4 s reply timeout and a 4 s backoff.
  - **Send sequence for every event:** `xpc_connection_send_message(conn, msg)`,
    then `xpc_connection_send_barrier(conn, block)`, and wait for the block.
    Tested gestures:
    - Tap: start, 60 ms, end.
    - Button: down, 60 ms, up.
    - Key: down, 30 ms, up.

    After the last event the probe waited about 300 ms before exiting. idb
    drains for 80 ms before disconnecting.
  - The exact `messageType` strings are `"IndigoDigitizerEvent"`,
    `"IndigoButtonEvent"` and `"IndigoKeyboardButtonEvent"`. All payload
    integers are XPC `uint64`. Coordinates are XPC `double`.
  - Results:

    | Message                                                               | Payload                                                                          | Result                                                        |
    | --------------------------------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------------------------------------------- |
    | `IndigoDigitizerEvent`                                                | `pointOne {x: double, y: double}`, `eventType` u64 (0 start, 1 position, 2 end), `edge` u64 0, `target` u64 0 | **Works**: opened Settings; opened a Safari tab in landscape. |
    | `IndigoButtonEvent`                                                   | `usagePage` u64, `usageCode` u64, `state` u64 (1 down, 2 up)                     | **Works**: Home (0x0C, 0x40), Power/lock (0x0C, 0x30), Volume up/down (0x0C, 0xE9 / 0xEA). |
    | `IndigoKeyboardButtonEvent`                                           | `usageCode` u64 (HID usage), `state` u64 (1 down, 2 up)                          | **Works**: typed "Abc" into the Settings search field.        |
- **Coordinates:** touch ratios are in the **portrait framebuffer** space,
  not the rotated view. Each check was a tap on Safari's "+" or URL field,
  confirmed by an extra tab or a focused field. (u, v) is the view ratio in
  the landscape screenshot.

  | `uiOrientation` | Mapping sent           | Indigo                   | DTUHID                   |
  | --------------- | ---------------------- | ------------------------ | ------------------------ |
  | 1 (portrait)    | identity               | confirmed                | confirmed                |
  | 3               | (u, v) → (v, 1 − u)    | confirmed (fix round 1)  | confirmed                |
  | 4               | (u, v) → (1 − v, u)    | confirmed (fix round 1)  | confirmed (fix round 1)  |
  | 2               | (1 − u, 1 − v)         | UNCONFIRMED, inferred    | UNCONFIRMED, inferred    |

  At `uiOrientation` 3, sending the unmapped (0.5, 0.08) did nothing.
- **Consequence for §5:** keys and text must use DTUHID
  `IndigoKeyboardButtonEvent`, because legacy keyboard input is dropped on
  CoreSimulator 1171.7. Touch and buttons work on either path. idb treats
  legacy touch as unreliable from CoreSimulator 1155.4 on, so prefer DTUHID
  for all input and keep Indigo touch/buttons as the fallback.
- **Headless: only the no-Simulator.app case was tested.**
  - With a GUI session logged in at the console and Simulator.app not
    running, booting, the callbacks and HID all work.
  - The case with **no GUI session at all** was not tested.
  - **S1 over SSH: NOT CONFIRMED (no key auth).** `ssh -o BatchMode=yes localhost`
    returned "Permission denied (publickey,password,keyboard-interactive)".
  - **Logged-out console: NOT CONFIRMED.** The probe could not log the GUI
    out.
  - The default stays "works". Task 6's `viewOnlyHeadless` branch stays
    implemented but unreachable.

**S2: rotation (confirmed: Purple GSEvent)**

The options were tried in the brief's order:

1. `SimDevice` / `SimDeviceIO` method lists filtered for `orient` or `rotat`:
   nothing usable. The only match was
   `updateNearbyInteractionDeviceRect:deviceRotation:error:`.
2. `IndigoHIDMessageForHIDArbitrary(0x32, 0x20, 0x8A, …)` (HID Sensor page,
   Device Orientation): no change.
3. `-[SimDevice postDarwinNotification:error:]` (`B32@0:8@16^@24`) with
   `com.apple.springboard.rotate`: returned YES, with no change.
4. **Purple GSEvent: works.**
   - Get the port with `-[SimDevice lookup:@"PurpleWorkspacePort" error:]`.
   - Send a 112-byte buffer with
     `mach_msg(MACH_SEND_MSG | MACH_SEND_TIMEOUT, size 108, timeout 2000 ms)`.
     The buffer is 112 bytes for 8-byte alignment, but the message size is
     108. Every byte not listed below is zero, including `msgh_local_port`
     (0x0C) and `msgh_voucher_port` (0x10). The buffer holds:

     | Offset | Value                                          |
     | ------ | ---------------------------------------------- |
     | 0x00   | `msgh_bits` 0x13                               |
     | 0x04   | `msgh_size` 108                                |
     | 0x08   | `msgh_remote_port` = the port                  |
     | 0x14   | `msgh_id` 0x7B                                 |
     | 0x18   | GSEvent type `50 \| 0x20000`                   |
     | 0x48   | record size 4                                  |
     | 0x4C   | orientation value                              |

   - Orientation values:
     - **Observed:** 1 returns to portrait.
     - **Observed:** after Purple 4 (`uiOrientation` 3), the UI's top edge
       lies along the **physical left** edge of the portrait surface.
     - **Observed in fix round 1:** after Purple 3 (`uiOrientation` 4), the
       UI's top edge lies along the **physical right** edge. Both were seen
       by dumping the surface.
     - **From idb, untested:** 2 means upside-down. It had no effect here.
       idb's names for 3 and 4, "landscape-right" and "landscape-left", are
       UNCONFIRMED.

     Task 8 must map the user's rotate left/right onto the observed effects,
     not onto idb's names.
   - Measured with Safari in the foreground:

     | Purple value sent | `uiOrientation` read back | Screenshot |
     | ----------------- | ------------------------- | ---------- |
     | 4                 | 3                         | 2622×1206  |
     | 3                 | 4                         | 2622×1206  |
     | 1                 | 1                         | 1206×2622  |

   - Value 2 had no effect in Safari, which does not support upside-down on
     iPhone.
   - The home screen does not rotate on iPhone. Whether the UI rotates
     depends on the foreground app.
5. idb's vendor-defined HID path made **no change on iOS 26.5**:
   - It uses DTUHID service
     `com.apple.coredevice.feature.remote.hid.vendordefined` and the message
     `IndigoVendorDefinedEvent {usagePage 0xff61, usage 0x5b, version 0, data: IOCFSerialize(binary)}`.
   - The serialized dictionary is
     `{provider: com.apple.Virtualization.VirtualMachines, source: orientation-picker-control, type: enum, value: landscape-left}`.
   - idb uses this path only when the
     `com.apple.coredevice.feature.monitormotion` /
     `querymotioncapabilities` query reports `deviceMotionState`. The probe
     **never ran that query** on any runtime, and it never tried this path on
     iOS 27.0. Both are UNCONFIRMED.

Rotate is therefore **enabled**, and `canRotate` is `true` for iOS.

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

Amended after the spikes (§0 Results); §0 names every call.

- **Screen source.** Register the `SimScreen` callbacks
  (`registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:`)
  on the class-0 display descriptor. Read `framebufferSurface` at start and on
  each `surfacesChanged`. Each `frameCallback` means "new pixels"; it carries
  no surface. Each `propertiesChanged` reads `uiOrientation`, which becomes the
  device's orientation in the catalog.
- **Rotation is done on the server.** The surface stays portrait (1206×2622,
  `'BGRA'`, padded rows) while the guest draws rotated content into it. Before
  encoding, the server rotates each frame into the current UI orientation:
  - H.264 uses `VTPixelRotationSession` on the `CVPixelBuffer` that wraps the
    surface (`CVPixelBufferCreateWithIOSurface`), so no CPU copy is made.
  - JPEG rotates while it converts BGRA to RGB.

  As on Android, a rotated device's frames therefore swap width and height,
  and the client needs no iOS-specific rendering.
- **Encoder.** A `VTCompressionSession` produces H.264 with real-time on, no
  frame reordering, a 2 s key-frame interval, and a bitrate scaled to pixel
  count. Output conversion:
  - AVCC length-prefixed NAL units become Annex B.
  - SPS and PPS from the format description become a config `VideoUnit`.
  - Key-frame flags and PTS are mapped.

  The output feeds `MediaHub` through the same `mpsc::Receiver<VideoUnit>` plus
  reset callback that Android uses (ruling R4 of the Android plan). A reset
  forces the next frame to be a key frame. A change of output size (a
  rotation, or a new surface) tears down and recreates the session, then emits
  a new config unit.
- **Pacing.** Frames go out only on `frameCallback`, capped at 60 fps.
  Backpressure, slow-viewer handling and the JPEG fallback rules are unchanged
  from Android.
- **JPEG.** `snapshot_jpeg` locks the current surface, honors its row stride,
  rotates, and encodes with `jpeg-encoder` (already a dependency) at the
  requested width. In view-only mode it uses
  `xcrun simctl io <udid> screenshot --type=jpeg <file>` instead, which is
  already rotated. (Correction, Task 6: Xcode 27 no longer writes screenshots
  to stdout; `-` becomes a file named `-`, so the server writes a temporary
  file and reads it.)
- **Threading and exceptions.**
  - Callbacks run on a private serial dispatch queue and hand work to Tokio
    through bounded channels. No Tokio runtime call happens on a dispatch
    thread, and no CoreFoundation object crosses threads unretained.
  - The IO ports and descriptors are remote proxies that raise Objective-C
    exceptions on unknown selectors. Every send to them is gated by
    `conformsToProtocol:`/`respondsToSelector:` and wrapped in `objc2`'s
    exception catching, so no exception unwinds through Rust.

## 5. iOS input (`hid.rs`)

Amended after the spikes (§0 Results).

- **Path.** All input goes through **DTUHID**: an XPC connection, made inside
  the server process, to the guest's
  `com.apple.coredevice.feature.remote.hid.digitizer` service. The connection
  is made with the libxpc `*_4sim` functions and the port from
  `-[SimDevice lookup:error:]`.
  - **Liveness.** Opening the connection sends a barrier
    `IndigoKeyboardButtonEvent` and waits for its reply: 5 attempts, a 4 s
    reply timeout each, 4 s apart. A port alone does not prove `dtuhidd` is
    alive.
  - **Fallback.** If DTUHID never answers, touch and buttons fall back to the
    legacy Indigo messages through `SimDeviceLegacyHIDClient`. Keys and text
    then return `DeviceInputError` ("The simulator is not accepting keyboard
    input yet. Try again in a few seconds."), because legacy keyboard input
    is dropped on current CoreSimulator.
- **Touch.** Normalized `x`/`y` (0..1, current orientation) are mapped into
  the **portrait** surface's ratios for the current UI orientation; for
  example, at `uiOrientation` 3, (u, v) becomes (v, 1 − u). They are sent as
  `IndigoDigitizerEvent`. **One pointer**:
  - Events whose `pointerId` is not the first active pointer are dropped.
  - A second `down` while one is active is ignored.
  - Multi-touch and pinch are out of scope for iOS v1.
- **Buttons.** `home`, `power`, `volumeUp` and `volumeDown` are
  `IndigoButtonEvent`s on usage page 0x0C, with usages 0x40, 0x30, 0xE9 and
  0xEA.
- **Keys and text.**
  - Keys are `IndigoKeyboardButtonEvent`s with HID keyboard usages.
  - Text in the US-layout printable set is sent as key down/up pairs, with
    Shift where needed.
  - Any other text goes through `xcrun simctl pbcopy <udid>` followed by
    Cmd+V. The pane notes that iOS may show its paste prompt.
  - The existing per-chunk limit (≤1000 UTF-16 units) still applies.
- **Rotation.** This uses S2's Purple GSEvent to `PurpleWorkspacePort`, and
  `canRotate` is `true`.
  - The catalog follows `uiOrientation` (§4), never what was asked.
  - Whether the UI turns depends on the foreground app; the iPhone home screen
    does not rotate.
  - The mapping of `landscape`/`reverseLandscape` to Purple values 4 and 3 is
    fixed by a real-simulator test against Android's convention.
    Result (Task 8): `landscape` is the phone turned left, as on Android
    (`ROTATION_90`) and in the client's rotate-left order, so the UI's top lies
    along the device's right edge: `landscape` is `uiOrientation` 4 and Purple
    3, and `reverseLandscape` is `uiOrientation` 3 and Purple 4.
  - `canRotate` stays on `DeviceSnapshot` (`true` on both platforms today) so
    a future runtime without the Purple port can report `false`.
- **Send failures.** A failed send surfaces as `DeviceInputError` and is
  logged at `warn` without text content.

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
