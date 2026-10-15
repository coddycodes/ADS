//! A scripted [`DeviceBackend`] for the platform-neutral manager's tests.

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::future::BoxFuture;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{
    DeviceError, DeviceManager, DeviceManagerConfig, DeviceOpError,
    backend::{
        DeviceBackend, ErasedVideoSource, JpegStream, LaunchFailure, PlatformDiscovery, Png,
        RunningDevice,
    },
    catalog::{
        Availability, AvailabilityKind, DeviceSnapshot, DeviceState, Image, Orientation, Platform,
    },
    input::DeviceInputEvent,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The native size every fake device reports.
pub(crate) const FAKE_SIZE: (u32, u32) = (1170, 2532);

/// One platform's scripted images, launches, and counters.
pub(crate) struct FakeBackend {
    platform: Platform,
    images: Vec<String>,
    /// Images `check_available` refuses.
    busy: Mutex<HashSet<String>>,
    /// Images whose launch fails late with `LaunchFailure::Busy`.
    busy_on_launch: Mutex<HashSet<String>>,
    never_boots: AtomicBool,
    can_rotate: AtomicBool,
    /// Every launched device, in launch order.
    launched: Mutex<Vec<Arc<FakeDeviceState>>>,
    stops: Arc<AtomicUsize>,
    discover_calls: AtomicUsize,
    check_calls: AtomicUsize,
}

struct FakeDeviceState {
    exit: Mutex<Option<String>>,
}

impl FakeBackend {
    pub(crate) fn new(platform: Platform, images: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            platform,
            images: images.iter().map(|image| (*image).to_owned()).collect(),
            busy: Mutex::default(),
            busy_on_launch: Mutex::default(),
            never_boots: AtomicBool::new(false),
            can_rotate: AtomicBool::new(true),
            launched: Mutex::default(),
            stops: Arc::default(),
            discover_calls: AtomicUsize::new(0),
            check_calls: AtomicUsize::new(0),
        })
    }

    /// `image` is in use outside BiBCode: `check_available` refuses it.
    pub(crate) fn busy(self: &Arc<Self>, image: &str) {
        lock(&self.busy).insert(image.to_owned());
    }

    /// `image` passes `check_available` but its launch finds it in use.
    pub(crate) fn busy_on_launch(self: &Arc<Self>, image: &str) {
        lock(&self.busy_on_launch).insert(image.to_owned());
    }

    /// Launches report `Booting` and wait for their cancel.
    pub(crate) fn never_boots(self: &Arc<Self>) {
        self.never_boots.store(true, Ordering::SeqCst);
    }

    /// Launched devices report that they cannot rotate.
    pub(crate) fn without_rotation(self: &Arc<Self>) {
        self.can_rotate.store(false, Ordering::SeqCst);
    }

    /// Makes the `device_index`th launched device report an exit nobody asked for.
    pub(crate) fn exit_running(&self, device_index: usize, reason: &str) {
        let launched = lock(&self.launched);
        *lock(&launched[device_index].exit) = Some(reason.to_owned());
    }

    /// How many running devices were stopped.
    pub(crate) fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }

    pub(crate) fn discover_calls(&self) -> usize {
        self.discover_calls.load(Ordering::SeqCst)
    }

    /// How many times `check_available` ran.
    pub(crate) fn check_calls(&self) -> usize {
        self.check_calls.load(Ordering::SeqCst)
    }

    /// How many launches started.
    pub(crate) fn launches(&self) -> usize {
        lock(&self.launched).len()
    }
}

impl DeviceBackend for FakeBackend {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn looking(&self) -> Availability {
        Availability {
            kind: AvailabilityKind::SdkMissing,
            message: "Looking for fake devices.".to_owned(),
        }
    }

