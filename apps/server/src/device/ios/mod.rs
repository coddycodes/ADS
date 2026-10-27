//! The iOS backend (macOS only): simulator discovery and lifecycle through `xcrun simctl`,
//! and the screen through CoreSimulator loaded in-process (`coresim`).
//!
//! When the private-API guard finds every name it needs, a booted simulator runs live: its
//! frames come from the framebuffer as it changes, its orientation reaches the catalog as
//! the simulator reports it, its video is H.264 from VideoToolbox (`encoder`), and input
//! and rotation go to it in-process (`hid`, `coresim::HidClient`). Otherwise it runs
//! view-only on `simctl` screenshots, and input, rotation and video are refused with the
//! reason.
pub(crate) mod coresim;
pub(crate) mod encoder;
pub(crate) mod guard;
pub(crate) mod hid;
pub(crate) mod keymap;
pub(crate) mod pixels;
pub(crate) mod simctl;
#[cfg(test)]
pub(crate) mod testing;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::{
    StreamExt as _,
    future::BoxFuture,
    stream::{self, BoxStream},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use self::{
    coresim::{CoreSim, HidClient, Screen},
    guard::{GuardCache, GuardResult, XcodeCache, availability_for},
    hid::{
        HidAction, HidError, HidSink, INPUT_FAILED, KEYBOARD_NOT_READY, PointerState, Press,
        ROTATE_FAILED, TouchPhase, paste_keys, plan_events,
    },
    pixels::IosOrientation,
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
    input::{DeviceInputEvent, oriented_size},
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
/// Why a simulator whose screen Coddy Studio could not attach runs view-only.
const ATTACH_FAILED_REASON: &str = "Coddy Studio could not attach to this simulator's screen.";
/// Why a live simulator whose input Coddy Studio could not reach takes no input.
const INPUT_ATTACH_FAILED_REASON: &str =
    "Coddy Studio could not connect to this simulator's input.";
/// How many times, and how far apart, `rotate` reads the orientation back.
const ROTATION_POLLS: u32 = 5;
const ROTATION_POLL_INTERVAL: Duration = Duration::from_millis(300);
pub(crate) const EXTERNAL_SHUTDOWN: &str = "Shutdown outside Coddy Studio";
pub(crate) const EXTERNAL_DELETE: &str = "Deleted outside Coddy Studio";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Makes the input sink for a test simulator, whatever its display.
#[cfg(test)]
pub(crate) type HidFactory = Arc<dyn Fn() -> Arc<dyn HidSink> + Send + Sync>;

pub(crate) struct IosBackendConfig {
    pub runner: SimctlRunner,
    pub boot_timeout: Duration,
    pub guard: GuardCache,
    /// Tests only: every simulator takes input through this sink instead of CoreSimulator.
    #[cfg(test)]
    pub hid_for_tests: Option<HidFactory>,
}

impl IosBackendConfig {
    /// The Mac's own `xcrun` and `xcode-select`. Test builds (`hermetic-test-guard`) name
    /// tools that do not exist, so a runtime with device previews on finds no Xcode and never
    /// lists or boots the host's simulators.
    pub(crate) fn from_process(registry: ProcessAttributionRegistry) -> Self {
        let bin = if cfg!(feature = "hermetic-test-guard") {
            PathBuf::from("/nonexistent/coddy-studio-hermetic")
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
            #[cfg(test)]
            hid_for_tests: None,
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
    /// UDIDs Coddy Studio booted and has not shut down; only those are never `in_use_elsewhere`.
    booted: Arc<Booted>,
    #[cfg(test)]
    hid_for_tests: Option<HidFactory>,
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
            #[cfg(test)]
            hid_for_tests: config.hid_for_tests,
        }
    }

    fn image(&self, device: &SimDevice) -> Image {
        Image {
            id: device.udid.clone(),
            display_name: simctl::display_name(device),
            in_use_elsewhere: device.state != SimState::Shutdown
                && !self.booted.contains(&device.udid),
        }
    }

    /// Shuts down a simulator `launch` booted, logging a failure.
    async fn shut_down_own(&self, udid: &str, launch: u64) {
        if let Err(error) = self.runner.shutdown(udid).await {
            tracing::warn!(%error, "could not shut down the simulator Coddy Studio booted");
        }
        self.booted.remove(udid, launch);
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
        let launch = self.booted.insert(udid);
        on_phase(DeviceState::Booting);
        let booted = tokio::select! {
            () = cancel.cancelled() => None,
            booted = self.runner.boot(udid, self.boot_timeout) => Some(booted),
        };
        match booted {
            Some(Ok(())) => {}
            // `simctl boot` failed, so it may be someone else's: leave it alone.
            Some(Err(error @ SimctlError::Failed { .. })) if error_is_boot(&error) => {
                self.booted.remove(udid, launch);
                return Err(LaunchFailure::Spawn(error.reason()));
            }
            Some(Err(error)) => {
                self.shut_down_own(udid, launch).await;
                return Err(match error {
                    SimctlError::Timeout { .. } => LaunchFailure::BootTimeout,
                    other => LaunchFailure::Exited(other.reason()),
                });
            }
            None => {
                self.shut_down_own(udid, launch).await;
                return Err(LaunchFailure::Cancelled);
            }
        }
        let display = tokio::select! {
            () = cancel.cancelled() => Err(LaunchFailure::Cancelled),
            display = self.attach_display(udid, mode) => display,
        };
        let (native, display, control) = match display {
            Ok(display) => display,
            Err(error) => {
                self.shut_down_own(udid, launch).await;
                return Err(error);
            }
        };
        #[cfg(test)]
        let control = match &self.hid_for_tests {
            Some(factory) => Control::Live(factory()),
            None => control,
        };
        let process = self.runner.register_simulator(udid).await;
        Ok(Box::new(RunningSimulator::start(
            udid.to_owned(),
            native,
            display,
            control,
            self.runner.clone(),
            Ownership {
                booted: self.booted.clone(),
                launch,
            },
            process,
        )))
    }

    /// The booted simulator's screen and input: live through CoreSimulator when the guard
    /// allows it and the screen attaches, otherwise `simctl` screenshots without input.
    /// Returns its portrait size.
    async fn attach_display(
        &self,
        udid: &str,
        mode: LaunchMode,
    ) -> Result<((u32, u32), Display, Control), LaunchFailure> {
        let reason = match mode {
            LaunchMode::Live(sim) => {
                let owned = udid.to_owned();
                let runtime = tokio::runtime::Handle::current();
                let attached = tokio::task::spawn_blocking(move || {
                    let screen = Screen::attach(&sim, &owned)?;
                    Ok((screen, HidClient::start(sim, &owned, runtime)))
                })
                .await
                .map_err(|error| DeviceError::Protocol(error.to_string()))
                .and_then(|attached| attached);
                match attached {
                    Ok((screen, hid)) => {
                        let control = match hid {
                            Ok(hid) => Control::Live(Arc::new(hid)),
                            Err(error) => {
                                tracing::warn!(%error, "could not reach the simulator's input; it takes none");
                                Control::Off {
                                    reason: INPUT_ATTACH_FAILED_REASON.to_owned(),
                                }
                            }
                        };
                        return Ok((screen.size(), Display::Live(Arc::new(screen)), control));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not attach to the simulator's screen; it runs view-only");
                        ATTACH_FAILED_REASON.to_owned()
                    }
                }
            }
            LaunchMode::ViewOnly { reason } => reason,
        };
        let native = self.first_screenshot_size(udid).await?;
        Ok((
            native,
            Display::ViewOnly {
                reason: reason.clone(),
            },
            Control::Off { reason },
        ))
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

    /// Refuses a simulator that is not `Shutdown`: it runs outside Coddy Studio.
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

/// Where a running simulator's input and rotations go.
enum Control {
    Live(Arc<dyn HidSink>),
    /// Input and rotation are refused for `reason`.
    Off {
        reason: String,
    },
}

/// A touch's up at `(x, y)`, in portrait ratios.
fn touch_up(x: f64, y: f64) -> HidAction {
    HidAction::Touch {
        phase: TouchPhase::Up,
        x,
        y,
    }
}

/// What a sink error means to the caller; `failed` is the copy for a send the simulator did
/// not take, which is logged without content.
fn sink_error(error: HidError, failed: &str) -> DeviceOpError {
    match error {
        HidError::KeyboardUnavailable => DeviceOpError::InvalidInput(KEYBOARD_NOT_READY.to_owned()),
        HidError::Released => DeviceOpError::NotReady(DeviceState::Stopped),
        HidError::Failed(detail) => {
            tracing::warn!(%detail, "the simulator did not accept input");
            DeviceOpError::Emulator(failed.to_owned())
        }
    }
}

/// What a batch whose `pbcopy` of `chars` characters failed returns. Only the stop cancels
/// a copy, so a cancelled one is the device stopping, not a failure to report; any other is
/// logged and the user is told to try again.
fn paste_failure(error: &SimctlError, chars: usize) -> DeviceOpError {
    if let SimctlError::Cancelled { .. } = error {
        return DeviceOpError::NotReady(DeviceState::Stopped);
    }
    tracing::warn!(%error, chars, "could not copy text to the simulator");
    DeviceOpError::Emulator(INPUT_FAILED.to_owned())
}

/// The simulators Coddy Studio booted, each with the launch that booted it.
#[derive(Default)]
struct Booted {
    next_launch: AtomicU64,
    launches: Mutex<HashMap<String, u64>>,
}

impl Booted {
    /// Records that a new launch boots `udid` and returns that launch's id.
    fn insert(&self, udid: &str) -> u64 {
        let launch = self.next_launch.fetch_add(1, Ordering::Relaxed);
        lock(&self.launches).insert(udid.to_owned(), launch);
        launch
    }

    fn contains(&self, udid: &str) -> bool {
        lock(&self.launches).contains_key(udid)
    }

    /// Forgets `udid` only while `launch` is the one that booted it.
    fn remove(&self, udid: &str, launch: u64) {
        let mut launches = lock(&self.launches);
        if launches.get(udid) == Some(&launch) {
            launches.remove(udid);
        }
    }
}

/// A running simulator's claim on its [`Booted`] entry.
struct Ownership {
    booted: Arc<Booted>,
    launch: u64,
}

/// The keys and buttons the guest may hold down. It covers the keys a batch synthesizes
/// (Shift around a capital, Cmd+V for a paste) as well as device keys, which may stay down
/// between batches.
#[derive(Default)]
struct Presses {
    /// In the order they went down: each is added before its down goes out and removed once
    /// its up was sent.
    down: Vec<Press>,
    /// Those whose release failed after a failed send, sent first in the next call.
    owed: Vec<Press>,
}

impl Presses {
    /// `press` went up.
    fn forget(&mut self, press: Press) {
        self.down.retain(|down| *down != press);
        self.owed.retain(|owed| *owed != press);
    }
}

/// Which presses [`Shared::release_presses`] releases.
#[derive(Clone, Copy, PartialEq)]
enum Releasing {
    /// Every one still down, after a failed send; what fails is owed.
    All,
    /// Only those owed.
    Owed,
}

/// A simulator Coddy Studio booted.
struct RunningSimulator {
    shared: Arc<Shared>,
}

struct Shared {
    udid: String,
    native: (u32, u32),
    display: Display,
    control: Control,
    /// The single touch pointer that is down, between batches.
    pointer: Mutex<PointerState>,
    /// Where a touch the guest may still hold was last sent, in portrait ratios: set
    /// before a down or move goes out, cleared once its up was sent.
    held: Mutex<Option<(f64, f64)>>,
    /// Where a touch was left down when its lift failed after a failed send: lifted first
    /// in the next call, before anything else, until a lift goes out.
    owed_lift: Mutex<Option<(f64, f64)>>,
    /// The keys and buttons the guest may still hold, and the releases still owed.
    presses: Mutex<Presses>,
    /// One input batch at a time, so batches arrive in order. An admitted batch's task
    /// holds it until the batch and its recovery are done.
    input_lock: Arc<tokio::sync::Mutex<()>>,
    /// The tasks that send admitted batches, whether or not their callers still wait; a
    /// stop waits for them.
    input_tasks: TaskTracker,
    runner: Arc<SimctlRunner>,
    /// Its entry in the backend's booted set, which only this launch's stop removes.
    ownership: Ownership,
    /// Why the simulator ended without Coddy Studio asking; set by the state poll.
    exit: Mutex<Option<String>>,
    /// Cancels the state poll.
    poll: CancellationToken,
    /// Cancelled by a stop, right after the input connections are released, to cut short a
    /// `pbcopy` still running: its child is killed instead of waited for.
    input_stop: CancellationToken,
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
        control: Control,
        runner: Arc<SimctlRunner>,
        ownership: Ownership,
        process: Option<SimulatorProcess>,
    ) -> Self {
        let shared = Arc::new(Shared {
            udid,
            native,
            display,
            control,
            pointer: Mutex::new(PointerState::default()),
            held: Mutex::new(None),
            owed_lift: Mutex::new(None),
            presses: Mutex::new(Presses::default()),
            input_lock: Arc::default(),
            input_tasks: TaskTracker::new(),
            runner,
            ownership,
            exit: Mutex::new(None),
            poll: CancellationToken::new(),
            input_stop: CancellationToken::new(),
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
        // Input closes first: nothing more reaches the simulator, and the HID connections
        // go before the screen and the simulator do.
        if let Control::Live(sink) = &self.control {
            sink.release().await;
        }
        // A `pbcopy` still running is cancelled, not waited for: its child is killed and
        // reaped within the runner's 2 s cleanup.
        self.input_stop.cancel();
        // A batch still sending finishes first. The released sink refuses its remaining
        // sends without FFI, so this waits at most for the step in flight: a HID send's 2 s
        // barrier or completion, or the 2 s cleanup of a cancelled `pbcopy`.
        self.input_tasks.close();
        self.input_tasks.wait().await;
        *lock(&self.pointer) = PointerState::default();
        lock(&self.held).take();
        lock(&self.owed_lift).take();
        *lock(&self.presses) = Presses::default();
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
        // A simulator shut down outside Coddy Studio may have been booted again by someone else.
        if !ended_outside && let Err(error) = self.runner.shutdown(&self.udid).await {
            tracing::warn!(%error, "could not shut down the simulator");
        }
        // A newer launch of the same simulator (reopened after an outside shutdown, before
        // this stop finished) keeps its entry.
        self.ownership
            .booted
            .remove(&self.udid, self.ownership.launch);
    }

    /// Plans and sends one validated batch in order; a paste is `simctl pbcopy` then Cmd+V.
    ///
    /// Once the batch is admitted (planned, the keyboard checked, the input lock taken), it
    /// is sent on a task of its own that holds the lock, and this call waits for its
    /// result. A caller that goes away, as the RPC layer drops a request whose connection
    /// closed, therefore never cuts a batch short between a press and its release, or
    /// before the recovery below: the batch and its recovery finish, and the next batch
    /// goes out after them.
    async fn input(
        self: &Arc<Self>,
        events: Vec<DeviceInputEvent>,
        orientation: Orientation,
    ) -> Result<(), DeviceOpError> {
        let sink = match &self.control {
            Control::Live(sink) => sink.clone(),
            Control::Off { reason } => {
                return Err(DeviceOpError::InvalidInput(format!(
                    "Touch input is off: {}.",
                    reason.trim_end_matches('.')
                )));
            }
        };
        let serial = self.input_lock.clone().lock_owned().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err(DeviceOpError::NotReady(DeviceState::Stopped));
        }
        // Planned against a copy: a refused batch leaves the pointer as it was.
        let mut pointer = lock(&self.pointer).clone();
        let plan = plan_events(&events, orientation, &mut pointer)?;
        if plan.iter().any(HidAction::needs_keyboard) && !sink.keyboard_ready() {
            return Err(DeviceOpError::InvalidInput(KEYBOARD_NOT_READY.to_owned()));
        }
        *lock(&self.pointer) = pointer;
        let shared = self.clone();
        let batch = self.input_tasks.spawn(async move {
            let _serial = serial;
            shared.send_admitted(sink.as_ref(), plan).await
        });
        batch.await.unwrap_or_else(|error| {
            tracing::warn!(%error, "an input batch's task ended without a result");
            Err(DeviceOpError::Emulator(INPUT_FAILED.to_owned()))
        })
    }

    /// Sends an admitted batch. A touch lift owed since a failed send goes out first, then
    /// the key and button releases owed, once the keyboard is ready for the keys among
    /// them. A call with a send that fails, theirs included, then lifts the touch the guest
    /// may hold and releases every key and button it may hold, best effort.
    async fn send_admitted(
        &self,
        sink: &dyn HidSink,
        plan: Vec<HidAction>,
    ) -> Result<(), DeviceOpError> {
        let owed = match self.lift_owed_touch(sink).await {
            Ok(()) => self.release_presses(sink, Releasing::Owed).await,
            Err(error) => Err(error),
        };
        let result = match owed {
            Ok(()) => self.send_plan(sink, plan).await,
            Err(error) => Err(sink_error(error, INPUT_FAILED)),
        };
        if result.is_err() {
            self.lift_held_touch(sink).await;
            if let Err(error) = self.release_presses(sink, Releasing::All).await {
                tracing::debug!(?error, "could not release a held key after a failed send");
            }
        }
        result
    }

    /// Sends a planned batch in order (see [`Shared::send_one`]).
    async fn send_plan(
        &self,
        sink: &dyn HidSink,
        plan: Vec<HidAction>,
    ) -> Result<(), DeviceOpError> {
        for action in plan {
            match action {
                HidAction::Paste { text } => {
                    if self.stopping.load(Ordering::SeqCst) {
                        return Err(DeviceOpError::NotReady(DeviceState::Stopped));
                    }
                    if let Err(error) = self
                        .runner
                        .pbcopy(&self.udid, &text, &self.input_stop)
                        .await
                    {
                        return Err(paste_failure(&error, text.chars().count()));
                    }
                    for key in paste_keys() {
                        self.send_one(sink, key).await?;
                    }
                }
                action => self.send_one(sink, action).await?,
            }
        }
        Ok(())
    }

    /// Sends one action, keeping `held` and `presses` in step with what the guest may
    /// hold. A touch or a press is recorded before its down (or move) goes out, since a
    /// send that fails may still have reached the guest, and forgotten once its up was
    /// sent.
    async fn send_one(&self, sink: &dyn HidSink, action: HidAction) -> Result<(), DeviceOpError> {
        let touch = match action {
            HidAction::Touch { phase, x, y } => Some((phase, x, y)),
            _ => None,
        };
        let press = action.press();
        if let Some((TouchPhase::Down | TouchPhase::Move, x, y)) = touch {
            *lock(&self.held) = Some((x, y));
        }
        if let Some((press, true)) = press {
            let mut presses = lock(&self.presses);
            if !presses.down.contains(&press) {
                presses.down.push(press);
            }
            // Down again: no longer a release owed, but a key held.
            presses.owed.retain(|owed| *owed != press);
        }
        sink.send(action)
            .await
            .map_err(|error| sink_error(error, INPUT_FAILED))?;
        if let Some((TouchPhase::Up, ..)) = touch {
            *lock(&self.held) = None;
        }
        if let Some((press, false)) = press {
            lock(&self.presses).forget(press);
        }
        Ok(())
    }

    /// Sends the releases of the presses `releasing` covers, the latest press first and
    /// modifiers last, and forgets each one that went out. Keys go only through DTUHID, so
    /// while the keyboard is not ready they are left for a later call; buttons go out
    /// either way. Every release is tried and the first failure is returned. After a
    /// failed send ([`Releasing::All`]), whatever is still down is owed, and the next call
    /// sends it before its batch. A key up for a key that is not down is harmless, so a
    /// release the client sends as well never does damage.
    ///
    /// The legacy Indigo client has no send path of its own: `HidSink::send` picks it for
    /// touches and buttons while DTUHID is not live, so a button is released through it
    /// like any other send. It cannot carry keys at all.
    async fn release_presses(
        &self,
        sink: &dyn HidSink,
        releasing: Releasing,
    ) -> Result<(), HidError> {
        let mut presses = {
            let presses = lock(&self.presses);
            match releasing {
                Releasing::All => presses.down.clone(),
                Releasing::Owed => presses.owed.clone(),
            }
        };
        if presses.is_empty() {
            return Ok(());
        }
        presses.reverse();
        // Stable: within each group, the latest press stays first.
        presses.sort_by_key(|press| press.is_modifier());
        let keyboard =
            presses.iter().any(|press| press.release().needs_keyboard()) && sink.keyboard_ready();
        let mut result = Ok(());
        for press in presses {
            let release = press.release();
            if release.needs_keyboard() && !keyboard {
                continue;
            }
            match sink.send(release).await {
                Ok(()) => lock(&self.presses).forget(press),
                Err(error) => {
                    if result.is_ok() {
                        result = Err(error);
                    }
                }
            }
        }
        if releasing == Releasing::All {
            let mut presses = lock(&self.presses);
            presses.owed = presses.down.clone();
        }
        result
    }

    /// Sends the touch lift owed since a failed send, if any, and forgets it once it went
    /// out. The pointer is already idle, so a client's own up or cancel for that touch
    /// plans nothing: this lift is what releases the guest's finger.
    async fn lift_owed_touch(&self, sink: &dyn HidSink) -> Result<(), HidError> {
        let owed = *lock(&self.owed_lift);
        if let Some((x, y)) = owed {
            sink.send(touch_up(x, y)).await?;
            lock(&self.owed_lift).take();
        }
        Ok(())
    }

    /// After a failed send: lifts a touch the guest may still hold, best effort, at its
    /// last known position (the sink sends it on the path its down took), and leaves the
    /// pointer idle. A lift that fails is owed, and the next call sends it first.
    async fn lift_held_touch(&self, sink: &dyn HidSink) {
        let held = lock(&self.held).take();
        if let Some((x, y)) = held
            && let Err(error) = sink.send(touch_up(x, y)).await
        {
            tracing::debug!(?error, "could not lift a held touch after a failed send");
            *lock(&self.owed_lift) = Some((x, y));
        }
        *lock(&self.pointer) = PointerState::default();
    }

    /// Sends the Purple orientation for `to`, then reads back what the simulator reports
    /// and passes that to `report`, never what was asked: the UI turns only when the
    /// foreground app allows it. Returns `to`: the readback is not evidence of a refusal,
    /// so the client never says an iOS turn was refused.
    async fn rotate(
        &self,
        to: Orientation,
        report: &(dyn Fn(Orientation, u32, u32) + Send + Sync),
    ) -> Result<Orientation, DeviceOpError> {
        let sink = match &self.control {
            Control::Live(sink) => sink.clone(),
            Control::Off { reason } => {
                return Err(DeviceOpError::InvalidInput(format!(
                    "Rotation is off: {}.",
                    reason.trim_end_matches('.')
                )));
            }
        };
        sink.rotate(IosOrientation::from_orientation(to).purple())
            .await
            .map_err(|error| sink_error(error, ROTATE_FAILED))?;
        let Display::Live(screen) = &self.display else {
            return Ok(to);
        };
        let mut reported = screen.orientation();
        let (width, height) = self.native;
        for poll in 0..ROTATION_POLLS {
            if poll > 0 {
                tokio::time::sleep(ROTATION_POLL_INTERVAL).await;
            }
            let orientation = reported.borrow_and_update().orientation();
            let (width, height) = oriented_size(width, height, orientation);
            report(orientation, width, height);
            if orientation == to {
                break;
            }
        }
        Ok(to)
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

    /// Live simulators turn through the Purple port; view-only ones cannot.
    fn can_rotate(&self) -> bool {
        matches!(self.shared.control, Control::Live(_))
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

    /// Live: H.264 from the framebuffer. View-only simulators refuse media tickets first
    /// (`video_unavailable`), so they do not reach this.
    fn video(&self) -> BoxFuture<'_, Result<Box<dyn ErasedVideoSource>, DeviceError>> {
        Box::pin(async move {
            match &self.shared.display {
                Display::Live(screen) => {
                    let video = encoder::SimulatorVideo::start(screen.clone()).await?;
                    Ok(Box::new(video) as Box<dyn ErasedVideoSource>)
                }
                Display::ViewOnly { .. } => Err(DeviceError::Protocol(
                    "a view-only simulator has no video".into(),
                )),
            }
        })
    }

    fn video_unavailable(&self) -> Option<String> {
        match &self.shared.display {
            Display::Live(_) => None,
            Display::ViewOnly { reason } => {
                Some(format!("Video is off: {}.", reason.trim_end_matches('.')))
            }
        }
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
        events: Vec<DeviceInputEvent>,
        orientation: Orientation,
    ) -> BoxFuture<'_, Result<(), DeviceOpError>> {
        Box::pin(self.shared.input(events, orientation))
    }

    fn rotate<'a>(
        &'a self,
        to: Orientation,
        report: &'a (dyn Fn(Orientation, u32, u32) + Send + Sync),
    ) -> BoxFuture<'a, Result<Orientation, DeviceOpError>> {
        Box::pin(self.shared.rotate(to, report))
    }

    fn exited(&self) -> Option<String> {
        lock(&self.shared.exit).clone()
    }

    fn report_unrequested_exit(&self, reason: &str) {
        let (pid, started) = lock(&self.shared.process)
            .as_ref()
            .map_or((0, Instant::now()), |process| {
                (process.pid, process.started)
            });
        tracing::warn!(reason, pid, "ios simulator ended outside Coddy Studio");
        report_simulator_exit(pid, started);
    }

    fn stop(self: Box<Self>) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.shared.stop_now().await })
    }

    fn stop_shared(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.shared.stop_now())
    }
}

