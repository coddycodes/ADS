//! The iOS backend (macOS only): simulator discovery and lifecycle through `xcrun simctl`,
//! and the screen through CoreSimulator loaded in-process (`coresim`).
//!
//! When the private-API guard finds every name it needs, a booted simulator runs live: its
//! frames come from the framebuffer as it changes, and its orientation reaches the catalog
//! as the simulator reports it. Otherwise it runs view-only on `simctl` screenshots, and
//! input and video are refused with the reason.
pub(crate) mod coresim;
pub(crate) mod guard;
pub(crate) mod pixels;
pub(crate) mod simctl;
#[cfg(test)]
pub(crate) mod testing;

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures_util::{
    StreamExt as _,
    future::BoxFuture,
    stream::{self, BoxStream},
};
use tokio_util::sync::CancellationToken;

use self::{
    coresim::{CoreSim, Screen},
    guard::{GuardCache, GuardResult, XcodeCache, availability_for},
    simctl::{SIMULATOR_LABEL, SimDevice, SimState, SimctlError, SimctlRunner, SimulatorProcess},
};
use super::{
    DeviceError, DeviceOpError,
    backend::{
        DeviceBackend, ErasedVideoSource, JpegStream, LaunchFailure, PlatformDiscovery, Png,
        RawJpeg, RunningDevice,
    },
    catalog::{Availability, AvailabilityKind, DeviceState, Image, Orientation, Platform},
    frames::{JPEG_MIN_INTERVAL, encode_rgb888},
    input::DeviceInputEvent,
};
use crate::diagnostics::ProcessAttributionRegistry;

/// How long `simctl boot` and `bootstatus` may take together.
const DEFAULT_BOOT_TIMEOUT: Duration = Duration::from_secs(180);
/// How often a booted simulator's state is checked for a shutdown nobody here asked for.
const STATE_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often the view-only screen is captured; one `simctl` screenshot takes about this long.
const SCREENSHOT_INTERVAL: Duration = Duration::from_millis(500);
/// Screenshots tried after boot before the simulator counts as unresponsive.
const FIRST_SCREENSHOT_ATTEMPTS: u32 = 5;
/// A live screen that has not changed for this long sends its last frame again, so a new
/// subscriber gets a picture.
const LIVE_REPEAT_INTERVAL: Duration = Duration::from_secs(1);
/// Why a simulator whose screen BiBCode could not attach runs view-only.
const ATTACH_FAILED_REASON: &str = "BiBCode could not attach to this simulator's screen.";
/// Live simulators refuse input until live input exists (Task 8).
const LIVE_INPUT_PENDING: &str =
    "Input to iOS simulators is not available in this version of BiBCode yet.";
/// Live simulators have no video until the encoder exists (Task 7); the client shows the
/// live JPEG frames instead.
const LIVE_VIDEO_PENDING: &str = "Video from iOS simulators is not available in this version of BiBCode yet, so the screen updates as still images.";
pub(crate) const EXTERNAL_SHUTDOWN: &str = "Shutdown outside BiBCode";
pub(crate) const EXTERNAL_DELETE: &str = "Deleted outside BiBCode";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct IosBackendConfig {
    pub runner: SimctlRunner,
    pub boot_timeout: Duration,
    pub guard: GuardCache,
}

impl IosBackendConfig {
    /// The Mac's own `xcrun` and `xcode-select`. Test builds (`hermetic-test-guard`) name
    /// tools that do not exist, so a runtime with device previews on finds no Xcode and never
    /// lists or boots the host's simulators.
    pub(crate) fn from_process(registry: ProcessAttributionRegistry) -> Self {
        let bin = if cfg!(feature = "hermetic-test-guard") {
            PathBuf::from("/nonexistent/bibcode-hermetic")
        } else {
            PathBuf::from("/usr/bin")
        };
        Self::with_tools(bin, registry)
    }

