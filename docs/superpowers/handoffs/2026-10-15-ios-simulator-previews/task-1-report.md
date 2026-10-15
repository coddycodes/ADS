# Task 1 report: Spikes S1–S3

Status: DONE_WITH_CONCERNS. The concerns are that headless/SSH and the logged-out console were not testable, the vendor-HID rotation path was not tried on iOS 27.0, and `vp fmt` skips the spec file.

## Environment

- macOS 27.0.1 (26A434), Xcode 27.0 at `/Applications/Xcode.app` (`xcode-select -p` = `/Applications/Xcode.app/Contents/Developer`).
- CoreSimulator `CFBundleVersion` **1171.7**. That is newer than 1155.4, the version from which idb says legacy Indigo input is suppressed.
- iPhone 17 Pro `308FD3CD-F46A-44B9-9B68-9A849F337C80`, runtime **iOS 26.5**. It was booted with `xcrun simctl boot` and `bootstatus -b`. **Simulator.app was not running** for the whole spike.
- At the end it was shut down with `xcrun simctl shutdown`, and `simctl list` showed `(Shutdown)`.

## Probes

The probes are in `/private/tmp/claude-501/-Users-admin-projects-BibCode/3f37dbc9-24b5-456b-aeef-1916d9791336/scratchpad/ios-spike/` and were not committed:

- `common.h` holds the helpers: dlopen, device lookup, the method and protocol dumps, and IOSurface description.
- `fb.m` is S3: `./fb <udid> legacy|screen|both <secs>`. `DUMP=1` also dumps the selectors.
- `hid.m` is S1/S2. Its modes:
  - `indigo-tap`, `indigo-raw-tap`, `indigo-button`, `indigo-key`, `indigo-arbitrary`
  - `dtu-tap`, `dtu-button`, `dtu-key`, `dtu-rotate`
  - `purple-rotate`, `darwin`
  - `methods <filter>`, `watch <secs>`, `props`, `dumpfb <png>`
- Build commands:
  - `xcrun clang -fobjc-arc -framework Foundation -framework IOSurface fb.m -o fb`
  - `xcrun clang -fobjc-arc -framework Foundation -framework IOSurface -framework IOKit -framework CoreGraphics -framework ImageIO hid.m -o hid`
- Deviation from the brief: the probes are Objective-C, not Swift. `@try`/`@catch` around ROCK proxy calls and direct `class_copyMethodList`/`protocol_copyMethodDescriptionList` introspection are simpler in ObjC. The names and signatures are the same.
- Reference: facebook/idb `main`, which is now Swift. The files were:
  - `FBSimulatorControl/Framebuffer/FramebufferSurface.swift`
  - `HID/SimulatorIndigoHID.swift`, `SimulatorIndigoHIDClient.swift`, `SimulatorHIDTransportSelection.swift`, `SimulatorDTUHIDConnection.swift`, `SimulatorDigitizerHIDTransport.swift`, `DTUHIDModels.swift`, `SimulatorVendorHIDTransport.swift`, `SimulatorPurpleHID*.swift`
  - `XPC/SimulatorXPCConnection.swift`
  - `PrivateHeaders/{SimulatorApp/Indigo.h, CoreSimDeviceIO/*.h, SimulatorKit/SimDeviceLegacyClient.h}`

  The old `.m` paths named in the brief no longer exist. They were downloaded to `scratchpad/ios-spike/idb/` and not vendored.

## S3: framebuffer (confirmed)

Lookup chain, all through `objc_msgSend`. KVC raises on the ROCK proxies (`valueForUndefinedKey:` for `descriptor`).

1. `+[SimServiceContext sharedServiceContextForDeveloperDir:error:]`
2. `-defaultDeviceSetWithError:`
3. `-devices`
4. Match `-UDID`
5. `-[SimDevice io]` returns a `SimDeviceIOClient`.
6. `-ioPorts` returns 14 `ROCKRemoteProxy-…-SimDeviceIOPortInterface` ports.
7. `-descriptor`

The main display descriptor's class is `ROCKRemoteProxy-…-SimScreen-SimDeviceIOPortDescriptorInterface-ROCKImpersonateable-SimDisplayIOSurfaceRenderable-SimDisplayResizeableRenderable-SimDisplayRenderable`. Its `-state` is a `ROCKImmutableProxy` with `displayClass` 0. There are also two class-1 displays.

Output of `fb` (abridged):

