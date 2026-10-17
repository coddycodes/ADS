//! The Android backend: SDK discovery, emulator lifecycle, its gRPC controller, input, and
//! the scrcpy H.264 source.
pub(crate) mod emulator;
pub(crate) mod grpc;
pub(crate) mod input;
pub(crate) mod orientation;
pub(crate) mod scrcpy;
pub(crate) mod sdk;
#[cfg(test)]
pub(crate) mod testing;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use futures_util::{StreamExt as _, future::BoxFuture, stream::BoxStream};
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use self::{
    emulator::{EmulatorLauncher, LaunchError, RunningEmulator},
    grpc::{
        EmulatorClient,
        proto::{self, image_format::ImgFormat},
    },
    input::{InputState, reported_orientation},
    orientation::OrientationTracker,
    sdk::{AndroidSdk, SdkEnv},
};
use super::{
    DeviceError, DeviceOpError,
    backend::{
        DeviceBackend, ErasedVideoSource, JpegStream, LaunchFailure, PlatformDiscovery, Png,
        RawJpeg, RunningDevice,
    },
    catalog::{Availability, AvailabilityKind, DeviceState, Orientation, Platform},
    frames::{JPEG_MIN_INTERVAL, encode_rgb888},
    input::DeviceInputEvent,
    media::VideoSource,
    video::VideoUnit,
};
use crate::diagnostics::ProcessAttributionRegistry;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct AndroidBackendConfig {
    pub env: SdkEnv,
    /// Where emulators publish their gRPC discovery files (`emulator::default_discovery_dirs`).
    pub discovery_dirs: Vec<PathBuf>,
    pub registry: ProcessAttributionRegistry,
    /// A fixed gRPC port for every launch; `None` picks a free one. Tests pin the fake
    /// controller's port.
    pub grpc_port: Option<u16>,
}

/// Android emulators from the SDK the server's environment names.
pub(crate) struct AndroidBackend {
    env: SdkEnv,
    discovery_dirs: Vec<PathBuf>,
    registry: ProcessAttributionRegistry,
    software_gpu: bool,
    grpc_port: Option<u16>,
    /// The SDK the last discovery found; launches use it.
    sdk: Mutex<Option<AndroidSdk>>,
}

impl AndroidBackend {
    pub(crate) fn new(config: AndroidBackendConfig) -> Self {
        let software_gpu =
            emulator::software_gpu_for(config.env.os, |key| config.env.vars.get(key).cloned());
        Self {
            env: config.env,
            discovery_dirs: config.discovery_dirs,
            registry: config.registry,
            software_gpu,
            grpc_port: config.grpc_port,
            sdk: Mutex::new(None),
        }
    }
}

impl DeviceBackend for AndroidBackend {
    fn platform(&self) -> Platform {
        Platform::Android
    }

    fn looking(&self) -> Availability {
        Availability {
            kind: AvailabilityKind::SdkMissing,
            message: "Looking for the Android SDK on the server.".to_owned(),
        }
    }

    fn discover(&self) -> BoxFuture<'_, PlatformDiscovery> {
        Box::pin(async move {
            let (availability, avds, sdk) = sdk::discover(&self.env).await;
            if availability.kind != AvailabilityKind::Ready {
                tracing::debug!(kind = ?availability.kind, "android sdk unavailable");
            }
            *lock(&self.sdk) = sdk;
            PlatformDiscovery {
                availability,
                images: avds,
            }
        })
    }

    fn launch<'a>(
        &'a self,
        image_id: &'a str,
        on_phase: Box<dyn Fn(DeviceState) + Send + Sync>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn RunningDevice>, LaunchFailure>> {
        Box::pin(async move {
            let Some(sdk) = lock(&self.sdk).clone() else {
                return Err(LaunchFailure::Spawn(
                    "the Android SDK is no longer available".to_owned(),
                ));
            };
            let adb = sdk.adb.clone();
            let mut launcher = EmulatorLauncher::new(
                sdk,
                self.discovery_dirs.clone(),
                self.registry.clone(),
                self.software_gpu,
            );
            launcher.grpc_port = self.grpc_port;
            match launcher.launch(image_id, on_phase, cancel).await {
                Ok(running) => Ok(
                    Box::new(RunningAndroid::new(running, adb, self.registry.clone()))
                        as Box<dyn RunningDevice>,
                ),
                Err(error) => Err(match error {
                    LaunchError::Exited(message) => LaunchFailure::Exited(message),
                    LaunchError::BootTimeout => LaunchFailure::BootTimeout,
                    LaunchError::Spawn(message) => LaunchFailure::Spawn(message),
                    LaunchError::Unresponsive(message) => LaunchFailure::Unresponsive(message),
                    LaunchError::Cancelled => LaunchFailure::Cancelled,
                }),
            }
        })
    }
}

