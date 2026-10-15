# Android device previews (Phase 3) — design

Status: approved in conversation 2026-10-14; spec awaiting review.
Issue: [#70](https://github.com/mubeda/BibCode/issues/70).
Research input (historical, verify before reuse):
`docs/plans/2026-10-06-internal-browser-and-mobile-emulator-research.md` §3, §5
Phase 3 items 12, 13 and 15.

## Goal

With **Device previews** turned on and an Android SDK on the environment
server, a user on any client (desktop or browser mode, local, LAN, SSH, relay)
can list the server's AVDs, start one, see it in a device pane, tap, swipe,
type, rotate, and stop it. Agents get `device_list`, `device_open`,
`device_screenshot`, and `device_close` on the existing `/mcp`. The emulator
runs on the server, never on the client, and is reaped on stop and on server
shutdown.

What the user decided:

- **Scope:** the Android MVP (issue items 1–3) **plus** the H.264 media socket
  (item 4). iOS (item 5) and SSH device hosts get their own later specs.
- **Approach 1, hybrid sources:** emulator gRPC for input, rotation,
  screenshots and the JPEG fallback; the scrcpy-server jar (video only, plus
  `RESET_VIDEO`) for H.264.
- **E2EE profiles encrypt the media socket too:** its own Noise_NK handshake
  with the pinned host key. Video never leaves Noise on a pinned profile.
- **Off by default**, behind one server setting that gates RPCs, the pane, and
  the MCP tools.

## Correction to the research input

The issue and research §3.3/§5 describe "emulator gRPC `streamScreenshot`
(JPEG)". The emulator's `emulator_controller.proto` `ImageFormat.ImgFormat` has
only `PNG`, `RGBA8888`, and `RGB888`, and the controller has no H.264 stream
(only the separate WebRTC `Rtc` service). Therefore:

- the JPEG path pulls scaled `RGB888` (`ImageFormat.width`/`height`) and
  encodes JPEG on the server;
- H.264 comes from the scrcpy-server jar, which runs on the device under
  `app_process`.

## Architecture decision record

| Decision | Chosen | Alternatives and why not |
| --- | --- | --- |
| Frame and input source | **Hybrid**: gRPC (`streamScreenshot` RGB888 → server JPEG, `streamInputEvent`, `setPhysicalModel`, `getScreenshot`, `setVmState`) + scrcpy-server video | **scrcpy-only**: input on scrcpy's unversioned control wire, and the only JPEG source is `adb exec-out screencap` (one process per frame, ≈ 1–3 fps), too slow for the browser-over-`http` scenario. **gRPC + server H.264 encode**: a native encoder library (openh264), licensing, heavy server CPU |
| Fallback transport | JPEG over a `LatestStream` RPC | It is the only path for browser mode over plain `http://<lan-ip>` (no secure context, so no WebCodecs), so it must be usable (≈ 10–15 fps) |
| H.264 transport | **Dedicated authenticated binary WebSocket** on the main listener, a documented exception to typed RPC | H.264 inside RPC: JSON + base64 (4/3), stop-and-wait (≤ 1/RTT), and `LatestStream` conflation corrupts P-frames. WebRTC: needs UDP/TURN, impossible over SSH `-L` and the relay |
| Media socket on E2EE profiles | **Separate Noise_NK channel** (same pinned host key) | Plain socket + capability ticket (the file-transfer exception) leaves LAN video readable on the wire; JPEG-only on E2EE profiles loses smoothness on the main desktop path |
| scrcpy-server provenance | **Embedded** (`include_bytes!`), pinned **v4.1**, SHA-256 checked in a unit test, Apache-2.0 notice beside it | Download on first use (Orca): network dependency at the first open, a fetch-and-verify path. v5.x: released 2026-10-05/08, too new to pin. The scrcpy wire has no compatibility guarantee, so the version is pinned in the launch arguments |
| Proto compilation | **`protox`** (pure Rust) in `build.rs` + `tonic-build` codegen; `emulator_controller.proto` vendored with its Apache-2.0 header | `protoc` on every dev machine and CI; checked-in generated code drifts silently |
| Device host | Environment server only | SSH device host (ref-preview `SshDeviceHost`): later spec |
| Concurrency | One running device per environment | Configurable limit: YAGNI until asked |

## 1. Setting

`ServerSettings.enableDevicePreviews: boolean`, default `false`
(`Schema.withDecodingDefault`), patch key `Schema.optionalKey(Schema.Boolean)`,
`validate_optional_bool` in `control.rs`, and a live `watch` channel published
after commit, exactly like `enableProfiler`.

While off:

- every `device.*` RPC fails with `DevicePreviewsDisabledError`;
- `tools/list` omits `device_*`, and a `device_*` call returns a tool error;
- the web entry points are hidden.

Turning it off stops every running device (graceful, then kill). The settings UI
asks for confirmation when a device is running ("Stops <AVD>").

## 2. Server device manager (`apps/server/src/device/`)

| Module | Responsibility |
| --- | --- |
| `sdk.rs` | Locate the SDK, list AVDs, classify availability |
| `emulator.rs` | Spawn, boot-wait, stop, reap one emulator process |
| `grpc.rs` | tonic client to `127.0.0.1:<grpc port>` with the emulator's token |
| `frames.rs` | Shared `streamScreenshot` → JPEG → `watch` per device |
| `input.rs` | Map typed input events to one `streamInputEvent` per device |
| `scrcpy.rs` | Push and run the pinned jar, forward, parse the video stream |
| `media.rs` | Media-socket fan-out, backpressure, tickets |
| `mod.rs` | `DeviceManager`: catalog, limits, shutdown |

### 2.1 SDK discovery

Search `ANDROID_HOME`, then `ANDROID_SDK_ROOT`, then `~/Library/Android/sdk`,
`~/Android/Sdk`, `%LOCALAPPDATA%\Android\Sdk`. Resolve `emulator/emulator` and
`platform-tools/adb` (`.exe` on Windows). List AVDs with
`emulator -list-avds` and check acceleration with `emulator -accel-check`, both
through `run_supervised` with timeouts and output caps. Availability is
`ready | sdkMissing | adbMissing | noAvds | accelerationUnavailable`, each with
an actionable message naming the server OS's hypervisor (KVM, HVF, WHPX).
Discovery reruns on catalog subscription and on demand, never on a hot path.

### 2.2 Lifecycle

```text
starting → booting → ready → stopping → stopped
    └──────────┴────────┴──→ failed{reason}
```

- **Spawn:** `emulator -avd <name> -no-window -no-audio -no-boot-anim
  -port <free even port 5554–5682> -grpc <free loopback port>`, plus
  `-gpu swiftshader_indirect` when the host reports no usable GPU. Process
  group via `configure_supervised_background_command_wrap`, following
  `production/managed_endpoint.rs`: identity from `NativeProcessSampler`,
  `register_identity` (`AttributionScope::External`, `AttributionKind::Helper`,
  label `android-emulator`), RAII registration, kill the unregistered child on
  `Shutdown`/`Capacity`.
- **gRPC auth:** read the token from the emulator's discovery file. The exact
  file (`pid_<PID>.ini` under the AVD `running` directory per the issue) and
  whether a flag such as `-grpc-use-token` is required are **verified against
  an installed SDK during implementation**; the fake emulator mirrors what is
  found. The client connects only to `127.0.0.1`.
  *Implementation note:* the server never sets `ANDROID_EMULATOR_HOME` and
  searches the ordered `emulator::default_discovery_dirs` (OS runtime
  directory first, then `<ANDROID_EMULATOR_HOME or ~/.android>/avd/running`),
  accepting only a `pid_*.ini` whose `grpc.port` matches; see
  `docs/architecture/overview.md#device-previews`.
- **Boot:** `booting` once gRPC answers `getStatus`; `ready` once
  `adb -s emulator-<port> wait-for-device` and `getprop sys.boot_completed`
  return `1`, within 180 s, otherwise `failed{bootTimeout}` and the process is
  stopped.
- **adb:** the user's adb server on loopback 5037. BiBCode never passes `-a`.
- **Stop:** `note_stop_requested`, gRPC `setVmState(SHUTDOWN)`, then kill the
  group after 10 s, then `wait()`.
- **Crash:** an exit not preceded by a stop request becomes
  `failed{exited}` and `report_exit(Watch::Failures, …)` raises
  `subprocess.crash`.
- **Limit:** one running (non-`stopped`/`failed`) device per environment;
  `device.open` otherwise fails with `DeviceLimitError{running}`.
- **Shutdown:** `DeviceManager::shutdown` runs in `quiesce_for_update` after
  `preview_gateway.shutdown` and before
  `terminal_services.freeze_process_ownership()`, so emulators are stopped
  gracefully first and still killed by ownership teardown if that fails.

### 2.3 State

`DeviceManager` owns `watch<DeviceCatalog>`:

```text
DeviceCatalog {
  availability: { kind, message }
  avds: [ { name, displayName } ]
  devices: [ { deviceId, avdName, state, failure?, width?, height?, orientation } ]
}
```

It is the single source of truth for the pane, MCP, and the frame and media
producers.

## 3. Typed RPC

Methods live in `packages/contracts` (`WS_METHODS`, `WsRpcGroup`, the RPC wire
parity manifest), `rpc/methods.rs`, `auth/scope.rs`, and a
`register_device_rpc` called from `production/runtime.rs`.

| Method | Mode | Scope | Notes |
| --- | --- | --- | --- |
| `device.subscribeCatalog` | `read_stream` (LatestStream) | `orchestration:read` | |
| `device.open {avdName}` | `mutation_unary` | `orchestration:operate` | returns `{deviceId}` at `starting`; boot progress via the catalog |
| `device.close {deviceId}` | `mutation_unary` | `orchestration:operate` | waits for stop (≤ 10 s); in `is_long_running` |
| `device.input {deviceId, events}` | `mutation_unary` | `orchestration:operate` | see 3.1 |
| `device.rotate {deviceId, orientation}` | `mutation_unary` | `orchestration:operate` | `portrait \| landscape \| reversePortrait \| reverseLandscape` via `setPhysicalModel` |
| `device.subscribeFrames {deviceId}` | `read_stream` (LatestStream) | `orchestration:read` | see 3.2 |
| `device.createMediaTicket {deviceId}` | `read_unary` | `orchestration:read` | see 4.2 |

### 3.1 Input

`events` is a non-empty array (≤ 64) of:

- `touch { action: down | move | up | cancel, pointerId, x, y }`, with `x`, `y`
  normalized to `0..1` in the device's current orientation;
- `key { key: back | home | appSwitch | power | volumeUp | volumeDown | enter |
  backspace | delete | tab | arrowUp | arrowDown | arrowLeft | arrowRight,
  action: down | up }`;
- `text { text }`, at most 1,000 characters.

The server forwards them on one long-lived gRPC `streamInputEvent` per device,
reopened on error. Unary RPCs may run concurrently, so ordering is the
client's job: it keeps **one** `device.input` in flight and coalesces pointer
moves per animation frame while waiting.

### 3.2 JPEG frames

One gRPC `streamScreenshot(RGB888, width ≤ 720)` per device, started on the
first `device.subscribeFrames` and stopped when the last one ends. Each frame
is encoded at quality 70 with `jpeg-encoder` on `spawn_blocking`, capped at 15
fps, and published to a per-device `watch` that the `LatestStream` handlers
read, so stale frames are dropped. Value:
`{ seq, width, height, orientation, jpegBase64 }`.

## 4. H.264 media socket

A documented exception to "normal application traffic uses typed RPC", limited
to device video.

### 4.1 Source

On the first media subscriber, `scrcpy.rs`:

1. pushes the embedded jar to `/data/local/tmp/bibcode-scrcpy-server-v4.1.jar`
   (skipped when the remote SHA-256 matches);
2. starts `adb -s <serial> shell CLASSPATH=<jar> app_process /
   com.genymobile.scrcpy.Server 4.1 scid=<random> tunnel_forward=true
   video=true audio=false control=true max_size=1280 video_bit_rate=4000000
   max_fps=30 send_device_meta=false`, as a registered helper process
   (label `android-scrcpy`);
3. `adb forward tcp:0 localabstract:scrcpy_<scid>` on loopback, then opens the
   video and control sockets in scrcpy's order;
4. parses the codec header and per-packet headers (config flag, key-frame
   flag, PTS) into `config | key | delta` units, caching the latest config.

*Implementation note:* scrcpy v4.1 sends a dummy byte, a 4-byte codec id, and
a 12-byte session packet, and marks packets with bit 63 (session), bit 62
(config), and bit 61 (key frame). The living description of that wire format
is in `docs/architecture/overview.md#device-previews`.

Control is used only for `RESET_VIDEO`. The session stops (process reaped,
forward removed) when the last subscriber leaves, the device stops, or on
shutdown.

### 4.2 Tickets and routes

`device.createMediaTicket` returns `{ ticket, expiresAt }`: 60 s, single use,
bound to the session principal, the device, and the channel it was minted on
(`plain` or `e2ee`). Routes on the main listener (so the relay hostname and
SSH `-L` reach them unchanged):

- `GET /api/device-media` — plain WebSocket; the first text message is
  `{"type":"ticket","ticket":"…"}`;
- `GET /api/device-media-e2ee` — Noise_NK_25519_ChaChaPoly_SHA256 with the
  pinned host key, reusing `E2eeChannel` and the record format (≤ 65,518-byte
  chunks with continuation flags); the first logical message is the ticket.
  The handshake and record layer are factored out of the RPC auth flow; the
  pre-auth admission limits apply.

The ticket goes in the first message, never in the URL, and cookies are not
accepted on these routes. Rejections close with a typed reason
(`expired | reused | wrongDevice | wrongChannel`); an `e2ee` ticket is refused
on the plain route.
*Implementation note:* the full close-reason list, the Noise prologue
`bibcode-device-media/1`, and the shared pre-auth admission are in
`docs/architecture/remote.md#device-media-sockets`.

### 4.3 Frames and backpressure

Server → client binary messages: `[u8 kind (0 config, 1 key, 2 delta)]
[u64 big-endian PTS µs][Annex-B payload]`. Client → server: only
`{"type":"keyframe"}` after a decoder error.

Each socket has a 2 MiB outbound queue. On overflow the server discards queued
deltas, marks the socket as waiting for a keyframe, forwards nothing until the
next key unit, and sends `RESET_VIDEO` (at most once per second per device).
Deltas are never conflated or skipped mid-GOP. A new subscriber receives the
cached config, and a `RESET_VIDEO` is sent. At most 4 media sockets per device.

### 4.4 Client selection

The device worker uses H.264 when `VideoDecoder` exists and
`isConfigSupported` accepts the stream's codec string; otherwise it uses JPEG.
No first frame within 5 s, a socket rejection, or a decoder error switches the
pane to JPEG at runtime. A reconnect requests a new ticket.

| Client | Context | Path |
| --- | --- | --- |
| Desktop (any profile) | secure | H.264 (Noise on E2EE profiles) |
| Browser over relay `https` | secure | H.264 |
| Browser over LAN `http://<ip>` | insecure | JPEG |

## 5. Client

### 5.1 Pane

New right-panel kind `device`: `RIGHT_PANEL_KINDS`, the `RightPanelSurface`
union, the parser (`RIGHT_PANEL_STORAGE_VERSION` 9 → 10, with old states
migrating unchanged), the `RightPanelTabs` title and icon, and the `ChatView`
content branch. Opened from the right-panel menu and the command palette
("Open device preview"); both are hidden while the setting is off. The device
belongs to the environment; panel state stays per client. Two clients may
drive one device; interleaved input is accepted and documented.

`components/device/DevicePanel.tsx`:

- toolbar: AVD picker, Start/Stop, rotate left/right, Back, Home, Recents,
  Power, Volume, and a state pill (`Starting…`, `Booting…`, `Ready`,
  `Failed: <reason>`); every disabled control has a tooltip saying why;
- empty and error states with the action: SDK missing (set `ANDROID_HOME` for
  the server and restart it), no AVDs (create one in Android Studio's Device
  Manager), acceleration unavailable (names KVM, HVF, or WHPX), device limit
  (names the running device, with Stop).

### 5.2 Rendering and input

`DeviceScreen` renders one `<canvas>` letterboxed to the device's aspect
ratio inside a plain rounded bezel.

- **H.264:** a dedicated worker owns the media WebSocket, the Noise decrypt
  (`client-runtime` `e2ee/noise.ts`, `e2ee/frame.ts`, pure `@noble`), and the
  `VideoDecoder`, and draws to a transferred `OffscreenCanvas`; where that is
  missing it transfers each `VideoFrame` to the main thread.
- **JPEG:** `createImageBitmap(blob)` → `drawImage`, keeping only the latest
  frame.
- `packages/client-runtime/src/device/` builds the media-socket URL and E2EE
  parameters from the prepared connection, for browser and Tauri alike.
- **Pointer:** `setPointerCapture` on the canvas, mapped through the letterbox
  and rotation to `0..1`; moves coalesced per animation frame; one
  `device.input` in flight.
- **Keyboard** (canvas focused): printable characters → `text`; Backspace,
  Delete, Enter, Tab, arrows → `key`. Escape is not captured. Wheel is out of
  scope.

### 5.3 Settings

A "Device previews (Android emulator)" toggle explains that the emulator runs
on the environment's server and lists its requirements (Android SDK with an
AVD, hardware acceleration).

