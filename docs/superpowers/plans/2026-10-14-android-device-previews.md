# Android Device Previews Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A server-hosted Android emulator that any BiBCode client can start, view (H.264 or JPEG), drive, rotate, and stop, plus `device_*` MCP tools, all behind an off-by-default setting.

**Architecture:** A Rust `device` module on the server owns SDK discovery, emulator process lifecycle, an emulator-gRPC client (input, rotation, screenshots, RGB888 frames → JPEG), and a scrcpy-server session (H.264). Clients use typed RPC for control and the JPEG fallback, and a dedicated ticket-authenticated media WebSocket (Noise-encrypted on E2EE profiles) for H.264 decoded by WebCodecs in a worker.

**Tech Stack:** Rust (tokio, axum, tonic 0.14 + tonic-prost, protox, jpeg-encoder, snow), Effect Schema contracts, React + Effect atoms, WebCodecs.

**Spec:** `docs/superpowers/specs/2026-10-14-android-device-previews-design.md` (read it before any task).

## Global Constraints

- Setting key `enableDevicePreviews`, default `false`; capability key `devicePreviews`.
- One running device per environment; boot timeout 180 s; graceful stop timeout 10 s.
- JPEG: width ≤ 720 px, quality 70, ≤ 15 fps. MCP screenshot: PNG ≤ 1080 px wide.
- scrcpy-server **v4.1**, embedded, SHA-256 asserted in a test; launch args exactly as spec §4.1 with `max_size=1280 video_bit_rate=4000000 max_fps=30`.
- Media socket: 2 MiB outbound queue per socket, `RESET_VIDEO` ≤ 1/s per device, ≤ 4 sockets per device, ticket TTL 60 s, single use.
- Input batch ≤ 64 events; text ≤ 1,000 characters; client keeps exactly one `device.input` in flight.
- Device processes listen on loopback only; never pass `adb -a`.
- No production Node runtime, no sidecar binary on the host; no `protoc` requirement (use `protox`).
- New dependency versions must be released at least two weeks before they are added; record the chosen versions in the commit message.
- Rust tests that spawn fake `emulator`/`adb` shell scripts are `#[cfg(unix)]` (CI runs `cargo test --workspace` on Linux).
- Every user-visible string follows `UI.md`: actionable errors, disabled controls explain themselves.

## Native verification items

Facts that cannot be verified on the development Mac (no Android SDK). Each task that depends on one codes against the stated assumption, keeps it in one named constant or function, and the native run (Task 14) confirms or corrects it:

1. gRPC discovery file: `<ANDROID_EMULATOR_HOME or ~/.android>/avd/running/pid_<pid>.ini` with `grpc.port=` and `grpc.token=` lines (`device::emulator::discovery_file_path`). Whether `-grpc-use-token` is needed (`device::emulator::EMULATOR_GRPC_FLAGS`).
2. Rotation vectors for `setPhysicalModel(ROTATION)` (`device::input::rotation_vector`).
3. Touch coordinates are in the current orientation's display pixels (`device::input::to_device_pixels`).
4. scrcpy v4.1 accepts the spec's arguments and emits the documented 12-byte codec header and 12-byte packet headers (`device::scrcpy`).

## Review Focus

1. **AVD name with spaces or shell metacharacters** → passed as one argv element, never through a shell; covered in Task 4 (`spawn_args_keep_avd_name_as_one_argument`).
2. **Emulator exits during boot (bad AVD, no acceleration)** → device becomes `failed{exited}` within one poll, not after the 180 s timeout; Task 4 (`boot_reports_early_exit_without_waiting_for_timeout`).
3. **Client disconnects mid-drag (pointer down sent, up never arrives)** → the server releases touches when the `device.input` stream for that device is reopened or the device stops; Task 6 (`dangling_touch_is_released_on_stream_reset`).
4. **Setting turned off while an H.264 socket and an MCP `device_open` are in flight** → the socket closes with reason `disabled`, the MCP call returns the "off in Settings" error, the emulator is reaped; Task 11 (`disabling_closes_media_sockets`) and Task 8 (`device_open_fails_when_disabled_mid_boot`).
5. **Browser tab hidden or throttled (no acks, no reads)** → JPEG stream conflates (bounded memory); H.264 socket drops to the next key unit rather than growing; Task 6 (`slow_subscriber_sees_only_latest_frame`) and Task 11 (`slow_socket_skips_to_next_key_unit`).

---

## File Structure

Server (`apps/server`):

| Path | Responsibility |
| --- | --- |
| `proto/android/emulator_controller.proto` | Vendored emulator proto (Apache-2.0, upstream `google/android-emulator-container-scripts@7c2c4dfd` `js/proto/emulator_controller.proto`) |
| `build.rs` | `protox` compile + `tonic-prost-build` codegen |
| `src/device/mod.rs` | `DeviceManager`: catalog watch, limits, setting follow, shutdown |
| `src/device/catalog.rs` | Serializable catalog/availability/state types |
| `src/device/sdk.rs` | SDK + AVD discovery, availability classification |
| `src/device/emulator.rs` | Spawn, discovery file, boot wait, stop, crash report |
| `src/device/grpc.rs` | Generated code include + authenticated client |
| `src/device/frames.rs` | Shared screenshot stream → JPEG → `watch` |
| `src/device/input.rs` | Typed input → `InputEvent` stream; rotation |
| `src/device/scrcpy.rs` | Push/run pinned jar, forward, parse H.264 units |
| `src/device/scrcpy/scrcpy-server-v4.1` | Embedded jar (binary) |
| `src/device/scrcpy/NOTICE.md` | Apache-2.0 attribution for jar and proto |
| `src/device/media.rs` | Fan-out, backpressure, tickets |
| `src/device/media_socket.rs` | `/api/device-media` and `/api/device-media-e2ee` handlers |
| `src/device/testing.rs` | `#[cfg(test)]` fake SDK scripts, fake gRPC controller, fake scrcpy source |
| `src/process/registered_helper.rs` | Spawn + identity + liveness + `register_identity` for long-lived helpers |
| `src/production/device_rpc.rs` | `device.*` RPC registration |

Contracts / client:

| Path | Responsibility |
| --- | --- |
| `packages/contracts/src/device.ts` | Schemas and tagged errors |
| `packages/client-runtime/src/state/device.ts` | Catalog/frames subscriptions, commands |
| `packages/client-runtime/src/device/mediaFrames.ts` | Media frame header parser |
| `packages/client-runtime/src/device/mediaSocket.ts` | Plain/E2EE media socket opener (worker-safe) |
| `apps/web/src/state/device.ts` | Binds atoms to the app runtime |
| `apps/web/src/components/device/*` | Pane, screen, toolbar, geometry, input queue, key map, worker |
| `apps/web/src/components/settings/DevicePreviewsSection.tsx` | Setting toggle |

---

### Task 1: Setting and capability

**Files:**
- Modify: `packages/contracts/src/settings.ts:497` (after `enableProfiler`) and `:663` (patch)
- Modify: `packages/contracts/src/environment.ts:78` (capability)
- Modify: `apps/server/src/production/control.rs:382,482,616,1958`
- Modify: `apps/server/src/lifecycle.rs:72`
- Test: `packages/contracts/src/settings.test.ts`, `apps/server/src/production/control.rs` tests module, `apps/server/src/lifecycle.rs` tests

**Interfaces:**
- Produces: `ServerControl::device_previews_enabled_signal(&self) -> watch::Receiver<bool>`; `ServerSettings.enableDevicePreviews`; `ExecutionEnvironmentCapabilities.devicePreviews`.

- [ ] **Step 1: Failing tests**

`packages/contracts/src/settings.test.ts`:
```ts
it("defaults enableDevicePreviews to false and accepts a patch", () => {
  expect(DEFAULT_SERVER_SETTINGS.enableDevicePreviews).toBe(false);
  expect(Schema.decodeUnknownSync(ServerSettingsPatch)({ enableDevicePreviews: true })).toEqual({
    enableDevicePreviews: true,
  });
});
```
`control.rs` tests (next to the `enableProfiler` assertions at ~4900):
```rust
#[test]
fn validates_enable_device_previews_as_bool() {
    assert!(validate_settings_document(&json!({"enableDevicePreviews": true})).is_ok());
    assert!(validate_settings_document(&json!({"enableDevicePreviews": "yes"})).is_err());
}

#[tokio::test]
async fn device_previews_signal_follows_committed_setting() {
    let control = test_control().await; // existing helper used by profiler signal tests
    let mut signal = control.device_previews_enabled_signal();
    assert!(!*signal.borrow());
    control.update_settings(json!({"enableDevicePreviews": true})).await.unwrap();
    signal.changed().await.unwrap();
    assert!(*signal.borrow());
}
```
(If the profiler signal test uses a different helper name, use that same helper; search `profiler_enabled_signal` in the tests module.)

`lifecycle.rs` test beside `assert_eq!(descriptor["capabilities"]["profiler"], true);`:
```rust
assert_eq!(descriptor["capabilities"]["devicePreviews"], true);
```

- [ ] **Step 2: Run, expect failures**

Run: `vp test packages/contracts/src/settings.test.ts` and `cargo test -p bibcode-server device_previews -- --nocapture`
Expected: FAIL (unknown key / missing method).

- [ ] **Step 3: Implement**

`settings.ts` (`ServerSettings`, after `enableProfiler`):
```ts
  enableDevicePreviews: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
```
patch schema (after `enableProfiler: Schema.optionalKey(Schema.Boolean),`):
```ts
  enableDevicePreviews: Schema.optionalKey(Schema.Boolean),
```
`environment.ts` (after `profiler`):
```ts
  /** `device.*` methods, the device media socket, and `enableDevicePreviews`. */
  devicePreviews: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
```
`control.rs`:
```rust
// beside `enabled_setting` import usage
fn device_previews_setting(settings: &Value) -> bool {
    settings.get("enableDevicePreviews").and_then(Value::as_bool) == Some(true)
}
```
Add field `device_previews_enabled: watch::Sender<bool>` next to `profiler_enabled`, created with `watch::channel(device_previews_setting(&settings))`; accessor:
```rust
/// The committed `enableDevicePreviews`; changes as soon as a settings update commits.
pub fn device_previews_enabled_signal(&self) -> watch::Receiver<bool> {
    self.device_previews_enabled.subscribe()
}
```
In the commit path after the profiler `send_if_modified`:
```rust
let next_device_previews = device_previews_setting(&next);
commit_control.device_previews_enabled.send_if_modified(|current| {
    let changed = *current != next_device_previews;
    *current = next_device_previews;
    changed
});
```
`validate_settings_document`: `validate_optional_bool(object, "enableDevicePreviews")?;`
`lifecycle.rs` capabilities: `"devicePreviews": true,`.

- [ ] **Step 4: Run tests, expect PASS** (same commands).

- [ ] **Step 5: Commit**
```bash
git add packages/contracts/src/settings.ts packages/contracts/src/settings.test.ts packages/contracts/src/environment.ts apps/server/src/production/control.rs apps/server/src/lifecycle.rs
git commit -m "feat(settings): add enableDevicePreviews setting and capability (#70)"
```

---

### Task 2: Emulator proto, codegen, and authenticated gRPC client

**Files:**
- Create: `apps/server/proto/android/emulator_controller.proto` (download upstream file at commit `7c2c4dfd6f18c3f85dabaf421fe51dcd4961f2ce`, keep its license header unchanged)
- Create: `apps/server/build.rs`
- Modify: `Cargo.toml` (`[workspace.dependencies]`), `apps/server/Cargo.toml` (`[dependencies]`, `[build-dependencies]`)
- Create: `apps/server/src/device/mod.rs` (module skeleton), `apps/server/src/device/grpc.rs`, `apps/server/src/device/testing.rs`
- Modify: `apps/server/src/lib.rs` (`pub mod device;`)

**Interfaces:**
- Produces:
  - `device::grpc::proto` (generated module `android.emulation.control`).
  - `pub(crate) struct EmulatorClient { inner: EmulatorControllerClient<InterceptedService<Channel, TokenInterceptor>> }`
  - `EmulatorClient::connect(port: u16, token: Option<String>) -> Result<Self, DeviceError>` (connects to `http://127.0.0.1:{port}`, 5 s connect timeout).
  - `EmulatorClient::status(&self) -> Result<proto::EmulatorStatus, DeviceError>`
  - `EmulatorClient::screenshot(&self, format: proto::image_format::ImgFormat, width: u32) -> Result<proto::Image, DeviceError>`
  - `EmulatorClient::stream_screenshot(&self, width: u32) -> Result<tonic::Streaming<proto::Image>, DeviceError>` (RGB888)
  - `EmulatorClient::input_sink(&self) -> Result<mpsc::Sender<proto::InputEvent>, DeviceError>` (spawns `stream_input_event` over `ReceiverStream`)
  - `EmulatorClient::set_rotation(&self, data: [f32; 3]) -> Result<(), DeviceError>`
  - `EmulatorClient::shutdown_vm(&self) -> Result<(), DeviceError>`
  - `device::DeviceError` (`thiserror`): `Grpc(String)`, `Io(String)`, `Protocol(String)`.
  - `#[cfg(test)] device::testing::FakeController` implementing the generated server trait, recording inputs, serving solid-color RGB888 frames; `FakeController::spawn() -> (FakeHandle { port, inputs: Arc<Mutex<Vec<InputEvent>>>, rotations: Arc<Mutex<Vec<[f32;3]>>>, frame_tx: watch::Sender<(u32,u32,u8)>, vm_state: Arc<Mutex<Option<i32>>> })`. Requires the token `"test-token"` and rejects others with `Unauthenticated`.

- [ ] **Step 1: Dependencies**

Workspace `Cargo.toml` (versions: the newest releases ≥ 2 weeks old that share one `prost` minor; at plan time tonic `0.14.x`, tonic-prost `0.14.x`, tonic-prost-build `0.14.x`, prost `0.14.x`, protox `0.9.x`, tokio-stream `0.1.x`, jpeg-encoder `0.6.x`):
```toml
tonic = { version = "0.14", default-features = false, features = ["transport", "codegen"] }
tonic-prost = "0.14"
tonic-prost-build = "0.14"
prost = "0.14"
protox = "0.9"
tokio-stream = "0.1"
jpeg-encoder = "0.6"
```
`apps/server/Cargo.toml`: add `tonic.workspace`, `tonic-prost.workspace`, `prost.workspace`, `tokio-stream.workspace`, `jpeg-encoder.workspace` to `[dependencies]`; add
```toml
[build-dependencies]
protox.workspace = true
tonic-prost-build.workspace = true
```
Run `cargo tree -p bibcode-server -i prost` and confirm exactly one `prost 0.14.x`; if protox pulls a different prost minor, pick the protox release that matches.