```
state.defaultWidthForDisplay=1206 defaultHeightForDisplay=2622 defaultPixelFormat='BGRA' (0x42475241)
conformsTo SimScreen=1 SimDisplayIOSurfaceRenderable=1 SimScreenAdapter=0
framebufferSurface: class=IOSurface id=1466 1206x2622 bpr=4864 bpe=4 planes=0 fmt='BGRA' (0x42475241) alloc=12763136
maskedFramebufferSurface: ... same shape (id=1478)
ioSurface raised -[ROCKRemoteProxy-... ioSurface]: unrecognized selector sent to instance
# legacy, 5 s with Settings launch/terminate:
damageRectanglesCallback rects=() thread-main=0
RESULT: ioSurfacesChange=0 damage=158 (empty=158)
# idle 3 s: damage=6
# SimScreen, 5 s with Settings launch/terminate:
surfacesChanged fb: id=1466 1206x2622 BGRA ; masked id=1478
RESULT: screenFrame=130 screenSurfacesChanged=1 screenProps=0
```

Protocol selectors and type encodings, from `protocol_copyMethodDescriptionList`:

- `SimDisplayIOSurfaceRenderable`:
  - `registerCallbackWithUUID:ioSurfacesChangeCallback:` `v32@0:8@16@?24`
  - `unregisterIOSurfacesChangeCallbackWithUUID:` `v24@0:8@16`
  - `framebufferSurface` `@16@0:8`
  - `maskedFramebufferSurface` `@16@0:8`
  - The singular `ioSurfaceChangeCallback` and `ioSurface` are **gone** on Xcode 27.
- `SimDisplayRenderable`:
  - `registerCallbackWithUUID:damageRectanglesCallback:` `v32@0:8@16@?24`
  - `unregisterDamageRectanglesCallbackWithUUID:`
  - `registerCallbackWithUUID:displayPropertiesChanged:`
  - `unregisterDisplayPropertiesChangedCallbackWithUUID:`
  - `displaySize` `{CGSize=dd}`
  - `displayPitch` `Q`
  - `displaySizeInBytes` `Q`
- `SimScreen`:
  - `registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:` `v56@0:8@16@24@?32@?40@?48`
  - `unregisterScreenCallbacksWithUUID:` `v24@0:8@16`
  - `setCurrentMode:pixelSize:carPlayProperties:completionQueue:completionHandler:`
  - `setPowerState:completionQueue:completionHandler:`
  - `screenProperties` `@16@0:8`
- `SimDisplayDescriptorState`:
  - `displayClass` `S`
  - `defaultWidthForDisplay` `I`
  - `defaultHeightForDisplay` `I`
  - `defaultPixelFormat` `I`
  - `mask` `@`
- `SimScreenProperties`:
  - `seed` `I`, `screenID` `I`, `name` `@`, `uniqueId` `@`, `deviceName` `@`
  - `powerState` `i`, `screenType` `Q`, `uiOrientation` `I`
  - `pixelSize` `{CGSize=dd}`, `dotPitch` `d`
  - `maskPDF`, `framebufferMaskIdentifier`, `carPlayProperties`, `chromeIdentifier`, `currentMode`, `supportedScreenModes`, `backlight`
  - `regionOfInterest` `{CGRect=…}`
- `SimDeviceIOClient`/`SimDeviceIO` also have:
  - `postDarwinNotification:error:`
  - `ioPortForUUID:`
  - `connectIOPort:completionQueue:completionHandler:`
  - `setDynamicIslandSuppressed:error:`

Outcomes:

- Pixel format `'BGRA'` (0x42475241). The surface is 1206×2622, 4864 bytes per row (padded, so the encoder must honour the stride) and non-planar.
- The damage callback is a bare tick with an empty array. Use `SimScreen` `frameCallback`.
- `surfacesChanged` fires once at registration with the current surface. The frame callback does not carry the surface, so read `framebufferSurface` again on `surfacesChanged`.
- After rotation the IOSurface is **still 1206×2622 portrait** (same id 1466). The content is drawn rotated (checked by dumping the surface to PNG: `fb-landscape.png`). The rotation shows up in two places:
  - `propertiesChangedCallback` fires.
  - `screenProperties.uiOrientation` changes.

  `simctl io screenshot` returns 2622×1206 because it is rotated for you.

## S1: input

Indigo client: `NSClassFromString(@"SimulatorKit.SimDeviceLegacyHIDClient")` has these methods:

- `-initWithDevice:error:` `@32@0:8@16^@24`
- `-initWithDevice:sessionResetQueue:error:sessionResetHandler:` `@48@0:8@16@24^@32@?40`
- `-resetHIDSession`
- `-sendWithMessage:freeWhenDone:completionQueue:completion:` `v44@0:8^{IndigoHIDMessageStruct=…}16B24@28@?36`

