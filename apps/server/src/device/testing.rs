//! In-process fake of the emulator gRPC controller for hermetic tests.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::{
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tokio::{net::TcpListener, sync::watch};
use tokio_stream::{
    Stream, StreamExt,
    wrappers::{TcpListenerStream, WatchStream},
};
use tonic::{Request, Response, Status, Streaming};

use super::grpc::proto::{
    self,
    emulator_controller_server::{EmulatorController, EmulatorControllerServer},
    image_format::ImgFormat,
    rotation::SkinRotation,
};

/// Token the fake requires as `authorization: Bearer test-token`.
const TOKEN: &str = "test-token";
const INITIAL_FRAME: (u32, u32, u8) = (1080, 2400, 0x80);

/// A 1x1 PNG served for PNG screenshot requests (the requested size is only reported
/// in `Image.format`, never encoded).
const MINIMAL_PNG: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x68, 0x68, 0x68, 0x00,
    0x00, 0x03, 0x04, 0x01, 0x81, 0x4b, 0xd3, 0xd2, 0x10, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

pub(crate) struct FakeHandle {
    pub(crate) port: u16,
    pub(crate) inputs: Arc<Mutex<Vec<proto::InputEvent>>>,
    pub(crate) rotations: Arc<Mutex<Vec<[f32; 3]>>>,
    /// Native display `(width, height, gray)`; every change emits one streamed frame.
    /// A `(0, 0, gray)` frame keeps the initial 1080x2400 display.
    pub(crate) frame_tx: watch::Sender<(u32, u32, u8)>,
    pub(crate) vm_state: Arc<Mutex<Option<i32>>>,
    /// The rotation every served image reports; `None` derives it from the frame's shape.
    rotation: watch::Sender<Option<SkinRotation>>,
    screenshot_streams: Arc<AtomicUsize>,
    input_streams: Arc<AtomicUsize>,
    input_resets: watch::Sender<u64>,
}

impl FakeHandle {
    /// Reports `rotation` on every served image from now on, with the display's width and
    /// height swapped to match it, and streams one frame.
    pub(crate) fn report_rotation(&self, rotation: SkinRotation) {
        self.rotation.send_replace(Some(rotation));
    }

    /// Open `streamScreenshot` calls.
    pub(crate) fn active_screenshot_streams(&self) -> usize {
        self.screenshot_streams.load(Ordering::SeqCst)
    }

    /// Ends every open input stream with an error, waits until they have ended, and forgets
    /// the inputs they recorded.
    pub(crate) async fn break_input_stream(&self) {
        self.input_resets.send_modify(|resets| *resets += 1);
        wait_until(|| self.input_streams.load(Ordering::SeqCst) == 0).await;
        self.inputs.lock().expect("fake inputs lock").clear();
    }

    /// Waits up to ten seconds for at least `count` recorded inputs and returns them all.
    pub(crate) async fn wait_inputs(&self, count: usize) -> Vec<proto::InputEvent> {
        wait_until(|| self.inputs.lock().expect("fake inputs lock").len() >= count).await;
        self.inputs.lock().expect("fake inputs lock").clone()
    }

    /// Sends `pid` `SIGTERM` once the VM is asked to shut down, as a real emulator exits
    /// after `setVmState(SHUTDOWN)`.
    #[cfg(unix)]
    pub(crate) fn exit_on_shutdown(&self, pid: u32) {
        let vm_state = self.vm_state.clone();
        tokio::spawn(async move {
            loop {
                // A process that is already gone (and reaped) needs no signal.
                if !process_alive(pid) {
                    return;
                }
                let state = *vm_state.lock().expect("fake vm state lock");
                if state == Some(proto::vm_run_state::RunState::Shutdown as i32) {
                    // SAFETY: plain signal delivery to a PID the test launched.
                    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });
    }
}

pub(crate) struct FakeController {
    inputs: Arc<Mutex<Vec<proto::InputEvent>>>,
    rotations: Arc<Mutex<Vec<[f32; 3]>>>,
    frames: watch::Receiver<(u32, u32, u8)>,
    vm_state: Arc<Mutex<Option<i32>>>,
    rotation: watch::Receiver<Option<SkinRotation>>,
    screenshot_streams: Arc<AtomicUsize>,
    input_streams: Arc<AtomicUsize>,
    input_resets: watch::Receiver<u64>,
}

/// Counts one open stream until dropped.
struct OpenStream(Arc<AtomicUsize>);

impl OpenStream {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count.clone())
    }
}