    /// The tools in `bin` and this Mac's simulator frameworks.
    fn with_tools(bin: PathBuf, registry: ProcessAttributionRegistry) -> Self {
        Self {
            runner: SimctlRunner {
                xcrun: bin.join("xcrun"),
                xcode_select: bin.join("xcode-select"),
                registry,
            },
            boot_timeout: DEFAULT_BOOT_TIMEOUT,
            guard: GuardCache::new(),
        }
    }
}

/// How the next simulator opens, from the last discovery.
#[derive(Clone)]
enum LaunchMode {
    Live(Arc<CoreSim>),
    ViewOnly { reason: String },
}

/// The Mac's iOS simulators.
pub(crate) struct IosBackend {
    runner: Arc<SimctlRunner>,
    boot_timeout: Duration,
    xcode: XcodeCache,
    guard: Arc<GuardCache>,
    mode: Mutex<LaunchMode>,
    /// UDIDs BiBCode booted and has not shut down; only those are never `in_use_elsewhere`.
    booted: Arc<Mutex<HashSet<String>>>,
}

impl IosBackend {
    pub(crate) fn new(config: IosBackendConfig) -> Self {
        Self {
            runner: Arc::new(config.runner),
            boot_timeout: config.boot_timeout,
            xcode: XcodeCache::default(),
            guard: Arc::new(config.guard),
            mode: Mutex::new(LaunchMode::ViewOnly {
                reason: ATTACH_FAILED_REASON.to_owned(),
            }),
            booted: Arc::default(),
        }
    }

    fn image(&self, device: &SimDevice) -> Image {
        Image {
            id: device.udid.clone(),
            display_name: simctl::display_name(device),
            in_use_elsewhere: device.state != SimState::Shutdown
                && !lock(&self.booted).contains(&device.udid),
        }
    }

    /// Shuts down a simulator this launch booted, logging a failure.
    async fn shut_down_own(&self, udid: &str) {
        if let Err(error) = self.runner.shutdown(udid).await {
            tracing::warn!(%error, "could not shut down the simulator BiBCode booted");
        }
        lock(&self.booted).remove(udid);
    }