Exported Indigo builders (`nm -gU`):

- `IndigoHIDMessageForButton`
- `ForHIDArbitrary`
- `ForKeyboardArbitrary`
- `ForKeyboardNSEvent`
- `ForMouseNSEvent`
- `ForScrollEvent`
- `ForTrackpadMoveEvent`
- `ForDeviceMotionLiteEvent`
- `ForDigitalCrownEvent`
- `ForGameControllerEvent`
- `ForPressureEvent`
- …and `IndigoHIDMessageToCreate/Remove{Mouse,Pointer,CarPlay}Service`

Signatures used, taken from idb and confirmed working:

- `IndigoMessage *IndigoHIDMessageForMouseNSEvent(CGPoint *p1, CGPoint *p2OrNULL, uint32_t target /*0x32*/, NSEventType /*1 down, 2 up*/, CGSize /*{1,1}*/, uint32_t edge /*0*/)`.
  - The builder returns a 384-byte message (`innerSize` 0xA0, eventType 2, eventKind 0xB, touch.field1 0x400002, field2 1, eventMask 3, range/touch 1 on down and 0 on up).
  - The probe re-enveloped it into idb's 320-byte single-touch layout (see spec §0) and sent that.
- `IndigoMessage *IndigoHIDMessageForButton(int32_t source, int32_t direction, int32_t target /*0x33*/)` returns 192 bytes.
- `IndigoMessage *IndigoHIDMessageForHIDArbitrary(int32_t target /*0x32*/, uint32_t page, uint32_t usage, int32_t direction)`.
- `IndigoMessage *IndigoHIDMessageForKeyboardArbitrary(int32_t keyCode, int32_t direction)`.

Tap verification: the brief said to tap the centre, but the home-screen centre (0.5, 0.5) on this device is empty wallpaper. The probe tapped the Settings icon at (0.848, 0.483) instead and compared screenshots, plus an IOSurface hash before and after.

| Input                                                                      | Result                                                                                       |
| -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| Indigo touch                                                               | Settings opened, 5 of 5 cycles.                                                              |
| Indigo Home (source 0)                                                     | Home screen, 5 of 5 cycles.                                                                  |
| Indigo Lock (source 1)                                                     | Lock screen.                                                                                 |
| Indigo `HIDArbitrary(0x32, 0x0C, 0xE9)` volume up                          | `sim_volume` 6→12→18, observed in `audiosettings.plist`.                                     |
| Indigo `KeyboardArbitrary` 0x04–0x08 (HID) and 0x02 (HIToolbox)            | **No text** in a focused Settings search field. Dropped, as idb says.                        |
| DTUHID digitizer start/end                                                 | Opened Settings, and opened a new Safari tab in landscape.                                   |
| DTUHID `IndigoButtonEvent` Home (0x0C, 0x40)                               | Worked.                                                                                      |
| DTUHID `IndigoButtonEvent` Power (0x0C, 0x30)                              | Locked the device.                                                                           |
| DTUHID `IndigoButtonEvent` Vol+/− (0xE9/0xEA)                              | 18→25 and 12→6.                                                                              |
| DTUHID `IndigoKeyboardButtonEvent` 0x04 0x05 0x06                          | Typed "Abc", auto-capitalized.                                                               |

DTUHID connection: it is in-process and starts no helper.

- `dlsym(RTLD_DEFAULT, …)` resolved all three symbols:
  - `xpc_endpoint_create_mach_port_4sim(mach_port_t, uint64_t, uint64_t)`
  - `xpc_connection_create_from_endpoint(xpc_object_t)`
  - `xpc_connection_enable_sim2host_4sim(xpc_connection_t)`
- `-[SimDevice lookup:error:]` (`I32@0:8@16^@24`) on `com.apple.coredevice.feature.remote.hid.digitizer` returned a port.
- The barrier `IndigoKeyboardButtonEvent{usageCode 0, state 2}` with `isBarrier = true`, sent with a reply, answered on the first attempt with an empty dictionary.
- Messages are sent with `xpc_connection_send_message` followed by `xpc_connection_send_barrier`.

Coordinates: both Indigo and DTUHID take ratios in **portrait-framebuffer space**. At `uiOrientation` 3 (after Purple value 4), these taps hit the expected controls:

| Landscape view ratio (u, v)   | Sent as (v, 1−u) | Control hit                                                               |
| ----------------------------- | ---------------- | ------------------------------------------------------------------------- |
| (0.5, 0.08)                   | (0.08, 0.5)      | Safari URL field. Sending (0.5, 0.08) unchanged did nothing.              |
| (0.84, 0.08), the "+" button  | (0.08, 0.16)     | The "+" button, which opened a tab.                                       |