/// A `subprocess.crash` for a simulator that ended without Coddy Studio asking. It has no exit
/// status of its own, so it is reported as exit code 1.
fn report_simulator_exit(pid: u32, started: Instant) {
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
            started,
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
    use super::{
        hid::{HidAction, INPUT_FAILED, KEYBOARD_NOT_READY, SimButton, TouchPhase},
        paste_failure,
        simctl::SimctlError,
        testing::{FakeXcrun, RecordingHid, ios_manager, live_fake_ios},
    };
    use crate::device::{
        DeviceOpError,
        catalog::{DeviceState, Orientation, Platform},
        fake_backend::wait_state,
        input::{DeviceInputEvent, DeviceKey, KeyAction, TouchAction},
    };

    /// Round-11 review M2: only the stop cancels a `pbcopy`, so a batch whose copy it cut
    /// short ends as the device stopping; every other failure is the generic input error.
    #[test]
    fn a_cancelled_pbcopy_is_the_device_stopping() {
        let command = || "simctl pbcopy".to_owned();
        assert_eq!(
            paste_failure(&SimctlError::Cancelled { command: command() }, 3),
            DeviceOpError::NotReady(DeviceState::Stopped)
        );
        for error in [
            SimctlError::Timeout { command: command() },
            SimctlError::Spawn {
                command: command(),
                message: "not found".to_owned(),
            },
            SimctlError::Failed {
                command: command(),
                status: "exit status: 1".to_owned(),
                stderr: None,
            },
        ] {
            assert_eq!(
                paste_failure(&error, 3),
                DeviceOpError::Emulator(INPUT_FAILED.to_owned()),
                "{error}"
            );
        }
    }

    fn tap_down(x: f64, y: f64) -> DeviceInputEvent {
        DeviceInputEvent::Touch {
            action: TouchAction::Down,
            pointer_id: 0,
            x,
            y,
        }
    }

    #[tokio::test]
    async fn input_after_stop_is_refused_without_ffi() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let hid = RecordingHid::new();
        let (manager, _enabled) = live_fake_ios(&sim, &hid);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        let ready = wait_state(&manager, &id, DeviceState::Ready).await;
        assert!(ready.can_rotate);
        manager.input(&id, vec![tap_down(0.5, 0.5)]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [HidAction::Touch {
                phase: TouchPhase::Down,
                x: 0.5,
                y: 0.5
            }]
        );
        manager.close(&id).await.unwrap();
        assert!(hid.released(), "stop releases the HID connections");
        let tap = vec![tap_down(0.5, 0.5)];
        assert!(matches!(
            manager.input(&id, tap).await,
            Err(DeviceOpError::NotReady(DeviceState::Stopped))
        ));
        assert!(matches!(
            manager.rotate(&id, Orientation::Landscape).await,
            Err(DeviceOpError::NotReady(DeviceState::Stopped))
        ));
        assert_eq!(hid.sent_after_release(), 0);
        assert!(hid.sent().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn text_with_a_non_us_character_is_pasted_whole() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let hid = RecordingHid::new();
        let (manager, _enabled) = live_fake_ios(&sim, &hid);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        manager
            .input(
                &id,
                vec![
                    DeviceInputEvent::Text { text: "aé".into() },
                    DeviceInputEvent::Key {
                        key: DeviceKey::Home,
                        action: KeyAction::Down,
                    },
                ],
            )
            .await
            .unwrap();
        let key = |usage, down| HidAction::Key { usage, down };
        // "aé" has a character no US key types, so the whole event is pasted.
        assert_eq!(
            hid.sent(),
            [
                key(0xE3, true),
                key(0x19, true),
                key(0x19, false),
                key(0xE3, false),
                HidAction::Button {
                    button: SimButton::Home,
                    down: true
                },
            ]
        );
        assert_eq!(sim.pasteboard().as_deref(), Some("aé"));
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn keys_wait_for_the_keyboard_and_buttons_do_not() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let hid = RecordingHid::new();
        hid.set_keyboard(false);
        let (manager, _enabled) = live_fake_ios(&sim, &hid);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        let refused = manager
            .input(
                &id,
                vec![
                    tap_down(0.1, 0.1),
                    DeviceInputEvent::Text { text: "é".into() },
                ],
            )
            .await;
        assert_eq!(
            refused,
            Err(DeviceOpError::InvalidInput(KEYBOARD_NOT_READY.into()))
        );
        // Nothing of the refused batch went out, not even to the pasteboard.
        assert!(hid.sent().is_empty());
        assert_eq!(sim.pasteboard(), None);
        let volume = DeviceInputEvent::Key {
            key: DeviceKey::VolumeUp,
            action: KeyAction::Down,
        };
        manager.input(&id, vec![volume]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [HidAction::Button {
                button: SimButton::VolumeUp,
                down: true
            }]
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_refused_batch_leaves_the_pointer_up() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let hid = RecordingHid::new();
        hid.set_keyboard(false);
        let (manager, _enabled) = live_fake_ios(&sim, &hid);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        let refused = manager
            .input(
                &id,
                vec![
                    tap_down(0.1, 0.1),
                    DeviceInputEvent::Text { text: "é".into() },
                ],
            )
            .await;
        assert_eq!(
            refused,
            Err(DeviceOpError::InvalidInput(KEYBOARD_NOT_READY.into()))
        );
        manager.input(&id, vec![tap_down(0.2, 0.3)]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [HidAction::Touch {
                phase: TouchPhase::Down,
                x: 0.2,
                y: 0.3
            }]
        );
        manager.shutdown().await;
    }

    fn touch(action: TouchAction, x: f64, y: f64) -> DeviceInputEvent {
        DeviceInputEvent::Touch {
            action,
            pointer_id: 0,
            x,
            y,
        }
    }

    fn sent_touch(phase: TouchPhase, x: f64, y: f64) -> HidAction {
        HidAction::Touch { phase, x, y }
    }

    async fn ready_live_fake(
        sim: &FakeXcrun,
        hid: &std::sync::Arc<RecordingHid>,
    ) -> (
        crate::device::DeviceManager,
        tokio::sync::watch::Sender<bool>,
        String,
    ) {
        let (manager, enabled) = live_fake_ios(sim, hid);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        (manager, enabled, id)
    }

    fn one_device() -> FakeXcrun {
        FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write()
    }

    #[tokio::test]
    async fn a_failed_move_lifts_the_touch_and_frees_the_pointer() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        hid.fail_next(|action| {
            matches!(
                action,
                HidAction::Touch {
                    phase: TouchPhase::Move,
                    ..
                }
            )
        });
        let failed = manager
            .input(
                &id,
                vec![
                    touch(TouchAction::Down, 0.1, 0.2),
                    touch(TouchAction::Move, 0.3, 0.4),
                    touch(TouchAction::Up, 0.3, 0.4),
                ],
            )
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        // The down went out, the move failed, and the touch was lifted where the move went.
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Down, 0.1, 0.2),
                sent_touch(TouchPhase::Up, 0.3, 0.4),
            ]
        );
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_failed_up_for_a_held_pointer_is_retried_and_resets() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        manager.input(&id, vec![tap_down(0.1, 0.2)]).await.unwrap();
        manager
            .input(&id, vec![touch(TouchAction::Move, 0.2, 0.2)])
            .await
            .unwrap();
        hid.sent();
        hid.fail_next(|action| {
            matches!(
                action,
                HidAction::Touch {
                    phase: TouchPhase::Up,
                    ..
                }
            )
        });
        let failed = manager
            .input(&id, vec![touch(TouchAction::Up, 0.25, 0.2)])
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        // The best-effort up goes where the touch last was.
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Up, 0.2, 0.2)]);
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    /// A drag held at (0.2, 0.2) whose up fails, and whose best-effort lift fails too
    /// (issue #130). Nothing reached the guest, so its finger is still down.
    async fn a_drag_whose_up_and_lift_both_failed(
        hid: &std::sync::Arc<RecordingHid>,
        manager: &crate::device::DeviceManager,
        id: &str,
    ) {
        manager.input(id, vec![tap_down(0.1, 0.2)]).await.unwrap();
        manager
            .input(id, vec![touch(TouchAction::Move, 0.2, 0.2)])
            .await
            .unwrap();
        hid.sent();
        hid.fail_times(2, |action| {
            matches!(
                action,
                HidAction::Touch {
                    phase: TouchPhase::Up,
                    ..
                }
            )
        });
        let failed = manager
            .input(id, vec![touch(TouchAction::Up, 0.25, 0.2)])
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert!(hid.sent().is_empty());
    }

    /// Once sends work again, the owed lift goes out before the next touch, and only once.
    #[tokio::test]
    async fn a_lift_that_failed_twice_goes_out_before_the_next_touch() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        a_drag_whose_up_and_lift_both_failed(&hid, &manager, &id).await;
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Up, 0.2, 0.2),
                sent_touch(TouchPhase::Down, 0.5, 0.6),
            ]
        );
        manager
            .input(&id, vec![touch(TouchAction::Up, 0.5, 0.6)])
            .await
            .unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Up, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    /// The input queue retries a failed up as a cancel. The pointer is already idle, so the
    /// cancel plans nothing, but the call still sends the owed lift.
    #[tokio::test]
    async fn a_cancel_retry_after_a_lift_failed_twice_lifts_the_touch() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        a_drag_whose_up_and_lift_both_failed(&hid, &manager, &id).await;
        manager
            .input(&id, vec![touch(TouchAction::Cancel, 0.25, 0.2)])
            .await
            .unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Up, 0.2, 0.2)]);
        // Lifted once: the next touch sends only itself.
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    /// Keyboard input after a lift failed twice also lifts the touch first.
    #[tokio::test]
    async fn keys_after_a_lift_failed_twice_lift_the_touch_first() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        a_drag_whose_up_and_lift_both_failed(&hid, &manager, &id).await;
        manager.input(&id, vec![text("b")]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Up, 0.2, 0.2),
                key(0x05, true),
                key(0x05, false),
            ]
        );
        manager.shutdown().await;
    }

    /// A call whose owed lift fails again fails, sends nothing of its own, and keeps the
    /// lift owed.
    #[tokio::test]
    async fn an_owed_lift_that_fails_again_stays_owed() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        a_drag_whose_up_and_lift_both_failed(&hid, &manager, &id).await;
        hid.fail_next(|action| *action == sent_touch(TouchPhase::Up, 0.2, 0.2));
        let failed = manager.input(&id, vec![tap_down(0.5, 0.6)]).await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert!(hid.sent().is_empty());
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Up, 0.2, 0.2),
                sent_touch(TouchPhase::Down, 0.5, 0.6),
            ]
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_failed_key_after_a_held_down_lifts_the_touch() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        hid.fail_next(|action| matches!(action, HidAction::Key { .. }));
        let failed = manager
            .input(
                &id,
                vec![
                    tap_down(0.1, 0.2),
                    DeviceInputEvent::Key {
                        key: DeviceKey::Enter,
                        action: KeyAction::Down,
                    },
                ],
            )
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        // The Enter down may have reached the guest before it failed, so it is released
        // too, after the touch.
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Down, 0.1, 0.2),
                sent_touch(TouchPhase::Up, 0.1, 0.2),
                key(0x28, false),
            ]
        );
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    fn key(usage: u32, down: bool) -> HidAction {
        HidAction::Key { usage, down }
    }

    fn text(text: &str) -> DeviceInputEvent {
        DeviceInputEvent::Text { text: text.into() }
    }

    fn device_key(key: DeviceKey, action: KeyAction) -> DeviceInputEvent {
        DeviceInputEvent::Key { key, action }
    }

    const SHIFT: u32 = 0xE1;
    const COMMAND: u32 = 0xE3;
    const KEY_A: u32 = 0x04;
    const KEY_V: u32 = 0x19;

    /// Codex review P2: typing "A" is Shift down, a down, a up, Shift up. A send that fails
    /// partway used to leave Shift down, and every later key typed shifted.
    #[tokio::test]
    async fn a_failure_after_shift_down_releases_shift_and_the_key() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        hid.fail_next(|action| *action == key(KEY_A, false));
        let failed = manager.input(&id, vec![text("A")]).await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        // The a up failed; the recovery sends it again, then Shift's, modifiers last.
        assert_eq!(
            hid.sent(),
            [
                key(SHIFT, true),
                key(KEY_A, true),
                key(KEY_A, false),
                key(SHIFT, false),
            ]
        );
        // Nothing is held any more: the next batch sends only itself.
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    /// The same for a paste's Cmd+V: Cmd is released whatever failed after it went down.
    #[tokio::test]
    async fn a_failed_paste_releases_command() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        hid.fail_next(|action| *action == key(KEY_V, true));
        let failed = manager.input(&id, vec![text("é")]).await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert_eq!(
            hid.sent(),
            [key(COMMAND, true), key(KEY_V, false), key(COMMAND, false)]
        );
        manager.shutdown().await;
    }

    /// A release that fails after a failed send stays held. It goes out first in the next
    /// call once the keyboard is ready, and a call whose release fails fails too.
    #[tokio::test]
    async fn a_failed_release_is_sent_again_before_the_next_batch() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        // The a up fails, and so does its release after the failure.
        hid.fail_times(2, |action| *action == key(KEY_A, false));
        let failed = manager.input(&id, vec![text("a")]).await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert_eq!(hid.sent(), [key(KEY_A, true)]);
        // Keys need the keyboard: while it is not ready, the release waits, and input that
        // needs no keyboard still goes out.
        hid.set_keyboard(false);
        manager.input(&id, vec![tap_down(0.1, 0.2)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.1, 0.2)]);
        // Once it is ready the release goes first. Here it fails again, so the call fails
        // as any failed send does: the touch is lifted, and the release is tried once more.
        hid.set_keyboard(true);
        hid.fail_next(|action| *action == key(KEY_A, false));
        let failed = manager
            .input(&id, vec![touch(TouchAction::Move, 0.2, 0.2)])
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert_eq!(
            hid.sent(),
            [sent_touch(TouchPhase::Up, 0.1, 0.2), key(KEY_A, false)]
        );
        manager.input(&id, vec![tap_down(0.5, 0.6)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.5, 0.6)]);
        manager.shutdown().await;
    }

    /// A release still held when its key is typed again goes out before the new press.
    #[tokio::test]
    async fn a_held_release_goes_out_before_the_next_batch() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        hid.fail_times(2, |action| *action == key(SHIFT, false));
        let failed = manager.input(&id, vec![text("A")]).await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        hid.sent();
        manager.input(&id, vec![text("b")]).await.unwrap();
        assert_eq!(
            hid.sent(),
            [key(SHIFT, false), key(0x05, true), key(0x05, false)]
        );
        manager.shutdown().await;
    }

    /// Device keys share the rule: a key or button held down between batches, as from the
    /// toolbar, is released when a later send fails. The client's own release later is a
    /// second key up, which is harmless.
    #[tokio::test]
    async fn a_failed_send_releases_device_keys_still_held() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        manager
            .input(
                &id,
                vec![
                    device_key(DeviceKey::Backspace, KeyAction::Down),
                    device_key(DeviceKey::Home, KeyAction::Down),
                ],
            )
            .await
            .unwrap();
        hid.sent();
        // They stay down while batches go through.
        manager.input(&id, vec![tap_down(0.1, 0.2)]).await.unwrap();
        assert_eq!(hid.sent(), [sent_touch(TouchPhase::Down, 0.1, 0.2)]);
        hid.fail_next(|action| matches!(action, HidAction::Touch { .. }));
        let failed = manager
            .input(&id, vec![touch(TouchAction::Move, 0.3, 0.4)])
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        // The touch is lifted, then the latest press is released first.
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Up, 0.3, 0.4),
                HidAction::Button {
                    button: SimButton::Home,
                    down: false
                },
                key(0x2A, false),
            ]
        );
        manager
            .input(&id, vec![device_key(DeviceKey::Backspace, KeyAction::Up)])
            .await
            .unwrap();
        assert_eq!(hid.sent(), [key(0x2A, false)]);
        manager.shutdown().await;
    }

    /// Keys and buttons that went down and up leave nothing to release after a later
    /// failure.
    #[tokio::test]
    async fn a_batch_that_goes_through_leaves_no_key_held() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        manager
            .input(
                &id,
                vec![
                    text("A"),
                    text("é"),
                    device_key(DeviceKey::Enter, KeyAction::Down),
                    device_key(DeviceKey::Enter, KeyAction::Up),
                    device_key(DeviceKey::Home, KeyAction::Down),
                    device_key(DeviceKey::Home, KeyAction::Up),
                ],
            )
            .await
            .unwrap();
        hid.sent();
        hid.fail_next(|action| {
            matches!(
                action,
                HidAction::Touch {
                    phase: TouchPhase::Move,
                    ..
                }
            )
        });
        let failed = manager
            .input(
                &id,
                vec![
                    touch(TouchAction::Down, 0.1, 0.2),
                    touch(TouchAction::Move, 0.3, 0.4),
                ],
            )
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Down, 0.1, 0.2),
                sent_touch(TouchPhase::Up, 0.3, 0.4),
            ]
        );
        manager.shutdown().await;
    }

    /// Codex review P2: the RPC layer drops a request's future when its connection closes.
    /// A batch dropped after Shift went down used to stop there, with Shift neither released
    /// nor owed, so it stayed down. An admitted batch now finishes on its own task, which
    /// holds the input lock, so the next batch goes out after it.
    #[tokio::test]
    async fn a_batch_whose_caller_goes_away_still_finishes() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        // The Shift down is on its way to the guest, and its barrier has not run.
        let (reached, go) = hid.hold_next(|action| *action == key(SHIFT, true));
        tokio::select! {
            result = manager.input(&id, vec![text("A")]) => panic!("the batch finished: {result:?}"),
            reached = reached => reached.expect("the Shift down was sent"),
        }
        // The caller's future is gone, as `run_unary` drops it.
        assert_eq!(hid.sent(), [key(SHIFT, true)]);
        // A later batch, made while the first is still held, waits for the task that owns
        // the input lock. Were the guard left with the caller, it would be free here and
        // `b` would go out while the Shift down is still unanswered.
        let next = manager.input(&id, vec![text("b")]);
        tokio::pin!(next);
        let early = tokio::time::timeout(std::time::Duration::from_millis(100), &mut next).await;
        assert!(
            early.is_err(),
            "a later batch waits for the batch whose caller left"
        );
        assert!(
            hid.sent().is_empty(),
            "nothing of the later batch has gone out"
        );
        let _ = go.send(());
        next.await.unwrap();
        assert_eq!(
            hid.sent(),
            [
                key(KEY_A, true),
                key(KEY_A, false),
                key(SHIFT, false),
                key(0x05, true),
                key(0x05, false),
            ]
        );
        // Nothing is held or owed: a failed send after it lifts only the touch.
        hid.fail_next(|action| {
            matches!(
                action,
                HidAction::Touch {
                    phase: TouchPhase::Move,
                    ..
                }
            )
        });
        let failed = manager
            .input(
                &id,
                vec![
                    touch(TouchAction::Down, 0.1, 0.2),
                    touch(TouchAction::Move, 0.3, 0.4),
                ],
            )
            .await;
        assert!(
            matches!(failed, Err(DeviceOpError::Emulator(_))),
            "{failed:?}"
        );
        assert_eq!(
            hid.sent(),
            [
                sent_touch(TouchPhase::Down, 0.1, 0.2),
                sent_touch(TouchPhase::Up, 0.3, 0.4),
            ]
        );
        manager.shutdown().await;
    }

    /// A stop waits for a batch still sending, whose caller went away. The sink is released
    /// first, so it refuses the rest of the batch and the stop ends once the send in flight
    /// does.
    #[tokio::test]
    async fn a_stop_waits_for_a_batch_still_sending() {
        let sim = one_device();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        let (reached, go) = hid.hold_next(|action| *action == key(SHIFT, true));
        tokio::select! {
            result = manager.input(&id, vec![text("A")]) => panic!("the batch finished: {result:?}"),
            reached = reached => reached.expect("the Shift down was sent"),
        }
        let close = manager.close(&id);
        tokio::pin!(close);
        let early = tokio::time::timeout(std::time::Duration::from_millis(300), &mut close).await;
        assert!(early.is_err(), "the stop waits for the batch");
        assert!(hid.released(), "the sink is released before the wait");
        let _ = go.send(());
        tokio::time::timeout(std::time::Duration::from_secs(5), close)
            .await
            .expect("the stop ends once the send in flight does")
            .unwrap();
        // The rest of the batch was refused without reaching the guest.
        assert_eq!(hid.sent(), [key(SHIFT, true)]);
        assert!(hid.sent_after_release() > 0);
        manager.shutdown().await;
    }

    /// Review M2: a stop used to wait out a hanging `pbcopy`, 15 s and 2 s of cleanup, before
    /// it issued `simctl shutdown`; the desktop gives a quitting backend 5 s. The stop now
    /// cancels the copy, so its child is killed and reaped within the runner's cleanup time.
    #[tokio::test]
    async fn a_stop_cuts_a_hanging_pbcopy_short_and_still_shuts_the_simulator_down() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .pbcopy_hangs()
            .write();
        let hid = RecordingHid::new();
        let (manager, _enabled, id) = ready_live_fake(&sim, &hid).await;
        let copying = || sim.calls().iter().any(|call| call.starts_with("pbcopy"));
        // The paste's copy is running and does not end; its caller goes away, as when the
        // connection closes.
        let wait_for_copy = async {
            while !copying() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            result = manager.input(&id, vec![text("é")]) => panic!("the batch finished: {result:?}"),
            () = wait_for_copy => {}
        }
        let started = std::time::Instant::now();
        // Well below the copy's own 15 s timeout, which a stop that waited it out would meet.
        let bound = super::simctl::CLEANUP_TIMEOUT + std::time::Duration::from_secs(3);
        tokio::time::timeout(bound, manager.close(&id))
            .await
            .expect("the stop does not wait out the hanging pbcopy")
            .unwrap();
        assert!(started.elapsed() < bound, "{:?}", started.elapsed());
        assert!(
            sim.calls().iter().any(|call| call == "shutdown UDID-1"),
            "the simulator was shut down: {:?}",
            sim.calls()
        );
        // The cancelled copy was not followed by a Cmd+V.
        assert!(hid.sent().is_empty(), "{:?}", hid.sent());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn rotate_sends_the_purple_value_for_the_android_convention() {
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let hid = RecordingHid::new();
        let (manager, _enabled) = live_fake_ios(&sim, &hid);
        let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
        wait_state(&manager, &id, DeviceState::Ready).await;
        for orientation in [
            Orientation::Landscape,
            Orientation::ReverseLandscape,
            Orientation::ReversePortrait,
            Orientation::Portrait,
        ] {
            // iOS returns what was asked, so the client never notes a refusal.
            assert_eq!(manager.rotate(&id, orientation).await.unwrap(), orientation);
        }
        // Landscape is Purple 3 (`uiOrientation` 4, the UI's top on the right edge).
        assert_eq!(hid.rotations(), [3, 4, 2, 1]);
        // Screenshots report no orientation, so the catalog stays where it was.
        let snapshot = manager.catalog().borrow().devices[0].clone();
        assert_eq!(snapshot.orientation, Orientation::Portrait);
        manager.shutdown().await;
    }

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
        assert_eq!(
            failed.failure.as_deref(),
            Some("Shutdown outside Coddy Studio")
        );
        manager.close(&id).await.unwrap();
        assert!(
            sim.calls().iter().all(|c| !c.starts_with("shutdown")),
            "Coddy Studio did not shut it down: {:?}",
            sim.calls()
        );
    }

    #[tokio::test]
    async fn an_old_stop_keeps_a_reopened_simulator_owned_by_coddy_studio() {
        use super::{IosBackend, IosBackendConfig, guard::GuardCache, guard::GuardResult};
        use crate::device::{backend::DeviceBackend, catalog::AvailabilityKind};
        use tokio_util::sync::CancellationToken;
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let backend = IosBackend::new(IosBackendConfig {
            runner: sim.runner(),
            boot_timeout: std::time::Duration::from_secs(10),
            guard: GuardCache::with_prober(Box::new(|_| GuardResult::ViewOnly {
                kind: AvailabilityKind::XcodeUnsupported,
                missing: "fake Xcode".to_owned(),
            })),
            hid_for_tests: None,
        });
        let cancel = CancellationToken::new();
        let first = backend
            .launch("UDID-1", Box::new(|_| {}), &cancel)
            .await
            .unwrap_or_else(|_| panic!("first launch failed"));
        sim.set_state("UDID-1", "Shutdown");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while first.exited().is_none() {
            assert!(std::time::Instant::now() < deadline, "no outside shutdown");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Reopened before the failed device's stop finished.
        let second = backend
            .launch("UDID-1", Box::new(|_| {}), &cancel)
            .await
            .unwrap_or_else(|_| panic!("second launch failed"));
        first.stop().await;
        let in_use = |discovery: crate::device::backend::PlatformDiscovery| {
            discovery.images[0].in_use_elsewhere
        };
        assert!(
            !in_use(backend.discover().await),
            "Coddy Studio's own simulator is not running outside Coddy Studio"
        );
        second.stop().await;
        sim.set_state("UDID-1", "Booted");
        assert!(in_use(backend.discover().await), "the last stop forgets it");
        assert_eq!(
            sim.calls()
                .iter()
                .filter(|c| c.starts_with("shutdown"))
                .count(),
            1,
            "only the second launch shuts it down: {:?}",
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

    /// Issue #131: two simulators with the same name and OS are told apart in the catalog the
    /// Device picker shows, by the end of each UDID; a name of its own stays as it was.
    #[tokio::test]
    async fn simulators_of_the_same_name_are_told_apart_in_the_catalog() {
        let sim = FakeXcrun::new()
            .device(
                "6F1C0D2E-9B3A-4C55-8E21-0A7D44B13F2A",
                "iPhone 17",
                "iOS 27.0",
            )
            .device(
                "A0B1C2D3-E4F5-4A6B-9C8D-7E6F5A4B9C01",
                "iPhone 17",
                "iOS 27.0",
            )
            .device("UDID-3", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let (manager, _enabled) = ios_manager(&sim);
        manager.refresh_now().await;
        let names: Vec<String> = manager.catalog().borrow().platforms[0]
            .images
            .iter()
            .map(|image| image.display_name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "iPhone 17 · iOS 27.0 · 3F2A",
                "iPhone 17 · iOS 27.0 · 9C01",
                "iPhone 17 Pro · iOS 27.0",
            ]
        );
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn close_shuts_down_only_what_coddy_studio_booted() {
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
        // Booted by Coddy Studio, so not in use elsewhere; the other one is.
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
                "Touch input is off: Xcode 27.0 changed the simulator interfaces Coddy Studio uses. Simulators open as screenshots without touch input.".into()
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