    /// The private-API guard's result for `status`; it loads frameworks, so it runs on a
    /// blocking thread.
    async fn guard(&self, status: &guard::XcodeStatus) -> GuardResult {
        let cache = self.guard.clone();
        let status = status.clone();
        tokio::task::spawn_blocking(move || cache.get(&status))
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "the simulator framework guard did not finish");
                GuardResult::ViewOnly {
                    kind: AvailabilityKind::XcodeUnsupported,
                    missing: "guard".to_owned(),
                }
            })
    }

    async fn launch_inner(
        &self,
        udid: &str,
        on_phase: Box<dyn Fn(DeviceState) + Send + Sync>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn RunningDevice>, LaunchFailure> {
        let devices = self
            .runner
            .list()
            .await
            .map_err(|error| LaunchFailure::Spawn(error.reason()))?;
        let Some(device) = devices.into_iter().find(|device| device.udid == udid) else {
            return Err(LaunchFailure::Spawn(
                "the simulator no longer exists".to_owned(),
            ));
        };
        if device.state != SimState::Shutdown {
            return Err(LaunchFailure::Busy);
        }
        let mode = lock(&self.mode).clone();
        lock(&self.booted).insert(udid.to_owned());
        on_phase(DeviceState::Booting);
        let booted = tokio::select! {
            () = cancel.cancelled() => None,
            booted = self.runner.boot(udid, self.boot_timeout) => Some(booted),
        };
        match booted {
            Some(Ok(())) => {}
            // `simctl boot` failed, so it may be someone else's: leave it alone.
            Some(Err(error @ SimctlError::Failed { .. })) if error_is_boot(&error) => {
                lock(&self.booted).remove(udid);
                return Err(LaunchFailure::Spawn(error.reason()));
            }
            Some(Err(error)) => {
                self.shut_down_own(udid).await;
                return Err(match error {
                    SimctlError::Timeout { .. } => LaunchFailure::BootTimeout,
                    other => LaunchFailure::Exited(other.reason()),
                });
            }
            None => {
                self.shut_down_own(udid).await;
                return Err(LaunchFailure::Cancelled);
            }
        }
        let display = tokio::select! {
            () = cancel.cancelled() => Err(LaunchFailure::Cancelled),
            display = self.attach_display(udid, mode) => display,
        };
        let (native, display) = match display {
            Ok(display) => display,
            Err(error) => {
                self.shut_down_own(udid).await;
                return Err(error);
            }
        };
        let process = self.runner.register_simulator(udid).await;
        Ok(Box::new(RunningSimulator::start(
            udid.to_owned(),
            native,
            display,
            self.runner.clone(),
            self.booted.clone(),
            process,
        )))
    }

    /// The booted simulator's screen: live through CoreSimulator when the guard allows it
    /// and the screen attaches, otherwise `simctl` screenshots. Returns its portrait size.
    async fn attach_display(
        &self,
        udid: &str,
        mode: LaunchMode,
    ) -> Result<((u32, u32), Display), LaunchFailure> {
        let reason = match mode {
            LaunchMode::Live(sim) => {
                let owned = udid.to_owned();
                let attached = tokio::task::spawn_blocking(move || Screen::attach(&sim, &owned))
                    .await
                    .map_err(|error| DeviceError::Protocol(error.to_string()))
                    .and_then(|attached| attached);
                match attached {
                    Ok(screen) => return Ok((screen.size(), Display::Live(Arc::new(screen)))),
                    Err(error) => {
                        tracing::warn!(%error, "could not attach to the simulator's screen; it runs view-only");
                        ATTACH_FAILED_REASON.to_owned()
                    }
                }
            }
            LaunchMode::ViewOnly { reason } => reason,
        };
        let native = self.first_screenshot_size(udid).await?;
        Ok((native, Display::ViewOnly { reason }))
    }

    /// The native screen size, from the first screenshot that succeeds.
    async fn first_screenshot_size(&self, udid: &str) -> Result<(u32, u32), LaunchFailure> {
        let mut last = String::new();
        for attempt in 0..FIRST_SCREENSHOT_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(SCREENSHOT_INTERVAL).await;
            }
            match self.runner.screenshot_jpeg(udid).await {
                Ok(jpeg) => match simctl::jpeg_size(&jpeg) {
                    Some(size) => return Ok(size),
                    None => "the screenshot is not a JPEG".clone_into(&mut last),
                },
                Err(error) => last = error.reason(),
            }
        }
        Err(LaunchFailure::Unresponsive(last))
    }
}

fn error_is_boot(error: &SimctlError) -> bool {
    matches!(error, SimctlError::Failed { command, .. } if command == "simctl boot")
}

impl DeviceBackend for IosBackend {
    fn platform(&self) -> Platform {
        Platform::Ios
    }

    fn looking(&self) -> Availability {
        Availability {
            kind: AvailabilityKind::XcodeMissing,
            message: "Looking for Xcode on the server.".to_owned(),
        }
    }

    fn discover(&self) -> BoxFuture<'_, PlatformDiscovery> {
        Box::pin(async move {
            let status = self.xcode.status(&self.runner).await;
            let ready = matches!(status, guard::XcodeStatus::Ready { .. });
            let devices = if ready {
                match self.runner.list().await {
                    Ok(devices) => devices,
                    Err(error) => {
                        tracing::warn!(%error, "could not list iOS simulators");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let guard = if ready {
                Some(self.guard(&status).await)
            } else {
                None
            };
            let availability = availability_for(&status, &devices, guard.as_ref());
            if availability.kind != AvailabilityKind::Ready {
                tracing::debug!(kind = ?availability.kind, "ios simulators are unavailable or view-only");
            }
            *lock(&self.mode) = match guard {
                Some(GuardResult::Live(sim)) => LaunchMode::Live(sim),
                _ => LaunchMode::ViewOnly {
                    reason: availability.message.clone(),
                },
            };
            PlatformDiscovery {
                availability,
                images: devices.iter().map(|device| self.image(device)).collect(),
            }
        })
    }

    fn launch<'a>(
        &'a self,
        image_id: &'a str,
        on_phase: Box<dyn Fn(DeviceState) + Send + Sync>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn RunningDevice>, LaunchFailure>> {
        Box::pin(self.launch_inner(image_id, on_phase, cancel))
    }

    /// Refuses a simulator that is not `Shutdown`: it runs outside BiBCode.
    fn check_available<'a>(
        &'a self,
        image_id: &'a str,
    ) -> BoxFuture<'a, Result<(), LaunchFailure>> {
        Box::pin(async move {
            let devices = self
                .runner
                .list()
                .await
                .map_err(|error| LaunchFailure::Spawn(error.reason()))?;
            match devices.iter().find(|device| device.udid == image_id) {
                Some(device) if device.state != SimState::Shutdown => Err(LaunchFailure::Busy),
                _ => Ok(()),
            }
        })
    }
}