/// A booted emulator, its input stream, and what its video source needs.
struct RunningAndroid {
    client: EmulatorClient,
    serial: String,
    native: (u32, u32),
    #[cfg(test)]
    pid: u32,
    /// Taken by whichever stop runs first.
    emulator: Mutex<Option<RunningEmulator>>,
    /// The SDK's `adb`, which runs the device's scrcpy-server.
    adb: PathBuf,
    registry: ProcessAttributionRegistry,
    /// The input stream, opened by the first `input` call.
    input: tokio::sync::Mutex<Option<InputState>>,
    /// The input stream's task; a stop waits for it.
    input_tasks: TaskTracker,
    /// The orientation the guest really shows, derived from the emulator's skin rotation
    /// and the running scrcpy session's display size.
    orientation: Arc<OrientationTracker>,
}

impl RunningAndroid {
    fn new(running: RunningEmulator, adb: PathBuf, registry: ProcessAttributionRegistry) -> Self {
        Self {
            client: running.client.clone(),
            serial: running.serial.clone(),
            native: (running.width, running.height),
            #[cfg(test)]
            pid: running.helper.pid,
            emulator: Mutex::new(Some(running)),
            adb,
            registry,
            input: tokio::sync::Mutex::new(None),
            input_tasks: TaskTracker::new(),
            orientation: OrientationTracker::new(),
        }
    }

    async fn stop_now(&self) {
        // The input stream ends once its sender is gone; a batch still holding it ends with
        // the emulator.
        if let Ok(mut input) = self.input.try_lock() {
            input.take();
        }
        let running = lock(&self.emulator).take();
        if let Some(running) = running {
            emulator::stop(running).await;
        }
        self.input_tasks.close();
        self.input_tasks.wait().await;
    }
}

impl RunningDevice for RunningAndroid {
    fn native_size(&self) -> (u32, u32) {
        self.native
    }

    fn can_rotate(&self) -> bool {
        true
    }

    fn jpeg_stream(&self, max_width: u32) -> BoxFuture<'_, Result<JpegStream, DeviceError>> {
        Box::pin(async move {
            let stream = self.client.stream_screenshot(max_width).await?;
            Ok(jpeg_frames(stream, self.orientation.clone()))
        })
    }

    fn png(&self, max_width: u32) -> BoxFuture<'_, Result<Png, DeviceError>> {
        Box::pin(async move {
            let image = self.client.screenshot(ImgFormat::Png, max_width).await?;
            let format = image.format.as_ref();
            let (width, height) = format.map_or((0, 0), |format| (format.width, format.height));
            let orientation = format
                .and_then(|format| format.rotation.as_ref())
                .and_then(|rotation| reported_orientation(rotation.rotation))
                .map_or_else(
                    || self.orientation.current(),
                    |skin| self.orientation.report_skin(skin),
                );
            Ok(Png {
                png: image.image,
                mime_type: "image/png",
                width,
                height,
                orientation,
            })
        })
    }

    fn video(&self) -> BoxFuture<'_, Result<Box<dyn ErasedVideoSource>, DeviceError>> {
        Box::pin(async move {
            let session = scrcpy::ScrcpySession::start(
                &self.adb,
                &self.serial,
                &self.registry,
                self.orientation.start_display(),
            )
            .await?;
            tracing::debug!(
                width = session.codec.width,
                height = session.codec.height,
                "device media stream started"
            );
            Ok(Box::new(session) as Box<dyn ErasedVideoSource>)
        })
    }

    /// The derived orientation as it changes, so a turn the guest makes on its own (an app
    /// that forces landscape, seen as a new scrcpy display size) reaches the catalog. `rotate`
    /// reports the same derived values, so the two settle on one orientation.
    fn orientation_changes(&self) -> Option<BoxStream<'static, Orientation>> {
        let mut changes = self.orientation.subscribe();
        changes.mark_changed();
        Some(
            futures_util::stream::unfold(changes, |mut changes| async move {
                changes.changed().await.ok()?;
                let orientation = *changes.borrow_and_update();
                Some((orientation, changes))
            })
            .boxed(),
        )
    }

    fn input(
        &self,
        events: Vec<DeviceInputEvent>,
        orientation: Orientation,
    ) -> BoxFuture<'_, Result<(), DeviceOpError>> {
        Box::pin(input::send_batch(
            &self.client,
            &self.input_tasks,
            &self.input,
            events,
            self.native,
            orientation,
        ))
    }

    fn rotate<'a>(
        &'a self,
        to: Orientation,
        report: &'a (dyn Fn(Orientation, u32, u32) + Send + Sync),
    ) -> BoxFuture<'a, Result<(), DeviceOpError>> {
        Box::pin(input::rotate(
            &self.client,
            self.native,
            &self.orientation,
            to,
            report,
        ))
    }

    fn exited(&self) -> Option<String> {
        let emulator = lock(&self.emulator);
        let status = emulator.as_ref()?.helper.exited()?;
        Some(format!("exited: {status}"))
    }

    fn report_unrequested_exit(&self, _reason: &str) {
        let emulator = lock(&self.emulator);
        if let Some(running) = emulator.as_ref()
            && let Some(status) = running.helper.exited()
        {
            running
                .helper
                .report_unrequested_exit(emulator::HELPER_LABEL, &status);
        }
    }

    fn stop(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.stop_now().await })
    }

    fn stop_shared(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.stop_now())
    }

    #[cfg(test)]
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    #[cfg(test)]
    fn input_stream_closed(&self) -> bool {
        self.input
            .try_lock()
            .is_ok_and(|state| state.as_ref().is_none_or(InputState::closed))
    }
}