- [ ] **Step 2: build.rs**
```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/android/emulator_controller.proto");
    let descriptors = protox::compile(["android/emulator_controller.proto"], ["proto"])?;
    tonic_prost_build::configure()
        .build_server(true) // the in-process fake controller in tests
        .build_client(true)
        .compile_fds(descriptors)?;
    Ok(())
}
```

- [ ] **Step 3: Failing test** (`grpc.rs` tests):
```rust
#[tokio::test]
async fn client_authenticates_and_reads_status_and_frames() {
    let fake = crate::device::testing::FakeController::spawn().await;
    let client = EmulatorClient::connect(fake.port, Some("test-token".into())).await.unwrap();
    assert!(client.status().await.unwrap().booted);
    let image = client.screenshot(ImgFormat::Rgb888, 360).await.unwrap();
    let format = image.format.unwrap();
    assert_eq!((format.width, format.height), (360, 800));
    assert_eq!(image.image.len(), 360 * 800 * 3);
}

#[tokio::test]
async fn client_with_wrong_token_is_rejected() {
    let fake = crate::device::testing::FakeController::spawn().await;
    let client = EmulatorClient::connect(fake.port, Some("nope".into())).await.unwrap();
    assert!(matches!(client.status().await, Err(DeviceError::Grpc(message)) if message.contains("Unauthenticated")));
}
```
Fake frames: native size 1080×2400; `screenshot`/`stream_screenshot` scale to the requested width keeping aspect (`height = width * 2400 / 1080`), filling every pixel with the current `(r,g,b)` gray value from `frame_tx`.

- [ ] **Step 4: Run** `cargo test -p bibcode-server device::grpc` → FAIL (missing module).

- [ ] **Step 5: Implement `grpc.rs`**
```rust
pub(crate) mod proto {
    tonic::include_proto!("android.emulation.control");
}

use std::time::Duration;
use proto::{emulator_controller_client::EmulatorControllerClient, image_format::ImgFormat};
use tonic::{metadata::MetadataValue, service::interceptor::InterceptedService, transport::{Channel, Endpoint}};
use tokio::sync::mpsc;
use super::DeviceError;

#[derive(Clone)]
pub(crate) struct TokenInterceptor(Option<MetadataValue<tonic::metadata::Ascii>>);

impl tonic::service::Interceptor for TokenInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(value) = &self.0 {
            request.metadata_mut().insert("authorization", value.clone());
        }
        Ok(request)
    }
}

#[derive(Clone)]
pub(crate) struct EmulatorClient {
    inner: EmulatorControllerClient<InterceptedService<Channel, TokenInterceptor>>,
}

const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024; // a full-size RGB888 frame is ~7.8 MB

impl EmulatorClient {
    pub(crate) async fn connect(port: u16, token: Option<String>) -> Result<Self, DeviceError> {
        let channel = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
            .map_err(|error| DeviceError::Grpc(error.to_string()))?
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await
            .map_err(|error| DeviceError::Grpc(error.to_string()))?;
        let header = token
            .map(|token| format!("Bearer {token}").parse())
            .transpose()
            .map_err(|_| DeviceError::Protocol("emulator token is not ASCII".into()))?;
        let inner = EmulatorControllerClient::with_interceptor(channel, TokenInterceptor(header))
            .max_decoding_message_size(MAX_MESSAGE_BYTES);
        Ok(Self { inner })
    }

    pub(crate) async fn status(&self) -> Result<proto::EmulatorStatus, DeviceError> {
        Ok(self.inner.clone().get_status(()).await.map_err(grpc_error)?.into_inner())
    }

    pub(crate) async fn screenshot(&self, format: ImgFormat, width: u32) -> Result<proto::Image, DeviceError> {
        let request = proto::ImageFormat { format: format as i32, width, ..Default::default() };
        Ok(self.inner.clone().get_screenshot(request).await.map_err(grpc_error)?.into_inner())
    }

    pub(crate) async fn stream_screenshot(&self, width: u32) -> Result<tonic::Streaming<proto::Image>, DeviceError> {
        let request = proto::ImageFormat { format: ImgFormat::Rgb888 as i32, width, ..Default::default() };
        Ok(self.inner.clone().stream_screenshot(request).await.map_err(grpc_error)?.into_inner())
    }

    pub(crate) fn input_sink(&self) -> mpsc::Sender<proto::InputEvent> {
        let (sender, receiver) = mpsc::channel(256);
        let mut inner = self.inner.clone();
        tokio::spawn(async move {
            if let Err(status) = inner.stream_input_event(tokio_stream::wrappers::ReceiverStream::new(receiver)).await {
                tracing::warn!(error = %status, "emulator input stream ended");
            }
        });
        sender
    }

    pub(crate) async fn set_rotation(&self, data: [f32; 3]) -> Result<(), DeviceError> {
        let request = proto::PhysicalModelValue {
            target: proto::physical_model_value::PhysicalType::Rotation as i32,
            value: Some(proto::ParameterValue { data: data.to_vec() }),
            ..Default::default()
        };
        self.inner.clone().set_physical_model(request).await.map_err(grpc_error)?;
        Ok(())
    }

    pub(crate) async fn shutdown_vm(&self) -> Result<(), DeviceError> {
        let request = proto::VmRunState { state: proto::vm_run_state::RunState::Shutdown as i32 };
        self.inner.clone().set_vm_state(request).await.map_err(grpc_error)?;
        Ok(())
    }
}

fn grpc_error(status: tonic::Status) -> DeviceError {
    DeviceError::Grpc(format!("{:?}: {}", status.code(), status.message()))
}
```
`mod.rs` skeleton:
```rust
//! Server-hosted Android device previews; see docs/architecture/overview.md#device-previews.
pub(crate) mod grpc;
#[cfg(test)]
pub(crate) mod testing;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DeviceError {
    #[error("emulator gRPC: {0}")]
    Grpc(String),
    #[error("{0}")]
    Io(String),
    #[error("protocol: {0}")]
    Protocol(String),
}
```
`testing.rs`: implement `proto::emulator_controller_server::EmulatorController` for `FakeController` — only `get_status`, `get_screenshot`, `stream_screenshot` (emit one frame per `frame_tx` change), `stream_input_event` (push to `inputs`), `set_physical_model` (push `data` to `rotations`), `set_vm_state` (store state); every other method returns `Status::unimplemented`. Check `authorization` metadata equals `Bearer test-token` in a `tonic::service::interceptor` passed to `EmulatorControllerServer::with_interceptor`. Serve on `127.0.0.1:0` with `tonic::transport::Server::builder().add_service(..).serve_with_incoming(TcpListenerStream::new(listener))`.

- [ ] **Step 6: Run** `cargo test -p bibcode-server device::grpc` → PASS. Also `cargo build -p bibcode-server` on a machine without `protoc` (this Mac has none) proves the `protox` path.

- [ ] **Step 7: Commit**
```bash
git add Cargo.toml Cargo.lock apps/server/Cargo.toml apps/server/build.rs apps/server/proto apps/server/src/device apps/server/src/lib.rs
git commit -m "feat(server): vendor emulator gRPC proto and add authenticated client (#70)"
```

---

### Task 3: SDK discovery and catalog types

**Files:**
- Create: `apps/server/src/device/sdk.rs`, `apps/server/src/device/catalog.rs`
- Modify: `apps/server/src/device/testing.rs` (fake SDK writer), `apps/server/src/device/mod.rs`

**Interfaces:**
- Produces (`catalog.rs`, all `#[derive(Clone, Debug, PartialEq, Serialize)] #[serde(rename_all = "camelCase")]`):
```rust
pub enum AvailabilityKind { Ready, SdkMissing, AdbMissing, NoAvds, AccelerationUnavailable }
pub struct Availability { pub kind: AvailabilityKind, pub message: String }
pub struct Avd { pub name: String, pub display_name: String }
pub enum DeviceState { Starting, Booting, Ready, Stopping, Stopped, Failed }
pub enum Orientation { Portrait, Landscape, ReversePortrait, ReverseLandscape }
pub struct DeviceSnapshot {
    pub device_id: String, pub avd_name: String, pub state: DeviceState,
    #[serde(skip_serializing_if = "Option::is_none")] pub failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")] pub height: Option<u32>,
    pub orientation: Orientation,
}
pub struct DeviceCatalog { pub availability: Availability, pub avds: Vec<Avd>, pub devices: Vec<DeviceSnapshot> }
```
(serde: `AvailabilityKind` and `DeviceState` use `rename_all = "camelCase"` so values are `sdkMissing`, `ready`, ….)
- Produces (`sdk.rs`):
```rust
pub(crate) struct AndroidSdk { pub emulator: PathBuf, pub adb: PathBuf }
pub(crate) struct SdkEnv { pub vars: HashMap<String, OsString>, pub home: Option<PathBuf>, pub os: &'static str }
impl SdkEnv { pub(crate) fn from_process() -> Self }
pub(crate) fn locate_sdk(env: &SdkEnv) -> Result<AndroidSdk, Availability>
pub(crate) async fn discover(env: &SdkEnv) -> (Availability, Vec<Avd>, Option<AndroidSdk>)
```

- [ ] **Step 1: Failing tests** (`sdk.rs`, `#[cfg(all(test, unix))]`):
```rust
#[tokio::test]
async fn reports_sdk_missing_with_actionable_message() {
    let env = SdkEnv { vars: HashMap::new(), home: Some(tempfile::tempdir().unwrap().keep()), os: "linux" };
    let (availability, avds, sdk) = discover(&env).await;
    assert_eq!(availability.kind, AvailabilityKind::SdkMissing);
    assert!(availability.message.contains("ANDROID_HOME"));
    assert!(avds.is_empty() && sdk.is_none());
}

#[tokio::test]
async fn lists_avds_from_android_home() {
    let sdk = FakeSdk::new().avds(&["Pixel_8_API_35", "Tablet API 34"]).accel_ok(true).write();
    let (availability, avds, _) = discover(&sdk.env()).await;
    assert_eq!(availability.kind, AvailabilityKind::Ready);
    assert_eq!(avds.iter().map(|avd| avd.name.as_str()).collect::<Vec<_>>(), ["Pixel_8_API_35", "Tablet API 34"]);
    assert_eq!(avds[0].display_name, "Pixel 8 API 35");
}

#[tokio::test]
async fn classifies_no_avds_adb_missing_and_no_acceleration() {
    assert_eq!(discover(&FakeSdk::new().write().env()).await.0.kind, AvailabilityKind::NoAvds);
    assert_eq!(discover(&FakeSdk::new().avds(&["a"]).without_adb().write().env()).await.0.kind, AvailabilityKind::AdbMissing);
    let no_accel = discover(&FakeSdk::new().avds(&["a"]).accel_ok(false).write().env()).await.0;
    assert_eq!(no_accel.kind, AvailabilityKind::AccelerationUnavailable);
    assert!(no_accel.message.contains("KVM")); // os = "linux"
}

#[test]
fn prefers_android_home_over_sdk_root_and_defaults() {
    let first = FakeSdk::new().write();
    let second = FakeSdk::new().write();
    let mut env = second.env();
    env.vars.insert("ANDROID_SDK_ROOT".into(), second.root().into());
    env.vars.insert("ANDROID_HOME".into(), first.root().into());
    assert_eq!(locate_sdk(&env).unwrap().emulator, first.root().join("emulator/emulator"));
}
```
`FakeSdk` (in `testing.rs`) writes `<root>/emulator/emulator` and `<root>/platform-tools/adb` as `#!/bin/sh` scripts (mode 0o755). The emulator script:
```sh
#!/bin/sh
case "$1" in
  -list-avds) printf '%s\n' __AVDS__ ;;
  -accel-check) echo "accel:"; echo "__ACCEL_CODE__"; echo "__ACCEL_TEXT__"; echo "accel"; exit __ACCEL_CODE__ ;;
  *) exec "__LAUNCH__" "$@" ;;
esac
```
with `__AVDS__` a quoted list, `__ACCEL_CODE__` `0`/`1`, and `__LAUNCH__` the launch stub configured in Task 4 (default `/bin/false`).

- [ ] **Step 2: Run** `cargo test -p bibcode-server device::sdk` → FAIL.