impl Drop for OpenStream {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FakeController {
    pub(crate) async fn spawn() -> FakeHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake emulator");
        let port = listener.local_addr().expect("fake emulator address").port();
        let (frame_tx, frames) = watch::channel(INITIAL_FRAME);
        let (rotation_tx, rotation) = watch::channel(None);
        let (input_resets_tx, input_resets) = watch::channel(0);
        let handle = FakeHandle {
            port,
            inputs: Arc::default(),
            rotations: Arc::default(),
            frame_tx,
            vm_state: Arc::default(),
            rotation: rotation_tx,
            screenshot_streams: Arc::default(),
            input_streams: Arc::default(),
            input_resets: input_resets_tx,
        };
        let service = EmulatorControllerServer::with_interceptor(
            Self {
                inputs: handle.inputs.clone(),
                rotations: handle.rotations.clone(),
                frames,
                vm_state: handle.vm_state.clone(),
                rotation,
                screenshot_streams: handle.screenshot_streams.clone(),
                input_streams: handle.input_streams.clone(),
                input_resets,
            },
            require_token,
        );
        tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await;
        });
        handle
    }
}

fn require_token(request: Request<()>) -> Result<Request<()>, Status> {
    let expected = format!("Bearer {TOKEN}");
    match request.metadata().get("authorization") {
        Some(value) if value.as_bytes() == expected.as_bytes() => Ok(request),
        _ => Err(Status::unauthenticated("missing or wrong emulator token")),
    }
}

/// Scales the native display to `requested_width` keeping the aspect ratio; `0` keeps native.
fn scaled_size(
    (native_width, native_height, _): (u32, u32, u8),
    requested_width: u32,
) -> (u32, u32) {
    if requested_width == 0 || requested_width >= native_width {
        return (native_width, native_height);
    }
    let height = u64::from(requested_width) * u64::from(native_height) / u64::from(native_width);
    (requested_width, height as u32)
}

fn frame_image(
    frame: (u32, u32, u8),
    reported: Option<SkinRotation>,
    request: &proto::ImageFormat,
    seq: u32,
) -> proto::Image {
    let (mut native_width, mut native_height, gray) = frame;
    if native_width == 0 || native_height == 0 {
        (native_width, native_height) = (INITIAL_FRAME.0, INITIAL_FRAME.1);
    }
    if let Some(reported) = reported {
        let landscape = matches!(
            reported,
            SkinRotation::Landscape | SkinRotation::ReverseLandscape
        );
        if landscape != (native_width > native_height) {
            (native_width, native_height) = (native_height, native_width);
        }
    }
    let frame = (native_width, native_height, gray);
    let (width, height) = scaled_size(frame, request.width);
    let png = request.format == ImgFormat::Png as i32;
    let bytes = if png {
        MINIMAL_PNG.to_vec()
    } else {
        let channels = if request.format == ImgFormat::Rgba8888 as i32 {
            4
        } else {
            3
        };
        vec![frame.2; width as usize * height as usize * channels]
    };
    let rotation = reported.unwrap_or(if width > height {
        SkinRotation::Landscape
    } else {
        SkinRotation::Portrait
    });
    proto::Image {
        format: Some(proto::ImageFormat {
            format: request.format,
            width,
            height,
            rotation: Some(proto::Rotation {
                rotation: rotation as i32,
                ..Default::default()
            }),
            ..Default::default()
        }),
        image: bytes,
        seq,
        ..Default::default()
    }
}