/// The newest image of `stream` as a JPEG at most every [`JPEG_MIN_INTERVAL`]; images that
/// arrive in between replace the pending one unencoded. Each frame carries the orientation
/// derived from its rotation (a frame without one keeps the current orientation).
fn jpeg_frames(
    stream: tonic::Streaming<proto::Image>,
    orientation: Arc<OrientationTracker>,
) -> JpegStream {
    let frames = FrameStream {
        stream,
        pending: None,
        next: Instant::now(),
        warned_malformed: false,
        orientation,
        failed: false,
    };
    futures_util::stream::unfold(frames, |mut frames| async move {
        let next = frames.next().await?;
        Some((next, frames))
    })
    .boxed()
}

struct FrameStream {
    stream: tonic::Streaming<proto::Image>,
    pending: Option<proto::Image>,
    next: Instant,
    /// Whether this stream already warned about a frame it could not encode; the rest are
    /// logged at debug.
    warned_malformed: bool,
    orientation: Arc<OrientationTracker>,
    /// Set after the stream yielded its error; it ends next.
    failed: bool,
}

impl FrameStream {
    async fn next(&mut self) -> Option<Result<RawJpeg, DeviceError>> {
        if self.failed {
            return None;
        }
        loop {
            tokio::select! {
                message = self.stream.message() => match message {
                    Ok(Some(image)) => self.pending = Some(image),
                    Ok(None) => return None,
                    Err(status) => {
                        self.failed = true;
                        return Some(Err(DeviceError::Grpc(format!(
                            "{:?}: {}",
                            status.code(),
                            status.message()
                        ))));
                    }
                },
                () = tokio::time::sleep_until(self.next), if self.pending.is_some() => {
                    let image = self.pending.take().expect("checked by the branch condition");
                    let frame = self.encode(image).await;
                    self.next = Instant::now() + JPEG_MIN_INTERVAL;
                    if let Some(frame) = frame {
                        return Some(Ok(frame));
                    }
                }
            }
        }
    }

    /// Encodes `image` off the runtime; `None` (logged) when it cannot be encoded.
    async fn encode(&mut self, image: proto::Image) -> Option<RawJpeg> {
        let format = image.format.unwrap_or_default();
        let (width, height) = (format.width, format.height);
        let reported = format
            .rotation
            .and_then(|rotation| reported_orientation(rotation.rotation));
        let rgb = image.image;
        let encoded = tokio::task::spawn_blocking(move || encode_rgb888(&rgb, width, height)).await;
        let jpeg = match encoded {
            Ok(Ok(jpeg)) => jpeg,
            Ok(Err(error)) => {
                if self.warned_malformed {
                    tracing::debug!(%error, width, height, "device frame could not be encoded");
                } else {
                    self.warned_malformed = true;
                    tracing::warn!(%error, width, height, "device frame could not be encoded");
                }
                return None;
            }
            Err(error) => {
                tracing::warn!(%error, "device frame encoder failed");
                return None;
            }
        };
        let orientation = reported.map_or_else(
            || self.orientation.current(),
            |skin| self.orientation.report_skin(skin),
        );
        Some(RawJpeg {
            jpeg,
            width,
            height,
            orientation,
        })
    }
}

impl VideoSource for scrcpy::ScrcpySession {
    async fn next_unit(&mut self) -> Option<VideoUnit> {
        self.units.recv().await
    }

    async fn reset_video(&mut self) -> Result<(), DeviceError> {
        Self::reset_video(self).await
    }

    fn failed(&self) -> bool {
        matches!(self.ended(), Some(scrcpy::StreamEnd::Failed(_)))
    }

    async fn stop(self, device_ended: bool) {
        Self::stop(self, device_ended).await;
    }
}

/// The error for an emulator call that failed on a `Ready` device: the cause is logged, and
/// the caller is told what to do.
pub(crate) fn emulator_failure(operation: &'static str, error: &DeviceError) -> DeviceOpError {
    tracing::warn!(operation, %error, "device emulator call failed");
    let what = match operation {
        "rotate" => "did not rotate",
        _ => "did not accept the input",
    };
    DeviceOpError::Emulator(format!(
        "The emulator {what}. Try again; if it keeps failing, stop the device and start it again."
    ))
}
