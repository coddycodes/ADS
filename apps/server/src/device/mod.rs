//! Server-hosted Android device previews; see docs/architecture/overview.md#device-previews.
pub mod catalog;
pub(crate) mod emulator;
pub(crate) mod grpc;
pub(crate) mod sdk;
// Handle fields are consumed by the lifecycle, frame, and input tests that follow.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod testing;

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use self::{
    catalog::{
        Availability, AvailabilityKind, Avd, DeviceCatalog, DeviceSnapshot, DeviceState,
        Orientation,
    },
    emulator::{EmulatorLauncher, LaunchError, RunningEmulator},
    grpc::EmulatorClient,
    sdk::{AndroidSdk, SdkEnv},
};
use crate::diagnostics::ProcessAttributionRegistry;

/// How long a discovery result serves `refresh` before it reruns.
const DISCOVERY_TTL: Duration = Duration::from_secs(30);
/// How often a running emulator is checked for an exit nobody asked for.
const EXIT_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Stopped and failed devices the catalog keeps, newest first, so their reason stays visible.
const RETAINED_ENDED_DEVICES: usize = 4;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DeviceError {
    #[error("emulator gRPC: {0}")]
    Grpc(String),
    #[error("{0}")]
    Io(String),
    #[error("protocol: {0}")]
    Protocol(String),
}

/// Why a device operation was refused. Each maps to a typed contract error.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DeviceOpError {
    #[error("Device previews are off. Turn on Device previews in Settings to start a device.")]
    Disabled,
    #[error("{}", .0.message)]
    Sdk(Availability),
    #[error("{running} is already running. Stop it before starting another device.")]
    Limit { running: String },
    #[error("This device is no longer running. Start it again from the device list.")]
    NotFound,
    #[error("{}", not_ready_message(*.0))]
    NotReady(DeviceState),
    #[error(
        "No virtual device named \"{0}\" exists on the server. Choose one from the device list, or create it in Android Studio's Device Manager."
    )]
    UnknownAvd(String),
}

fn not_ready_message(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Starting | DeviceState::Booting => {
            "The device is still starting. Wait until it is ready, then try again."
        }
        DeviceState::Ready => "The device is not ready yet. Try again in a moment.",
        DeviceState::Stopping | DeviceState::Stopped => {
            "The device has stopped. Start it again from the device list."
        }
        DeviceState::Failed => {
            "The device failed. Check its reason in the device list, then start it again."
        }
    }
}

impl DeviceOpError {
    /// The typed RPC failure for this error.
    pub fn to_rpc_error(&self) -> serde_json::Value {
        let message = self.to_string();
        match self {
            Self::Disabled => serde_json::json!({
                "_tag": "DevicePreviewsDisabledError",
                "message": message,
            }),
            Self::Sdk(availability) => serde_json::json!({
                "_tag": "DeviceSdkUnavailableError",
                "kind": availability.kind,
                "message": message,
            }),
            Self::Limit { running } => serde_json::json!({
                "_tag": "DeviceLimitError",
                "running": running,
                "message": message,
            }),
            Self::NotFound | Self::UnknownAvd(_) => serde_json::json!({
                "_tag": "DeviceNotFoundError",
                "message": message,
            }),
            Self::NotReady(state) => serde_json::json!({
                "_tag": "DeviceNotReadyError",
                "state": state,
                "message": message,
            }),
        }
    }
}

pub(crate) struct DeviceManagerConfig {
    pub env: SdkEnv,
    /// Where emulators publish their gRPC discovery files (`emulator::default_discovery_dirs`).
    pub discovery_dirs: Vec<PathBuf>,
    pub registry: ProcessAttributionRegistry,
    /// The committed `enableDevicePreviews` setting.
    pub enabled: watch::Receiver<bool>,
    /// A fixed gRPC port for every launch; `None` picks a free one. Tests pin the fake
    /// controller's port.
    pub grpc_port: Option<u16>,
}

/// Owns every emulator of this environment and the catalog that describes them: at most one
/// running device, stopped when the setting turns off and on shutdown.
#[derive(Clone)]
pub struct DeviceManager {
    inner: Arc<Inner>,
}