Headless:

- Booting, the callbacks and HID all work with no Simulator.app.
- `ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=<scratch>/known_hosts localhost` failed with `Permission denied (publickey,password,keyboard-interactive)`. So **S1 over SSH: NOT CONFIRMED (no key auth)**. The only file written was the scratch known_hosts; `~/.ssh` was not touched.
- **Logged-out console: NOT CONFIRMED.** The probe cannot log the GUI out.

## S2: rotation

The probe first used `methods <filter>` to list `SimDevice`, `SimDeviceIOClient` and every class in the SimulatorKit image:

- `orient`: none.
- `rotat`: only `-[SimDevice updateNearbyInteractionDeviceRect:deviceRotation:error:]` and `-[SimulatorKit.EventHandlingView rotateWithEvent:]`. Neither is an orientation setter.

The options were then tried in order:

1. A `SimDevice` orientation selector: none exists.
2. `IndigoHIDMessageForHIDArbitrary(0x32, 0x20 /*Sensor*/, 0x8A /*Device Orientation*/, down/up)`: `uiOrientation` stayed 1.
3. `-[SimDevice postDarwinNotification:@"com.apple.springboard.rotate" error:]` returned YES. Nothing changed: no frames, and the screenshot stayed 1206×2622.
4. **Purple GSEvent works.** It goes to the `PurpleWorkspacePort` from `-[SimDevice lookup:error:]`, as a 112-byte mach message (layout in spec §0); `mach_msg` returned `KERN_SUCCESS`. The Safari runs showed:

   | Purple value sent | `uiOrientation` read back | Screenshot |
   | ----------------- | ------------------------- | ---------- |
   | 4                 | 3                         | 2622×1206  |
   | 3                 | 4                         | 2622×1206  |
   | 2                 | 4 (unchanged)             | —          |
   | 1                 | 1                         | 1206×2622  |

   Safari does not support upside-down on iPhone, which is why value 2 changed nothing. The `watch` run during the rotation showed `propertiesChanged` and 62 frames, with the surface unchanged.
5. idb's vendor-defined DTUHID path made no change on iOS 26.5:
   - The liveness barrier succeeded.
   - The service was `…remote.hid.vendordefined`.
   - The message was `IndigoVendorDefinedEvent{usagePage 0xff61, usage 0x5b, version 0, data = IOCFSerialize(binary) of {provider, source: orientation-picker-control, type: enum, value}}`, with values `landscape-left`, `landscape-right` and `portrait`.

   idb uses this path only where `com.apple.coredevice.feature.monitormotion` reports `deviceMotionState`, so it is UNCONFIRMED on iOS 27.0.

Outcome: rotation is available via Purple, so `canRotate` can be `true`.

## Spec

The probe appended `### Results (2026-10-15)` to §0 of `docs/superpowers/specs/2026-10-15-ios-simulator-previews-design.md`. `vp fmt <file>` printed "Expected at least one target file. All matched files may have been excluded by ignore rules", so the file is excluded from the formatter and was left as hand-formatted Markdown.

## Concerns and impact on later tasks

1. **§5 must change for keys and text.** `IndigoHIDMessageForKeyboardArbitrary` is dropped on CoreSimulator 1171.7, so keys need DTUHID `IndigoKeyboardButtonEvent` over the sim2host XPC connection. Home, lock and volume work on both paths.
   - Recommendation: use DTUHID for everything, with Indigo touch and buttons as the fallback. idb reports that legacy touch is intermittent from 1155.4 on, even though it was 5 of 5 here.
   - DTUHID adds a liveness and retry step at connect, because `dtuhidd` is demand-launched and can die early in boot.
2. **§4: the IOSurface does not change size on rotation.** The encoder keeps 1206×2622. Either the encoder rotates the frames per `uiOrientation`, or the client rotates the view and maps touches back to portrait ratios. The landscape mapping (v, 1−u) was observed for `uiOrientation` 3 only; values 2 and 4 are UNCONFIRMED.
3. ObjC exceptions from ROCK proxies (unknown selectors, KVC) must not unwind into Rust. Call only the confirmed selectors.
4. Only iOS 26.5 was exercised; the iOS 27.0 runtime is UNCONFIRMED.
5. Headless over SSH and the logged-out console: NOT CONFIRMED.
6. **The new §0 contradicts other spec sections**, which were left alone because the task scope is §0 only. The controller should route these:
   - §5 says keys and text use Indigo. They need DTUHID `IndigoKeyboardButtonEvent`.
   - §5 says touch takes "x/y (0..1, current orientation)". Both paths actually take portrait-framebuffer ratios whatever the orientation.
   - §4 says "a size change (rotation) tears down the session". The surface never changes size. Rotation arrives through `propertiesChangedCallback` / `uiOrientation`, so either the encoder or the client must rotate. The 4864-byte stride must also be honoured.
   - The ADR row "Input: Indigo HID messages" should become "DTUHID primary, Indigo fallback".