## 6. Agent tools (`production/connect_mcp.rs`)

`tool_descriptors()` takes the live setting: `preview_*` always, `device_*`
only while on. `call_tool` dispatches by prefix to the preview invoker or a new
device invoker (a `DeviceManager` handle). A call after the setting is turned
off returns a tool error ("Device previews are off in Settings"). The endpoint
has no push channel, so providers that cache `tools/list` see a toggle on their
next session; this is documented.

| Tool | Behavior |
| --- | --- |
| `device_list` | availability, AVDs, devices (structured JSON + text) |
| `device_open {avdName?}` | omitted name uses the only AVD, else a tool error listing them; waits for `ready` (≤ 180 s); returns `{deviceId, width, height}`; operate required |
| `device_screenshot {deviceId?}` | gRPC `getScreenshot(PNG)` scaled to ≤ 1080 px wide, returned via `snapshot_tool_result` (text + image + `structuredContent`); omitted id uses the only running device |
| `device_close {deviceId}` | stops it; operate required |

Agent input tools (tap, type) are out of scope.

## 7. Errors

Typed contract errors: `DevicePreviewsDisabledError`,
`DeviceSdkUnavailableError{kind, message}`, `DeviceLimitError{running}`,
`DeviceNotFoundError`, `DeviceNotReadyError{state}`. Media-socket close
reasons: `expired | reused | wrongDevice | wrongChannel | limit`. Each maps to
an actionable pane message and MCP tool error.