/// How a running simulator's screen is read.
enum Display {
    /// Frames from the framebuffer through CoreSimulator.
    Live(Arc<Screen>),
    /// `simctl` screenshots; input and video are refused for `reason`.
    ViewOnly { reason: String },
}

/// A simulator BiBCode booted.
struct RunningSimulator {
    shared: Arc<Shared>,
}

struct Shared {
    udid: String,
    native: (u32, u32),
    display: Display,
    runner: Arc<SimctlRunner>,
    booted: Arc<Mutex<HashSet<String>>>,
    /// Why the simulator ended without BiBCode asking; set by the state poll.
    exit: Mutex<Option<String>>,
    /// Cancels the state poll.
    poll: CancellationToken,
    poll_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    process: Mutex<Option<SimulatorProcess>>,
    /// Set by whichever stop runs first.
    stopping: AtomicBool,
}

impl RunningSimulator {
    fn start(
        udid: String,
        native: (u32, u32),
        display: Display,
        runner: Arc<SimctlRunner>,
        booted: Arc<Mutex<HashSet<String>>>,
        process: Option<SimulatorProcess>,
    ) -> Self {
        let shared = Arc::new(Shared {
            udid,
            native,
            display,
            runner,
            booted,
            exit: Mutex::new(None),
            poll: CancellationToken::new(),
            poll_task: Mutex::new(None),
            process: Mutex::new(process),
            stopping: AtomicBool::new(false),
        });
        let task = tokio::spawn(watch_state(Arc::downgrade(&shared), shared.poll.clone()));
        *lock(&shared.poll_task) = Some(task);
        Self { shared }
    }
}

/// Polls `simctl list` until cancelled; a simulator that leaves `Booted` is recorded as an
/// exit nobody here asked for.
async fn watch_state(shared: std::sync::Weak<Shared>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(STATE_POLL_INTERVAL) => {}
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let listed = tokio::select! {
            () = cancel.cancelled() => return,
            listed = shared.runner.list() => listed,
        };
        let devices = match listed {
            Ok(devices) => devices,
            Err(error) => {
                tracing::debug!(%error, "could not poll the simulator's state");
                continue;
            }
        };
        let reason = match devices.iter().find(|device| device.udid == shared.udid) {
            Some(device) if device.state == SimState::Booted => continue,
            Some(_) => EXTERNAL_SHUTDOWN,
            None => EXTERNAL_DELETE,
        };
        // A stop that started meanwhile asked for this.
        if cancel.is_cancelled() {
            return;
        }
        *lock(&shared.exit) = Some(reason.to_owned());
        return;
    }
}

impl Shared {
    async fn stop_now(&self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        // The poll ends first, so this shutdown is never taken for an external one.
        self.poll.cancel();
        let task = lock(&self.poll_task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
        // The screen's callbacks go before the simulator does.
        if let Display::Live(screen) = &self.display {
            let screen = screen.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || screen.detach()).await {
                tracing::warn!(%error, "could not detach from the simulator's screen");
            }
        }
        if let Some(mut process) = lock(&self.process).take() {
            process.unregister();
        }
        let ended_outside = lock(&self.exit).is_some();
        // A simulator shut down outside BiBCode may have been booted again by someone else.
        if !ended_outside && let Err(error) = self.runner.shutdown(&self.udid).await {
            tracing::warn!(%error, "could not shut down the simulator");
        }
        lock(&self.booted).remove(&self.udid);
    }