struct Inner {
    catalog: watch::Sender<DeviceCatalog>,
    devices: Mutex<DeviceTable>,
    discovery: tokio::sync::Mutex<Option<Discovery>>,
    enabled: watch::Receiver<bool>,
    env: SdkEnv,
    discovery_dirs: Vec<PathBuf>,
    registry: ProcessAttributionRegistry,
    software_gpu: bool,
    grpc_port: Option<u16>,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

#[derive(Default)]
struct DeviceTable {
    slots: HashMap<String, Slot>,
    next_seq: u64,
}

struct Slot {
    snapshot: DeviceSnapshot,
    /// Set while the device is `Ready`; taken by whoever stops it.
    running: Option<RunningEmulator>,
    /// Cancels the launch and the exit watcher.
    cancel: CancellationToken,
    started: Instant,
    /// Open order, which decides which ended devices the catalog keeps.
    seq: u64,
}

#[derive(Clone)]
struct Discovery {
    at: Instant,
    availability: Availability,
    avds: Vec<Avd>,
    sdk: Option<AndroidSdk>,
}

fn ended(state: DeviceState) -> bool {
    matches!(state, DeviceState::Stopped | DeviceState::Failed)
}

impl DeviceManager {
    /// Starts the manager and the task that follows the setting: on, it discovers the SDK;
    /// off, it stops every device.
    pub(crate) fn start(config: DeviceManagerConfig) -> Self {
        let software_gpu =
            emulator::software_gpu_for(config.env.os, |key| config.env.vars.get(key).cloned());
        let (catalog, _) = watch::channel(DeviceCatalog {
            availability: Availability {
                kind: AvailabilityKind::SdkMissing,
                message: "Looking for the Android SDK on the server.".to_owned(),
            },
            avds: Vec::new(),
            devices: Vec::new(),
        });
        let inner = Arc::new(Inner {
            catalog,
            devices: Mutex::default(),
            discovery: tokio::sync::Mutex::new(None),
            enabled: config.enabled,
            env: config.env,
            discovery_dirs: config.discovery_dirs,
            registry: config.registry,
            software_gpu,
            grpc_port: config.grpc_port,
            shutdown: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        inner.tasks.spawn(inner.clone().follow_setting());
        Self { inner }
    }

    pub fn catalog(&self) -> watch::Receiver<DeviceCatalog> {
        self.inner.catalog.subscribe()
    }

    /// Reruns SDK discovery unless the last run is under 30 s old, and publishes it.
    pub async fn refresh(&self) {
        self.inner.discover(false).await;
    }

    /// Reruns SDK discovery now and publishes it.
    pub async fn refresh_now(&self) {
        self.inner.discover(true).await;
    }

    /// Starts `avd_name` and returns its device id at `Starting`; boot progress is published
    /// in the catalog.
    pub async fn open(&self, avd_name: &str) -> Result<String, DeviceOpError> {
        if !self.inner.accepting() {
            return Err(DeviceOpError::Disabled);
        }
        let mut found = self.inner.discover(false).await;
        if found.availability.kind != AvailabilityKind::Ready || !found.lists(avd_name) {
            found = self.inner.discover(true).await;
        }
        let sdk = match (found.availability.kind, found.sdk) {
            (AvailabilityKind::Ready, Some(sdk)) => sdk,
            _ => return Err(DeviceOpError::Sdk(found.availability)),
        };
        if !found.avds.iter().any(|avd| avd.name == avd_name) {
            return Err(DeviceOpError::UnknownAvd(avd_name.to_owned()));
        }
        let started = Instant::now();
        let device_id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        {
            let mut table = self.inner.table();
            // Checked under the lock the setting follower and shutdown take, so neither can
            // miss a device opened while they stop the others.
            if !self.inner.accepting() {
                return Err(DeviceOpError::Disabled);
            }
            if let Some(active) = table
                .slots
                .values()
                .find(|slot| !ended(slot.snapshot.state))
            {
                return Err(DeviceOpError::Limit {
                    running: active.snapshot.avd_name.clone(),
                });
            }
            let seq = table.next_seq;
            table.next_seq += 1;
            table.slots.insert(
                device_id.clone(),
                Slot {
                    snapshot: DeviceSnapshot {
                        device_id: device_id.clone(),
                        avd_name: avd_name.to_owned(),
                        state: DeviceState::Starting,
                        failure: None,
                        width: None,
                        height: None,
                        orientation: Orientation::Portrait,
                    },
                    running: None,
                    cancel: cancel.clone(),
                    started,
                    seq,
                },
            );
            self.inner.publish(&mut table);
        }
        record_lifecycle("start", started, None);
        let mut launcher = EmulatorLauncher::new(
            sdk,
            self.inner.discovery_dirs.clone(),
            self.inner.registry.clone(),
            self.inner.software_gpu,
        );
        launcher.grpc_port = self.inner.grpc_port;
        self.inner.tasks.spawn(self.inner.clone().launch(
            device_id.clone(),
            avd_name.to_owned(),
            launcher,
            cancel,
        ));
        Ok(device_id)
    }

    /// Waits until `device_id` leaves `Starting`/`Booting`: its snapshot once `Ready`,
    /// otherwise `NotReady` with the state it reached, or still had after `timeout`.
    pub async fn wait_ready(
        &self,
        device_id: &str,
        timeout: Duration,
    ) -> Result<DeviceSnapshot, DeviceOpError> {
        let find = |catalog: &DeviceCatalog| {
            catalog
                .devices
                .iter()
                .find(|device| device.device_id == device_id)
                .cloned()
        };
        let mut catalog = self.catalog();
        let _ = tokio::time::timeout(
            timeout,
            catalog.wait_for(|catalog| {
                find(catalog).is_none_or(|device| {
                    !matches!(device.state, DeviceState::Starting | DeviceState::Booting)
                })
            }),
        )
        .await;
        let current = find(&catalog.borrow());
        match current {
            None => Err(DeviceOpError::NotFound),
            Some(device) if device.state == DeviceState::Ready => Ok(device),
            Some(device) => Err(DeviceOpError::NotReady(device.state)),
        }
    }

    /// Stops `device_id` and returns once it has ended; closing an ended device succeeds.
    pub async fn close(&self, device_id: &str) -> Result<(), DeviceOpError> {
        self.inner.close(device_id).await
    }

    /// The gRPC client, adb serial, display size, and orientation of a `Ready` device.
    // Consumed by the frame and input producers (Task 6).
    #[allow(dead_code)]
    pub(crate) fn client(
        &self,
        device_id: &str,
    ) -> Result<(EmulatorClient, String, (u32, u32), Orientation), DeviceOpError> {
        let table = self.inner.table();
        let slot = table.slots.get(device_id).ok_or(DeviceOpError::NotFound)?;
        match (&slot.running, slot.snapshot.state) {
            (Some(running), DeviceState::Ready) => Ok((
                running.client.clone(),
                running.serial.clone(),
                (
                    slot.snapshot.width.unwrap_or(running.width),
                    slot.snapshot.height.unwrap_or(running.height),
                ),
                slot.snapshot.orientation,
            )),
            (_, state) => Err(DeviceOpError::NotReady(state)),
        }
    }

    /// Records the orientation the emulator reported; never what a rotate command asked for.
    // Consumed by the frame and input producers (Task 6).
    #[allow(dead_code)]
    pub(crate) fn set_orientation(&self, device_id: &str, orientation: Orientation) {
        let mut table = self.inner.table();
        let Some(slot) = table.slots.get_mut(device_id) else {
            return;
        };
        if slot.snapshot.orientation != orientation {
            slot.snapshot.orientation = orientation;
            self.inner.publish(&mut table);
        }
    }

    /// Refuses new devices, stops every device, and waits for the manager's tasks.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        self.inner.close_all().await;
        self.inner.tasks.close();
        self.inner.tasks.wait().await;
    }

    #[cfg(test)]
    pub(crate) fn pid(&self, device_id: &str) -> Option<u32> {
        let table = self.inner.table();
        let running = table.slots.get(device_id)?.running.as_ref()?;
        Some(running.helper.pid)
    }
}

impl Discovery {
    fn lists(&self, avd_name: &str) -> bool {
        self.avds.iter().any(|avd| avd.name == avd_name)
    }
}

impl Inner {
    fn table(&self) -> MutexGuard<'_, DeviceTable> {
        self.devices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn accepting(&self) -> bool {
        *self.enabled.borrow() && !self.shutdown.is_cancelled()
    }