Reconnect: the catalog and frame streams resubscribe; the H.264 worker mints a
new ticket and receives the cached config plus a reset. A failed device shows
its reason and Start is enabled again.

## 8. Profiler, crash, and trace coverage

- **Child processes:** emulator and scrcpy `adb shell` registered with
  `register_identity` (Helper) and reported through
  `report_exit(Watch::Failures)`; requested stops call `note_stop_requested`.
  Short `emulator`/`adb` commands use `run_supervised`.
- **RPC:** registered through the `session.rs` wrappers; `device.close` in
  `is_long_running`.
- **Shutdown:** `DeviceManager::shutdown` before the profiler teardown (§2.2).
- **Events:** new `device.lifecycle` `{phase: start|ready|fail|stop,
  durationMs, failureKind?}`. No AVD paths, frames, input, or text recorded.
  `tracing::warn!` on gRPC, adb, scrcpy, and media-socket failures.
- **Docs:** `docs/skills/bibcode-profiler/references/` and the overview's
  Profiler section.

## 9. Testing

Rust (hermetic):

- fake `emulator`/`adb` executables in a temp SDK: discovery and each
  availability kind, boot timeout, stop escalation to kill, reaping on
  `shutdown()`, crash → `failed` + exit report, device limit, setting-off stop;