- [ ] **Step 3: Implement `sdk.rs`**
```rust
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn locate_sdk(env: &SdkEnv) -> Result<AndroidSdk, Availability> {
    let exe = if env.os == "windows" { ".exe" } else { "" };
    let candidates = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|key| env.vars.get(*key).map(PathBuf::from))
        .chain(default_sdk_dirs(env));
    for root in candidates {
        let emulator = root.join("emulator").join(format!("emulator{exe}"));
        if !emulator.is_file() { continue; }
        let adb = root.join("platform-tools").join(format!("adb{exe}"));
        if !adb.is_file() {
            return Err(availability(AvailabilityKind::AdbMissing, format!(
                "Android platform-tools are missing from {}. Install them with the Android SDK Manager, then restart the BiBCode server.", root.display())));
        }
        return Ok(AndroidSdk { emulator, adb });
    }
    Err(availability(AvailabilityKind::SdkMissing,
        "No Android SDK found on this server. Set ANDROID_HOME for the BiBCode server to your SDK folder and restart it.".into()))
}

fn default_sdk_dirs(env: &SdkEnv) -> Vec<PathBuf> {
    match (env.os, &env.home) {
        ("macos", Some(home)) => vec![home.join("Library/Android/sdk")],
        ("windows", _) => env.vars.get("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Android/Sdk")).into_iter().collect(),
        (_, Some(home)) => vec![home.join("Android/Sdk")],
        _ => Vec::new(),
    }
}

pub(crate) async fn discover(env: &SdkEnv) -> (Availability, Vec<Avd>, Option<AndroidSdk>) {
    let sdk = match locate_sdk(env) { Ok(sdk) => sdk, Err(availability) => return (availability, Vec::new(), None) };
    let avds = match run_tool(&sdk.emulator, &["-list-avds"]).await {
        Ok(output) => parse_avds(&output.stdout),
        Err(message) => return (availability(AvailabilityKind::SdkMissing, message), Vec::new(), Some(sdk)),
    };
    if avds.is_empty() {
        return (availability(AvailabilityKind::NoAvds,
            "This server's Android SDK has no virtual devices. Create one in Android Studio's Device Manager.".into()), avds, Some(sdk));
    }
    match run_tool(&sdk.emulator, &["-accel-check"]).await {
        Ok(output) if output.success => (availability(AvailabilityKind::Ready, String::new()), avds, Some(sdk)),
        _ => (availability(AvailabilityKind::AccelerationUnavailable, acceleration_message(env.os)), avds, Some(sdk)),
    }
}

fn acceleration_message(os: &str) -> String {
    match os {
        "linux" => "Hardware acceleration is unavailable. Enable KVM on this server (bare metal or a VM with nested virtualization) and give the BiBCode user access to /dev/kvm.",
        "macos" => "Hardware acceleration (Hypervisor.framework) is unavailable on this server.",
        "windows" => "Hardware acceleration is unavailable. Enable the Windows Hypervisor Platform (WHPX) on this server.",
        _ => "Hardware acceleration is unavailable on this server.",
    }.to_owned()
}

fn parse_avds(stdout: &str) -> Vec<Avd> {
    stdout.lines().map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("INFO") && !line.starts_with("WARNING"))
        .map(|name| Avd { name: name.to_owned(), display_name: name.replace('_', " ") })
        .collect()
}
```
`run_tool` uses `crate::process::run_supervised` with `LIST_TIMEOUT`, 64 KiB output cap, and returns `ToolOutput { success: bool, stdout: String }` or `Err(message)`; model it on an existing `run_supervised` caller (`rg -n "run_supervised\(" apps/server/src | head`). `availability(kind, message)` is a one-line constructor.

- [ ] **Step 4: Run** → PASS. **Step 5: Commit** `feat(server): discover Android SDK and AVDs (#70)`.

---

### Task 4: Registered helper spawn and emulator lifecycle

**Files:**
- Create: `apps/server/src/process/registered_helper.rs`; modify `apps/server/src/process/mod.rs` (export)
- Create: `apps/server/src/device/emulator.rs`
- Modify: `apps/server/src/device/testing.rs` (launch stub, fake adb behaviour)

**Interfaces:**
- Produces (`registered_helper.rs`):
```rust
pub struct RegisteredHelper { pub child: Box<dyn ChildWrapper>, pub pid: u32, pub exe: String, pub started: Instant, _registration: ProcessRegistration }
pub async fn spawn_registered_helper(command: CommandWrap, exe: String, label: &str, registry: &ProcessAttributionRegistry) -> Result<RegisteredHelper, String>
impl RegisteredHelper {
    pub fn exited(&self) -> Option<ExitStatus>;            // crate::process::exited_status
    pub async fn terminate(mut self);                       // start_kill + wait
    pub fn report_unrequested_exit(&self, label: &str, status: &ExitStatus); // tracing::warn + exits::report_exit(Watch::Failures, kind "command")
}
```
The body follows `production/managed_endpoint.rs:285-345` exactly (spawn → `NativeProcessSampler::process_identity` → liveness via `waitid(WNOWAIT)` on unix / `exited_status` elsewhere → `register_identity` with `AttributionScope::External`, `AttributionKind::Helper`, `RegistrationSource::Helper`; on any failure `start_kill` + `wait` the unregistered child). Move `connector_is_live` into this module as `pub(crate) fn helper_is_live` and make `managed_endpoint.rs` call it, so the liveness policy has one home. `managed_endpoint` keeps its own spawn path (its test race hooks sit between spawn and identity).
- Produces (`emulator.rs`):
```rust
pub(crate) const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const EMULATOR_GRPC_FLAGS: &[&str] = &["-grpc-use-token"]; // native verification item 1
pub(crate) struct EmulatorPorts { pub console: u16, pub grpc: u16 }
pub(crate) fn spawn_args(avd: &str, ports: &EmulatorPorts, software_gpu: bool) -> Vec<OsString>
pub(crate) fn discovery_file_path(discovery_dir: &Path, pid: u32) -> PathBuf
pub(crate) fn parse_discovery(contents: &str) -> Option<(u16, Option<String>)>   // grpc.port, grpc.token
pub(crate) struct RunningEmulator { pub helper: RegisteredHelper, pub serial: String, pub client: EmulatorClient, pub width: u32, pub height: u32 }
pub(crate) struct EmulatorLauncher { pub sdk: AndroidSdk, pub discovery_dir: PathBuf, pub registry: ProcessAttributionRegistry, pub software_gpu: bool }
impl EmulatorLauncher {
    pub(crate) async fn launch(&self, avd: &str, on_phase: impl Fn(DeviceState) + Send, cancel: &CancellationToken) -> Result<RunningEmulator, LaunchError>;
}
pub(crate) async fn stop(running: RunningEmulator);   // note_stop_requested, shutdown_vm, wait ≤ STOP_TIMEOUT, then terminate
pub(crate) enum LaunchError { Exited(String), BootTimeout, Spawn(String), Cancelled }
```

- [ ] **Step 1: Failing tests** (`emulator.rs`, `#[cfg(all(test, unix))]`):
```rust
#[test]
fn spawn_args_keep_avd_name_as_one_argument() {
    let args = spawn_args("Tablet API 34; rm -rf /", &EmulatorPorts { console: 5554, grpc: 8554 }, true);
    let avd_index = args.iter().position(|arg| arg == "-avd").unwrap();
    assert_eq!(args[avd_index + 1], OsString::from("Tablet API 34; rm -rf /"));
    for expected in ["-no-window", "-no-audio", "-no-boot-anim", "-gpu", "swiftshader_indirect", "-port", "5554", "-grpc", "8554"] {
        assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
    }
}

#[test]
fn parses_discovery_file() {
    assert_eq!(parse_discovery("grpc.port=8554\ngrpc.token=abc\nother=1\n"), Some((8554, Some("abc".into()))));
    assert_eq!(parse_discovery("grpc.port=8554\n"), Some((8554, None)));
    assert_eq!(parse_discovery("port.serial=5554\n"), None);
}

#[tokio::test]
async fn launches_boots_and_stops_a_fake_emulator() {
    let controller = FakeController::spawn().await;
    let sdk = FakeSdk::new().avds(&["Pixel"]).launch_stub(LaunchStub::Runs { grpc_port: controller.port, token: "test-token" }).boot_completed_after(2).write();
    let launcher = sdk.launcher();
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    let running = launcher.launch("Pixel", move |phase| seen.lock().unwrap().push(phase), &CancellationToken::new()).await.unwrap();
    assert_eq!((running.width, running.height), (1080, 2400));
    assert_eq!(*phases.lock().unwrap(), [DeviceState::Booting]);
    let pid = running.helper.pid;
    stop(running).await;
    assert_eq!(*controller.vm_state.lock().unwrap(), Some(RunState::Shutdown as i32));
    assert!(!process_alive(pid));
}

#[tokio::test]
async fn boot_reports_early_exit_without_waiting_for_timeout() {
    let sdk = FakeSdk::new().avds(&["Pixel"]).launch_stub(LaunchStub::ExitsWith(1, "PANIC: Missing emulator engine")).write();
    let started = Instant::now();
    let error = sdk.launcher().launch("Pixel", |_| {}, &CancellationToken::new()).await.unwrap_err();
    assert!(matches!(error, LaunchError::Exited(message) if message.contains("Missing emulator engine")));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn boot_times_out_and_kills_the_process() {
    let controller = FakeController::spawn().await;
    let sdk = FakeSdk::new().avds(&["Pixel"]).launch_stub(LaunchStub::Runs { grpc_port: controller.port, token: "test-token" }).never_boots().write();
    let error = sdk.launcher().launch("Pixel", |_| {}, &CancellationToken::new()).await.unwrap_err();
    assert!(matches!(error, LaunchError::BootTimeout));
    assert!(sdk.launched_pids().iter().all(|pid| !process_alive(*pid)));
}

#[tokio::test]
async fn stop_escalates_to_kill_when_vm_ignores_shutdown() {
    let controller = FakeController::spawn().await;
    let sdk = FakeSdk::new().avds(&["Pixel"]).launch_stub(LaunchStub::IgnoresShutdown { grpc_port: controller.port, token: "test-token" }).write();
    let running = sdk.launcher().launch("Pixel", |_| {}, &CancellationToken::new()).await.unwrap();
    let pid = running.helper.pid;
    tokio::time::timeout(STOP_TIMEOUT + Duration::from_secs(5), stop(running)).await.unwrap();
    assert!(!process_alive(pid));
}
```
(`start_paused` cannot drive real child processes; if the paused-time test is flaky, make `BOOT_TIMEOUT` a field on `EmulatorLauncher` with the constant as default and set 2 s in the test instead.)

Launch stubs (`testing.rs`): `LaunchStub::Runs` writes a script that writes `grpc.port=<port>\ngrpc.token=<token>` to `$DISCOVERY_DIR/pid_$$.ini` then `exec sleep 600`; `IgnoresShutdown` adds `trap '' TERM`; `ExitsWith(code, msg)` prints `msg` to stderr and exits. Fake `adb` handles `-s <serial> wait-for-device` (exit 0) and `-s <serial> shell getprop sys.boot_completed` (prints `1` once a counter file reaches `boot_completed_after`, else empty; `never_boots` always empty). `DISCOVERY_DIR` is passed to the stub through the environment of the spawned command (the launcher sets `ANDROID_EMULATOR_HOME` for the child to the parent of `discovery_dir/..`; the stub reads `$ANDROID_EMULATOR_HOME/avd/running`).

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `registered_helper.rs` as specified, then `emulator.rs`:
```rust
pub(crate) fn spawn_args(avd: &str, ports: &EmulatorPorts, software_gpu: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-avd"].into_iter().map(Into::into).collect();
    args.push(avd.into());
    for arg in ["-no-window", "-no-audio", "-no-boot-anim"] { args.push(arg.into()); }
    args.extend(["-port".into(), ports.console.to_string().into(), "-grpc".into(), ports.grpc.to_string().into()]);
    args.extend(EMULATOR_GRPC_FLAGS.iter().map(OsString::from));
    if software_gpu { args.extend(["-gpu".into(), "swiftshader_indirect".into()]); }
    args
}
```
`launch`:
1. Pick ports: console = first even port in 5554..=5682 where both `port` and `port+1` bind on `127.0.0.1` (probe with `std::net::TcpListener::bind` and drop); grpc = OS-assigned port from binding `127.0.0.1:0`.
2. Build `CommandWrap::with_new(&self.sdk.emulator, |c| { c.args(spawn_args(..)).stdin(null).stdout(null).stderr(piped) })`, `configure_supervised_background_command_wrap`, `spawn_registered_helper(.., "android-emulator", &self.registry)`. Keep the last 4 KiB of stderr in a ring buffer task for `LaunchError::Exited`.
3. Poll every 250 ms until deadline `BOOT_TIMEOUT`: if `helper.exited()` → terminate and `Err(Exited(stderr tail or status))`; if no client yet and the discovery file parses → `EmulatorClient::connect(port, token)`; once `client.status()` succeeds call `on_phase(Booting)` once; then run `adb -s emulator-<console> shell getprop sys.boot_completed` (`run_supervised`, 10 s timeout) until stdout trims to `"1"`. Cancellation → terminate, `Err(Cancelled)`. Deadline → terminate, `Err(BootTimeout)`.
4. Read native size with `client.screenshot(ImgFormat::Rgb888, 0)` → `format.width/height`.
`stop`: `exits::note_stop_requested(pid)` (match its actual signature in `diagnostics/profiler/exits.rs:377`), `client.shutdown_vm()` (ignore error), wait up to `STOP_TIMEOUT` polling `helper.exited()`, then `helper.terminate().await`, then remove the discovery file.

- [ ] **Step 4: Run** `cargo test -p bibcode-server device::emulator process::registered_helper managed_endpoint` → PASS (managed endpoint tests guard the moved liveness helper).

- [ ] **Step 5: Commit** `feat(server): supervise Android emulator processes (#70)`.

---

### Task 5: DeviceManager (catalog, limit, setting, shutdown)

**Files:**
- Modify: `apps/server/src/device/mod.rs`
- Modify: `apps/server/src/production/runtime.rs` (construct, hold, quiesce)

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
```rust
#[derive(Clone)] pub struct DeviceManager { inner: Arc<Inner> }
pub struct DeviceManagerConfig { pub env: SdkEnv, pub discovery_dir: PathBuf, pub registry: ProcessAttributionRegistry, pub enabled: watch::Receiver<bool> }
impl DeviceManager {
    pub fn start(config: DeviceManagerConfig) -> Self;                 // spawns the setting follower
    pub fn catalog(&self) -> watch::Receiver<DeviceCatalog>;
    pub async fn refresh(&self);                                         // rerun discovery, publish
    pub async fn open(&self, avd_name: &str) -> Result<String, DeviceOpError>;   // returns device_id at Starting
    pub async fn wait_ready(&self, device_id: &str, timeout: Duration) -> Result<DeviceSnapshot, DeviceOpError>;
    pub async fn close(&self, device_id: &str) -> Result<(), DeviceOpError>;
    pub(crate) fn client(&self, device_id: &str) -> Result<(EmulatorClient, String /*serial*/, (u32, u32), Orientation), DeviceOpError>; // Ready only
    pub(crate) fn set_orientation(&self, device_id: &str, orientation: Orientation);
    pub async fn shutdown(&self);
}
pub enum DeviceOpError { Disabled, Sdk(Availability), Limit { running: String }, NotFound, NotReady(DeviceState), UnknownAvd(String) }
```
`DeviceOpError::to_rpc_error(&self) -> serde_json::Value` maps to the contract tags of Task 7 (`DevicePreviewsDisabledError`, `DeviceSdkUnavailableError{kind,message}`, `DeviceLimitError{running}`, `DeviceNotFoundError`, `DeviceNotReadyError{state}`; `UnknownAvd` → `DeviceNotFoundError` with message naming the AVD).