7. The Purple names "landscape-right" (3) and "landscape-left" (4) are idb's labels and were not observed here. Only the effect was recorded: after value 4, the UI top lies along the physical left edge of the surface. The commit was amended to say so, and also to state that DTUHID liveness needs a barrier with retries. The final commit is 26b2af03.

## Fix round 1

### Probe re-runs

The iPhone 17 Pro was booted on iOS 26.5, with Simulator.app not running. Afterwards it was shut down, and `simctl list` showed `(Shutdown)`.

1. **Encodings.** A new `enc.m` printed these type encodings:
   - `+[SimServiceContext sharedServiceContextForDeveloperDir:error:] @32@0:8@16^@24`
   - `-[SimServiceContext defaultDeviceSetWithError:] @24@0:8^@16`
   - `-[SimDeviceSet devices] @16@0:8`
   - `-[SimDevice UDID] @16@0:8`
   - `-[SimDevice state] Q16@0:8`
   - `-[SimDevice io] @16@0:8`
   - `-[SimDevice lookup:error:] I32@0:8@16^@24`
   - `-[SimDevice postDarwinNotification:error:] B32@0:8@16^@24`
2. **Indigo tap at `uiOrientation` 3.** Safari was in the foreground. `purple-rotate 4` returned `mach_msg -> 0`, and props showed `uiOrientation=3`. `indigo-tap 0.08 0.16` sent 320 bytes twice with completion ok, and the surface changed. The screenshot went from 2 Safari tabs to 3, so the "+" was hit.
3. **Taps at `uiOrientation` 4.** `purple-rotate 3` returned `mach_msg -> 0`, and props showed `uiOrientation=4`.
   - `dumpfb` showed the UI top along the physical right edge of the portrait surface, with "+" near physical (0.92, 0.84).
   - `dtu-tap 0.92 0.84` passed liveness and took the tabs from 3 to 4.
   - `indigo-tap 0.92 0.84` took the tabs from 4 to 5.

   So the mapping at `uiOrientation` 4 is (u, v) → (1 − v, u), confirmed on both paths.
4. Purple 1 restored portrait before shutdown.

### Spec §0 edits (only §0 was changed)

1. Coordinates are now a table by `uiOrientation` and path:
   - 1, 3 and 4 are confirmed on both Indigo and DTUHID.
   - 2 is marked UNCONFIRMED and inferred.
2. The Purple orientation values are split into observed and inferred:
   - Observed: 1 → portrait; 4 → UI top along the physical left edge; 3 → UI top along the physical right edge (now observed).
   - From idb, untested: 2 means upside-down (it had no effect here), and idb's landscape names.
3. The Indigo re-envelope is now step by step:
   - All offsets count from the start of the message.
   - 0x30 is both the source and the destination offset.
   - Header 0x00–0x17 is left zero.
   - The table of builder field values is added: field1 0x400002, field2 1, eventMask 3, and range/touch 1/1 down, 0/0 up.
   - Field widths, zeroed gaps and the final send with `freeWhenDone:YES` are spelled out.
4. DTUHID now gives:
   - The connect order.
   - That one digitizer connection carries all three message types, with `featureIdentifier` always the digitizer service.
   - The exact `messageType` strings and the XPC value types.
   - The per-event `send_message` then `send_barrier` sequence, with the gesture timings and the post-send wait.
   - That a long-lived multi-gesture connection was not tested.
5. The environment line says only iOS 26.5 was exercised and that iOS 27.0 is UNCONFIRMED. The vendor-HID sentence now says the `monitormotion`/`querymotioncapabilities` query was never run.
6. The headless heading says only the no-Simulator.app case (GUI session logged in) was tested; the no-GUI-session case was not.
7. The minor items:
   - The side button 0xBB8 is marked "from idb, untested".
   - "+1/16" is replaced by the observed 6→12→18 steps, about 100/16 each.
   - The 112-byte buffer versus the 108-byte size is explained, along with the zeroed fields.
   - Block parameter types are noted as inferred from `@?`.
   - The lookup encodings are added.

`vp fmt` still reports the file as excluded by ignore rules.