    /// Publishes the devices, sorted by id, after dropping all but the newest ended ones.
    fn publish(&self, table: &mut DeviceTable) {
        let mut ended_slots: Vec<(u64, String)> = table
            .slots
            .iter()
            .filter(|(_, slot)| ended(slot.snapshot.state))
            .map(|(id, slot)| (slot.seq, id.clone()))
            .collect();
        ended_slots.sort_unstable_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        for (_, id) in ended_slots.into_iter().skip(RETAINED_ENDED_DEVICES) {
            table.slots.remove(&id);
        }
        let mut devices: Vec<DeviceSnapshot> = table
            .slots
            .values()
            .map(|slot| slot.snapshot.clone())
            .collect();
        devices.sort_unstable_by(|left, right| left.device_id.cmp(&right.device_id));
        self.catalog.send_if_modified(|catalog| {
            let changed = catalog.devices != devices;
            if changed {
                catalog.devices = devices;
            }
            changed
        });
    }

    async fn discover(&self, force: bool) -> Discovery {
        let mut cached = self.discovery.lock().await;
        if !force
            && let Some(discovery) = cached.as_ref()
            && discovery.at.elapsed() < DISCOVERY_TTL
        {
            return discovery.clone();
        }
        let (availability, avds, sdk) = sdk::discover(&self.env).await;
        if availability.kind != AvailabilityKind::Ready {
            tracing::debug!(kind = ?availability.kind, "android sdk unavailable");
        }
        self.catalog.send_if_modified(|catalog| {
            let changed = catalog.availability != availability || catalog.avds != avds;
            if changed {
                catalog.availability = availability.clone();
                catalog.avds = avds.clone();
            }
            changed
        });
        let discovery = Discovery {
            at: Instant::now(),
            availability,
            avds,
            sdk,
        };
        *cached = Some(discovery.clone());
        discovery
    }

