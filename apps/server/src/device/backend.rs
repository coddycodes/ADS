//! What the platform-neutral device manager needs from a platform: discovery, launch, and a
//! running device's frames, video, input, rotation, exit, and stop.

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use super::{
    DeviceError, DeviceOpError,
    catalog::{Availability, DeviceState, Image, Orientation, Platform},
    input::DeviceInputEvent,
    video::VideoUnit,
};

/// What a backend currently offers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PlatformDiscovery {
    pub availability: Availability,
    pub images: Vec<Image>,
}

/// Why a launch did not reach `Ready`; mirrors the emulator's `LaunchError`.
#[derive(Debug)]
pub(crate) enum LaunchFailure {
    Exited(String),
    BootTimeout,
    Spawn(String),
    Unresponsive(String),
    Cancelled,
    /// The image exists but is in use outside BiBCode. `check_available` reports it before
    /// a slot exists (`DeviceBusyError`); a launch that finds it late fails its slot. No
    /// Android backend reports it.
    #[allow(dead_code)]
    Busy,
}

/// One encoded JPEG from a device, before the hub numbers it.
#[derive(Clone)]
pub(crate) struct RawJpeg {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
}

/// A screenshot for the MCP screenshot tool: a PNG, or a JPEG from a device that cannot
/// take PNG screenshots, as `mime_type` says.
pub(crate) struct Png {
    pub png: Vec<u8>,
    /// `image/png` or `image/jpeg`.
    pub mime_type: &'static str,
    pub width: u32,
    pub height: u32,
    #[allow(dead_code)] // Read by the iOS backend's screenshot and by tests.
    pub orientation: Orientation,
}

/// A running device's JPEG frames; it ends when the device's stream does.
pub(crate) type JpegStream = BoxStream<'static, Result<RawJpeg, DeviceError>>;

pub(crate) trait DeviceBackend: Send + Sync + 'static {
    /// The platform this backend runs; the manager configures at most one backend for each.
    fn platform(&self) -> Platform;
    /// What the catalog shows before the first discovery finishes.
    fn looking(&self) -> Availability;
    /// Fresh discovery; the manager caches it for 30 s.
    fn discover(&self) -> BoxFuture<'_, PlatformDiscovery>;
    /// Boots `image_id`, reporting `Booting` through `on_phase`. Stops what it started on
    /// error or cancel before returning.
    fn launch<'a>(
        &'a self,
        image_id: &'a str,
        on_phase: Box<dyn Fn(DeviceState) + Send + Sync>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn RunningDevice>, LaunchFailure>>;
    /// Refuses an image that cannot be opened now, before the manager creates a slot for it.
    /// Only `LaunchFailure::Busy` is reported to the caller as such.
    fn check_available<'a>(
        &'a self,
        image_id: &'a str,
    ) -> BoxFuture<'a, Result<(), LaunchFailure>> {
        let _ = image_id;
        Box::pin(async { Ok(()) })
    }
}

pub(crate) trait RunningDevice: Send + Sync + 'static {
    /// Native display size, in the device's natural orientation (`Portrait`).
    fn native_size(&self) -> (u32, u32);
    /// Whether the device turns; the catalog reports it once the device is `Ready`.
    fn can_rotate(&self) -> bool;
    /// The orientation the device shows now. The manager's first `Ready` catalog entry
    /// carries it, with the display size in it, so a device that boots turned (an Android
    /// Quick Boot snapshot saved turned) is never first shown upright.
    fn orientation(&self) -> Orientation {
        Orientation::Portrait
    }
    /// A stream of JPEGs at most `max_width` wide, at most one per
    /// [`frames::JPEG_MIN_INTERVAL`](super::frames::JPEG_MIN_INTERVAL); the frame producer
    /// reopens it on end.
    fn jpeg_stream(&self, max_width: u32) -> BoxFuture<'_, Result<JpegStream, DeviceError>>;
    fn png(&self, max_width: u32) -> BoxFuture<'_, Result<Png, DeviceError>>;
    /// Starts an H.264 source for the media hub.
    fn video(&self) -> BoxFuture<'_, Result<Box<dyn ErasedVideoSource>, DeviceError>>;
    /// Why this device has no video, so a media ticket is refused at once and the client
    /// shows still images; `None` when [`video`](Self::video) can start.
    fn video_unavailable(&self) -> Option<String> {
        None
    }
    /// Each orientation the device reports, starting with the current one, for a device
    /// that reports turns on its own; the manager follows it into the catalog while the
    /// device runs. `None` for devices whose frames and `rotate` report it.
    fn orientation_changes(&self) -> Option<BoxStream<'static, Orientation>> {
        None
    }
    /// Sends one validated batch in order. `orientation` is the catalog's current one.
    fn input(
        &self,
        events: Vec<DeviceInputEvent>,
        orientation: Orientation,
    ) -> BoxFuture<'_, Result<(), DeviceOpError>>;
    /// Turns toward `to`, calling `report` with each orientation and size the device
    /// reports, never with what was asked. A backend whose derived orientation reaches the
    /// catalog another way may leave `report` uncalled: Android's `rotate` does, because
    /// [`orientation_changes`](Self::orientation_changes) and its still-image frames carry
    /// the orientation, while iOS's reports each readback and also has
    /// `orientation_changes` on a live display. Returns the orientation the device ended
    /// in, which differs from `to` when the screen kept its orientation.
    fn rotate<'a>(
        &'a self,
        to: Orientation,
        report: &'a (dyn Fn(Orientation, u32, u32) + Send + Sync),
    ) -> BoxFuture<'a, Result<Orientation, DeviceOpError>>;
    /// `Some(reason)` once the device ended without being asked; polled every second.
    fn exited(&self) -> Option<String>;
    /// Reports the unrequested exit to the profiler (`report_exit`, `Watch::Failures`).
    fn report_unrequested_exit(&self, reason: &str);
    /// Stops the device and waits until it is gone. Must be safe to run detached.
    fn stop(self: Box<Self>) -> BoxFuture<'static, ()>;
    /// [`stop`](Self::stop) for a device someone still holds; the manager uses it only when
    /// an in-flight call keeps the device shared for too long.
    fn stop_shared(&self) -> BoxFuture<'_, ()>;
    /// The emulator's PID, for the unix manager tests that signal it.
    #[cfg(all(test, unix))]
    fn pid(&self) -> Option<u32> {
        None
    }
    /// Whether the device has no open input stream; only the unix manager tests ask.
    #[cfg(all(test, unix))]
    fn input_stream_closed(&self) -> bool {
        true
    }
}

/// Object-safe form of `media::VideoSource`, implemented for every `VideoSource`.
pub(crate) trait ErasedVideoSource: Send {
    fn next_unit(&mut self) -> BoxFuture<'_, Option<VideoUnit>>;
    fn reset_video(&mut self) -> BoxFuture<'_, Result<(), DeviceError>>;
    fn failed(&self) -> bool;
    fn stop(self: Box<Self>, device_ended: bool) -> BoxFuture<'static, ()>;
}