- in-process tonic `EmulatorController` fake: RGB888 → JPEG, scaling,
  conflation, producer start/stop with subscribers, input mapping and order,
  rotation, screenshot;
- fake scrcpy source over loopback TCP: header parsing, slow reader drops
  deltas until the next key unit, `RESET_VIDEO` rate limit, late joiner gets
  the config, 4-socket cap;
- tickets: expired, reused, wrong device, wrong channel;
- E2EE media handshake interop with the TS client (pattern of
  `client-runtime/src/e2ee/serverInterop.test.ts`);
- RPC wire parity manifest, scope completeness, `tools/list` on and off,
  `device_screenshot` image block, embedded jar SHA-256.

Web and client-runtime:

- right-panel v9 → v10 migration; input mapping through the letterbox and all
  four orientations; move coalescing with one request in flight; codec
  selection and the 5 s fallback; latest-only JPEG rendering; worker frame
  parser; empty and error states.

Native acceptance (a real AVD on a Linux KVM server, driven from a remote
desktop client and a browser-mode client) needs an Android SDK host; this
development Mac has none. It is a required open item, added to the
`docs/testing/` runbooks.

## 10. Documentation

- `docs/architecture/overview.md`: device subsystem, process topology,
  Profiler section;
- `docs/architecture/rpc-and-orchestration.md`: `device.*` methods and the
  media-socket exception as an invariant;
- `docs/architecture/remote.md`: media socket over relay, SSH, E2EE; security
  boundaries; current limitations;
- `docs/reference/workspace-layout.md` and `docs/user/workspace-ui.md`;
- `docs/testing/` runbooks: device-preview validation;
- third-party notices for the scrcpy-server jar and the vendored proto
  (Apache-2.0).

## 11. Delivery order

1. Setting and contracts.
2. Device manager (SDK, lifecycle, catalog, shutdown).
3. gRPC: JPEG frames, input, rotation.
4. Device pane (JPEG path).
5. MCP tools.
6. Media socket (plain) and H.264 worker.
7. E2EE media channel.
8. Documentation and runbooks.

## Out of scope

iOS Simulator, SSH device hosts, physical USB devices, agent input tools,
audio, wheel input, device skins, multiple displays and foldable postures.
