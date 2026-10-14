//! In-process fake of the emulator gRPC controller for hermetic tests.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
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
    pub(crate) frame_tx: watch::Sender<(u32, u32, u8)>,
    pub(crate) vm_state: Arc<Mutex<Option<i32>>>,
}

pub(crate) struct FakeController {
    inputs: Arc<Mutex<Vec<proto::InputEvent>>>,
    rotations: Arc<Mutex<Vec<[f32; 3]>>>,
    frames: watch::Receiver<(u32, u32, u8)>,
    vm_state: Arc<Mutex<Option<i32>>>,
}

impl FakeController {
    pub(crate) async fn spawn() -> FakeHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake emulator");
        let port = listener.local_addr().expect("fake emulator address").port();
        let (frame_tx, frames) = watch::channel(INITIAL_FRAME);
        let handle = FakeHandle {
            port,
            inputs: Arc::default(),
            rotations: Arc::default(),
            frame_tx,
            vm_state: Arc::default(),
        };
        let service = EmulatorControllerServer::with_interceptor(
            Self {
                inputs: handle.inputs.clone(),
                rotations: handle.rotations.clone(),
                frames,
                vm_state: handle.vm_state.clone(),
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

fn frame_image(frame: (u32, u32, u8), request: &proto::ImageFormat, seq: u32) -> proto::Image {
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
    let rotation = if width > height {
        proto::rotation::SkinRotation::Landscape
    } else {
        proto::rotation::SkinRotation::Portrait
    };
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
        Ok(Response::new(frame_image(frame, request.get_ref(), 0)))
    }

    async fn stream_screenshot(
        &self,
        request: Request<proto::ImageFormat>,
    ) -> Result<Response<Self::streamScreenshotStream>, Status> {
        let format = request.into_inner();
        let mut seq = 0;
        let stream = WatchStream::new(self.frames.clone()).map(move |frame| {
            let image = frame_image(frame, &format, seq);
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
        while let Some(event) = events.message().await? {
            self.inputs.lock().expect("fake inputs lock").push(event);
        }
        Ok(Response::new(()))
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
