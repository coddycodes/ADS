// Generated code: the workspace denies all warnings (`[workspace.lints.rust] warnings = "deny"`).
#[allow(warnings, clippy::all, clippy::pedantic)]
pub(crate) mod proto {
    tonic::include_proto!("android.emulation.control");
}

use std::time::Duration;

use proto::{emulator_controller_client::EmulatorControllerClient, image_format::ImgFormat};
use tokio::sync::mpsc;
use tokio_util::task::TaskTracker;
use tonic::{
    metadata::MetadataValue,
    service::interceptor::InterceptedService,
    transport::{Channel, Endpoint},
};

use super::DeviceError;

#[derive(Clone)]
pub(crate) struct TokenInterceptor(Option<MetadataValue<tonic::metadata::Ascii>>);

impl tonic::service::Interceptor for TokenInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        if let Some(value) = &self.0 {
            request
                .metadata_mut()
                .insert("authorization", value.clone());
        }
        Ok(request)
    }
}

#[derive(Clone)]
pub(crate) struct EmulatorClient {
    inner: EmulatorControllerClient<InterceptedService<Channel, TokenInterceptor>>,
}

const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024; // a full-size RGB888 frame is ~7.8 MB
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INPUT_QUEUE_DEPTH: usize = 256;
/// Bound on each unary call; the streams run for the device's lifetime and have none.
const UNARY_TIMEOUT: Duration = Duration::from_secs(5);

impl EmulatorClient {
    pub(crate) async fn connect(port: u16, token: Option<String>) -> Result<Self, DeviceError> {
        let channel = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
            .map_err(|error| DeviceError::Grpc(error.to_string()))?
            .connect_timeout(CONNECT_TIMEOUT)
            .connect()
            .await
            .map_err(|error| DeviceError::Grpc(error.to_string()))?;
        let header = token
            .map(|token| format!("Bearer {token}").parse::<MetadataValue<_>>())
            .transpose()
            .map_err(|_| DeviceError::Protocol("emulator token is not ASCII".into()))?
            .map(|mut value| {
                value.set_sensitive(true);
                value
            });
        let inner = EmulatorControllerClient::with_interceptor(channel, TokenInterceptor(header))
            .max_decoding_message_size(MAX_MESSAGE_BYTES);
        Ok(Self { inner })
    }

    pub(crate) async fn status(&self) -> Result<proto::EmulatorStatus, DeviceError> {
        Ok(unary(self.inner.clone().get_status(())).await?.into_inner())
    }

    pub(crate) async fn screenshot(
        &self,
        format: ImgFormat,
        width: u32,
    ) -> Result<proto::Image, DeviceError> {
        let request = proto::ImageFormat {
            format: format as i32,
            width,
            ..Default::default()
        };
        Ok(unary(self.inner.clone().get_screenshot(request))
            .await?
            .into_inner())
    }

    pub(crate) async fn stream_screenshot(
        &self,
        width: u32,
    ) -> Result<tonic::Streaming<proto::Image>, DeviceError> {
        let request = proto::ImageFormat {
            format: ImgFormat::Rgb888 as i32,
            width,
            ..Default::default()
        };
        Ok(self
            .inner
            .clone()
            .stream_screenshot(request)
            .await
            .map_err(grpc_error)?
            .into_inner())
    }

    /// Opens a `streamInputEvent` call on a task in `tasks`; it ends when every sender is
    /// dropped or the emulator ends the stream, which closes the sender.
    pub(crate) fn input_sink(
        &self,
        tasks: &TaskTracker,
    ) -> Result<mpsc::Sender<proto::InputEvent>, DeviceError> {
        let (sender, receiver) = mpsc::channel(INPUT_QUEUE_DEPTH);
        let mut inner = self.inner.clone();
        tasks.spawn(async move {
            if let Err(status) = inner
                .stream_input_event(tokio_stream::wrappers::ReceiverStream::new(receiver))
                .await
            {
                tracing::warn!(error = %status, "emulator input stream ended");
            }
        });
        Ok(sender)
    }

    pub(crate) async fn set_rotation(&self, data: [f32; 3]) -> Result<(), DeviceError> {
        let request = proto::PhysicalModelValue {
            target: proto::physical_model_value::PhysicalType::Rotation as i32,
            value: Some(proto::ParameterValue {
                data: data.to_vec(),
            }),
            ..Default::default()
        };
        unary(self.inner.clone().set_physical_model(request)).await?;
        Ok(())
    }

    pub(crate) async fn shutdown_vm(&self) -> Result<(), DeviceError> {
        let request = proto::VmRunState {
            state: proto::vm_run_state::RunState::Shutdown as i32,
        };
        unary(self.inner.clone().set_vm_state(request)).await?;
        Ok(())
    }
}

/// Runs a unary call within [`UNARY_TIMEOUT`].
async fn unary<T>(call: impl Future<Output = Result<T, tonic::Status>>) -> Result<T, DeviceError> {
    tokio::time::timeout(UNARY_TIMEOUT, call)
        .await
        .map_err(|_| DeviceError::Grpc(format!("no answer within {UNARY_TIMEOUT:?}")))?
        .map_err(grpc_error)
}

fn grpc_error(status: tonic::Status) -> DeviceError {
    DeviceError::Grpc(format!("{:?}: {}", status.code(), status.message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_authenticates_and_reads_status_and_frames() {
        let fake = crate::device::testing::FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();
        assert!(client.status().await.unwrap().booted);
        let image = client.screenshot(ImgFormat::Rgb888, 360).await.unwrap();
        let format = image.format.unwrap();
        assert_eq!((format.width, format.height), (360, 800));
        assert_eq!(image.image.len(), 360 * 800 * 3);
    }

    #[tokio::test(start_paused = true)]
    async fn unary_calls_give_up_after_the_timeout() {
        let call = std::future::pending::<Result<(), tonic::Status>>();
        assert!(matches!(
            unary(call).await,
            Err(DeviceError::Grpc(message)) if message.contains("no answer")
        ));
    }

    #[tokio::test]
    async fn client_with_wrong_token_is_rejected() {
        let fake = crate::device::testing::FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("nope".into()))
            .await
            .unwrap();
        assert!(matches!(
            client.status().await,
            Err(DeviceError::Grpc(message)) if message.contains("Unauthenticated")
        ));
    }

    #[tokio::test]
    async fn client_streams_frames_and_forwards_input_rotation_and_shutdown() {
        let fake = crate::device::testing::FakeController::spawn().await;
        let client = EmulatorClient::connect(fake.port, Some("test-token".into()))
            .await
            .unwrap();

        let mut frames = client.stream_screenshot(360).await.unwrap();
        let first = frames.message().await.unwrap().unwrap();
        assert_eq!(first.image[0], 0x80);
        fake.frame_tx.send((1080, 2400, 0x20)).unwrap();
        let second = frames.message().await.unwrap().unwrap();
        assert_eq!((second.image[0], second.seq), (0x20, 1));

        let sink = client.input_sink(&TaskTracker::new()).unwrap();
        sink.send(proto::InputEvent::default()).await.unwrap();
        client.set_rotation([0.0, 0.0, 90.0]).await.unwrap();
        client.shutdown_vm().await.unwrap();
        assert_eq!(*fake.rotations.lock().unwrap(), vec![[0.0, 0.0, 90.0]]);
        assert_eq!(
            *fake.vm_state.lock().unwrap(),
            Some(proto::vm_run_state::RunState::Shutdown as i32)
        );
        drop(sink);
        for _ in 0..50 {
            if fake.inputs.lock().unwrap().len() == 1 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("input event never reached the fake controller");
    }
}