    async fn screenshot(&self) -> Result<RawJpeg, DeviceError> {
        let jpeg = self
            .runner
            .screenshot_jpeg(&self.udid)
            .await
            .map_err(|error| DeviceError::Io(error.to_string()))?;
        let (width, height) = simctl::jpeg_size(&jpeg)
            .ok_or_else(|| DeviceError::Protocol("the screenshot is not a JPEG".into()))?;
        Ok(RawJpeg {
            jpeg,
            width,
            height,
            orientation: Orientation::Portrait,
        })
    }
}

/// The live screen's current frame as a JPEG at most `max_width` wide. Reads about 12 MB
/// and encodes, so it runs on a blocking thread.
async fn live_jpeg(screen: Arc<Screen>, max_width: u32) -> Result<RawJpeg, DeviceError> {
    tokio::task::spawn_blocking(move || {
        let frame = screen.read_rgb(max_width)?;
        Ok(RawJpeg {
            jpeg: encode_rgb888(&frame.rgb, frame.width, frame.height)?,
            width: frame.width,
            height: frame.height,
            orientation: frame.orientation.orientation(),
        })
    })
    .await
    .map_err(|error| DeviceError::Protocol(error.to_string()))?
}

/// The live screen's current frame as a PNG at most `max_width` wide.
async fn live_png(screen: Arc<Screen>, max_width: u32) -> Result<Png, DeviceError> {
    tokio::task::spawn_blocking(move || {
        let frame = screen.read_rgb(max_width)?;
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let encode_error = |error: png::EncodingError| DeviceError::Protocol(error.to_string());
        let mut writer = encoder.write_header().map_err(encode_error)?;
        writer.write_image_data(&frame.rgb).map_err(encode_error)?;
        writer.finish().map_err(encode_error)?;
        Ok(Png {
            png,
            mime_type: "image/png",
            width: frame.width,
            height: frame.height,
            orientation: frame.orientation.orientation(),
        })
    })
    .await
    .map_err(|error| DeviceError::Protocol(error.to_string()))?
}

/// Frames of a live screen: one per change, at most one per [`JPEG_MIN_INTERVAL`], and the
/// last one again after [`LIVE_REPEAT_INTERVAL`] without a change. It ends after an error;
/// the frame producer reopens it.
fn live_frames(screen: Arc<Screen>, max_width: u32) -> JpegStream {
    struct State {
        screen: Arc<Screen>,
        damage: tokio::sync::watch::Receiver<u64>,
        last: Option<RawJpeg>,
        sent: Option<tokio::time::Instant>,
        failed: bool,
    }
    let mut damage = screen.damage();
    // The first frame goes out at once.
    damage.mark_changed();
    let state = State {
        screen,
        damage,
        last: None,
        sent: None,
        failed: false,
    };
    stream::unfold(state, move |mut state| async move {
        if state.failed {
            return None;
        }
        if let Some(sent) = state.sent {
            tokio::time::sleep_until(sent + JPEG_MIN_INTERVAL).await;
        }
        let changed = tokio::time::timeout(LIVE_REPEAT_INTERVAL, state.damage.changed()).await;
        state.damage.borrow_and_update();
        state.sent = Some(tokio::time::Instant::now());
        if changed.is_err()
            && let Some(last) = &state.last
        {
            return Some((Ok(last.clone()), state));
        }
        let frame = live_jpeg(state.screen.clone(), max_width).await;
        match &frame {
            Ok(frame) => state.last = Some(frame.clone()),
            Err(_) => state.failed = true,
        }
        Some((frame, state))
    })
    .boxed()
}

impl RunningDevice for RunningSimulator {
    fn native_size(&self) -> (u32, u32) {
        self.shared.native
    }

    fn can_rotate(&self) -> bool {
        false
    }