type ImageStream = Pin<Box<dyn Stream<Item = Result<proto::Image, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl EmulatorController for FakeController {
    type streamSensorStream = tokio_stream::Empty<Result<proto::SensorValue, Status>>;
    type streamPhysicalModelStream = tokio_stream::Empty<Result<proto::PhysicalModelValue, Status>>;
    type streamClipboardStream = tokio_stream::Empty<Result<proto::ClipData, Status>>;
    type streamAudioStream = tokio_stream::Empty<Result<proto::AudioPacket, Status>>;
    type streamLogcatStream = tokio_stream::Empty<Result<proto::LogMessage, Status>>;
    type streamNotificationStream = tokio_stream::Empty<Result<proto::Notification, Status>>;
    type streamScreenshotStream = ImageStream;

    async fn get_status(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::EmulatorStatus>, Status> {
        Ok(Response::new(proto::EmulatorStatus {
            booted: true,
            ..Default::default()
        }))
    }

    async fn get_screenshot(
        &self,
        request: Request<proto::ImageFormat>,
    ) -> Result<Response<proto::Image>, Status> {
        let frame = *self.frames.borrow();
        let rotation = *self.rotation.borrow();
        Ok(Response::new(frame_image(
            frame,
            rotation,
            request.get_ref(),
            0,
        )))
    }

    async fn stream_screenshot(
        &self,
        request: Request<proto::ImageFormat>,
    ) -> Result<Response<Self::streamScreenshotStream>, Status> {
        let format = request.into_inner();
        let mut seq = 0;
        let open = OpenStream::new(&self.screenshot_streams);
        let (frames, rotation) = (self.frames.clone(), self.rotation.clone());
        let changes = WatchStream::new(self.frames.clone())
            .map(|_| ())
            .merge(WatchStream::from_changes(self.rotation.clone()).map(|_| ()));
        let stream = changes.map(move |()| {
            // Held by the stream, so the count drops when the client goes away.
            let _open = &open;
            let image = frame_image(*frames.borrow(), *rotation.borrow(), &format, seq);
            seq += 1;
            Ok(image)
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn stream_input_event(
        &self,
        request: Request<Streaming<proto::InputEvent>>,
    ) -> Result<Response<()>, Status> {
        let mut events = request.into_inner();
        let _open = OpenStream::new(&self.input_streams);
        let mut resets = self.input_resets.clone();
        resets.mark_unchanged();
        loop {
            tokio::select! {
                event = events.message() => match event? {
                    Some(event) => self.inputs.lock().expect("fake inputs lock").push(event),
                    None => return Ok(Response::new(())),
                },
                Ok(()) = resets.changed() => return Err(Status::aborted("input stream reset")),
            }
        }
    }

    async fn set_physical_model(
        &self,
        request: Request<proto::PhysicalModelValue>,
    ) -> Result<Response<()>, Status> {
        let data = request
            .into_inner()
            .value
            .map(|value| value.data)
            .unwrap_or_default();
        let rotation: [f32; 3] = data
            .try_into()
            .map_err(|_| Status::invalid_argument("rotation needs three values"))?;
        self.rotations
            .lock()
            .expect("fake rotations lock")
            .push(rotation);
        Ok(Response::new(()))
    }

    async fn set_vm_state(
        &self,
        request: Request<proto::VmRunState>,
    ) -> Result<Response<()>, Status> {
        *self.vm_state.lock().expect("fake vm state lock") = Some(request.into_inner().state);
        Ok(Response::new(()))
    }

    async fn stream_sensor(
        &self,
        _request: Request<proto::SensorValue>,
    ) -> Result<Response<Self::streamSensorStream>, Status> {
        Err(Status::unimplemented("stream_sensor"))
    }
    async fn get_sensor(
        &self,
        _request: Request<proto::SensorValue>,
    ) -> Result<Response<proto::SensorValue>, Status> {
        Err(Status::unimplemented("get_sensor"))
    }
    async fn set_sensor(
        &self,
        _request: Request<proto::SensorValue>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_sensor"))
    }
    async fn get_physical_model(
        &self,
        _request: Request<proto::PhysicalModelValue>,
    ) -> Result<Response<proto::PhysicalModelValue>, Status> {
        Err(Status::unimplemented("get_physical_model"))
    }
    async fn stream_physical_model(
        &self,
        _request: Request<proto::PhysicalModelValue>,
    ) -> Result<Response<Self::streamPhysicalModelStream>, Status> {
        Err(Status::unimplemented("stream_physical_model"))
    }
    async fn set_clipboard(
        &self,
        _request: Request<proto::ClipData>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_clipboard"))
    }
    async fn get_clipboard(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::ClipData>, Status> {
        Err(Status::unimplemented("get_clipboard"))
    }
    async fn stream_clipboard(
        &self,
        _request: Request<()>,
    ) -> Result<Response<Self::streamClipboardStream>, Status> {
        Err(Status::unimplemented("stream_clipboard"))
    }
    async fn set_battery(
        &self,
        _request: Request<proto::BatteryState>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_battery"))
    }
    async fn get_battery(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::BatteryState>, Status> {
        Err(Status::unimplemented("get_battery"))
    }
    async fn set_gps(&self, _request: Request<proto::GpsState>) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_gps"))
    }
    async fn get_gps(&self, _request: Request<()>) -> Result<Response<proto::GpsState>, Status> {
        Err(Status::unimplemented("get_gps"))
    }
    async fn send_fingerprint(
        &self,
        _request: Request<proto::Fingerprint>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("send_fingerprint"))
    }
    async fn send_key(
        &self,
        _request: Request<proto::KeyboardEvent>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("send_key"))
    }
    async fn send_touch(
        &self,
        _request: Request<proto::TouchEvent>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("send_touch"))
    }
    async fn send_mouse(
        &self,
        _request: Request<proto::MouseEvent>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("send_mouse"))
    }
    async fn inject_wheel(
        &self,
        _request: Request<tonic::Streaming<proto::WheelEvent>>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("inject_wheel"))
    }
    async fn send_phone(
        &self,
        _request: Request<proto::PhoneCall>,
    ) -> Result<Response<proto::PhoneResponse>, Status> {
        Err(Status::unimplemented("send_phone"))
    }
    async fn send_sms(
        &self,
        _request: Request<proto::SmsMessage>,
    ) -> Result<Response<proto::PhoneResponse>, Status> {
        Err(Status::unimplemented("send_sms"))
    }
    async fn set_phone_number(
        &self,
        _request: Request<proto::PhoneNumber>,
    ) -> Result<Response<proto::PhoneResponse>, Status> {
        Err(Status::unimplemented("set_phone_number"))
    }
    async fn stream_audio(
        &self,
        _request: Request<proto::AudioFormat>,
    ) -> Result<Response<Self::streamAudioStream>, Status> {
        Err(Status::unimplemented("stream_audio"))
    }
    async fn inject_audio(
        &self,
        _request: Request<tonic::Streaming<proto::AudioPacket>>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("inject_audio"))
    }
    async fn get_microphone_state(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::MicrophoneState>, Status> {
        Err(Status::unimplemented("get_microphone_state"))
    }
    async fn set_microphone_state(
        &self,
        _request: Request<proto::MicrophoneState>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_microphone_state"))
    }
    async fn get_logcat(
        &self,
        _request: Request<proto::LogMessage>,
    ) -> Result<Response<proto::LogMessage>, Status> {
        Err(Status::unimplemented("get_logcat"))
    }
    async fn stream_logcat(
        &self,
        _request: Request<proto::LogMessage>,
    ) -> Result<Response<Self::streamLogcatStream>, Status> {
        Err(Status::unimplemented("stream_logcat"))
    }
    async fn get_vm_state(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::VmRunState>, Status> {
        Err(Status::unimplemented("get_vm_state"))
    }
    async fn set_display_configurations(
        &self,
        _request: Request<proto::DisplayConfigurations>,
    ) -> Result<Response<proto::DisplayConfigurations>, Status> {
        Err(Status::unimplemented("set_display_configurations"))
    }
    async fn get_display_configurations(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::DisplayConfigurations>, Status> {
        Err(Status::unimplemented("get_display_configurations"))
    }
    async fn stream_notification(
        &self,
        _request: Request<()>,
    ) -> Result<Response<Self::streamNotificationStream>, Status> {
        Err(Status::unimplemented("stream_notification"))
    }
    async fn rotate_virtual_scene_camera(
        &self,
        _request: Request<proto::RotationRadian>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("rotate_virtual_scene_camera"))
    }
    async fn set_virtual_scene_camera_velocity(
        &self,
        _request: Request<proto::Velocity>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_virtual_scene_camera_velocity"))
    }
    async fn set_posture(&self, _request: Request<proto::Posture>) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_posture"))
    }
    async fn get_brightness(
        &self,
        _request: Request<proto::BrightnessValue>,
    ) -> Result<Response<proto::BrightnessValue>, Status> {
        Err(Status::unimplemented("get_brightness"))
    }
    async fn set_brightness(
        &self,
        _request: Request<proto::BrightnessValue>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_brightness"))
    }
    async fn get_display_mode(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::DisplayMode>, Status> {
        Err(Status::unimplemented("get_display_mode"))
    }
    async fn set_display_mode(
        &self,
        _request: Request<proto::DisplayMode>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_display_mode"))
    }
    async fn set_xr_options(
        &self,
        _request: Request<proto::XrOptions>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_xr_options"))
    }
    async fn get_xr_options(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::XrOptions>, Status> {
        Err(Status::unimplemented("get_xr_options"))
    }
    async fn set_environment(
        &self,
        _request: Request<proto::Environment>,
    ) -> Result<Response<()>, Status> {
        Err(Status::unimplemented("set_environment"))
    }
    async fn get_environment(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::Environment>, Status> {
        Err(Status::unimplemented("get_environment"))
    }
    async fn get_host_cameras(
        &self,
        _request: Request<()>,
    ) -> Result<Response<proto::CameraList>, Status> {
        Err(Status::unimplemented("get_host_cameras"))
    }
}

/// What the fake `emulator` runs for a launch (any invocation other than a query). Every stub
/// records its PID in the SDK's `launched` file.
#[cfg(unix)]
#[derive(Clone, Copy)]
pub(crate) enum LaunchStub {
    /// Writes the gRPC discovery file, then runs until killed.
    Runs { grpc_port: u16, token: &'static str },
    /// Like `Runs`, but ignores `SIGTERM`.
    IgnoresShutdown { grpc_port: u16, token: &'static str },
    /// Like `Runs`, but keeps a `sleep` child (its PID in the SDK's fork PID file) instead of
    /// exec'ing it, and publishes the discovery file under the child's PID, as an `emulator`
    /// launcher running qemu as a child does.
    ForksChild { grpc_port: u16, token: &'static str },
    /// Prints the message to stderr and exits with the code.
    ExitsWith(i32, &'static str),
}

/// Builder for a fake Android SDK tree of shell-script `emulator` and `adb` tools.
#[cfg(unix)]
pub(crate) struct FakeSdk {
    dir: tempfile::TempDir,
    avds: Vec<String>,
    accel_ok: bool,
    with_adb: bool,
    launch: Option<LaunchStub>,
    /// `getprop sys.boot_completed` answers `1` from this call on; `None` never.
    boot_completed_after: Option<u32>,
    /// Which of `discovery_dirs()` the launch stub publishes in.
    discovery_index: usize,
    /// Seconds `-list-avds` sleeps after counting its call.
    list_avds_delay_secs: u32,
}

#[cfg(unix)]
impl FakeSdk {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("fake sdk dir"),
            avds: Vec::new(),
            accel_ok: true,
            with_adb: true,
            launch: None,
            boot_completed_after: Some(1),
            discovery_index: 0,
            list_avds_delay_secs: 0,
        }
    }

    pub(crate) fn avds(mut self, names: &[&str]) -> Self {
        self.avds = names.iter().map(|name| (*name).to_owned()).collect();
        self
    }

    pub(crate) fn accel_ok(mut self, ok: bool) -> Self {
        self.accel_ok = ok;
        self
    }

    pub(crate) fn without_adb(mut self) -> Self {
        self.with_adb = false;
        self
    }

    /// What the emulator script runs for any non-query invocation; without one it fails.
    pub(crate) fn launch_stub(mut self, stub: LaunchStub) -> Self {
        self.launch = Some(stub);
        self
    }

    pub(crate) fn boot_completed_after(mut self, calls: u32) -> Self {
        self.boot_completed_after = Some(calls);
        self
    }

    /// Publishes the discovery file in `discovery_dirs()[index]`.
    pub(crate) fn discovery_in(mut self, index: usize) -> Self {
        self.discovery_index = index;
        self
    }

    pub(crate) fn list_avds_delay_secs(mut self, secs: u32) -> Self {
        self.list_avds_delay_secs = secs;
        self
    }

    pub(crate) fn never_boots(mut self) -> Self {
        self.boot_completed_after = None;
        self
    }

    pub(crate) fn write(self) -> Self {
        use std::{fs, os::unix::fs::PermissionsExt};
        let avds = self
            .avds
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(" ");
        let (code, text) = if self.accel_ok {
            (0, "HAXM/KVM is installed and usable.")
        } else {
            (1, "KVM is not available.")
        };
        let launch = self.dir.path().join("launch-stub");
        let emulator = format!(
            "#!/bin/sh\ncase \"$1\" in\n  -list-avds) echo >> \"{calls}\"; sleep {delay}; printf '%s\\n' {avds} ;;\n  -accel-check) echo \"accel:\"; echo \"{code}\"; echo \"{text}\"; echo \"accel\"; exit {code} ;;\n  *) exec \"{launch}\" \"$@\" ;;\nesac\n",
            launch = launch.display(),
            calls = self.list_avds_calls_file().display(),
            delay = self.list_avds_delay_secs
        );
        let write_script = |path: &Path, body: &str| {
            fs::create_dir_all(path.parent().expect("script parent")).expect("script dir");
            fs::write(path, body).expect("write script");
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
        };
        write_script(&self.dir.path().join("emulator/emulator"), &emulator);
        write_script(&launch, &self.launch_script());
        if self.with_adb {
            write_script(
                &self.dir.path().join("platform-tools/adb"),
                &self.adb_script(),
            );
        }
        self
    }

    fn launch_script(&self) -> String {
        let launched = self.dir.path().join("launched");
        let script = format!("#!/bin/sh\necho $$ >> \"{}\"\n", launched.display());
        let (grpc_port, token) = match self.launch {
            None => return script + "exit 1\n",
            Some(LaunchStub::ExitsWith(code, message)) => {
                return script + &format!("echo '{message}' >&2\nexit {code}\n");
            }
            Some(
                LaunchStub::Runs { grpc_port, token }
                | LaunchStub::IgnoresShutdown { grpc_port, token }
                | LaunchStub::ForksChild { grpc_port, token },
            ) => (grpc_port, token),
        };
        // Written whole, then renamed, so a reader never sees half a file.
        let dir = &self.discovery_dirs()[self.discovery_index];
        let publish = |pid: &str| {
            format!(
                "dir=\"{dir}\"\nmkdir -p \"$dir\"\nprintf 'grpc.port={grpc_port}\\ngrpc.token={token}\\n' > \"$dir/pid_{pid}.tmp\"\nmv \"$dir/pid_{pid}.tmp\" \"$dir/pid_{pid}.ini\"\n",
                dir = dir.display()
            )
        };
        match self.launch {
            Some(LaunchStub::IgnoresShutdown { .. }) => {
                script + &publish("$$") + "trap '' TERM\nexec sleep 600\n"
            }
            Some(LaunchStub::ForksChild { .. }) => {
                script
                    + &format!(
                        "sleep 600 &\nchild=$!\necho \"$child\" > \"{}\"\n",
                        self.fork_pid_file().display()
                    )
                    + &publish("$child")
                    + "wait\n"
            }
            _ => script + &publish("$$") + "exec sleep 600\n",
        }
    }

    fn adb_script(&self) -> String {
        let counter = self.dir.path().join("boot-polls");
        let after = self.boot_completed_after.unwrap_or(0);
        format!(
            "#!/bin/sh\n[ \"$1\" = \"-s\" ] && shift 2\ncase \"$1 $2 $3\" in\n  wait-for-device*) exit 0 ;;\n  'shell getprop sys.boot_completed')\n    polls=$(( $(cat \"{counter}\" 2>/dev/null || echo 0) + 1 ))\n    echo \"$polls\" > \"{counter}\"\n    if [ {after} -gt 0 ] && [ \"$polls\" -ge {after} ]; then echo 1; else echo; fi ;;\nesac\nexit 0\n",
            counter = counter.display()
        )
    }

    fn list_avds_calls_file(&self) -> PathBuf {
        self.dir.path().join("list-avds-calls")
    }

    /// How many times `emulator -list-avds` has run.
    pub(crate) fn list_avds_calls(&self) -> usize {
        std::fs::read_to_string(self.list_avds_calls_file())
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn fork_pid_file(&self) -> PathBuf {
        self.dir.path().join("fork-pid")
    }

    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn env(&self) -> crate::device::sdk::SdkEnv {
        crate::device::sdk::SdkEnv {
            vars: std::collections::HashMap::from([(
                "ANDROID_HOME".to_owned(),
                self.root().into(),
            )]),
            home: Some(self.root().join("home")),
            os: "linux",
        }
    }

    /// Two discovery directories inside the SDK's temporary tree, in search order.
    pub(crate) fn discovery_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.root().join("runtime/avd/running"),
            self.root().join("emulator-home/avd/running"),
        ]
    }

    /// A launcher for this SDK that searches `discovery_dirs()` and requests the stub's
    /// gRPC port, where the fake controller listens.
    pub(crate) fn launcher(&self) -> crate::device::emulator::EmulatorLauncher {
        let mut launcher = crate::device::emulator::EmulatorLauncher::new(
            crate::device::sdk::locate_sdk(&self.env()).expect("fake sdk is complete"),
            self.discovery_dirs(),
            crate::diagnostics::ProcessAttributionRegistry::new(),
            false,
        );
        launcher.grpc_port = match self.launch {
            Some(
                LaunchStub::Runs { grpc_port, .. }
                | LaunchStub::IgnoresShutdown { grpc_port, .. }
                | LaunchStub::ForksChild { grpc_port, .. },
            ) => Some(grpc_port),
            _ => None,
        };
        launcher
    }

    /// PIDs of every launch stub started so far.
    pub(crate) fn launched_pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.root().join("launched"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }

    /// The PID of the `sleep` a `ForksChild` stub started.
    pub(crate) async fn wait_forked_child_pid(&self) -> u32 {
        for _ in 0..200 {
            if let Some(pid) = std::fs::read_to_string(self.fork_pid_file())
                .ok()
                .and_then(|contents| contents.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the forked child never wrote its PID");
    }
}

/// Whether `pid` names a live (or unreaped) process.
#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Kills `pid` with `SIGKILL`, as a crash would end it.
#[cfg(unix)]
pub(crate) fn kill_process(pid: u32) {
    // SAFETY: plain signal delivery to a PID the test launched.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

/// A [`DeviceManager`](super::DeviceManager) on a fake SDK whose emulators talk to one
/// [`FakeController`]; device previews start enabled.
#[cfg(unix)]
pub(crate) struct ManagerFixture {
    pub(crate) manager: super::DeviceManager,
    pub(crate) enabled: watch::Sender<bool>,
    pub(crate) controller: FakeHandle,
    pub(crate) sdk: FakeSdk,
}

#[cfg(unix)]
impl ManagerFixture {
    /// Emulators for `avds` that boot and run until stopped.
    pub(crate) async fn ready(avds: &[&str]) -> Self {
        Self::with_sdk(|sdk| sdk.avds(avds)).await
    }

    /// `configure` adjusts a fake SDK whose launch stub runs against the fake controller.
    pub(crate) async fn with_sdk(configure: impl FnOnce(FakeSdk) -> FakeSdk) -> Self {
        let controller = FakeController::spawn().await;
        let sdk = configure(FakeSdk::new().launch_stub(LaunchStub::Runs {
            grpc_port: controller.port,
            token: TOKEN,
        }))
        .write();
        let (enabled, signal) = watch::channel(true);
        let manager = super::DeviceManager::start(super::DeviceManagerConfig {
            env: sdk.env(),
            discovery_dirs: sdk.discovery_dirs(),
            registry: crate::diagnostics::ProcessAttributionRegistry::new(),
            enabled: signal,
            grpc_port: Some(controller.port),
        });
        Self {
            manager,
            enabled,
            controller,
            sdk,
        }
    }

    /// Waits up to 20 s for `device_id` to reach `state`. A device that reached `Ready` exits
    /// when its VM is asked to shut down, so stopping it never waits for the kill timeout.
    pub(crate) async fn wait_state(
        &self,
        device_id: &str,
        state: super::catalog::DeviceState,
    ) -> super::catalog::DeviceSnapshot {
        let mut catalog = self.manager.catalog();
        let snapshot = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            catalog.wait_for(|catalog| {
                catalog
                    .devices
                    .iter()
                    .any(|device| device.device_id == device_id && device.state == state)
            }),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{device_id} never reached {state:?}: {:?}",
                self.manager.catalog().borrow().devices
            )
        })
        .expect("catalog open")
        .devices
        .iter()
        .find(|device| device.device_id == device_id)
        .cloned()
        .expect("device in catalog");
        if state == super::catalog::DeviceState::Ready {
            *self.controller.vm_state.lock().expect("fake vm state lock") = None;
            self.controller.exit_on_shutdown(self.pid_of(device_id));
        }
        snapshot
    }

    /// The emulator PID of a running device.
    pub(crate) fn pid_of(&self, device_id: &str) -> u32 {
        self.manager
            .pid(device_id)
            .expect("device has a running emulator")
    }
}

/// A [`ManagerFixture`] with one `Ready` device.
#[cfg(unix)]
pub(crate) struct DeviceFixture {
    fixture: ManagerFixture,
    pub(crate) id: String,
}

#[cfg(unix)]
impl std::ops::Deref for DeviceFixture {
    type Target = ManagerFixture;

    fn deref(&self) -> &ManagerFixture {
        &self.fixture
    }
}

#[cfg(unix)]
impl ManagerFixture {
    /// A manager whose only AVD, `Pixel`, is open and `Ready`.
    pub(crate) async fn ready_device() -> DeviceFixture {
        let fixture = Self::ready(&["Pixel"]).await;
        let id = fixture.manager.open("Pixel").await.expect("open Pixel");
        fixture
            .wait_state(&id, super::catalog::DeviceState::Ready)
            .await;
        DeviceFixture { fixture, id }
    }
}

#[cfg(unix)]
impl DeviceFixture {
    /// The device's catalog entry.
    pub(crate) fn snapshot(&self) -> super::catalog::DeviceSnapshot {
        self.manager
            .catalog()
            .borrow()
            .devices
            .iter()
            .find(|device| device.device_id == self.id)
            .cloned()
            .expect("device in catalog")
    }

    /// Waits up to ten seconds for the catalog to report `orientation`.
    pub(crate) async fn wait_orientation(&self, orientation: super::catalog::Orientation) {
        let mut catalog = self.manager.catalog();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            catalog.wait_for(|catalog| {
                catalog
                    .devices
                    .iter()
                    .any(|device| device.device_id == self.id && device.orientation == orientation)
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("never reported {orientation:?}: {:?}", self.snapshot()))
        .expect("catalog open");
    }
}

/// Polls `done` for up to ten seconds.
pub(crate) async fn wait_until(done: impl Fn() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("condition not reached within ten seconds");
}
