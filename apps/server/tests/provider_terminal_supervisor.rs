#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
// Windows compile-checks shared observer fixtures whose integration tests are Unix-only.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use bibcode_server::{
    ServerConfig,
    activity::{
        ActivityCapabilities, ActivityHistoryRecovery, ActivityLifecycle, ActivityProjection,
        ActivityRecordKind, ActivityRepository, ActivityScopeRef, AgentActivityController,
        AgentActivityDisableReport, AgentActivitySource,
    },
    diagnostics::{ProcessAttributionRegistry, ProcessIdentity},
    persistence::{Database, StorageInstanceId, run_migrations},
    production::{
        agent_activity::{AgentActivitySettingsHandler, AgentActivityTransitionReport},
        control::NativeServerControl,
        server_terminal::ProductionServerControl,
    },
    provider_terminal::{
        CachedClaudeCapabilityProbe, CachedCodexCapabilityProbe, CachedOpenCodeCapabilityProbe,
        ClaudeAdditiveHookAttestor, ClaudeCapabilities, ClaudeCapabilityProbeRunner,
        ClaudeExecutablePinner, ClaudeProbeOutput, ClaudeTerminalObserverFactory,
        CodexCapabilityProbeRunner, CodexHelperLaunch, CodexHelperLauncher, CodexHelperProcess,
        CodexProbeOutput, CodexRemoteClient, CodexRemoteClientFactory,
        CodexTerminalObserverFactory, OpenCodeCapabilityProbeRunner, OpenCodeEventStream,
        OpenCodeHelperLaunch, OpenCodeHelperLauncher, OpenCodeHelperProcess, OpenCodeHelperReady,
        OpenCodeProbeOutput, OpenCodeRemoteClient, OpenCodeRemoteClientFactory,
        OpenCodeTerminalObserverFactory, PreparedTerminalLaunch, PreparedTerminalObserver,
        ProviderSettingsInventoryAuthority, ProviderTerminalActivitySupervisor,
        ProviderTerminalInventory, ProviderTerminalInventoryAuthority,
        ProviderTerminalObserverFactories, ProviderTerminalObserverFactory,
        ProviderTerminalObserverFactoryInput, TerminalAgentActivityTransition,
        TerminalGenerationActivityPublisher, TerminalLaunchPreparation,
        TerminalLaunchPreparationInput, TerminalLaunchPreparer, TerminalObserverCancellationReason,
        TerminalObserverGeneration, TerminalObserverGenerationLease, TerminalObserverWorkerContext,
        TerminalObserverWorkerSpawnError,
    },
    server_settings::{ProviderInstanceState, ProviderSettingsState, ProviderSettingsStore},
    terminal::{
        ProviderTerminalActivityLaunch, PtyBackend, PtyExit, PtyProcess, PtySpawnInput,
        TerminalLaunchCommand, TerminalManager, TerminalManagerOptions, TerminalOpenInput,
        TerminalStatus,
    },
};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const CLAUDE_HOOK_FIXTURE: &str =
    include_str!("fixtures/provider-terminal/claude-hook-handshake.json");
const OPENCODE_ATTACH_FIXTURE: &str =
    include_str!("fixtures/provider-terminal/opencode-attach-handshake.json");

#[cfg(unix)]
fn assert_empty_retired_generation(directory: &std::path::Path, context: &str) {
    assert!(
        directory.is_dir(),
        "{context}: retired directory is preserved"
    );
    assert_eq!(
        std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("{context}: read retired directory: {error}"))
            .count(),
        0,
        "{context}: retired directory contains no owned artifacts or marker"
    );
}

#[cfg(unix)]
fn assert_runtime_has_only_empty_retired_generations(
    runtime: &std::path::Path,
    expected_count: usize,
    context: &str,
) {
    let entries = std::fs::read_dir(runtime)
        .unwrap_or_else(|error| panic!("{context}: read runtime directory: {error}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("{context}: read runtime entry: {error}"))
                .path()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        expected_count,
        "{context}: bounded retired directory count"
    );
    for entry in entries {
        assert_empty_retired_generation(&entry, context);
    }
}

#[derive(Clone, Default)]
struct TraceCapture(Arc<Mutex<Vec<u8>>>);

struct TraceCaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for TraceCaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("trace capture")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for TraceCapture {
    type Writer = TraceCaptureWriter;

    fn make_writer(&'writer self) -> Self::Writer {
        TraceCaptureWriter(self.0.clone())
    }
}

impl TraceCapture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("trace capture").clone())
            .expect("trace capture UTF-8")
    }
}

#[derive(Debug)]
struct RecordingProcess {
    pid: u32,
    identity: ProcessIdentity,
    output: broadcast::Sender<String>,
    exit: watch::Sender<Option<PtyExit>>,
    killed: AtomicBool,
}

impl RecordingProcess {
    fn new(pid: u32) -> Self {
        static NEXT_SYNTHETIC_PROCESS_GENERATION: AtomicU64 = AtomicU64::new(1);
        assert_ne!(pid, 0, "synthetic PTY PID must be nonzero");
        let generation = NEXT_SYNTHETIC_PROCESS_GENERATION.fetch_add(1, Ordering::Relaxed);
        assert_ne!(
            generation, 0,
            "synthetic PTY process generation wrapped to zero"
        );
        let (output, _) = broadcast::channel(8);
        let (exit, _) = watch::channel(None);
        Self {
            pid,
            identity: ProcessIdentity {
                pid,
                started_at: generation,
            },
            output,
            exit,
            killed: AtomicBool::new(false),
        }
    }

    fn emit(&self, value: &str) {
        let _ = self.output.send(value.to_owned());
    }

    fn exit(&self, exit_code: i32) {
        self.exit.send_replace(Some(PtyExit {
            exit_code: Some(exit_code),
            signal: None,
        }));
    }
}

impl PtyProcess for RecordingProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn process_identity(&self) -> Option<ProcessIdentity> {
        Some(self.identity)
    }

    fn write(&self, _data: &str) -> Result<(), String> {
        Ok(())
    }

    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), String> {
        Ok(())
    }

    fn kill(&self) -> Result<(), String> {
        self.killed.store(true, Ordering::Release);
        self.exit.send_replace(Some(PtyExit {
            exit_code: None,
            signal: None,
        }));
        Ok(())
    }

    fn subscribe_output(&self) -> broadcast::Receiver<String> {
        self.output.subscribe()
    }

    fn subscribe_exit(&self) -> watch::Receiver<Option<PtyExit>> {
        self.exit.subscribe()
    }
}

#[test]
fn synthetic_pty_identities_are_nonzero_and_distinguish_pid_generations() {
    let first = RecordingProcess::new(41);
    let replacement = RecordingProcess::new(41);

    assert_ne!(first.identity.started_at, 0);
    assert_ne!(replacement.identity.started_at, 0);
    assert_eq!(first.identity.pid, replacement.identity.pid);
    assert!(
        replacement.identity.started_at > first.identity.started_at,
        "a replacement using the same synthetic PID needs a later stable generation"
    );
    assert_ne!(first.identity, replacement.identity);
}

#[derive(Debug)]
struct RecordingBackend {
    events: Arc<Mutex<Vec<&'static str>>>,
    spawns: Mutex<Vec<PtySpawnInput>>,
    processes: Mutex<Vec<Arc<RecordingProcess>>>,
    failures: Mutex<VecDeque<String>>,
}

#[derive(Debug)]
struct BlockingBackend {
    process: Arc<RecordingProcess>,
    spawn_started: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    spawn_release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PtyBackend for BlockingBackend {
    fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        if let Some(spawn_started) = self
            .spawn_started
            .lock()
            .expect("spawn started lock")
            .take()
        {
            spawn_started.send(()).expect("spawn started receiver");
        }
        // Let the runtime hand off this worker so close and its timer can run.
        tokio::task::block_in_place(|| {
            self.spawn_release
                .lock()
                .expect("spawn release lock")
                // This is only a deadlock guard. The 250 ms observer wait in the
                // lifecycle test below owns the behavioral timing assertion, so a
                // loaded CI runner must not make this blocking helper fail first.
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("spawn release");
        });
        Ok(self.process.clone())
    }
}

#[derive(Debug)]
struct LaunchBarrier {
    started: tokio::sync::Notify,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl LaunchBarrier {
    fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let (release, receiver) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                started: tokio::sync::Notify::new(),
                release: Mutex::new(receiver),
            }),
            release,
        )
    }

    fn block(&self) {
        self.started.notify_one();
        self.release
            .lock()
            .expect("launch barrier lock")
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("release launch barrier");
    }
}

#[derive(Debug)]
struct FirstSpawnBarrierBackend {
    backend: Arc<RecordingBackend>,
    barrier: Mutex<Option<Arc<LaunchBarrier>>>,
}

impl PtyBackend for FirstSpawnBarrierBackend {
    fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        let barrier = self.barrier.lock().expect("spawn barrier lock").take();
        if let Some(barrier) = barrier {
            barrier.block();
        }
        self.backend.spawn(input)
    }
}

struct ReadinessBarrierPreparer {
    generation: Mutex<Option<oneshot::Sender<TerminalObserverGenerationLease>>>,
    readiness: Arc<LaunchBarrier>,
}

impl TerminalLaunchPreparer for ReadinessBarrierPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            self.generation
                .lock()
                .expect("generation sender lock")
                .take()
                .expect("one preparation")
                .send(input.generation.observation())
                .expect("generation receiver");
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: "prepared-command".to_owned(),
                args: Vec::new(),
                private_env: BTreeMap::new(),
                observer: Box::new(ReadinessBarrierObserver(self.readiness.clone())),
            })
        })
    }
}

struct ReadinessBarrierObserver(Arc<LaunchBarrier>);

impl PreparedTerminalObserver for ReadinessBarrierObserver {
    fn is_ready_for_on_spawned(&self) -> bool {
        self.0.block();
        false
    }

    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
        panic!("cancelled observer must not receive on_spawned");
    }

    fn diagnostic_label(&self) -> &str {
        "readiness-barrier-observer"
    }
}

impl RecordingBackend {
    fn new(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            events,
            spawns: Mutex::new(Vec::new()),
            processes: Mutex::new(Vec::new()),
            failures: Mutex::new(VecDeque::new()),
        }
    }

    fn fail_next(&self, error: impl Into<String>) {
        self.failures
            .lock()
            .expect("failures lock")
            .push_back(error.into());
    }

    fn spawns(&self) -> Vec<PtySpawnInput> {
        self.spawns.lock().expect("spawns lock").clone()
    }

    fn latest(&self) -> Arc<RecordingProcess> {
        self.processes
            .lock()
            .expect("processes lock")
            .last()
            .cloned()
            .expect("spawned process")
    }
}

impl PtyBackend for RecordingBackend {
    fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        self.events.lock().expect("events lock").push("spawn");
        self.spawns.lock().expect("spawns lock").push(input.clone());
        if let Some(error) = self.failures.lock().expect("failures lock").pop_front() {
            return Err(error);
        }
        let mut processes = self.processes.lock().expect("processes lock");
        let process = Arc::new(RecordingProcess::new(processes.len() as u32 + 41));
        processes.push(process.clone());
        Ok(process)
    }
}

#[derive(Debug)]
struct PassThroughPreparer {
    events: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Debug)]
struct DelayedPassThroughPreparer {
    delay: std::time::Duration,
    completed: Arc<AtomicBool>,
}

impl TerminalLaunchPreparer for DelayedPassThroughPreparer {
    fn prepare(
        &self,
        _input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            self.completed.store(true, Ordering::Release);
            TerminalLaunchPreparation::PassThrough
        })
    }
}

#[derive(Debug)]
struct OversizedBudgetPreparer {
    delay: std::time::Duration,
    completed: Arc<AtomicBool>,
}

impl TerminalLaunchPreparer for OversizedBudgetPreparer {
    fn preparation_execution_budget(
        &self,
        _input: &TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = std::time::Duration> + Send + '_>> {
        Box::pin(async { std::time::Duration::from_secs(5) })
    }

    fn prepare(
        &self,
        _input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            self.completed.store(true, Ordering::Release);
            TerminalLaunchPreparation::PassThrough
        })
    }
}

#[derive(Debug)]
struct DelayedProviderFactory {
    delay: std::time::Duration,
    completed: Arc<AtomicBool>,
}

impl ProviderTerminalObserverFactory for DelayedProviderFactory {
    fn prepare(
        &self,
        _input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            self.completed.store(true, Ordering::Release);
            None
        })
    }
}

#[derive(Clone, Debug)]
struct SpawnObservation {
    pid: u32,
    generation: TerminalObserverGenerationLease,
}

#[derive(Clone, Debug)]
struct CancelObservation {
    reason: TerminalObserverCancellationReason,
    generation_was_current: bool,
}

#[derive(Debug, Default)]
struct ObserverState {
    generation: Mutex<Option<TerminalObserverGenerationLease>>,
    spawned: Mutex<Vec<SpawnObservation>>,
    cancelled: Mutex<Vec<CancelObservation>>,
    cancellation: tokio::sync::Notify,
}

impl ObserverState {
    async fn wait_for_cancellation(&self) -> CancelObservation {
        if let Some(observation) = self
            .cancelled
            .lock()
            .expect("cancelled lock")
            .last()
            .cloned()
        {
            return observation;
        }
        let generation = loop {
            if let Some(generation) = self.generation.lock().expect("generation lock").clone() {
                break generation;
            }
            self.cancellation.notified().await;
        };
        let reason = generation.cancelled().await;
        let observation = CancelObservation {
            reason,
            generation_was_current: generation.cancellation_was_requested_while_current(),
        };
        self.cancelled
            .lock()
            .expect("cancelled lock")
            .push(observation.clone());
        observation
    }
}

#[derive(Debug)]
struct RecordingObserver {
    state: Arc<ObserverState>,
}

impl PreparedTerminalObserver for RecordingObserver {
    fn on_spawned(
        &self,
        pid: u32,
        generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
        *self.state.generation.lock().expect("generation lock") = Some(generation.clone());
        self.state
            .spawned
            .lock()
            .expect("spawned lock")
            .push(SpawnObservation { pid, generation });
        self.state.cancellation.notify_waiters();
    }

    fn diagnostic_label(&self) -> &str {
        "recording-observer"
    }
}

#[derive(Debug, Default)]
struct CountingActivityObserver {
    disabled: AtomicUsize,
    enabled: AtomicUsize,
}

impl PreparedTerminalObserver for CountingActivityObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) -> Pin<Box<dyn Future<Output = TerminalAgentActivityTransition> + Send + '_>> {
        Box::pin(async move {
            if enabled {
                self.enabled.fetch_add(1, Ordering::AcqRel);
                TerminalAgentActivityTransition {
                    resumed: 1,
                    ..TerminalAgentActivityTransition::default()
                }
            } else {
                self.disabled.fetch_add(1, Ordering::AcqRel);
                TerminalAgentActivityTransition {
                    stopped: 1,
                    dormant: 1,
                    ..TerminalAgentActivityTransition::default()
                }
            }
        })
    }

    fn diagnostic_label(&self) -> &str {
        "counting-activity-observer"
    }
}

#[derive(Debug)]
struct CountingActivityPreparer {
    observer: Arc<CountingActivityObserver>,
}

impl TerminalLaunchPreparer for CountingActivityPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(CountingActivityObserverProxy {
                    observer: self.observer.clone(),
                }),
            })
        })
    }
}

#[derive(Debug)]
struct PostActivityCheckPausingPreparer {
    controller: AgentActivityController,
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
    observer: Arc<CountingActivityObserver>,
}

impl TerminalLaunchPreparer for PostActivityCheckPausingPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            let Some(admission) = self.controller.admit() else {
                return TerminalLaunchPreparation::PassThrough;
            };
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("post-activity-check release")
                .forget();
            TerminalLaunchPreparation::Admitted(
                PreparedTerminalLaunch {
                    executable: "instrumented-codex".to_owned(),
                    args: input.args,
                    private_env: BTreeMap::from([(
                        "BIBCODE_ACTIVITY_OBSERVER".to_owned(),
                        "enabled".to_owned(),
                    )]),
                    observer: Box::new(CountingActivityObserverProxy {
                        observer: self.observer.clone(),
                    }),
                },
                admission,
            )
        })
    }
}

#[derive(Debug)]
struct CountingActivityObserverProxy {
    observer: Arc<CountingActivityObserver>,
}

impl PreparedTerminalObserver for CountingActivityObserverProxy {
    fn on_spawned(
        &self,
        pid: u32,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) {
        self.observer.on_spawned(pid, generation, workers);
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) -> Pin<Box<dyn Future<Output = TerminalAgentActivityTransition> + Send + '_>> {
        self.observer
            .set_agent_activity_enabled(enabled, generation, workers)
    }

    fn diagnostic_label(&self) -> &str {
        self.observer.diagnostic_label()
    }
}

#[derive(Debug)]
struct CountingInventoryAuthority {
    calls: AtomicUsize,
    inventory: ProviderTerminalInventory,
}

impl ProviderTerminalInventoryAuthority for CountingInventoryAuthority {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderTerminalInventory, ()>> + Send + '_>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(self.inventory.clone())
        })
    }
}

#[derive(Debug, Default)]
struct CountingProviderFactory {
    prepare_calls: AtomicUsize,
}

impl ProviderTerminalObserverFactory for CountingProviderFactory {
    fn prepare(
        &self,
        _input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            self.prepare_calls.fetch_add(1, Ordering::AcqRel);
            None
        })
    }
}

#[derive(Debug)]
struct RacingProviderFactory {
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
    live_resources: Arc<AtomicUsize>,
}

impl ProviderTerminalObserverFactory for RacingProviderFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("race preparation release")
                .forget();
            self.live_resources.fetch_add(2, Ordering::AcqRel);
            let worker_resource = RacingOwnedResource {
                live_resources: self.live_resources.clone(),
            };
            input
                .launch
                .generation
                .worker_context()
                .spawn(async move {
                    let _worker_resource = worker_resource;
                    std::future::pending::<()>().await;
                })
                .expect("race worker");
            Some(PreparedTerminalLaunch {
                executable: input.launch.executable,
                args: input.launch.args,
                private_env: BTreeMap::new(),
                observer: Box::new(RacingPreparedObserver {
                    live_resources: self.live_resources.clone(),
                }),
            })
        })
    }
}

struct RacingOwnedResource {
    live_resources: Arc<AtomicUsize>,
}

impl Drop for RacingOwnedResource {
    fn drop(&mut self) {
        self.live_resources.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct RacingPreparedObserver {
    live_resources: Arc<AtomicUsize>,
}

impl Drop for RacingPreparedObserver {
    fn drop(&mut self) {
        self.live_resources.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PreparedTerminalObserver for RacingPreparedObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
    }

    fn diagnostic_label(&self) -> &str {
        "racing-prepared-observer"
    }
}

#[derive(Debug)]
struct HungProviderFactory {
    entered: Arc<tokio::sync::Semaphore>,
    release: Arc<tokio::sync::Semaphore>,
    returned: Arc<tokio::sync::Semaphore>,
    dropped: Arc<tokio::sync::Semaphore>,
    live_observers: Arc<AtomicUsize>,
}

impl ProviderTerminalObserverFactory for HungProviderFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("hung factory release")
                .forget();
            self.live_observers.fetch_add(1, Ordering::AcqRel);
            self.returned.add_permits(1);
            Some(PreparedTerminalLaunch {
                executable: input.launch.executable,
                args: input.launch.args,
                private_env: BTreeMap::from([(
                    "BIBCODE_HUNG_FACTORY_OBSERVER".to_owned(),
                    "installed".to_owned(),
                )]),
                observer: Box::new(HungPreparedObserver {
                    dropped: self.dropped.clone(),
                    live_observers: self.live_observers.clone(),
                }),
            })
        })
    }
}

#[derive(Debug)]
struct HungPreparedObserver {
    dropped: Arc<tokio::sync::Semaphore>,
    live_observers: Arc<AtomicUsize>,
}

impl Drop for HungPreparedObserver {
    fn drop(&mut self) {
        self.live_observers.fetch_sub(1, Ordering::AcqRel);
        self.dropped.add_permits(1);
    }
}

impl PreparedTerminalObserver for HungPreparedObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
    }

    fn diagnostic_label(&self) -> &str {
        "hung-prepared-observer"
    }
}

struct HungFactoryReleaseGuard {
    release: Arc<tokio::sync::Semaphore>,
    released: bool,
}

impl HungFactoryReleaseGuard {
    fn new(release: Arc<tokio::sync::Semaphore>) -> Self {
        Self {
            release,
            released: false,
        }
    }

    fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.release.add_permits(1);
        }
    }
}

impl Drop for HungFactoryReleaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone)]
struct TerminalSettingsTransitionHandler {
    controller: AgentActivityController,
    manager: TerminalManager,
}

impl AgentActivitySettingsHandler for TerminalSettingsTransitionHandler {
    fn transition(
        &self,
        source: AgentActivitySource,
        enabled: bool,
        settings_generation: u64,
    ) -> Pin<Box<dyn Future<Output = AgentActivityTransitionReport> + Send + '_>> {
        Box::pin(async move {
            assert_eq!(source, AgentActivitySource::Terminal);
            let state = if enabled {
                self.controller.enable()
            } else {
                self.controller.disable().await.state
            };
            let terminal = self.manager.set_agent_activity_enabled(enabled).await;
            AgentActivityTransitionReport {
                enabled: state.enabled,
                settings_generation,
                observation_generation: state.generation,
                stopped_observers: terminal.stopped,
                dormant_observers: terminal.dormant,
                resumed_observers: terminal.resumed,
                failed_observers: terminal.failed,
                unavailable_observers: terminal.unavailable,
                terminal_observation_epochs: terminal.epochs,
                ..AgentActivityTransitionReport::default()
            }
        })
    }
}

#[derive(Debug)]
struct PreparedPlan {
    executable: String,
    args: Vec<String>,
    private_env: BTreeMap<String, String>,
    observer: Arc<ObserverState>,
}

#[derive(Debug)]
enum PreparationPlan {
    PassThrough,
    Prepared(PreparedPlan),
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Ready,
    Hang,
    Panic,
}

#[derive(Debug)]
struct FaultingPreparer {
    prepare_fault: Fault,
    observer_on_spawned_fault: Fault,
    prepare_started: Arc<tokio::sync::Semaphore>,
    observer_started: Arc<tokio::sync::Semaphore>,
    generation: Arc<Mutex<Option<TerminalObserverGenerationLease>>>,
}

impl FaultingPreparer {
    fn new(
        prepare_fault: Fault,
        observer_on_spawned_fault: Fault,
        _observer_cancel_fault: Fault,
    ) -> Self {
        Self {
            prepare_fault,
            observer_on_spawned_fault,
            prepare_started: Arc::new(tokio::sync::Semaphore::new(0)),
            observer_started: Arc::new(tokio::sync::Semaphore::new(0)),
            generation: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Debug)]
struct FaultingObserver {
    on_spawned_fault: Fault,
    observer_started: Arc<tokio::sync::Semaphore>,
    generation: Arc<Mutex<Option<TerminalObserverGenerationLease>>>,
}

#[derive(Debug)]
struct NonYieldingPreparer {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl NonYieldingPreparer {
    fn new() -> Self {
        Self {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
        }
    }

    fn release(&self) {
        let (released, wake) = &*self.release;
        *released.lock().expect("release lock") = true;
        wake.notify_all();
    }
}

impl TerminalLaunchPreparer for NonYieldingPreparer {
    fn prepare(
        &self,
        _input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            self.started.add_permits(1);
            let (released, wake) = &*self.release;
            let mut released = released.lock().expect("release lock");
            while !*released {
                released = wake.wait(released).expect("release wait");
            }
            TerminalLaunchPreparation::PassThrough
        })
    }
}

#[derive(Debug)]
struct SerializedCallbackState {
    active: std::sync::atomic::AtomicUsize,
    maximum: std::sync::atomic::AtomicUsize,
    spawned: tokio::sync::Semaphore,
    release_spawned: (std::sync::Mutex<bool>, std::sync::Condvar),
    spawned_generation: Mutex<Option<TerminalObserverGenerationLease>>,
}

impl Default for SerializedCallbackState {
    fn default() -> Self {
        Self {
            active: std::sync::atomic::AtomicUsize::new(0),
            maximum: std::sync::atomic::AtomicUsize::new(0),
            spawned: tokio::sync::Semaphore::new(0),
            release_spawned: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
            spawned_generation: Mutex::new(None),
        }
    }
}

impl SerializedCallbackState {
    fn enter(self: &Arc<Self>) -> SerializedCallbackGuard {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum.fetch_max(active, Ordering::AcqRel);
        SerializedCallbackGuard {
            state: self.clone(),
        }
    }

    fn release_spawned(&self) {
        *self.release_spawned.0.lock().expect("spawn release lock") = true;
        self.release_spawned.1.notify_all();
    }
}

struct SerializedCallbackGuard {
    state: Arc<SerializedCallbackState>,
}

impl Drop for SerializedCallbackGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct SerializedCallbackObserver {
    state: Arc<SerializedCallbackState>,
}

impl PreparedTerminalObserver for SerializedCallbackObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
        let _active = self.state.enter();
        *self
            .state
            .spawned_generation
            .lock()
            .expect("spawned generation lock") = Some(generation);
        self.state.spawned.add_permits(1);
        let mut released = self
            .state
            .release_spawned
            .0
            .lock()
            .expect("spawn release lock");
        while !*released {
            released = self
                .state
                .release_spawned
                .1
                .wait(released)
                .expect("spawn release wait");
        }
    }

    fn diagnostic_label(&self) -> &str {
        "serialized-callback-observer"
    }
}

#[derive(Debug)]
struct SerializedCallbackPreparer {
    state: Arc<SerializedCallbackState>,
}

impl TerminalLaunchPreparer for SerializedCallbackPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(SerializedCallbackObserver {
                    state: self.state.clone(),
                }),
            })
        })
    }
}

#[derive(Debug)]
struct NonCooperativeCallbackState {
    active: AtomicUsize,
    maximum: AtomicUsize,
    started: AtomicUsize,
    release: (std::sync::Mutex<bool>, std::sync::Condvar),
    generation: Mutex<Option<TerminalObserverGenerationLease>>,
}

impl Default for NonCooperativeCallbackState {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            release: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
            generation: Mutex::new(None),
        }
    }
}

impl NonCooperativeCallbackState {
    fn release(&self) {
        *self.release.0.lock().expect("release lock") = true;
        self.release.1.notify_all();
    }
}

#[derive(Debug)]
struct NonCooperativeCallbackObserver {
    state: Arc<NonCooperativeCallbackState>,
}

impl PreparedTerminalObserver for NonCooperativeCallbackObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
        *self.state.generation.lock().expect("generation lock") = Some(generation);
        let active = self.state.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.state.maximum.fetch_max(active, Ordering::AcqRel);
        self.state.started.fetch_add(1, Ordering::AcqRel);
        let mut released = self.state.release.0.lock().expect("release lock");
        while !*released {
            released = self.state.release.1.wait(released).expect("release wait");
        }
        self.state.active.fetch_sub(1, Ordering::AcqRel);
    }

    fn diagnostic_label(&self) -> &str {
        "non-cooperative-callback-observer"
    }
}

#[derive(Debug)]
struct NonCooperativeCallbackPreparer {
    state: Arc<NonCooperativeCallbackState>,
    prepared: AtomicUsize,
    limit: usize,
}

impl TerminalLaunchPreparer for NonCooperativeCallbackPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            if self.prepared.fetch_add(1, Ordering::AcqRel) >= self.limit {
                return TerminalLaunchPreparation::PassThrough;
            }
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(NonCooperativeCallbackObserver {
                    state: self.state.clone(),
                }),
            })
        })
    }
}

#[derive(Debug, Default)]
struct CallbackBlockingPoolState {
    active: AtomicUsize,
    maximum: AtomicUsize,
    started: AtomicUsize,
    release: (std::sync::Mutex<bool>, std::sync::Condvar),
}

impl CallbackBlockingPoolState {
    fn release(&self) {
        *self.release.0.lock().expect("release lock") = true;
        self.release.1.notify_all();
    }

    fn blocking_job(self: Arc<Self>) {
        let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.maximum.fetch_max(active, Ordering::AcqRel);
        self.started.fetch_add(1, Ordering::AcqRel);
        let mut released = self.release.0.lock().expect("release lock");
        while !*released {
            released = self.release.1.wait(released).expect("release wait");
        }
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct CallbackBlockingPoolObserver;

impl PreparedTerminalObserver for CallbackBlockingPoolObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
    }

    fn diagnostic_label(&self) -> &str {
        "callback-blocking-pool-observer"
    }
}

#[derive(Debug)]
struct CallbackBlockingPoolPreparer {
    state: Arc<CallbackBlockingPoolState>,
}

impl TerminalLaunchPreparer for CallbackBlockingPoolPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            let mut jobs = Vec::new();
            for _ in 0..2 {
                let state = self.state.clone();
                jobs.push(tokio::task::spawn_blocking(move || state.blocking_job()));
            }
            for job in jobs {
                job.await.expect("callback blocking job");
            }
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(CallbackBlockingPoolObserver),
            })
        })
    }
}

#[derive(Debug)]
struct DurableObserverWorkerState {
    ping_sender: tokio::sync::mpsc::UnboundedSender<()>,
    ping_receiver: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<()>>>,
    started: Arc<tokio::sync::Semaphore>,
    ping_seen: Arc<tokio::sync::Semaphore>,
    cancellation_reasons: Arc<Mutex<Vec<TerminalObserverCancellationReason>>>,
    stubborn_dropped: Arc<AtomicUsize>,
}

impl Default for DurableObserverWorkerState {
    fn default() -> Self {
        let (ping_sender, ping_receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            ping_sender,
            ping_receiver: Mutex::new(Some(ping_receiver)),
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            ping_seen: Arc::new(tokio::sync::Semaphore::new(0)),
            cancellation_reasons: Arc::new(Mutex::new(Vec::new())),
            stubborn_dropped: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Debug)]
struct DurableObserverWorkerDrop {
    dropped: Arc<AtomicUsize>,
}

impl Drop for DurableObserverWorkerDrop {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct DurableObserverWorker {
    state: Arc<DurableObserverWorkerState>,
}

impl PreparedTerminalObserver for DurableObserverWorker {
    fn on_spawned(
        &self,
        _pid: u32,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) {
        let mut ping_receiver = self
            .state
            .ping_receiver
            .lock()
            .expect("ping receiver lock")
            .take()
            .expect("one on_spawned callback");
        let started = self.state.started.clone();
        let ping_seen = self.state.ping_seen.clone();
        let cancellation_reasons = self.state.cancellation_reasons.clone();
        workers
            .spawn(async move {
                started.add_permits(1);
                ping_receiver.recv().await.expect("post-spawn ping");
                ping_seen.add_permits(1);
                let reason = generation.cancelled().await;
                cancellation_reasons
                    .lock()
                    .expect("cancellation reasons lock")
                    .push(reason);
            })
            .expect("cooperative observer worker admission");

        let started = self.state.started.clone();
        let stubborn_dropped = self.state.stubborn_dropped.clone();
        workers
            .spawn(async move {
                let _drop = DurableObserverWorkerDrop {
                    dropped: stubborn_dropped,
                };
                started.add_permits(1);
                std::future::pending::<()>().await;
            })
            .expect("stubborn observer worker admission");
    }

    fn diagnostic_label(&self) -> &str {
        "durable-observer-worker"
    }
}

#[derive(Debug)]
struct DurableObserverWorkerPreparer {
    state: Arc<DurableObserverWorkerState>,
    prepared: AtomicUsize,
}

impl TerminalLaunchPreparer for DurableObserverWorkerPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            if self.prepared.fetch_add(1, Ordering::AcqRel) != 0 {
                return TerminalLaunchPreparation::PassThrough;
            }
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(DurableObserverWorker {
                    state: self.state.clone(),
                }),
            })
        })
    }
}

#[derive(Debug)]
struct ObserverSetupBoundaryState {
    callback_ran: AtomicBool,
    ambient_runtime_present: AtomicBool,
    registered_worker_started: Arc<tokio::sync::Semaphore>,
}

impl Default for ObserverSetupBoundaryState {
    fn default() -> Self {
        Self {
            callback_ran: AtomicBool::new(false),
            ambient_runtime_present: AtomicBool::new(false),
            registered_worker_started: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

#[derive(Debug)]
struct ObserverSetupBoundary {
    state: Arc<ObserverSetupBoundaryState>,
}

impl PreparedTerminalObserver for ObserverSetupBoundary {
    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) {
        self.state.callback_ran.store(true, Ordering::Release);
        self.state.ambient_runtime_present.store(
            tokio::runtime::Handle::try_current().is_ok(),
            Ordering::Release,
        );
        let registered_worker_started = self.state.registered_worker_started.clone();
        workers
            .spawn(async move {
                registered_worker_started.add_permits(1);
            })
            .expect("manager-owned worker registration");
    }

    fn diagnostic_label(&self) -> &str {
        "observer-setup-boundary"
    }
}

#[derive(Debug)]
struct ObserverSetupBoundaryPreparer {
    state: Arc<ObserverSetupBoundaryState>,
}

impl TerminalLaunchPreparer for ObserverSetupBoundaryPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(ObserverSetupBoundary {
                    state: self.state.clone(),
                }),
            })
        })
    }
}

#[derive(Debug, Default)]
struct WorkerContextCaptureState {
    contexts: Mutex<Vec<TerminalObserverWorkerContext>>,
}

#[derive(Debug)]
struct WorkerContextCaptureObserver;

impl PreparedTerminalObserver for WorkerContextCaptureObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        _generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
    }

    fn diagnostic_label(&self) -> &str {
        "worker-context-capture-observer"
    }
}

#[derive(Debug)]
struct WorkerContextCapturePreparer {
    state: Arc<WorkerContextCaptureState>,
}

impl TerminalLaunchPreparer for WorkerContextCapturePreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            self.state
                .contexts
                .lock()
                .expect("worker contexts lock")
                .push(input.generation.worker_context());
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(WorkerContextCaptureObserver),
            })
        })
    }
}

#[derive(Debug)]
struct PassThroughWorkerState {
    dropped: Arc<AtomicUsize>,
}

impl Default for PassThroughWorkerState {
    fn default() -> Self {
        Self {
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[derive(Debug)]
struct PassThroughWorkerPreparer {
    state: Arc<PassThroughWorkerState>,
}

impl TerminalLaunchPreparer for PassThroughWorkerPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            let drop = DurableObserverWorkerDrop {
                dropped: self.state.dropped.clone(),
            };
            input
                .generation
                .worker_context()
                .spawn(async move {
                    let _drop = drop;
                    std::future::pending::<()>().await;
                })
                .expect("pass-through worker admission");
            TerminalLaunchPreparation::PassThrough
        })
    }
}

async fn apply_fault(fault: Fault) {
    match fault {
        Fault::Ready => {}
        Fault::Hang => std::future::pending::<()>().await,
        Fault::Panic => panic!("injected observer callback panic"),
    }
}

fn apply_setup_fault(fault: Fault) {
    match fault {
        Fault::Ready => {}
        Fault::Hang => loop {
            std::thread::park();
        },
        Fault::Panic => panic!("injected observer callback panic"),
    }
}

impl PreparedTerminalObserver for FaultingObserver {
    fn on_spawned(
        &self,
        _pid: u32,
        generation: TerminalObserverGenerationLease,
        _workers: TerminalObserverWorkerContext,
    ) {
        *self.generation.lock().expect("generation lock") = Some(generation);
        self.observer_started.add_permits(1);
        apply_setup_fault(self.on_spawned_fault);
    }

    fn diagnostic_label(&self) -> &str {
        "faulting-observer"
    }
}

impl TerminalLaunchPreparer for FaultingPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            self.prepare_started.add_permits(1);
            apply_fault(self.prepare_fault).await;
            TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                executable: input.executable,
                args: input.args,
                private_env: BTreeMap::new(),
                observer: Box::new(FaultingObserver {
                    on_spawned_fault: self.observer_on_spawned_fault,
                    observer_started: self.observer_started.clone(),
                    generation: self.generation.clone(),
                }),
            })
        })
    }
}

#[derive(Debug)]
struct PlannedPreparer {
    events: Arc<Mutex<Vec<&'static str>>>,
    plans: Mutex<VecDeque<PreparationPlan>>,
    inputs: Mutex<Vec<TerminalLaunchPreparationInput>>,
}

impl PlannedPreparer {
    fn new(
        events: Arc<Mutex<Vec<&'static str>>>,
        plans: impl IntoIterator<Item = PreparationPlan>,
    ) -> Self {
        Self {
            events,
            plans: Mutex::new(plans.into_iter().collect()),
            inputs: Mutex::new(Vec::new()),
        }
    }

    fn input_count(&self) -> usize {
        self.inputs.lock().expect("inputs lock").len()
    }
}

impl TerminalLaunchPreparer for PlannedPreparer {
    fn prepare(
        &self,
        input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            let generation = input.generation.clone();
            self.events.lock().expect("events lock").push("prepare");
            self.inputs.lock().expect("inputs lock").push(input);
            match self
                .plans
                .lock()
                .expect("plans lock")
                .pop_front()
                .unwrap_or(PreparationPlan::PassThrough)
            {
                PreparationPlan::PassThrough => TerminalLaunchPreparation::PassThrough,
                PreparationPlan::Prepared(plan) => {
                    *plan.observer.generation.lock().expect("generation lock") =
                        Some(generation.observation());
                    plan.observer.cancellation.notify_waiters();
                    TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
                        executable: plan.executable,
                        args: plan.args,
                        private_env: plan.private_env,
                        observer: Box::new(RecordingObserver {
                            state: plan.observer,
                        }),
                    })
                }
            }
        })
    }
}

#[derive(Debug, Default)]
struct RecordingFactory {
    scopes: Mutex<Vec<String>>,
    runtime_dirs: Mutex<Vec<std::path::PathBuf>>,
    publishers: Mutex<Vec<TerminalGenerationActivityPublisher>>,
    input_debugs: Mutex<Vec<String>>,
}

impl ProviderTerminalObserverFactory for RecordingFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            self.input_debugs
                .lock()
                .expect("input debugs lock")
                .push(format!("{input:?}"));
            self.scopes
                .lock()
                .expect("scopes lock")
                .push(input.launch.generation.observation().scope_id().to_owned());
            self.runtime_dirs
                .lock()
                .expect("runtime dirs lock")
                .push(input.runtime_dir);
            self.publishers
                .lock()
                .expect("publishers lock")
                .push(input.activity_publisher);
            let _ = input.process_attribution;
            None
        })
    }
}

#[derive(Debug, Default)]
struct PinningFactory {
    executables: Mutex<Vec<String>>,
}

impl ProviderTerminalObserverFactory for PinningFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            self.executables
                .lock()
                .expect("executables lock")
                .push(input.launch.executable.clone());
            Some(PreparedTerminalLaunch {
                executable: input.launch.executable,
                args: input.launch.args,
                private_env: BTreeMap::from([(
                    "BIBCODE_PRIVATE_TOKEN".to_owned(),
                    "observer-secret".to_owned(),
                )]),
                observer: Box::new(RecordingObserver {
                    state: Arc::new(ObserverState::default()),
                }),
            })
        })
    }
}

impl TerminalLaunchPreparer for PassThroughPreparer {
    fn prepare(
        &self,
        _input: TerminalLaunchPreparationInput,
    ) -> Pin<Box<dyn Future<Output = TerminalLaunchPreparation> + Send + '_>> {
        Box::pin(async move {
            self.events.lock().expect("events lock").push("prepare");
            TerminalLaunchPreparation::PassThrough
        })
    }
}

fn command(activity: bool) -> TerminalLaunchCommand {
    let mut command = serde_json::json!({
        "executable": "codex",
        "args": ["--help"],
        "label": "Codex",
    });
    if activity {
        command["activity"] = serde_json::json!({
            "driverKind": "codex",
            "providerInstanceId": "codex_personal",
        });
    }
    serde_json::from_value(command).expect("valid command")
}

#[tokio::test]
async fn lifecycle_unhinted_bypasses_preparer_and_hinted_prepares_before_spawn() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(events.clone()));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(PassThroughPreparer {
                events: events.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );

    let mut unhinted =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    unhinted.command = Some(command(false));
    manager.open(unhinted).await.expect("unhinted terminal");
    assert_eq!(
        events.lock().expect("events lock").as_slice(),
        ["spawn"],
        "an untrusted activity trigger must be absent before preparation is attempted"
    );

    let mut hinted =
        TerminalOpenInput::new("thread-1", "terminal-2", root.path().to_path_buf(), 80, 24);
    hinted.command = Some(command(true));
    manager.open(hinted).await.expect("hinted terminal");
    assert_eq!(
        events.lock().expect("events lock").as_slice(),
        ["spawn", "prepare", "spawn"],
        "a hinted command must finish preparation before PTY spawn"
    );
    let spawns = backend.spawns();
    assert_eq!(spawns[0].executable, "codex");
    assert_eq!(spawns[0].args, ["--help"]);
    assert_eq!(spawns[1].executable, "codex");
    assert_eq!(spawns[1].args, ["--help"]);

    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_preparation_execution_budget_defaults_to_500ms_and_clamps_at_1s() {
    let root = tempfile::tempdir().expect("timeout root");
    let default_completed = Arc::new(AtomicBool::new(false));
    let default_preparer = Arc::new(DelayedPassThroughPreparer {
        delay: std::time::Duration::from_millis(612),
        completed: default_completed.clone(),
    });
    let default_input = TerminalLaunchPreparationInput {
        executable: "codex".to_owned(),
        args: Vec::new(),
        cwd: root.path().to_path_buf(),
        worktree_path: None,
        launch_env: BTreeMap::new(),
        activity: ProviderTerminalActivityLaunch {
            driver_kind: "codex".to_owned(),
            provider_instance_id: "codex".to_owned(),
        },
        generation: TerminalObserverGeneration::new(
            "thread-default-budget".to_owned(),
            "terminal-default-budget".to_owned(),
        ),
    };
    assert_eq!(
        default_preparer
            .preparation_execution_budget(&default_input)
            .await,
        std::time::Duration::from_millis(500),
        "preparers that do not opt in retain the fixed 500ms execution budget"
    );
    let default_manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(default_preparer),
            ..TerminalManagerOptions::default()
        },
    );
    let mut default_open = TerminalOpenInput::new(
        "thread-default-budget",
        "terminal-default-budget",
        root.path().to_path_buf(),
        80,
        24,
    );
    default_open.command = Some(command(true));
    let default_started = std::time::Instant::now();
    default_manager
        .open(default_open)
        .await
        .expect("default-budget terminal");
    let default_elapsed = default_started.elapsed();
    assert!(
        default_elapsed >= std::time::Duration::from_millis(450)
            && default_elapsed < std::time::Duration::from_millis(850),
        "default preparation completed outside its 500ms fail-open bound: {default_elapsed:?}"
    );
    assert!(
        !default_completed.load(Ordering::Acquire),
        "the 612ms callback unexpectedly completed inside the default budget"
    );
    tokio::time::sleep(std::time::Duration::from_millis(180)).await;
    default_manager.shutdown().await;

    let oversized_completed = Arc::new(AtomicBool::new(false));
    let oversized_manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(OversizedBudgetPreparer {
                delay: std::time::Duration::from_millis(1_500),
                completed: oversized_completed.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut oversized_open = TerminalOpenInput::new(
        "thread-oversized-budget",
        "terminal-oversized-budget",
        root.path().to_path_buf(),
        80,
        24,
    );
    oversized_open.command = Some(command(true));
    let oversized_started = std::time::Instant::now();
    oversized_manager
        .open(oversized_open)
        .await
        .expect("clamped-budget terminal");
    let oversized_elapsed = oversized_started.elapsed();
    assert!(
        oversized_elapsed >= std::time::Duration::from_millis(900)
            && oversized_elapsed < std::time::Duration::from_millis(1_350),
        "preparer override escaped the hard 1s cap: {oversized_elapsed:?}"
    );
    assert!(
        !oversized_completed.load(Ordering::Acquire),
        "the 1.5s callback unexpectedly completed inside the hard cap"
    );
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    oversized_manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_only_validated_opencode_preparation_receives_the_1s_budget() {
    let root = tempfile::tempdir().expect("provider budget root");
    let codex = root.path().join("configured-codex");
    let claude = root.path().join("configured-claude");
    let opencode = root.path().join("configured-opencode");
    for executable in [&codex, &claude, &opencode] {
        std::fs::write(executable, b"configured").expect("configured executable");
    }
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = codex.to_string_lossy().into_owned();
    settings.providers.claude_agent.binary_path = claude.to_string_lossy().into_owned();
    settings.providers.opencode.binary_path = opencode.to_string_lossy().into_owned();
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let completed = Arc::new(AtomicBool::new(false));
    let factory = Arc::new(DelayedProviderFactory {
        delay: std::time::Duration::from_millis(612),
        completed: completed.clone(),
    });
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            claude: Some(factory.clone()),
            opencode: Some(factory),
        },
    )
    .expect("provider budget supervisor");
    let preparation_input =
        |executable: &std::path::Path, driver_kind: &str, provider_instance_id: &str| {
            TerminalLaunchPreparationInput {
                executable: executable.to_string_lossy().into_owned(),
                args: Vec::new(),
                cwd: root.path().to_path_buf(),
                worktree_path: None,
                launch_env: BTreeMap::new(),
                activity: ProviderTerminalActivityLaunch {
                    driver_kind: driver_kind.to_owned(),
                    provider_instance_id: provider_instance_id.to_owned(),
                },
                generation: TerminalObserverGeneration::new(
                    format!("thread-{driver_kind}-{provider_instance_id}"),
                    format!("terminal-{driver_kind}-{provider_instance_id}"),
                ),
            }
        };

    for (input, expected) in [
        (
            preparation_input(&codex, "codex", "codex"),
            std::time::Duration::from_millis(500),
        ),
        (
            preparation_input(&claude, "claudeAgent", "claudeAgent"),
            std::time::Duration::from_millis(500),
        ),
        (
            preparation_input(&opencode, "opencode", "opencode"),
            std::time::Duration::from_secs(1),
        ),
        (
            preparation_input(&opencode, "opencode", "missing"),
            std::time::Duration::from_millis(500),
        ),
        (
            preparation_input(&opencode, "codex", "opencode"),
            std::time::Duration::from_millis(500),
        ),
        (
            preparation_input(&codex, "opencode", "opencode"),
            std::time::Duration::from_millis(500),
        ),
    ] {
        assert_eq!(
            supervisor.preparation_execution_budget(&input).await,
            expected,
            "unexpected preparation budget for {:?}",
            input.activity
        );
    }

    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-opencode-budget",
        "terminal-opencode-budget",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: opencode.to_string_lossy().into_owned(),
        args: Vec::new(),
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });
    let started = std::time::Instant::now();
    manager
        .open(input)
        .await
        .expect("extended-budget OpenCode terminal");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_millis(580)
            && elapsed < std::time::Duration::from_millis(900),
        "validated OpenCode did not receive enough time for its 612ms preparation: {elapsed:?}"
    );
    assert!(
        completed.load(Ordering::Acquire),
        "validated OpenCode preparation was cut off at the default 500ms"
    );
    assert_eq!(backend.spawns().len(), 1);
    manager.shutdown().await;
}

#[tokio::test]
async fn lifecycle_prepared_launch_replaces_command_and_notifies_only_after_spawn() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(events.clone()));
    let observer = Arc::new(ObserverState::default());
    let preparer = Arc::new(PlannedPreparer::new(
        events.clone(),
        [PreparationPlan::Prepared(PreparedPlan {
            executable: "observed-codex".to_owned(),
            args: vec!["app-server".to_owned()],
            private_env: BTreeMap::from([
                (
                    "BIBCODE_PRIVATE_SOCKET".to_owned(),
                    "/private/socket".to_owned(),
                ),
                (
                    "BIBCODE_PRIVATE_TOKEN".to_owned(),
                    "secret-token".to_owned(),
                ),
            ]),
            observer: observer.clone(),
        })],
    ));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(preparer.clone()),
            ..TerminalManagerOptions::default()
        },
    );

    let mut input =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    input.env.insert("PUBLIC".to_owned(), "visible".to_owned());
    input.command = Some(command(true));
    let opened = manager.open(input).await.expect("prepared terminal");

    assert_eq!(
        events.lock().expect("events lock").as_slice(),
        ["prepare", "spawn"],
    );
    assert_eq!(preparer.input_count(), 1);
    let spawn = &backend.spawns()[0];
    assert_eq!(spawn.executable, "observed-codex");
    assert_eq!(spawn.args, ["app-server"]);
    assert_eq!(spawn.env.get("PUBLIC").map(String::as_str), Some("visible"));
    assert_eq!(
        spawn.env.get("BIBCODE_PRIVATE_SOCKET").map(String::as_str),
        Some("/private/socket")
    );
    assert_eq!(
        spawn.env.get("BIBCODE_PRIVATE_TOKEN").map(String::as_str),
        Some("secret-token")
    );

    {
        let spawned = observer.spawned.lock().expect("spawned lock");
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].pid, opened.pid.expect("terminal pid"));
        assert!(spawned[0].generation.is_current());
        assert_eq!(
            spawned[0].generation.scope_id(),
            format!("terminal:{}", spawned[0].generation.id())
        );
        assert_eq!(spawned[0].generation.thread_id(), "thread-1");
        assert_eq!(spawned[0].generation.terminal_id(), "terminal-1");
    }

    let mut terminal_events = manager.subscribe_events();
    backend.latest().emit("secret-token from provider");
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(bibcode_server::terminal::TerminalEvent::Output { data, .. }) =
                terminal_events.recv().await
            {
                break data;
            }
        }
    })
    .await
    .expect("redacted output");
    assert!(!output.contains("secret-token"), "{output}");
    let attachment = manager
        .attach(bibcode_server::terminal::TerminalAttachInput::existing(
            "thread-1",
            "terminal-1",
        ))
        .await
        .expect("attach prepared terminal");
    let history = serde_json::to_string(&attachment.initial.history).expect("history JSON");
    assert!(!history.contains("secret-token"), "{history}");

    manager.shutdown().await;
}

#[tokio::test]
async fn lifecycle_rejects_client_collision_with_private_environment_without_spawning() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(events.clone()));
    let observer = Arc::new(ObserverState::default());
    let preparer = Arc::new(PlannedPreparer::new(
        events,
        [PreparationPlan::Prepared(PreparedPlan {
            executable: "observed-codex".to_owned(),
            args: vec!["app-server".to_owned()],
            private_env: BTreeMap::from([(
                "BIBCODE_PRIVATE_TOKEN".to_owned(),
                "server-secret".to_owned(),
            )]),
            observer: observer.clone(),
        })],
    ));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(preparer),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    input.env.insert(
        "BIBCODE_PRIVATE_TOKEN".to_owned(),
        "client-value".to_owned(),
    );
    input.command = Some(command(true));

    let rendered = manager
        .open(input)
        .await
        .expect_err("reserved private environment collision")
        .to_string();
    assert!(rendered.contains("BIBCODE_PRIVATE_TOKEN"), "{rendered}");
    assert!(!rendered.contains("server-secret"), "{rendered}");
    assert!(!rendered.contains("client-value"), "{rendered}");
    assert!(backend.spawns().is_empty());
    assert_eq!(
        observer.wait_for_cancellation().await.reason,
        TerminalObserverCancellationReason::PreparationRejected
    );

    manager.shutdown().await;
}

#[tokio::test]
async fn lifecycle_spawn_failure_cancels_prepared_observer_without_disclosing_private_values() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(events.clone()));
    backend.fail_next("spawn rejected secret-token");
    let observer = Arc::new(ObserverState::default());
    let preparer = Arc::new(PlannedPreparer::new(
        events,
        [PreparationPlan::Prepared(PreparedPlan {
            executable: "observed-codex".to_owned(),
            args: vec!["app-server".to_owned()],
            private_env: BTreeMap::from([(
                "BIBCODE_PRIVATE_TOKEN".to_owned(),
                "secret-token".to_owned(),
            )]),
            observer: observer.clone(),
        })],
    ));
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(preparer),
            ..TerminalManagerOptions::default()
        },
    );

    let mut input =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    input.command = Some(command(true));
    let error = manager
        .open(input)
        .await
        .expect_err("injected spawn failure");
    let rendered = error.to_string();
    assert!(!rendered.contains("secret-token"), "{rendered}");
    assert_eq!(
        observer.wait_for_cancellation().await.reason,
        TerminalObserverCancellationReason::SpawnFailed
    );
    assert!(observer.spawned.lock().expect("spawned lock").is_empty());

    manager.shutdown().await;
}

#[tokio::test]
async fn lifecycle_exit_close_restart_and_shutdown_cancel_before_generation_invalidation() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(events.clone()));
    let observers = (0..5)
        .map(|_| Arc::new(ObserverState::default()))
        .collect::<Vec<_>>();
    let preparer = Arc::new(PlannedPreparer::new(
        events,
        observers.iter().cloned().map(|observer| {
            PreparationPlan::Prepared(PreparedPlan {
                executable: "observed-codex".to_owned(),
                args: vec!["app-server".to_owned()],
                private_env: BTreeMap::new(),
                observer,
            })
        }),
    ));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(preparer.clone()),
            ..TerminalManagerOptions::default()
        },
    );

    let mut exit_input =
        TerminalOpenInput::new("thread-1", "exit", root.path().to_path_buf(), 80, 24);
    exit_input.command = Some(command(true));
    manager.open(exit_input).await.expect("exit terminal");
    backend.latest().exit(0);
    let exited = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observers[0].wait_for_cancellation(),
    )
    .await
    .expect("exit cancellation");
    assert_eq!(
        exited.reason,
        TerminalObserverCancellationReason::ProcessExited
    );
    assert!(exited.generation_was_current);

    let mut close_input =
        TerminalOpenInput::new("thread-1", "close", root.path().to_path_buf(), 80, 24);
    close_input.command = Some(command(true));
    manager.open(close_input).await.expect("close terminal");
    manager
        .close("thread-1", Some("close"))
        .await
        .expect("close terminal");
    let closed = observers[1].wait_for_cancellation().await;
    assert_eq!(closed.reason, TerminalObserverCancellationReason::Closed);
    assert!(closed.generation_was_current);

    let mut restart_input =
        TerminalOpenInput::new("thread-1", "restart", root.path().to_path_buf(), 80, 24);
    restart_input.command = Some(command(true));
    manager
        .open(restart_input.clone())
        .await
        .expect("restart terminal");
    let first_generation = observers[2].spawned.lock().expect("spawned lock")[0]
        .generation
        .clone();
    manager
        .restart(restart_input)
        .await
        .expect("restarted terminal");
    let restarted = observers[2].wait_for_cancellation().await;
    assert_eq!(
        restarted.reason,
        TerminalObserverCancellationReason::Restarted
    );
    assert!(restarted.generation_was_current);
    assert!(!first_generation.is_current());
    let replacement_generation = observers[3].spawned.lock().expect("spawned lock")[0]
        .generation
        .clone();
    assert_ne!(first_generation.id(), replacement_generation.id());
    assert_ne!(
        first_generation.scope_id(),
        replacement_generation.scope_id()
    );

    let prepared_before_attach = preparer.input_count();
    let attached = manager
        .attach(bibcode_server::terminal::TerminalAttachInput::existing(
            "thread-1", "restart",
        ))
        .await
        .expect("attach existing terminal");
    assert_eq!(attached.initial.status, TerminalStatus::Running);
    assert_eq!(preparer.input_count(), prepared_before_attach);

    manager.shutdown().await;
    let shutdown = observers[3].wait_for_cancellation().await;
    assert_eq!(
        shutdown.reason,
        TerminalObserverCancellationReason::Shutdown
    );
    assert!(shutdown.generation_was_current);
    assert!(!replacement_generation.is_current());
}

#[tokio::test]
async fn lifecycle_restart_rejects_late_observer_and_pty_output_from_the_old_generation() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let backend = Arc::new(RecordingBackend::new(events.clone()));
    let old_observer = Arc::new(ObserverState::default());
    let new_observer = Arc::new(ObserverState::default());
    let preparer = Arc::new(PlannedPreparer::new(
        events,
        [old_observer.clone(), new_observer.clone()]
            .into_iter()
            .map(|observer| {
                PreparationPlan::Prepared(PreparedPlan {
                    executable: "observed-codex".to_owned(),
                    args: vec!["app-server".to_owned()],
                    private_env: BTreeMap::new(),
                    observer,
                })
            }),
    ));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(preparer),
            ..TerminalManagerOptions::default()
        },
    );
    let mut terminal_events = manager.subscribe_events();
    let mut input =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    input.command = Some(command(true));
    manager.open(input.clone()).await.expect("first generation");
    let old_process = backend.latest();
    manager
        .restart(input)
        .await
        .expect("replacement generation");
    let new_process = backend.latest();
    old_process.emit("stale-generation-output");
    new_process.emit("current-generation-output");

    let mut saw_current = false;
    let mut saw_stale = false;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !saw_current {
            if let Ok(bibcode_server::terminal::TerminalEvent::Output { data, .. }) =
                terminal_events.recv().await
            {
                saw_stale |= data.contains("stale-generation-output");
                saw_current |= data.contains("current-generation-output");
            }
        }
    })
    .await
    .expect("current output");
    assert!(!saw_stale);
    assert!(
        !old_observer
            .generation
            .lock()
            .expect("generation lock")
            .as_ref()
            .expect("old generation")
            .is_current()
    );
    assert!(
        new_observer
            .generation
            .lock()
            .expect("generation lock")
            .as_ref()
            .expect("new generation")
            .is_current()
    );

    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_close_cancels_a_prepared_in_flight_observer_before_invalidating_spawn() {
    let root = tempfile::tempdir().expect("temp dir");
    let events = Arc::new(Mutex::new(Vec::new()));
    let observer = Arc::new(ObserverState::default());
    let preparer = Arc::new(PlannedPreparer::new(
        events,
        [PreparationPlan::Prepared(PreparedPlan {
            executable: "observed-codex".to_owned(),
            args: vec!["app-server".to_owned()],
            private_env: BTreeMap::new(),
            observer: observer.clone(),
        })],
    ));
    let process = Arc::new(RecordingProcess::new(99));
    let (spawn_started_tx, spawn_started_rx) = std::sync::mpsc::channel();
    let (spawn_release_tx, spawn_release_rx) = std::sync::mpsc::channel();
    let backend = Arc::new(BlockingBackend {
        process: process.clone(),
        spawn_started: Mutex::new(Some(spawn_started_tx)),
        spawn_release: Mutex::new(spawn_release_rx),
    });
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(preparer),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    input.command = Some(command(true));
    let open_manager = manager.clone();
    let open = tokio::spawn(async move { open_manager.open(input).await });
    tokio::task::spawn_blocking(move || {
        spawn_started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("spawn started")
    })
    .await
    .expect("spawn waiter");

    let close = manager.close("thread-1", Some("terminal-1"));
    tokio::pin!(close);
    // Drive close and cancellation observation in the same task so spawn cannot
    // resume between close's cancellation signal and its invalidation fence.
    let cancelled_before_release =
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            tokio::select! {
                biased;
                result = &mut close => panic!("close completed before spawn release: {result:?}"),
                cancellation = observer.wait_for_cancellation() => cancellation,
            }
        })
        .await;
    spawn_release_tx.send(()).expect("spawn release receiver");
    let open_result = open.await.expect("open task");
    close.await.expect("close terminal");

    let cancellation =
        cancelled_before_release.expect("observer cancellation must precede spawn invalidation");
    assert_eq!(
        cancellation.reason,
        TerminalObserverCancellationReason::Closed
    );
    assert!(cancellation.generation_was_current);
    assert!(open_result.is_err());
    assert!(process.killed.load(Ordering::Acquire));
    manager.shutdown().await;
}

async fn assert_lifecycle_cancellation_never_falls_back(restart: bool, during_readiness: bool) {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let (spawn_barrier, spawn_release) = LaunchBarrier::new();
    let (readiness, readiness_release) = LaunchBarrier::new();
    let (generation_tx, generation_rx) = oneshot::channel();
    let manager = TerminalManager::new(
        Arc::new(FirstSpawnBarrierBackend {
            backend: backend.clone(),
            barrier: Mutex::new(Some(spawn_barrier.clone())),
        }),
        TerminalManagerOptions {
            subprocess_poll_interval: std::time::Duration::ZERO,
            launch_preparer: Some(Arc::new(ReadinessBarrierPreparer {
                generation: Mutex::new(Some(generation_tx)),
                readiness: readiness.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input =
        TerminalOpenInput::new("thread-1", "terminal-1", root.path().to_path_buf(), 80, 24);
    input.command = Some(command(true));
    let open_manager = manager.clone();
    let open_input = input.clone();
    let open = tokio::spawn(async move { open_manager.open(open_input).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        spawn_barrier.started.notified(),
    )
    .await
    .expect("first spawn started");
    let generation = generation_rx.await.expect("prepared generation");
    if during_readiness {
        spawn_release.send(()).expect("release first spawn");
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            readiness.started.notified(),
        )
        .await
        .expect("readiness callback started");
    }

    // A real restart has one legitimate replacement spawn, distinguishable from
    // the original command's forbidden fallback.
    input.command = Some(TerminalLaunchCommand {
        executable: "replacement-command".to_owned(),
        activity: None,
        ..command(false)
    });
    let cleanup_manager = manager.clone();
    let cleanup = tokio::spawn(async move {
        if restart {
            cleanup_manager.restart(input).await.map(|_| ())
        } else {
            cleanup_manager.close("thread-1", Some("terminal-1")).await
        }
    });
    let cancellation =
        tokio::time::timeout(std::time::Duration::from_secs(2), generation.cancelled()).await;
    // Always release the fixture before asserting, including on timeout.
    if during_readiness {
        readiness_release.send(()).expect("release readiness");
    } else {
        spawn_release.send(()).expect("release cancelled spawn");
    }
    let open_result = tokio::time::timeout(std::time::Duration::from_secs(2), open)
        .await
        .expect("open completed")
        .expect("open task");
    tokio::time::timeout(std::time::Duration::from_secs(2), cleanup)
        .await
        .expect("lifecycle cleanup completed")
        .expect("cleanup task")
        .expect("close or restart succeeded");
    let spawns = backend.spawns();
    let processes = backend.processes.lock().expect("processes lock").clone();
    let old_process_killed = processes[0].killed.load(Ordering::Acquire);
    manager.shutdown().await;

    assert_eq!(
        cancellation.expect("observer cancellation observed before release"),
        if restart {
            TerminalObserverCancellationReason::Restarted
        } else {
            TerminalObserverCancellationReason::Closed
        }
    );
    assert!(generation.cancellation_was_requested_while_current());
    assert!(
        matches!(
            open_result,
            Err(bibcode_server::terminal::TerminalError::NotFound { .. })
        ),
        "invalidated open: {open_result:?}"
    );
    assert!(old_process_killed, "invalidated process was not killed");
    let executables = spawns
        .iter()
        .map(|spawn| spawn.executable.as_str())
        .collect::<Vec<_>>();
    let expected = if restart {
        vec!["prepared-command", "replacement-command"]
    } else {
        vec!["prepared-command"]
    };
    assert_eq!(
        executables, expected,
        "invalidated open must spawn exactly once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifecycle_close_during_spawn_or_readiness_never_falls_back() {
    for during_readiness in [false, true] {
        assert_lifecycle_cancellation_never_falls_back(false, during_readiness).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifecycle_restart_during_spawn_or_readiness_never_falls_back() {
    for during_readiness in [false, true] {
        assert_lifecycle_cancellation_never_falls_back(true, during_readiness).await;
    }
}

#[tokio::test]
async fn lifecycle_supervisor_validates_instance_driver_and_full_executable_before_factory() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured_dir = fixture.path().join("configured");
    let impostor_dir = fixture.path().join("impostor");
    std::fs::create_dir_all(&configured_dir).expect("configured dir");
    std::fs::create_dir_all(&impostor_dir).expect("impostor dir");
    let configured = configured_dir.join("codex");
    let impostor = impostor_dir.join("codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    std::fs::write(&impostor, b"impostor").expect("impostor binary");

    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    for driver_kind in ["cursor", "grok"] {
        settings.provider_instances.insert(
            format!("{driver_kind}_malicious"),
            ProviderInstanceState {
                driver: driver_kind.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "binaryPath": configured.to_string_lossy(),
                }),
                ..ProviderInstanceState::default()
            },
        );
    }
    let inventory = ProviderTerminalInventory::from_settings(&settings);
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let attribution = ProcessAttributionRegistry::new();
    let runtime_dir = fixture.path().join("runtime");
    let expected_runtime_dir = std::fs::canonicalize(fixture.path())
        .expect("canonical fixture")
        .join("runtime");
    let factory = Arc::new(RecordingFactory::default());
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings,
        inventory,
        projection,
        attribution,
        runtime_dir.clone(),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("valid supervisor");
    let generation =
        TerminalObserverGeneration::new("thread-1".to_owned(), "terminal-1".to_owned());
    let preparation_input = |executable: &std::path::Path,
                             activity: ProviderTerminalActivityLaunch| {
        TerminalLaunchPreparationInput {
            executable: executable.to_string_lossy().into_owned(),
            args: vec!["--help".to_owned()],
            cwd: fixture.path().to_path_buf(),
            worktree_path: Some(fixture.path().to_path_buf()),
            launch_env: BTreeMap::new(),
            activity,
            generation: generation.clone(),
        }
    };

    assert!(matches!(
        supervisor
            .prepare(preparation_input(
                &impostor,
                ProviderTerminalActivityLaunch {
                    driver_kind: "codex".to_owned(),
                    provider_instance_id: "codex".to_owned(),
                },
            ))
            .await,
        TerminalLaunchPreparation::PassThrough
    ));
    assert!(factory.scopes.lock().expect("scopes lock").is_empty());

    for provider_instance_id in ["cursor_malicious", "grok_malicious"] {
        assert!(matches!(
            supervisor
                .prepare(preparation_input(
                    &configured,
                    ProviderTerminalActivityLaunch {
                        driver_kind: "codex".to_owned(),
                        provider_instance_id: provider_instance_id.to_owned(),
                    },
                ))
                .await,
            TerminalLaunchPreparation::PassThrough
        ));
    }
    assert!(
        factory.scopes.lock().expect("scopes lock").is_empty(),
        "unsupported providers must never reach a supported observer factory through a forged hint"
    );

    assert!(matches!(
        supervisor
            .prepare(preparation_input(
                &configured,
                ProviderTerminalActivityLaunch {
                    driver_kind: "claudeAgent".to_owned(),
                    provider_instance_id: "codex".to_owned(),
                },
            ))
            .await,
        TerminalLaunchPreparation::PassThrough
    ));
    assert!(matches!(
        supervisor
            .prepare(preparation_input(
                &configured,
                ProviderTerminalActivityLaunch {
                    driver_kind: "codex".to_owned(),
                    provider_instance_id: "missing".to_owned(),
                },
            ))
            .await,
        TerminalLaunchPreparation::PassThrough
    ));
    assert!(factory.scopes.lock().expect("scopes lock").is_empty());

    assert!(matches!(
        supervisor
            .prepare(preparation_input(
                &configured,
                ProviderTerminalActivityLaunch {
                    driver_kind: "codex".to_owned(),
                    provider_instance_id: "codex".to_owned(),
                },
            ))
            .await,
        TerminalLaunchPreparation::PassThrough
    ));
    assert_eq!(
        factory.scopes.lock().expect("scopes lock").as_slice(),
        [format!("terminal:{}", generation.id())]
    );
    assert_eq!(
        factory
            .runtime_dirs
            .lock()
            .expect("runtime dirs lock")
            .as_slice(),
        [expected_runtime_dir]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn hardening_startup_cleans_only_direct_marked_private_runtime_artifacts_without_following_symlinks()
 {
    let fixture = tempfile::tempdir().expect("fixture root");
    let runtime_dir = fixture.path().join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("runtime permissions");

    let owned = runtime_dir.join("owned-stale");
    std::fs::create_dir(&owned).expect("owned generation");
    std::fs::set_permissions(&owned, std::fs::Permissions::from_mode(0o700))
        .expect("owned permissions");
    std::fs::write(
        owned.join(".bibcode-provider-terminal-owner"),
        b"bibcode-provider-terminal-v1\n",
    )
    .expect("ownership marker");
    std::fs::write(owned.join("credentials.json"), b"secret").expect("stale credentials");
    std::fs::write(owned.join("socket"), b"socket").expect("stale socket");

    let unmarked = runtime_dir.join("unmarked");
    std::fs::create_dir(&unmarked).expect("unmarked directory");
    std::fs::write(unmarked.join("credentials.json"), b"preserve").expect("unmarked file");

    let outside = fixture.path().join("outside");
    std::fs::create_dir(&outside).expect("outside directory");
    std::fs::write(
        outside.join(".bibcode-provider-terminal-owner"),
        b"bibcode-provider-terminal-v1\n",
    )
    .expect("outside marker");
    std::fs::write(outside.join("preserve"), b"outside").expect("outside file");
    std::os::unix::fs::symlink(&outside, runtime_dir.join("linked-outside"))
        .expect("outside symlink");

    let marker_target = fixture.path().join("marker-target");
    std::fs::write(&marker_target, b"bibcode-provider-terminal-v1\n").expect("marker target");
    let linked_marker_dir = runtime_dir.join("linked-marker");
    std::fs::create_dir(&linked_marker_dir).expect("linked marker dir");
    std::fs::set_permissions(&linked_marker_dir, std::fs::Permissions::from_mode(0o700))
        .expect("linked marker permissions");
    std::os::unix::fs::symlink(
        &marker_target,
        linked_marker_dir.join(".bibcode-provider-terminal-owner"),
    )
    .expect("marker symlink");
    std::fs::write(linked_marker_dir.join("preserve"), b"linked marker")
        .expect("linked marker payload");

    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let settings = ProviderSettingsState::default();
    ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        runtime_dir,
        ProviderTerminalObserverFactories::default(),
    )
    .expect("supervisor");

    assert_empty_retired_generation(&owned, "direct marked stale generation cleanup");
    assert!(unmarked.exists(), "unmarked private directory is preserved");
    assert!(
        outside.exists(),
        "symlink target outside the runtime is preserved"
    );
    assert!(
        linked_marker_dir.exists(),
        "a symlinked marker never proves ownership"
    );
    assert!(marker_target.exists(), "marker symlink target is preserved");
}

#[cfg(unix)]
#[tokio::test]
async fn hardening_executable_uses_client_path_for_validation_and_pins_canonical_spawn_target() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let trusted_dir = fixture.path().join("trusted");
    std::fs::create_dir(&trusted_dir).expect("trusted fixture directory");
    let configured = trusted_dir.join("sh");
    std::fs::write(&configured, b"trusted recorded provider fixture")
        .expect("configured fixture executable");
    let configured = configured
        .canonicalize()
        .expect("canonical configured fixture");
    let attacker_dir = fixture.path().join("attacker");
    std::fs::create_dir_all(&attacker_dir).expect("attacker dir");
    std::fs::write(attacker_dir.join("sh"), b"attacker").expect("attacker shell");

    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let inventory = ProviderTerminalInventory::from_settings(&settings);
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let factory = Arc::new(PinningFactory::default());
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings,
        inventory,
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-1",
        "terminal-1",
        fixture.path().to_path_buf(),
        80,
        24,
    );
    input.env.insert(
        "PATH".to_owned(),
        attacker_dir.to_string_lossy().into_owned(),
    );
    input.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": "sh",
            "args": ["-c", "exit 0"],
            "activity": {
            "driverKind": "codex",
                "providerInstanceId": "codex"
            }
        }))
        .expect("command"),
    );

    manager.open(input).await.expect("terminal");

    assert!(
        factory
            .executables
            .lock()
            .expect("executables lock")
            .is_empty(),
        "client PATH resolves the hinted basename to an untrusted executable, so no factory may run"
    );
    let spawn = &backend.spawns()[0];
    assert_eq!(spawn.executable, "sh");
    assert!(
        !spawn.env.contains_key("BIBCODE_PRIVATE_TOKEN"),
        "a rejected hint must not inject observer-private values"
    );

    let configured_dir = configured.parent().expect("configured parent");
    let mut trusted = TerminalOpenInput::new(
        "thread-1",
        "terminal-2",
        fixture.path().to_path_buf(),
        80,
        24,
    );
    trusted.env.insert(
        "PATH".to_owned(),
        configured_dir.to_string_lossy().into_owned(),
    );
    trusted.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": "sh",
            "args": ["-c", "exit 0"],
            "activity": {
                "driverKind": "codex",
                "providerInstanceId": "codex"
            }
        }))
        .expect("command"),
    );
    manager.open(trusted).await.expect("trusted terminal");
    assert_eq!(
        factory
            .executables
            .lock()
            .expect("executables lock")
            .as_slice(),
        [configured.to_string_lossy().as_ref()],
        "the factory must receive the validated canonical target"
    );
    let trusted_spawn = &backend.spawns()[1];
    assert_eq!(
        trusted_spawn.executable,
        configured.to_string_lossy(),
        "the prepared PTY must execute the pinned canonical target"
    );
    assert_eq!(
        trusted_spawn
            .env
            .get("BIBCODE_PRIVATE_TOKEN")
            .map(String::as_str),
        Some("observer-secret")
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_callbacks_hung_prepare_fails_open_without_blocking_unrelated_terminal() {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let preparer = Arc::new(FaultingPreparer::new(
        Fault::Hang,
        Fault::Ready,
        Fault::Ready,
    ));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(preparer.clone()),
            ..TerminalManagerOptions::default()
        },
    );
    let mut hinted = TerminalOpenInput::new("thread-1", "hung", root.path().to_path_buf(), 80, 24);
    hinted.command = Some(command(true));
    let open_manager = manager.clone();
    let hung_open = tokio::spawn(async move { open_manager.open(hinted).await });
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        preparer.prepare_started.clone().acquire_owned(),
    )
    .await
    .expect("prepare started")
    .expect("prepare semaphore");
    permit.forget();

    let unrelated =
        TerminalOpenInput::new("thread-1", "unrelated", root.path().to_path_buf(), 80, 24);
    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        manager.open(unrelated),
    )
    .await
    .expect("hung prepare must not hold the manager lifecycle")
    .expect("unrelated terminal");
    tokio::time::timeout(std::time::Duration::from_secs(2), hung_open)
        .await
        .expect("prepare timeout")
        .expect("open task")
        .expect("original launch after prepare timeout");
    assert_eq!(backend.spawns().len(), 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn hardening_callbacks_panicking_prepare_fails_open_to_original_launch() {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(FaultingPreparer::new(
                Fault::Panic,
                Fault::Ready,
                Fault::Ready,
            ))),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new("thread-1", "panic", root.path().to_path_buf(), 80, 24);
    input.command = Some(command(true));
    let open_manager = manager.clone();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::spawn(async move { open_manager.open(input).await }),
    )
    .await
    .expect("prepare panic boundary")
    .expect("open task must not panic")
    .expect("original launch after prepare panic");
    assert_eq!(backend.spawns()[0].executable, "codex");
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_serialized_callbacks_non_yielding_prepare_has_a_hard_deadline() {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let preparer = Arc::new(NonYieldingPreparer::new());
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(preparer.clone()),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-hard-timeout",
        "terminal-1",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    let open_manager = manager.clone();
    let mut open = tokio::spawn(async move { open_manager.open(input).await });
    preparer
        .started
        .clone()
        .acquire_owned()
        .await
        .expect("prepare started")
        .forget();

    let completed = tokio::time::timeout(std::time::Duration::from_secs(1), &mut open).await;
    preparer.release();
    if completed.is_err() {
        tokio::time::timeout(std::time::Duration::from_secs(2), open)
            .await
            .expect("released prepare")
            .expect("open task")
            .expect("original launch");
    }
    manager.shutdown().await;
    assert!(
        completed.is_ok(),
        "the callback timeout awaited a non-cooperative aborted task"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_serialized_callbacks_cancel_never_overlaps_on_spawned_and_fences_it() {
    let root = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(SerializedCallbackState::default());
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(SerializedCallbackPreparer {
                state: state.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-serialized",
        "terminal-serialized",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    let open_manager = manager.clone();
    let open = tokio::spawn(async move { open_manager.open(input).await });
    state
        .spawned
        .acquire()
        .await
        .expect("on_spawned started")
        .forget();

    let close_manager = manager.clone();
    let close = tokio::spawn(async move {
        close_manager
            .close("thread-serialized", Some("terminal-serialized"))
            .await
            .expect("close terminal");
    });
    close.await.expect("close task");
    state.release_spawned();
    let _ = open.await.expect("open task");

    assert_eq!(
        state.maximum.load(Ordering::Acquire),
        1,
        "on_spawned and cancel overlapped"
    );
    assert!(
        !state
            .spawned_generation
            .lock()
            .expect("spawned generation lock")
            .as_ref()
            .expect("on_spawned generation")
            .is_current(),
        "late on_spawned retained publication authority after cancellation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hardening_cancellation_signal_is_exactly_once_and_independent_of_blocked_on_spawned() {
    for (terminal_id, restart) in [("blocked-close", false), ("blocked-restart", true)] {
        let root = tempfile::tempdir().expect("temp dir");
        let state = Arc::new(NonCooperativeCallbackState::default());
        let manager = TerminalManager::new(
            Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(NonCooperativeCallbackPreparer {
                    state: state.clone(),
                    prepared: AtomicUsize::new(0),
                    limit: 1,
                })),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-blocked-cleanup",
            terminal_id,
            root.path().to_path_buf(),
            80,
            24,
        );
        input.command = Some(command(true));
        let open_manager = manager.clone();
        let open = tokio::spawn(async move { open_manager.open(input).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while state.started.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("on_spawned did not start");

        let cleanup = if restart {
            let mut replacement = TerminalOpenInput::new(
                "thread-blocked-cleanup",
                terminal_id,
                root.path().to_path_buf(),
                80,
                24,
            );
            replacement.command = Some(command(true));
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                manager.restart(replacement),
            )
            .await
            .expect("restart progress exceeded the cleanup bound")
            .map(|_| ())
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                manager.close("thread-blocked-cleanup", Some(terminal_id)),
            )
            .await
            .expect("close progress exceeded the cleanup bound")
            .expect("close terminal");
            Ok(())
        };
        let generation = state
            .generation
            .lock()
            .expect("generation lock")
            .clone()
            .expect("spawn generation");
        let cancellation_reason = generation.cancellation_reason();
        state.release();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), open)
            .await
            .expect("released open")
            .expect("open task");
        cleanup.expect("replacement terminal");
        manager.shutdown().await;

        assert_eq!(
            cancellation_reason,
            Some(if restart {
                TerminalObserverCancellationReason::Restarted
            } else {
                TerminalObserverCancellationReason::Closed
            }),
            "{terminal_id}: cleanup signal was not invoked with the first lifecycle reason"
        );
        assert_eq!(
            generation.cancellation_reason(),
            cancellation_reason,
            "{terminal_id}: one-shot cleanup reason changed after later lifecycle paths"
        );
        assert!(
            !generation.is_current(),
            "{terminal_id}: blocked callback retained publication authority"
        );
    }
}

#[test]
fn hardening_non_cooperative_observer_callbacks_have_a_fixed_global_bound() {
    const CHILD_PROCESS_ENV: &str = "BIBCODE_CALLBACK_BOUND_CHILD";
    if std::env::var_os(CHILD_PROCESS_ENV).is_some() {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(8)
            .enable_all()
            .build()
            .expect("callback-bound test runtime")
            .block_on(assert_non_cooperative_observer_callback_global_bound());
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .env(CHILD_PROCESS_ENV, "1")
        .args([
            "--exact",
            "hardening_non_cooperative_observer_callbacks_have_a_fixed_global_bound",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("isolated callback-bound child process");
    assert!(
        output.status.success(),
        "callback-bound child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_non_cooperative_observer_callback_global_bound() {
    const MANAGER_COUNT: usize = 3;
    const ATTEMPTS_PER_MANAGER: usize = 8;
    const MAX_GLOBAL_RETAINED_CALLBACKS: usize = 16;

    let root = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(NonCooperativeCallbackState::default());
    let managers = (0..MANAGER_COUNT)
        .map(|_| {
            TerminalManager::new(
                Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
                TerminalManagerOptions {
                    launch_preparer: Some(Arc::new(NonCooperativeCallbackPreparer {
                        state: state.clone(),
                        prepared: AtomicUsize::new(0),
                        limit: usize::MAX,
                    })),
                    ..TerminalManagerOptions::default()
                },
            )
        })
        .collect::<Vec<_>>();
    let mut opens = Vec::new();
    for (manager_index, manager) in managers.iter().enumerate() {
        for attempt in 0..ATTEMPTS_PER_MANAGER {
            let mut input = TerminalOpenInput::new(
                format!("thread-callback-bound-{manager_index}"),
                format!("terminal-{attempt}"),
                root.path().to_path_buf(),
                80,
                24,
            );
            input.command = Some(command(true));
            let open_manager = manager.clone();
            opens.push(tokio::spawn(async move { open_manager.open(input).await }));
        }
    }
    for open in opens {
        tokio::time::timeout(std::time::Duration::from_secs(2), open)
            .await
            .expect("observer admission/callback exceeded its caller bound")
            .expect("open task")
            .expect("terminal survives observer callback failure");
    }
    let started = state.started.load(Ordering::Acquire);
    state.release();
    assert_eq!(
        started, MAX_GLOBAL_RETAINED_CALLBACKS,
        "multiple managers did not share the process-global callback admission bound"
    );
    assert!(
        state.maximum.load(Ordering::Acquire) <= MAX_GLOBAL_RETAINED_CALLBACKS,
        "non-cooperative callback worker impact exceeded the process-global bound"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while state.active.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released isolated callbacks did not finish");
    let mut follow_up = TerminalOpenInput::new(
        "thread-callback-bound-follow-up",
        "terminal-after-release",
        root.path().to_path_buf(),
        80,
        24,
    );
    follow_up.command = Some(command(true));
    managers[0]
        .open(follow_up)
        .await
        .expect("released callback capacity was not reusable");
    assert_eq!(
        state.started.load(Ordering::Acquire),
        started + 1,
        "cooperative callback completion did not release its admission permit"
    );
    for manager in managers {
        manager.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hardening_isolated_callback_runtime_has_one_blocking_worker() {
    let root = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(CallbackBlockingPoolState::default());
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(CallbackBlockingPoolPreparer {
                state: state.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-callback-blocking-budget",
        "terminal-callback-blocking-budget",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    let open_manager = manager.clone();
    let open = tokio::spawn(async move { open_manager.open(input).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while state.started.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("callback blocking worker did not start");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let started_before_release = state.started.load(Ordering::Acquire);
    let maximum_before_release = state.maximum.load(Ordering::Acquire);
    state.release();
    tokio::time::timeout(std::time::Duration::from_secs(2), open)
        .await
        .expect("released callback runtime")
        .expect("open task")
        .expect("terminal survives callback isolation");

    assert_eq!(
        started_before_release, 1,
        "one isolated callback runtime created more than one blocking worker"
    );
    assert_eq!(
        maximum_before_release, 1,
        "isolated callback blocking pool exceeded its configured thread budget"
    );
    manager.shutdown().await;
}

#[derive(Clone, Copy, Debug)]
enum DurableWorkerCleanup {
    Close,
    Restart,
    Shutdown,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hardening_observer_workers_survive_callback_and_teardown_with_generation() {
    for (cleanup, expected_reason) in [
        (
            DurableWorkerCleanup::Close,
            TerminalObserverCancellationReason::Closed,
        ),
        (
            DurableWorkerCleanup::Restart,
            TerminalObserverCancellationReason::Restarted,
        ),
        (
            DurableWorkerCleanup::Shutdown,
            TerminalObserverCancellationReason::Shutdown,
        ),
    ] {
        let root = tempfile::tempdir().expect("temp dir");
        let state = Arc::new(DurableObserverWorkerState::default());
        let manager = TerminalManager::new(
            Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(DurableObserverWorkerPreparer {
                    state: state.clone(),
                    prepared: AtomicUsize::new(0),
                })),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-durable-observer-worker",
            "terminal-durable-observer-worker",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.command = Some(command(true));
        manager
            .open(input.clone())
            .await
            .expect("observed terminal");

        for _ in 0..2 {
            state
                .started
                .acquire()
                .await
                .expect("durable worker started")
                .forget();
        }
        state
            .ping_sender
            .send(())
            .expect("ping durable worker after on_spawned returned");
        state
            .ping_seen
            .acquire()
            .await
            .expect("durable worker stayed alive after callback return")
            .forget();

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            match cleanup {
                DurableWorkerCleanup::Close => {
                    manager
                        .close(
                            "thread-durable-observer-worker",
                            Some("terminal-durable-observer-worker"),
                        )
                        .await
                        .expect("close terminal");
                }
                DurableWorkerCleanup::Restart => {
                    manager.restart(input).await.expect("replacement terminal");
                }
                DurableWorkerCleanup::Shutdown => manager.shutdown().await,
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{cleanup:?} exceeded the observer worker teardown bound"));

        assert_eq!(
            *state
                .cancellation_reasons
                .lock()
                .expect("cancellation reasons lock"),
            [expected_reason],
            "{cleanup:?} did not join the worker after delivering generation cancellation"
        );
        assert_eq!(
            state.stubborn_dropped.load(Ordering::Acquire),
            1,
            "{cleanup:?} did not abort and reap a worker that ignored cancellation"
        );
        manager.shutdown().await;
        assert_eq!(
            state
                .cancellation_reasons
                .lock()
                .expect("cancellation reasons lock")
                .len(),
            1,
            "{cleanup:?} delivered cancellation more than once"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_on_spawned_setup_has_no_ambient_disposable_tokio_runtime() {
    let root = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(ObserverSetupBoundaryState::default());
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(ObserverSetupBoundaryPreparer {
                state: state.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-observer-setup-boundary",
        "terminal-observer-setup-boundary",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    manager.open(input).await.expect("observed terminal");
    state
        .registered_worker_started
        .acquire()
        .await
        .expect("registered worker start")
        .forget();
    manager.shutdown().await;

    assert!(
        state.callback_ran.load(Ordering::Acquire),
        "on_spawned setup callback did not run"
    );
    assert!(
        !state.ambient_runtime_present.load(Ordering::Acquire),
        "on_spawned setup exposed a disposable ambient Tokio runtime"
    );
}

#[test]
fn hardening_durable_observer_workers_have_a_reusable_process_global_bound() {
    const CHILD_PROCESS_ENV: &str = "BIBCODE_DURABLE_WORKER_BOUND_CHILD";
    if std::env::var_os(CHILD_PROCESS_ENV).is_some() {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("durable-worker-bound test runtime")
            .block_on(assert_durable_observer_worker_global_bound());
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .env(CHILD_PROCESS_ENV, "1")
        .args([
            "--exact",
            "hardening_durable_observer_workers_have_a_reusable_process_global_bound",
            "--nocapture",
            "--test-threads=1",
        ])
        .output()
        .expect("isolated durable-worker-bound child process");
    assert!(
        output.status.success(),
        "durable-worker-bound child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_durable_observer_worker_global_bound() {
    const MANAGER_COUNT: usize = 3;
    const WORKERS_PER_GENERATION: usize = 8;
    const MAX_GLOBAL_DURABLE_WORKERS: usize = 16;

    let root = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(WorkerContextCaptureState::default());
    let managers = (0..MANAGER_COUNT)
        .map(|manager_index| {
            let manager = TerminalManager::new(
                Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
                TerminalManagerOptions {
                    launch_preparer: Some(Arc::new(WorkerContextCapturePreparer {
                        state: state.clone(),
                    })),
                    ..TerminalManagerOptions::default()
                },
            );
            (manager_index, manager)
        })
        .collect::<Vec<_>>();
    for (manager_index, manager) in &managers {
        let mut input = TerminalOpenInput::new(
            format!("thread-durable-worker-bound-{manager_index}"),
            "terminal-durable-worker-bound",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.command = Some(command(true));
        manager.open(input).await.expect("observed terminal");
    }

    let contexts = state.contexts.lock().expect("worker contexts lock").clone();
    assert_eq!(contexts.len(), MANAGER_COUNT);
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut accepted = 0;
    let mut rejected = 0;
    let mut unexpected_errors = Vec::new();
    for context in &contexts {
        for _ in 0..WORKERS_PER_GENERATION {
            let release = release.clone();
            let dropped = dropped.clone();
            match context.spawn(async move {
                let _drop = DurableObserverWorkerDrop { dropped };
                let (released, wake) = &*release;
                let mut released = released.lock().expect("worker release lock");
                while !*released {
                    released = wake.wait(released).expect("worker release wait");
                }
            }) {
                Ok(()) => accepted += 1,
                Err(TerminalObserverWorkerSpawnError::CapacityExceeded) => rejected += 1,
                Err(error) => unexpected_errors.push(error),
            }
        }
    }
    let release_thread = {
        let release = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            *release.0.lock().expect("worker release lock") = true;
            release.1.notify_all();
        })
    };
    let production_runtime_remained_live = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        tokio::task::yield_now(),
    )
    .await
    .is_ok();
    release_thread.join().expect("worker release thread");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while dropped.load(Ordering::Acquire) != accepted {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("durable workers did not release process capacity");

    let reused = Arc::new(tokio::sync::Semaphore::new(0));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            match contexts[0].spawn({
                let reused = reused.clone();
                async move {
                    reused.add_permits(1);
                }
            }) {
                Ok(()) => break,
                Err(TerminalObserverWorkerSpawnError::CapacityExceeded) => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("unexpected reusable worker admission failure: {error}"),
            }
        }
    })
    .await
    .expect("released durable worker capacity was not reusable");
    reused
        .acquire()
        .await
        .expect("reused durable worker")
        .forget();
    for (_, manager) in managers {
        manager.shutdown().await;
    }

    assert!(
        unexpected_errors.is_empty(),
        "unexpected durable worker admission failures: {unexpected_errors:?}"
    );
    assert_eq!(
        accepted, MAX_GLOBAL_DURABLE_WORKERS,
        "durable worker admission was not bounded process-wide across managers"
    );
    assert_eq!(
        rejected,
        MANAGER_COUNT * WORKERS_PER_GENERATION - MAX_GLOBAL_DURABLE_WORKERS,
        "durable worker capacity did not fail closed"
    );
    assert!(
        production_runtime_remained_live,
        "registered non-yielding workers exhausted the production Tokio runtime"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_pass_through_preparation_reaps_registered_workers() {
    let root = tempfile::tempdir().expect("temp dir");
    let state = Arc::new(PassThroughWorkerState::default());
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(PassThroughWorkerPreparer {
                state: state.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-pass-through-worker",
        "terminal-pass-through-worker",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    manager
        .open(input)
        .await
        .expect("pass-through terminal remains usable");

    assert_eq!(
        state.dropped.load(Ordering::Acquire),
        1,
        "pass-through preparation left a registered worker alive"
    );
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hardening_callbacks_hung_on_spawned_is_bounded_and_isolated() {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let preparer = Arc::new(FaultingPreparer::new(
        Fault::Ready,
        Fault::Hang,
        Fault::Ready,
    ));
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(preparer.clone()),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-1",
        "hung-observer",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    let open_manager = manager.clone();
    let open = tokio::spawn(async move { open_manager.open(input).await });
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        preparer.observer_started.clone().acquire_owned(),
    )
    .await
    .expect("on_spawned started")
    .expect("observer semaphore");
    permit.forget();

    let unrelated = TerminalOpenInput::new(
        "thread-1",
        "unrelated-observer",
        root.path().to_path_buf(),
        80,
        24,
    );
    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        manager.open(unrelated),
    )
    .await
    .expect("hung on_spawned must not hold the manager lifecycle")
    .expect("unrelated terminal");
    tokio::time::timeout(std::time::Duration::from_secs(2), open)
        .await
        .expect("on_spawned timeout")
        .expect("open task")
        .expect("terminal survives observer timeout");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        manager.close("thread-1", Some("hung-observer")),
    )
    .await
    .expect("blocked observer cleanup must be bounded")
    .expect("close terminal");
    assert!(
        !preparer
            .generation
            .lock()
            .expect("generation lock")
            .as_ref()
            .expect("observer generation")
            .is_current()
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn hardening_callbacks_panicking_on_spawned_does_not_escape_manager() {
    for (terminal_id, on_spawned_fault) in [("panic-on-spawned", Fault::Panic)] {
        let root = tempfile::tempdir().expect("temp dir");
        let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
        let manager = TerminalManager::new(
            backend,
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(FaultingPreparer::new(
                    Fault::Ready,
                    on_spawned_fault,
                    Fault::Ready,
                ))),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input =
            TerminalOpenInput::new("thread-1", terminal_id, root.path().to_path_buf(), 80, 24);
        input.command = Some(command(true));
        let open_manager = manager.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::spawn(async move { open_manager.open(input).await }),
        )
        .await
        .expect("callback panic boundary")
        .expect("open task must not panic")
        .expect("terminal survives observer panic");
        let close_manager = manager.clone();
        let terminal_id = terminal_id.to_owned();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::spawn(async move {
                close_manager
                    .close("thread-1", Some(&terminal_id))
                    .await
                    .expect("close terminal");
            }),
        )
        .await
        .expect("cancel panic boundary")
        .expect("close task must not panic");
        manager.shutdown().await;
    }
}

#[tokio::test]
async fn hardening_streaming_redaction_handles_every_split_longest_first_and_flushes_tail() {
    let secret = "private-long-secret";
    for split in 1..secret.len() {
        let root = tempfile::tempdir().expect("temp dir");
        let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
        let preparer = Arc::new(PlannedPreparer::new(
            Arc::new(Mutex::new(Vec::new())),
            [PreparationPlan::Prepared(PreparedPlan {
                executable: "observed-codex".to_owned(),
                args: Vec::new(),
                private_env: BTreeMap::from([
                    ("A_SHORT".to_owned(), "private".to_owned()),
                    ("Z_LONG".to_owned(), secret.to_owned()),
                ]),
                observer: Arc::new(ObserverState::default()),
            })],
        ));
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                launch_preparer: Some(preparer),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-redaction",
            format!("terminal-{split}"),
            root.path().to_path_buf(),
            80,
            24,
        );
        input.command = Some(command(true));
        manager.open(input).await.expect("terminal");
        let mut events = manager.subscribe_events();
        backend.latest().emit(&secret[..split]);
        backend.latest().emit(&secret[split..]);
        let rendered = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            let mut rendered = String::new();
            loop {
                if let Ok(bibcode_server::terminal::TerminalEvent::Output { data, .. }) =
                    events.recv().await
                {
                    rendered.push_str(&data);
                    if rendered.contains("[redacted]") || rendered.len() >= secret.len() {
                        break rendered;
                    }
                }
            }
        })
        .await
        .expect("redacted output");
        assert!(!rendered.contains(secret), "split {split}: {rendered}");
        assert_eq!(rendered, "[redacted]", "split {split}");
        let attached = manager
            .attach(bibcode_server::terminal::TerminalAttachInput::existing(
                "thread-redaction",
                format!("terminal-{split}"),
            ))
            .await
            .expect("attachment");
        assert!(!attached.initial.history.contains(secret));
        assert_eq!(attached.initial.history, "[redacted]");
        manager.shutdown().await;
    }

    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(PlannedPreparer::new(
                Arc::new(Mutex::new(Vec::new())),
                [PreparationPlan::Prepared(PreparedPlan {
                    executable: "observed-codex".to_owned(),
                    args: Vec::new(),
                    private_env: BTreeMap::from([("PRIVATE".to_owned(), secret.to_owned())]),
                    observer: Arc::new(ObserverState::default()),
                })],
            ))),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-redaction",
        "tail",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    manager.open(input).await.expect("tail terminal");
    backend.latest().emit("private-long-sec");
    backend.latest().exit(0);
    let attachment = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let attachment = manager
                .attach(bibcode_server::terminal::TerminalAttachInput::existing(
                    "thread-redaction",
                    "tail",
                ))
                .await
                .expect("tail attachment");
            if attachment.initial.status == TerminalStatus::Exited {
                break attachment;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("tail exit");
    assert!(
        !attachment.initial.history.contains("private-long-sec"),
        "{}",
        attachment.initial.history
    );
    assert!(attachment.initial.history.contains("[redacted]"));
    manager.shutdown().await;
}

#[tokio::test]
async fn hardening_diagnostics_manager_debug_never_exposes_private_values() {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(PlannedPreparer::new(
                Arc::new(Mutex::new(Vec::new())),
                [PreparationPlan::Prepared(PreparedPlan {
                    executable: "observed-codex".to_owned(),
                    args: Vec::new(),
                    private_env: BTreeMap::from([(
                        "PRIVATE".to_owned(),
                        "secret-token".to_owned(),
                    )]),
                    observer: Arc::new(ObserverState::default()),
                })],
            ))),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-debug",
        "terminal-1",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    manager.open(input).await.expect("terminal");

    let debug = format!("{manager:?}");
    assert!(!debug.contains("secret-token"), "{debug}");
    let prepared_debug = format!(
        "{:?}",
        TerminalLaunchPreparation::Prepared(PreparedTerminalLaunch {
            executable: "observed-codex".to_owned(),
            args: vec!["secret-token".to_owned()],
            private_env: BTreeMap::from([("PRIVATE".to_owned(), "secret-token".to_owned(),)]),
            observer: Box::new(RecordingObserver {
                state: Arc::new(ObserverState::default()),
            }),
        })
    );
    assert!(!prepared_debug.contains("secret-token"), "{prepared_debug}");
    manager.shutdown().await;
}

#[tokio::test]
async fn hardening_diagnostics_attempted_args_and_backend_errors_redact_private_values() {
    let root = tempfile::tempdir().expect("temp dir");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    backend.fail_next("secret-token backend secret-token");
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(PlannedPreparer::new(
                Arc::new(Mutex::new(Vec::new())),
                [PreparationPlan::Prepared(PreparedPlan {
                    executable: "observed-codex".to_owned(),
                    args: vec!["secret-token".to_owned(), "again-secret-token".to_owned()],
                    private_env: BTreeMap::from([(
                        "PRIVATE".to_owned(),
                        "secret-token".to_owned(),
                    )]),
                    observer: Arc::new(ObserverState::default()),
                })],
            ))),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-debug",
        "terminal-2",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    let error = manager
        .open(input)
        .await
        .expect_err("spawn failure")
        .to_string();
    assert!(!error.contains("secret-token"), "{error}");
    assert_eq!(error.matches("[redacted]").count(), 4, "{error}");
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn hardening_rejected_hint_diagnostics_are_bounded_to_provider_strategy_and_status() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories::default(),
    )
    .expect("supervisor");
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_target(false)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);

    supervisor
        .prepare(TerminalLaunchPreparationInput {
            executable: configured.to_string_lossy().into_owned(),
            args: vec!["private-argument".to_owned()],
            cwd: fixture.path().to_path_buf(),
            worktree_path: None,
            launch_env: BTreeMap::from([(
                "PRIVATE_TOKEN".to_owned(),
                "private-environment".to_owned(),
            )]),
            activity: ProviderTerminalActivityLaunch {
                driver_kind: "codex".to_owned(),
                provider_instance_id: "private_instance".to_owned(),
            },
            generation: TerminalObserverGeneration::new(
                "private-thread".to_owned(),
                "private-terminal".to_owned(),
            ),
        })
        .await;

    let diagnostic = capture.text();
    assert!(diagnostic.contains("provider=codex"), "{diagnostic}");
    assert!(
        diagnostic.contains("strategy=\"remote-app-server\""),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("status=\"unknown_instance\""),
        "{diagnostic}"
    );
    for secret in [
        "private_instance",
        "private-argument",
        "private-environment",
        "PRIVATE_TOKEN",
        "private-thread",
        "private-terminal",
    ] {
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }
}

#[tokio::test]
async fn hardening_live_authority_next_prepare_uses_reloaded_provider_settings() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let settings_root = fixture.path().join("settings");
    std::fs::create_dir_all(&settings_root).expect("settings root");
    let mut enabled = ProviderSettingsState::default();
    enabled.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    enabled.provider_instances.insert(
        "codex_deleted".to_owned(),
        ProviderInstanceState {
            driver: "codex".to_owned(),
            enabled: true,
            config: serde_json::json!({
                "binaryPath": configured.to_string_lossy(),
            }),
            ..ProviderInstanceState::default()
        },
    );
    std::fs::write(
        settings_root.join("settings.json"),
        serde_json::to_vec(&enabled).expect("settings JSON"),
    )
    .expect("enabled settings");
    let store = ProviderSettingsStore::new(&settings_root);
    store.get().await.expect("initial settings");
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let factory = Arc::new(RecordingFactory::default());
    let controller = AgentActivityController::new(true);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let supervisor = ProviderTerminalActivitySupervisor::new_with_authority(
        Arc::new(ProviderSettingsInventoryAuthority::new(store.clone())),
        controller,
        projection,
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let generation =
        TerminalObserverGeneration::new("thread-live".to_owned(), "terminal-live".to_owned());
    let input = || TerminalLaunchPreparationInput {
        executable: configured.to_string_lossy().into_owned(),
        args: Vec::new(),
        cwd: fixture.path().to_path_buf(),
        worktree_path: None,
        launch_env: BTreeMap::new(),
        activity: ProviderTerminalActivityLaunch {
            driver_kind: "codex".to_owned(),
            provider_instance_id: "codex_deleted".to_owned(),
        },
        generation: generation.clone(),
    };
    supervisor.prepare(input()).await;
    assert_eq!(factory.scopes.lock().expect("scopes lock").len(), 1);

    enabled.provider_instances.remove("codex_deleted");
    std::fs::write(
        settings_root.join("settings.json"),
        serde_json::to_vec(&enabled).expect("settings JSON"),
    )
    .expect("deleted-instance settings");
    supervisor.prepare(input()).await;

    assert_eq!(
        factory.scopes.lock().expect("scopes lock").len(),
        1,
        "the next prepare must reject a provider deleted from hot-reloaded settings"
    );
}

#[tokio::test]
async fn hardening_generation_scope_is_published_only_after_factory_correlation() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let inventory = ProviderTerminalInventory::from_settings(&settings);
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let factory = Arc::new(RecordingFactory::default());
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings,
        inventory,
        projection.clone(),
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let generation =
        TerminalObserverGeneration::new("thread-correlation".to_owned(), "terminal-1".to_owned());
    let logical_scope = ActivityScopeRef::Terminal {
        thread_id: "thread-correlation".to_owned(),
        terminal_id: "terminal-1".to_owned(),
    };

    supervisor
        .prepare(TerminalLaunchPreparationInput {
            executable: configured.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: fixture.path().to_path_buf(),
            worktree_path: None,
            launch_env: BTreeMap::new(),
            activity: ProviderTerminalActivityLaunch {
                driver_kind: "codex".to_owned(),
                provider_instance_id: "codex".to_owned(),
            },
            generation: generation.clone(),
        })
        .await;

    assert!(
        projection.snapshot(&logical_scope).await.is_err(),
        "preparation must not invent pre-handshake capabilities"
    );
    let publisher = factory
        .publishers
        .lock()
        .expect("publishers lock")
        .pop()
        .expect("factory publisher");
    assert!(
        publisher
            .publish_correlated(
                "codex",
                Some("codex"),
                ActivityCapabilities::structured_full(false),
            )
            .await
            .expect("correlated publication")
    );
    let snapshot = projection
        .snapshot(&logical_scope)
        .await
        .expect("published terminal scope");
    assert_eq!(snapshot.scope_id, generation.observation().scope_id());
    assert_eq!(
        snapshot.capabilities,
        ActivityCapabilities::structured_full(false)
    );
}

#[tokio::test]
async fn hardening_input_debug_redacts_preparation_and_factory_chain_values() {
    const SENTINEL: &str = "sentinel-private-debug-value";
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let factory = Arc::new(RecordingFactory::default());
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let input = TerminalLaunchPreparationInput {
        executable: configured.to_string_lossy().into_owned(),
        args: vec![SENTINEL.to_owned()],
        cwd: fixture.path().to_path_buf(),
        worktree_path: None,
        launch_env: BTreeMap::from([("PRIVATE_INPUT".to_owned(), SENTINEL.to_owned())]),
        activity: ProviderTerminalActivityLaunch {
            driver_kind: "codex".to_owned(),
            provider_instance_id: "codex".to_owned(),
        },
        generation: TerminalObserverGeneration::new(
            "thread-debug-chain".to_owned(),
            "terminal-debug-chain".to_owned(),
        ),
    };
    let direct_debug = format!("{input:?}");
    supervisor.prepare(input).await;
    let factory_debug = factory
        .input_debugs
        .lock()
        .expect("input debugs lock")
        .last()
        .cloned()
        .expect("factory debug");

    assert!(!direct_debug.contains(SENTINEL), "{direct_debug}");
    assert!(!factory_debug.contains(SENTINEL), "{factory_debug}");
    assert!(direct_debug.contains("arg_count: 1"), "{direct_debug}");
    assert!(direct_debug.contains("PRIVATE_INPUT"), "{direct_debug}");
    assert!(factory_debug.contains("arg_count: 1"), "{factory_debug}");
    assert!(factory_debug.contains("PRIVATE_INPUT"), "{factory_debug}");
}

#[test]
fn hardening_generation_scope_native_ids_are_generation_namespaced_and_bounded() {
    let generation =
        TerminalObserverGeneration::new("thread-native".to_owned(), "terminal-native".to_owned());
    assert_eq!(
        generation.observation().namespace_native_id("actor:42"),
        format!("{}:actor:42", generation.observation().generation_id())
    );
    let long = generation
        .observation()
        .namespace_native_id(&"native".repeat(100));
    assert!(long.len() <= 256, "namespaced native id is unbounded");
    assert_ne!(
        long,
        generation
            .observation()
            .namespace_native_id(&"other".repeat(100))
    );
}

#[derive(Debug, Default)]
struct CodexProbeFixtureRunner {
    calls: Mutex<Vec<Vec<String>>>,
    outputs: Mutex<VecDeque<CodexProbeOutput>>,
}

impl CodexCapabilityProbeRunner for CodexProbeFixtureRunner {
    fn run(
        &self,
        _executable: &std::path::Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<CodexProbeOutput, String>> + Send + '_>> {
        Box::pin(async move {
            self.calls.lock().expect("probe calls lock").push(args);
            self.outputs
                .lock()
                .expect("probe outputs lock")
                .pop_front()
                .ok_or_else(|| "missing probe output".to_owned())
        })
    }
}

#[derive(Debug, Default)]
struct CodexResultProbeRunner {
    calls: Mutex<Vec<Vec<String>>>,
    outputs: Mutex<VecDeque<Result<CodexProbeOutput, String>>>,
}

impl CodexCapabilityProbeRunner for CodexResultProbeRunner {
    fn run(
        &self,
        _executable: &std::path::Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<CodexProbeOutput, String>> + Send + '_>> {
        Box::pin(async move {
            self.calls.lock().expect("probe calls lock").push(args);
            self.outputs
                .lock()
                .expect("probe outputs lock")
                .pop_front()
                .unwrap_or_else(|| Err("missing probe output".to_owned()))
        })
    }
}

#[derive(Debug)]
struct CodexFixtureProcess {
    terminated: AtomicBool,
}

impl CodexHelperProcess for CodexFixtureProcess {
    fn terminate(&self) {
        self.terminated.store(true, Ordering::Release);
    }
}

#[derive(Debug, Default)]
struct CodexFixtureHelperLauncher {
    launches: Mutex<Vec<CodexHelperLaunch>>,
    fail: AtomicBool,
    processes: Mutex<Vec<Arc<CodexFixtureProcess>>>,
    timeline: Arc<Mutex<Vec<&'static str>>>,
}

impl CodexHelperLauncher for CodexFixtureHelperLauncher {
    fn start(
        &self,
        launch: CodexHelperLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn CodexHelperProcess>, String>> + Send + '_>>
    {
        Box::pin(async move {
            self.timeline
                .lock()
                .expect("Codex topology timeline lock")
                .push("helper");
            self.launches
                .lock()
                .expect("helper launches lock")
                .push(launch);
            if self.fail.load(Ordering::Acquire) {
                return Err("injected helper failure".to_owned());
            }
            let process = Arc::new(CodexFixtureProcess {
                terminated: AtomicBool::new(false),
            });
            self.processes
                .lock()
                .expect("helper processes lock")
                .push(process.clone());
            Ok(process as Arc<dyn CodexHelperProcess>)
        })
    }
}

#[derive(Debug, Default)]
struct CodexFixtureRemoteState {
    endpoints: Mutex<Vec<String>>,
    writes: Mutex<Vec<Value>>,
    events: Arc<Mutex<VecDeque<Value>>>,
    scripted_events: Mutex<VecDeque<Arc<Mutex<VecDeque<Value>>>>>,
    resume_responses: Mutex<VecDeque<Result<Value, String>>>,
    resume_events: Mutex<VecDeque<Vec<Value>>>,
    request_responses: Mutex<BTreeMap<String, VecDeque<Result<Value, String>>>>,
    request_events: Mutex<BTreeMap<String, VecDeque<Vec<Value>>>>,
    request_delays: Mutex<BTreeMap<String, VecDeque<std::time::Duration>>>,
    late_request_delays: Mutex<BTreeMap<String, VecDeque<std::time::Duration>>>,
    connect_delay: Mutex<std::time::Duration>,
    connect_error: Mutex<Option<String>>,
    request_errors: Mutex<VecDeque<String>>,
    next_errors: Arc<Mutex<VecDeque<String>>>,
    clean_close_requests: AtomicUsize,
    completed_clean_closes: AtomicUsize,
    timeline: Arc<Mutex<Vec<&'static str>>>,
    active_resume_requests: AtomicUsize,
    maximum_active_resume_requests: AtomicUsize,
    completed_resume_responses: AtomicUsize,
    completed_late_request_responses: Arc<AtomicUsize>,
    request_buffered_deliveries: AtomicUsize,
    active_connections: AtomicUsize,
    maximum_active_connections: AtomicUsize,
}

struct CodexFixtureResumeRequestGuard {
    state: Arc<CodexFixtureRemoteState>,
}

impl CodexFixtureResumeRequestGuard {
    fn new(state: Arc<CodexFixtureRemoteState>) -> Self {
        let active = state.active_resume_requests.fetch_add(1, Ordering::AcqRel) + 1;
        state
            .maximum_active_resume_requests
            .fetch_max(active, Ordering::AcqRel);
        Self { state }
    }
}

impl Drop for CodexFixtureResumeRequestGuard {
    fn drop(&mut self) {
        self.state
            .active_resume_requests
            .fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct CodexFixtureRemoteClient {
    state: Arc<CodexFixtureRemoteState>,
    events: Arc<Mutex<VecDeque<Value>>>,
    request_buffered_events: VecDeque<Value>,
}

impl Drop for CodexFixtureRemoteClient {
    fn drop(&mut self) {
        self.state.active_connections.fetch_sub(1, Ordering::AcqRel);
    }
}

impl CodexRemoteClient for CodexFixtureRemoteClient {
    fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let method = method.to_owned();
        Box::pin(async move {
            let _resume_request_guard = (method == "thread/resume")
                .then(|| CodexFixtureResumeRequestGuard::new(self.state.clone()));
            self.state
                .timeline
                .lock()
                .expect("Codex topology timeline lock")
                .push(if method == "thread/resume" {
                    "resume"
                } else {
                    "initialize"
                });
            self.state
                .writes
                .lock()
                .expect("remote writes lock")
                .push(serde_json::json!({ "method": method, "params": params.clone() }));
            if let Some(delay) = self
                .state
                .late_request_delays
                .lock()
                .expect("late request delays lock")
                .get_mut(&method)
                .and_then(VecDeque::pop_front)
            {
                let completed = self.state.completed_late_request_responses.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    completed.fetch_add(1, Ordering::AcqRel);
                });
            }
            let delay = self
                .state
                .request_delays
                .lock()
                .expect("request delays lock")
                .get_mut(&method)
                .and_then(VecDeque::pop_front)
                .unwrap_or_default();
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if let Some(events) = self
                .state
                .request_events
                .lock()
                .expect("request events lock")
                .get_mut(&method)
                .and_then(VecDeque::pop_front)
            {
                self.request_buffered_events.extend(events);
            }
            if let Some(error) = self
                .state
                .request_errors
                .lock()
                .expect("request errors lock")
                .pop_front()
            {
                if method == "thread/resume" {
                    self.state
                        .completed_resume_responses
                        .fetch_add(1, Ordering::AcqRel);
                }
                return Err(error);
            }
            if method == "thread/resume" {
                if params.get("excludeTurns").is_some() {
                    self.state
                        .completed_resume_responses
                        .fetch_add(1, Ordering::AcqRel);
                    return Err(
                        "thread/resume excludeTurns requires the experimental API".to_owned()
                    );
                }
                if let Some(events) = self
                    .state
                    .resume_events
                    .lock()
                    .expect("resume events lock")
                    .pop_front()
                {
                    self.events
                        .lock()
                        .expect("remote events lock")
                        .extend(events);
                }
                if let Some(response) = self
                    .state
                    .resume_responses
                    .lock()
                    .expect("resume responses lock")
                    .pop_front()
                {
                    self.state
                        .completed_resume_responses
                        .fetch_add(1, Ordering::AcqRel);
                    return response;
                }
                let thread_id = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "resume missing threadId".to_owned())?;
                self.state
                    .completed_resume_responses
                    .fetch_add(1, Ordering::AcqRel);
                return Ok(serde_json::json!({
                    "thread": {
                        "id": thread_id,
                        "parentThreadId": null,
                        "turns": [],
                    },
                }));
            }
            if let Some(response) = self
                .state
                .request_responses
                .lock()
                .expect("request responses lock")
                .get_mut(&method)
                .and_then(VecDeque::pop_front)
            {
                return response;
            }
            Ok(match method.as_str() {
                "thread/list" => serde_json::json!({"data": [], "nextCursor": null}),
                "thread/backgroundTerminals/list" => {
                    serde_json::json!({"data": [], "nextCursor": null})
                }
                _ => serde_json::json!({}),
            })
        })
    }

    fn notify(
        &mut self,
        method: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let method = method.to_owned();
        Box::pin(async move {
            self.state
                .timeline
                .lock()
                .expect("Codex topology timeline lock")
                .push("initialized");
            self.state
                .writes
                .lock()
                .expect("remote writes lock")
                .push(serde_json::json!({ "method": method }));
            Ok(())
        })
    }

    fn drain_request_buffered_notifications(&mut self) -> Vec<Value> {
        self.state
            .request_buffered_deliveries
            .fetch_add(self.request_buffered_events.len(), Ordering::AcqRel);
        self.request_buffered_events.drain(..).collect()
    }

    fn next(&mut self) -> Pin<Box<dyn Future<Output = Result<Option<Value>, String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                if let Some(error) = self
                    .state
                    .next_errors
                    .lock()
                    .expect("next errors lock")
                    .pop_front()
                {
                    return Err(error);
                }
                if self
                    .state
                    .clean_close_requests
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                        pending.checked_sub(1)
                    })
                    .is_ok()
                {
                    self.state
                        .completed_clean_closes
                        .fetch_add(1, Ordering::Release);
                    return Ok(None);
                }
                if let Some(event) = self.request_buffered_events.pop_front() {
                    self.state
                        .request_buffered_deliveries
                        .fetch_add(1, Ordering::AcqRel);
                    return Ok(Some(event));
                }
                if let Some(event) = self.events.lock().expect("remote events lock").pop_front() {
                    return Ok(Some(event));
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    }
}

#[derive(Debug)]
struct CodexFixtureRemoteFactory {
    state: Arc<CodexFixtureRemoteState>,
}

impl CodexRemoteClientFactory for CodexFixtureRemoteFactory {
    fn connect(
        &self,
        endpoint: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn CodexRemoteClient>, String>> + Send + '_>> {
        let endpoint = endpoint.to_owned();
        Box::pin(async move {
            let delay = *self.state.connect_delay.lock().expect("connect delay lock");
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            self.state
                .timeline
                .lock()
                .expect("Codex topology timeline lock")
                .push("connect");
            self.state
                .endpoints
                .lock()
                .expect("remote endpoints lock")
                .push(endpoint);
            if let Some(error) = self
                .state
                .connect_error
                .lock()
                .expect("connect error lock")
                .take()
            {
                return Err(error);
            }
            let events = self
                .state
                .scripted_events
                .lock()
                .expect("scripted Codex events lock")
                .pop_front()
                .unwrap_or_else(|| self.state.events.clone());
            let active_connections =
                self.state.active_connections.fetch_add(1, Ordering::AcqRel) + 1;
            self.state
                .maximum_active_connections
                .fetch_max(active_connections, Ordering::AcqRel);
            Ok(Box::new(CodexFixtureRemoteClient {
                state: self.state.clone(),
                events,
                request_buffered_events: VecDeque::new(),
            }) as Box<dyn CodexRemoteClient>)
        })
    }
}

fn codex_remote_fixture() -> Value {
    let mut fixture: Value = serde_json::from_str(include_str!(
        "fixtures/provider-terminal/codex-remote-handshake.json"
    ))
    .expect("Codex remote handshake fixture");
    fixture["initializeRequest"]["params"]["clientInfo"]["version"] =
        Value::String(env!("CARGO_PKG_VERSION").to_owned());
    fixture
}

fn codex_root_notification(fixture: &Value, root_id: &str, cwd: &std::path::Path) -> Value {
    let mut notification = fixture["rootNotification"].clone();
    notification["params"]["thread"]["id"] = Value::String(root_id.to_owned());
    notification["params"]["thread"]["sessionId"] = Value::String(root_id.to_owned());
    notification["params"]["thread"]["cwd"] = Value::String(cwd.to_string_lossy().into_owned());
    notification
}

fn script_codex_root(
    state: &CodexFixtureRemoteState,
    fixture: &Value,
    root_id: &str,
    cwd: &std::path::Path,
) {
    state
        .scripted_events
        .lock()
        .expect("scripted events lock")
        .push_back(Arc::new(Mutex::new(VecDeque::from([
            codex_root_notification(fixture, root_id, cwd),
        ]))));
}

fn codex_resume_request_count(state: &CodexFixtureRemoteState) -> usize {
    state
        .writes
        .lock()
        .expect("remote writes lock")
        .iter()
        .filter(|write| write["method"] == "thread/resume")
        .count()
}

async fn wait_for_codex_resume_request(state: &CodexFixtureRemoteState) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if codex_resume_request_count(state) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resume request");
}

async fn wait_for_codex_resume_response(state: &CodexFixtureRemoteState) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if state.completed_resume_responses.load(Ordering::Acquire) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resume response");
}

fn codex_fixture_probe(fixture: &Value) -> Arc<CachedCodexCapabilityProbe> {
    Arc::new(CachedCodexCapabilityProbe::new(Arc::new(
        CodexProbeFixtureRunner {
            calls: Mutex::new(Vec::new()),
            outputs: Mutex::new(
                ["versionOutput", "rootHelp", "appServerHelp"]
                    .into_iter()
                    .map(|field| CodexProbeOutput {
                        success: true,
                        stdout: fixture[field].as_str().expect("probe fixture").to_owned(),
                        stderr: String::new(),
                    })
                    .collect(),
            ),
        },
    )))
}

async fn codex_fixture_supervisor(
    root: &tempfile::TempDir,
    configured: &std::path::Path,
    helper: Arc<CodexFixtureHelperLauncher>,
    remote: Arc<CodexFixtureRemoteFactory>,
) -> (ProviderTerminalActivitySupervisor, ActivityProjection) {
    codex_fixture_supervisor_with_reattach_timeout(root, configured, helper, remote, None).await
}

async fn codex_fixture_supervisor_with_reattach_timeout(
    root: &tempfile::TempDir,
    configured: &std::path::Path,
    helper: Arc<CodexFixtureHelperLauncher>,
    remote: Arc<CodexFixtureRemoteFactory>,
    reattach_timeout: Option<std::time::Duration>,
) -> (ProviderTerminalActivitySupervisor, ActivityProjection) {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let fixture = codex_remote_fixture();
    let probe = codex_fixture_probe(&fixture);
    let factory = Arc::new(match reattach_timeout {
        Some(reattach_timeout) => CodexTerminalObserverFactory::new_with_reattach_timeout(
            probe,
            helper,
            remote,
            reattach_timeout,
        ),
        None => CodexTerminalObserverFactory::new(probe, helper, remote),
    });
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("Codex fixture supervisor");
    (supervisor, projection)
}

fn codex_fixture_open_input(
    fixture: &Value,
    configured: &std::path::Path,
    root: &tempfile::TempDir,
    terminal_id: &str,
) -> TerminalOpenInput {
    let mut input = TerminalOpenInput::new(
        "thread-codex",
        terminal_id,
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": configured,
            "args": fixture["originalArgs"],
            "label": "Codex",
            "activity": {
                "driverKind": "codex",
                "providerInstanceId": "codex",
            },
        }))
        .expect("Codex fixture launch"),
    );
    input
}

async fn open_codex_fixture_terminal(
    root: &tempfile::TempDir,
    configured: &std::path::Path,
    remote_state: Arc<CodexFixtureRemoteState>,
    terminal_id: &str,
) -> (TerminalManager, ActivityProjection, ActivityScopeRef) {
    open_codex_fixture_terminal_with_reattach_timeout(
        root,
        configured,
        remote_state,
        terminal_id,
        None,
    )
    .await
}

async fn open_codex_fixture_terminal_with_reattach_timeout(
    root: &tempfile::TempDir,
    configured: &std::path::Path,
    remote_state: Arc<CodexFixtureRemoteState>,
    terminal_id: &str,
    reattach_timeout: Option<std::time::Duration>,
) -> (TerminalManager, ActivityProjection, ActivityScopeRef) {
    let fixture = codex_remote_fixture();
    let (supervisor, projection) = codex_fixture_supervisor_with_reattach_timeout(
        root,
        configured,
        Arc::new(CodexFixtureHelperLauncher::default()),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state,
        }),
        reattach_timeout,
    )
    .await;
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    manager
        .open(codex_fixture_open_input(
            &fixture,
            configured,
            root,
            terminal_id,
        ))
        .await
        .expect("Codex terminal");
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: terminal_id.to_owned(),
    };
    (manager, projection, scope)
}

#[cfg(unix)]
fn count_codex_method_calls(state: &CodexFixtureRemoteState, method: &str) -> usize {
    state
        .writes
        .lock()
        .expect("remote writes lock")
        .iter()
        .filter(|write| write["method"] == method)
        .count()
}

#[cfg(unix)]
fn count_codex_history_calls(state: &CodexFixtureRemoteState) -> usize {
    [
        "thread/list",
        "thread/read",
        "thread/backgroundTerminals/list",
    ]
    .into_iter()
    .map(|method| count_codex_method_calls(state, method))
    .sum()
}

#[cfg(unix)]
fn codex_detail_notification(root_id: &str, item_id: &str, detail: &str) -> Value {
    serde_json::json!({
        "method": "item/completed",
        "params": {
            "threadId": root_id,
            "turnId": format!("{item_id}-turn"),
            "item": {"type": "agentMessage", "id": item_id, "text": detail}
        }
    })
}

#[cfg(unix)]
async fn wait_for_codex_initial_live(projection: &ActivityProjection, scope: &ActivityScopeRef) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if projection
                .snapshot(scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial Codex observation");
}

#[cfg(unix)]
async fn wait_for_codex_root_detail(
    projection: &ActivityProjection,
    scope: &ActivityScopeRef,
    root_id: &str,
    expected: &str,
) -> Vec<String> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(scope).await
                && let Ok(detail) = projection
                    .list_detail(
                        scope,
                        &snapshot.scope_id,
                        ActivityRecordKind::Actor,
                        &format!("codex:thread:{root_id}"),
                        None,
                        50,
                    )
                    .await
            {
                let details = detail
                    .entries
                    .iter()
                    .filter_map(|entry| entry.detail.clone())
                    .collect::<Vec<_>>();
                if details.iter().any(|detail| detail == expected) {
                    break details;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("post-barrier Codex detail")
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_codex_retains_connection_and_crosses_ordered_barrier() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_root_notification(
            &fixture,
            "terminal-retained-root",
            root.path(),
        ));
    let (manager, projection, scope) = open_codex_fixture_terminal_with_reattach_timeout(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-retained",
        Some(std::time::Duration::from_millis(500)),
    )
    .await;
    wait_for_codex_initial_live(&projection, &scope).await;
    let history_calls_before = count_codex_history_calls(&remote_state);
    let connections_before = remote_state
        .endpoints
        .lock()
        .expect("Codex endpoints")
        .len();

    let disabled = manager.set_agent_activity_enabled(false).await;
    assert_eq!((disabled.stopped, disabled.dormant), (1, 1));
    remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_detail_notification(
            "terminal-retained-root",
            "disabled-item",
            "disabled-detail",
        ));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !remote_state.events.lock().expect("Codex events").is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dormant Codex notification drain");
    remote_state
        .request_events
        .lock()
        .expect("Codex request events")
        .entry("account/read".to_owned())
        .or_default()
        .push_back(vec![codex_detail_notification(
            "terminal-retained-root",
            "pre-barrier-item",
            "pre-barrier-detail",
        )]);

    let enabled = manager.set_agent_activity_enabled(true).await;
    assert_eq!((enabled.resumed, enabled.failed), (1, 0));
    assert_eq!(
        remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len(),
        connections_before,
        "healthy enable reuses the retained WebSocket",
    );
    assert_eq!(
        count_codex_history_calls(&remote_state),
        history_calls_before
    );
    let account_reads = remote_state
        .writes
        .lock()
        .expect("remote writes lock")
        .iter()
        .filter(|write| write["method"] == "account/read")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        account_reads,
        vec![serde_json::json!({
            "method": "account/read",
            "params": {"refreshToken": false},
        })],
        "enable barrier request must be exact",
    );

    remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_detail_notification(
            "terminal-retained-root",
            "post-barrier-item",
            "post-barrier-detail",
        ));
    let details = wait_for_codex_root_detail(
        &projection,
        &scope,
        "terminal-retained-root",
        "post-barrier-detail",
    )
    .await;
    assert!(
        details
            .iter()
            .all(|detail| !matches!(detail.as_str(), "disabled-detail" | "pre-barrier-detail")),
        "disabled or pre-barrier Codex detail leaked: {details:?}",
    );
    manager
        .close("thread-codex", Some("terminal-retained"))
        .await
        .expect("close retained Codex terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_codex_barrier_failure_stays_dormant_then_recovers() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_root_notification(
            &fixture,
            "terminal-barrier-root",
            root.path(),
        ));
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let (supervisor, projection) = codex_fixture_supervisor_with_reattach_timeout(
        &root,
        &configured,
        helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
        Some(std::time::Duration::from_millis(500)),
    )
    .await;
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-barrier",
        ))
        .await
        .expect("Codex terminal");
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-barrier".to_owned(),
    };
    wait_for_codex_initial_live(&projection, &scope).await;
    let history_calls_before = count_codex_history_calls(&remote_state);
    let connections_before = remote_state
        .endpoints
        .lock()
        .expect("Codex endpoints")
        .len();
    let process = backend.latest();

    let disabled = manager.set_agent_activity_enabled(false).await;
    assert_eq!((disabled.stopped, disabled.dormant), (1, 1));
    remote_state
        .request_responses
        .lock()
        .expect("Codex request responses")
        .insert(
            "account/read".to_owned(),
            VecDeque::from([
                Err("injected account/read error".to_owned()),
                Ok(serde_json::json!({"account": null})),
            ]),
        );
    let failed = manager.set_agent_activity_enabled(true).await;
    assert_eq!(
        (failed.resumed, failed.failed, failed.unavailable),
        (0, 1, 1)
    );
    assert!(!process.killed.load(Ordering::Acquire));
    assert!(
        !helper.processes.lock().expect("Codex helper processes")[0]
            .terminated
            .load(Ordering::Acquire),
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while count_codex_method_calls(&remote_state, "account/read") < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same-generation account/read retry");
    let recovered = manager.set_agent_activity_enabled(true).await;
    assert_eq!((recovered.resumed, recovered.failed), (1, 0));
    assert_eq!(recovered.epochs.codex, failed.epochs.codex);
    assert_eq!(
        remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len(),
        connections_before,
    );
    assert_eq!(
        count_codex_history_calls(&remote_state),
        history_calls_before
    );
    manager
        .close("thread-codex", Some("terminal-barrier"))
        .await
        .expect("close barrier Codex terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_codex_stale_epoch_barrier_cannot_mark_replacement_live() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_root_notification(
            &fixture,
            "terminal-stale-root",
            root.path(),
        ));
    let (manager, projection, scope) = open_codex_fixture_terminal_with_reattach_timeout(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-stale",
        Some(std::time::Duration::from_millis(500)),
    )
    .await;
    let manager = Arc::new(manager);
    wait_for_codex_initial_live(&projection, &scope).await;
    let disabled = manager.set_agent_activity_enabled(false).await;
    assert_eq!((disabled.stopped, disabled.dormant), (1, 1));
    remote_state
        .request_delays
        .lock()
        .expect("Codex request delays")
        .insert(
            "account/read".to_owned(),
            VecDeque::from([std::time::Duration::from_millis(400)]),
        );
    remote_state
        .late_request_delays
        .lock()
        .expect("late request delays")
        .insert(
            "account/read".to_owned(),
            VecDeque::from([std::time::Duration::from_millis(250)]),
        );

    let enabling_manager = manager.clone();
    let stale_enable =
        tokio::spawn(async move { enabling_manager.set_agent_activity_enabled(true).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while count_codex_method_calls(&remote_state, "account/read") == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old-epoch barrier request");
    let disabled_again = manager.set_agent_activity_enabled(false).await;
    assert_eq!((disabled_again.stopped, disabled_again.dormant), (1, 1));
    remote_state
        .next_errors
        .lock()
        .expect("next errors")
        .push_back("old connection lost".to_owned());
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len()
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement Codex connection");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while remote_state
            .completed_late_request_responses
            .load(Ordering::Acquire)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old connection delayed barrier response");

    let dormant = manager.set_agent_activity_enabled(false).await;
    assert_eq!((dormant.dormant, dormant.failed), (1, 0));
    assert_eq!(dormant.epochs.codex, 1);
    let barriers_before_new_epoch = count_codex_method_calls(&remote_state, "account/read");
    let enabled = manager.set_agent_activity_enabled(true).await;
    assert_eq!((enabled.resumed, enabled.failed), (1, 0));
    assert_eq!(enabled.epochs.codex, 1);
    assert_eq!(
        count_codex_method_calls(&remote_state, "account/read"),
        barriers_before_new_epoch + 1,
        "the replacement epoch must cross its own barrier",
    );
    let stale = tokio::time::timeout(std::time::Duration::from_secs(2), stale_enable)
        .await
        .expect("stale enable acknowledgement timeout")
        .expect("stale enable task");
    assert_eq!((stale.resumed, stale.failed), (0, 1));
    manager
        .close("thread-codex", Some("terminal-stale"))
        .await
        .expect("close stale Codex terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_clean_close_before_root_discovery_reconnects_on_a_fresh_epoch() {
    // Mutations caught:
    // - polling the first client's permanent EOF instead of reconnecting;
    // - continuing root discovery on the stale transport boundary;
    // - reconnecting without advancing the observation epoch.
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted Codex events")
        .extend([
            Arc::new(Mutex::new(VecDeque::new())),
            Arc::new(Mutex::new(VecDeque::from([codex_root_notification(
                &fixture,
                "terminal-pre-root-close-root",
                root.path(),
            )]))),
        ]);
    remote_state
        .clean_close_requests
        .store(1, Ordering::Release);

    let (manager, projection, scope) = open_codex_fixture_terminal_with_reattach_timeout(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-pre-root-close",
        Some(std::time::Duration::from_millis(750)),
    )
    .await;
    wait_for_codex_initial_live(&projection, &scope).await;

    assert_eq!(
        remote_state.completed_clean_closes.load(Ordering::Acquire),
        1
    );
    assert_eq!(
        remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len(),
        2,
        "root discovery must continue only on one replacement connection"
    );
    assert_eq!(codex_resume_request_count(&remote_state), 1);
    assert_eq!(
        remote_state
            .maximum_active_connections
            .load(Ordering::Acquire),
        1,
        "the closed discovery client must be dropped before reconnecting"
    );
    let observed = manager.set_agent_activity_enabled(true).await;
    assert_eq!((observed.resumed, observed.failed), (1, 0));
    assert_eq!(observed.epochs.codex, 1);

    manager
        .close("thread-codex", Some("terminal-pre-root-close"))
        .await
        .expect("close pre-root clean-close Codex terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_pre_root_reconnect_failure_publishes_unavailable_and_parks() {
    // Mutations caught:
    // - retrying past the reattach deadline;
    // - retaining or polling the closed discovery client;
    // - failing to acknowledge the lost epoch as unavailable.
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted Codex events")
        .push_back(Arc::new(Mutex::new(VecDeque::new())));
    let (manager, _projection, _scope) = open_codex_fixture_terminal_with_reattach_timeout(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-pre-root-unavailable",
        Some(std::time::Duration::from_millis(50)),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len()
            != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial Codex connection");
    *remote_state
        .connect_delay
        .lock()
        .expect("connect delay lock") = std::time::Duration::from_millis(200);
    remote_state
        .clean_close_requests
        .store(1, Ordering::Release);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while remote_state.completed_clean_closes.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pre-root clean close");

    let unavailable = manager.set_agent_activity_enabled(true).await;
    assert_eq!(
        (
            unavailable.resumed,
            unavailable.failed,
            unavailable.unavailable
        ),
        (0, 1, 1)
    );
    assert_eq!(unavailable.epochs.codex, 1);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len(),
        1,
        "failed pre-root recovery must stop reconnecting at the deadline"
    );
    assert_eq!(
        remote_state.active_connections.load(Ordering::Acquire),
        0,
        "failed recovery must not retain the closed client"
    );

    manager
        .close("thread-codex", Some("terminal-pre-root-unavailable"))
        .await
        .expect("close unavailable pre-root Codex terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_codex_clean_close_advances_epoch_and_recovers() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_root_notification(
            &fixture,
            "terminal-clean-close-root",
            root.path(),
        ));
    let (manager, projection, scope) = open_codex_fixture_terminal_with_reattach_timeout(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-clean-close",
        Some(std::time::Duration::from_millis(750)),
    )
    .await;
    wait_for_codex_initial_live(&projection, &scope).await;
    let history_calls_before = count_codex_history_calls(&remote_state);
    assert_eq!(codex_resume_request_count(&remote_state), 1);
    *remote_state
        .connect_delay
        .lock()
        .expect("connect delay lock") = std::time::Duration::from_millis(300);

    remote_state
        .clean_close_requests
        .fetch_add(1, Ordering::Release);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while remote_state.completed_clean_closes.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("clean Codex close delivery");

    let unavailable = manager.set_agent_activity_enabled(true).await;
    assert_eq!(
        (
            unavailable.resumed,
            unavailable.failed,
            unavailable.unavailable
        ),
        (0, 1, 1)
    );
    assert_eq!(unavailable.epochs.codex, 1);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while remote_state
            .endpoints
            .lock()
            .expect("Codex endpoints")
            .len()
            < 2
            || codex_resume_request_count(&remote_state) < 2
            || count_codex_method_calls(&remote_state, "account/read") == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Codex clean-close recovery");

    let recovered = manager.set_agent_activity_enabled(true).await;
    assert_eq!((recovered.resumed, recovered.failed), (1, 0));
    assert_eq!(recovered.epochs.codex, 1);
    assert_eq!(codex_resume_request_count(&remote_state), 2);
    assert_eq!(
        count_codex_history_calls(&remote_state),
        history_calls_before,
        "clean-close recovery must not reconstruct toggle history",
    );
    manager
        .close("thread-codex", Some("terminal-clean-close"))
        .await
        .expect("close clean-close Codex terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_probe_is_cached_and_remote_topology_is_fixture_exact() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let probe_runner = Arc::new(CodexProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(
            [
                ("versionOutput", vec!["--version"]),
                ("rootHelp", vec!["--help"]),
                ("appServerHelp", vec!["app-server", "--help"]),
            ]
            .into_iter()
            .map(|(field, _)| CodexProbeOutput {
                success: true,
                stdout: fixture[field].as_str().expect("probe fixture").to_owned(),
                stderr: String::new(),
            })
            .collect(),
        ),
    });
    let probe = Arc::new(CachedCodexCapabilityProbe::new(probe_runner.clone()));
    let first = probe
        .probe(&configured)
        .await
        .expect("supported Codex topology");
    let second = probe
        .probe(&configured)
        .await
        .expect("cached Codex topology");

    assert_eq!(first, second);
    assert_eq!(first.version, "0.145.0");
    assert!(first.unix_listener);
    assert!(first.remote_tui);
    assert_eq!(
        probe_runner
            .calls
            .lock()
            .expect("probe calls lock")
            .as_slice(),
        [
            vec!["--version".to_owned()],
            vec!["--help".to_owned()],
            vec!["app-server".to_owned(), "--help".to_owned()],
        ]
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_probe_retries_after_a_transient_failure() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let runner = Arc::new(CodexResultProbeRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([
            Err("transient probe failure".to_owned()),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["versionOutput"]
                    .as_str()
                    .expect("version fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["rootHelp"]
                    .as_str()
                    .expect("root help fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["appServerHelp"]
                    .as_str()
                    .expect("App Server help fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
        ])),
    });
    let probe = CachedCodexCapabilityProbe::new(runner.clone());

    assert!(probe.probe(&configured).await.is_none());
    assert!(
        probe.probe(&configured).await.is_some(),
        "a transient process failure must not poison the capability cache"
    );
    assert_eq!(
        runner.calls.lock().expect("probe calls lock").len(),
        4,
        "the second attempt must execute a fresh three-command probe"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_probe_invalidates_success_after_in_place_binary_replacement() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"first").expect("initial executable");
    let mut outputs = VecDeque::new();
    for version in ["codex-cli 0.145.0", "codex-cli 0.146.0"] {
        outputs.extend([
            Ok(CodexProbeOutput {
                success: true,
                stdout: version.to_owned(),
                stderr: String::new(),
            }),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["rootHelp"]
                    .as_str()
                    .expect("root help fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["appServerHelp"]
                    .as_str()
                    .expect("App Server help fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
        ]);
    }
    let runner = Arc::new(CodexResultProbeRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(outputs),
    });
    let probe = CachedCodexCapabilityProbe::new(runner.clone());

    assert_eq!(
        probe
            .probe(&configured)
            .await
            .expect("initial probe")
            .version,
        "0.145.0"
    );
    std::fs::write(&configured, b"second binary has a new fingerprint")
        .expect("replacement executable");
    assert_eq!(
        probe
            .probe(&configured)
            .await
            .expect("replacement probe")
            .version,
        "0.146.0",
        "a live in-place binary update must not reuse stale capabilities"
    );
    assert_eq!(
        runner.calls.lock().expect("probe calls lock").len(),
        6,
        "the replacement binary must receive a fresh three-command probe"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_probe_success_cache_evicts_the_oldest_unique_executable() {
    const CACHE_CAPACITY: usize = 64;

    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let mut outputs = VecDeque::new();
    for _ in 0..=CACHE_CAPACITY {
        outputs.extend([
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["versionOutput"]
                    .as_str()
                    .expect("version fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["rootHelp"]
                    .as_str()
                    .expect("root help fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(CodexProbeOutput {
                success: true,
                stdout: fixture["appServerHelp"]
                    .as_str()
                    .expect("App Server help fixture")
                    .to_owned(),
                stderr: String::new(),
            }),
        ]);
    }
    outputs.extend([
        Ok(CodexProbeOutput {
            success: true,
            stdout: fixture["versionOutput"]
                .as_str()
                .expect("version fixture")
                .to_owned(),
            stderr: String::new(),
        }),
        Ok(CodexProbeOutput {
            success: true,
            stdout: fixture["rootHelp"]
                .as_str()
                .expect("root help fixture")
                .to_owned(),
            stderr: String::new(),
        }),
        Ok(CodexProbeOutput {
            success: true,
            stdout: fixture["appServerHelp"]
                .as_str()
                .expect("App Server help fixture")
                .to_owned(),
            stderr: String::new(),
        }),
    ]);
    let runner = Arc::new(CodexResultProbeRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(outputs),
    });
    let probe = CachedCodexCapabilityProbe::new(runner.clone());
    let mut executables = Vec::new();
    for index in 0..=CACHE_CAPACITY {
        let executable = root.path().join(format!("codex-{index}"));
        std::fs::write(&executable, format!("binary-{index}")).expect("fixture executable");
        probe
            .probe(&executable)
            .await
            .expect("supported executable");
        executables.push(executable);
    }
    let calls_before_reprobe = runner.calls.lock().expect("probe calls lock").len();

    probe
        .probe(&executables[0])
        .await
        .expect("evicted executable reprobe");

    assert_eq!(
        runner.calls.lock().expect("probe calls lock").len(),
        calls_before_reprobe + 3,
        "the oldest unique executable must be evicted at the fixed cache capacity"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_helper_precedes_pty_and_generation_worker_owns_remote_transport() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let probe_runner = Arc::new(CodexProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(
            ["versionOutput", "rootHelp", "appServerHelp"]
                .into_iter()
                .map(|field| CodexProbeOutput {
                    success: true,
                    stdout: fixture[field].as_str().expect("probe fixture").to_owned(),
                    stderr: String::new(),
                })
                .collect(),
        ),
    });
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let helper = Arc::new(CodexFixtureHelperLauncher {
        timeline: timeline.clone(),
        ..CodexFixtureHelperLauncher::default()
    });
    let remote_state = Arc::new(CodexFixtureRemoteState {
        timeline: timeline.clone(),
        ..CodexFixtureRemoteState::default()
    });
    let remote = Arc::new(CodexFixtureRemoteFactory {
        state: remote_state.clone(),
    });
    let factory = Arc::new(CodexTerminalObserverFactory::new(
        Arc::new(CachedCodexCapabilityProbe::new(probe_runner)),
        helper.clone(),
        remote,
    ));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let backend = Arc::new(RecordingBackend::new(timeline.clone()));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-codex",
        "terminal-codex",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": configured,
            "args": fixture["originalArgs"],
            "label": "Codex",
            "activity": {
                "driverKind": "codex",
                "providerInstanceId": "codex",
            },
        }))
        .expect("Codex launch"),
    );
    manager.open(input).await.expect("observed Codex terminal");

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if timeline
                .lock()
                .expect("Codex topology timeline lock")
                .contains(&"initialized")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("generation-owned remote initialization");
    assert_eq!(
        timeline
            .lock()
            .expect("Codex topology timeline lock")
            .as_slice(),
        ["helper", "spawn", "connect", "initialize", "initialized"],
        "the helper must precede the PTY, while transport registration must occur on the durable generation worker"
    );
    let (endpoint, socket) = {
        let launches = helper.launches.lock().expect("helper launches lock");
        assert_eq!(launches.len(), 1);
        (
            launches[0].endpoint.clone(),
            launches[0].socket_path.clone(),
        )
    };
    assert!(endpoint.starts_with("unix://"));
    assert!(socket.starts_with(
        std::fs::canonicalize(root.path().join("runtime")).expect("canonical runtime directory")
    ));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(socket.parent().expect("socket parent"))
            .expect("private runtime directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let expected_helper = fixture["helperArgs"]
        .as_array()
        .expect("helper args")
        .iter()
        .map(|value| {
            let value = value.as_str().expect("helper arg");
            if value == "$ENDPOINT" {
                endpoint.clone()
            } else {
                value.to_owned()
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        helper.launches.lock().expect("helper launches lock")[0].args,
        expected_helper
    );
    let expected_tui = fixture["tuiArgs"]
        .as_array()
        .expect("TUI args")
        .iter()
        .map(|value| {
            let value = value.as_str().expect("TUI arg");
            if value == "$ENDPOINT" {
                endpoint.clone()
            } else {
                value.to_owned()
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(backend.spawns()[0].args, expected_tui);

    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-codex".to_owned(),
    };
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "pre-handshake terminal must not advertise activity"
    );
    remote_state
        .events
        .lock()
        .expect("remote events lock")
        .push_back(serde_json::json!({
            "method": "thread/started",
            "params": {
                "thread": {
                    "id": "pre-existing-root",
                    "createdAt": 0,
                },
            },
        }));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "a pre-generation thread must not establish the terminal handshake"
    );
    let valid_root = codex_root_notification(&fixture, "terminal-root", root.path());
    let mut unrelated_roots = Vec::new();
    for (case, field, value) in [
        (
            "mismatched session id",
            "sessionId",
            serde_json::json!("different-session"),
        ),
        (
            "forked thread",
            "forkedFromId",
            serde_json::json!("fork-source"),
        ),
        (
            "child thread",
            "parentThreadId",
            serde_json::json!("parent-thread"),
        ),
        (
            "subagent source",
            "source",
            serde_json::json!({
                "subAgent": {
                    "threadSpawn": {
                        "parentThreadId": "parent-thread",
                        "depth": 1,
                        "agentPath": null,
                        "agentNickname": null,
                        "agentRole": null
                    }
                }
            }),
        ),
        (
            "app-server source",
            "source",
            serde_json::json!("appServer"),
        ),
        ("arbitrary source", "source", serde_json::json!("extension")),
        (
            "non-null thread source",
            "threadSource",
            serde_json::json!("subagent"),
        ),
        (
            "unrelated cwd",
            "cwd",
            serde_json::json!("/unrelated/worktree"),
        ),
    ] {
        let mut notification = valid_root.clone();
        notification["params"]["thread"][field] = value;
        unrelated_roots.push((case, notification));
    }
    for (case, notification) in unrelated_roots {
        remote_state
            .events
            .lock()
            .expect("remote events lock")
            .push_back(notification);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            projection.snapshot(&scope).await.is_err(),
            "post-generation thread with {case} must not establish the terminal handshake"
        );
    }
    remote_state
        .events
        .lock()
        .expect("remote events lock")
        .push_back(valid_root);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native root handshake");
    assert_eq!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .as_slice(),
        [
            fixture["initializeRequest"].clone(),
            fixture["initializedNotification"].clone(),
            serde_json::json!({
                "method": "thread/resume",
                "params": {
                    "threadId": "terminal-root",
                },
            }),
            serde_json::json!({
                "method": "thread/backgroundTerminals/list",
                "params": {
                    "threadId": "terminal-root",
                    "limit": 128,
                },
            }),
            serde_json::json!({
                "method": "thread/list",
                "params": {
                    "ancestorThreadId": "terminal-root",
                    "limit": 50,
                },
            }),
        ]
    );
    assert_eq!(
        remote_state
            .endpoints
            .lock()
            .expect("remote endpoints lock")
            .as_slice(),
        [endpoint]
    );

    manager
        .close("thread-codex", Some("terminal-codex"))
        .await
        .expect("close terminal");
    assert!(
        helper
            .processes
            .lock()
            .expect("helper processes lock")
            .first()
            .expect("helper process")
            .terminated
            .load(Ordering::Acquire)
    );
    assert!(!socket.exists(), "owned socket must be removed on close");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_resume_atomically_recovers_history_then_receives_scoped_live_events() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted events lock")
        .push_back(Arc::new(Mutex::new(VecDeque::from([
            codex_root_notification(&fixture, "terminal-resume-root", root.path()),
        ]))));
    remote_state
        .resume_responses
        .lock()
        .expect("resume responses lock")
        .push_back(Ok(serde_json::json!({
            "thread": {
                "id": "terminal-resume-root",
                "parentThreadId": null,
                "createdAt": 2000000000_u64,
                "updatedAt": 2000000001_u64,
                "status": {"type": "active", "activeFlags": []},
                "turns": [{
                    "id": "history-turn",
                    "status": "completed",
                    "startedAt": 2000000000_u64,
                    "completedAt": 2000000001_u64,
                    "items": [{
                        "type": "agentMessage",
                        "id": "history-message",
                        "text": "recovered before subscription"
                    }]
                }]
            }
        })));
    remote_state
        .resume_events
        .lock()
        .expect("resume events lock")
        .push_back(vec![serde_json::json!({
            "method": "item/completed",
            "params": {
                "threadId": "terminal-resume-root",
                "turnId": "live-turn",
                "item": {
                    "type": "agentMessage",
                    "id": "live-message",
                    "text": "delivered after subscription"
                }
            }
        })]);
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        helper,
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
    )
    .await;
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );

    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-resume",
        ))
        .await
        .expect("Codex terminal");
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-resume".to_owned(),
    };
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.capabilities.terminal_observation
                && let Some(actor) = snapshot.actors.first()
                && projection
                    .list_detail(
                        &scope,
                        &snapshot.scope_id,
                        ActivityRecordKind::Actor,
                        &actor.id,
                        None,
                        10,
                    )
                    .await
                    .is_ok_and(|detail| detail.entries.len() >= 3)
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resume baseline and subscribed live event");

    let detail = projection
        .list_detail(
            &scope,
            &snapshot.scope_id,
            ActivityRecordKind::Actor,
            &snapshot.actors[0].id,
            None,
            10,
        )
        .await
        .expect("root actor detail");
    assert!(
        detail
            .entries
            .iter()
            .any(|entry| entry.detail.as_deref() == Some("recovered before subscription")),
        "resume history must be projected before the dock is enabled"
    );
    assert!(
        detail
            .entries
            .iter()
            .any(|entry| entry.detail.as_deref() == Some("delivered after subscription")),
        "connection-scoped live events must be delivered after thread/resume"
    );
    assert!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .contains(&serde_json::json!({
            "method": "thread/resume",
            "params": {
                "threadId": "terminal-resume-root",
            },
            })),
        "the root connection must subscribe with the canonical non-experimental thread/resume parameters"
    );
    manager
        .close("thread-codex", Some("terminal-resume"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_resume_retries_transient_materialization_without_losing_pending_events() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    script_codex_root(
        &remote_state,
        &fixture,
        "terminal-materializing-root",
        root.path(),
    );
    remote_state
        .request_delays
        .lock()
        .expect("request delays lock")
        .insert(
            "thread/resume".to_owned(),
            VecDeque::from([std::time::Duration::from_millis(1_250)]),
        );
    remote_state
        .resume_events
        .lock()
        .expect("resume events lock")
        .push_back(vec![serde_json::json!({
            "method": "item/completed",
            "params": {
                "threadId": "terminal-materializing-root",
                "turnId": "pending-turn",
                "item": {
                    "type": "agentMessage",
                    "id": "pending-message",
                    "text": "retained while the thread materialized"
                }
            }
        })]);
    remote_state
        .resume_responses
        .lock()
        .expect("resume responses lock")
        .push_back(Err(
            "Codex remote request failed: {\"code\":-32600,\"message\":\"No rollout is available for thread id terminal-materializing-root\"}".to_owned(),
        ));
    let (manager, projection, scope) = open_codex_fixture_terminal(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-materialization-retry",
    )
    .await;

    wait_for_codex_resume_request(&remote_state).await;
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && let Some(actor) = snapshot.actors.first()
                && projection
                    .list_detail(
                        &scope,
                        &snapshot.scope_id,
                        ActivityRecordKind::Actor,
                        &actor.id,
                        None,
                        10,
                    )
                    .await
                    .is_ok_and(|detail| {
                        detail.entries.iter().any(|entry| {
                            entry.detail.as_deref()
                                == Some("retained while the thread materialized")
                        })
                    })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("materialized root scope and retained pending event");
    assert_eq!(
        codex_resume_request_count(&remote_state),
        2,
        "only the transient not-found response should be retried"
    );
    assert_eq!(
        remote_state
            .maximum_active_resume_requests
            .load(Ordering::Acquire),
        1,
        "the first request outlives the retry cadence, so concurrent retries must be observable"
    );

    manager
        .close("thread-codex", Some("terminal-materialization-retry"))
        .await
        .expect("close materialization-retry terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_resume_reconnects_after_the_remote_tui_replaces_the_materializing_connection() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    script_codex_root(
        &remote_state,
        &fixture,
        "terminal-reconnected-root",
        root.path(),
    );
    remote_state
        .resume_responses
        .lock()
        .expect("resume responses lock")
        .extend([
            Err(
                "Codex remote request failed: {\"code\":-32600,\"message\":\"No rollout is available for thread id terminal-reconnected-root\"}".to_owned(),
            ),
            Err("Codex remote write failed: connection closed by remote TUI attach".to_owned()),
            Ok(serde_json::json!({
                "thread": {
                    "id": "terminal-reconnected-root",
                    "parentThreadId": null,
                    "createdAt": 2000000000_u64,
                    "updatedAt": 2000000001_u64,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": [{
                        "id": "reconnected-history-turn",
                        "status": "completed",
                        "startedAt": 2000000000_u64,
                        "completedAt": 2000000001_u64,
                        "items": [{
                            "type": "agentMessage",
                            "id": "reconnected-history-message",
                            "text": "history recovered after reconnect"
                        }]
                    }]
                }
            })),
        ]);
    remote_state
        .resume_events
        .lock()
        .expect("resume events lock")
        .extend([
            Vec::new(),
            Vec::new(),
            vec![serde_json::json!({
                "method": "item/completed",
                "params": {
                    "threadId": "terminal-reconnected-root",
                    "turnId": "reconnected-live-turn",
                    "item": {
                        "type": "agentMessage",
                        "id": "reconnected-live-message",
                        "text": "queued on the reconnected observer"
                    }
                }
            })],
        ]);
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
    )
    .await;
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-reconnected-resume",
        ))
        .await
        .expect("Codex terminal");
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-reconnected-resume".to_owned(),
    };

    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.capabilities.terminal_observation
                && let Some(actor) = snapshot.actors.first()
                && projection
                    .list_detail(
                        &scope,
                        &snapshot.scope_id,
                        ActivityRecordKind::Actor,
                        &actor.id,
                        None,
                        10,
                    )
                    .await
                    .is_ok_and(|detail| {
                        let details = detail
                            .entries
                            .iter()
                            .filter_map(|entry| entry.detail.as_deref())
                            .collect::<Vec<_>>();
                        details.contains(&"history recovered after reconnect")
                            && details.contains(&"queued on the reconnected observer")
                    })
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reconnected scope with recovered history and queued live event");

    assert_eq!(snapshot.scope, scope);
    assert_eq!(
        remote_state
            .endpoints
            .lock()
            .expect("remote endpoints lock")
            .len(),
        2,
        "the closed observer connection must be replaced exactly once"
    );
    assert_eq!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .iter()
            .filter(|write| write["method"] == "initialize")
            .count(),
        2,
        "the replacement connection must complete its own initialization handshake"
    );
    assert_eq!(
        codex_resume_request_count(&remote_state),
        3,
        "materialization, closed-connection detection, and fresh resume are sequential"
    );
    assert_eq!(
        helper
            .processes
            .lock()
            .expect("helper processes lock")
            .len(),
        1,
        "reconnecting the observer must not start a second App Server helper"
    );

    manager
        .close("thread-codex", Some("terminal-reconnected-resume"))
        .await
        .expect("close reconnected terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_resume_non_materialization_error_fails_closed_without_retry() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    script_codex_root(
        &remote_state,
        &fixture,
        "terminal-invalid-resume-root",
        root.path(),
    );
    remote_state
        .resume_responses
        .lock()
        .expect("resume responses lock")
        .extend([
            Err(
                "Codex remote request failed: {\"code\":-32600,\"message\":\"Invalid request\"}"
                    .to_owned(),
            ),
            Ok(serde_json::json!({
                "thread": {
                    "id": "terminal-invalid-resume-root",
                    "parentThreadId": null,
                    "turns": [],
                },
            })),
        ]);
    let (manager, projection, scope) = open_codex_fixture_terminal(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-invalid-resume",
    )
    .await;

    wait_for_codex_resume_request(&remote_state).await;
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "non-materialization resume failures must not publish a terminal scope"
    );
    assert_eq!(
        codex_resume_request_count(&remote_state),
        1,
        "non-materialization resume failures must fail closed without retry"
    );

    manager
        .close("thread-codex", Some("terminal-invalid-resume"))
        .await
        .expect("close invalid-resume terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_resume_materialization_retry_is_cancelled_without_a_late_request() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    script_codex_root(
        &remote_state,
        &fixture,
        "terminal-cancelled-resume-root",
        root.path(),
    );
    remote_state
        .resume_responses
        .lock()
        .expect("resume responses lock")
        .push_back(Err(
            "Codex remote request failed: {\"code\":-32600,\"message\":\"No rollout is available for thread id terminal-cancelled-resume-root\"}".to_owned(),
        ));
    let (manager, _projection, _scope) = open_codex_fixture_terminal(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-cancelled-resume",
    )
    .await;

    wait_for_codex_resume_request(&remote_state).await;
    wait_for_codex_resume_response(&remote_state).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    manager
        .close("thread-codex", Some("terminal-cancelled-resume"))
        .await
        .expect("close cancelled-resume terminal");
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    assert_eq!(
        codex_resume_request_count(&remote_state),
        1,
        "generation cancellation must prevent a delayed materialization retry"
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_resume_immediately_reconciles_pre_subscription_descendants_and_background_work() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted events lock")
        .push_back(Arc::new(Mutex::new(VecDeque::from([
            codex_root_notification(&fixture, "terminal-reconcile-root", root.path()),
        ]))));
    remote_state
        .request_responses
        .lock()
        .expect("request responses lock")
        .extend([
            (
                "thread/list".to_owned(),
                VecDeque::from([Ok(serde_json::json!({
                    "data": [{
                        "id": "pre-resume-child",
                        "parentThreadId": "terminal-reconcile-root",
                        "agentNickname": "Pre-resume child",
                        "agentRole": "worker",
                        "createdAt": 2000000000_u64,
                        "updatedAt": 2000000001_u64,
                        "status": {"type": "idle"}
                    }],
                    "nextCursor": null
                }))]),
            ),
            (
                "thread/read".to_owned(),
                VecDeque::from([Ok(serde_json::json!({
                    "thread": {
                        "id": "pre-resume-child",
                        "parentThreadId": "terminal-reconcile-root",
                        "agentNickname": "Pre-resume child",
                        "agentRole": "worker",
                        "createdAt": 2000000000_u64,
                        "updatedAt": 2000000001_u64,
                        "status": {"type": "idle"},
                        "turns": [{
                            "id": "pre-resume-turn",
                            "status": "completed",
                            "startedAt": 2000000000_u64,
                            "completedAt": 2000000001_u64,
                            "items": [{
                                "type": "agentMessage",
                                "id": "pre-resume-message",
                                "text": "materialized before root subscription"
                            }]
                        }]
                    }
                }))]),
            ),
            (
                "thread/backgroundTerminals/list".to_owned(),
                VecDeque::from([Ok(serde_json::json!({
                    "data": [{
                        "itemId": "pre-resume-background",
                        "processId": "background-process",
                        "command": "cargo test --workspace"
                    }],
                    "nextCursor": null
                }))]),
            ),
        ]);
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        Arc::new(CodexFixtureHelperLauncher::default()),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
    )
    .await;
    let manager = TerminalManager::new(
        Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new())))),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );

    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-reconcile",
        ))
        .await
        .expect("Codex terminal");
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-reconcile".to_owned(),
    };
    let snapshot = tokio::time::timeout(std::time::Duration::from_millis(500), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.capabilities.terminal_observation
                && snapshot
                    .actors
                    .iter()
                    .any(|actor| actor.name == "Pre-resume child")
                && snapshot
                    .work_items
                    .iter()
                    .any(|work| work.name == "cargo test --workspace")
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("immediate authoritative reconciliation without a later live trigger");
    let child = snapshot
        .actors
        .iter()
        .find(|actor| actor.name == "Pre-resume child")
        .expect("reconciled child");
    let detail = projection
        .list_detail(
            &scope,
            &snapshot.scope_id,
            ActivityRecordKind::Actor,
            &child.id,
            None,
            10,
        )
        .await
        .expect("reconciled child detail");
    assert!(
        detail.entries.iter().any(|entry| {
            entry.detail.as_deref() == Some("materialized before root subscription")
        })
    );
    assert_eq!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .iter()
            .filter_map(|write| write["method"].as_str())
            .collect::<Vec<_>>(),
        [
            "initialize",
            "initialized",
            "thread/resume",
            "thread/backgroundTerminals/list",
            "thread/list",
            "thread/read",
        ]
    );

    manager
        .close("thread-codex", Some("terminal-reconcile"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_reconciliation_stalled_thread_list_does_not_starve_background_snapshot() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted events lock")
        .push_back(Arc::new(Mutex::new(VecDeque::from([
            codex_root_notification(&fixture, "stalled-list-root", root.path()),
        ]))));
    remote_state
        .request_delays
        .lock()
        .expect("request delays lock")
        .insert(
            "thread/list".to_owned(),
            VecDeque::from([std::time::Duration::from_secs(5)]),
        );
    remote_state
        .request_responses
        .lock()
        .expect("request responses lock")
        .insert(
            "thread/backgroundTerminals/list".to_owned(),
            VecDeque::from([Ok(serde_json::json!({
                "data": [{
                    "itemId": "background-after-stalled-list",
                    "processId": "background-process",
                    "command": "background survives stalled list"
                }],
                "nextCursor": null
            }))]),
        );
    let (manager, projection, scope) =
        open_codex_fixture_terminal(&root, &configured, remote_state, "terminal-stalled-list")
            .await;

    tokio::time::timeout(std::time::Duration::from_millis(500), async {
        loop {
            if projection.snapshot(&scope).await.is_ok_and(|snapshot| {
                snapshot
                    .work_items
                    .iter()
                    .any(|work| work.name == "background survives stalled list")
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stalled thread/list must not starve independent background reconciliation");

    manager
        .close("thread-codex", Some("terminal-stalled-list"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_reconciliation_stalled_thread_read_does_not_starve_background_snapshot() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted events lock")
        .push_back(Arc::new(Mutex::new(VecDeque::from([
            codex_root_notification(&fixture, "stalled-read-root", root.path()),
        ]))));
    remote_state
        .request_delays
        .lock()
        .expect("request delays lock")
        .insert(
            "thread/read".to_owned(),
            VecDeque::from([std::time::Duration::from_secs(5)]),
        );
    remote_state
        .request_responses
        .lock()
        .expect("request responses lock")
        .extend([
            (
                "thread/list".to_owned(),
                VecDeque::from([Ok(serde_json::json!({
                    "data": [{
                        "id": "stalled-read-child",
                        "parentThreadId": "stalled-read-root",
                        "agentNickname": "Stalled read child",
                        "agentRole": "worker",
                        "createdAt": 2000000000_u64,
                        "updatedAt": 2000000001_u64,
                        "status": {"type": "idle"}
                    }],
                    "nextCursor": null
                }))]),
            ),
            (
                "thread/backgroundTerminals/list".to_owned(),
                VecDeque::from([Ok(serde_json::json!({
                    "data": [{
                        "itemId": "background-after-stalled-read",
                        "processId": "background-process",
                        "command": "background survives stalled read"
                    }],
                    "nextCursor": null
                }))]),
            ),
        ]);
    let (manager, projection, scope) =
        open_codex_fixture_terminal(&root, &configured, remote_state, "terminal-stalled-read")
            .await;

    tokio::time::timeout(std::time::Duration::from_millis(500), async {
        loop {
            if projection.snapshot(&scope).await.is_ok_and(|snapshot| {
                snapshot
                    .work_items
                    .iter()
                    .any(|work| work.name == "background survives stalled read")
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stalled thread/read must not starve independent background reconciliation");

    manager
        .close("thread-codex", Some("terminal-stalled-read"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_live_reconciliation_timeout_resumes_queued_event_processing() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .scripted_events
        .lock()
        .expect("scripted events lock")
        .push_back(Arc::new(Mutex::new(VecDeque::from([
            codex_root_notification(&fixture, "live-timeout-root", root.path()),
        ]))));
    remote_state
        .request_delays
        .lock()
        .expect("request delays lock")
        .insert(
            "thread/list".to_owned(),
            VecDeque::from([std::time::Duration::ZERO, std::time::Duration::from_secs(5)]),
        );
    remote_state
        .resume_events
        .lock()
        .expect("resume events lock")
        .push_back(vec![
            serde_json::json!({
                "method": "item/started",
                "params": {
                    "threadId": "live-timeout-root",
                    "turnId": "root-turn",
                    "item": {
                        "id": "trigger-reconciliation",
                        "type": "subAgentActivity",
                        "agentThreadId": "pending-child",
                        "agentPath": "/root/pending-child",
                        "kind": "started"
                    }
                }
            }),
            serde_json::json!({
                "method": "item/completed",
                "params": {
                    "threadId": "live-timeout-root",
                    "turnId": "root-turn",
                    "item": {
                        "id": "after-reconciliation",
                        "type": "agentMessage",
                        "text": "processed after bounded reconciliation"
                    }
                }
            }),
        ]);
    let (manager, projection, scope) =
        open_codex_fixture_terminal(&root, &configured, remote_state, "terminal-live-timeout")
            .await;

    tokio::time::timeout(std::time::Duration::from_millis(750), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && let Some(root_actor) = snapshot.actors.first()
                && projection
                    .list_detail(
                        &scope,
                        &snapshot.scope_id,
                        ActivityRecordKind::Actor,
                        &root_actor.id,
                        None,
                        10,
                    )
                    .await
                    .is_ok_and(|detail| {
                        detail.entries.iter().any(|entry| {
                            entry.detail.as_deref()
                                == Some("processed after bounded reconciliation")
                        })
                    })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("live reconciliation must return to queued events within its fixed bound");

    manager
        .close("thread-codex", Some("terminal-live-timeout"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_startup_failures_fail_open_without_interrupting_remote_tui() {
    let fixture = codex_remote_fixture();
    for phase in ["helper", "connect", "initialize"] {
        let root = tempfile::tempdir().expect("fixture root");
        let configured = root.path().join("configured-codex");
        std::fs::write(&configured, b"configured").expect("configured executable");
        let helper = Arc::new(CodexFixtureHelperLauncher {
            fail: AtomicBool::new(phase == "helper"),
            ..CodexFixtureHelperLauncher::default()
        });
        let remote_state = Arc::new(CodexFixtureRemoteState::default());
        if phase == "connect" {
            *remote_state
                .connect_error
                .lock()
                .expect("connect error lock") = Some("injected connect failure".to_owned());
        } else if phase == "initialize" {
            remote_state
                .request_errors
                .lock()
                .expect("request errors lock")
                .push_back("injected initialize failure".to_owned());
        }
        let (supervisor, projection) = codex_fixture_supervisor(
            &root,
            &configured,
            helper.clone(),
            Arc::new(CodexFixtureRemoteFactory {
                state: remote_state.clone(),
            }),
        )
        .await;
        let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(supervisor)),
                ..TerminalManagerOptions::default()
            },
        );
        let terminal_id = format!("terminal-fail-open-{phase}");

        manager
            .open(codex_fixture_open_input(
                &fixture,
                &configured,
                &root,
                &terminal_id,
            ))
            .await
            .unwrap_or_else(|error| panic!("{phase}: fail-open terminal: {error}"));

        if phase != "helper" {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let reached_failure = remote_state
                        .timeline
                        .lock()
                        .expect("Codex topology timeline lock")
                        .contains(&if phase == "connect" {
                            "connect"
                        } else {
                            "initialize"
                        });
                    if reached_failure {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{phase}: generation worker startup failure"));
        }
        assert_eq!(backend.spawns().len(), 1, "{phase}");
        assert_eq!(
            backend.spawns()[0].executable,
            if phase == "helper" {
                configured.to_string_lossy().into_owned()
            } else {
                std::fs::canonicalize(&configured)
                    .expect("canonical configured executable")
                    .to_string_lossy()
                    .into_owned()
            },
            "{phase}"
        );
        let expected_args = if phase == "helper" {
            fixture["originalArgs"]
                .as_array()
                .expect("original args")
                .iter()
                .map(|value| value.as_str().expect("original arg").to_owned())
                .collect::<Vec<_>>()
        } else {
            let endpoint = helper.launches.lock().expect("helper launches lock")[0]
                .endpoint
                .clone();
            fixture["tuiArgs"]
                .as_array()
                .expect("TUI args")
                .iter()
                .map(|value| {
                    let value = value.as_str().expect("TUI arg");
                    if value == "$ENDPOINT" {
                        endpoint.clone()
                    } else {
                        value.to_owned()
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            backend.spawns()[0].args,
            expected_args,
            "{phase}: startup failure must preserve a usable terminal topology"
        );
        assert!(
            projection
                .snapshot(&ActivityScopeRef::Terminal {
                    thread_id: "thread-codex".to_owned(),
                    terminal_id,
                })
                .await
                .is_err(),
            "{phase}: failed startup must not publish a dock"
        );
        if phase != "helper" {
            assert!(
                !helper.processes.lock().expect("helper processes lock")[0]
                    .terminated
                    .load(Ordering::Acquire),
                "{phase}: observer failure must not terminate the App Server backing the remote TUI"
            );
        }
        manager
            .close("thread-codex", Some(&format!("terminal-fail-open-{phase}")))
            .await
            .unwrap_or_else(|error| panic!("{phase}: close terminal: {error}"));
        if phase != "helper" {
            assert!(
                helper.processes.lock().expect("helper processes lock")[0]
                    .terminated
                    .load(Ordering::Acquire),
                "{phase}: terminal close must terminate its owned helper"
            );
        }
        let runtime = std::fs::canonicalize(root.path().join("runtime")).expect("runtime");
        assert_runtime_has_only_empty_retired_generations(
            &runtime,
            1,
            &format!("{phase}: terminal close cleanup"),
        );
        manager.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_generation_slot_exhaustion_fails_open_without_growing_the_namespace() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let runtime = root.path().join("runtime");
    std::fs::create_dir(&runtime).expect("runtime directory");
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
        .expect("runtime permissions");
    for slot in 0..64 {
        let ambiguous = runtime.join(format!("c{slot:016x}"));
        std::fs::create_dir(&ambiguous).expect("ambiguous Codex slot");
        std::fs::set_permissions(&ambiguous, std::fs::Permissions::from_mode(0o700))
            .expect("ambiguous slot permissions");
        std::fs::write(ambiguous.join("preserve"), b"ambiguous").expect("ambiguous slot payload");
    }
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: Arc::new(CodexFixtureRemoteState::default()),
        }),
    )
    .await;
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );

    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-slot-exhaustion",
        ))
        .await
        .expect("slot exhaustion fails open");

    let spawn = backend.spawns().pop().expect("original PTY spawn");
    assert_eq!(spawn.executable, configured.to_string_lossy());
    assert_eq!(
        spawn.args,
        fixture["originalArgs"]
            .as_array()
            .expect("original args")
            .iter()
            .map(|value| value.as_str().expect("original arg").to_owned())
            .collect::<Vec<_>>()
    );
    assert!(
        helper
            .launches
            .lock()
            .expect("helper launches lock")
            .is_empty(),
        "slot exhaustion is rejected before helper launch"
    );
    assert!(
        projection
            .snapshot(&ActivityScopeRef::Terminal {
                thread_id: "thread-codex".to_owned(),
                terminal_id: "terminal-slot-exhaustion".to_owned(),
            })
            .await
            .is_err(),
        "slot exhaustion publishes no activity dock"
    );
    assert_eq!(
        std::fs::read_dir(&runtime)
            .expect("runtime entries")
            .flatten()
            .filter(|entry| entry.file_name().as_encoded_bytes().starts_with(b"c"))
            .count(),
        64,
        "slot exhaustion never creates a 65th Codex slot"
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_list_discovery_correlates_root_when_broadcast_is_absent() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
    )
    .await;
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-timeout",
        ))
        .await
        .expect("Codex terminal");
    tokio::time::sleep(std::time::Duration::from_millis(75)).await;

    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-timeout".to_owned(),
    };
    assert!(projection.snapshot(&scope).await.is_err());
    assert_eq!(
        manager
            .attach(bibcode_server::terminal::TerminalAttachInput::existing(
                "thread-codex",
                "terminal-timeout",
            ))
            .await
            .expect("terminal remains attachable")
            .initial
            .status,
        TerminalStatus::Running
    );
    assert!(
        !helper.processes.lock().expect("helper processes lock")[0]
            .terminated
            .load(Ordering::Acquire),
        "waiting for a delayed root must not stop the healthy TUI App Server"
    );
    let root_candidate =
        codex_root_notification(&fixture, "terminal-listed-root", root.path())["params"]["thread"]
            .clone();
    let mut ambiguous_candidate = root_candidate.clone();
    ambiguous_candidate["id"] = serde_json::json!("terminal-listed-root-ambiguous");
    ambiguous_candidate["sessionId"] = serde_json::json!("terminal-listed-root-ambiguous");
    remote_state
        .request_responses
        .lock()
        .expect("request responses lock")
        .insert(
            "thread/list".to_owned(),
            VecDeque::from([
                Ok(serde_json::json!({
                    "data": [],
                    "nextCursor": null,
                })),
                Ok(serde_json::json!({
                    "data": [root_candidate.clone()],
                    "nextCursor": "opaque-more",
                })),
                Ok(serde_json::json!({
                    "data": [root_candidate.clone(), ambiguous_candidate],
                    "nextCursor": null,
                })),
                Ok(serde_json::json!({
                    "data": [root_candidate],
                    "nextCursor": null,
                })),
            ]),
        );
    tokio::time::sleep(std::time::Duration::from_millis(3_200)).await;
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "empty, truncated, and ambiguous listed roots must remain fail closed"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("listed native root handshake");
    assert!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .iter()
            .filter(|write| {
                write["method"] == "thread/list"
                    && write["params"].get("ancestorThreadId").is_none()
            })
            .all(|write| {
                write["params"]
                    == serde_json::json!({
                        "limit": 50,
                        "sortKey": "created_at",
                        "sortDirection": "desc",
                        "sourceKinds": ["cli", "vscode"],
                        "cwd": std::fs::canonicalize(root.path())
                            .expect("canonical fixture cwd")
                            .to_string_lossy(),
                        "useStateDbOnly": true,
                    })
            }),
        "root discovery must use a bounded state-DB query scoped to the expected interactive cwd"
    );
    manager
        .close("thread-codex", Some("terminal-timeout"))
        .await
        .expect("close terminal");
    assert!(
        helper.processes.lock().expect("helper processes lock")[0]
            .terminated
            .load(Ordering::Acquire)
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_stalled_list_discovery_returns_to_live_root_notifications_without_overlap() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .request_delays
        .lock()
        .expect("request delays lock")
        .insert(
            "thread/list".to_owned(),
            VecDeque::from([std::time::Duration::from_secs(5)]),
        );
    let (manager, projection, scope) = open_codex_fixture_terminal(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-stalled-discovery",
    )
    .await;

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if remote_state
                .writes
                .lock()
                .expect("remote writes lock")
                .iter()
                .any(|write| {
                    write["method"] == "thread/list"
                        && write["params"].get("ancestorThreadId").is_none()
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("root discovery request started");
    remote_state
        .events
        .lock()
        .expect("remote events lock")
        .push_back(codex_root_notification(
            &fixture,
            "terminal-live-root-after-stall",
            root.path(),
        ));

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("stalled discovery must return to live root notifications");
    assert_eq!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .iter()
            .filter(|write| {
                write["method"] == "thread/list"
                    && write["params"].get("ancestorThreadId").is_none()
            })
            .count(),
        1,
        "a stalled discovery request must never overlap a replacement request"
    );

    manager
        .close("thread-codex", Some("terminal-stalled-discovery"))
        .await
        .expect("close stalled-discovery terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_list_discovery_retries_only_after_the_timed_out_response_is_drained() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    remote_state
        .request_delays
        .lock()
        .expect("request delays lock")
        .insert(
            "thread/list".to_owned(),
            VecDeque::from([std::time::Duration::from_secs(5)]),
        );
    let root_candidate = codex_root_notification(
        &fixture,
        "terminal-root-after-late-response",
        root.path(),
    )["params"]["thread"]
        .clone();
    remote_state
        .request_responses
        .lock()
        .expect("request responses lock")
        .insert(
            "thread/list".to_owned(),
            VecDeque::from([Ok(serde_json::json!({
                "data": [root_candidate],
                "nextCursor": null,
            }))]),
        );
    let (manager, projection, scope) = open_codex_fixture_terminal(
        &root,
        &configured,
        remote_state.clone(),
        "terminal-late-discovery-response",
    )
    .await;

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if remote_state
                .writes
                .lock()
                .expect("remote writes lock")
                .iter()
                .filter(|write| {
                    write["method"] == "thread/list"
                        && write["params"].get("ancestorThreadId").is_none()
                })
                .count()
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial discovery request");
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    remote_state
        .events
        .lock()
        .expect("remote events lock")
        .push_back(serde_json::json!({
            "id": 999,
            "result": {
                "data": [],
                "nextCursor": null,
            },
        }));

    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("discovery retry after draining the late response");
    assert_eq!(
        remote_state
            .writes
            .lock()
            .expect("remote writes lock")
            .iter()
            .filter(|write| {
                write["method"] == "thread/list"
                    && write["params"].get("ancestorThreadId").is_none()
            })
            .count(),
        2,
        "the retry must wait until the first response is drained"
    );

    manager
        .close("thread-codex", Some("terminal-late-discovery-response"))
        .await
        .expect("close late-response terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_slow_worker_connect_does_not_consume_preparation_budget_or_interrupt_remote_tui() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let remote_state = Arc::new(CodexFixtureRemoteState {
        connect_delay: Mutex::new(std::time::Duration::from_secs(2)),
        ..CodexFixtureRemoteState::default()
    });
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state,
        }),
    )
    .await;
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );

    let started = std::time::Instant::now();
    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-cold-budget",
        ))
        .await
        .expect("cold preparation must fail open");

    assert!(
        started.elapsed() < std::time::Duration::from_millis(450),
        "Codex preparation must finish before the generic 500ms callback deadline"
    );
    assert_eq!(
        backend.spawns()[0].args,
        fixture["tuiArgs"]
            .as_array()
            .expect("TUI args")
            .iter()
            .map(|value| {
                let value = value.as_str().expect("TUI arg");
                if value == "$ENDPOINT" {
                    helper.launches.lock().expect("helper launches lock")[0]
                        .endpoint
                        .clone()
                } else {
                    value.to_owned()
                }
            })
            .collect::<Vec<_>>(),
        "slow observer connection must not prevent the remote TUI from starting"
    );
    assert!(
        !helper.processes.lock().expect("helper processes lock")[0]
            .terminated
            .load(Ordering::Acquire),
        "slow observer connection must not terminate the App Server backing the remote TUI"
    );
    assert!(
        projection
            .snapshot(&ActivityScopeRef::Terminal {
                thread_id: "thread-codex".to_owned(),
                terminal_id: "terminal-cold-budget".to_owned(),
            })
            .await
            .is_err(),
        "fail-open terminal must not advertise a dock"
    );
    manager
        .close("thread-codex", Some("terminal-cold-budget"))
        .await
        .expect("close slow-connect terminal");
    assert!(
        helper.processes.lock().expect("helper processes lock")[0]
            .terminated
            .load(Ordering::Acquire),
        "terminal close must terminate the owned helper"
    );
    assert_runtime_has_only_empty_retired_generations(
        &std::fs::canonicalize(root.path().join("runtime")).expect("runtime directory"),
        1,
        "slow-connect terminal close cleanup",
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_terminals_isolate_endpoints_scopes_and_owned_helper_cleanup() {
    let fixture = codex_remote_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured executable");
    let helper = Arc::new(CodexFixtureHelperLauncher::default());
    let remote_state = Arc::new(CodexFixtureRemoteState::default());
    {
        let mut scripts = remote_state
            .scripted_events
            .lock()
            .expect("scripted events lock");
        for root_id in ["root-a", "root-b"] {
            scripts.push_back(Arc::new(Mutex::new(VecDeque::from([
                codex_root_notification(&fixture, root_id, root.path()),
            ]))));
        }
    }
    let (supervisor, projection) = codex_fixture_supervisor(
        &root,
        &configured,
        helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: remote_state,
        }),
    )
    .await;
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-a",
        ))
        .await
        .expect("Codex terminal A");
    manager
        .open(codex_fixture_open_input(
            &fixture,
            &configured,
            &root,
            "terminal-b",
        ))
        .await
        .expect("Codex terminal B");

    let scope_a = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-a".to_owned(),
    };
    let scope_b = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-b".to_owned(),
    };
    let (snapshot_a, snapshot_b) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let (Ok(snapshot_a), Ok(snapshot_b)) = (
                projection.snapshot(&scope_a).await,
                projection.snapshot(&scope_b).await,
            ) && snapshot_a.capabilities.terminal_observation
                && snapshot_b.capabilities.terminal_observation
                && snapshot_a.actors.len() == 1
                && snapshot_b.actors.len() == 1
            {
                break (snapshot_a, snapshot_b);
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("isolated handshakes");
    assert_ne!(snapshot_a.scope_id, snapshot_b.scope_id);
    assert!(snapshot_a.capabilities.terminal_observation);
    assert!(snapshot_b.capabilities.terminal_observation);
    assert_eq!(snapshot_a.actors.len(), 1);
    assert_eq!(snapshot_b.actors.len(), 1);
    assert_ne!(snapshot_a.actors[0].id, snapshot_b.actors[0].id);
    let (directory_a, directory_b) = {
        let launches = helper.launches.lock().expect("helper launches lock");
        assert_eq!(launches.len(), 2);
        assert_ne!(launches[0].endpoint, launches[1].endpoint);
        assert_ne!(launches[0].socket_path, launches[1].socket_path);
        (
            launches[0]
                .socket_path
                .parent()
                .expect("terminal A generation directory")
                .to_path_buf(),
            launches[1]
                .socket_path
                .parent()
                .expect("terminal B generation directory")
                .to_path_buf(),
        )
    };
    for directory in [&directory_a, &directory_b] {
        assert_eq!(
            std::fs::read(directory.join(".bibcode-provider-terminal-owner"))
                .expect("Codex generation ownership marker"),
            b"bibcode-provider-terminal-v1\n",
            "every live Codex socket directory must carry the exact cleanup ownership marker"
        );
    }

    manager
        .close("thread-codex", Some("terminal-a"))
        .await
        .expect("close terminal");
    {
        let processes = helper.processes.lock().expect("helper processes lock");
        assert!(processes[0].terminated.load(Ordering::Acquire));
        assert!(!processes[1].terminated.load(Ordering::Acquire));
    }
    assert_empty_retired_generation(&directory_a, "terminal A cleanup");
    assert!(directory_b.exists());

    manager
        .close("thread-codex", Some("terminal-b"))
        .await
        .expect("close terminal");
    assert!(
        helper.processes.lock().expect("helper processes lock")[1]
            .terminated
            .load(Ordering::Acquire)
    );
    assert_empty_retired_generation(&directory_b, "terminal B cleanup");
    manager.shutdown().await;
}

#[derive(Debug, Default)]
struct ClaudeProbeFixtureRunner {
    calls: Mutex<Vec<Vec<String>>>,
    outputs: Mutex<VecDeque<Result<ClaudeProbeOutput, String>>>,
}

impl ClaudeCapabilityProbeRunner for ClaudeProbeFixtureRunner {
    fn run(
        &self,
        _executable: &std::path::Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<ClaudeProbeOutput, String>> + Send + '_>> {
        self.calls
            .lock()
            .expect("Claude probe calls lock")
            .push(args);
        let output = self
            .outputs
            .lock()
            .expect("Claude probe outputs lock")
            .pop_front()
            .unwrap_or_else(|| Err("unexpected Claude probe".to_owned()));
        Box::pin(std::future::ready(output))
    }
}

#[derive(Debug)]
struct ClaudeAttestorFixture;

impl ClaudeAdditiveHookAttestor for ClaudeAttestorFixture {
    fn prove(
        &self,
        _executable: &std::path::Path,
        version: &str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(std::future::ready(version == "2.1.220"))
    }
}

#[derive(Debug)]
struct CopyClaudeExecutablePinner;

impl ClaudeExecutablePinner for CopyClaudeExecutablePinner {
    fn pin(&self, source: &std::path::Path, destination: &std::path::Path) -> io::Result<()> {
        std::fs::copy(source, destination).map(|_| ())
    }
}

#[derive(Debug)]
struct UnsupportedClaudeExecutablePinner;

impl ClaudeExecutablePinner for UnsupportedClaudeExecutablePinner {
    fn pin(&self, _source: &std::path::Path, _destination: &std::path::Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "fixture clone unsupported",
        ))
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum ClaudeSourceMutation {
    AtomicReplacement,
    InPlace,
}

#[cfg(unix)]
#[derive(Debug)]
struct MutatingClaudeBackend {
    recording: RecordingBackend,
    source: std::path::PathBuf,
    mutation: ClaudeSourceMutation,
}

#[cfg(unix)]
impl PtyBackend for MutatingClaudeBackend {
    fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        use std::os::unix::fs::PermissionsExt;

        match self.mutation {
            ClaudeSourceMutation::AtomicReplacement => {
                let replacement = self.source.with_extension("replacement");
                std::fs::write(&replacement, b"rejected").expect("replacement Claude");
                std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700))
                    .expect("replacement permissions");
                std::fs::rename(replacement, &self.source).expect("replace canonical Claude");
            }
            ClaudeSourceMutation::InPlace => {
                std::fs::write(&self.source, b"rejected").expect("mutate canonical Claude");
            }
        }
        self.recording.spawn(input)
    }
}

async fn claude_pinning_fixture_supervisor(
    root: &tempfile::TempDir,
    configured: &std::path::Path,
    pinner: Arc<dyn ClaudeExecutablePinner>,
) -> ProviderTerminalActivitySupervisor {
    let fixture = claude_hook_fixture();
    let runner = Arc::new(ClaudeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["versionOutput"]
                    .as_str()
                    .expect("version output")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["rootHelp"].as_str().expect("root help").to_owned(),
                stderr: String::new(),
            }),
        ])),
    });
    let factory = Arc::new(ClaudeTerminalObserverFactory::with_pinner(
        Arc::new(CachedClaudeCapabilityProbe::with_attestor(
            runner,
            Arc::new(ClaudeAttestorFixture),
        )),
        pinner,
    ));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.claude_agent.binary_path = configured.to_string_lossy().into_owned();
    ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            claude: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("Claude supervisor")
}

fn claude_pinning_open_input(
    root: &tempfile::TempDir,
    configured: &std::path::Path,
    terminal_id: &str,
) -> TerminalOpenInput {
    let fixture = claude_hook_fixture();
    let mut input = TerminalOpenInput::new(
        "thread-claude-pin",
        terminal_id,
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": configured,
            "args": fixture["originalArgs"],
            "label": "Claude",
            "activity": {
                "driverKind": "claudeAgent",
                "providerInstanceId": "claudeAgent",
            },
        }))
        .expect("Claude launch"),
    );
    input
}

#[tokio::test]
async fn agent_activity_toggle_disabled_launch_is_pure_pass_through() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let authority = Arc::new(CountingInventoryAuthority {
        calls: AtomicUsize::new(0),
        inventory: ProviderTerminalInventory::from_settings(&settings),
    });
    let factory = Arc::new(CountingProviderFactory::default());
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let controller = AgentActivityController::new(false);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let supervisor = ProviderTerminalActivitySupervisor::new_with_authority(
        authority.clone(),
        controller,
        projection,
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");

    let preparation = supervisor
        .prepare(TerminalLaunchPreparationInput {
            executable: configured.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: fixture.path().to_path_buf(),
            worktree_path: None,
            launch_env: BTreeMap::new(),
            activity: ProviderTerminalActivityLaunch {
                driver_kind: "codex".to_owned(),
                provider_instance_id: "codex".to_owned(),
            },
            generation: TerminalObserverGeneration::new(
                "thread-disabled".to_owned(),
                "terminal-disabled".to_owned(),
            ),
        })
        .await;

    assert!(matches!(
        preparation,
        TerminalLaunchPreparation::PassThrough
    ));
    assert_eq!(authority.calls.load(Ordering::Acquire), 0);
    assert_eq!(factory.prepare_calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn agent_activity_toggle_disable_racing_preparation_drops_owned_resources_and_passes_through()
{
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let authority = Arc::new(CountingInventoryAuthority {
        calls: AtomicUsize::new(0),
        inventory: ProviderTerminalInventory::from_settings(&settings),
    });
    let live_resources = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(RacingProviderFactory {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        live_resources: live_resources.clone(),
    });
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let controller = AgentActivityController::new(true);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let supervisor = ProviderTerminalActivitySupervisor::new_with_authority(
        authority,
        controller.clone(),
        projection,
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let preparation = tokio::spawn({
        let supervisor = supervisor.clone();
        let configured = configured.clone();
        let cwd = fixture.path().to_path_buf();
        async move {
            supervisor
                .prepare(TerminalLaunchPreparationInput {
                    executable: configured.to_string_lossy().into_owned(),
                    args: Vec::new(),
                    cwd,
                    worktree_path: None,
                    launch_env: BTreeMap::new(),
                    activity: ProviderTerminalActivityLaunch {
                        driver_kind: "codex".to_owned(),
                        provider_instance_id: "codex".to_owned(),
                    },
                    generation: TerminalObserverGeneration::new(
                        "thread-race".to_owned(),
                        "terminal-race".to_owned(),
                    ),
                })
                .await
        }
    });
    factory
        .entered
        .acquire()
        .await
        .expect("preparation entered")
        .forget();

    let before_disable = controller.snapshot();
    let disabling = tokio::spawn({
        let controller = controller.clone();
        async move { controller.disable().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while controller.snapshot().enabled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Terminal activity gate closes");
    factory.release.add_permits(1);
    disabling.await.expect("disable task");
    assert_ne!(controller.snapshot().generation, before_disable.generation);
    let result = preparation.await.expect("preparation task");

    assert!(matches!(result, TerminalLaunchPreparation::PassThrough));
    assert_eq!(
        live_resources.load(Ordering::Acquire),
        0,
        "the raced prepared observer and its owned resources are dropped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_disable_after_final_check_rejects_late_observer_install() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let controller = AgentActivityController::new(true);
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let observer = Arc::new(CountingActivityObserver::default());
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(PostActivityCheckPausingPreparer {
                controller: controller.clone(),
                entered: entered.clone(),
                release: release.clone(),
                observer: observer.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-post-check-race",
        "terminal-post-check-race",
        fixture.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));

    let opening = tokio::spawn({
        let manager = manager.clone();
        async move { manager.open(input).await }
    });
    entered
        .acquire()
        .await
        .expect("preparation reached post-check pause")
        .forget();

    let disabling = tokio::spawn({
        let controller = controller.clone();
        let manager = manager.clone();
        async move {
            controller.disable().await;
            manager.set_agent_activity_enabled(false).await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while controller.snapshot().enabled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal activity gate closes");

    release.add_permits(1);
    let stopped = disabling.await.expect("disable transition");
    opening
        .await
        .expect("open task")
        .expect("pass-through terminal still launches");

    assert_eq!(
        backend.spawns()[0].executable,
        "codex",
        "the stale prepared command never reaches the PTY backend"
    );
    assert_eq!(
        manager.agent_activity_restart_descriptor_count_for_integration_test(),
        0,
        "no observer is retained after Terminal activity is disabled"
    );
    assert_eq!(observer.disabled.load(Ordering::Acquire), 0);
    assert_eq!(stopped, TerminalAgentActivityTransition::default());
    manager
        .write(
            "thread-post-check-race",
            "terminal-post-check-race",
            "still alive\n",
        )
        .await
        .expect("pass-through PTY remains writable");
    assert!(!backend.latest().killed.load(Ordering::Acquire));
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_hung_factory_does_not_block_terminal_disable_or_later_settings_updates() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut config = ServerConfig::new(fixture.path());
    hermetic_providers::write_hermetic_settings(&config.state_dir(), serde_json::json!({}));
    config.storage_instance_id = Some(StorageInstanceId::from_uuid(Uuid::from_u128(
        0x00000000000040008000000000000005,
    )));
    let control = NativeServerControl::new(config, serde_json::json!({"policy":"test"})).await;
    control
        .call(
            "server.updateSettings",
            serde_json::json!({"patch":{"enableTerminalAgentActivity":true}}),
            CancellationToken::new(),
        )
        .await
        .expect("enable Terminal activity before attaching runtime handler");
    let stream_cancellation = CancellationToken::new();
    let mut config_events = control.subscribe("subscribeServerConfig", stream_cancellation.clone());
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), config_events.recv())
        .await
        .expect("config snapshot timeout")
        .expect("config stream remains open")
        .expect("config snapshot succeeds");
    assert_eq!(snapshot[0]["type"], "snapshot");

    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let controller = AgentActivityController::new(true);
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let returned = Arc::new(tokio::sync::Semaphore::new(0));
    let dropped = Arc::new(tokio::sync::Semaphore::new(0));
    let live_observers = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(HungProviderFactory {
        entered: entered.clone(),
        release: release.clone(),
        returned: returned.clone(),
        dropped: dropped.clone(),
        live_observers: live_observers.clone(),
    });
    let mut release_guard = HungFactoryReleaseGuard::new(release);
    let supervisor = ProviderTerminalActivitySupervisor::new_with_authority(
        Arc::new(CountingInventoryAuthority {
            calls: AtomicUsize::new(0),
            inventory: ProviderTerminalInventory::from_settings(&settings),
        }),
        controller.clone(),
        projection,
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    control
        .attach_agent_activity_handler(Arc::new(TerminalSettingsTransitionHandler {
            controller: controller.clone(),
            manager: manager.clone(),
        }))
        .await;

    let mut input = TerminalOpenInput::new(
        "thread-hung-factory",
        "terminal-hung-factory",
        fixture.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--help".to_owned()],
        label: Some("Codex".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "codex".to_owned(),
            provider_instance_id: "codex".to_owned(),
        }),
        env: None,
    });
    let opening = tokio::spawn({
        let manager = manager.clone();
        async move { manager.open(input).await }
    });
    entered
        .acquire()
        .await
        .expect("provider factory entered")
        .forget();
    // The manager enforces the production 500 ms callback budget. Keep this
    // outer integration watchdog wide enough for a concurrently loaded release
    // test graph while still proving that the original PTY launches.
    tokio::time::timeout(std::time::Duration::from_secs(10), opening)
        .await
        .expect("manager preparation timeout")
        .expect("open task")
        .expect("original PTY launches after preparation timeout");
    assert_eq!(backend.spawns()[0].executable, configured.to_string_lossy());
    assert!(
        !backend.spawns()[0]
            .env
            .contains_key("BIBCODE_HUNG_FACTORY_OBSERVER")
    );

    let mut disabling_update = tokio::spawn({
        let control = control.clone();
        async move {
            control
                .call(
                    "server.updateSettings",
                    serde_json::json!({"patch":{"enableTerminalAgentActivity":false}}),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while controller.snapshot().enabled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Terminal activity gate closes");
    let disabled = match tokio::time::timeout(
        std::time::Duration::from_secs(1),
        &mut disabling_update,
    )
    .await
    {
        Ok(result) => result
            .expect("disable settings task")
            .expect("disable settings update"),
        Err(_) => {
            release_guard.release();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut disabling_update)
                .await;
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), dropped.acquire()).await;
            panic!("Terminal settings publication waited for the hung provider factory");
        }
    };
    assert_eq!(disabled["enableTerminalAgentActivity"], false);
    let settings_event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let events = config_events
                .recv()
                .await
                .expect("config stream remains open")
                .expect("config event succeeds");
            if let Some(event) = events
                .into_iter()
                .find(|event| event["type"] == "settingsUpdated")
            {
                break event;
            }
        }
    })
    .await
    .expect("disabled settings publication");
    assert_eq!(
        settings_event["payload"]["settings"]["enableTerminalAgentActivity"],
        false
    );

    let later = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        control.call(
            "server.updateSettings",
            serde_json::json!({"patch":{"terminalDefaultShell":"/bin/test-shell"}}),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("later settings update is not blocked")
    .expect("later settings update succeeds");
    assert_eq!(later["terminalDefaultShell"], "/bin/test-shell");

    release_guard.release();
    returned
        .acquire()
        .await
        .expect("hung factory returns after release")
        .forget();
    tokio::time::timeout(std::time::Duration::from_secs(2), dropped.acquire())
        .await
        .expect("late prepared observer is dropped")
        .expect("observer drop semaphore")
        .forget();
    assert_eq!(live_observers.load(Ordering::Acquire), 0);
    assert_eq!(
        manager.agent_activity_restart_descriptor_count_for_integration_test(),
        0
    );
    manager
        .write(
            "thread-hung-factory",
            "terminal-hung-factory",
            "still alive\n",
        )
        .await
        .expect("fallback PTY remains writable");
    assert!(!backend.latest().killed.load(Ordering::Acquire));
    stream_cancellation.cancel();
    manager.shutdown().await;
}

#[tokio::test]
async fn agent_activity_toggle_terminal_launched_disabled_stays_uninstrumented_after_enable() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured binary");
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let authority = Arc::new(CountingInventoryAuthority {
        calls: AtomicUsize::new(0),
        inventory: ProviderTerminalInventory::from_settings(&settings),
    });
    let factory = Arc::new(CountingProviderFactory::default());
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let controller = AgentActivityController::new(false);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let supervisor = ProviderTerminalActivitySupervisor::new_with_authority(
        authority.clone(),
        controller.clone(),
        projection,
        ProcessAttributionRegistry::new(),
        fixture.path().join("runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-disabled-launch",
        "terminal-disabled-launch",
        fixture.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "gpt-5".to_owned()],
        label: Some("Codex".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "codex".to_owned(),
            provider_instance_id: "codex".to_owned(),
        }),
        env: None,
    });
    manager.open(input).await.expect("pass-through terminal");
    assert_eq!(backend.spawns()[0].executable, configured.to_string_lossy());
    assert_eq!(authority.calls.load(Ordering::Acquire), 0);
    assert_eq!(factory.prepare_calls.load(Ordering::Acquire), 0);

    controller.enable();
    assert_eq!(
        manager.set_agent_activity_enabled(true).await,
        TerminalAgentActivityTransition::default()
    );
    assert_eq!(
        factory.prepare_calls.load(Ordering::Acquire),
        0,
        "re-enable never retrofits a pass-through terminal"
    );
    assert!(!backend.latest().killed.load(Ordering::Acquire));
    manager.shutdown().await;
}

#[tokio::test]
async fn agent_activity_toggle_pauses_and_resumes_installed_observer_without_killing_pty() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let observer = Arc::new(CountingActivityObserver::default());
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(CountingActivityPreparer {
                observer: observer.clone(),
            })),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-toggle",
        "terminal-toggle",
        fixture.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(command(true));
    manager.open(input).await.expect("observed terminal");
    let process = backend.latest();

    let stopped = manager.set_agent_activity_enabled(false).await;
    assert_eq!(
        stopped,
        TerminalAgentActivityTransition {
            stopped: 1,
            dormant: 1,
            resumed: 0,
            failed: 0,
            ..TerminalAgentActivityTransition::default()
        }
    );
    assert_eq!(observer.disabled.load(Ordering::Acquire), 1);
    assert!(!process.killed.load(Ordering::Acquire));

    let resumed = manager.set_agent_activity_enabled(true).await;
    assert_eq!(
        resumed,
        TerminalAgentActivityTransition {
            stopped: 0,
            dormant: 0,
            resumed: 1,
            failed: 0,
            ..TerminalAgentActivityTransition::default()
        }
    );
    assert_eq!(observer.enabled.load(Ordering::Acquire), 1);
    assert!(!process.killed.load(Ordering::Acquire));

    manager.shutdown().await;
}

#[cfg(unix)]
async fn assert_claude_source_mutation_cannot_change_pinned_spawn(mutation: ClaudeSourceMutation) {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-claude");
    std::fs::write(&configured, b"approved").expect("approved Claude");
    std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o700))
        .expect("Claude permissions");
    let supervisor =
        claude_pinning_fixture_supervisor(&root, &configured, Arc::new(CopyClaudeExecutablePinner))
            .await;
    let backend = Arc::new(MutatingClaudeBackend {
        recording: RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))),
        source: configured.clone(),
        mutation,
    });
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );

    manager
        .open(claude_pinning_open_input(
            &root,
            &configured,
            "terminal-claude-pin",
        ))
        .await
        .expect("Claude terminal");

    let spawn = backend.recording.spawns().pop().expect("Claude PTY spawn");
    let pinned = std::path::PathBuf::from(&spawn.executable);
    assert_ne!(
        pinned, configured,
        "the approved executable must be private"
    );
    assert_eq!(
        std::fs::read(&pinned).expect("read pinned executable"),
        b"approved"
    );
    assert_eq!(
        std::fs::read(&configured).expect("read mutated canonical executable"),
        b"rejected"
    );
    let (_, _, _, settings_path) = claude_hook_launch(&backend.recording);
    assert_eq!(
        &spawn.args[..4],
        ["--model", "sonnet", "--permission-mode", "plan"]
    );

    manager
        .close("thread-claude-pin", Some("terminal-claude-pin"))
        .await
        .expect("close terminal");
    assert!(
        !pinned.exists(),
        "closing must remove the pinned executable"
    );
    assert!(
        !settings_path.exists(),
        "closing must remove the hook overlay"
    );
    assert_empty_retired_generation(
        pinned.parent().expect("generation directory"),
        "closing a pinned Claude generation",
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_atomic_replacement_after_preparation_cannot_change_pinned_spawn() {
    assert_claude_source_mutation_cannot_change_pinned_spawn(
        ClaudeSourceMutation::AtomicReplacement,
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_in_place_mutation_after_preparation_cannot_change_pinned_spawn() {
    assert_claude_source_mutation_cannot_change_pinned_spawn(ClaudeSourceMutation::InPlace).await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_unsupported_clone_is_exact_original_pass_through() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-claude");
    std::fs::write(&configured, b"approved").expect("approved Claude");
    std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o700))
        .expect("Claude permissions");
    let supervisor = claude_pinning_fixture_supervisor(
        &root,
        &configured,
        Arc::new(UnsupportedClaudeExecutablePinner),
    )
    .await;
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let input = claude_pinning_open_input(&root, &configured, "terminal-clone-unsupported");
    let original_args = input.command.as_ref().expect("Claude command").args.clone();

    manager.open(input).await.expect("pass-through terminal");

    let spawn = backend.spawns().pop().expect("Claude PTY spawn");
    assert_eq!(spawn.executable, configured.to_string_lossy());
    assert_eq!(spawn.args, original_args);
    assert!(!spawn.args.iter().any(|argument| argument == "--settings"));
    assert_runtime_has_only_empty_retired_generations(
        &root.path().join("runtime"),
        1,
        "failed pinning cleanup",
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_spawn_failure_removes_pin_overlay_and_generation_directory() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-claude");
    std::fs::write(&configured, b"approved").expect("approved Claude");
    std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o700))
        .expect("Claude permissions");
    let supervisor =
        claude_pinning_fixture_supervisor(&root, &configured, Arc::new(CopyClaudeExecutablePinner))
            .await;
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    backend.fail_next("fixture PTY failure");
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );

    assert!(
        manager
            .open(claude_pinning_open_input(
                &root,
                &configured,
                "terminal-spawn-failure",
            ))
            .await
            .is_err()
    );

    let spawn = backend.spawns().pop().expect("attempted Claude PTY spawn");
    let pinned = std::path::PathBuf::from(spawn.executable);
    let settings_index = spawn
        .args
        .iter()
        .position(|argument| argument == "--settings")
        .expect("settings argument");
    let overlay = std::path::PathBuf::from(
        spawn
            .args
            .get(settings_index + 1)
            .expect("settings overlay"),
    );
    assert!(!pinned.exists());
    assert!(!overlay.exists());
    assert_empty_retired_generation(
        pinned.parent().expect("generation directory"),
        "Claude spawn failure cleanup",
    );
    manager.shutdown().await;
}

fn claude_hook_fixture() -> Value {
    serde_json::from_str(CLAUDE_HOOK_FIXTURE).expect("Claude hook fixture")
}

fn correlated_claude_root_hook(fixture: &Value, root: &tempfile::TempDir) -> Value {
    let mut hook = fixture["rootHook"].clone();
    hook["transcript_path"] = Value::String(
        root.path()
            .join("claude-root-session.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    hook["cwd"] = Value::String(root.path().to_string_lossy().into_owned());
    hook
}

#[tokio::test]
async fn claude_feature_probe_requires_additional_settings_and_authenticated_http_hooks() {
    let fixture = claude_hook_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let executable = root.path().join("claude");
    std::fs::write(&executable, b"configured").expect("configured Claude executable");
    let runner = Arc::new(ClaudeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["versionOutput"]
                    .as_str()
                    .expect("version output")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["rootHelp"].as_str().expect("root help").to_owned(),
                stderr: String::new(),
            }),
        ])),
    });
    let probe =
        CachedClaudeCapabilityProbe::with_attestor(runner.clone(), Arc::new(ClaudeAttestorFixture));

    let capabilities = probe
        .probe(&executable)
        .await
        .expect("fixture proves supported Claude hooks");

    assert_eq!(
        capabilities,
        ClaudeCapabilities {
            version: "2.1.220".to_owned(),
            additional_settings: true,
            authenticated_http_hooks: true,
            additive_hook_merge: true,
        }
    );
    assert_eq!(
        runner
            .calls
            .lock()
            .expect("Claude probe calls lock")
            .as_slice(),
        [vec!["--version".to_owned()], vec!["--help".to_owned()],]
    );
}

#[cfg(all(unix, target_os = "macos"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn versioned_claude_source_is_privately_pinned_and_cleans_overlay() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("fixture root");
    let installed = root.path().join("installed/versions/2.1.220");
    std::fs::create_dir_all(installed.parent().expect("version directory"))
        .expect("create version directory");
    std::fs::write(&installed, b"approved").expect("versioned Claude executable");
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o500))
        .expect("versioned Claude permissions");
    let fixture = claude_hook_fixture();
    let runner = Arc::new(ClaudeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["versionOutput"]
                    .as_str()
                    .expect("version output")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["rootHelp"].as_str().expect("root help").to_owned(),
                stderr: String::new(),
            }),
        ])),
    });
    let factory = Arc::new(ClaudeTerminalObserverFactory::new(Arc::new(
        CachedClaudeCapabilityProbe::with_attestor(runner, Arc::new(ClaudeAttestorFixture)),
    )));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.claude_agent.binary_path = installed.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            claude: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("Claude supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-installed-claude",
        "terminal-installed-claude",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": installed,
            "args": fixture["originalArgs"],
            "label": "Claude",
            "activity": {
                "driverKind": "claudeAgent",
                "providerInstanceId": "claudeAgent",
            },
        }))
        .expect("installed Claude launch"),
    );

    manager
        .open(input)
        .await
        .expect("versioned Claude preparation");
    let spawn = backend.spawns().pop().expect("versioned Claude PTY spawn");
    let pinned = std::path::PathBuf::from(&spawn.executable);
    assert_ne!(
        pinned, installed,
        "the installed launch must use a private pin"
    );
    assert_eq!(
        pinned
            .parent()
            .and_then(std::path::Path::parent)
            .expect("private generation directory"),
        std::fs::canonicalize(root.path().join("runtime")).expect("canonical runtime"),
        "the pin must live directly below the owner-only runtime directory"
    );
    assert_eq!(
        std::fs::metadata(&pinned)
            .expect("pinned metadata")
            .permissions()
            .mode()
            & 0o777,
        0o500,
        "the pinned executable must not remain writable"
    );
    assert_eq!(
        std::fs::read(&pinned).expect("pinned Claude contents"),
        b"approved",
        "the native clone must preserve the approved executable contents"
    );
    let (_endpoint, _token, _correlation, settings_path) = claude_hook_launch(&backend);
    assert!(
        settings_path.is_file(),
        "the real callback boundary must retain the prepared overlay"
    );

    manager
        .close("thread-installed-claude", Some("terminal-installed-claude"))
        .await
        .expect("close terminal");
    assert!(
        !settings_path.exists(),
        "closing the prepared terminal must clean the private overlay"
    );
    assert!(
        !pinned.exists(),
        "closing the prepared terminal must clean the private executable"
    );
    manager.shutdown().await;
}

async fn claude_direct_preparation(
    version_output: &str,
    help_output: &str,
    args: Vec<String>,
) -> TerminalLaunchPreparation {
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-claude");
    std::fs::write(&configured, b"configured").expect("configured Claude executable");
    let runner = Arc::new(ClaudeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: version_output.to_owned(),
                stderr: String::new(),
            }),
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: help_output.to_owned(),
                stderr: String::new(),
            }),
        ])),
    });
    let factory = Arc::new(ClaudeTerminalObserverFactory::with_pinner(
        Arc::new(CachedClaudeCapabilityProbe::with_attestor(
            runner,
            Arc::new(ClaudeAttestorFixture),
        )),
        Arc::new(CopyClaudeExecutablePinner),
    ));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.claude_agent.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            claude: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("Claude supervisor");
    supervisor
        .prepare(TerminalLaunchPreparationInput {
            executable: configured.to_string_lossy().into_owned(),
            args,
            cwd: root.path().to_path_buf(),
            worktree_path: Some(root.path().to_path_buf()),
            launch_env: BTreeMap::new(),
            activity: ProviderTerminalActivityLaunch {
                driver_kind: "claudeAgent".to_owned(),
                provider_instance_id: "claudeAgent".to_owned(),
            },
            generation: TerminalObserverGeneration::new(
                "thread-claude".to_owned(),
                "terminal-claude".to_owned(),
            ),
        })
        .await
}

#[tokio::test]
async fn claude_unsafe_or_unproven_settings_composition_is_pass_through() {
    let fixture = claude_hook_fixture();
    let proven_help = fixture["rootHelp"].as_str().expect("root help");
    for (version, help, args) in [
        (
            "2.1.62 (Claude Code)",
            proven_help,
            vec!["--model".to_owned(), "sonnet".to_owned()],
        ),
        (
            "2.1.220 (Claude Code)",
            "Usage: claude [options]",
            vec!["--model".to_owned(), "sonnet".to_owned()],
        ),
        (
            "2.1.220 (Claude Code)",
            proven_help,
            vec!["--settings".to_owned(), "/user/overlay.json".to_owned()],
        ),
        (
            "2.1.220 (Claude Code)",
            proven_help,
            vec!["--safe-mode".to_owned()],
        ),
        (
            "2.1.221 (Claude Code)",
            proven_help,
            vec!["--model".to_owned(), "sonnet".to_owned()],
        ),
    ] {
        assert!(
            matches!(
                claude_direct_preparation(version, help, args).await,
                TerminalLaunchPreparation::PassThrough
            ),
            "unsupported or conflicting settings semantics must leave the original launch unchanged"
        );
    }
}

async fn claude_fixture_terminal(
    terminal_id: &str,
) -> (
    tempfile::TempDir,
    TerminalManager,
    Arc<RecordingBackend>,
    ActivityProjection,
    ActivityScopeRef,
    Database,
    Arc<ClaudeTerminalObserverFactory>,
) {
    let fixture = claude_hook_fixture();
    let root = tempfile::tempdir().expect("fixture root");
    let configured = root.path().join("configured-claude");
    std::fs::write(&configured, b"configured").expect("configured Claude executable");
    let runner = Arc::new(ClaudeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["versionOutput"]
                    .as_str()
                    .expect("version output")
                    .to_owned(),
                stderr: String::new(),
            }),
            Ok(ClaudeProbeOutput {
                success: true,
                stdout: fixture["rootHelp"].as_str().expect("root help").to_owned(),
                stderr: String::new(),
            }),
        ])),
    });
    let factory = Arc::new(ClaudeTerminalObserverFactory::with_pinner(
        Arc::new(CachedClaudeCapabilityProbe::with_attestor(
            runner,
            Arc::new(ClaudeAttestorFixture),
        )),
        Arc::new(CopyClaudeExecutablePinner),
    ));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database.clone()));
    let mut settings = ProviderSettingsState::default();
    settings.providers.claude_agent.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            claude: Some(factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("Claude supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-claude",
        terminal_id,
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": configured,
            "args": fixture["originalArgs"],
            "label": "Claude",
            "activity": {
                "driverKind": "claudeAgent",
                "providerInstanceId": "claudeAgent",
            },
        }))
        .expect("Claude launch"),
    );
    manager.open(input).await.expect("Claude terminal");
    let pinned_executable =
        std::path::PathBuf::from(backend.spawns().pop().expect("Claude PTY spawn").executable);
    assert_eq!(
        std::fs::read(
            pinned_executable
                .parent()
                .expect("Claude generation directory")
                .join(".bibcode-provider-terminal-owner"),
        )
        .expect("Claude generation ownership marker"),
        b"bibcode-provider-terminal-v1\n",
        "every live Claude credential directory must carry the exact cleanup ownership marker"
    );
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-claude".to_owned(),
        terminal_id: terminal_id.to_owned(),
    };
    (root, manager, backend, projection, scope, database, factory)
}

fn claude_hook_launch(backend: &RecordingBackend) -> (String, String, String, std::path::PathBuf) {
    let spawn = backend.spawns().pop().expect("Claude PTY spawn");
    assert_eq!(
        &spawn.args[..4],
        ["--model", "sonnet", "--permission-mode", "plan"],
        "the observer must preserve the requested interactive arguments"
    );
    assert!(
        !spawn.args.iter().any(|arg| arg == "--setting-sources"),
        "default user/project/local setting sources must stay enabled"
    );
    let settings_index = spawn
        .args
        .iter()
        .position(|arg| arg == "--settings")
        .expect("additional settings overlay");
    let settings_path = std::path::PathBuf::from(
        spawn
            .args
            .get(settings_index + 1)
            .expect("settings overlay path"),
    );
    let overlay: Value = serde_json::from_slice(
        &std::fs::read(&settings_path).expect("read Claude settings overlay"),
    )
    .expect("settings overlay JSON");
    let events = overlay["hooks"].as_object().expect("overlay hook events");
    assert_eq!(
        events
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "PostToolUse",
            "PostToolUseFailure",
            "PreToolUse",
            "SubagentStart",
            "SubagentStop",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&settings_path)
            .expect("settings overlay metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let handler = &overlay["hooks"]["SubagentStart"][0]["hooks"][0];
    let endpoint = handler["url"].as_str().expect("hook endpoint").to_owned();
    assert!(endpoint.starts_with("http://127.0.0.1:"));
    assert!(!endpoint.contains("token"));
    let correlation = handler["headers"]["X-BiBCode-Launch-Correlation"]
        .as_str()
        .expect("launch correlation")
        .to_owned();
    assert!(
        !spawn.env.contains_key("BIBCODE_CLAUDE_TERMINAL_HOOK_TOKEN"),
        "the bearer token must not reach Claude or tool subprocess environments"
    );
    let token = handler["headers"]["Authorization"]
        .as_str()
        .and_then(|value| value.strip_prefix("Bearer "))
        .expect("literal private bearer in mode-0600 overlay")
        .to_owned();
    assert_eq!(token.len(), 64, "the bearer token must contain 256 bits");
    assert!(
        !format!("{spawn:?}").contains(&token),
        "PTY diagnostics must not expose settings content"
    );
    (endpoint, token, correlation, settings_path)
}

async fn post_claude_hook(
    endpoint: &str,
    token: &str,
    correlation: &str,
    body: Value,
) -> reqwest::Response {
    let client = reqwest::Client::new();
    let mut last_error = None;
    for _ in 0..40 {
        match client
            .post(endpoint)
            .bearer_auth(token)
            .header("X-BiBCode-Launch-Correlation", correlation)
            .json(&body)
            .send()
            .await
        {
            Ok(response) => return response,
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
    panic!("Claude hook sink did not start: {last_error:?}");
}

async fn begin_incomplete_claude_hook(
    endpoint: &str,
    token: &str,
    correlation: &str,
    first_chunk: &[u8],
    body_len: usize,
) -> tokio::net::TcpStream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let endpoint = reqwest::Url::parse(endpoint).expect("hook URL");
        let host = endpoint.host_str().expect("hook host");
        let port = endpoint.port().expect("hook port");
        let mut socket = tokio::net::TcpStream::connect((host, port))
            .await
            .expect("hook socket");
        let headers = format!(
            "POST {} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Bearer {token}\r\nX-BiBCode-Launch-Correlation: {correlation}\r\nContent-Type: application/json\r\nContent-Length: {body_len}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n",
            endpoint.path(),
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("hook headers");
        // Hyper sends Continue only when capture_claude_hook polls the body,
        // after authenticating and capturing the activity admission generation.
        let mut continued = [0_u8; 25];
        socket
            .read_exact(&mut continued)
            .await
            .expect("hook body admission response");
        assert_eq!(&continued, b"HTTP/1.1 100 Continue\r\n\r\n");
        socket
            .write_all(first_chunk)
            .await
            .expect("first admitted body chunk");
        socket
    })
    .await
    .expect("Claude hook must admit the body before toggling activity")
}

async fn complete_claude_hook_body(
    mut socket: tokio::net::TcpStream,
    remainder: &[u8],
) -> reqwest::StatusCode {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        socket
            .write_all(remainder)
            .await
            .expect("release the admitted hook body");
        let mut response = String::new();
        BufReader::new(socket)
            .read_line(&mut response)
            .await
            .expect("hook response status");
        let status = response
            .strip_prefix("HTTP/1.1 ")
            .and_then(|line| line.split_whitespace().next())
            .unwrap_or_else(|| panic!("invalid hook response: {response:?}"));
        reqwest::StatusCode::from_bytes(status.as_bytes()).expect("hook HTTP status")
    })
    .await
    .expect("completed Claude hook body must receive a response")
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_claude_hook_is_dormant_without_stopping_terminal() {
    let fixture = claude_hook_fixture();
    let (root, manager, backend, projection, scope, _database, _factory) =
        claude_fixture_terminal("terminal-claude-dormant").await;
    let (endpoint, token, correlation, settings_path) = claude_hook_launch(&backend);
    let process = backend.latest();

    let client = reqwest::Client::new();
    let mut hook_ready = false;
    for _ in 0..40 {
        if client
            .post(&endpoint)
            .send()
            .await
            .is_ok_and(|response| response.status() == reqwest::StatusCode::FORBIDDEN)
        {
            hook_ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(hook_ready, "Claude hook sink must start before streaming");

    let old_hook = serde_json::to_vec(&correlated_claude_root_hook(&fixture, &root))
        .expect("old-generation hook JSON");
    let split = old_hook.len() / 2;
    let request = begin_incomplete_claude_hook(
        &endpoint,
        &token,
        &correlation,
        &old_hook[..split],
        old_hook.len(),
    )
    .await;

    // The hook sink bounds an incomplete body with a fixed deadline, so the
    // in-flight body only spans the two toggles: anything slower (an HTTP
    // round trip, a projection read) on a starved runner would let the
    // server answer before the body is completed.
    let stopped = manager.set_agent_activity_enabled(false).await;
    assert_eq!(stopped.stopped, 1);
    assert_eq!(stopped.dormant, 1);
    assert!(!process.killed.load(Ordering::Acquire));
    let resumed = manager.set_agent_activity_enabled(true).await;
    assert_eq!(resumed.resumed, 1);
    let response = complete_claude_hook_body(request, &old_hook[split..]).await;
    assert_eq!(response, reqwest::StatusCode::NO_CONTENT);
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "an old-generation hook body must not create activity after re-enable",
    );

    let malformed_remainder = b"invalid JSON";
    let request = begin_incomplete_claude_hook(
        &endpoint,
        &token,
        &correlation,
        b"{",
        1 + malformed_remainder.len(),
    )
    .await;

    let stopped = manager.set_agent_activity_enabled(false).await;
    assert_eq!(stopped.stopped, 1);
    let response = complete_claude_hook_body(request, malformed_remainder).await;
    assert_eq!(
        response,
        reqwest::StatusCode::NO_CONTENT,
        "a body completed while dormant must be rejected before JSON parsing",
    );
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "a body completed while dormant must not create tracker state",
    );

    // Still dormant, with no body in flight: a fresh hook is refused before
    // its body is decoded and publishes nothing.
    let response = reqwest::Client::new()
        .post(&endpoint)
        .bearer_auth(&token)
        .header("X-BiBCode-Launch-Correlation", &correlation)
        .header("content-type", "application/json")
        .body("{this body must not be decoded while dormant")
        .send()
        .await
        .expect("dormant Claude hook response");
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "dormant hook must not publish or create tracker state"
    );
    let resumed = manager.set_agent_activity_enabled(true).await;
    assert_eq!(resumed.resumed, 1);

    let response = post_claude_hook(
        &endpoint,
        &token,
        &correlation,
        correlated_claude_root_hook(&fixture, &root),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resumed Claude hook activity");
    assert!(!process.killed.load(Ordering::Acquire));

    process.exit(0);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while settings_path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal exit removes dormant-capable Claude resources");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_overlay_is_private_additive_and_first_authenticated_root_hook_gates_capability() {
    let fixture = claude_hook_fixture();
    let (root, manager, backend, projection, scope, _database, _factory) =
        claude_fixture_terminal("terminal-claude").await;
    let (endpoint, token, correlation, settings_path) = claude_hook_launch(&backend);
    let root_hook = correlated_claude_root_hook(&fixture, &root);

    assert!(
        projection.snapshot(&scope).await.is_err(),
        "no dock may publish before native root correlation"
    );
    let response = post_claude_hook(&endpoint, &token, &correlation, root_hook).await;
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.capabilities.terminal_observation
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("authenticated Claude root handshake");
    assert_eq!(snapshot.provider, "claude");
    assert_eq!(snapshot.actors.len(), 1);
    assert!(snapshot.capabilities.actors);
    assert!(snapshot.capabilities.attributed_activity);
    assert!(!snapshot.capabilities.background_work);

    manager
        .close("thread-claude", Some("terminal-claude"))
        .await
        .expect("close terminal");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while settings_path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("settings overlay cleanup");
    assert!(
        reqwest::Client::new().post(endpoint).send().await.is_err(),
        "closing the terminal must stop the hook sink"
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_hook_sink_rejects_wrong_credentials_session_content_type_and_size_without_mutation()
{
    let fixture = claude_hook_fixture();
    let (root, manager, backend, projection, scope, _database, _factory) =
        claude_fixture_terminal("terminal-rejections").await;
    let (endpoint, token, correlation, _settings_path) = claude_hook_launch(&backend);
    let root_hook = correlated_claude_root_hook(&fixture, &root);
    let client = reqwest::Client::new();

    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth("0".repeat(64))
            .header("X-BiBCode-Launch-Correlation", &correlation)
            .json(&root_hook)
            .send()
            .await
            .expect("wrong token response")
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&token)
            .header("X-BiBCode-Launch-Correlation", "wrong-correlation")
            .json(&root_hook)
            .send()
            .await
            .expect("wrong correlation response")
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&token)
            .header("X-BiBCode-Launch-Correlation", &correlation)
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body("{}")
            .send()
            .await
            .expect("wrong content type response")
            .status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        client
            .post(&endpoint)
            .bearer_auth(&token)
            .header("X-BiBCode-Launch-Correlation", &correlation)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(format!("\"{}\"", "x".repeat(1_048_577)))
            .send()
            .await
            .expect("oversized response")
            .status(),
        reqwest::StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        post_claude_hook(
            &endpoint,
            &token,
            &correlation,
            serde_json::json!({
                "hook_event_name": "SessionStart",
                "session_id": "claude-root-session"
            }),
        )
        .await
        .status(),
        reqwest::StatusCode::BAD_REQUEST,
        "an unconfigured hook event must not establish root correlation"
    );
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "rejected HTTP input must not create a graph scope"
    );
    assert_eq!(
        post_claude_hook(
            &endpoint,
            &token,
            &correlation,
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "malformed-root",
            }),
        )
        .await
        .status(),
        reqwest::StatusCode::BAD_REQUEST,
        "event-specific fields and common root evidence are required before handshake"
    );
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "malformed authenticated hooks must not publish terminal capability"
    );
    let mut relative_transcript = root_hook.clone();
    relative_transcript["transcript_path"] = Value::String("transcript.jsonl".to_owned());
    let mut relative_cwd = root_hook.clone();
    relative_cwd["cwd"] = Value::String(".".to_owned());
    for malformed_root in [relative_transcript, relative_cwd] {
        assert_eq!(
            post_claude_hook(&endpoint, &token, &correlation, malformed_root)
                .await
                .status(),
            reqwest::StatusCode::BAD_REQUEST,
            "common transcript/cwd evidence must be bounded absolute paths"
        );
    }
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "invalid common root evidence must not establish terminal capability"
    );

    assert_eq!(
        post_claude_hook(&endpoint, &token, &correlation, root_hook.clone())
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let malformed_hooks = [
        serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "claude-root-session",
            "agent_id": "claude-agent-2",
            "transcript_path": root.path().join("claude-root-session.jsonl"),
            "cwd": root.path(),
        }),
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "claude-root-session",
            "tool_name": "Read",
            "tool_use_id": "tool-1",
            "transcript_path": root.path().join("claude-root-session.jsonl"),
            "cwd": root.path(),
        }),
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "claude-root-session",
            "tool_name": "Read",
            "tool_use_id": "tool-1",
            "tool_input": {},
            "transcript_path": root.path().join("claude-root-session.jsonl"),
            "cwd": root.path(),
        }),
        serde_json::json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "claude-root-session",
            "tool_name": "Read",
            "tool_use_id": "tool-1",
            "tool_input": {},
            "transcript_path": root.path().join("claude-root-session.jsonl"),
            "cwd": root.path(),
        }),
        serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "claude-root-session",
            "agent_id": "claude-agent-1",
            "agent_type": "Explore",
            "agent_transcript_path": root.path().join("agent-claude-agent-1.jsonl"),
            "last_assistant_message": "done",
            "transcript_path": root.path().join("claude-root-session.jsonl"),
            "cwd": root.path(),
        }),
    ];
    for malformed in malformed_hooks {
        assert_eq!(
            post_claude_hook(&endpoint, &token, &correlation, malformed)
                .await
                .status(),
            reqwest::StatusCode::BAD_REQUEST,
            "missing documented event fields must be rejected"
        );
    }
    assert_eq!(
        post_claude_hook(
            &endpoint,
            &token,
            &correlation,
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "claude-root-session",
                "tool_name": "Read",
                "tool_use_id": "tool-cwd-change",
                "tool_input": {},
                "transcript_path": root.path().join("provider-chosen-transcript.jsonl"),
                "cwd": root.path().parent().expect("fixture parent"),
            }),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT,
        "documented provider paths are evidence, not synthetic launch/session correlations"
    );
    let mut foreign = root_hook;
    foreign["session_id"] = Value::String("foreign-root".to_owned());
    foreign["transcript_path"] = Value::String(
        root.path()
            .join("foreign-root.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    foreign["agent_id"] = Value::String("foreign-agent".to_owned());
    assert_eq!(
        post_claude_hook(&endpoint, &token, &correlation, foreign)
            .await
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    let snapshot = projection.snapshot(&scope).await.expect("root scope");
    assert_eq!(
        snapshot.actors.len(),
        1,
        "foreign hooks must not mutate graph"
    );

    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_authenticated_stop_recovers_a_bounded_correlated_child_transcript() {
    let fixture = claude_hook_fixture();
    let (root, manager, backend, projection, scope, _database, _factory) =
        claude_fixture_terminal("terminal-recovery").await;
    let (endpoint, token, correlation, _settings_path) = claude_hook_launch(&backend);
    let root_hook = correlated_claude_root_hook(&fixture, &root);
    assert_eq!(
        post_claude_hook(&endpoint, &token, &correlation, root_hook)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let transcript = root.path().join("child.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"assistant","sessionId":"claude-root-session","agentId":"claude-agent-1","isSidechain":true,"uuid":"message-1","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Recovered terminal commentary"}]}}"#,
            "\n"
        ),
    )
    .expect("child transcript");
    let stop = serde_json::json!({
        "hook_event_name": "SubagentStop",
        "session_id": "claude-root-session",
        "agent_id": "claude-agent-1",
        "agent_type": "Explore",
        "stop_hook_active": false,
        "agent_transcript_path": transcript,
        "transcript_path": root.path().join("claude-root-session.jsonl"),
        "last_assistant_message": "done",
        "cwd": root.path(),
    });
    assert_eq!(
        post_claude_hook(&endpoint, &token, &correlation, stop)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.capabilities.history_recovery == ActivityHistoryRecovery::Bounded
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded transcript recovery");
    let actor = snapshot.actors.first().expect("recovered actor");
    let detail = projection
        .list_detail(
            &scope,
            &snapshot.scope_id,
            ActivityRecordKind::Actor,
            &actor.id,
            None,
            20,
        )
        .await
        .expect("actor detail");
    assert!(
        serde_json::to_string(&detail)
            .expect("detail JSON")
            .contains("Recovered terminal commentary")
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_terminal_restart_interrupts_old_activity_before_a_new_root_handshake() {
    let fixture = claude_hook_fixture();
    let (root, manager, backend, projection, scope, _database, _factory) =
        claude_fixture_terminal("terminal-restart").await;
    let old_pinned_executable = std::path::PathBuf::from(
        backend
            .spawns()
            .last()
            .expect("old Claude spawn")
            .executable
            .clone(),
    );
    let (old_endpoint, token, correlation, old_settings_path) = claude_hook_launch(&backend);
    let root_hook = correlated_claude_root_hook(&fixture, &root);
    assert_eq!(
        post_claude_hook(&old_endpoint, &token, &correlation, root_hook.clone(),)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let old_snapshot = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.actors.len() == 1
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old Claude scope");
    let old_scope_id = old_snapshot.scope_id.clone();

    let configured = root.path().join("configured-claude");
    let mut restart = TerminalOpenInput::new(
        "thread-claude",
        "terminal-restart",
        root.path().to_path_buf(),
        80,
        24,
    );
    restart.command = Some(
        serde_json::from_value(serde_json::json!({
            "executable": configured,
            "args": fixture["originalArgs"],
            "label": "Claude",
            "activity": {
                "driverKind": "claudeAgent",
                "providerInstanceId": "claudeAgent",
            },
        }))
        .expect("Claude restart launch"),
    );
    manager.restart(restart).await.expect("Claude restart");

    let interrupted = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot
                    .actors
                    .first()
                    .is_some_and(|actor| actor.status.as_str() == "interrupted")
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old Claude activity interruption");
    assert_eq!(interrupted.scope_id, old_scope_id);
    assert!(!old_settings_path.exists());
    assert!(!old_pinned_executable.exists());
    assert!(
        reqwest::Client::new()
            .post(old_endpoint)
            .send()
            .await
            .is_err()
    );

    let (new_endpoint, new_token, new_correlation, new_settings_path) =
        claude_hook_launch(&backend);
    assert_ne!(new_settings_path, old_settings_path);
    assert_eq!(
        post_claude_hook(&new_endpoint, &new_token, &new_correlation, root_hook,)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let replacement = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(snapshot) = projection.snapshot(&scope).await
                && snapshot.scope_id != old_scope_id
                && snapshot.capabilities.terminal_observation
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("new Claude root correlation");
    assert_ne!(replacement.scope_id, old_scope_id);
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_failed_stop_projection_is_staged_until_cancellation_can_interrupt_history() {
    let fixture = claude_hook_fixture();
    let (root, manager, backend, projection, scope, database, _factory) =
        claude_fixture_terminal("terminal-failed-stop").await;
    let (endpoint, token, correlation, _settings_path) = claude_hook_launch(&backend);
    let root_hook = correlated_claude_root_hook(&fixture, &root);
    assert_eq!(
        post_claude_hook(&endpoint, &token, &correlation, root_hook.clone())
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.actors.len() == 1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("root actor projection");

    database
        .call(|connection| {
            connection.execute_batch(
                "ALTER TABLE activity_records RENAME TO activity_records_unavailable;",
            )?;
            Ok(())
        })
        .await
        .expect("temporarily disable activity-record persistence");
    let mut stop = root_hook;
    stop["hook_event_name"] = Value::String("SubagentStop".to_owned());
    stop["stop_hook_active"] = Value::Bool(false);
    stop["agent_transcript_path"] = Value::String(
        root.path()
            .join("agent-claude-agent-1.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    stop["last_assistant_message"] = Value::String("done".to_owned());
    assert_eq!(
        post_claude_hook(&endpoint, &token, &correlation, stop)
            .await
            .status(),
        reqwest::StatusCode::GONE
    );
    database
        .call(|connection| {
            connection.execute_batch(
                "ALTER TABLE activity_records_unavailable RENAME TO activity_records;",
            )?;
            Ok(())
        })
        .await
        .expect("restore activity-record persistence");

    manager
        .close("thread-claude", Some("terminal-failed-stop"))
        .await
        .expect("close terminal");
    let snapshot = projection
        .snapshot(&scope)
        .await
        .expect("inspect retained failed-stop history");
    assert_eq!(snapshot.actors.len(), 1);
    assert_eq!(
        snapshot.actors[0].status,
        ActivityLifecycle::Interrupted,
        "failed terminal mutations must not remove actors from observer state before persistence"
    );
    manager.shutdown().await;
}

#[derive(Debug, Default)]
struct OpenCodeProbeFixtureRunner {
    calls: Mutex<Vec<Vec<String>>>,
    outputs: Mutex<VecDeque<OpenCodeProbeOutput>>,
    delay: std::time::Duration,
}

impl OpenCodeCapabilityProbeRunner for OpenCodeProbeFixtureRunner {
    fn run(
        &self,
        _executable: &std::path::Path,
        args: Vec<String>,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeProbeOutput, String>> + Send + '_>> {
        Box::pin(async move {
            let output = self
                .outputs
                .lock()
                .expect("OpenCode probe outputs")
                .pop_front()
                .ok_or_else(|| "missing OpenCode probe output".to_owned());
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.calls.lock().expect("OpenCode probe calls").push(args);
            output
        })
    }
}

#[derive(Debug)]
struct OpenCodeFixtureProcess {
    terminated: AtomicBool,
    reaped: AtomicBool,
    reap_calls: AtomicUsize,
    reap_delay: std::time::Duration,
    timeline: Arc<Mutex<Vec<&'static str>>>,
}

impl OpenCodeHelperProcess for OpenCodeFixtureProcess {
    fn terminate(&self) {
        if !self.terminated.swap(true, Ordering::AcqRel) {
            self.timeline
                .lock()
                .expect("OpenCode topology timeline")
                .push("helper-terminate");
        }
    }

    fn terminate_and_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if !self.reap_delay.is_zero() {
                tokio::time::sleep(self.reap_delay).await;
            }
            self.terminate();
            self.reap_calls.fetch_add(1, Ordering::AcqRel);
            self.reaped.store(true, Ordering::Release);
        })
    }
}

#[derive(Debug, Default)]
struct OpenCodeFixtureHelperLauncher {
    launches: Mutex<Vec<OpenCodeHelperLaunch>>,
    endpoint: Mutex<Option<String>>,
    endpoints: Mutex<VecDeque<String>>,
    fail: AtomicBool,
    processes: Mutex<Vec<Arc<OpenCodeFixtureProcess>>>,
    reap_delay: std::time::Duration,
    timeline: Arc<Mutex<Vec<&'static str>>>,
}

impl OpenCodeHelperLauncher for OpenCodeFixtureHelperLauncher {
    fn start(
        &self,
        launch: OpenCodeHelperLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeHelperReady, String>> + Send + '_>> {
        Box::pin(async move {
            self.timeline
                .lock()
                .expect("OpenCode topology timeline")
                .push("helper");
            self.launches
                .lock()
                .expect("OpenCode helper launches")
                .push(launch);
            if self.fail.load(Ordering::Acquire) {
                return Err("injected OpenCode helper failure".to_owned());
            }
            let process = Arc::new(OpenCodeFixtureProcess {
                terminated: AtomicBool::new(false),
                reaped: AtomicBool::new(false),
                reap_calls: AtomicUsize::new(0),
                reap_delay: self.reap_delay,
                timeline: self.timeline.clone(),
            });
            self.processes
                .lock()
                .expect("OpenCode helper processes")
                .push(process.clone());
            Ok(OpenCodeHelperReady {
                endpoint: self
                    .endpoints
                    .lock()
                    .expect("OpenCode fixture endpoints")
                    .pop_front()
                    .or_else(|| {
                        self.endpoint
                            .lock()
                            .expect("OpenCode fixture endpoint")
                            .clone()
                    })
                    .expect("configured OpenCode fixture endpoint"),
                process,
            })
        })
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ActualExitedOpenCodeHelperProcess {
    child: Mutex<std::process::Child>,
    cleanup_calls: AtomicUsize,
}

#[cfg(unix)]
impl ActualExitedOpenCodeHelperProcess {
    fn start() -> Arc<Self> {
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("actual OpenCode helper child");
        Arc::new(Self {
            child: Mutex::new(child),
            cleanup_calls: AtomicUsize::new(0),
        })
    }

    fn exit_and_reap(&self) {
        let mut child = self.child.lock().expect("actual OpenCode helper child");
        let _ = child.kill();
        child.wait().expect("reap actual OpenCode helper child");
    }
}

#[cfg(unix)]
impl OpenCodeHelperProcess for ActualExitedOpenCodeHelperProcess {
    fn has_exited(&self) -> bool {
        self.child
            .lock()
            .expect("actual OpenCode helper child")
            .try_wait()
            .expect("observe actual OpenCode helper child")
            .is_some()
    }

    fn terminate(&self) {
        self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
        let _ = self
            .child
            .lock()
            .expect("actual OpenCode helper child")
            .kill();
    }

    fn terminate_and_reap(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.cleanup_calls.fetch_add(1, Ordering::AcqRel);
            self.exit_and_reap();
        })
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ActualExitedOpenCodeHelperLauncher {
    endpoint: String,
    process: Mutex<Option<Arc<ActualExitedOpenCodeHelperProcess>>>,
}

#[cfg(unix)]
impl OpenCodeHelperLauncher for ActualExitedOpenCodeHelperLauncher {
    fn start(
        &self,
        _launch: OpenCodeHelperLaunch,
    ) -> Pin<Box<dyn Future<Output = Result<OpenCodeHelperReady, String>> + Send + '_>> {
        Box::pin(async move {
            let process = ActualExitedOpenCodeHelperProcess::start();
            *self.process.lock().expect("actual OpenCode helper process") = Some(process.clone());
            Ok(OpenCodeHelperReady {
                endpoint: self.endpoint.clone(),
                process,
            })
        })
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct ExitHelperOnFirstSpawnBackend {
    helper: Arc<ActualExitedOpenCodeHelperLauncher>,
    spawns: Mutex<Vec<PtySpawnInput>>,
    processes: Mutex<Vec<Arc<RecordingProcess>>>,
}

#[cfg(unix)]
impl ExitHelperOnFirstSpawnBackend {
    fn spawns(&self) -> Vec<PtySpawnInput> {
        self.spawns.lock().expect("gap spawns").clone()
    }
}

#[cfg(unix)]
impl PtyBackend for ExitHelperOnFirstSpawnBackend {
    fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        let mut spawns = self.spawns.lock().expect("gap spawns");
        let first = spawns.is_empty();
        spawns.push(input.clone());
        drop(spawns);
        if first {
            self.helper
                .process
                .lock()
                .expect("actual OpenCode helper process")
                .as_ref()
                .expect("ready actual OpenCode helper")
                .exit_and_reap();
        }
        let process = Arc::new(RecordingProcess::new(
            u32::try_from(self.processes.lock().expect("gap processes").len() + 1)
                .expect("gap PID"),
        ));
        self.processes
            .lock()
            .expect("gap processes")
            .push(process.clone());
        Ok(process)
    }
}

type OpenCodeFixtureFrames = Arc<Mutex<VecDeque<Result<Vec<u8>, String>>>>;

#[derive(Clone, Debug)]
struct OpenCodeFixtureStreamScript {
    frames: OpenCodeFixtureFrames,
    notify: Arc<tokio::sync::Notify>,
    busy_dormant: bool,
}

impl OpenCodeFixtureStreamScript {
    fn new(frames: impl IntoIterator<Item = Result<Vec<u8>, String>>) -> Self {
        Self {
            frames: Arc::new(Mutex::new(frames.into_iter().collect())),
            notify: Arc::new(tokio::sync::Notify::new()),
            busy_dormant: false,
        }
    }

    fn busy_dormant(frames: impl IntoIterator<Item = Result<Vec<u8>, String>>) -> Self {
        Self {
            busy_dormant: true,
            ..Self::new(frames)
        }
    }
}

#[derive(Debug, Default)]
struct OpenCodeReplacementPause {
    entered: tokio::sync::Notify,
    released: (Mutex<bool>, std::sync::Condvar),
}

impl OpenCodeReplacementPause {
    fn wait_until_released(&self) {
        self.entered.notify_one();
        let mut released = self.released.0.lock().expect("OpenCode replacement pause");
        while !*released {
            released = self
                .released
                .1
                .wait(released)
                .expect("OpenCode replacement pause");
        }
    }

    fn release(&self) {
        *self.released.0.lock().expect("OpenCode replacement pause") = true;
        self.released.1.notify_all();
    }
}

#[derive(Debug)]
struct OpenCodeActivityTransitionProbe {
    first_polls: tokio::sync::Semaphore,
}

impl Default for OpenCodeActivityTransitionProbe {
    fn default() -> Self {
        Self {
            first_polls: tokio::sync::Semaphore::new(0),
        }
    }
}

impl OpenCodeActivityTransitionProbe {
    async fn wait_for_first_poll_while<F>(
        &self,
        transition: Pin<&mut F>,
    ) -> Option<TerminalAgentActivityTransition>
    where
        F: Future<Output = TerminalAgentActivityTransition> + ?Sized,
    {
        // Keep driving the manager future while it waits for callback
        // admission. If admission fails, surface that bounded result instead
        // of waiting forever for a provider callback that never started.
        tokio::select! {
            biased;
            permit = self.first_polls.acquire() => {
                permit
                    .expect("OpenCode activity transition probe")
                    .forget();
                None
            }
            completed = transition => {
                if let Ok(permit) = self.first_polls.try_acquire() {
                    permit.forget();
                    return Some(completed);
                }
                panic!(
                    "OpenCode activity transition completed before reaching the provider: {completed:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn opencode_activity_transition_probe_accepts_provider_completion_on_first_poll() {
    let activity_transition = Arc::new(OpenCodeActivityTransitionProbe::default());
    let provider_entry = activity_transition.clone();
    let mut transition = Box::pin(async move {
        provider_entry.first_polls.add_permits(1);
        TerminalAgentActivityTransition {
            stopped: 1,
            dormant: 1,
            ..TerminalAgentActivityTransition::default()
        }
    });

    let completed = activity_transition
        .wait_for_first_poll_while(transition.as_mut())
        .await
        .expect("provider completed on its first poll");
    assert_eq!((completed.stopped, completed.dormant), (1, 1));
}

struct ProbedOpenCodeObserverFactory {
    inner: Arc<dyn ProviderTerminalObserverFactory>,
    activity_transition: Arc<OpenCodeActivityTransitionProbe>,
}

impl ProviderTerminalObserverFactory for ProbedOpenCodeObserverFactory {
    fn requires_private_executable_pin(&self) -> bool {
        self.inner.requires_private_executable_pin()
    }

    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            let mut prepared = self.inner.prepare(input).await?;
            prepared.observer = Box::new(ProbedOpenCodeObserver {
                inner: prepared.observer,
                activity_transition: self.activity_transition.clone(),
            });
            Some(prepared)
        })
    }
}

struct ProbedOpenCodeObserver {
    inner: Box<dyn PreparedTerminalObserver>,
    activity_transition: Arc<OpenCodeActivityTransitionProbe>,
}

impl PreparedTerminalObserver for ProbedOpenCodeObserver {
    fn is_ready_for_on_spawned(&self) -> bool {
        self.inner.is_ready_for_on_spawned()
    }

    fn on_spawned(
        &self,
        pid: u32,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) {
        self.inner.on_spawned(pid, generation, workers);
    }

    fn agent_activity_enable_ack_timeout(&self) -> Option<std::time::Duration> {
        self.inner.agent_activity_enable_ack_timeout()
    }

    fn set_agent_activity_enabled(
        &self,
        enabled: bool,
        generation: TerminalObserverGenerationLease,
        workers: TerminalObserverWorkerContext,
    ) -> Pin<Box<dyn Future<Output = TerminalAgentActivityTransition> + Send + '_>> {
        let activity_transition = self.activity_transition.clone();
        let transition = self
            .inner
            .set_agent_activity_enabled(enabled, generation, workers);
        Box::pin(async move {
            tokio::pin!(transition);
            // The OpenCode delegate publishes its activity state before its
            // first pending wait for observer acknowledgement. The test waits
            // for this signal before constructing the next transition, so the
            // delegate's publication mutex is uncontended on this poll.
            let first_poll = std::future::poll_fn(|context| {
                std::task::Poll::Ready(transition.as_mut().poll(context))
            })
            .await;
            activity_transition.first_polls.add_permits(1);
            match first_poll {
                std::task::Poll::Ready(transition) => transition,
                std::task::Poll::Pending => transition.await,
            }
        })
    }

    fn diagnostic_label(&self) -> &str {
        self.inner.diagnostic_label()
    }
}

#[derive(Debug, Default)]
struct OpenCodeFixtureRemoteState {
    connections: Mutex<Vec<(String, String, String, String)>>,
    calls: Mutex<Vec<String>>,
    request_delays: Mutex<BTreeMap<String, VecDeque<std::time::Duration>>>,
    stream_scripts: Mutex<VecDeque<OpenCodeFixtureStreamScript>>,
    active_streams: Mutex<Vec<OpenCodeFixtureStreamScript>>,
    opened_stream_count: AtomicUsize,
    open_streams: AtomicUsize,
    maximum_open_streams: AtomicUsize,
    discarded_frames: AtomicUsize,
    decoded_activity_events: AtomicUsize,
    replacement_waiting: tokio::sync::Notify,
    replacement_pause: Mutex<Option<Arc<OpenCodeReplacementPause>>>,
    connect_fail: AtomicBool,
    hide_root: AtomicBool,
    dropped: AtomicBool,
    sessions: Mutex<Vec<Value>>,
    children: Mutex<BTreeMap<String, Value>>,
    statuses: Mutex<Value>,
    messages: Mutex<BTreeMap<String, Value>>,
    timeline: Arc<Mutex<Vec<&'static str>>>,
    root_session_id: Mutex<String>,
    root_session_ids: Mutex<VecDeque<String>>,
    #[cfg(unix)]
    exit_helper_on_create_root: Mutex<Option<Arc<ActualExitedOpenCodeHelperLauncher>>>,
}

async fn apply_opencode_fixture_delay(state: &OpenCodeFixtureRemoteState, operation: &str) {
    let delay = state
        .request_delays
        .lock()
        .expect("OpenCode request delays")
        .get_mut(operation)
        .and_then(VecDeque::pop_front)
        .unwrap_or_default();
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
}

#[derive(Debug)]
struct OpenCodeFixtureRemoteClient {
    state: Arc<OpenCodeFixtureRemoteState>,
}

#[derive(Debug)]
struct OpenCodeFixtureEventStream {
    state: Arc<OpenCodeFixtureRemoteState>,
    script: OpenCodeFixtureStreamScript,
    replacement: bool,
}

impl OpenCodeFixtureEventStream {
    async fn next_raw_frame(&self) -> Result<Vec<u8>, String> {
        loop {
            let notified = self.script.notify.notified();
            if let Some(frame) = self
                .script
                .frames
                .lock()
                .expect("OpenCode stream frames")
                .pop_front()
            {
                return frame;
            }
            if self.replacement {
                self.state.replacement_waiting.notify_one();
                let pause = self
                    .state
                    .replacement_pause
                    .lock()
                    .expect("OpenCode replacement pause")
                    .take();
                if let Some(pause) = pause {
                    pause.wait_until_released();
                }
            }
            notified.await;
        }
    }
}

impl Drop for OpenCodeFixtureEventStream {
    fn drop(&mut self) {
        let mut active = self
            .state
            .active_streams
            .lock()
            .expect("active OpenCode streams");
        if let Some(index) = active
            .iter()
            .position(|script| Arc::ptr_eq(&script.frames, &self.script.frames))
        {
            active.remove(index);
        }
        self.state.open_streams.fetch_sub(1, Ordering::AcqRel);
    }
}

impl OpenCodeEventStream for OpenCodeFixtureEventStream {
    fn discard_next(&mut self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            if !self.script.busy_dormant
                || !self
                    .script
                    .frames
                    .lock()
                    .expect("OpenCode stream frames")
                    .is_empty()
            {
                self.next_raw_frame().await?;
            }
            self.state.discarded_frames.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }

    fn next_data(&mut self) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + '_>> {
        Box::pin(async move {
            loop {
                let raw = match self.next_raw_frame().await {
                    Ok(raw) => raw,
                    Err(error) => {
                        self.state
                            .calls
                            .lock()
                            .expect("OpenCode remote calls")
                            .push("sse-error".to_owned());
                        return Err(error);
                    }
                };
                let Some(data) = opencode_fixture_frame_data(&raw)? else {
                    continue;
                };
                if serde_json::from_slice::<Value>(&data)
                    .ok()
                    .and_then(|event| event.get("type").and_then(Value::as_str).map(str::to_owned))
                    .is_some_and(|event_type| event_type != "server.connected")
                {
                    self.state
                        .decoded_activity_events
                        .fetch_add(1, Ordering::AcqRel);
                }
                self.state
                    .calls
                    .lock()
                    .expect("OpenCode remote calls")
                    .push("sse".to_owned());
                return Ok(data);
            }
        })
    }
}

impl Drop for OpenCodeFixtureRemoteClient {
    fn drop(&mut self) {
        self.state.dropped.store(true, Ordering::Release);
    }
}

impl OpenCodeRemoteClient for OpenCodeFixtureRemoteClient {
    fn create_root(
        &mut self,
        model: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let model = model.to_owned();
        Box::pin(async move {
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push("create-root".to_owned());
            self.state
                .timeline
                .lock()
                .expect("OpenCode topology timeline")
                .push("create-root");
            let root_session_id = self
                .state
                .root_session_ids
                .lock()
                .expect("OpenCode fixture root IDs")
                .pop_front()
                .unwrap_or_else(|| {
                    self.state
                        .root_session_id
                        .lock()
                        .expect("OpenCode fixture root ID")
                        .clone()
                });
            let root_session_id = if root_session_id.is_empty() {
                "root-tui-session".to_owned()
            } else {
                root_session_id
            };
            self.state
                .sessions
                .lock()
                .expect("OpenCode sessions")
                .push(serde_json::json!({
                    "id": root_session_id,
                    "title": "BiBCode terminal",
                    "model": model,
                }));
            #[cfg(unix)]
            if let Some(helper) = self
                .state
                .exit_helper_on_create_root
                .lock()
                .expect("create-root helper exit")
                .as_ref()
            {
                helper
                    .process
                    .lock()
                    .expect("actual OpenCode helper process")
                    .as_ref()
                    .expect("ready actual OpenCode helper")
                    .exit_and_reap();
            }
            Ok(root_session_id)
        })
    }

    fn cleanup_pre_spawn(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let root_session_id = root_session_id.to_owned();
        Box::pin(async move {
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push(format!("delete:{root_session_id}"));
            self.state
                .timeline
                .lock()
                .expect("OpenCode topology timeline")
                .push("delete-root");
            Ok(())
        })
    }

    fn abort(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        let root_session_id = root_session_id.to_owned();
        Box::pin(async move {
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push(format!("abort:{root_session_id}"));
            Ok(())
        })
    }

    fn root(
        &mut self,
        root_session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let root_session_id = root_session_id.to_owned();
        Box::pin(async move {
            apply_opencode_fixture_delay(&self.state, "root").await;
            if self.state.hide_root.load(Ordering::Acquire) {
                return Err("injected missing OpenCode root".to_owned());
            }
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push(format!("root:{root_session_id}"));
            self.state
                .sessions
                .lock()
                .expect("OpenCode sessions")
                .iter()
                .find(|session| {
                    session.get("id").and_then(Value::as_str) == Some(root_session_id.as_str())
                })
                .cloned()
                .ok_or_else(|| "missing OpenCode root".to_owned())
        })
    }

    fn statuses(&mut self) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        Box::pin(async move {
            apply_opencode_fixture_delay(&self.state, "statuses").await;
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push("statuses".to_owned());
            Ok(self
                .state
                .statuses
                .lock()
                .expect("OpenCode statuses")
                .clone())
        })
    }

    fn children(
        &mut self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let session_id = session_id.to_owned();
        Box::pin(async move {
            apply_opencode_fixture_delay(&self.state, "children").await;
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push(format!("children:{session_id}"));
            Ok(self
                .state
                .children
                .lock()
                .expect("OpenCode children")
                .get(&session_id)
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())))
        })
    }

    fn messages(
        &mut self,
        session_id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + '_>> {
        let session_id = session_id.to_owned();
        Box::pin(async move {
            apply_opencode_fixture_delay(&self.state, "messages").await;
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push(format!("messages:{session_id}"));
            Ok(self
                .state
                .messages
                .lock()
                .expect("OpenCode messages")
                .get(&session_id)
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())))
        })
    }

    fn open_event_stream(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn OpenCodeEventStream>, String>> + Send + '_>>
    {
        Box::pin(async move {
            let script = self
                .state
                .stream_scripts
                .lock()
                .expect("OpenCode stream scripts")
                .pop_front()
                .unwrap_or_else(|| {
                    OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())])
                });
            let replacement = self
                .state
                .opened_stream_count
                .fetch_add(1, Ordering::AcqRel)
                > 0;
            self.state
                .active_streams
                .lock()
                .expect("active OpenCode streams")
                .push(script.clone());
            let open_streams = self.state.open_streams.fetch_add(1, Ordering::AcqRel) + 1;
            self.state
                .maximum_open_streams
                .fetch_max(open_streams, Ordering::AcqRel);
            self.state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .push("open-stream".to_owned());
            Ok(Box::new(OpenCodeFixtureEventStream {
                state: self.state.clone(),
                script,
                replacement,
            }) as Box<dyn OpenCodeEventStream>)
        })
    }
}

fn opencode_fixture_frame_data(frame: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let frame = std::str::from_utf8(frame)
        .map_err(|_| "injected OpenCode frame was not UTF-8".to_owned())?;
    let mut data = Vec::new();
    for line in frame.lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push(b'\n');
            }
            data.extend_from_slice(value.trim_start().as_bytes());
        }
    }
    Ok((!data.is_empty()).then_some(data))
}

fn opencode_fixture_event_frame(event: Value) -> Vec<u8> {
    format!("data: {event}\n\n").into_bytes()
}

fn opencode_fixture_connected_frame() -> Vec<u8> {
    opencode_fixture_event_frame(serde_json::json!({
        "type": "server.connected",
        "properties": {},
    }))
}

#[derive(Debug)]
struct OpenCodeFixtureRemoteFactory {
    state: Arc<OpenCodeFixtureRemoteState>,
}

impl OpenCodeRemoteClientFactory for OpenCodeFixtureRemoteFactory {
    fn connect(
        &self,
        endpoint: &str,
        username: &str,
        password: &str,
        directory: &std::path::Path,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn OpenCodeRemoteClient>, String>> + Send + '_>>
    {
        let endpoint = endpoint.to_owned();
        let username = username.to_owned();
        let password = password.to_owned();
        let directory = directory.to_string_lossy().into_owned();
        Box::pin(async move {
            apply_opencode_fixture_delay(&self.state, "connect").await;
            self.state
                .timeline
                .lock()
                .expect("OpenCode topology timeline")
                .push("connect");
            self.state
                .connections
                .lock()
                .expect("OpenCode connections")
                .push((endpoint, username, password, directory));
            if self.state.connect_fail.load(Ordering::Acquire) {
                return Err("injected helper exit before PTY".to_owned());
            }
            Ok(Box::new(OpenCodeFixtureRemoteClient {
                state: self.state.clone(),
            }) as Box<dyn OpenCodeRemoteClient>)
        })
    }
}

fn opencode_attach_fixture() -> Value {
    serde_json::from_str(OPENCODE_ATTACH_FIXTURE).expect("OpenCode attach fixture")
}

fn opencode_fixture_probe(
    fixture: &Value,
) -> (
    Arc<OpenCodeProbeFixtureRunner>,
    Arc<OpenCodeTerminalObserverFactory>,
) {
    let runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(
            ["attachHelp"]
                .into_iter()
                .map(|field| OpenCodeProbeOutput {
                    success: true,
                    stdout: fixture[field]
                        .as_str()
                        .expect("OpenCode probe fixture")
                        .to_owned(),
                    stderr: String::new(),
                })
                .collect(),
        ),
        delay: std::time::Duration::from_millis(180),
    });
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoint: Mutex::new(Some(
            fixture["endpoint"]
                .as_str()
                .expect("OpenCode endpoint fixture")
                .to_owned(),
        )),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let remote = Arc::new(OpenCodeFixtureRemoteFactory {
        state: Arc::new(OpenCodeFixtureRemoteState::default()),
    });
    (
        runner.clone(),
        Arc::new(OpenCodeTerminalObserverFactory::new(
            Arc::new(CachedOpenCodeCapabilityProbe::new(runner.clone())),
            helper,
            remote,
            std::time::Duration::from_secs(10),
        )),
    )
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_feature_probe_requires_serve_and_attach_support() {
    let fixture = opencode_attach_fixture();
    let (runner, factory) = opencode_fixture_probe(&fixture);
    let root = tempfile::tempdir().expect("OpenCode probe root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode fixture supervisor");
    let input = TerminalLaunchPreparationInput {
        executable: configured.to_string_lossy().into_owned(),
        args: fixture["originalArgs"]
            .as_array()
            .expect("OpenCode original args")
            .iter()
            .map(|value| value.as_str().expect("OpenCode original arg").to_owned())
            .collect(),
        cwd: root.path().to_path_buf(),
        worktree_path: Some(root.path().to_path_buf()),
        launch_env: BTreeMap::from([(
            "OPENCODE_CONFIG_CONTENT".to_owned(),
            serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode config fixture"),
        )]),
        activity: ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        },
        generation: TerminalObserverGeneration::new(
            "thread-opencode-probe".to_owned(),
            "terminal-opencode-probe".to_owned(),
        ),
    };

    let prepared = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        supervisor.prepare(input),
    )
    .await
    .expect("feature probing must fit the terminal callback boundary");

    assert!(
        matches!(prepared, TerminalLaunchPreparation::Admitted(_, _)),
        "supported serve+attach build is observed"
    );
    let mut probe_calls = runner.calls.lock().expect("OpenCode probe calls").clone();
    probe_calls.sort();
    let mut expected_probe_calls = vec![vec!["attach".to_owned(), "--help".to_owned()]];
    expected_probe_calls.sort();
    assert_eq!(probe_calls, expected_probe_calls);

    let unsupported_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: "opencode attach <url>\n--dir".to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    assert_eq!(
        CachedOpenCodeCapabilityProbe::new(unsupported_runner)
            .probe(&configured)
            .await
            .map(|capabilities| capabilities.attach),
        Some(false),
        "attach builds without exact-session selection must fail closed"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_cancelled_preparation_terminates_ready_helper_before_pass_through() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode cancellation root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoint: Mutex::new(Some(
            fixture["endpoint"]
                .as_str()
                .expect("OpenCode endpoint fixture")
                .to_owned(),
        )),
        reap_delay: std::time::Duration::from_millis(75),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(
            ["attachHelp"]
                .into_iter()
                .map(|field| OpenCodeProbeOutput {
                    success: true,
                    stdout: fixture[field]
                        .as_str()
                        .expect("OpenCode probe fixture")
                        .to_owned(),
                    stderr: String::new(),
                })
                .collect(),
        ),
        delay: std::time::Duration::from_secs(2),
    });
    let remote = Arc::new(OpenCodeFixtureRemoteFactory {
        state: Arc::new(OpenCodeFixtureRemoteState::default()),
    });
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::new(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper.clone(),
                remote,
                std::time::Duration::from_secs(1),
            ))),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode cancellation supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-opencode-cancel",
        "terminal-opencode-cancel",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "openai/gpt-5.2".to_owned()],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });

    manager.open(input).await.expect("OpenCode pass-through");

    assert_eq!(
        backend
            .spawns()
            .pop()
            .expect("OpenCode pass-through spawn")
            .args,
        ["--model", "openai/gpt-5.2"]
    );
    tokio::time::timeout(std::time::Duration::from_millis(200), async {
        loop {
            if helper
                .processes
                .lock()
                .expect("OpenCode helper processes")
                .first()
                .is_some_and(|process| process.terminated.load(Ordering::Acquire))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed-out OpenCode helper cleanup precedes outer fallback");
    assert!(
        helper
            .processes
            .lock()
            .expect("OpenCode helper processes")
            .first()
            .is_some_and(|process| process.reaped.load(Ordering::Acquire)),
        "preparation fallback must allow the generation worker to reap the helper asynchronously"
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_spawn_failure_deletes_owned_root_before_terminating_helper() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode spawn-failure root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoint: Mutex::new(Some(
            fixture["endpoint"]
                .as_str()
                .expect("OpenCode endpoint fixture")
                .to_owned(),
        )),
        timeline: timeline.clone(),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let remote_state = Arc::new(OpenCodeFixtureRemoteState {
        timeline: timeline.clone(),
        ..OpenCodeFixtureRemoteState::default()
    });
    let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: fixture["attachHelp"]
                .as_str()
                .expect("OpenCode attach help")
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::new(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper,
                Arc::new(OpenCodeFixtureRemoteFactory {
                    state: remote_state.clone(),
                }),
                std::time::Duration::from_secs(1),
            ))),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode spawn-failure supervisor");
    let backend = Arc::new(RecordingBackend::new(timeline.clone()));
    backend.fail_next("injected OpenCode PTY spawn failure");
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-opencode-spawn-failure",
        "terminal-opencode-spawn-failure",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "openai/gpt-5.2".to_owned()],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });

    manager
        .open(input)
        .await
        .expect_err("injected OpenCode PTY spawn failure");

    assert_eq!(
        timeline
            .lock()
            .expect("OpenCode topology timeline")
            .as_slice(),
        [
            "helper",
            "connect",
            "create-root",
            "spawn",
            "delete-root",
            "helper-terminate",
        ],
        "pre-spawn cleanup must delete the owned root before stopping its API"
    );
    assert!(
        remote_state
            .calls
            .lock()
            .expect("OpenCode remote calls")
            .iter()
            .any(|call| call == "delete:root-tui-session")
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_root_correlation_timeout_keeps_attach_usable_until_terminal_closes() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode correlation-timeout root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoint: Mutex::new(Some(
            fixture["endpoint"]
                .as_str()
                .expect("OpenCode endpoint fixture")
                .to_owned(),
        )),
        timeline: timeline.clone(),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let remote_state = Arc::new(OpenCodeFixtureRemoteState {
        hide_root: AtomicBool::new(true),
        timeline: timeline.clone(),
        ..OpenCodeFixtureRemoteState::default()
    });
    let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: fixture["attachHelp"]
                .as_str()
                .expect("OpenCode attach help")
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::new(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper.clone(),
                Arc::new(OpenCodeFixtureRemoteFactory {
                    state: remote_state.clone(),
                }),
                std::time::Duration::from_millis(50),
            ))),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode correlation-timeout supervisor");
    let backend = Arc::new(RecordingBackend::new(timeline));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-opencode-correlation-timeout",
        "terminal-opencode-correlation-timeout",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "openai/gpt-5.2".to_owned()],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });

    manager.open(input).await.expect("OpenCode attach terminal");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert_eq!(
        backend
            .spawns()
            .first()
            .and_then(|spawn| spawn.args.first())
            .map(String::as_str),
        Some("attach")
    );
    assert!(
        !helper
            .processes
            .lock()
            .expect("OpenCode helper processes")
            .first()
            .expect("OpenCode helper process")
            .terminated
            .load(Ordering::Acquire),
        "correlation timeout must not dock the usable attach session"
    );
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-opencode-correlation-timeout".to_owned(),
        terminal_id: "terminal-opencode-correlation-timeout".to_owned(),
    };
    assert!(
        projection.snapshot(&scope).await.is_err(),
        "activity remains fail-closed when the exact owned root is not confirmed"
    );

    manager
        .close(
            "thread-opencode-correlation-timeout",
            Some("terminal-opencode-correlation-timeout"),
        )
        .await
        .expect("close terminal");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if helper
                .processes
                .lock()
                .expect("OpenCode helper processes")
                .first()
                .is_some_and(|process| process.terminated.load(Ordering::Acquire))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("OpenCode helper closes with terminal generation");
    assert!(
        remote_state
            .calls
            .lock()
            .expect("OpenCode remote calls")
            .iter()
            .any(|call| call == "abort:root-tui-session")
    );
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_publication_failure_keeps_attach_usable_until_terminal_closes() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode publication-failure root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoint: Mutex::new(Some(
            fixture["endpoint"]
                .as_str()
                .expect("OpenCode endpoint fixture")
                .to_owned(),
        )),
        timeline: timeline.clone(),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let remote_state = Arc::new(OpenCodeFixtureRemoteState {
        timeline: timeline.clone(),
        ..OpenCodeFixtureRemoteState::default()
    });
    let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: fixture["attachHelp"]
                .as_str()
                .expect("OpenCode attach help")
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let database = Database::open_in_memory().await.expect("database");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection,
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::new(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper.clone(),
                Arc::new(OpenCodeFixtureRemoteFactory {
                    state: remote_state.clone(),
                }),
                std::time::Duration::from_millis(50),
            ))),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode publication-failure supervisor");
    let backend = Arc::new(RecordingBackend::new(timeline));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-opencode-publication-failure",
        "terminal-opencode-publication-failure",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "openai/gpt-5.2".to_owned()],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });

    manager.open(input).await.expect("OpenCode attach terminal");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert_eq!(
        backend
            .spawns()
            .first()
            .and_then(|spawn| spawn.args.first())
            .map(String::as_str),
        Some("attach")
    );
    assert!(
        !helper
            .processes
            .lock()
            .expect("OpenCode helper processes")
            .first()
            .expect("OpenCode helper process")
            .terminated
            .load(Ordering::Acquire),
        "activity publication failure must not dock the usable attach session"
    );

    manager
        .close(
            "thread-opencode-publication-failure",
            Some("terminal-opencode-publication-failure"),
        )
        .await
        .expect("close terminal");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if helper
                .processes
                .lock()
                .expect("OpenCode helper processes")
                .first()
                .is_some_and(|process| process.terminated.load(Ordering::Acquire))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("OpenCode helper closes with terminal generation");
    assert!(
        remote_state
            .calls
            .lock()
            .expect("OpenCode remote calls")
            .iter()
            .any(|call| call == "abort:root-tui-session")
    );
    manager.shutdown().await;
}

#[cfg(unix)]
struct OpenCodeToggleFixture {
    _root: tempfile::TempDir,
    manager: TerminalManager,
    projection: ActivityProjection,
    remote_state: Arc<OpenCodeFixtureRemoteState>,
    helper: Arc<OpenCodeFixtureHelperLauncher>,
    backend: Arc<RecordingBackend>,
}

#[cfg(unix)]
impl OpenCodeToggleFixture {
    async fn open(stream_scripts: Vec<OpenCodeFixtureStreamScript>) -> Self {
        Self::open_with_reattach_timeout(stream_scripts, std::time::Duration::from_secs(3)).await
    }

    async fn open_with_reattach_timeout(
        stream_scripts: Vec<OpenCodeFixtureStreamScript>,
        reattach_timeout: std::time::Duration,
    ) -> Self {
        Self::open_with_reattach_timeout_and_probe(stream_scripts, reattach_timeout, None).await
    }

    async fn open_with_activity_transition_probe(
        stream_scripts: Vec<OpenCodeFixtureStreamScript>,
    ) -> (Self, Arc<OpenCodeActivityTransitionProbe>) {
        let activity_transition = Arc::new(OpenCodeActivityTransitionProbe::default());
        let fixture = Self::open_with_reattach_timeout_and_probe(
            stream_scripts,
            std::time::Duration::from_secs(3),
            Some(activity_transition.clone()),
        )
        .await;
        (fixture, activity_transition)
    }

    async fn open_with_reattach_timeout_and_probe(
        stream_scripts: Vec<OpenCodeFixtureStreamScript>,
        reattach_timeout: std::time::Duration,
        activity_transition: Option<Arc<OpenCodeActivityTransitionProbe>>,
    ) -> Self {
        let fixture = opencode_attach_fixture();
        let root = tempfile::tempdir().expect("OpenCode toggle root");
        let configured = root.path().join("configured-opencode");
        std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
        let timeline = Arc::new(Mutex::new(Vec::new()));
        let helper = Arc::new(OpenCodeFixtureHelperLauncher {
            endpoint: Mutex::new(Some(
                fixture["endpoint"]
                    .as_str()
                    .expect("OpenCode endpoint fixture")
                    .to_owned(),
            )),
            timeline: timeline.clone(),
            ..OpenCodeFixtureHelperLauncher::default()
        });
        let remote_state = Arc::new(OpenCodeFixtureRemoteState {
            stream_scripts: Mutex::new(stream_scripts.into_iter().collect()),
            timeline: timeline.clone(),
            ..OpenCodeFixtureRemoteState::default()
        });
        let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
            calls: Mutex::new(Vec::new()),
            outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
                success: true,
                stdout: fixture["attachHelp"]
                    .as_str()
                    .expect("OpenCode attach help fixture")
                    .to_owned(),
                stderr: String::new(),
            }])),
            delay: std::time::Duration::ZERO,
        });
        let factory: Arc<dyn ProviderTerminalObserverFactory> =
            Arc::new(OpenCodeTerminalObserverFactory::new_with_reattach_timeout(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper.clone(),
                Arc::new(OpenCodeFixtureRemoteFactory {
                    state: remote_state.clone(),
                }),
                std::time::Duration::from_secs(1),
                reattach_timeout,
            ));
        let factory: Arc<dyn ProviderTerminalObserverFactory> = match activity_transition {
            Some(activity_transition) => Arc::new(ProbedOpenCodeObserverFactory {
                inner: factory,
                activity_transition,
            }),
            None => factory,
        };
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let mut settings = ProviderSettingsState::default();
        settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let supervisor = ProviderTerminalActivitySupervisor::new(
            settings.clone(),
            ProviderTerminalInventory::from_settings(&settings),
            projection.clone(),
            ProcessAttributionRegistry::new(),
            root.path().join("runtime"),
            ProviderTerminalObserverFactories {
                opencode: Some(factory),
                ..ProviderTerminalObserverFactories::default()
            },
        )
        .expect("OpenCode toggle supervisor");
        let backend = Arc::new(RecordingBackend::new(timeline));
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(supervisor)),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-opencode-toggle",
            "terminal-opencode-toggle",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.env = BTreeMap::from([(
            "OPENCODE_CONFIG_CONTENT".to_owned(),
            serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode fixture config"),
        )]);
        input.command = Some(TerminalLaunchCommand {
            executable: configured.to_string_lossy().into_owned(),
            args: vec![
                "--model".to_owned(),
                "anthropic/claude-sonnet-4-5".to_owned(),
            ],
            label: Some("OpenCode".to_owned()),
            activity: Some(ProviderTerminalActivityLaunch {
                driver_kind: "opencode".to_owned(),
                provider_instance_id: "opencode".to_owned(),
            }),
            env: None,
        });
        manager
            .open(input)
            .await
            .expect("observed OpenCode terminal");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let connected = remote_state
                    .calls
                    .lock()
                    .expect("OpenCode remote calls")
                    .iter()
                    .any(|call| call == "sse");
                if connected && remote_state.open_streams.load(Ordering::Acquire) == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("initial OpenCode connected stream");
        Self {
            _root: root,
            manager,
            projection,
            remote_state,
            helper,
            backend,
        }
    }

    async fn finish(self) {
        self.backend.latest().exit(0);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let reaped = self
                    .helper
                    .processes
                    .lock()
                    .expect("OpenCode helper processes")
                    .first()
                    .is_some_and(|process| process.reaped.load(Ordering::Acquire));
                if reaped
                    && self.remote_state.dropped.load(Ordering::Acquire)
                    && self.remote_state.open_streams.load(Ordering::Acquire) == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("OpenCode toggle cleanup");
        assert_eq!(
            self.helper
                .launches
                .lock()
                .expect("OpenCode helper launches")
                .len(),
            1,
            "toggles retain the one helper and attached PTY"
        );
        assert!(
            self.remote_state
                .calls
                .lock()
                .expect("OpenCode remote calls")
                .iter()
                .any(|call| call == "abort:root-tui-session")
        );
        assert_eq!(
            self.helper
                .processes
                .lock()
                .expect("OpenCode helper processes")
                .first()
                .expect("OpenCode helper process")
                .reap_calls
                .load(Ordering::Acquire),
            1,
            "the retained helper is reaped exactly once"
        );
        self.manager.shutdown().await;
    }
}

fn opencode_history_call_count(state: &OpenCodeFixtureRemoteState) -> usize {
    state
        .calls
        .lock()
        .expect("OpenCode remote calls")
        .iter()
        .filter(|call| {
            call.starts_with("children:")
                || call.as_str() == "statuses"
                || call.starts_with("messages:")
        })
        .count()
}

fn push_raw_dormant_frame(state: &OpenCodeFixtureRemoteState, frame: &[u8]) {
    let stream = state
        .active_streams
        .lock()
        .expect("active OpenCode streams")
        .first()
        .cloned()
        .expect("dormant OpenCode stream");
    stream
        .frames
        .lock()
        .expect("OpenCode stream frames")
        .push_back(Ok(frame.to_vec()));
    stream.notify.notify_one();
}

fn push_connected_frame_to_replacement(state: &OpenCodeFixtureRemoteState) {
    let stream = state
        .active_streams
        .lock()
        .expect("active OpenCode streams")
        .last()
        .cloned()
        .expect("replacement OpenCode stream");
    stream
        .frames
        .lock()
        .expect("OpenCode stream frames")
        .push_back(Ok(opencode_fixture_connected_frame()));
    stream.notify.notify_one();
}

async fn wait_for_opencode_discard_count(state: &OpenCodeFixtureRemoteState, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while state.discarded_frames.load(Ordering::Acquire) < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("OpenCode raw frame discard");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_drains_raw_dormant_stream() {
    let fixture = OpenCodeToggleFixture::open(vec![OpenCodeFixtureStreamScript::new([Ok(
        opencode_fixture_connected_frame(),
    )])])
    .await;
    let decoded_before = fixture
        .remote_state
        .decoded_activity_events
        .load(Ordering::Acquire);

    let disabled = fixture.manager.set_agent_activity_enabled(false).await;
    assert_eq!((disabled.stopped, disabled.dormant), (1, 1));
    push_raw_dormant_frame(&fixture.remote_state, b"data: not-json\n\n");
    wait_for_opencode_discard_count(&fixture.remote_state, 1).await;

    assert_eq!(
        fixture
            .remote_state
            .decoded_activity_events
            .load(Ordering::Acquire),
        decoded_before,
        "dormant frames are never decoded"
    );
    assert!(!fixture.backend.latest().killed.load(Ordering::Acquire));
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_handoff_requires_server_connected() {
    let fixture = OpenCodeToggleFixture::open(vec![
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([]),
    ])
    .await;
    let unary_before = opencode_history_call_count(&fixture.remote_state);
    fixture.manager.set_agent_activity_enabled(false).await;
    push_raw_dormant_frame(&fixture.remote_state, b"data: not-json\n\n");
    wait_for_opencode_discard_count(&fixture.remote_state, 1).await;

    let replacement_waiting = fixture.remote_state.replacement_waiting.notified();
    let enabling = tokio::spawn({
        let manager = fixture.manager.clone();
        async move { manager.set_agent_activity_enabled(true).await }
    });
    replacement_waiting.await;
    assert!(!enabling.is_finished(), "enable waits for server.connected");
    push_connected_frame_to_replacement(&fixture.remote_state);
    let enabled = enabling.await.expect("enable transition");

    assert_eq!((enabled.resumed, enabled.failed), (1, 0));
    assert_eq!(enabled.epochs.opencode, 1);
    assert_eq!(
        opencode_history_call_count(&fixture.remote_state),
        unary_before
    );
    assert_eq!(fixture.remote_state.open_streams.load(Ordering::Acquire), 1);
    assert!(
        fixture
            .remote_state
            .maximum_open_streams
            .load(Ordering::Acquire)
            <= 2
    );
    assert!(!fixture.backend.latest().killed.load(Ordering::Acquire));
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_busy_dormant_stream_does_not_starve_connected_replacement()
{
    let fixture = OpenCodeToggleFixture::open(vec![
        OpenCodeFixtureStreamScript::busy_dormant([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
    ])
    .await;
    fixture.manager.set_agent_activity_enabled(false).await;

    let enabled = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        fixture.manager.set_agent_activity_enabled(true),
    )
    .await
    .expect("busy dormant stream must not starve connected replacement");

    assert_eq!((enabled.resumed, enabled.failed), (1, 0));
    assert_eq!(fixture.remote_state.open_streams.load(Ordering::Acquire), 1);
    assert!(
        fixture
            .remote_state
            .maximum_open_streams
            .load(Ordering::Acquire)
            <= 2
    );
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_busy_dormant_stream_does_not_starve_handoff_deadline() {
    let fixture = OpenCodeToggleFixture::open_with_reattach_timeout(
        vec![
            OpenCodeFixtureStreamScript::busy_dormant([Ok(opencode_fixture_connected_frame())]),
            OpenCodeFixtureStreamScript::new([]),
        ],
        std::time::Duration::from_millis(50),
    )
    .await;
    fixture.manager.set_agent_activity_enabled(false).await;

    let failed = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        fixture.manager.set_agent_activity_enabled(true),
    )
    .await
    .expect("busy dormant stream must not starve handoff deadline");

    assert_eq!(
        (
            failed.resumed,
            failed.failed,
            failed.unavailable,
            fixture.remote_state.open_streams.load(Ordering::Acquire),
        ),
        (0, 1, 1, 1)
    );
    assert_eq!(
        fixture.remote_state.open_streams.load(Ordering::Acquire),
        1,
        "the timed-out replacement is dropped while the dormant stream remains"
    );
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_retries_latest_enabled_generation_after_supersession() {
    let (fixture, activity_transition) =
        OpenCodeToggleFixture::open_with_activity_transition_probe(vec![
            OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
            OpenCodeFixtureStreamScript::new([]),
            OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        ])
        .await;
    let mut initial_disabling = Box::pin(fixture.manager.set_agent_activity_enabled(false));
    let initially_disabled = if let Some(completed) = activity_transition
        .wait_for_first_poll_while(initial_disabling.as_mut())
        .await
    {
        completed
    } else {
        initial_disabling.as_mut().await
    };
    drop(initial_disabling);
    assert_eq!(
        (
            initially_disabled.stopped,
            initially_disabled.dormant,
            initially_disabled.failed,
        ),
        (1, 1, 0)
    );

    let pause = Arc::new(OpenCodeReplacementPause::default());
    *fixture
        .remote_state
        .replacement_pause
        .lock()
        .expect("OpenCode replacement pause") = Some(pause.clone());

    let mut first_enabling = Box::pin(fixture.manager.set_agent_activity_enabled(true));
    assert!(
        activity_transition
            .wait_for_first_poll_while(first_enabling.as_mut())
            .await
            .is_none(),
        "OpenCode enable completed before the paused replacement"
    );
    tokio::select! {
        biased;
        () = pause.entered.notified() => {}
        completed = &mut first_enabling => {
            panic!("OpenCode enable completed before the paused replacement: {completed:?}");
        }
    }

    let mut disabling = Box::pin(fixture.manager.set_agent_activity_enabled(false));
    assert!(
        activity_transition
            .wait_for_first_poll_while(disabling.as_mut())
            .await
            .is_none(),
        "OpenCode disable completed before superseding the paused replacement"
    );
    let mut latest_enabling = Box::pin(fixture.manager.set_agent_activity_enabled(true));
    assert!(
        activity_transition
            .wait_for_first_poll_while(latest_enabling.as_mut())
            .await
            .is_none(),
        "latest OpenCode enable completed before the paused replacement was released"
    );
    drop(first_enabling);
    drop(disabling);

    pause.release();
    let enabled = latest_enabling.await;

    assert_eq!((enabled.resumed, enabled.failed), (1, 0));
    assert_eq!(enabled.epochs.opencode, 1);
    assert_eq!(fixture.remote_state.open_streams.load(Ordering::Acquire), 1);
    assert_eq!(
        fixture
            .remote_state
            .opened_stream_count
            .load(Ordering::Acquire),
        3,
        "the superseded replacement is dropped before retrying the latest generation"
    );
    assert!(
        fixture
            .remote_state
            .maximum_open_streams
            .load(Ordering::Acquire)
            <= 2
    );
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_failed_handoff_stays_dormant() {
    let fixture = OpenCodeToggleFixture::open(vec![
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Err("injected replacement stream failure".to_owned())]),
    ])
    .await;
    let unary_before = opencode_history_call_count(&fixture.remote_state);
    fixture.manager.set_agent_activity_enabled(false).await;

    let failed = fixture.manager.set_agent_activity_enabled(true).await;
    assert_eq!(
        (failed.resumed, failed.failed, failed.unavailable),
        (0, 1, 1)
    );
    assert_eq!(
        fixture.remote_state.open_streams.load(Ordering::Acquire),
        1,
        "failed replacement is dropped while the dormant stream remains"
    );
    push_raw_dormant_frame(&fixture.remote_state, b"data: still-not-json\n\n");
    wait_for_opencode_discard_count(&fixture.remote_state, 1).await;
    assert_eq!(
        fixture
            .remote_state
            .decoded_activity_events
            .load(Ordering::Acquire),
        0
    );
    assert_eq!(
        opencode_history_call_count(&fixture.remote_state),
        unary_before
    );
    assert!(!fixture.backend.latest().killed.load(Ordering::Acquire));
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_activity_toggle_opencode_repeated_handoffs_return_to_one_stream() {
    let fixture = OpenCodeToggleFixture::open(vec![
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
    ])
    .await;
    let unary_before = opencode_history_call_count(&fixture.remote_state);

    for expected_epoch in 1..=3 {
        let disabled = fixture.manager.set_agent_activity_enabled(false).await;
        assert_eq!((disabled.stopped, disabled.dormant), (1, 1));
        let enabled = fixture.manager.set_agent_activity_enabled(true).await;
        assert_eq!((enabled.resumed, enabled.failed), (1, 0));
        assert_eq!(enabled.epochs.opencode, expected_epoch);
        assert_eq!(fixture.remote_state.open_streams.load(Ordering::Acquire), 1);
    }

    assert!(
        fixture
            .remote_state
            .maximum_open_streams
            .load(Ordering::Acquire)
            <= 2
    );
    assert_eq!(
        opencode_history_call_count(&fixture.remote_state),
        unary_before
    );
    assert!(!fixture.backend.latest().killed.load(Ordering::Acquire));
    fixture.finish().await;
}

#[cfg(unix)]
async fn begin_activity_controller_disables(
    controllers: &[AgentActivityController; 3],
) -> [tokio::task::JoinHandle<AgentActivityDisableReport>; 3] {
    let mut state_receivers = controllers
        .each_ref()
        .map(|controller| controller.subscribe());
    let disabling_activity = controllers.each_ref().map(|controller| {
        let controller = controller.clone();
        tokio::spawn(async move { controller.disable().await })
    });
    // begin_disable snapshots closed_subscriptions before publishing this watch change.
    // Waiting here creates the happens-before edge required before registrations are dropped.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        for state_receiver in &mut state_receivers {
            state_receiver
                .changed()
                .await
                .expect("activity controller state publisher");
            assert!(!state_receiver.borrow().enabled);
        }
    })
    .await
    .expect("all activity controllers publish their draining snapshot");
    disabling_activity
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_activity_toggle_across_providers_keeps_resources_bounded_and_releases_them() {
    let (
        _claude_root,
        claude_manager,
        claude_backend,
        claude_projection,
        _scope,
        _database,
        claude_factory,
    ) = claude_fixture_terminal("terminal-resource-claude").await;
    let (claude_endpoint, _token, _correlation, claude_settings_path) =
        claude_hook_launch(&claude_backend);
    let claude_client = reqwest::Client::new();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if claude_client
                .post(&claude_endpoint)
                .send()
                .await
                .is_ok_and(|response| response.status() == reqwest::StatusCode::FORBIDDEN)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one retained Claude listener");
    assert_eq!(
        claude_factory.listener_counts_for_integration_test(),
        (1, 1),
        "the one live Claude terminal owns exactly one listener"
    );

    let codex_fixture = codex_remote_fixture();
    let codex_root = tempfile::tempdir().expect("Codex resource root");
    let codex_configured = codex_root.path().join("configured-codex");
    std::fs::write(&codex_configured, b"configured").expect("configured Codex executable");
    let codex_helper = Arc::new(CodexFixtureHelperLauncher::default());
    let codex_remote_state = Arc::new(CodexFixtureRemoteState::default());
    codex_remote_state
        .events
        .lock()
        .expect("Codex events")
        .push_back(codex_root_notification(
            &codex_fixture,
            "terminal-resource-codex-root",
            codex_root.path(),
        ));
    let (codex_supervisor, codex_projection) = codex_fixture_supervisor(
        &codex_root,
        &codex_configured,
        codex_helper.clone(),
        Arc::new(CodexFixtureRemoteFactory {
            state: codex_remote_state.clone(),
        }),
    )
    .await;
    let codex_backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let codex_manager = TerminalManager::new(
        codex_backend,
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(codex_supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    codex_manager
        .open(codex_fixture_open_input(
            &codex_fixture,
            &codex_configured,
            &codex_root,
            "terminal-resource-codex",
        ))
        .await
        .expect("Codex resource terminal");
    let codex_scope = ActivityScopeRef::Terminal {
        thread_id: "thread-codex".to_owned(),
        terminal_id: "terminal-resource-codex".to_owned(),
    };
    wait_for_codex_initial_live(&codex_projection, &codex_scope).await;

    let opencode = OpenCodeToggleFixture::open(vec![
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
        OpenCodeFixtureStreamScript::new([Ok(opencode_fixture_connected_frame())]),
    ])
    .await;
    let activity_controllers = [
        claude_projection.agent_activity_controller_for_integration_test(),
        codex_projection.agent_activity_controller_for_integration_test(),
        opencode
            .projection
            .agent_activity_controller_for_integration_test(),
    ];
    let mut activity_registrations = Some(activity_controllers.each_ref().map(|controller| {
        controller
            .register_stream()
            .expect("enabled provider activity registration")
    }));
    assert_eq!(
        activity_controllers
            .each_ref()
            .map(|controller| controller.active_stream_count_for_integration_test()),
        [1, 1, 1],
        "one real activity controller registration is retained per enabled provider fixture"
    );
    let codex_history_before = count_codex_history_calls(&codex_remote_state);
    let opencode_history_before = opencode_history_call_count(&opencode.remote_state);

    assert_eq!(
        claude_manager.agent_activity_restart_descriptor_count_for_integration_test()
            + codex_manager.agent_activity_restart_descriptor_count_for_integration_test()
            + opencode
                .manager
                .agent_activity_restart_descriptor_count_for_integration_test(),
        3,
        "one restart descriptor is retained per live instrumented terminal"
    );
    assert_eq!(
        codex_helper
            .launches
            .lock()
            .expect("Codex helper launches")
            .len(),
        1
    );
    assert_eq!(
        opencode
            .helper
            .launches
            .lock()
            .expect("OpenCode helper launches")
            .len(),
        1
    );

    for expected_epoch in 1..=3 {
        let disabling_activity = begin_activity_controller_disables(&activity_controllers).await;
        assert_eq!(
            activity_controllers
                .each_ref()
                .map(|controller| controller.active_stream_count_for_integration_test()),
            [1, 1, 1],
            "disable observes every real registration before draining"
        );
        drop(
            activity_registrations
                .take()
                .expect("enabled activity registrations"),
        );
        let disabled_activity = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            futures_util::future::join_all(disabling_activity),
        )
        .await
        .expect("real activity registrations drain on disable");
        for report in disabled_activity {
            assert_eq!(
                report
                    .expect("activity controller disable")
                    .closed_subscriptions,
                1
            );
        }
        let claude_disabled = claude_manager.set_agent_activity_enabled(false).await;
        let codex_disabled = codex_manager.set_agent_activity_enabled(false).await;
        let opencode_disabled = opencode.manager.set_agent_activity_enabled(false).await;
        assert_eq!((claude_disabled.stopped, claude_disabled.dormant), (1, 1));
        assert_eq!((codex_disabled.stopped, codex_disabled.dormant), (1, 1));
        assert_eq!(
            (opencode_disabled.stopped, opencode_disabled.dormant),
            (1, 1)
        );
        assert_eq!(
            activity_controllers
                .each_ref()
                .map(|controller| controller.active_stream_count_for_integration_test()),
            [0, 0, 0],
            "all three real activity controller registrations return to zero while disabled"
        );
        assert_eq!(
            codex_remote_state
                .active_connections
                .load(Ordering::Acquire),
            1,
            "the healthy Codex connection remains retained"
        );
        assert_eq!(
            opencode.remote_state.open_streams.load(Ordering::Acquire),
            1,
            "the dormant OpenCode stream remains the sole steady-state stream"
        );
        assert!(claude_settings_path.exists());
        assert_eq!(
            claude_manager.agent_activity_restart_descriptor_count_for_integration_test()
                + codex_manager.agent_activity_restart_descriptor_count_for_integration_test()
                + opencode
                    .manager
                    .agent_activity_restart_descriptor_count_for_integration_test(),
            3
        );

        let claude_enabled = claude_manager.set_agent_activity_enabled(true).await;
        let codex_enabled = codex_manager.set_agent_activity_enabled(true).await;
        let opencode_enabled = opencode.manager.set_agent_activity_enabled(true).await;
        assert_eq!((claude_enabled.resumed, claude_enabled.failed), (1, 0));
        assert_eq!((codex_enabled.resumed, codex_enabled.failed), (1, 0));
        assert_eq!((opencode_enabled.resumed, opencode_enabled.failed), (1, 0));
        assert_eq!(opencode_enabled.epochs.opencode, expected_epoch);
        for controller in &activity_controllers {
            controller.enable();
        }
        activity_registrations = Some(activity_controllers.each_ref().map(|controller| {
            controller
                .register_stream()
                .expect("re-enabled provider activity registration")
        }));
        assert_eq!(
            activity_controllers
                .each_ref()
                .map(|controller| controller.active_stream_count_for_integration_test()),
            [1, 1, 1],
            "real activity registrations remain bounded after re-enable"
        );
        assert_eq!(
            codex_remote_state
                .active_connections
                .load(Ordering::Acquire),
            1
        );
        assert_eq!(
            opencode.remote_state.open_streams.load(Ordering::Acquire),
            1
        );
        assert_eq!(
            codex_helper
                .launches
                .lock()
                .expect("Codex helper launches")
                .len(),
            1
        );
        assert_eq!(
            opencode
                .helper
                .launches
                .lock()
                .expect("OpenCode helper launches")
                .len(),
            1
        );
        assert!(
            claude_client
                .post(&claude_endpoint)
                .send()
                .await
                .is_ok_and(|response| response.status() == reqwest::StatusCode::FORBIDDEN),
            "the same sole Claude listener remains reachable"
        );
        assert_eq!(
            claude_factory.listener_counts_for_integration_test(),
            (1, 1),
            "Claude listener count remains exactly one across activity transitions"
        );
    }

    assert_eq!(
        count_codex_history_calls(&codex_remote_state),
        codex_history_before
    );
    assert_eq!(
        opencode_history_call_count(&opencode.remote_state),
        opencode_history_before
    );
    assert_eq!(
        codex_remote_state
            .maximum_active_connections
            .load(Ordering::Acquire),
        1
    );
    assert!(
        opencode
            .remote_state
            .maximum_open_streams
            .load(Ordering::Acquire)
            <= 2
    );

    let disabling_activity = begin_activity_controller_disables(&activity_controllers).await;
    assert_eq!(
        activity_controllers
            .each_ref()
            .map(|controller| controller.active_stream_count_for_integration_test()),
        [1, 1, 1],
        "final disable observes every real registration before draining"
    );
    drop(
        activity_registrations
            .take()
            .expect("final enabled activity registrations"),
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        futures_util::future::join_all(disabling_activity),
    )
    .await
    .expect("final activity registrations drain before close")
    .into_iter()
    .for_each(|report| {
        assert_eq!(
            report
                .expect("final activity controller disable")
                .closed_subscriptions,
            1
        );
    });
    assert_eq!(
        activity_controllers
            .each_ref()
            .map(|controller| controller.active_stream_count_for_integration_test()),
        [0, 0, 0],
    );

    claude_manager
        .close("thread-claude", Some("terminal-resource-claude"))
        .await
        .expect("close Claude resource terminal");
    codex_manager
        .close("thread-codex", Some("terminal-resource-codex"))
        .await
        .expect("close Codex resource terminal");
    opencode
        .manager
        .close("thread-opencode-toggle", Some("terminal-opencode-toggle"))
        .await
        .expect("close OpenCode resource terminal");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let codex_released = codex_remote_state
                .active_connections
                .load(Ordering::Acquire)
                == 0
                && codex_helper
                    .processes
                    .lock()
                    .expect("Codex helper processes")
                    .first()
                    .is_some_and(|process| process.terminated.load(Ordering::Acquire));
            let opencode_released = opencode
                .helper
                .processes
                .lock()
                .expect("OpenCode helper processes")
                .first()
                .is_some_and(|process| process.reaped.load(Ordering::Acquire))
                && opencode.remote_state.dropped.load(Ordering::Acquire)
                && opencode.remote_state.open_streams.load(Ordering::Acquire) == 0;
            if codex_released
                && opencode_released
                && !claude_settings_path.exists()
                && claude_factory.listener_counts_for_integration_test().0 == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider transports and descriptors release on close");
    assert_eq!(
        claude_manager.agent_activity_restart_descriptor_count_for_integration_test()
            + codex_manager.agent_activity_restart_descriptor_count_for_integration_test()
            + opencode
                .manager
                .agent_activity_restart_descriptor_count_for_integration_test(),
        0
    );
    assert_eq!(
        activity_controllers
            .each_ref()
            .map(|controller| controller.active_stream_count_for_integration_test()),
        [0, 0, 0],
        "activity registrations remain drained after every provider closes"
    );
    assert!(
        claude_client.post(&claude_endpoint).send().await.is_err(),
        "closing the Claude terminal releases its sole listener"
    );
    assert_eq!(
        claude_factory.listener_counts_for_integration_test(),
        (0, 1),
        "Claude listener lifecycle releases the only bind without overlap"
    );
    assert_eq!(
        opencode
            .helper
            .processes
            .lock()
            .expect("OpenCode helper processes")
            .first()
            .expect("OpenCode helper process")
            .reap_calls
            .load(Ordering::Acquire),
        1
    );

    claude_manager.shutdown().await;
    codex_manager.shutdown().await;
    opencode.manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_parallel_terminals_have_unique_endpoints_credentials_roots_and_scopes() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode parallel root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoints: Mutex::new(VecDeque::from([
            "http://127.0.0.1:41001".to_owned(),
            "http://127.0.0.1:41002".to_owned(),
        ])),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let remote_state = Arc::new(OpenCodeFixtureRemoteState {
        root_session_ids: Mutex::new(VecDeque::from([
            "root-parallel-a".to_owned(),
            "root-parallel-b".to_owned(),
        ])),
        ..OpenCodeFixtureRemoteState::default()
    });
    let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: fixture["attachHelp"]
                .as_str()
                .expect("OpenCode attach help")
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::new(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper,
                Arc::new(OpenCodeFixtureRemoteFactory {
                    state: remote_state,
                }),
                std::time::Duration::from_secs(1),
            ))),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode parallel supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    for terminal_id in ["terminal-opencode-a", "terminal-opencode-b"] {
        let mut input = TerminalOpenInput::new(
            "thread-opencode-parallel",
            terminal_id,
            root.path().to_path_buf(),
            80,
            24,
        );
        input.command = Some(TerminalLaunchCommand {
            executable: configured.to_string_lossy().into_owned(),
            args: vec!["--model".to_owned(), "openai/gpt-5.2".to_owned()],
            label: Some("OpenCode".to_owned()),
            activity: Some(ProviderTerminalActivityLaunch {
                driver_kind: "opencode".to_owned(),
                provider_instance_id: "opencode".to_owned(),
            }),
            env: None,
        });
        manager.open(input).await.expect("parallel OpenCode attach");
    }

    let spawns = backend.spawns();
    assert_eq!(spawns.len(), 2);
    assert_ne!(spawns[0].args[1], spawns[1].args[1], "unique endpoint");
    assert_ne!(spawns[0].args[5], spawns[1].args[5], "unique owned root");
    assert_ne!(
        spawns[0].env.get("OPENCODE_SERVER_PASSWORD"),
        spawns[1].env.get("OPENCODE_SERVER_PASSWORD"),
        "unique private credential"
    );
    for spawn in &spawns {
        let password = spawn
            .env
            .get("OPENCODE_SERVER_PASSWORD")
            .expect("OpenCode private credential");
        assert!(
            !spawn
                .args
                .iter()
                .any(|argument| argument.contains(password))
        );
    }
    for terminal_id in ["terminal-opencode-a", "terminal-opencode-b"] {
        let scope = ActivityScopeRef::Terminal {
            thread_id: "thread-opencode-parallel".to_owned(),
            terminal_id: terminal_id.to_owned(),
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if projection
                    .snapshot(&scope)
                    .await
                    .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("independent OpenCode activity scope");
    }
    manager
        .close("thread-opencode-parallel", Some("terminal-opencode-a"))
        .await
        .expect("close terminal");
    manager
        .close("thread-opencode-parallel", Some("terminal-opencode-b"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

/// OpenCode 2 has no `attach`: its `attach --help` prints the root help, and the observed
/// terminal opens the owned root with `--server`/`--session`, the password in `OPENCODE_PASSWORD`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_2_terminals_open_the_owned_root_with_server_flags() {
    let root = tempfile::tempdir().expect("OpenCode 2 root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let helper = Arc::new(OpenCodeFixtureHelperLauncher {
        endpoints: Mutex::new(VecDeque::from(["http://127.0.0.1:41003".to_owned()])),
        ..OpenCodeFixtureHelperLauncher::default()
    });
    let helper_launches = helper.clone();
    let remote_state = Arc::new(OpenCodeFixtureRemoteState {
        root_session_ids: Mutex::new(VecDeque::from(["ses_root".to_owned()])),
        ..OpenCodeFixtureRemoteState::default()
    });
    let probe_runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: "USAGE\n  opencode <subcommand> [flags] [<directory>]\n\nFLAGS\n  \
                     --server string         Connect to a server URL\n  \
                     --session, -s string    Session ID to continue\n"
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection,
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::new(
                Arc::new(CachedOpenCodeCapabilityProbe::new(probe_runner)),
                helper,
                Arc::new(OpenCodeFixtureRemoteFactory {
                    state: remote_state,
                }),
                std::time::Duration::from_secs(1),
            ))),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode 2 supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-opencode-2",
        "terminal-opencode-2",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "mock/mock-model".to_owned()],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });
    manager
        .open(input)
        .await
        .expect("observed OpenCode 2 terminal");

    let spawns = backend.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(
        spawns[0].args,
        [
            "--server".to_owned(),
            "http://127.0.0.1:41003".to_owned(),
            "--session".to_owned(),
            "ses_root".to_owned(),
            root.path().to_string_lossy().into_owned(),
        ]
    );
    let password = spawns[0]
        .env
        .get("OPENCODE_PASSWORD")
        .expect("OpenCode 2 client credential");
    assert!(
        !spawns[0]
            .args
            .iter()
            .any(|argument| argument.contains(password))
    );
    // OpenCode 2 prefers `OPENCODE_PASSWORD` to `OPENCODE_SERVER_PASSWORD`, so the helper gets
    // both: one inherited from BiBCode's environment cannot override its own.
    {
        let launches = helper_launches
            .launches
            .lock()
            .expect("helper launches lock");
        assert_eq!(launches[0].env.get("OPENCODE_PASSWORD"), Some(password));
        assert_eq!(
            launches[0].env.get("OPENCODE_SERVER_PASSWORD"),
            Some(password)
        );
    }
    manager
        .close("thread-opencode-2", Some("terminal-opencode-2"))
        .await
        .expect("close terminal");
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_helper_already_exited_before_prepared_observer_is_rejected() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode early-helper-exit root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let helper = Arc::new(ActualExitedOpenCodeHelperLauncher {
        endpoint: fixture["endpoint"]
            .as_str()
            .expect("OpenCode endpoint fixture")
            .to_owned(),
        process: Mutex::new(None),
    });
    let runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: fixture["attachHelp"]
                .as_str()
                .expect("OpenCode attach help fixture")
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let remote_state = Arc::new(OpenCodeFixtureRemoteState {
        exit_helper_on_create_root: Mutex::new(Some(helper.clone())),
        ..OpenCodeFixtureRemoteState::default()
    });
    let factory = Arc::new(OpenCodeTerminalObserverFactory::new(
        Arc::new(CachedOpenCodeCapabilityProbe::new(runner)),
        helper.clone(),
        Arc::new(OpenCodeFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
        std::time::Duration::from_millis(50),
    ));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        ActivityProjection::new(ActivityRepository::new(database)),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode early-helper-exit supervisor");
    let preparation = supervisor
        .prepare(TerminalLaunchPreparationInput {
            executable: configured.to_string_lossy().into_owned(),
            args: vec![
                "--model".to_owned(),
                "anthropic/claude-sonnet-4-5".to_owned(),
            ],
            cwd: root.path().to_path_buf(),
            worktree_path: Some(root.path().to_path_buf()),
            launch_env: BTreeMap::from([(
                "OPENCODE_CONFIG_CONTENT".to_owned(),
                serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode fixture config"),
            )]),
            activity: ProviderTerminalActivityLaunch {
                driver_kind: "opencode".to_owned(),
                provider_instance_id: "opencode".to_owned(),
            },
            generation: TerminalObserverGeneration::new(
                "thread-opencode-early-helper-exit".to_owned(),
                "terminal-opencode-early-helper-exit".to_owned(),
            ),
        })
        .await;

    assert!(
        matches!(preparation, TerminalLaunchPreparation::PassThrough),
        "an actual helper already exited before factory return must not produce a prepared observer"
    );
    assert!(
        helper
            .process
            .lock()
            .expect("actual OpenCode helper process")
            .as_ref()
            .is_some_and(|process| process.cleanup_calls.load(Ordering::Acquire) == 1),
        "factory rejection cleans the already-reaped helper resources exactly once"
    );
    assert!(
        remote_state
            .calls
            .lock()
            .expect("OpenCode remote calls")
            .iter()
            .any(|call| call == "delete:root-tui-session"),
        "factory rejection deletes the pre-spawn root"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_helper_exit_in_manager_gap_falls_back_to_original_pty() {
    let fixture = opencode_attach_fixture();
    let root = tempfile::tempdir().expect("OpenCode helper-exit root");
    let configured = root.path().join("configured-opencode");
    std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
    let helper = Arc::new(ActualExitedOpenCodeHelperLauncher {
        endpoint: fixture["endpoint"]
            .as_str()
            .expect("OpenCode endpoint fixture")
            .to_owned(),
        process: Mutex::new(None),
    });
    let runner = Arc::new(OpenCodeProbeFixtureRunner {
        calls: Mutex::new(Vec::new()),
        outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
            success: true,
            stdout: fixture["attachHelp"]
                .as_str()
                .expect("OpenCode attach help fixture")
                .to_owned(),
            stderr: String::new(),
        }])),
        delay: std::time::Duration::ZERO,
    });
    let remote_state = Arc::new(OpenCodeFixtureRemoteState::default());
    let factory = Arc::new(OpenCodeTerminalObserverFactory::new(
        Arc::new(CachedOpenCodeCapabilityProbe::new(runner)),
        helper.clone(),
        Arc::new(OpenCodeFixtureRemoteFactory {
            state: remote_state.clone(),
        }),
        std::time::Duration::from_millis(50),
    ));
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(factory),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode helper-exit supervisor");
    let backend = Arc::new(ExitHelperOnFirstSpawnBackend {
        helper: helper.clone(),
        spawns: Mutex::new(Vec::new()),
        processes: Mutex::new(Vec::new()),
    });
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let original_args = vec![
        "--model".to_owned(),
        "anthropic/claude-sonnet-4-5".to_owned(),
    ];
    let mut input = TerminalOpenInput::new(
        "thread-opencode-helper-exit",
        "terminal-opencode-helper-exit",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.env = BTreeMap::from([(
        "OPENCODE_CONFIG_CONTENT".to_owned(),
        serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode fixture config"),
    )]);
    input.command = Some(TerminalLaunchCommand {
        executable: configured.to_string_lossy().into_owned(),
        args: original_args.clone(),
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });

    manager
        .open(input)
        .await
        .expect("original OpenCode PTY remains usable");

    let spawns = backend.spawns();
    assert_eq!(
        spawns.len(),
        2,
        "the dead helper's prepared attach is discarded and the original PTY is spawned"
    );
    assert_eq!(spawns[1].args, original_args);
    assert_eq!(spawns[1].executable, configured.to_string_lossy());
    assert!(
        backend
            .processes
            .lock()
            .expect("gap processes")
            .first()
            .is_some_and(|process| process.killed.load(Ordering::Acquire)),
        "the uncommitted prepared attach is killed before pass-through"
    );
    assert!(
        helper
            .process
            .lock()
            .expect("actual OpenCode helper process")
            .as_ref()
            .is_some_and(|process| process.cleanup_calls.load(Ordering::Acquire) == 1),
        "owned helper cleanup completes exactly once before pass-through"
    );
    assert!(
        projection
            .snapshot(&ActivityScopeRef::Terminal {
                thread_id: "thread-opencode-helper-exit".to_owned(),
                terminal_id: "terminal-opencode-helper-exit".to_owned(),
            })
            .await
            .is_err(),
        "a helper that exits before on_spawned publishes no dock"
    );
    assert!(
        remote_state
            .calls
            .lock()
            .expect("OpenCode remote calls")
            .iter()
            .any(|call| call == "delete:root-tui-session"),
        "pre-spawn root cleanup runs before pass-through"
    );
    manager.shutdown().await;
}

/// OpenCode 2 has no `--model`, so the requested `opencode --model <model>` would not start. The
/// server runs it as OpenCode 2 accepts: a private server (`--standalone`) whose configuration
/// selects the model, also when observation is not prepared. OpenCode 1 keeps `--model`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_terminal_arguments_follow_the_installed_major_version() {
    let fixture = opencode_attach_fixture();
    let opencode_2_help = "USAGE\n  opencode <subcommand> [flags] [<directory>]\n\nFLAGS\n  \
                           --standalone            Run with a private server\n  \
                           --server string         Connect to a server URL\n  \
                           --session, -s string    Session ID to continue\n";
    for (help, probe_delay, expected_args, expected_config) in [
        (
            opencode_2_help.to_owned(),
            std::time::Duration::ZERO,
            vec!["--standalone".to_owned()],
            serde_json::json!({ "theme": "system", "model": "mock/mock-model" }),
        ),
        // A cold capability probe may take most of its 900 ms budget; the native form still
        // applies.
        (
            opencode_2_help.to_owned(),
            std::time::Duration::from_millis(700),
            vec!["--standalone".to_owned()],
            serde_json::json!({ "theme": "system", "model": "mock/mock-model" }),
        ),
        (
            fixture["attachHelp"]
                .as_str()
                .expect("OpenCode 1 attach help")
                .to_owned(),
            std::time::Duration::ZERO,
            vec!["--model".to_owned(), "mock/mock-model".to_owned()],
            serde_json::json!({ "theme": "system" }),
        ),
    ] {
        let root = tempfile::tempdir().expect("OpenCode arguments root");
        let configured = root.path().join("configured-opencode");
        std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
        // The helper fails, so the terminal starts unobserved.
        let helper = Arc::new(OpenCodeFixtureHelperLauncher {
            fail: AtomicBool::new(true),
            ..OpenCodeFixtureHelperLauncher::default()
        });
        let runner = Arc::new(OpenCodeProbeFixtureRunner {
            calls: Mutex::new(Vec::new()),
            outputs: Mutex::new(VecDeque::from([OpenCodeProbeOutput {
                success: true,
                stdout: help,
                stderr: String::new(),
            }])),
            delay: probe_delay,
        });
        let factory = Arc::new(OpenCodeTerminalObserverFactory::new(
            Arc::new(CachedOpenCodeCapabilityProbe::new(runner)),
            helper,
            Arc::new(OpenCodeFixtureRemoteFactory {
                state: Arc::new(OpenCodeFixtureRemoteState::default()),
            }),
            std::time::Duration::from_millis(50),
        ));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let mut settings = ProviderSettingsState::default();
        settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
        let supervisor = ProviderTerminalActivitySupervisor::new(
            settings.clone(),
            ProviderTerminalInventory::from_settings(&settings),
            ActivityProjection::new(ActivityRepository::new(database)),
            ProcessAttributionRegistry::new(),
            root.path().join("runtime"),
            ProviderTerminalObserverFactories {
                opencode: Some(factory),
                ..ProviderTerminalObserverFactories::default()
            },
        )
        .expect("OpenCode arguments supervisor");
        let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(supervisor)),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-opencode-arguments",
            "terminal-opencode-arguments",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.env = BTreeMap::from([(
            "OPENCODE_CONFIG_CONTENT".to_owned(),
            "{\"theme\":\"system\"}".to_owned(),
        )]);
        input.command = Some(TerminalLaunchCommand {
            executable: configured.to_string_lossy().into_owned(),
            args: vec!["--model".to_owned(), "mock/mock-model".to_owned()],
            label: Some("OpenCode".to_owned()),
            activity: Some(ProviderTerminalActivityLaunch {
                driver_kind: "opencode".to_owned(),
                provider_instance_id: "opencode".to_owned(),
            }),
            env: Some(BTreeMap::from([(
                "OPENCODE_CONFIG_CONTENT".to_owned(),
                "{\"theme\":\"system\"}".to_owned(),
            )])),
        });
        manager
            .open(input)
            .await
            .expect("unobserved OpenCode terminal");
        let spawn = backend.spawns().pop().expect("OpenCode spawn");
        assert_eq!(spawn.args, expected_args);
        // The session keeps the requested command, so a relaunch derives the native form again,
        // and the published descriptor stays the client's.
        let summaries = manager.subscribe_metadata().await.initial;
        let command = summaries[0].command.clone().expect("saved command");
        assert_eq!(command.args, ["--model", "mock/mock-model"]);
        assert_eq!(
            command.env.expect("saved env")["OPENCODE_CONFIG_CONTENT"],
            "{\"theme\":\"system\"}"
        );
        let config: serde_json::Value = serde_json::from_str(
            spawn
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .expect("OpenCode config"),
        )
        .expect("OpenCode config JSON");
        assert_eq!(config, expected_config);
        manager
            .close(
                "thread-opencode-arguments",
                Some("terminal-opencode-arguments"),
            )
            .await
            .expect("close terminal");
        manager.shutdown().await;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opencode_helper_failure_and_unsafe_args_are_exact_pass_through() {
    let fixture = opencode_attach_fixture();
    for (args, config, helper_fail, connect_fail) in [
        (
            vec![
                "--model".to_owned(),
                "anthropic/claude-sonnet-4-5".to_owned(),
            ],
            serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode fixture config"),
            true,
            false,
        ),
        (
            vec![
                "--model".to_owned(),
                "anthropic/claude-sonnet-4-5".to_owned(),
            ],
            serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode fixture config"),
            false,
            true,
        ),
        (
            vec![
                "--model".to_owned(),
                "anthropic/claude-sonnet-4-5".to_owned(),
                "--unknown".to_owned(),
            ],
            serde_json::to_string(&fixture["originalConfig"]).expect("OpenCode fixture config"),
            false,
            false,
        ),
        (
            vec![
                "--model".to_owned(),
                "anthropic/claude-sonnet-4-5".to_owned(),
            ],
            "[]".to_owned(),
            false,
            false,
        ),
    ] {
        let root = tempfile::tempdir().expect("OpenCode pass-through root");
        let configured = root.path().join("configured-opencode");
        std::fs::write(&configured, b"configured").expect("configured OpenCode executable");
        let helper = Arc::new(OpenCodeFixtureHelperLauncher {
            endpoint: Mutex::new(Some(
                fixture["endpoint"]
                    .as_str()
                    .expect("OpenCode endpoint fixture")
                    .to_owned(),
            )),
            fail: AtomicBool::new(helper_fail),
            ..OpenCodeFixtureHelperLauncher::default()
        });
        let runner = Arc::new(OpenCodeProbeFixtureRunner {
            calls: Mutex::new(Vec::new()),
            outputs: Mutex::new(
                ["attachHelp"]
                    .into_iter()
                    .map(|field| OpenCodeProbeOutput {
                        success: true,
                        stdout: fixture[field]
                            .as_str()
                            .expect("OpenCode probe fixture")
                            .to_owned(),
                        stderr: String::new(),
                    })
                    .collect(),
            ),
            delay: std::time::Duration::ZERO,
        });
        let remote = Arc::new(OpenCodeFixtureRemoteFactory {
            state: Arc::new(OpenCodeFixtureRemoteState {
                connect_fail: AtomicBool::new(connect_fail),
                ..OpenCodeFixtureRemoteState::default()
            }),
        });
        let factory = Arc::new(OpenCodeTerminalObserverFactory::new(
            Arc::new(CachedOpenCodeCapabilityProbe::new(runner)),
            helper.clone(),
            remote,
            std::time::Duration::from_millis(50),
        ));
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let mut settings = ProviderSettingsState::default();
        settings.providers.opencode.binary_path = configured.to_string_lossy().into_owned();
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let supervisor = ProviderTerminalActivitySupervisor::new(
            settings.clone(),
            ProviderTerminalInventory::from_settings(&settings),
            projection.clone(),
            ProcessAttributionRegistry::new(),
            root.path().join("runtime"),
            ProviderTerminalObserverFactories {
                opencode: Some(factory),
                ..ProviderTerminalObserverFactories::default()
            },
        )
        .expect("OpenCode pass-through supervisor");
        let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
        let manager = TerminalManager::new(
            backend.clone(),
            TerminalManagerOptions {
                launch_preparer: Some(Arc::new(supervisor)),
                ..TerminalManagerOptions::default()
            },
        );
        let mut input = TerminalOpenInput::new(
            "thread-opencode-pass",
            "terminal-opencode-pass",
            root.path().to_path_buf(),
            80,
            24,
        );
        input.env = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".to_owned(), config.clone())]);
        input.command = Some(TerminalLaunchCommand {
            executable: configured.to_string_lossy().into_owned(),
            args: args.clone(),
            label: Some("OpenCode".to_owned()),
            activity: Some(ProviderTerminalActivityLaunch {
                driver_kind: "opencode".to_owned(),
                provider_instance_id: "opencode".to_owned(),
            }),
            env: None,
        });
        manager
            .open(input)
            .await
            .expect("pass-through OpenCode terminal");
        let spawn = backend.spawns().pop().expect("pass-through spawn");
        assert_eq!(spawn.executable, configured.to_string_lossy());
        assert_eq!(spawn.args, args);
        assert_eq!(spawn.env.get("OPENCODE_CONFIG_CONTENT"), Some(&config));
        assert!(!spawn.env.contains_key("OPENCODE_SERVER_PASSWORD"));
        assert!(
            projection
                .snapshot(&ActivityScopeRef::Terminal {
                    thread_id: "thread-opencode-pass".to_owned(),
                    terminal_id: "terminal-opencode-pass".to_owned(),
                })
                .await
                .is_err(),
            "helper exit or unsafe observer setup must leave the usable pass-through terminal without a dock"
        );
        if connect_fail {
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if helper
                        .processes
                        .lock()
                        .expect("OpenCode helper processes")
                        .first()
                        .is_some_and(|process| process.reaped.load(Ordering::Acquire))
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("helper that exits before PTY is reaped while PTY stays usable");
        }
        manager.shutdown().await;
    }
}

/// Observes a terminal of a real OpenCode 2 binary through the production factory: the
/// capability probe, the authenticated helper, the `/api` client and root correlation. The PTY
/// itself is recorded, not run. Set `BIBCODE_OPENCODE_V2_BINARY` (a copy under the temporary
/// directory, which the hermetic test guard allows), `BIBCODE_OPENCODE_V2_HOME` (a scratch home
/// whose configuration points the default model at a scripted local endpoint) and
/// `BIBCODE_OPENCODE_V2_MODEL`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a live OpenCode 2 binary"]
async fn live_opencode_2_terminal_observation() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let binary = var("BIBCODE_OPENCODE_V2_BINARY");
    let home = std::path::PathBuf::from(var("BIBCODE_OPENCODE_V2_HOME"));
    let root = tempfile::tempdir().expect("OpenCode 2 terminal root");
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = binary.clone();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::system())),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("OpenCode 2 supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-live-opencode-2",
        "terminal-live-opencode-2",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.env = [
        ("HOME", home.clone()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_STATE_HOME", home.join(".local/state")),
        ("XDG_CACHE_HOME", home.join(".cache")),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_string_lossy().into_owned()))
    .collect();
    input.command = Some(TerminalLaunchCommand {
        executable: binary,
        args: vec!["--model".to_owned(), var("BIBCODE_OPENCODE_V2_MODEL")],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });
    let started = std::time::Instant::now();
    manager.open(input).await.expect("OpenCode 2 terminal");
    let elapsed = started.elapsed();

    let spawn = backend.spawns().pop().expect("OpenCode 2 PTY spawn");
    assert_eq!(
        spawn.args.first().map(String::as_str),
        Some("--server"),
        "observed launch (preparation took {elapsed:?})"
    );
    assert_eq!(spawn.args.get(2).map(String::as_str), Some("--session"));
    assert!(spawn.env.contains_key("OPENCODE_PASSWORD"));
    let endpoint = spawn.args[1].clone();
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-live-opencode-2".to_owned(),
        terminal_id: "terminal-live-opencode-2".to_owned(),
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if projection
                .snapshot(&scope)
                .await
                .is_ok_and(|snapshot| snapshot.capabilities.terminal_observation)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("OpenCode 2 root correlation");
    manager
        .close("thread-live-opencode-2", Some("terminal-live-opencode-2"))
        .await
        .expect("close terminal");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if tokio::net::TcpStream::connect(
                endpoint
                    .strip_prefix("http://")
                    .expect("loopback OpenCode endpoint"),
            )
            .await
            .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned OpenCode 2 helper reaped on terminal close");
    manager.shutdown().await;
}

#[cfg(all(unix, target_os = "macos"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_opencode_1184_cold_topology_reaps_owned_listener() {
    let Some(path) = std::env::var_os("PATH") else {
        return;
    };
    let Some(installed) = std::env::split_paths(&path)
        .map(|directory| directory.join("opencode"))
        .find(|candidate| candidate.is_file())
    else {
        return;
    };
    let Ok(version) = std::process::Command::new(&installed)
        .arg("--version")
        .output()
    else {
        return;
    };
    if !version.status.success() || String::from_utf8_lossy(&version.stdout).trim() != "1.18.4" {
        return;
    }
    let root = tempfile::tempdir().expect("installed OpenCode root");
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let mut settings = ProviderSettingsState::default();
    settings.providers.opencode.binary_path = installed.to_string_lossy().into_owned();
    let supervisor = ProviderTerminalActivitySupervisor::new(
        settings.clone(),
        ProviderTerminalInventory::from_settings(&settings),
        projection.clone(),
        ProcessAttributionRegistry::new(),
        root.path().join("runtime"),
        ProviderTerminalObserverFactories {
            opencode: Some(Arc::new(OpenCodeTerminalObserverFactory::system())),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("installed OpenCode supervisor");
    let backend = Arc::new(RecordingBackend::new(Arc::new(Mutex::new(Vec::new()))));
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(supervisor)),
            ..TerminalManagerOptions::default()
        },
    );
    let mut input = TerminalOpenInput::new(
        "thread-installed-opencode",
        "terminal-installed-opencode",
        root.path().to_path_buf(),
        80,
        24,
    );
    input.env = BTreeMap::from([(
        "OPENCODE_CONFIG_CONTENT".to_owned(),
        "{\"theme\":\"system\"}".to_owned(),
    )]);
    input.command = Some(TerminalLaunchCommand {
        executable: installed.to_string_lossy().into_owned(),
        args: vec!["--model".to_owned(), "openai/gpt-5.2".to_owned()],
        label: Some("OpenCode".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "opencode".to_owned(),
            provider_instance_id: "opencode".to_owned(),
        }),
        env: None,
    });
    let started = std::time::Instant::now();

    manager
        .open(input)
        .await
        .expect("installed OpenCode terminal");
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(850),
        "installed OpenCode preparation took {elapsed:?}"
    );

    let spawn = backend
        .spawns()
        .pop()
        .expect("installed OpenCode PTY spawn");
    assert_eq!(spawn.args.first().map(String::as_str), Some("attach"));
    assert_eq!(spawn.args.get(2).map(String::as_str), Some("--dir"));
    assert_eq!(spawn.args.get(4).map(String::as_str), Some("--session"));
    let endpoint = spawn.args[1].clone();
    let root_session_id = spawn.args[5].clone();
    assert!(endpoint.starts_with("http://127.0.0.1:"));
    assert!(!root_session_id.is_empty());
    let scope = ActivityScopeRef::Terminal {
        thread_id: "thread-installed-opencode".to_owned(),
        terminal_id: "terminal-installed-opencode".to_owned(),
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if projection.snapshot(&scope).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("installed OpenCode correlation");
    manager
        .close(
            "thread-installed-opencode",
            Some("terminal-installed-opencode"),
        )
        .await
        .expect("close terminal");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if tokio::net::TcpStream::connect(
                endpoint
                    .strip_prefix("http://")
                    .expect("loopback OpenCode endpoint"),
            )
            .await
            .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned OpenCode listener reaped on terminal close");
    manager.shutdown().await;
}