    /// Live: framebuffer frames scaled to `max_width`. View-only: full-size `simctl`
    /// screenshots every [`SCREENSHOT_INTERVAL`]; `max_width` is not applied, since
    /// decoding and re-encoding each one would cost more than sending it.
    fn jpeg_stream(&self, max_width: u32) -> BoxFuture<'_, Result<JpegStream, DeviceError>> {
        let shared = self.shared.clone();
        Box::pin(async move {
            if let Display::Live(screen) = &shared.display {
                return Ok(live_frames(screen.clone(), max_width));
            }
            let frames = stream::unfold(
                (shared, None::<tokio::time::Instant>, false),
                |(shared, last, failed)| async move {
                    if failed {
                        return None;
                    }
                    if let Some(last) = last {
                        tokio::time::sleep_until(last + SCREENSHOT_INTERVAL).await;
                    }
                    let started = tokio::time::Instant::now();
                    let frame = shared.screenshot().await;
                    let failed = frame.is_err();
                    Some((frame, (shared, Some(started), failed)))
                },
            );
            Ok(frames.boxed())
        })
    }

    /// Live: a PNG of the framebuffer. View-only: a `simctl` JPEG, as `mime_type` says.
    fn png(&self, max_width: u32) -> BoxFuture<'_, Result<Png, DeviceError>> {
        Box::pin(async move {
            if let Display::Live(screen) = &self.shared.display {
                return live_png(screen.clone(), max_width).await;
            }
            let frame = self.shared.screenshot().await?;
            Ok(Png {
                png: frame.jpeg,
                mime_type: "image/jpeg",
                width: frame.width,
                height: frame.height,
                orientation: frame.orientation,
            })
        })
    }

    fn video(&self) -> BoxFuture<'_, Result<Box<dyn ErasedVideoSource>, DeviceError>> {
        // Media tickets are refused first (`video_unavailable`), so this is not reached.
        Box::pin(async {
            Err(DeviceError::Protocol(
                "iOS simulators have no video yet".into(),
            ))
        })
    }

    fn video_unavailable(&self) -> Option<String> {
        Some(match &self.shared.display {
            Display::Live(_) => LIVE_VIDEO_PENDING.to_owned(),
            Display::ViewOnly { reason } => {
                format!("Video is off: {}.", reason.trim_end_matches('.'))
            }
        })
    }

    fn orientation_changes(&self) -> Option<BoxStream<'static, Orientation>> {
        let Display::Live(screen) = &self.shared.display else {
            return None;
        };
        let mut changes = screen.orientation();
        changes.mark_changed();
        Some(
            stream::unfold(changes, |mut changes| async move {
                changes.changed().await.ok()?;
                let orientation = changes.borrow_and_update().orientation();
                Some((orientation, changes))
            })
            .boxed(),
        )
    }

    fn input(
        &self,
        _events: Vec<DeviceInputEvent>,
        _orientation: Orientation,
    ) -> BoxFuture<'_, Result<(), DeviceOpError>> {
        let message = match &self.shared.display {
            Display::Live(_) => LIVE_INPUT_PENDING.to_owned(),
            Display::ViewOnly { reason } => {
                format!("Touch input is off: {}.", reason.trim_end_matches('.'))
            }
        };
        Box::pin(async move { Err(DeviceOpError::InvalidInput(message)) })
    }

    fn rotate<'a>(
        &'a self,
        _to: Orientation,
        _report: &'a (dyn Fn(Orientation, u32, u32) + Send + Sync),
    ) -> BoxFuture<'a, Result<(), DeviceOpError>> {
        Box::pin(async {
            Err(DeviceOpError::InvalidInput(
                "This simulator cannot rotate from BiBCode.".into(),
            ))
        })
    }

    fn exited(&self) -> Option<String> {
        lock(&self.shared.exit).clone()
    }

    fn report_unrequested_exit(&self, reason: &str) {
        let (pid, uptime) = lock(&self.shared.process)
            .as_ref()
            .map_or((0, Duration::ZERO), |process| {
                (process.pid, process.started.elapsed())
            });
        tracing::warn!(reason, pid, "ios simulator ended outside BiBCode");
        report_simulator_exit(pid, uptime);
    }

    fn stop(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.shared.stop_now().await })
    }

    fn stop_shared(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.shared.stop_now())
    }
}