    fn discover(&self) -> BoxFuture<'_, PlatformDiscovery> {
        Box::pin(async move {
            self.discover_calls.fetch_add(1, Ordering::SeqCst);
            let availability = if self.images.is_empty() {
                Availability {
                    kind: AvailabilityKind::NoAvds,
                    message: "No fake devices exist.".to_owned(),
                }
            } else {
                Availability {
                    kind: AvailabilityKind::Ready,
                    message: String::new(),
                }
            };
            PlatformDiscovery {
                availability,
                images: self
                    .images
                    .iter()
                    .map(|id| Image {
                        id: id.clone(),
                        display_name: format!("iPhone {id}"),
                        in_use_elsewhere: lock(&self.busy).contains(id),
                    })
                    .collect(),
            }
        })
    }

    fn check_available<'a>(
        &'a self,
        image_id: &'a str,
    ) -> BoxFuture<'a, Result<(), LaunchFailure>> {
        Box::pin(async move {
            self.check_calls.fetch_add(1, Ordering::SeqCst);
            if lock(&self.busy).contains(image_id) {
                Err(LaunchFailure::Busy)
            } else {
                Ok(())
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
            let state = Arc::new(FakeDeviceState {
                exit: Mutex::new(None),
            });
            lock(&self.launched).push(state.clone());
            if lock(&self.busy_on_launch).contains(image_id) {
                return Err(LaunchFailure::Busy);
            }
            on_phase(DeviceState::Booting);
            if self.never_boots.load(Ordering::SeqCst) {
                cancel.cancelled().await;
                return Err(LaunchFailure::Cancelled);
            }
            Ok(Box::new(FakeRunning {
                state,
                can_rotate: self.can_rotate.load(Ordering::SeqCst),
                stops: self.stops.clone(),
                stopped: AtomicBool::new(false),
            }) as Box<dyn RunningDevice>)
        })
    }
}

struct FakeRunning {
    state: Arc<FakeDeviceState>,
    can_rotate: bool,
    stops: Arc<AtomicUsize>,
    stopped: AtomicBool,
}

impl FakeRunning {
    fn record_stop(&self) {
        if !self.stopped.swap(true, Ordering::SeqCst) {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
    }
}

fn unsupported() -> DeviceError {
    DeviceError::Io("the fake backend has no screen".to_owned())
}

impl RunningDevice for FakeRunning {
    fn native_size(&self) -> (u32, u32) {
        FAKE_SIZE
    }

    fn can_rotate(&self) -> bool {
        self.can_rotate
    }

    fn jpeg_stream(&self, _max_width: u32) -> BoxFuture<'_, Result<JpegStream, DeviceError>> {
        Box::pin(async { Err(unsupported()) })
    }

    fn png(&self, _max_width: u32) -> BoxFuture<'_, Result<Png, DeviceError>> {
        Box::pin(async { Err(unsupported()) })
    }

    fn video(&self) -> BoxFuture<'_, Result<Box<dyn ErasedVideoSource>, DeviceError>> {
        Box::pin(async { Err(unsupported()) })
    }

    fn input(
        &self,
        _events: Vec<DeviceInputEvent>,
        _orientation: Orientation,
    ) -> BoxFuture<'_, Result<(), DeviceOpError>> {
        Box::pin(async { Ok(()) })
    }

    fn rotate<'a>(
        &'a self,
        to: Orientation,
        report: &'a (dyn Fn(Orientation, u32, u32) + Send + Sync),
    ) -> BoxFuture<'a, Result<(), DeviceOpError>> {
        Box::pin(async move {
            let (width, height) = super::input::oriented_size(FAKE_SIZE.0, FAKE_SIZE.1, to);
            report(to, width, height);
            Ok(())
        })
    }

    fn exited(&self) -> Option<String> {
        lock(&self.state.exit).clone()
    }

    fn report_unrequested_exit(&self, _reason: &str) {}

    fn stop(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.record_stop();
        Box::pin(async {})
    }

    fn stop_shared(&self) -> BoxFuture<'_, ()> {
        self.record_stop();
        Box::pin(async {})
    }
}

/// A manager over `backends` with device previews on.
pub(crate) fn fake_manager(
    backends: Vec<Arc<FakeBackend>>,
) -> (DeviceManager, watch::Sender<bool>) {
    let (enabled, signal) = watch::channel(true);
    let manager = DeviceManager::start(DeviceManagerConfig {
        backends: backends
            .into_iter()
            .map(|backend| backend as Arc<dyn DeviceBackend>)
            .collect(),
        enabled: signal,
    });
    (manager, enabled)
}

/// Waits up to 20 s for `device_id` to reach `state` and returns its snapshot.
pub(crate) async fn wait_state(
    manager: &DeviceManager,
    device_id: &str,
    state: DeviceState,
) -> DeviceSnapshot {
    let mut catalog = manager.catalog();
    tokio::time::timeout(
        Duration::from_secs(20),
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
            manager.catalog().borrow().devices
        )
    })
    .expect("catalog open")
    .devices
    .iter()
    .find(|device| device.device_id == device_id)
    .cloned()
    .expect("device in catalog")
}