- [ ] **Step 1: Failing tests** (`mod.rs`, `#[cfg(all(test, unix))]`):
```rust
#[tokio::test]
async fn open_publishes_states_and_enforces_one_device() {
    let fixture = ManagerFixture::ready(&["Pixel", "Tablet"]).await; // FakeSdk + FakeController, enabled = true
    let id = fixture.manager.open("Pixel").await.unwrap();
    fixture.wait_state(&id, DeviceState::Ready).await;
    assert!(matches!(fixture.manager.open("Tablet").await, Err(DeviceOpError::Limit { running }) if running == "Pixel"));
    fixture.manager.close(&id).await.unwrap();
    fixture.wait_state(&id, DeviceState::Stopped).await;
    assert!(fixture.manager.open("Tablet").await.is_ok());
}

#[tokio::test]
async fn disabled_manager_refuses_and_disabling_stops_devices() {
    let fixture = ManagerFixture::ready(&["Pixel"]).await;
    let id = fixture.manager.open("Pixel").await.unwrap();
    fixture.wait_state(&id, DeviceState::Ready).await;
    let pid = fixture.pid_of(&id);
    fixture.enabled.send(false).unwrap();
    fixture.wait_state(&id, DeviceState::Stopped).await;
    assert!(!process_alive(pid));
    assert!(matches!(fixture.manager.open("Pixel").await, Err(DeviceOpError::Disabled)));
}

#[tokio::test]
async fn crash_marks_failed_and_frees_the_slot() {
    let fixture = ManagerFixture::ready(&["Pixel"]).await;
    let id = fixture.manager.open("Pixel").await.unwrap();
    fixture.wait_state(&id, DeviceState::Ready).await;
    kill_process(fixture.pid_of(&id));
    let snapshot = fixture.wait_state(&id, DeviceState::Failed).await;
    assert!(snapshot.failure.unwrap().contains("exited"));
    assert!(fixture.manager.open("Pixel").await.is_ok());
}

#[tokio::test]
async fn shutdown_reaps_running_emulators() {
    let fixture = ManagerFixture::ready(&["Pixel"]).await;
    let id = fixture.manager.open("Pixel").await.unwrap();
    fixture.wait_state(&id, DeviceState::Ready).await;
    let pid = fixture.pid_of(&id);
    fixture.manager.shutdown().await;
    assert!(!process_alive(pid));
}

#[tokio::test]
async fn unknown_avd_is_rejected_before_spawning() {
    let fixture = ManagerFixture::ready(&["Pixel"]).await;
    assert!(matches!(fixture.manager.open("Nope").await, Err(DeviceOpError::UnknownAvd(_))));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

`Inner` holds `catalog: watch::Sender<DeviceCatalog>`, `devices: Mutex<HashMap<String, Slot>>` where `Slot { snapshot: DeviceSnapshot, running: Option<RunningEmulator>, cancel: CancellationToken }`, `sdk: Mutex<Option<AndroidSdk>>`, `enabled: watch::Receiver<bool>`, `shutdown: CancellationToken`, and a `TaskTracker`.
- `open`: check enabled → `Disabled`; `refresh()` if `sdk` is `None`; availability not `Ready` → `Sdk(..)`; AVD not listed → `UnknownAvd`; any slot in `Starting|Booting|Ready|Stopping` → `Limit`; insert slot (`uuid::Uuid::new_v4()` id, `Starting`), publish, spawn the launch task: `launcher.launch(avd, |phase| update(id, phase), &slot.cancel)`; on `Ok` store `running`, set `Ready` + size; on `Err` set `Failed` with message (`Exited(m)` → `"exited: {m}"`, `BootTimeout` → `"did not finish booting within 3 minutes"`), emit `device.lifecycle` (Task 6 adds the event helper; here call `profiler_lifecycle(phase, started, failure_kind)` stub defined in `mod.rs` that Task 6 fills — no, define it fully here: see Step 3b).
- A watcher task per running device polls `helper.exited()` every 1 s; an exit while not `Stopping` → `report_unrequested_exit`, `Failed{"exited: <status>"}`.
- `close`: `NotFound` if absent; set `Stopping`, cancel the slot, take `running`, `emulator::stop(running).await`, set `Stopped`.
- Setting follower: on `false`, `close` every non-terminal slot.
- `shutdown`: cancel `shutdown`, close every slot concurrently (`futures_util::future::join_all`), then `tracker.close(); tracker.wait()`.
- Publish snapshots sorted by device id; drop `Stopped`/`Failed` slots older than the newest 4 to bound the catalog.

Step 3b, profiler event (in `mod.rs`):
```rust
fn record_lifecycle(phase: &'static str, started: Instant, failure_kind: Option<&'static str>) {
    if !crate::diagnostics::profiler::is_enabled() { return; }
    crate::diagnostics::profiler::emit("device.lifecycle", serde_json::json!({
        "phase": phase, "durationMs": started.elapsed().as_millis() as u64, "failureKind": failure_kind,
    }));
}
```
(Match the actual `profiler::emit` signature: `rg -n "pub fn emit" apps/server/src/diagnostics/profiler`.) Call with `start` at spawn, `ready`, `fail` (`exited|bootTimeout|spawn`), `stop`.

`runtime.rs`: construct after `process_attribution` (~line 255):
```rust
let device_manager = crate::device::DeviceManager::start(crate::device::DeviceManagerConfig {
    env: crate::device::sdk::SdkEnv::from_process(),
    discovery_dir: crate::device::emulator::default_discovery_dir(),
    registry: process_attribution.clone(),
    enabled: control.device_previews_enabled_signal(),
});
```
Store as field `device_manager`; in `quiesce_for_update` insert `self.device_manager.shutdown().await;` immediately after `self.preview_gateway.shutdown().await;`. `default_discovery_dir()` in `emulator.rs`: `ANDROID_EMULATOR_HOME` else `ANDROID_USER_HOME` else `~/.android`, joined with `avd/running` (native verification item 1).

- [ ] **Step 4: Run** `cargo test -p bibcode-server device::` → PASS; `cargo test -p bibcode-server production::runtime` → PASS.

- [ ] **Step 5: Commit** `feat(server): device manager with catalog, limits and shutdown (#70)`.

---

### Task 6: JPEG frames, input, rotation

**Files:**
- Create: `apps/server/src/device/frames.rs`, `apps/server/src/device/input.rs`
- Modify: `apps/server/src/device/mod.rs`

**Interfaces:**
- Produces (`frames.rs`):
```rust
pub(crate) const JPEG_MAX_WIDTH: u32 = 720;
pub(crate) const JPEG_QUALITY: u8 = 70;
pub(crate) const JPEG_MIN_INTERVAL: Duration = Duration::from_millis(66); // ≤ 15 fps
#[derive(Clone, Serialize)] #[serde(rename_all = "camelCase")]
pub struct JpegFrame { pub seq: u64, pub width: u32, pub height: u32, pub orientation: Orientation, pub jpeg_base64: String }
pub(crate) fn encode_rgb888(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, DeviceError>;
impl DeviceManager { pub fn subscribe_frames(&self, device_id: &str) -> Result<FrameSubscription, DeviceOpError>; }
pub struct FrameSubscription { pub frames: watch::Receiver<Option<JpegFrame>>, _guard: SubscriberGuard }
```
One producer per device; it starts on the first subscription and stops when the last `SubscriberGuard` drops (an `Arc` count checked in `Drop`, which cancels the producer token).
- Produces (`input.rs`):
```rust
#[derive(Deserialize)] #[serde(tag = "type", rename_all = "camelCase")]
pub enum DeviceInputEvent {
    Touch { action: TouchAction, pointer_id: u32, x: f64, y: f64 },
    Key { key: DeviceKey, action: KeyAction },
    Text { text: String },
}
pub enum TouchAction { Down, Move, Up, Cancel }   // serde camelCase
pub enum KeyAction { Down, Up }
pub enum DeviceKey { Back, Home, AppSwitch, Power, VolumeUp, VolumeDown, Enter, Backspace, Delete, Tab, ArrowUp, ArrowDown, ArrowLeft, ArrowRight }
pub(crate) fn w3c_key(key: DeviceKey) -> &'static str;   // GoBack, GoHome, AppSwitch, Power, AudioVolumeUp, AudioVolumeDown, Enter, Backspace, Delete, Tab, ArrowUp, …
pub(crate) fn to_device_pixels(x: f64, y: f64, width: u32, height: u32, orientation: Orientation) -> (i32, i32); // native verification item 3
pub(crate) fn rotation_vector(orientation: Orientation) -> [f32; 3]; // Portrait [0,0,0], Landscape [0,0,-90], ReversePortrait [0,0,180], ReverseLandscape [0,0,90]; item 2
impl DeviceManager {
    pub async fn input(&self, device_id: &str, events: Vec<DeviceInputEvent>) -> Result<(), DeviceOpError>;
    pub async fn rotate(&self, device_id: &str, orientation: Orientation) -> Result<(), DeviceOpError>;
}
```
`DeviceOpError` gains `InvalidInput(String)` → `DeviceInputError{message}` contract tag.

- [ ] **Step 1: Failing tests**

`frames.rs`:
```rust
#[test]
fn encodes_rgb888_to_jpeg() {
    let rgb = vec![200_u8; 360 * 800 * 3];
    let jpeg = encode_rgb888(&rgb, 360, 800).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    assert!(jpeg.len() < rgb.len() / 10);
}

#[cfg(unix)]
#[tokio::test]
async fn slow_subscriber_sees_only_latest_frame() {
    let fixture = ManagerFixture::ready_device().await; // device Ready on FakeController
    let mut sub = fixture.manager.subscribe_frames(&fixture.id).unwrap();
    for gray in [10, 20, 30, 40] { fixture.controller.frame_tx.send((0, 0, gray)).unwrap(); tokio::time::sleep(Duration::from_millis(80)).await; }
    sub.frames.changed().await.unwrap();
    let frame = sub.frames.borrow_and_update().clone().unwrap();
    assert_eq!(frame.width, 720);
    assert_eq!(decoded_gray(&frame.jpeg_base64), 40); // decode with a tiny test-only JPEG check: compare to encode_rgb888 of gray 40
}

#[cfg(unix)]
#[tokio::test]
async fn producer_stops_when_last_subscriber_leaves() {
    let fixture = ManagerFixture::ready_device().await;
    let first = fixture.manager.subscribe_frames(&fixture.id).unwrap();
    let second = fixture.manager.subscribe_frames(&fixture.id).unwrap();
    assert_eq!(fixture.controller.active_screenshot_streams(), 1);
    drop(first);
    assert_eq!(fixture.controller.active_screenshot_streams(), 1);
    drop(second);
    wait_until(|| fixture.controller.active_screenshot_streams() == 0).await;
}
```
(`decoded_gray`: compare `jpeg_base64` to `base64(encode_rgb888(&vec![40; 720*1600*3], 720, 1600))` — encoding is deterministic.)

`input.rs`:
```rust
#[test]
fn maps_normalized_coordinates_per_orientation() {
    assert_eq!(to_device_pixels(0.5, 0.25, 1080, 2400, Orientation::Portrait), (540, 600));
    assert_eq!(to_device_pixels(0.5, 0.25, 1080, 2400, Orientation::Landscape), (1200, 270));
    assert_eq!(to_device_pixels(1.2, -0.1, 1080, 2400, Orientation::Portrait), (1079, 0)); // clamped
}

#[cfg(unix)]
#[tokio::test]
async fn forwards_touch_key_and_text_in_order() {
    let fixture = ManagerFixture::ready_device().await;
    fixture.manager.input(&fixture.id, vec![
        touch(TouchAction::Down, 0.5, 0.5), touch(TouchAction::Move, 0.6, 0.5), touch(TouchAction::Up, 0.6, 0.5),
        DeviceInputEvent::Key { key: DeviceKey::Back, action: KeyAction::Down },
        DeviceInputEvent::Text { text: "hi".into() },
    ]).await.unwrap();
    let inputs = fixture.controller.wait_inputs(5).await;
    assert_eq!(touch_pressure(&inputs[0]), 1);
    assert_eq!(touch_xy(&inputs[1]), (648, 1200));
    assert_eq!(touch_pressure(&inputs[2]), 0);
    assert_eq!(key_of(&inputs[3]), ("GoBack", 0 /* keydown */));
    assert_eq!(text_of(&inputs[4]), "hi");
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_oversized_batches_and_text() {
    let fixture = ManagerFixture::ready_device().await;
    let many = (0..65).map(|_| touch(TouchAction::Move, 0.1, 0.1)).collect();
    assert!(matches!(fixture.manager.input(&fixture.id, many).await, Err(DeviceOpError::InvalidInput(_))));
    let long = DeviceInputEvent::Text { text: "x".repeat(1001) };
    assert!(matches!(fixture.manager.input(&fixture.id, vec![long]).await, Err(DeviceOpError::InvalidInput(_))));
}

#[cfg(unix)]
#[tokio::test]
async fn dangling_touch_is_released_on_stream_reset() {
    let fixture = ManagerFixture::ready_device().await;
    fixture.manager.input(&fixture.id, vec![touch(TouchAction::Down, 0.5, 0.5)]).await.unwrap();
    fixture.controller.break_input_stream();               // fake drops the stream
    fixture.manager.input(&fixture.id, vec![touch(TouchAction::Down, 0.2, 0.2)]).await.unwrap();
    let inputs = fixture.controller.wait_inputs(2).await;  // new stream: release of pointer 0, then the new down
    assert_eq!(touch_pressure(&inputs[0]), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn rotate_sends_vector_and_updates_catalog() {
    let fixture = ManagerFixture::ready_device().await;
    fixture.manager.rotate(&fixture.id, Orientation::Landscape).await.unwrap();
    assert_eq!(fixture.controller.rotations.lock().unwrap().last(), Some(&[0.0, 0.0, -90.0]));
    assert_eq!(fixture.snapshot().orientation, Orientation::Landscape);
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**

`encode_rgb888`:
```rust
pub(crate) fn encode_rgb888(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, DeviceError> {
    let (w, h) = (u16::try_from(width), u16::try_from(height));
    let (Ok(w), Ok(h)) = (w, h) else { return Err(DeviceError::Protocol("frame too large".into())) };
    if rgb.len() != width as usize * height as usize * 3 { return Err(DeviceError::Protocol("frame size mismatch".into())) }
    let mut out = Vec::with_capacity(rgb.len() / 8);
    jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
        .encode(rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|error| DeviceError::Protocol(error.to_string()))?;
    Ok(out)
}
```
Producer task: `client.stream_screenshot(JPEG_MAX_WIDTH)`; for each image, skip if less than `JPEG_MIN_INTERVAL` since the last published frame (keep the newest pending and publish it when the interval elapses), encode on `tokio::task::spawn_blocking`, publish `Some(JpegFrame { seq, .. })` via `watch::Sender::send_replace`. On stream error, `tracing::warn!` and retry after 1 s while subscribers remain and the device is `Ready`. Orientation in the frame comes from the catalog snapshot.

Input: per device keep `InputState { sink: mpsc::Sender<InputEvent>, pressed: BTreeSet<u32> }` created lazily from `client.input_sink()`. Validate the whole batch first (≤ 64 events, text ≤ 1,000 chars, finite coordinates). If `sink.send` fails (stream closed), create a new sink, then send a release (`pressure 0`) for every pointer in `pressed`, clear it, and resend the current event. Touch → `TouchEvent { touches: [Touch { x, y, identifier: pointer_id, pressure: if Up|Cancel {0} else {1}, expiration: NeverExpire }] }`; Key → `KeyboardEvent { code_type: Usb (ignored when key set), event_type: keydown|keyup, key: w3c_key(..) }`; Text → `KeyboardEvent { text }`.
`to_device_pixels`: in Portrait/ReversePortrait use `(width, height)`; in Landscape/ReverseLandscape use `(height, width)`; `x = (x.clamp(0,1) * w).min(w-1)`, same for y.

- [ ] **Step 4: Run** → PASS. **Step 5: Commit** `feat(server): device JPEG frames, input and rotation (#70)`.

---

### Task 7: Contracts and RPC surface

**Files:**
- Create: `packages/contracts/src/device.ts`, `packages/contracts/src/device.test.ts`
- Modify: `packages/contracts/src/rpc.ts` (WS_METHODS, Rpc defs, `WsRpcGroup`), `packages/contracts/src/index.ts`, `packages/contracts/fixtures/rpc-wire/manifest.json`
- Modify: `packages/client-runtime/src/rpc/client.ts:53` (`EnvironmentSubscriptionRpcTag`)
- Modify: `apps/server/src/rpc/methods.rs`, `apps/server/src/auth/scope.rs`, `apps/server/src/diagnostics/profiler/latency.rs:106`
- Create: `apps/server/src/production/device_rpc.rs`; modify `apps/server/src/production/mod.rs`, `runtime.rs` (register)
- Test: `apps/server/tests/device_rpc.rs`

**Interfaces:**
- Produces (contracts):
```ts
export const DeviceAvailabilityKind = Schema.Literals(["ready", "sdkMissing", "adbMissing", "noAvds", "accelerationUnavailable"]);
export const DeviceState = Schema.Literals(["starting", "booting", "ready", "stopping", "stopped", "failed"]);
export const DeviceOrientation = Schema.Literals(["portrait", "landscape", "reversePortrait", "reverseLandscape"]);
export const DeviceSnapshot = Schema.Struct({ deviceId: Schema.String, avdName: Schema.String, state: DeviceState,
  failure: Schema.optionalKey(Schema.String), width: Schema.optionalKey(Schema.Number), height: Schema.optionalKey(Schema.Number), orientation: DeviceOrientation });
export const DeviceCatalog = Schema.Struct({ availability: Schema.Struct({ kind: DeviceAvailabilityKind, message: Schema.String }),
  avds: Schema.Array(Schema.Struct({ name: Schema.String, displayName: Schema.String })), devices: Schema.Array(DeviceSnapshot) });
export const DeviceKey = Schema.Literals(["back","home","appSwitch","power","volumeUp","volumeDown","enter","backspace","delete","tab","arrowUp","arrowDown","arrowLeft","arrowRight"]);
export const DeviceInputEvent = Schema.Union([
  Schema.Struct({ type: Schema.Literal("touch"), action: Schema.Literals(["down","move","up","cancel"]), pointerId: Schema.Number, x: Schema.Number, y: Schema.Number }),
  Schema.Struct({ type: Schema.Literal("key"), key: DeviceKey, action: Schema.Literals(["down","up"]) }),
  Schema.Struct({ type: Schema.Literal("text"), text: Schema.String.check(Schema.isMaxLength(1000)) }),
]);
export const DeviceOpenInput = Schema.Struct({ avdName: Schema.String });
export const DeviceOpenResult = Schema.Struct({ deviceId: Schema.String });
export const DeviceIdInput = Schema.Struct({ deviceId: Schema.String });
export const DeviceInputInput = Schema.Struct({ deviceId: Schema.String, events: Schema.Array(DeviceInputEvent).check(Schema.isMinLength(1), Schema.isMaxLength(64)) });
export const DeviceRotateInput = Schema.Struct({ deviceId: Schema.String, orientation: DeviceOrientation });
export const DeviceJpegFrame = Schema.Struct({ seq: Schema.Number, width: Schema.Number, height: Schema.Number, orientation: DeviceOrientation, jpegBase64: Schema.String });
export const DeviceMediaTicket = Schema.Struct({ ticket: Schema.String, expiresAtMs: Schema.Number });
export class DevicePreviewsDisabledError extends Schema.TaggedError<DevicePreviewsDisabledError>()("DevicePreviewsDisabledError", { message: Schema.String }) {}
export class DeviceSdkUnavailableError extends Schema.TaggedError<DeviceSdkUnavailableError>()("DeviceSdkUnavailableError", { kind: DeviceAvailabilityKind, message: Schema.String }) {}
export class DeviceLimitError extends Schema.TaggedError<DeviceLimitError>()("DeviceLimitError", { running: Schema.String, message: Schema.String }) {}
export class DeviceNotFoundError extends Schema.TaggedError<DeviceNotFoundError>()("DeviceNotFoundError", { message: Schema.String }) {}
export class DeviceNotReadyError extends Schema.TaggedError<DeviceNotReadyError>()("DeviceNotReadyError", { state: DeviceState, message: Schema.String }) {}
export class DeviceInputError extends Schema.TaggedError<DeviceInputError>()("DeviceInputError", { message: Schema.String }) {}
export const DeviceRpcError = Schema.Union([DevicePreviewsDisabledError, DeviceSdkUnavailableError, DeviceLimitError, DeviceNotFoundError, DeviceNotReadyError, DeviceInputError]);
```
(Use the same `Schema.check`/length helpers that neighbouring contracts use — `rg -n "isMaxLength|maxLength" packages/contracts/src | head -3` — and match their spelling.)
- RPC names (WS_METHODS keys → wire): `deviceSubscribeCatalog: "device.subscribeCatalog"`, `deviceOpen: "device.open"`, `deviceClose: "device.close"`, `deviceInput: "device.input"`, `deviceRotate: "device.rotate"`, `deviceSubscribeFrames: "device.subscribeFrames"`, `deviceCreateMediaTicket: "device.createMediaTicket"`. Every Rpc's `error` is `Schema.Union([DeviceRpcError, EnvironmentRpcError])`; streams set `stream: true`.
- `createMediaTicket` is declared here but registered in Task 11; until then `methods.rs` omits it and the manifest omits it (both added in Task 11), so `validate_complete` stays green.

- [ ] **Step 1: Failing tests**

`device.test.ts`: decode a catalog fixture; reject 65-event batch; reject 1,001-char text; decode each tagged error.
`apps/server/tests/device_rpc.rs` (use the existing RPC test harness from `tests/support`; look at `tests/production_git_vcs_rpc.rs` for the pattern):
```rust
#[tokio::test]
async fn device_methods_are_scoped_and_registered() {
    for (method, scope) in [
        ("device.subscribeCatalog", "orchestration:read"), ("device.subscribeFrames", "orchestration:read"),
        ("device.open", "orchestration:operate"), ("device.close", "orchestration:operate"),
        ("device.input", "orchestration:operate"), ("device.rotate", "orchestration:operate"),
    ] {
        assert_eq!(bibcode_server::auth_scope_for_tests(method), Some(scope), "{method}");
    }
}

#[tokio::test]
async fn open_fails_typed_when_disabled() {
    let harness = RpcHarness::production_with_settings(json!({})).await;
    let error = harness.call("device.open", json!({"avdName": "Pixel"})).await.unwrap_err();
    assert_eq!(error["_tag"], "DevicePreviewsDisabledError");
}

#[tokio::test]
async fn catalog_streams_availability_when_enabled() {
    let harness = RpcHarness::production_with_settings(json!({"enableDevicePreviews": true})).await;
    let first = harness.subscribe_first("device.subscribeCatalog", json!({})).await;
    assert!(first["availability"]["kind"].is_string());
}
```
(If no `auth_scope_for_tests` export exists, put the scope assertions in `auth/scope.rs`'s own tests module instead, next to the existing `required_scope("preview.gatewayOpen")` assertion; if the harness names differ, use the harness the git-vcs RPC tests use.)

- [ ] **Step 2: Run** `vp test packages/contracts/src/device.test.ts packages/contracts/src/rpcRustParity.test.ts` and `cargo test -p bibcode-server --test device_rpc --test rpc_wire` → FAIL.

- [ ] **Step 3: Implement**
- contracts as above; export from `index.ts`; add the six methods to `WsRpcGroup`; add manifest entries (`unary` for open/close/input/rotate, `stream` for the two subscriptions), keeping the manifest sorted.
- client-runtime: add `typeof WS_METHODS.deviceSubscribeCatalog | typeof WS_METHODS.deviceSubscribeFrames` to `EnvironmentSubscriptionRpcTag`.
- `methods.rs`: `read_stream("device.subscribeCatalog")`, `read_stream("device.subscribeFrames")`, `mutation_unary("device.open")`, `mutation_unary("device.close")`, `mutation_unary("device.input")`, `mutation_unary("device.rotate")` in sorted position.
- `scope.rs`: the two subscriptions in the `SCOPE_ORCHESTRATION_READ` arm; the four mutations in the `SCOPE_ORCHESTRATION_OPERATE` arm.
- `latency.rs` `is_long_running`: add `"device.close"`.
- `device_rpc.rs`:
```rust
pub fn register_device_rpc(registry: &mut RpcRegistry, devices: DeviceManager) {
    let catalog = devices.clone();
    registry.register_latest_stream("device.subscribeCatalog", move |_request, cancellation| {
        let mut source = catalog.catalog();
        let (sender, receiver) = watch::channel(None);
        let devices = catalog.clone();
        tokio::spawn(async move {
            devices.refresh().await;
            loop {
                let value = serde_json::to_value(&*source.borrow_and_update()).expect("catalog serializes");
                sender.send_replace(Some(Ok(vec![value])));
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    changed = source.changed() => if changed.is_err() { break },
                }
            }
        });
        receiver
    });
    let frames = devices.clone();
    registry.register_latest_stream("device.subscribeFrames", move |request, cancellation| {
        let (sender, receiver) = watch::channel(None);
        let devices = frames.clone();
        tokio::spawn(async move {
            let input: DeviceIdInput = match serde_json::from_value(request.payload) {
                Ok(input) => input,
                Err(error) => { sender.send_replace(Some(Err(json!({"_tag":"Defect","message":error.to_string()})))); return; }
            };
            let mut subscription = match devices.subscribe_frames(&input.device_id) {
                Ok(subscription) => subscription,
                Err(error) => { sender.send_replace(Some(Err(error.to_rpc_error()))); return; }
            };
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    changed = subscription.frames.changed() => {
                        if changed.is_err() { break }
                        if let Some(frame) = subscription.frames.borrow_and_update().clone() {
                            sender.send_replace(Some(Ok(vec![serde_json::to_value(frame).expect("frame serializes")])));
                        }
                    }
                }
            }
        });
        receiver
    });
    register_unary_op(registry, "device.open", devices.clone(), |devices, payload| async move {
        let input: DeviceOpenInput = decode(payload)?;
        let device_id = devices.open(&input.avd_name).await.map_err(|error| error.to_rpc_error())?;
        Ok(json!({ "deviceId": device_id }))
    });
    register_unary_op(registry, "device.close", devices.clone(), |devices, payload| async move {
        let input: DeviceIdInput = decode(payload)?;
        devices.close(&input.device_id).await.map_err(|error| error.to_rpc_error())?;
        Ok(Value::Null)
    });
    register_unary_op(registry, "device.input", devices.clone(), |devices, payload| async move {
        let input: DeviceInputInput = decode(payload)?;
        devices.input(&input.device_id, input.events).await.map_err(|error| error.to_rpc_error())?;
        Ok(Value::Null)
    });
    register_unary_op(registry, "device.rotate", devices, |devices, payload| async move {
        let input: DeviceRotateInput = decode(payload)?;
        devices.rotate(&input.device_id, input.orientation).await.map_err(|error| error.to_rpc_error())?;
        Ok(Value::Null)
    });
}
```
`register_unary_op` is a 6-line local helper wrapping `registry.register_unary` and cloning `devices`; `decode` maps serde errors to `{"_tag":"Defect"}` as `workspace_preview.rs` does. In `runtime.rs` call `crate::production::device_rpc::register_device_rpc(&mut registry, device_manager.clone());` next to `register_workspace_preview_rpc`.

- [ ] **Step 4: Run** → PASS; also `cargo test -p bibcode-server rpc::methods auth::scope`.

- [ ] **Step 5: Commit** `feat(rpc): device RPC contracts and server registration (#70)`.

---

### Task 8: MCP device tools

**Files:**
- Modify: `apps/server/src/production/connect_mcp.rs` (`call_tool`, `tool_descriptors`, struct field + constructor arg), `apps/server/src/lifecycle.rs:320-380` (wire `DeviceInvoker`)
- Test: `apps/server/src/production/connect_mcp/tests.rs` (existing tests module) and `apps/server/tests/production_connect_mcp.rs`

**Interfaces:**
- Consumes: `DeviceManager` (Tasks 5–6).
- Produces:
```rust
#[derive(Clone)]
pub struct DeviceTools { manager: DeviceManager, enabled: watch::Receiver<bool> }
impl DeviceTools {
    pub fn new(manager: DeviceManager, enabled: watch::Receiver<bool>) -> Self;
    fn enabled(&self) -> bool;
    async fn call(&self, operation: &str, arguments: Value, principal_can_operate: bool) -> Value; // MCP tool result
}
fn device_tool_descriptors() -> Vec<Value>;   // device_list, device_open, device_screenshot, device_close
fn tool_descriptors(devices_enabled: bool) -> Vec<Value>;
```
`ConnectMcpService::open` gains `devices: Option<DeviceTools>`. `PreviewScope` (or the MCP credential) is checked for operate permission the same way existing preview mutations are; if MCP credentials carry no scope list, `principal_can_operate` is `true` (MCP credentials are issued per provider thread, which already runs with operate rights) — confirm by reading `issue_mcp_credential` and record the finding in the commit message.

- [ ] **Step 1: Failing tests** (`connect_mcp/tests.rs`):
```rust
#[test]
fn device_tools_are_listed_only_when_enabled() {
    let names = |enabled| tool_descriptors(enabled).iter().map(|tool| tool["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
    assert!(!names(false).iter().any(|name| name.starts_with("device_")));
    for tool in ["device_list", "device_open", "device_screenshot", "device_close"] {
        assert!(names(true).contains(&tool.to_owned()), "{tool}");
    }
    assert!(names(false).contains(&"preview_snapshot".to_owned()));
}

#[cfg(unix)]
#[tokio::test]
async fn device_screenshot_returns_an_image_block() {
    let fixture = DeviceToolsFixture::ready_device().await;
    let result = fixture.tools.call("screenshot", json!({}), true).await;
    assert_eq!(result["isError"], false);
    let image = result["content"].as_array().unwrap().iter().find(|block| block["type"] == "image").unwrap();
    assert_eq!(image["mimeType"], "image/png");
    assert!(base64_decodes_to_png(image["data"].as_str().unwrap()));
    assert_eq!(result["structuredContent"]["screenshot"]["width"], 1080);
}

#[cfg(unix)]
#[tokio::test]
async fn device_open_fails_when_disabled_mid_boot() {
    let fixture = DeviceToolsFixture::slow_boot().await;
    let call = tokio::spawn({ let tools = fixture.tools.clone(); async move { tools.call("open", json!({"avdName":"Pixel"}), true).await } });
    tokio::time::sleep(Duration::from_millis(200)).await;
    fixture.enabled_tx.send(false).unwrap();
    let result = call.await.unwrap();
    assert_eq!(result["isError"], true);
    assert!(result["content"][0]["text"].as_str().unwrap().contains("off in Settings"));
    wait_until(|| fixture.launched_pids().iter().all(|pid| !process_alive(*pid))).await;
}

#[tokio::test]
async fn device_call_when_disabled_is_a_tool_error() {
    let fixture = DeviceToolsFixture::disabled().await;
    let result = fixture.tools.call("list", json!({}), true).await;
    assert_eq!(result["structuredContent"]["error"]["_tag"], "DevicePreviewsDisabledError");
}

#[cfg(unix)]
#[tokio::test]
async fn device_open_without_name_uses_the_only_avd_and_lists_choices_otherwise() {
    let one = DeviceToolsFixture::ready_sdk(&["Pixel"]).await;
    assert_eq!(one.tools.call("open", json!({}), true).await["isError"], false);
    let two = DeviceToolsFixture::ready_sdk(&["Pixel", "Tablet"]).await;
    let result = two.tools.call("open", json!({}), true).await;
    assert!(result["content"][0]["text"].as_str().unwrap().contains("Pixel, Tablet"));
}
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**
- `"tools/list" => json!({"tools": tool_descriptors(self.devices.as_ref().is_some_and(DeviceTools::enabled))})`.
- `call_tool`: `if let Some(operation) = name.strip_prefix("device_")` → `match &self.devices { Some(devices) => devices.call(operation, arguments, can_operate).await, None => tool_failure("McpUnknownToolError", "Unknown tool.") }`; else existing preview branch.
- `DeviceTools::call`:
  - disabled → `tool_failure("DevicePreviewsDisabledError", "Device previews are off in Settings. Turn on Settings → Device previews on this environment.")`;
  - `list` → `refresh()`, `structuredContent` = catalog JSON;
  - `open` → resolve name (only AVD, or error listing `avds.join(", ")`), require `can_operate`, `manager.open`, then `tokio::select!` on `manager.wait_ready(id, BOOT_TIMEOUT)` and the setting turning false (→ disabled failure; the manager's follower already stops the device);
  - `screenshot` → resolve id (only running device), `client.screenshot(ImgFormat::Png, min(1080, width))` → reuse `snapshot_tool_result(json!({"deviceId": id, "screenshot": {"data": base64, "mimeType": "image/png", "width": w, "height": h}}))`;
  - `close` → require `can_operate`, `manager.close`.
  - Errors map through `DeviceOpError` to `tool_failure(tag, message)`.
- Descriptors: same shape as preview tools; `inputSchema` per tool (`device_open`: `{"type":"object","properties":{"avdName":{"type":"string"}},"additionalProperties":false}`; `device_screenshot`: optional `deviceId`; `device_close`: required `deviceId`; `device_list`: empty object); annotations `readOnlyHint` true for list/screenshot; descriptions state that the device runs on the BiBCode server.
- `lifecycle.rs`: pass `Some(DeviceTools::new(runtime.device_manager.clone(), control.device_previews_enabled_signal()))` (expose `device_manager()` and the signal from `ProductionRuntime`).

- [ ] **Step 4: Run** `cargo test -p bibcode-server connect_mcp --test production_connect_mcp` → PASS.

- [ ] **Step 5: Commit** `feat(mcp): setting-gated device_* agent tools (#70)`.

---

### Task 9: Client device pane (JPEG path), settings toggle

**Files:**
- Create: `packages/client-runtime/src/state/device.ts`, `apps/web/src/state/device.ts`
- Create: `apps/web/src/components/device/DevicePanel.tsx`, `DeviceToolbar.tsx`, `DeviceScreen.tsx`, `deviceGeometry.ts`, `deviceInputQueue.ts`, `deviceKeyMap.ts`, `jpegRenderer.ts`, `deviceMessages.ts`
- Create tests: `deviceGeometry.test.ts`, `deviceInputQueue.test.ts`, `deviceKeyMap.test.ts`, `jpegRenderer.test.ts`, `DevicePanel.test.tsx`
- Create: `apps/web/src/components/settings/DevicePreviewsSection.tsx` (+ test); modify the settings panel list (where `ProfilerSection` is mounted)
- Modify: `apps/web/src/rightPanelStore.ts` (kind, surface, parser, version 10) + its test; `apps/web/src/components/RightPanelTabs.tsx:211-282`; `apps/web/src/components/ChatView.tsx` (content chain ~6599 and right-panel menu); command palette registration (search `"Open browser"` or the palette's preview entry and add "Open device preview" beside it)

**Interfaces:**
- Consumes: Task 7 contracts.
- Produces:
```ts
// client-runtime/src/state/device.ts
export function createDeviceEnvironmentAtoms<R, E>(runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>): {
  catalog: /* subscription atom family */; frames: /* subscription atom family, idleTtlMs: 0 */;
  open; close; input; rotate; createMediaTicket /* commands; input uses concurrency { mode: "serial", key: environmentId+deviceId } */;
};
// deviceGeometry.ts
export interface Letterbox { readonly x: number; readonly y: number; readonly width: number; readonly height: number }
export function letterbox(containerW: number, containerH: number, deviceW: number, deviceH: number): Letterbox;
export function toNormalized(clientX: number, clientY: number, box: Letterbox): { x: number; y: number } | null; // null outside
export function displaySize(width: number, height: number, orientation: DeviceOrientation): { width: number; height: number };
// deviceInputQueue.ts
export class DeviceInputQueue {
  constructor(send: (events: ReadonlyArray<DeviceInputEvent>) => Promise<void>, schedule?: (flush: () => void) => void /* default requestAnimationFrame */);
  push(event: DeviceInputEvent): void;     // moves for the same pointer coalesce to the latest; others append
  readonly inFlight: boolean;
  dispose(): void;
}
// deviceKeyMap.ts
export function keyboardEventToDeviceInput(event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey">): DeviceInputEvent | null;
```
Right panel surface: `{ id: "device"; kind: "device" }` (single device pane per thread panel).

- [ ] **Step 1: Failing tests**

`deviceGeometry.test.ts`:
```ts
it("letterboxes a portrait device in a wide container", () => {
  expect(letterbox(1000, 800, 1080, 2400)).toEqual({ x: 320, y: 0, width: 360, height: 800 });
});
it("maps client points to normalized coordinates and rejects the bars", () => {
  const box = letterbox(1000, 800, 1080, 2400);
  expect(toNormalized(500, 400, box)).toEqual({ x: 0.5, y: 0.5 });
  expect(toNormalized(100, 400, box)).toBeNull();
});
it("swaps display size in landscape", () => {
  expect(displaySize(1080, 2400, "landscape")).toEqual({ width: 2400, height: 1080 });
});
```
`deviceInputQueue.test.ts`:
```ts
it("keeps one request in flight and coalesces moves", async () => {
  const sent: Array<ReadonlyArray<DeviceInputEvent>> = [];
  let release!: () => void;
  const queue = new DeviceInputQueue(
    (events) => { sent.push(events); return new Promise<void>((resolve) => (release = resolve)); },
    (flush) => flush(),
  );
  queue.push(touch("down", 0.1, 0.1));
  queue.push(touch("move", 0.2, 0.2));
  queue.push(touch("move", 0.3, 0.3));
  queue.push(touch("up", 0.3, 0.3));
  expect(sent).toHaveLength(1);
  release(); await Promise.resolve(); await Promise.resolve();
  expect(sent[1]).toEqual([touch("move", 0.3, 0.3), touch("up", 0.3, 0.3)]);
});
it("splits batches above 64 events", async () => { /* push 70 key events, expect batches of 64 then 6 */ });
it("keeps going after a failed send", async () => { /* send rejects once; next push still sends */ });
```
`deviceKeyMap.test.ts`: `"a"` → text, `"Backspace"` → key backspace down, `"Escape"` → `null`, `"c"` with `metaKey` → `null`.
`jpegRenderer.test.ts`: when two frames arrive while one decode is pending, only the newest is decoded next (inject a fake `createImageBitmap`).
`rightPanelStore.test.ts`: a v9 persisted state migrates unchanged; `{id:"device",kind:"device"}` round-trips at v10; unknown kinds still drop.
`DevicePanel.test.tsx` (render with a mocked catalog atom):
- availability `sdkMissing` → shows the server message and no Start button;
- `ready` with AVDs, no device → AVD select + enabled Start;
- device `booting` → state pill "Booting…", Stop enabled, rotate disabled with title "Available once the device has booted";
- `failed` → "Failed: <reason>" and Start enabled;
- `DeviceLimitError` from Start → message naming the running AVD with a Stop action.
`DevicePreviewsSection.test.tsx`: toggling on calls `updateSettings({enableDevicePreviews:true})`; toggling off with a running device opens a confirm dialog whose confirm button says "Stop <AVD> and turn off".

- [ ] **Step 2: Run** `vp test apps/web/src/components/device apps/web/src/rightPanelStore.test.ts apps/web/src/components/settings/DevicePreviewsSection.test.tsx` → FAIL.

- [ ] **Step 3: Implement**
- client-runtime atoms in the style of `state/preview.ts` (subscription families via `createEnvironmentRpcSubscriptionAtomFamily`, commands via `createEnvironmentRpcCommand`); web binding `export const deviceEnvironment = createDeviceEnvironmentAtoms(connectionAtomRuntime);`.
- `DevicePanel`: reads the thread's environment id (same source the preview panel uses), the server config capability `devicePreviews` and setting `enableDevicePreviews`; hidden entry points when either is false; when the setting is off but the pane was persisted open, render "Device previews are off. Turn them on in Settings → Device previews." with a button that opens that settings section.
- `DeviceToolbar` buttons: Start (select + button), Stop, Rotate left/right (cycles orientation and calls `rotate`), Back/Home/Recents (key down+up), Power, Volume up/down. Each disabled button gets a `title` from `deviceMessages.ts` explaining why (`"Start the device first"`, `"Available once the device has booted"`).
- `DeviceScreen`: `ResizeObserver` → `letterbox`; canvas sized to the box at `devicePixelRatio`; JPEG frames from the `frames` atom drawn by `jpegRenderer` (latest-only: hold at most one pending base64 string; decode via `createImageBitmap(new Blob([bytes], {type:"image/jpeg"}))`, `close()` each bitmap after drawing). Pointer handlers: `onPointerDown` → `setPointerCapture`, push `touch down`; `onPointerMove` (only while captured) → `touch move`; `onPointerUp`/`onPointerCancel`/`onLostPointerCapture` → `touch up|cancel`. Keyboard: canvas `tabIndex=0`, `onKeyDown`/`onKeyUp` via `keyboardEventToDeviceInput`, `preventDefault` only when mapped. Video path is attached in Task 13.
- Panel registration: add `"device"` to `RIGHT_PANEL_KINDS`, `{ id: "device"; kind: "device" }` to the union, parser `case "device": return { id: "device", kind: "device" };`, bump `RIGHT_PANEL_STORAGE_VERSION` to 10 with v9 accepted as-is; tab title "Device" with a phone icon from the icon set already used in `RightPanelTabs`; `ChatView` branch rendering `<DevicePanel environmentId={…} />`; menu + palette entry "Open device preview", hidden unless capability and setting are on.
- `DevicePreviewsSection` modelled on `ProfilerSection` (`settings.enableDevicePreviews`, `updateSettings`), description: "Run an Android emulator on this environment's server and show it in a side panel. Requires the Android SDK with at least one virtual device and hardware acceleration on the server."

- [ ] **Step 4: Run** the Step 2 command → PASS; then `vp run typecheck`.

- [ ] **Step 5: Review** changed components against `/vercel-react-best-practices` and `UI.md` (disabled-state titles, actionable errors, the off-toggle confirmation preserving the user's running device). Note the results for the final report.

- [ ] **Step 6: Commit** `feat(web): device preview pane with JPEG rendering and input (#70)`.

---

### Task 10: scrcpy-server session

**Files:**
- Create: `apps/server/src/device/scrcpy.rs`, `apps/server/src/device/scrcpy/scrcpy-server-v4.1` (download the `scrcpy-server-v4.1` asset from the GitHub release `v4.1` and verify against that release's `SHA256SUMS.txt`), `apps/server/src/device/scrcpy/NOTICE.md`
- Modify: `apps/server/src/device/testing.rs` (fake scrcpy source + adb `push`/`forward`/`shell` handling)

**Interfaces:**
- Produces:
```rust
pub(crate) const SCRCPY_VERSION: &str = "4.1";
pub(crate) const SCRCPY_SERVER_JAR: &[u8] = include_bytes!("scrcpy/scrcpy-server-v4.1");
pub(crate) const SCRCPY_SERVER_SHA256: &str = "deacb991ed2509715160ffdc7907e47b4160eb30d1566217e9047fd5b8850cae";
#[derive(Clone, Debug, PartialEq)] pub enum UnitKind { Config, Key, Delta }
#[derive(Clone, Debug)] pub struct VideoUnit { pub kind: UnitKind, pub pts_us: u64, pub data: bytes::Bytes }
pub(crate) struct CodecInfo { pub codec_id: u32, pub width: u32, pub height: u32 }
pub(crate) fn parse_codec_header(bytes: [u8; 12]) -> CodecInfo;
pub(crate) fn parse_packet_header(bytes: [u8; 12]) -> (UnitKind, u64 /*pts*/, u32 /*len*/);
pub(crate) fn server_args(scid: u32) -> Vec<String>;
pub(crate) struct ScrcpySession { pub units: mpsc::Receiver<VideoUnit>, pub codec: CodecInfo, control: tokio::net::TcpStream, helper: RegisteredHelper, forward: (PathBuf /*adb*/, String /*serial*/, u16 /*local port*/) }
impl ScrcpySession {
    pub(crate) async fn start(adb: &Path, serial: &str, registry: &ProcessAttributionRegistry) -> Result<Self, DeviceError>;
    pub(crate) async fn reset_video(&mut self) -> Result<(), DeviceError>;   // writes control byte 17 (TYPE_RESET_VIDEO)
    pub(crate) async fn stop(self);                                         // terminate helper, `adb forward --remove`
}
```
Packet header (scrcpy `doc/develop.md`, "Video" section; confirm for v4.1 by reading `app/src/demuxer.c` at tag `v4.1`): 8-byte big-endian field whose bit 63 is the config flag, bit 62 the key-frame flag, low 62 bits the PTS; then 4-byte big-endian length. Codec header: 4-byte codec id (`0x68323634` = "h264"), 4-byte width, 4-byte height.

- [ ] **Step 1: Failing tests**
```rust
#[test]
fn embedded_jar_matches_pinned_checksum() {
    use sha2::{Digest, Sha256};
    assert_eq!(hex(&Sha256::digest(SCRCPY_SERVER_JAR)), SCRCPY_SERVER_SHA256);
}

#[test]
fn parses_packet_flags_and_pts() {
    let mut header = [0_u8; 12];
    header[..8].copy_from_slice(&((1_u64 << 62) | 123_456).to_be_bytes());
    header[8..].copy_from_slice(&4096_u32.to_be_bytes());
    assert_eq!(parse_packet_header(header), (UnitKind::Key, 123_456, 4096));
    header[..8].copy_from_slice(&(1_u64 << 63).to_be_bytes());
    assert_eq!(parse_packet_header(header).0, UnitKind::Config);
}

#[test]
fn server_args_pin_version_and_disable_audio() {
    let args = server_args(0x1234_abcd);
    assert_eq!(args[..3], ["CLASSPATH=/data/local/tmp/bibcode-scrcpy-server-v4.1.jar".to_owned(), "app_process".into(), "/".into()]);
    for expected in ["com.genymobile.scrcpy.Server", "4.1", "scid=1234abcd", "tunnel_forward=true", "video=true", "audio=false",
                     "control=true", "max_size=1280", "video_bit_rate=4000000", "max_fps=30", "send_device_meta=false"] {
        assert!(args.iter().any(|arg| arg == expected), "{expected}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn session_streams_units_and_sends_reset() {
    let source = FakeScrcpySource::spawn(vec![config_unit(), key_unit(1), delta_unit(2)]).await;
    let sdk = FakeSdk::new().adb_forwards_to(source.port).write();
    let mut session = ScrcpySession::start(&sdk.adb(), "emulator-5554", &ProcessAttributionRegistry::new()).await.unwrap();
    assert_eq!(session.codec.codec_id, 0x6832_3634);
    let kinds: Vec<_> = [session.units.recv().await, session.units.recv().await, session.units.recv().await]
        .into_iter().map(|unit| unit.unwrap().kind).collect();
    assert_eq!(kinds, [UnitKind::Config, UnitKind::Key, UnitKind::Delta]);
    session.reset_video().await.unwrap();
    assert_eq!(source.control_bytes().await, vec![17]);
    session.stop().await;
    assert!(sdk.adb_log().contains("forward --remove tcp:"));
}
```
`FakeScrcpySource` listens on loopback; on the first connection (video) it writes one dummy byte `0` (tunnel_forward mode), the 12-byte codec header, then the given packets; on the second connection (control) it records bytes. The fake `adb` script: `push` → exit 0; `shell sha256sum …` → prints a mismatching hash (forces push); `shell CLASSPATH=…` → `exec sleep 600`; `forward tcp:0 localabstract:scrcpy_<scid>` → prints the fake source port; `forward --remove …` → logs and exits 0.

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement** `start`:
1. `adb -s <serial> shell sha256sum /data/local/tmp/bibcode-scrcpy-server-v4.1.jar`; push only when the hash differs (`adb -s <serial> push <tempfile> <remote>` with the embedded bytes written to a temp file).
2. `scid = rand::random::<u32>() & 0x7fff_ffff` (use the crate's existing randomness source, e.g. `getrandom`).
3. `adb -s <serial> forward tcp:0 localabstract:scrcpy_<scid as 8 hex>`; parse the printed local port.
4. Spawn `adb -s <serial> shell <server_args…>` through `spawn_registered_helper(.., "android-scrcpy", registry)`.
5. Connect the video socket to `127.0.0.1:<port>` (retry every 100 ms for 5 s; scrcpy needs a moment to listen), read the dummy byte, then connect the control socket; read the codec header.
6. Spawn a reader task that loops `read_exact(12)` → header → `read_exact(len)` → send `VideoUnit` on a `mpsc::channel(64)`; on EOF or error, log with `tracing::warn!` and end (the channel closes).
`reset_video` writes `[17]` to the control socket. `stop`: `note_stop_requested`, terminate helper, `adb forward --remove tcp:<port>`.

`NOTICE.md`: "scrcpy-server v4.1, © Genymobile and Romain Vimont, Apache License 2.0, https://github.com/Genymobile/scrcpy (asset `scrcpy-server-v4.1`, SHA-256 `deacb991ed2509715160ffdc7907e47b4160eb30d1566217e9047fd5b8850cae`). `proto/android/emulator_controller.proto`, © The Android Open Source Project, Apache License 2.0, from https://github.com/google/android-emulator-container-scripts at 7c2c4dfd6f18c3f85dabaf421fe51dcd4961f2ce." plus the full Apache-2.0 text.

- [ ] **Step 4: Run** → PASS. **Step 5: Commit** `feat(server): pinned scrcpy-server H.264 session (#70)`.

---

### Task 11: Media fan-out, tickets, plain media socket

**Files:**
- Create: `apps/server/src/device/media.rs`, `apps/server/src/device/media_socket.rs`
- Modify: `apps/server/src/production/http_routes.rs:324` (route), `HttpRoutesState` (add `devices: DeviceManager`), `apps/server/src/production/device_rpc.rs` (ticket RPC), `apps/server/src/rpc/session.rs` (record the connection channel), `apps/server/src/rpc/e2ee.rs` (mark context as E2EE), `apps/server/src/http.rs:364-376` (mark plain)
- Modify: contracts `rpc.ts` (+ `deviceCreateMediaTicket` Rpc), manifest, `methods.rs` (`read_unary("device.createMediaTicket")`), `scope.rs` (read arm)
- Test: unit tests in `media.rs`; `apps/server/tests/device_media_socket.rs`

**Interfaces:**
- Produces:
```rust
pub(crate) const SOCKET_QUEUE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_SOCKETS_PER_DEVICE: usize = 4;
pub(crate) const RESET_MIN_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const TICKET_TTL: Duration = Duration::from_secs(60);
#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum MediaChannel { Plain, E2ee }
pub enum TicketError { Expired, Reused, WrongDevice, WrongChannel, Unknown }   // close reasons expired|reused|wrongDevice|wrongChannel|expired(unknown)
pub(crate) fn encode_frame(unit: &VideoUnit) -> Vec<u8>;                    // [kind u8][pts u64 BE][data]
impl DeviceManager {
    pub fn create_media_ticket(&self, device_id: &str, principal: &str, channel: MediaChannel) -> Result<(String, i64), DeviceOpError>;
    pub(crate) fn redeem_media_ticket(&self, ticket: &str, channel: MediaChannel) -> Result<(String /*device*/, String /*principal*/), TicketError>;
    pub(crate) async fn attach_media(&self, device_id: &str) -> Result<MediaSubscriber, MediaAttachError>; // Limit | NotReady | Disabled
}
pub(crate) struct MediaSubscriber { pub frames: mpsc::Receiver<Bytes>, pub closed: CancellationToken, guard: MediaGuard }
impl MediaSubscriber { pub(crate) fn request_keyframe(&self); }
```
`RpcSessionContext` gains `channel: MediaChannel` (default `Plain`) with `pub(crate) fn with_channel(self, MediaChannel) -> Self` and `pub(crate) fn channel(&self) -> MediaChannel`; `e2ee.rs:1452` calls `.with_channel(MediaChannel::E2ee)`.

Fan-out design (`media.rs`): per device one `MediaHub { session: ScrcpySession, subscribers: Vec<SubscriberSlot>, cached_config: Option<Bytes>, last_reset: Option<Instant> }`, started on the first attach and stopped when the last subscriber leaves, the device leaves `Ready`, or the setting turns off. `SubscriberSlot { sender: mpsc::Sender<Bytes> (bounded 256 items), queued_bytes: Arc<AtomicUsize>, waiting_for_key: bool }`. The hub loop takes each `VideoUnit`:
- `Config` → cache, send to all.
- `Key` → for each slot: clear `waiting_for_key`, send.
- `Delta` → skip slots with `waiting_for_key`; send to others.
- Before sending, if `queued_bytes + len > SOCKET_QUEUE_BYTES` or `try_send` fails → mark `waiting_for_key = true` and drain that slot's queue (the socket writer drains with a generation counter so stale deltas already queued are discarded), then `maybe_reset()` (send `RESET_VIDEO` if `last_reset` is older than `RESET_MIN_INTERVAL`).
- A new subscriber starts with `waiting_for_key = true`, receives the cached config, and triggers `maybe_reset()`.
- `request_keyframe()` from a socket sets its slot `waiting_for_key` and calls `maybe_reset()`.

- [ ] **Step 1: Failing tests**

`media.rs`:
```rust
#[test]
fn frames_encode_kind_and_pts() {
    let frame = encode_frame(&VideoUnit { kind: UnitKind::Key, pts_us: 7, data: Bytes::from_static(&[0, 0, 0, 1, 0x65]) });
    assert_eq!(frame[0], 1);
    assert_eq!(u64::from_be_bytes(frame[1..9].try_into().unwrap()), 7);
    assert_eq!(&frame[9..], &[0, 0, 0, 1, 0x65]);
}

#[test]
fn tickets_are_single_use_bound_and_expiring() {
    let tickets = TicketBook::default();
    let (ticket, _) = tickets.issue("dev", "session-a", MediaChannel::E2ee, now());
    assert_eq!(tickets.redeem(&ticket, MediaChannel::Plain, now()), Err(TicketError::WrongChannel));
    let (ticket, _) = tickets.issue("dev", "session-a", MediaChannel::Plain, now());
    assert!(tickets.redeem(&ticket, MediaChannel::Plain, now()).is_ok());
    assert_eq!(tickets.redeem(&ticket, MediaChannel::Plain, now()), Err(TicketError::Reused));
    let (ticket, _) = tickets.issue("dev", "session-a", MediaChannel::Plain, now());
    assert_eq!(tickets.redeem(&ticket, MediaChannel::Plain, now() + TICKET_TTL + Duration::from_secs(1)), Err(TicketError::Expired));
}

#[tokio::test]
async fn slow_socket_skips_to_next_key_unit() {
    let hub = HubFixture::new();                   // drives MediaHub with a channel-fed fake session
    let fast = hub.subscribe();
    let slow = hub.subscribe_with_capacity(1);     // tiny queue
    hub.feed([config(), key(1), delta(2), delta(3), delta(4), key(5), delta(6)]).await;
    assert_eq!(fast.kinds().await, ["config", "key", "delta", "delta", "delta", "key", "delta"]);
    let slow_kinds = slow.kinds().await;
    assert_eq!(slow_kinds.first().map(String::as_str), Some("config"));
    assert!(!slow_kinds.windows(2).any(|pair| pair[0] == "skipped-key" ), "never a delta after a dropped unit before the next key");
    assert_eq!(slow_kinds.iter().filter(|kind| *kind == "key").last().map(String::as_str), Some("key"));
    assert!(hub.resets() >= 1);
}

#[tokio::test(start_paused = true)]
async fn resets_are_rate_limited_per_device() {
    let hub = HubFixture::new();
    for _ in 0..5 { hub.subscribe(); }
    assert_eq!(hub.resets(), 1);
    tokio::time::advance(RESET_MIN_INTERVAL).await;
    hub.subscribe();
    assert_eq!(hub.resets(), 2);
}

#[tokio::test]
async fn fifth_socket_is_refused() {
    let hub = HubFixture::new();
    for _ in 0..MAX_SOCKETS_PER_DEVICE { hub.subscribe(); }
    assert!(matches!(hub.try_subscribe(), Err(MediaAttachError::Limit)));
}
```
(`slow.kinds()` reports what the slow socket actually received; the assertion that matters: after the first dropped unit, the next unit it receives is a `key`. Write it as: find the first gap in received PTS, assert the unit after the gap has kind `key`.)

`tests/device_media_socket.rs` (`#[cfg(unix)]`, full server with fake SDK, fake controller, fake scrcpy):
```rust
#[tokio::test]
async fn plain_socket_requires_a_ticket_and_streams_config_first() { /* connect ws://…/api/device-media, send {"type":"ticket",…}, expect first binary frame kind 0 */ }
#[tokio::test]
async fn bad_ticket_closes_with_reason() { /* reused ticket → close frame reason "reused" */ }
#[tokio::test]
async fn ticket_in_query_or_cookie_is_not_accepted() { /* no first message within 5 s → close "expired" */ }
#[tokio::test]
async fn disabling_closes_media_sockets() { /* settings update enableDevicePreviews=false → close reason "disabled", emulator reaped */ }
```

- [ ] **Step 2: Run** → FAIL.

- [ ] **Step 3: Implement**
- `TicketBook`: `Mutex<HashMap<String, TicketRecord>>`; ticket = 32 random bytes base64url; prune expired on each issue; cap 256 outstanding (oldest evicted).
- `create_media_ticket` checks enabled and `Ready`.
- `device.createMediaTicket` handler via `register_unary_with_context`: principal session id from `context.principal()` (missing → `DevicePreviewsDisabledError`-style defect is wrong; return `{"_tag":"Defect","message":"device media needs an authenticated session"}`), channel from `context.channel()`; returns `{ticket, expiresAtMs}`.
- `media_socket.rs` plain handler (`GET /api/device-media`, axum `WebSocketUpgrade`): no cookie/bearer auth layer (the route is added to the public-route allowlist alongside `/api/transfers/{token}`; find it with `rg -n "api/transfers" apps/server/src/http.rs`). After upgrade: wait ≤ 5 s for one text message `{"type":"ticket","ticket":…}`; redeem with `MediaChannel::Plain`; verify the principal's session is still active (`auth.session_is_active(principal)` or the equivalent check used by `/api/transfers` tokens); `attach_media`; then loop: `select!` on `subscriber.frames` → `Message::Binary`, inbound text `{"type":"keyframe"}` → `request_keyframe()`, `subscriber.closed` → close with reason (`disabled|stopped`), socket close → drop. Close codes: 4400 + reason string.
- Log failures with `tracing::warn!`; never log the ticket.

- [ ] **Step 4: Run** `cargo test -p bibcode-server device::media --test device_media_socket --test rpc_wire` and `vp test packages/contracts` → PASS.

- [ ] **Step 5: Commit** `feat(server): ticketed H.264 media socket with keyframe backpressure (#70)`.

---

### Task 12: E2EE media channel

**Files:**
- Modify: `apps/server/src/rpc/e2ee.rs` (make `E2eeChannel::respond` non-test `pub(crate)`, add `pub(crate) fn encrypt_message`, add `pub(crate) fn decrypt_message(&mut self, frame: &[u8], max: usize) -> Result<Option<Vec<u8>>, E2eeSessionError>` without the RPC budget types, using the existing record assembly with `MAX_E2EE_PREAUTH_MESSAGE_BYTES` for inbound)
- Modify: `apps/server/src/device/media_socket.rs` (E2EE handler), `http_routes.rs` (route `/api/device-media-e2ee`)
- Test: `apps/server/tests/device_media_socket.rs`; `packages/client-runtime/src/device/mediaSocket.test.ts` (interop, modelled on `e2ee/serverInterop.test.ts`)

**Interfaces:**
- Consumes: `HostIdentity` (the same one `/ws-e2ee` uses — pass it into `HttpRoutesState` or reach it the way the `/ws-e2ee` handler does; `rg -n "run_e2ee_session\(" apps/server/src`).
- Produces: E2EE route semantics identical to Task 11 except: handshake first (10 s timeout, pre-auth admission via `E2eePreauthAdmission` exactly as `/ws-e2ee`), then the first decrypted logical message is the ticket JSON, redeemed with `MediaChannel::E2ee`; every outbound frame is `encrypt_message(&encode_frame(unit))` sent as one or more binary messages (records ≤ 65,518 bytes, continuation flag).

- [ ] **Step 1: Failing tests**
```rust
#[tokio::test]
async fn e2ee_socket_handshakes_and_streams_encrypted_units() {
    // Use the Rust test initiator that tests/e2ee_ws.rs already uses (snow initiator with the pinned key).
    // Mint the ticket over an E2EE RPC connection, open /api/device-media-e2ee, handshake, send the ticket record,
    // reassemble and decrypt; the first logical message has kind byte 0 (config); a key unit > 65,518 bytes arrives in several records.
}
#[tokio::test]
async fn plain_ticket_is_refused_on_e2ee_route() { /* close reason "wrongChannel" */ }
#[tokio::test]
async fn wrong_host_key_fails_handshake() { /* initiator pinned to another key → connection closed, no ticket consumed */ }
```
`mediaSocket.test.ts`: run the real server binary as `serverInterop.test.ts` does (or skip under the same env guard it uses), open the E2EE media socket with `openDeviceMediaSocket` (Task 13 API), assert the first frame parses as `config`.

- [ ] **Step 2: Run** → FAIL. **Step 3: Implement** as described; reuse `next_binary_frame`/`send_encrypted_frames` helpers from `e2ee.rs` where their signatures fit, otherwise add small `pub(crate)` wrappers rather than copying the record logic.

- [ ] **Step 4: Run** `cargo test -p bibcode-server --test device_media_socket --test e2ee_ws rpc::e2ee` → PASS.

- [ ] **Step 5: Commit** `feat(server): Noise-encrypted device media socket for pinned profiles (#70)`.

---

### Task 13: H.264 worker, codec selection, fallback

**Files:**
- Create: `packages/client-runtime/src/device/mediaFrames.ts` (+ test), `packages/client-runtime/src/device/mediaSocket.ts` (+ test); export subpaths in `packages/client-runtime/package.json` the way `e2ee` is exported
- Create: `apps/web/src/components/device/deviceVideo.worker.ts`, `useDeviceVideo.ts` (+ tests for the selection state machine)
- Modify: `apps/web/src/components/device/DeviceScreen.tsx`

**Interfaces:**
- Produces:
```ts
// mediaFrames.ts
export type MediaFrameKind = "config" | "key" | "delta";
export interface MediaFrame { readonly kind: MediaFrameKind; readonly ptsUs: bigint; readonly data: Uint8Array }
export function parseMediaFrame(bytes: Uint8Array): MediaFrame; // throws MediaFrameError on short/unknown kind
export function avcCodecString(sps: Uint8Array): string;        // "avc1.PPCCLL" from the SPS NAL in a config frame
// mediaSocket.ts (no DOM; runs in a worker)
export interface DeviceMediaTarget { readonly url: string; readonly hostKey: string | null } // hostKey set → E2EE route
export function deviceMediaTarget(prepared: Pick<PreparedConnection, "socketUrl" | "e2ee">): DeviceMediaTarget;
export function openDeviceMediaSocket(target: DeviceMediaTarget, ticket: string, handlers: {
  onFrame(frame: MediaFrame): void; onClose(reason: string): void;
}): { requestKeyframe(): void; close(): void };
// useDeviceVideo.ts
export type VideoMode = { kind: "probing" } | { kind: "h264" } | { kind: "jpeg"; reason: "unsupported" | "noFirstFrame" | "decoderError" | "rejected" };
export function useDeviceVideo(input: { environmentId: EnvironmentId; deviceId: string | null; canvas: HTMLCanvasElement | null }): VideoMode;
```
`deviceMediaTarget`: replace the socket URL's path with `/api/device-media` (plain) or `/api/device-media-e2ee` (when `prepared.e2ee` is non-null, `hostKey = prepared.e2ee.hostKey`), keeping scheme/host/port.

- [ ] **Step 1: Failing tests**
```ts
// mediaFrames.test.ts
it("parses kind, pts and payload", () => {
  const bytes = new Uint8Array([1, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, 0x65]);
  expect(parseMediaFrame(bytes)).toEqual({ kind: "key", ptsUs: 7n, data: new Uint8Array([0, 0, 0, 1, 0x65]) });
});
it("rejects truncated frames", () => { expect(() => parseMediaFrame(new Uint8Array([1, 0]))).toThrow(MediaFrameError); });
it("derives the avc1 codec string from an SPS", () => {
  expect(avcCodecString(new Uint8Array([0x67, 0x42, 0xc0, 0x1f]))).toBe("avc1.42c01f");
});
// mediaSocket.test.ts
it("targets the plain or e2ee route on the same origin", () => {
  expect(deviceMediaTarget({ socketUrl: "wss://h.example/ws", e2ee: null }).url).toBe("wss://h.example/api/device-media");
  expect(deviceMediaTarget({ socketUrl: "ws://10.0.0.2:3773/ws-e2ee", e2ee: { hostKey: "k", auth: anyAuth } })).toEqual({ url: "ws://10.0.0.2:3773/api/device-media-e2ee", hostKey: "k" });
});
it("sends the ticket as the first message on the plain route", () => { /* fake WebSocket records first send === JSON ticket */ });
// useDeviceVideo.test.ts
it("uses jpeg when VideoDecoder is missing", () => { /* expect { kind: "jpeg", reason: "unsupported" } and no ticket request */ });
it("falls back to jpeg when no frame arrives within 5 s", () => { /* fake timers */ });
it("falls back to jpeg on decoder error and closes the socket", () => {});
it("re-requests a ticket on reconnect", () => {});
```

- [ ] **Step 2: Run** `vp test packages/client-runtime/src/device apps/web/src/components/device` → FAIL.

- [ ] **Step 3: Implement**
- `mediaSocket.ts` plain: `new WebSocket(url)`, `binaryType = "arraybuffer"`, on open send `JSON.stringify({type:"ticket",ticket})`, on message `parseMediaFrame(new Uint8Array(data))`. E2EE: `createNkInitiator({ responderStaticPublicKey: decodeBase64UrlKey(hostKey) })`, send `writeMessageA(new Uint8Array())`, read message B, `split()`, send the ticket as records (`splitIntoRecords` + `transport.send.encryptWithAd(EMPTY, record)`), decrypt incoming with `transport.receive.decryptWithAd` into a `RecordAssembler`, and `parseMediaFrame` each assembled message. `requestKeyframe` sends `{"type":"keyframe"}` (encrypted on E2EE).
- Worker: receives `{ type: "start", target, ticket, canvas?: OffscreenCanvas }`; owns the socket and a `VideoDecoder` configured from the first config frame (`codec: avcCodecString(sps)`, `optimizeForLatency: true`, Annex-B input so no `description`); decodes `key`/`delta` as `EncodedVideoChunk({ type: kind === "key" ? "key" : "delta", timestamp: Number(ptsUs), data })`; draws each `VideoFrame` to the `OffscreenCanvas` (or posts it, transferred, to the main thread when no canvas was given) and closes it; on decoder error posts `{type:"error"}` and requests a keyframe once before giving up; posts `{type:"firstFrame"}` once.
- `useDeviceVideo`: if `typeof VideoDecoder === "undefined"` or `VideoDecoder.isConfigSupported({ codec: "avc1.42E01F" })` is not supported → jpeg/unsupported. Otherwise mint a ticket (`deviceEnvironment.createMediaTicket`), start the worker (transfer `canvas.transferControlToOffscreen()` when available), 5 s first-frame timer → jpeg/noFirstFrame, worker error → jpeg/decoderError, socket close with a ticket reason → jpeg/rejected, transient close → new ticket and restart (max 3 retries per minute, then jpeg). `DeviceScreen` renders the JPEG path only when the mode is `jpeg` (subscribing to `frames` only then, so the JPEG producer does not run while H.264 works).

- [ ] **Step 4: Run** → PASS; `vp run typecheck`.

- [ ] **Step 5: Review** with `/vercel-react-best-practices` (worker lifetime tied to effect cleanup, no re-render per frame) and `UI.md` (a small "Low-bandwidth mode" badge with a tooltip explaining why when the mode is `jpeg`).

- [ ] **Step 6: Commit** `feat(web): H.264 device video via WebCodecs worker with JPEG fallback (#70)`.

---

### Task 14: Documentation, runbooks, full validation

**Files:**
- Modify: `docs/architecture/overview.md` (new "Device previews" section; process topology adds `android-emulator` and `android-scrcpy` helpers; Profiler section adds `device.lifecycle`), `docs/architecture/rpc-and-orchestration.md` (methods table; invariant: "device H.264 media is the only application traffic outside typed RPC; it is ticket-authenticated and Noise-encrypted on pinned profiles"), `docs/architecture/remote.md` (media socket over relay/SSH/E2EE, security boundaries, current limitations: emulator-only, one device, MCP tool list refresh), `docs/reference/workspace-layout.md` (right-panel kind `device`), `docs/user/workspace-ui.md` (how to enable and use), `docs/skills/bibcode-profiler/references/` (event `device.lifecycle`), `docs/testing/README.md` + `linux-desktop.md` + `macos-desktop.md` + `windows-desktop.md` (device-preview validation procedure: prerequisites, enable setting, start AVD, verify H.264 in desktop, JPEG in browser over `http://<lan-ip>`, tap/swipe/type/rotate, stop, confirm no `emulator`/`qemu` process remains after stop and after server shutdown), and the execution-report template if it enumerates validation areas
- Modify: `docs/plans/2026-10-06-internal-browser-and-mobile-emulator-research.md` is historical — do not edit; the correction lives in the spec.

- [ ] **Step 1: Write the docs** (verify every command and path against source while writing).

- [ ] **Step 2: Full validation**
```bash
vp install
vp check
vp run typecheck
vp test
cargo fmt --all --check
cargo clippy -p bibcode-server --all-targets -- -D warnings
cargo clippy -p bibcode-desktop --all-targets -- -D warnings
cargo test --workspace -j 2
```
Expected: all pass. Fix and re-run only what failed.

- [ ] **Step 3: Native acceptance (needs an Android SDK host)** following the new runbook section on a Linux KVM server reached from a remote desktop client and a browser-mode client; record results in a report from `docs/testing/execution-report-template.md` (outside the repo). Confirm or correct each "Native verification item"; any correction is a code change with its test updated.

- [ ] **Step 4: Final review** — `git diff main...` and `git status --short` for stray files, debug output, dependency drift; confirm `.codegraph/` is untouched.

- [ ] **Step 5: Commit** `docs: device previews architecture, user guide and validation runbooks (#70)`.