/// A `subprocess.crash` for a simulator that ended without BiBCode asking. It has no exit
/// status of its own, so it is reported as exit code 1.
fn report_simulator_exit(pid: u32, uptime: Duration) {
    use crate::diagnostics::profiler::exits::{self, Exited, Watch};
    use std::os::unix::process::ExitStatusExt as _;
    if !exits::watching() {
        return;
    }
    exits::report_exit(
        Watch::Failures,
        &Exited {
            kind: "command",
            label: SIMULATOR_LABEL,
            exe: "launchd_sim",
            pid,
            uptime,
        },
        &std::process::ExitStatus::from_raw(1 << 8),
        false,
    );
}

/// A device manager over this Mac's own simulators (`/usr/bin/xcrun` and the real
/// frameworks), with device previews on, whatever features the build has. Only for the
/// opt-in real-simulator tests (`tests/ios_simulator.rs`).
#[doc(hidden)]
pub fn host_manager_for_tests() -> (super::DeviceManager, tokio::sync::watch::Sender<bool>) {
    let (enabled, signal) = tokio::sync::watch::channel(true);
    let backend = IosBackend::new(IosBackendConfig::with_tools(
        PathBuf::from("/usr/bin"),
        ProcessAttributionRegistry::new(),
    ));
    let manager = super::DeviceManager::start(super::DeviceManagerConfig {
        backends: vec![Arc::new(backend)],
        enabled: signal,
    });
    (manager, enabled)
}

#[cfg(test)]
mod tests {
    use super::testing::{FakeXcrun, ios_manager};
    use crate::device::{
        DeviceOpError,
        catalog::{DeviceState, Platform},
        fake_backend::wait_state,
        input::{DeviceInputEvent, KeyAction},
    };