    fn update(&self, device_id: &str, change: impl FnOnce(&mut Slot)) {
        let mut table = self.table();
        if let Some(slot) = table.slots.get_mut(device_id) {
            change(slot);
            self.publish(&mut table);
        }
    }

    /// Ends a device in `state`.
    fn finish(&self, device_id: &str, state: DeviceState, failure: Option<String>) {
        self.update(device_id, |slot| {
            slot.snapshot.state = state;
            slot.snapshot.failure = failure;
        });
    }

    async fn launch(
        self: Arc<Self>,
        device_id: String,
        avd: String,
        launcher: EmulatorLauncher,
        cancel: CancellationToken,
    ) {
        let phases = self.clone();
        let phase_device = device_id.clone();
        let on_phase = move |phase: DeviceState| {
            phases.update(&phase_device, |slot| {
                if slot.snapshot.state == DeviceState::Starting {
                    slot.snapshot.state = phase;
                }
            });
        };
        let result = launcher.launch(&avd, on_phase, &cancel).await;
        let started = self.started(&device_id);
        let error = match result {
            Ok(running) => {
                let unwanted = {
                    let mut table = self.table();
                    match table.slots.get_mut(&device_id) {
                        Some(slot)
                            if matches!(
                                slot.snapshot.state,
                                DeviceState::Starting | DeviceState::Booting
                            ) =>
                        {
                            slot.snapshot.state = DeviceState::Ready;
                            slot.snapshot.width = Some(running.width);
                            slot.snapshot.height = Some(running.height);
                            slot.running = Some(running);
                            self.publish(&mut table);
                            None
                        }
                        _ => Some(running),
                    }
                };
                match unwanted {
                    None => {
                        record_lifecycle("ready", started, None);
                        self.tasks
                            .spawn(self.clone().watch_exit(device_id, cancel.clone()));
                    }
                    // Closed while it booted: whoever closed it waits for this stop.
                    Some(running) => {
                        emulator::stop(running).await;
                        self.finish(&device_id, DeviceState::Stopped, None);
                        record_lifecycle("stop", started, None);
                    }
                }
                return;
            }
            Err(error) => error,
        };
        // `launch` already stopped the emulator and removed its discovery file.
        if matches!(error, LaunchError::Cancelled) || cancel.is_cancelled() {
            self.finish(&device_id, DeviceState::Stopped, None);
            record_lifecycle("stop", started, None);
            return;
        }
        let (failure_kind, failure) = launch_failure(&error);
        self.finish(&device_id, DeviceState::Failed, Some(failure));
        record_lifecycle("fail", started, Some(failure_kind));
    }

