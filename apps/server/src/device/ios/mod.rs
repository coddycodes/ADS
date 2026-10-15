//! The iOS backend (macOS only): simulator discovery and lifecycle through `xcrun simctl`.
//! Simulators run view-only: `simctl` screenshots, no touch input, no video, no rotation.
pub(crate) mod guard;
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

use futures_util::{StreamExt as _, future::BoxFuture};
use tokio_util::sync::CancellationToken;

use self::{
    guard::{availability_for, xcode_status},
    simctl::{SIMULATOR_LABEL, SimDevice, SimState, SimctlError, SimctlRunner, SimulatorProcess},
};
use super::{
    DeviceError, DeviceOpError,
    backend::{
        DeviceBackend, ErasedVideoSource, JpegStream, LaunchFailure, PlatformDiscovery, Png,
        RawJpeg, RunningDevice,
    },
    catalog::{Availability, AvailabilityKind, DeviceState, Image, Orientation, Platform},
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
/// Why touch input is off until live mode exists.
const VIEW_ONLY_REASON: &str = "live input is not available yet";
pub(crate) const EXTERNAL_SHUTDOWN: &str = "Shutdown outside BiBCode";
pub(crate) const EXTERNAL_DELETE: &str = "Deleted outside BiBCode";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct IosBackendConfig {
    pub runner: SimctlRunner,
    pub boot_timeout: Duration,
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
        Self {
            runner: SimctlRunner {
                xcrun: bin.join("xcrun"),
                xcode_select: bin.join("xcode-select"),
                registry,
            },
            boot_timeout: DEFAULT_BOOT_TIMEOUT,
        }
    }
}

/// The Mac's iOS simulators.
pub(crate) struct IosBackend {
    runner: Arc<SimctlRunner>,
    boot_timeout: Duration,
    /// UDIDs BiBCode booted and has not shut down; only those are never `in_use_elsewhere`.
    booted: Arc<Mutex<HashSet<String>>>,
}

impl IosBackend {
    pub(crate) fn new(config: IosBackendConfig) -> Self {
        Self {
            runner: Arc::new(config.runner),
            boot_timeout: config.boot_timeout,
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
        let native = tokio::select! {
            () = cancel.cancelled() => Err(LaunchFailure::Cancelled),
            native = self.first_screenshot_size(udid) => native,
        };
        let native = match native {
            Ok(native) => native,
            Err(error) => {
                self.shut_down_own(udid).await;
                return Err(error);
            }
        };
        let process = self.runner.register_simulator(udid).await;
        Ok(Box::new(RunningSimulator::start(
            udid.to_owned(),
            native,
            self.runner.clone(),
            self.booted.clone(),
            process,
        )))
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
            let status = xcode_status(&self.runner).await;
            let devices = if matches!(status, guard::XcodeStatus::Ready { .. }) {
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
            let availability = availability_for(&status, &devices);
            if availability.kind != AvailabilityKind::Ready {
                tracing::debug!(kind = ?availability.kind, "ios simulators unavailable");
            }
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

/// A simulator BiBCode booted, seen through `simctl` screenshots.
struct RunningSimulator {
    shared: Arc<Shared>,
}

struct Shared {
    udid: String,
    native: (u32, u32),
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
        runner: Arc<SimctlRunner>,
        booted: Arc<Mutex<HashSet<String>>>,
        process: Option<SimulatorProcess>,
    ) -> Self {
        let shared = Arc::new(Shared {
            udid,
            native,
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

impl RunningDevice for RunningSimulator {
    fn native_size(&self) -> (u32, u32) {
        self.shared.native
    }

    fn can_rotate(&self) -> bool {
        false
    }

    /// Full-size `simctl` screenshots every [`SCREENSHOT_INTERVAL`]; `max_width` is not
    /// applied, since decoding and re-encoding each one would cost more than sending it.
    fn jpeg_stream(&self, _max_width: u32) -> BoxFuture<'_, Result<JpegStream, DeviceError>> {
        let shared = self.shared.clone();
        Box::pin(async move {
            let frames = futures_util::stream::unfold(
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

    /// A JPEG: `simctl` screenshots are PNGs only in live mode.
    fn png(&self, _max_width: u32) -> BoxFuture<'_, Result<Png, DeviceError>> {
        Box::pin(async move {
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
        Box::pin(async {
            Err(DeviceError::Protocol(
                "iOS simulators have no video in view-only mode".into(),
            ))
        })
    }

    fn input(
        &self,
        _events: Vec<DeviceInputEvent>,
        _orientation: Orientation,
    ) -> BoxFuture<'_, Result<(), DeviceOpError>> {
        Box::pin(async {
            Err(DeviceOpError::InvalidInput(format!(
                "Touch input is off: {VIEW_ONLY_REASON}."
            )))
        })
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
                "Touch input is off: live input is not available yet.".into()
            ))
        );
        let screenshot = manager.png(&id, 720).await.unwrap();
        assert_eq!(screenshot.mime_type, "image/jpeg");
        assert_eq!((screenshot.width, screenshot.height), (60, 130));
        assert_eq!(&screenshot.png[..2], &[0xFF, 0xD8]);
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