    #[tokio::test]
    async fn unrequested_shutdown_fails_the_device() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        sim.set_state("UDID-1", "Shutdown");
        let failed = wait_state(&manager, &id, DeviceState::Failed).await;
        assert_eq!(failed.failure.as_deref(), Some("Shutdown outside BiBCode"));
        manager.close(&id).await.unwrap();
        assert!(
            sim.calls().iter().all(|c| !c.starts_with("shutdown")),
            "BiBCode did not shut it down: {:?}",
            sim.calls()
        );
    }

    #[tokio::test]
    async fn a_simulator_booted_elsewhere_is_listed_and_refused() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .state("UDID-1", "Booted")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        manager.refresh_now().await;
        let catalog = manager.catalog().borrow().clone();
        assert!(catalog.platforms[0].images[0].in_use_elsewhere);
        assert!(matches!(
            manager.open(Platform::Ios, "UDID-1").await,
            Err(DeviceOpError::Busy { .. })
        ));
        assert!(sim.calls().iter().all(|c| !c.starts_with("boot")));
    }

    #[tokio::test]
    async fn close_shuts_down_only_what_bibcode_booted() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .device("UDID-2", "iPhone 17e", "iOS 27.0")
            .state("UDID-2", "Booted")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        let ready = wait_state(&manager, &id, DeviceState::Ready).await;
        assert_eq!((ready.width, ready.height), (Some(60), Some(130)));
        assert!(!ready.can_rotate);
        // Booted by BiBCode, so not in use elsewhere; the other one is.
        manager.refresh_now().await;
        let images = manager.catalog().borrow().platforms[0].images.clone();
        let in_use: Vec<_> = images
            .iter()
            .map(|image| (image.id.as_str(), image.in_use_elsewhere))
            .collect();
        assert_eq!(in_use, [("UDID-1", false), ("UDID-2", true)]);
        assert_eq!(images[0].display_name, "iPhone 17 Pro · iOS 27.0");
        manager.close(&id).await.unwrap();
        assert_eq!(
            sim.calls()
                .iter()
                .filter(|c| c.starts_with("shutdown UDID-1"))
                .count(),
            1
        );
        manager.shutdown().await;
        assert!(
            sim.calls()
                .iter()
                .all(|c| !c.starts_with("shutdown UDID-2"))
        );
    }

    #[tokio::test]
    async fn view_only_simulators_refuse_input_and_screenshot_as_jpeg() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        let refused = manager
            .input(
                &id,
                vec![DeviceInputEvent::Key {
                    key: crate::device::input::DeviceKey::Home,
                    action: KeyAction::Down,
                }],
            )
            .await;
        assert_eq!(
            refused,
            Err(DeviceOpError::InvalidInput(
                "Touch input is off: Xcode 27.0 changed the simulator interfaces BiBCode uses. Simulators open as screenshots without touch input.".into()
            ))
        );
        // Video tickets are refused at once, so the client shows still images.
        assert_eq!(
            manager
                .create_media_ticket(&id, "session", crate::device::media::MediaChannel::Plain)
                .unwrap_err()
                .to_rpc_error()["_tag"],
            "DeviceMediaUnavailableError"
        );
        let screenshot = manager.png(&id, 720).await.unwrap();
        assert_eq!(screenshot.mime_type, "image/jpeg");
        assert_eq!((screenshot.width, screenshot.height), (60, 130));
        assert_eq!(&screenshot.png[..2], &[0xFF, 0xD8]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn an_unsupported_xcode_still_opens_simulators_view_only() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        manager.refresh_now().await;
        let availability = manager.catalog().borrow().platforms[0].availability.clone();
        assert_eq!(
            availability.kind,
            crate::device::catalog::AvailabilityKind::XcodeUnsupported
        );
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        let ready = wait_state(&manager, &id, DeviceState::Ready).await;
        assert_eq!((ready.width, ready.height), (Some(60), Some(130)));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn closing_a_booting_simulator_shuts_down_only_it_once() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .device("UDID-2", "iPhone 17e", "iOS 27.0")
            .state("UDID-2", "Booted")
            .bootstatus_blocks()
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Booting).await;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !sim.calls().iter().any(|c| c == "bootstatus UDID-1 -b") {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("bootstatus started");
        manager.close(&id).await.unwrap();
        let stopped = wait_state(&manager, &id, DeviceState::Stopped).await;
        assert_eq!(stopped.failure, None);
        let shutdowns: Vec<_> = sim
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("shutdown"))
            .collect();
        assert_eq!(shutdowns, ["shutdown UDID-1"]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_boot_that_fails_leaves_the_simulator_alone() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .boot_fails("Unable to boot device in current state: Booted")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        let failed = wait_state(&manager, &id, DeviceState::Failed).await;
        assert_eq!(
            failed.failure.as_deref(),
            Some(
                "could not start: An error was encountered processing the command\nUnable to boot device in current state: Booted"
            )
        );
        assert!(sim.calls().iter().all(|c| !c.starts_with("shutdown")));
    }

    #[tokio::test]
    async fn without_xcode_the_platform_says_how_to_install_it() {
        let sim = FakeXcrun::new().write();
        let mut runner = sim.runner();
        runner.xcode_select = "/nonexistent/xcode-select".into();
        let (manager, _enabled) = super::testing::manager_for(runner);
        manager.refresh_now().await;
        let catalog = manager.catalog().borrow().clone();
        assert_eq!(
            catalog.platforms[0].availability.kind,
            crate::device::catalog::AvailabilityKind::XcodeMissing
        );
        assert_eq!(
            catalog.platforms[0].availability.message,
            "Install Xcode, then open it once to finish setup."
        );
        let no_sims = FakeXcrun::new().write();
        let (manager, _enabled) = ios_manager(&no_sims);
        manager.refresh_now().await;
        assert_eq!(
            manager.catalog().borrow().platforms[0].availability.kind,
            crate::device::catalog::AvailabilityKind::NoSimulators
        );
    }
}