    /// Polls a `Ready` device's emulator; an exit nobody asked for fails the device.
    async fn watch_exit(self: Arc<Self>, device_id: String, cancel: CancellationToken) {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(EXIT_POLL_INTERVAL) => {}
            }
            let (running, status, started) = {
                let mut table = self.table();
                let Some(slot) = table.slots.get_mut(&device_id) else {
                    return;
                };
                let Some(status) = slot.running.as_ref().map(|running| running.helper.exited())
                else {
                    return;
                };
                let Some(status) = status else {
                    continue;
                };
                let running = slot.running.take().expect("checked above");
                (running, status, slot.started)
            };
            // Before `stop`, which marks the exit as requested.
            running
                .helper
                .report_unrequested_exit(emulator::HELPER_LABEL, &status);
            emulator::stop(running).await;
            self.finish(
                &device_id,
                DeviceState::Failed,
                Some(format!("exited: {status}")),
            );
            record_lifecycle("fail", started, Some("exited"));
            return;
        }
    }

    fn started(&self, device_id: &str) -> Instant {
        self.table()
            .slots
            .get(device_id)
            .map_or_else(Instant::now, |slot| slot.started)
    }

    async fn close(&self, device_id: &str) -> Result<(), DeviceOpError> {
        let (running, started) = {
            let mut table = self.table();
            let slot = table
                .slots
                .get_mut(device_id)
                .ok_or(DeviceOpError::NotFound)?;
            match slot.snapshot.state {
                DeviceState::Stopped | DeviceState::Failed => return Ok(()),
                DeviceState::Stopping => (None, slot.started),
                DeviceState::Starting | DeviceState::Booting | DeviceState::Ready => {
                    slot.snapshot.state = DeviceState::Stopping;
                    slot.cancel.cancel();
                    let taken = (slot.running.take(), slot.started);
                    self.publish(&mut table);
                    taken
                }
            }
        };
        match running {
            Some(running) => {
                emulator::stop(running).await;
                self.finish(device_id, DeviceState::Stopped, None);
                record_lifecycle("stop", started, None);
            }
            // The launch, the exit watcher, or another close is stopping it.
            None => self.wait_ended(device_id).await,
        }
        Ok(())
    }

    async fn wait_ended(&self, device_id: &str) {
        let mut catalog = self.catalog.subscribe();
        let _ = catalog
            .wait_for(|catalog| {
                !catalog
                    .devices
                    .iter()
                    .any(|device| device.device_id == device_id && !ended(device.state))
            })
            .await;
    }

    async fn close_all(&self) {
        let ids: Vec<String> = self
            .table()
            .slots
            .iter()
            .filter(|(_, slot)| !ended(slot.snapshot.state))
            .map(|(id, _)| id.clone())
            .collect();
        futures_util::future::join_all(ids.iter().map(|id| async move {
            let _ = self.close(id).await;
        }))
        .await;
    }

    async fn follow_setting(self: Arc<Self>) {
        let mut enabled = self.enabled.clone();
        loop {
            if *enabled.borrow_and_update() {
                self.discover(false).await;
            } else {
                self.close_all().await;
            }
            tokio::select! {
                () = self.shutdown.cancelled() => return,
                changed = enabled.changed() => if changed.is_err() {
                    return;
                },
            }
        }
    }
}

/// The profiler failure kind and the catalog's failure reason for a failed launch.
fn launch_failure(error: &LaunchError) -> (&'static str, String) {
    match error {
        LaunchError::Exited(message) => ("exited", format!("exited: {message}")),
        LaunchError::BootTimeout => (
            "bootTimeout",
            "did not finish booting within 3 minutes".to_owned(),
        ),
        LaunchError::Spawn(message) => ("spawn", format!("could not start: {message}")),
        LaunchError::Unresponsive(message) => (
            "unresponsive",
            format!("did not report its display: {message}"),
        ),
        LaunchError::Cancelled => ("cancelled", "cancelled".to_owned()),
    }
}

/// A `device.lifecycle` profiler event; it never names the AVD or its paths.
fn record_lifecycle(phase: &'static str, started: Instant, failure_kind: Option<&'static str>) {
    if !crate::diagnostics::profiler::is_enabled() {
        return;
    }
    crate::diagnostics::profiler::emit(
        "device.lifecycle",
        serde_json::json!({
            "phase": phase,
            "durationMs": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "failureKind": failure_kind,
        }),
    );
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::device::testing::{ManagerFixture, kill_process, process_alive};

    #[tokio::test]
    async fn open_publishes_states_and_enforces_one_device() {
        let fixture = ManagerFixture::ready(&["Pixel", "Tablet"]).await;
        let id = fixture.manager.open("Pixel").await.unwrap();
        let ready = fixture.wait_state(&id, DeviceState::Ready).await;
        assert_eq!((ready.width, ready.height), (Some(1080), Some(2400)));
        assert!(matches!(
            fixture.manager.open("Tablet").await,
            Err(DeviceOpError::Limit { running }) if running == "Pixel"
        ));
        fixture.manager.close(&id).await.unwrap();
        fixture.wait_state(&id, DeviceState::Stopped).await;
        assert!(fixture.manager.open("Tablet").await.is_ok());
        fixture.manager.shutdown().await;
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
        assert!(matches!(
            fixture.manager.open("Pixel").await,
            Err(DeviceOpError::Disabled)
        ));
    }

    #[tokio::test]
    async fn crash_marks_failed_and_frees_the_slot() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        let id = fixture.manager.open("Pixel").await.unwrap();
        fixture.wait_state(&id, DeviceState::Ready).await;
        let pid = fixture.pid_of(&id);
        kill_process(pid);
        let snapshot = fixture.wait_state(&id, DeviceState::Failed).await;
        assert!(snapshot.failure.unwrap().contains("exited"));
        assert!(!process_alive(pid));
        assert!(fixture.manager.open("Pixel").await.is_ok());
        fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_reaps_running_emulators() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        let id = fixture.manager.open("Pixel").await.unwrap();
        fixture.wait_state(&id, DeviceState::Ready).await;
        let pid = fixture.pid_of(&id);
        fixture.manager.shutdown().await;
        assert!(!process_alive(pid));
        assert!(matches!(
            fixture.manager.open("Pixel").await,
            Err(DeviceOpError::Disabled)
        ));
    }

    #[tokio::test]
    async fn unknown_avd_is_rejected_before_spawning() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        assert!(matches!(
            fixture.manager.open("Nope").await,
            Err(DeviceOpError::UnknownAvd(_))
        ));
        assert!(fixture.sdk.launched_pids().is_empty());
    }

    #[tokio::test]
    async fn refresh_is_cached_for_30_seconds() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        fixture.manager.refresh().await;
        fixture.manager.refresh().await;
        assert_eq!(fixture.sdk.list_avds_calls(), 1);
        let catalog = fixture.manager.catalog().borrow().clone();
        assert_eq!(catalog.availability.kind, AvailabilityKind::Ready);
        assert_eq!(catalog.avds[0].name, "Pixel");
        fixture.manager.refresh_now().await;
        assert_eq!(fixture.sdk.list_avds_calls(), 2);
    }

    #[tokio::test]
    async fn closing_a_booting_device_stops_its_launch() {
        let fixture = ManagerFixture::with_sdk(|sdk| sdk.avds(&["Pixel"]).never_boots()).await;
        let id = fixture.manager.open("Pixel").await.unwrap();
        fixture.wait_state(&id, DeviceState::Booting).await;
        fixture.manager.close(&id).await.unwrap();
        let state = fixture.manager.catalog().borrow().devices[0].state;
        assert_eq!(state, DeviceState::Stopped);
        let launched = fixture.sdk.launched_pids();
        assert_eq!(launched.len(), 1);
        assert!(launched.iter().all(|pid| !process_alive(*pid)));
        assert!(fixture.manager.open("Pixel").await.is_ok());
        fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn wait_ready_returns_a_launch_failure_without_waiting() {
        let fixture = ManagerFixture::with_sdk(|sdk| {
            sdk.avds(&["Pixel"])
                .launch_stub(crate::device::testing::LaunchStub::ExitsWith(
                    1,
                    "PANIC: Missing emulator engine",
                ))
        })
        .await;
        let id = fixture.manager.open("Pixel").await.unwrap();
        let started = Instant::now();
        let result = fixture
            .manager
            .wait_ready(&id, emulator::BOOT_TIMEOUT)
            .await;
        assert!(
            matches!(result, Err(DeviceOpError::NotReady(DeviceState::Failed))),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        let snapshot = fixture.wait_state(&id, DeviceState::Failed).await;
        assert!(
            snapshot
                .failure
                .unwrap()
                .contains("Missing emulator engine")
        );
    }

    #[tokio::test]
    async fn ready_device_exposes_its_client_and_orientation() {
        let fixture = ManagerFixture::ready(&["Pixel"]).await;
        let id = fixture.manager.open("Pixel").await.unwrap();
        assert!(matches!(
            fixture.manager.client(&id),
            Err(DeviceOpError::NotReady(_))
        ));
        let ready = fixture
            .manager
            .wait_ready(&id, emulator::BOOT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(ready.state, DeviceState::Ready);
        fixture.wait_state(&id, DeviceState::Ready).await;
        let (_, serial, size, orientation) = fixture.manager.client(&id).unwrap();
        assert!(serial.starts_with("emulator-"));
        assert_eq!(size, (1080, 2400));
        assert_eq!(orientation, Orientation::Portrait);
        fixture.manager.set_orientation(&id, Orientation::Landscape);
        assert_eq!(
            fixture.manager.catalog().borrow().devices[0].orientation,
            Orientation::Landscape
        );
        assert!(matches!(
            fixture.manager.client("missing"),
            Err(DeviceOpError::NotFound)
        ));
        fixture.manager.shutdown().await;
    }

    #[test]
    fn errors_map_to_contract_tags() {
        assert_eq!(
            DeviceOpError::Limit {
                running: "Pixel".into()
            }
            .to_rpc_error(),
            serde_json::json!({
                "_tag": "DeviceLimitError",
                "running": "Pixel",
                "message": "Pixel is already running. Stop it before starting another device.",
            })
        );
        let sdk = DeviceOpError::Sdk(Availability {
            kind: AvailabilityKind::NoAvds,
            message: "Create a virtual device.".into(),
        })
        .to_rpc_error();
        assert_eq!(sdk["_tag"], "DeviceSdkUnavailableError");
        assert_eq!(sdk["kind"], "noAvds");
        assert_eq!(sdk["message"], "Create a virtual device.");
        let unknown = DeviceOpError::UnknownAvd("Nope".into()).to_rpc_error();
        assert_eq!(unknown["_tag"], "DeviceNotFoundError");
        assert!(unknown["message"].as_str().unwrap().contains("\"Nope\""));
        let not_ready = DeviceOpError::NotReady(DeviceState::Booting).to_rpc_error();
        assert_eq!(not_ready["_tag"], "DeviceNotReadyError");
        assert_eq!(not_ready["state"], "booting");
        assert_eq!(
            DeviceOpError::Disabled.to_rpc_error()["_tag"],
            "DevicePreviewsDisabledError"
        );
        assert_eq!(
            DeviceOpError::NotFound.to_rpc_error()["_tag"],
            "DeviceNotFoundError"
        );
    }

    #[test]
    fn launch_failures_name_their_reason() {
        assert_eq!(
            launch_failure(&LaunchError::Exited("boom".into())),
            ("exited", "exited: boom".to_owned())
        );
        assert_eq!(
            launch_failure(&LaunchError::BootTimeout),
            (
                "bootTimeout",
                "did not finish booting within 3 minutes".to_owned()
            )
        );
        assert_eq!(launch_failure(&LaunchError::Spawn("x".into())).0, "spawn");
        assert_eq!(
            launch_failure(&LaunchError::Unresponsive("x".into())).0,
            "unresponsive"
        );
    }
}
