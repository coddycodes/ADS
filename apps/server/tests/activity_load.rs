use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use coddy_studio_server::{
    RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    activity::{
        ActivityCapabilities, ActivityEntry, ActivityEntryKind, ActivityEntryTone,
        ActivityLifecycle, ActivityProjection, ActivityProjections, ActivityRecordKind,
        ActivityRepository, ActivityRosterBucket, ActivityScopeSeed, ActivitySection,
        ActivityWorkItemSummary, AgentActivityController, AgentActivitySource,
        ProviderActivityMutation, register_activity_rpc_for_integration_test,
    },
    diagnostics::{ProcessAttributionRegistry, ProcessIdentity, TraceDiagnosticsStore},
    orchestration::engine::{EngineOptions, OrchestrationEngine},
    persistence::{Database, run_migrations},
    production::{
        agent_activity::ProductionAgentActivity,
        provider_runtime::{
            BoxRuntimeFuture, ProviderDriver, ProviderDriverFactory, ProviderLaunchRequest,
            ProviderRuntimeError, ProviderRuntimeSupervisor, SupervisorOptions,
        },
    },
    provider_terminal::{
        PreparedTerminalLaunch, PreparedTerminalObserver, ProviderTerminalActivitySupervisor,
        ProviderTerminalInventory, ProviderTerminalObserverFactories,
        ProviderTerminalObserverFactory, ProviderTerminalObserverFactoryInput,
        TerminalAgentActivityTransition, TerminalObserverGenerationLease,
        TerminalObserverWorkerContext,
    },
    server_settings::ProviderSettingsState,
    terminal::{
        ProviderTerminalActivityLaunch, PtyBackend, PtyExit, PtyProcess, PtySpawnInput,
        TerminalLaunchCommand, TerminalManager, TerminalManagerOptions, TerminalOpenInput,
    },
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tempfile::TempDir;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{Barrier, oneshot},
    task::JoinSet,
    time::{sleep, timeout},
};
use tokio_tungstenite::{WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const ACTOR_COUNT: usize = 50;
const EVENTS_PER_ACTOR: usize = 100;
const MUTATION_BATCH_LIMIT: usize = 256;
const PAGE_LIMIT: usize = 200;
const JOURNAL_ROW_LIMIT: i64 = 5_000;
const QUEUED_WRITER_COUNT: usize = 80;
const ROSTER_RECORDS_PER_BUCKET: usize = PAGE_LIMIT + 25;
const DATABASE_QUEUE_CAPACITY: usize = 64;
const PROCESS_RSS_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const PROCESS_RSS_DELTA_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
const DISABLED_EVENT_COUNT: usize = 10_000;
const PREVIOUSLY_INSTRUMENTED_TERMINALS: usize = 2;
const DISABLED_TERMINAL_LAUNCHES: usize = 8;
const SECRET_DISABLED_PAYLOAD: &str = "disabled-load-secret-content";

#[test]
fn cancellation_control_mutation_surface_is_not_publicly_exported() {
    // Mutation caught: letting external crates register scopes or inject activity into the overlay.
    let activity_module = include_str!("../src/activity/mod.rs");
    let cancellation_module = include_str!("../src/activity/cancellation.rs");
    let control_module = include_str!("../src/activity/control.rs");

    assert!(!activity_module.contains("pub use control::"));
    for signature in [
        "pub struct ActivityControlRegistry",
        "pub struct ActivityRuntimeControlRegistration",
        "pub enum ActivityControlEvent",
        "    pub fn register_runtime(",
        "    pub fn observe_provider_batch(",
        "    pub fn observe_activity_batch(",
        "pub trait ActivityCancellationDispatcher",
        "pub struct ActivityCancellationService",
    ] {
        assert!(
            !control_module.contains(signature) && !cancellation_module.contains(signature),
            "cancellation mutation surface leaked publicly: {signature}"
        );
    }
}

#[test]
fn targeted_activity_bridge_exposes_no_production_test_bypass() {
    // Mutation caught: making a generation, native target constructor, dispatcher error, or
    // supervisor test hook callable by an external production-library consumer.
    let activity_module = include_str!("../src/activity/mod.rs");
    let cancellation_module = include_str!("../src/activity/cancellation.rs");
    let model_module = include_str!("../src/activity/model.rs");
    let provider_runtime = include_str!("../src/production/provider_runtime.rs");

    for forbidden in [
        "pub struct ActivityRuntimeGeneration",
        "pub enum ActivityDispatchError",
        "pub fn codex_turn_for_integration_test",
        "pub fn claude_task_for_integration_test",
        "pub fn codex_actor_for_integration_test",
        "pub async fn activity_runtime_generation_for_integration_test",
        "pub async fn cancel_activity_target_for_integration_test",
        "pub async fn attach_activity_cancellation_for_integration_test",
        "pub async fn cancel_activity_subtree_for_integration_test",
    ] {
        assert!(
            !activity_module.contains(forbidden)
                && !cancellation_module.contains(forbidden)
                && !model_module.contains(forbidden)
                && !provider_runtime.contains(forbidden),
            "production test bypass is publicly callable: {forbidden}"
        );
    }
}

#[derive(Debug)]
struct RejectingProviderDriverFactory;

impl ProviderDriverFactory for RejectingProviderDriverFactory {
    fn create(
        &self,
        request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        Box::pin(async move {
            Err(ProviderRuntimeError::UnsupportedProvider {
                provider: request.provider,
            })
        })
    }
}

#[derive(Debug, Default)]
struct LoadTerminalObserverFactory {
    helper_launches: AtomicUsize,
    dormant_frames: AtomicUsize,
    decoded_events: AtomicUsize,
}

impl LoadTerminalObserverFactory {
    fn deliver_bounded_dormant_frame(&self) {
        self.dormant_frames.fetch_add(1, Ordering::AcqRel);
    }
}

impl ProviderTerminalObserverFactory for LoadTerminalObserverFactory {
    fn prepare(
        &self,
        input: ProviderTerminalObserverFactoryInput,
    ) -> Pin<Box<dyn Future<Output = Option<PreparedTerminalLaunch>> + Send + '_>> {
        Box::pin(async move {
            self.helper_launches.fetch_add(1, Ordering::AcqRel);
            Some(PreparedTerminalLaunch {
                executable: input.launch.executable,
                args: input.launch.args,
                private_env: BTreeMap::new(),
                observer: Box::new(LoadTerminalObserver),
            })
        })
    }
}

#[derive(Debug)]
struct LoadTerminalObserver;

impl PreparedTerminalObserver for LoadTerminalObserver {
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
                TerminalAgentActivityTransition {
                    resumed: 1,
                    ..TerminalAgentActivityTransition::default()
                }
            } else {
                TerminalAgentActivityTransition {
                    stopped: 1,
                    dormant: 1,
                    ..TerminalAgentActivityTransition::default()
                }
            }
        })
    }

    fn diagnostic_label(&self) -> &str {
        "activity-load-observer"
    }
}

#[derive(Debug)]
struct LoadPtyProcess {
    pid: u32,
    identity: ProcessIdentity,
    output: tokio::sync::broadcast::Sender<String>,
    exit: tokio::sync::watch::Sender<Option<PtyExit>>,
}

impl LoadPtyProcess {
    fn new(pid: u32) -> Self {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        assert_ne!(pid, 0, "synthetic PTY PID must be nonzero");
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        assert_ne!(generation, 0, "synthetic PTY generation wrapped to zero");
        let (output, _) = tokio::sync::broadcast::channel(2);
        let (exit, _) = tokio::sync::watch::channel(None);
        Self {
            pid,
            identity: ProcessIdentity {
                pid,
                started_at: generation,
            },
            output,
            exit,
        }
    }
}

impl PtyProcess for LoadPtyProcess {
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
        self.exit.send_replace(Some(PtyExit {
            exit_code: None,
            signal: None,
        }));
        Ok(())
    }

    fn subscribe_output(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.output.subscribe()
    }

    fn subscribe_exit(&self) -> tokio::sync::watch::Receiver<Option<PtyExit>> {
        self.exit.subscribe()
    }
}

#[test]
fn load_test_ptys_distinguish_same_pid_generations() {
    let first = LoadPtyProcess::new(10_000);
    let replacement = LoadPtyProcess::new(10_000);
    assert_ne!(first.identity.started_at, 0);
    assert!(replacement.identity.started_at > first.identity.started_at);
    assert_ne!(first.identity, replacement.identity);
}

#[derive(Debug, Default)]
struct LoadPtyBackend {
    next_pid: AtomicU32,
    spawns: StdMutex<usize>,
}

impl PtyBackend for LoadPtyBackend {
    fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        *self.spawns.lock().expect("spawn count") += 1;
        Ok(Arc::new(LoadPtyProcess::new(
            self.next_pid.fetch_add(1, Ordering::AcqRel) + 10_000,
        )))
    }
}

#[tokio::test]
async fn disabled_gate_rejects_dormant_volume_without_work_or_trace_growth() {
    let fixture = tempfile::tempdir().expect("activity load fixture");
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .expect("orchestration engine");
    let chat_controller = AgentActivityController::new(true);
    let terminal_controller = AgentActivityController::new(true);
    let projections = ActivityProjections::new(
        ActivityRepository::new(database.clone()),
        chat_controller.clone(),
        terminal_controller.clone(),
    );
    let controller = terminal_controller;
    let projection = projections.terminal();
    let provider_runtime = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(RejectingProviderDriverFactory),
        projections.chat(),
        SupervisorOptions::default(),
    ));

    let configured = fixture.path().join("configured-codex");
    std::fs::write(&configured, b"configured").expect("configured provider executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&configured, std::fs::Permissions::from_mode(0o700))
            .expect("provider executable permissions");
    }
    let mut settings = ProviderSettingsState::default();
    settings.providers.codex.binary_path = configured.to_string_lossy().into_owned();
    let inventory = ProviderTerminalInventory::from_settings(&settings);
    let terminal_factory = Arc::new(LoadTerminalObserverFactory::default());
    let terminal_supervisor = ProviderTerminalActivitySupervisor::new(
        settings,
        inventory,
        projection.clone(),
        ProcessAttributionRegistry::new(),
        fixture.path().join("provider-terminal-runtime"),
        ProviderTerminalObserverFactories {
            codex: Some(terminal_factory.clone()),
            ..ProviderTerminalObserverFactories::default()
        },
    )
    .expect("provider terminal supervisor");
    let backend = Arc::new(LoadPtyBackend::default());
    let terminal_manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            launch_preparer: Some(Arc::new(terminal_supervisor)),
            subprocess_poll_interval: Duration::ZERO,
            ..TerminalManagerOptions::default()
        },
    );
    let trace = TraceDiagnosticsStore::new(fixture.path().join("agent-activity.trace.ndjson"));
    let coordinator = ProductionAgentActivity::new(
        projections,
        provider_runtime.clone(),
        terminal_manager.clone(),
        trace.clone(),
        "activity-load-environment".to_owned(),
    );
    coordinator.record_startup(0).await;

    for index in 0..=PREVIOUSLY_INSTRUMENTED_TERMINALS {
        terminal_manager
            .open(activity_terminal_input(
                fixture.path(),
                &configured,
                "enabled",
                index,
            ))
            .await
            .expect("instrumented terminal");
    }
    terminal_manager
        .close("thread-enabled-2", Some("terminal-enabled-2"))
        .await
        .expect("closed instrumented terminal");
    assert_eq!(
        terminal_manager.agent_activity_restart_descriptor_count_for_integration_test(),
        PREVIOUSLY_INSTRUMENTED_TERMINALS
    );

    let disabled = coordinator
        .transition(AgentActivitySource::Terminal, false, 1)
        .await;
    assert!(!disabled.enabled);
    assert_eq!(
        disabled.dormant_observers,
        PREVIOUSLY_INSTRUMENTED_TERMINALS
    );
    assert_eq!(
        terminal_manager.agent_activity_restart_descriptor_count_for_integration_test(),
        PREVIOUSLY_INSTRUMENTED_TERMINALS
    );
    let trace_count_before_rejected_volume = trace_record_count(&trace);
    assert_eq!(trace_count_before_rejected_volume, 4);

    let queue_observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("database queue observer");
    let streams_before = controller.active_stream_count_for_integration_test();
    let database_before = database.queue_backpressure_snapshot_for_integration_test();
    let trace_before = trace_record_count(&trace);
    let mut apply_completions = projection.subscribe_apply_completions_for_integration_test();

    for _ in 0..DISABLED_EVENT_COUNT {
        terminal_factory.deliver_bounded_dormant_frame();
    }

    assert_eq!(
        terminal_factory.dormant_frames.load(Ordering::Acquire),
        DISABLED_EVENT_COUNT
    );
    assert_eq!(terminal_factory.decoded_events.load(Ordering::Acquire), 0);
    assert_eq!(
        controller.active_stream_count_for_integration_test(),
        streams_before
    );
    assert_eq!(
        database.queue_backpressure_snapshot_for_integration_test(),
        database_before
    );
    assert!(matches!(
        apply_completions.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(trace_record_count(&trace), trace_before);

    let helper_launches_before_disabled_terminals =
        terminal_factory.helper_launches.load(Ordering::Acquire);
    for index in 0..DISABLED_TERMINAL_LAUNCHES {
        terminal_manager
            .open(activity_terminal_input(
                fixture.path(),
                &configured,
                "disabled",
                index,
            ))
            .await
            .expect("disabled pass-through terminal");
    }
    assert_eq!(
        terminal_factory.helper_launches.load(Ordering::Acquire)
            - helper_launches_before_disabled_terminals,
        0,
        "disabled terminal launches start no activity helpers"
    );
    assert_eq!(
        *backend.spawns.lock().expect("spawn count"),
        PREVIOUSLY_INSTRUMENTED_TERMINALS + 1 + DISABLED_TERMINAL_LAUNCHES,
        "disabled terminals still launch their requested PTY in pass-through mode"
    );
    assert_eq!(
        terminal_manager.agent_activity_restart_descriptor_count_for_integration_test(),
        PREVIOUSLY_INSTRUMENTED_TERMINALS,
        "only live terminals instrumented before disable retain restart descriptors"
    );

    let scope = ActivityScopeSeed::thread(
        "thread:disabled-volume",
        "disabled-volume",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("disabled load scope");
    let mut projection_delta_count = 0;
    for index in 0..DISABLED_EVENT_COUNT {
        assert!(
            controller.register_stream().is_none(),
            "disabled gate rejects activity stream registration"
        );
        projection_delta_count += projection
            .apply(
                &scope.scope_id,
                format!("disabled-load-event:{index}"),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        format!("actor:disabled-load:{index}"),
                        None,
                        SECRET_DISABLED_PAYLOAD,
                        "running",
                    )
                    .expect("disabled actor mutation"),
                ],
                timestamp(index),
            )
            .await
            .expect("disabled projection rejection")
            .len();
    }

    assert_eq!(projection_delta_count, 0);
    assert!(matches!(
        apply_completions.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(controller.active_stream_count_for_integration_test(), 0);
    assert_eq!(
        projection.activity_stream_receiver_count_for_integration_test(),
        0
    );
    assert_eq!(projection.registry_counts_for_integration_test(), (0, 0));
    let queue = database.queue_backpressure_snapshot_for_integration_test();
    assert_eq!(queue.reserved_or_queued_jobs, 0);
    assert_eq!(queue.waiting_for_permit, 0);
    assert_eq!(queue.max_reserved_or_queued_jobs, 0);
    assert_eq!(
        trace_record_count(&trace),
        trace_count_before_rejected_volume,
        "rejected event volume creates no trace growth"
    );
    assert_safe_transition_trace(&trace);

    drop(queue_observer);
    terminal_manager.shutdown().await;
    provider_runtime
        .shutdown()
        .await
        .expect("provider shutdown");
    engine.shutdown().await;
}

fn activity_terminal_input(
    root: &std::path::Path,
    executable: &std::path::Path,
    phase: &str,
    index: usize,
) -> TerminalOpenInput {
    let mut input = TerminalOpenInput::new(
        format!("thread-{phase}-{index}"),
        format!("terminal-{phase}-{index}"),
        root.to_path_buf(),
        80,
        24,
    );
    input.command = Some(TerminalLaunchCommand {
        executable: executable.to_string_lossy().into_owned(),
        args: Vec::new(),
        label: Some("Codex load fixture".to_owned()),
        activity: Some(ProviderTerminalActivityLaunch {
            driver_kind: "codex".to_owned(),
            provider_instance_id: "codex".to_owned(),
        }),
        env: None,
    });
    input
}

fn trace_record_count(trace: &TraceDiagnosticsStore) -> usize {
    std::fs::read_to_string(trace.path())
        .unwrap_or_default()
        .lines()
        .count()
}

fn assert_safe_transition_trace(trace: &TraceDiagnosticsStore) {
    let raw = std::fs::read_to_string(trace.path()).expect("transition trace");
    assert!(!raw.contains(SECRET_DISABLED_PAYLOAD));
    assert!(!raw.contains("disabled-load-event:"));
    let records = raw
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid trace record"))
        .collect::<Vec<_>>();
    assert_eq!(
        records
            .iter()
            .map(|record| record["name"].as_str().expect("trace name"))
            .collect::<Vec<_>>(),
        vec![
            "agent_activity_enabled",
            "agent_activity_enabled",
            "agent_activity_change_requested",
            "agent_activity_disabled",
        ]
    );
    let allowed_attributes = [
        "cause",
        "environmentId",
        "source",
        "enabled",
        "settingsGeneration",
        "observationGeneration",
        "closedSubscriptions",
        "stoppedObservers",
        "dormantObservers",
        "resumedObservers",
        "failedObservers",
        "unavailableObservers",
        "terminalObservationEpochs",
        "finalizedRecords",
        "durationMs",
    ];
    for record in records {
        assert_eq!(record["exit"]["_tag"], "Success");
        let name = record["name"].as_str().expect("trace name");
        let attributes = record["events"][0]["attributes"]
            .as_object()
            .expect("bounded trace attributes");
        assert!(
            attributes
                .keys()
                .all(|key| allowed_attributes.contains(&key.as_str()))
        );
        if name == "agent_activity_change_requested" {
            assert!(!attributes.contains_key("terminalObservationEpochs"));
        } else {
            let epochs = attributes
                .get("terminalObservationEpochs")
                .and_then(Value::as_object)
                .expect("fixed provider epoch object");
            assert_eq!(
                epochs.keys().map(String::as_str).collect::<BTreeSet<_>>(),
                BTreeSet::from(["claude", "codex", "opencode"]),
            );
            assert!(epochs.values().all(Value::is_u64));
        }
        for (key, value) in attributes {
            if key == "terminalObservationEpochs" {
                continue;
            }
            match value {
                Value::Bool(_) | Value::Number(_) => {}
                Value::String(value) => assert!(value.chars().count() <= 128),
                value => panic!("unbounded transition trace value: {value:?}"),
            }
        }
    }
}

#[tokio::test]
async fn disabled_gate_reserves_no_database_jobs_for_mutations_or_reads() {
    // Mutation caught: allocating a publication lock or reserving database work before gate rejection.
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("queue observer");
    let scope = ActivityScopeSeed::thread(
        "thread:disabled-gate",
        "disabled-gate",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("scope");
    let projection = ActivityProjection::with_controller(
        ActivityRepository::new(database.clone()),
        AgentActivityController::new(false),
    );

    projection
        .ensure_scope(scope.clone())
        .await
        .expect("disabled ensure is a no-op");
    let deltas = projection
        .apply(
            &scope.scope_id,
            "disabled-event".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:disabled",
                    None,
                    "Disabled",
                    "running",
                )
                .expect("actor"),
            ],
            timestamp(0),
        )
        .await
        .expect("disabled apply is a no-op");
    assert!(deltas.is_empty());
    assert!(projection.snapshot(&scope.scope).await.is_err());
    assert_eq!(
        database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs,
        0
    );
    assert_eq!(projection.registry_counts_for_integration_test(), (0, 0));
    drop(observer);
}

#[tokio::test]
async fn high_volume_projection_keeps_batches_pages_subscribers_and_retention_bounded() {
    let started_at = Instant::now();
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection =
        ActivityProjection::with_capacity(ActivityRepository::new(database.clone()), 8);
    let scope = ActivityScopeSeed::thread(
        "thread:activity-load",
        "activity-load",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("scope");
    projection
        .ensure_scope(scope.clone())
        .await
        .expect("scope persistence");

    let mut lagged_subscriber = projection.subscribe();
    let mut writers = JoinSet::new();
    for actor_index in 0..ACTOR_COUNT {
        let projection = projection.clone();
        let scope_id = scope.scope_id.clone();
        writers.spawn(async move {
            let actor_id = actor_id(actor_index);
            let mut deltas = projection
                .apply(
                    &scope_id,
                    format!("load:{actor_index}:actor"),
                    vec![
                        ProviderActivityMutation::upsert_actor(
                            actor_id.clone(),
                            None,
                            format!("Load actor {actor_index}"),
                            "running",
                        )
                        .expect("actor"),
                    ],
                    timestamp(actor_index * EVENTS_PER_ACTOR),
                )
                .await
                .expect("actor projection");

            for event_index in 0..EVENTS_PER_ACTOR {
                let sequence = actor_index * EVENTS_PER_ACTOR + event_index;
                let entry = ActivityEntry::try_new(
                    format!("entry:{actor_index:02}:{event_index:03}"),
                    ActivityRecordKind::Actor,
                    actor_id.clone(),
                    entry_kind(event_index),
                    format!("Load event {sequence}"),
                    None,
                    ActivityEntryTone::Info,
                    timestamp(sequence),
                )
                .expect("entry");
                let event_deltas = projection
                    .apply(
                        &scope_id,
                        format!("load:{actor_index}:event:{event_index}"),
                        vec![ProviderActivityMutation::AppendEntry(entry)],
                        timestamp(sequence),
                    )
                    .await
                    .expect("event projection");
                deltas.extend(event_deltas);
            }
            deltas
        });
    }

    let mut delta_count = 0;
    while let Some(result) = writers.join_next().await {
        let writer_deltas = result.expect("writer task");
        assert!(
            writer_deltas
                .iter()
                .all(|delta| delta.changes.len() <= MUTATION_BATCH_LIMIT)
        );
        delta_count += writer_deltas.len();
    }

    assert_eq!(delta_count, ACTOR_COUNT * (EVENTS_PER_ACTOR + 1));

    let snapshot = projection.snapshot(&scope.scope).await.expect("snapshot");
    assert!(snapshot.actors.len() <= PAGE_LIMIT);
    assert!(snapshot.work_items.len() <= PAGE_LIMIT);
    assert_eq!(snapshot.counts.subagents.active, ACTOR_COUNT as u64);

    let roster = projection
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Active,
            None,
            PAGE_LIMIT,
        )
        .await
        .expect("roster page");
    assert!(roster.records.len() <= PAGE_LIMIT);

    let detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            &actor_id(0),
            None,
            75,
        )
        .await
        .expect("detail page");
    assert_eq!(detail.entries.len(), 75);
    assert!(detail.entries.len() <= PAGE_LIMIT);

    assert!(matches!(
        lagged_subscriber.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    let fresh_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("fresh snapshot");
    assert_eq!(fresh_snapshot.revision, snapshot.revision);

    let cancellation = CancellationToken::new();
    let cancelled = cancellation.clone();
    let mut cancellable_subscriber = projection.subscribe();
    let subscriber = tokio::spawn(async move {
        tokio::select! {
            () = cancelled.cancelled() => true,
            _ = cancellable_subscriber.recv() => false,
        }
    });
    cancellation.cancel();
    assert!(
        timeout(Duration::from_secs(1), subscriber)
            .await
            .expect("cancellation must finish promptly")
            .expect("subscriber task")
    );

    for _ in 0..100 {
        let journal_rows = journal_rows(&database).await;
        let event_rows = event_idempotency_rows(&database).await;
        if journal_rows <= JOURNAL_ROW_LIMIT && event_rows <= JOURNAL_ROW_LIMIT {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(journal_rows(&database).await <= JOURNAL_ROW_LIMIT);
    assert!(event_idempotency_rows(&database).await <= JOURNAL_ROW_LIMIT);

    assert_database_queue_backpressure(&database).await;
    assert_same_scope_publication_order(&database).await;
    assert_exact_delta_limit_split(&database).await;

    println!(
        "activity projection load: {} actors × {} events in {:.1?}",
        ACTOR_COUNT,
        EVENTS_PER_ACTOR,
        started_at.elapsed()
    );
}

async fn assert_database_queue_backpressure(database: &Database) {
    let queue_observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("queue observation must be exclusively enabled");
    let projection =
        ActivityProjection::with_capacity(ActivityRepository::new(database.clone()), 1);
    let scopes = (0..QUEUED_WRITER_COUNT)
        .map(|index| {
            ActivityScopeSeed::thread(
                format!("thread:activity-queue:{index:03}"),
                format!("activity-queue-{index:03}"),
                "codex",
                Some("codex"),
                ActivityCapabilities::structured_full(false),
            )
            .expect("queue scope")
        })
        .collect::<Vec<_>>();
    for scope in &scopes {
        projection
            .ensure_scope(scope.clone())
            .await
            .expect("queue scope persistence");
    }

    let (release_sender, release_receiver) = mpsc::sync_channel(1);
    let (blocker_started_sender, blocker_started_receiver) = oneshot::channel();
    let blocked_database = database.clone();
    let blocker = tokio::spawn(async move {
        blocked_database
            .call(move |_connection| {
                blocker_started_sender
                    .send(())
                    .expect("blocker start receiver");
                release_receiver.recv().expect("blocker release");
                Ok(())
            })
            .await
            .expect("blocked database job")
    });
    blocker_started_receiver
        .await
        .expect("database blocker must start");

    let barrier = Arc::new(Barrier::new(QUEUED_WRITER_COUNT + 1));
    let mut writers = JoinSet::new();
    for (index, scope) in scopes.into_iter().enumerate() {
        let barrier = Arc::clone(&barrier);
        let projection = projection.clone();
        writers.spawn(async move {
            barrier.wait().await;
            projection
                .apply(
                    &scope.scope_id,
                    format!("queue-pressure:{index}"),
                    vec![
                        ProviderActivityMutation::upsert_actor(
                            format!("actor:queue-pressure:{index}"),
                            None,
                            format!("Queue pressure actor {index}"),
                            "running",
                        )
                        .expect("queue pressure actor"),
                    ],
                    timestamp(index),
                )
                .await
                .expect("independent queue-pressure apply")
        });
    }
    barrier.wait().await;
    let queue_snapshot = timeout(
        Duration::from_secs(3),
        queue_observer.wait_for_queue_backpressure_for_integration_test(QUEUED_WRITER_COUNT),
    )
    .await
    .expect("database queue must fill before the held worker releases")
    .expect("queue observation must remain enabled until the bounded state is captured");
    assert_eq!(queue_snapshot.queue_capacity, DATABASE_QUEUE_CAPACITY);
    assert_eq!(
        queue_snapshot.reserved_or_queued_jobs, DATABASE_QUEUE_CAPACITY,
        "the sender boundary must never accept more than its capacity"
    );
    assert_eq!(
        queue_snapshot.waiting_for_permit,
        QUEUED_WRITER_COUNT - DATABASE_QUEUE_CAPACITY,
        "all remaining independent writers must block at sender reserve"
    );
    assert_eq!(
        queue_snapshot.max_reserved_or_queued_jobs, DATABASE_QUEUE_CAPACITY,
        "the queue peak must reach but never exceed the bounded capacity"
    );
    release_sender.send(()).expect("release database queue");
    timeout(Duration::from_secs(5), async {
        while let Some(result) = writers.join_next().await {
            let deltas = result.expect("queue-pressure writer");
            assert_eq!(deltas.len(), 1);
            assert_eq!(deltas[0].previous_revision, 0);
            assert_eq!(deltas[0].revision, 1);
        }
    })
    .await
    .expect("independent database writers must drain after backpressure releases");
    blocker.await.expect("database blocker joins");
    drop(queue_observer);
    let drained_snapshot = database.queue_backpressure_snapshot_for_integration_test();
    assert!(
        !drained_snapshot.observation_enabled,
        "dropping the observer must disable queue diagnostics"
    );
    assert_eq!(drained_snapshot.reserved_or_queued_jobs, 0);
    assert_eq!(drained_snapshot.waiting_for_permit, 0);
    assert_eq!(drained_snapshot.max_reserved_or_queued_jobs, 0);
}

async fn assert_same_scope_publication_order(database: &Database) {
    let projection = ActivityProjection::with_capacity(
        ActivityRepository::new(database.clone()),
        QUEUED_WRITER_COUNT + 1,
    );
    let scope = ActivityScopeSeed::thread(
        "thread:activity-publication-order",
        "activity-publication-order",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("publication-order scope");
    projection
        .ensure_scope(scope.clone())
        .await
        .expect("publication-order scope persistence");
    let mut receiver = projection.subscribe();
    let mut completion_receiver = projection.subscribe_apply_completions_for_integration_test();
    let mut writers = JoinSet::new();
    for index in 0..QUEUED_WRITER_COUNT {
        let projection = projection.clone();
        let scope_id = scope.scope_id.clone();
        writers.spawn(async move {
            projection
                .apply(
                    &scope_id,
                    format!("publication-order:{index}"),
                    vec![
                        ProviderActivityMutation::upsert_actor(
                            format!("actor:publication-order:{index}"),
                            None,
                            format!("Publication order actor {index}"),
                            "running",
                        )
                        .expect("publication-order actor"),
                    ],
                    timestamp(index),
                )
                .await
                .expect("same-scope apply")
        });
    }

    let mut broadcast_revisions = Vec::new();
    for expected_revision in 1..=QUEUED_WRITER_COUNT as u64 {
        let event = timeout(Duration::from_secs(3), receiver.recv())
            .await
            .expect("publication must arrive")
            .expect("publication receiver");
        let coddy_studio_server::activity::ActivityProjectionEvent::Delta(delta) = event else {
            panic!("expected same-scope delta");
        };
        assert_eq!(delta.scope_id, scope.scope_id);
        assert_eq!(delta.previous_revision, expected_revision - 1);
        assert_eq!(delta.revision, expected_revision);
        broadcast_revisions.push(delta.revision);
    }
    let mut authoritative_completion_revisions = Vec::new();
    for expected_revision in 1..=QUEUED_WRITER_COUNT as u64 {
        let completion = timeout(Duration::from_secs(3), completion_receiver.recv())
            .await
            .expect("apply completion must arrive")
            .expect("apply completion receiver");
        assert_eq!(completion.scope_id, scope.scope_id);
        assert_eq!(completion.previous_revision, expected_revision - 1);
        assert_eq!(completion.revision, expected_revision);
        authoritative_completion_revisions.push(completion.revision);
    }
    assert_eq!(authoritative_completion_revisions, broadcast_revisions);
    let mut joined_completion_revisions = Vec::new();
    timeout(Duration::from_secs(5), async {
        while let Some(result) = writers.join_next().await {
            let deltas = result.expect("same-scope writer");
            assert_eq!(deltas.len(), 1);
            joined_completion_revisions.push(deltas[0].revision);
        }
    })
    .await
    .expect("same-scope writers must complete");
    assert_eq!(joined_completion_revisions.len(), QUEUED_WRITER_COUNT);
    // `JoinSet` observes executor scheduling after `apply` returns, so its sequence is
    // intentionally not an ordering contract. The in-lock completion stream above is the
    // authoritative order; this audit proves JoinSet did observe that exact contiguous set.
    let mut joined_revision_seen = [false; QUEUED_WRITER_COUNT + 1];
    for revision in &joined_completion_revisions {
        assert!(
            (1..=QUEUED_WRITER_COUNT as u64).contains(revision),
            "JoinSet returned a revision outside the authoritative contiguous range"
        );
        let index = *revision as usize;
        assert!(
            !joined_revision_seen[index],
            "JoinSet returned the same apply completion revision twice"
        );
        joined_revision_seen[index] = true;
    }
    for revision in &authoritative_completion_revisions {
        assert!(
            joined_revision_seen[*revision as usize],
            "the actual JoinSet completion sequence must contain every authoritative completion"
        );
    }
    let snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("same-scope final snapshot");
    assert_eq!(
        snapshot.revision,
        *authoritative_completion_revisions
            .last()
            .expect("completion")
    );
    println!(
        "same-scope publication: broadcast {:?}, in-lock completion {:?}, JoinSet scheduling order {:?}",
        broadcast_revisions, authoritative_completion_revisions, joined_completion_revisions
    );
}

async fn assert_exact_delta_limit_split(database: &Database) {
    let projection =
        ActivityProjection::with_capacity(ActivityRepository::new(database.clone()), 3);
    let scope = ActivityScopeSeed::thread(
        "thread:activity-exact-delta-limit",
        "activity-exact-delta-limit",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("exact-limit scope");
    projection
        .ensure_scope(scope.clone())
        .await
        .expect("exact-limit scope persistence");
    let mut receiver = projection.subscribe();
    let mutations = (0..MUTATION_BATCH_LIMIT)
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:exact-limit:{index:03}"),
                None,
                format!("Exact limit actor {index}"),
                "running",
            )
            .expect("exact-limit actor")
        })
        .collect();
    let deltas = projection
        .apply(
            &scope.scope_id,
            "exact-delta-limit".to_owned(),
            mutations,
            "2026-07-22T12:00:00Z".to_owned(),
        )
        .await
        .expect("exact-limit apply");
    assert_eq!(deltas.len(), 2);
    assert_eq!(deltas[0].changes.len(), MUTATION_BATCH_LIMIT);
    assert_eq!(deltas[1].changes.len(), 1);
    assert_eq!(
        deltas
            .iter()
            .map(|delta| (delta.previous_revision, delta.revision))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 2)]
    );
    for expected in &deltas {
        assert_eq!(
            receiver.recv().await.expect("split publication"),
            coddy_studio_server::activity::ActivityProjectionEvent::Delta(expected.clone())
        );
    }
}

fn actor_id(index: usize) -> String {
    format!("actor:load:{index:02}")
}

fn timestamp(index: usize) -> String {
    format!("2026-07-22T12:00:{:02}.000Z", index % 60)
}

fn entry_kind(index: usize) -> ActivityEntryKind {
    match index % 3 {
        0 => ActivityEntryKind::Commentary,
        1 => ActivityEntryKind::Tool,
        _ => ActivityEntryKind::State,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessRssSample {
    baseline_rss_bytes: u64,
    peak_rss_bytes: u64,
    delta_bytes: u64,
}

#[derive(Debug)]
struct ProcessRssSampler {
    baseline_rss_bytes: u64,
    stop_sender: mpsc::SyncSender<()>,
    sampler_thread: std::thread::JoinHandle<u64>,
}

impl ProcessRssSampler {
    fn start() -> Self {
        let process_id = Pid::from_u32(std::process::id());
        let mut system = System::new();
        let baseline_rss_bytes = current_process_rss_bytes(&mut system, process_id);
        let (stop_sender, stop_receiver) = mpsc::sync_channel(1);
        let sampler_thread = std::thread::Builder::new()
            .name("coddy-studio-activity-load-rss".to_owned())
            .spawn(move || {
                let mut peak_rss_bytes = baseline_rss_bytes;
                loop {
                    match stop_receiver.recv_timeout(PROCESS_RSS_SAMPLE_INTERVAL) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            peak_rss_bytes = peak_rss_bytes
                                .max(current_process_rss_bytes(&mut system, process_id));
                            return peak_rss_bytes;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            peak_rss_bytes = peak_rss_bytes
                                .max(current_process_rss_bytes(&mut system, process_id));
                        }
                    }
                }
            })
            .expect("process RSS sampler thread");
        Self {
            baseline_rss_bytes,
            stop_sender,
            sampler_thread,
        }
    }

    fn finish(self) -> ProcessRssSample {
        self.stop_sender
            .send(())
            .expect("process RSS sampler remains available");
        let peak_rss_bytes = self
            .sampler_thread
            .join()
            .expect("process RSS sampler thread joins");
        ProcessRssSample {
            baseline_rss_bytes: self.baseline_rss_bytes,
            peak_rss_bytes,
            delta_bytes: peak_rss_bytes
                .checked_sub(self.baseline_rss_bytes)
                .expect("sampled peak RSS includes the baseline"),
        }
    }
}

fn current_process_rss_bytes(system: &mut System, process_id: Pid) -> u64 {
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[process_id]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system
        .process(process_id)
        .expect("current process remains visible to sysinfo")
        .memory()
}

fn print_runtime_memory_diagnostic(
    label: &str,
    process_rss: ProcessRssSample,
    retained_snapshot_summaries: usize,
    retained_detail_entries: usize,
    retained_journal_rows: i64,
    retained_idempotency_rows: i64,
) {
    assert!(
        process_rss.delta_bytes <= PROCESS_RSS_DELTA_LIMIT_BYTES,
        "{label} allocated an unexpectedly large process RSS delta: {} bytes",
        process_rss.delta_bytes
    );
    println!(
        "{label} memory: baseline_rss_bytes={}, sampled_peak_rss_bytes={}, rss_delta_bytes={}; retained snapshot summaries={retained_snapshot_summaries}, detail entries={retained_detail_entries}, journal/idempotency rows={retained_journal_rows}/{retained_idempotency_rows}",
        process_rss.baseline_rss_bytes, process_rss.peak_rss_bytes, process_rss.delta_bytes
    );
}

async fn journal_rows(database: &Database) -> i64 {
    database
        .call(|connection| {
            let count =
                connection.query_row("SELECT COUNT(*) FROM activity_journal", [], |row| {
                    row.get(0)
                })?;
            Ok(count)
        })
        .await
        .expect("journal count")
}

async fn event_idempotency_rows(database: &Database) -> i64 {
    database
        .call(|connection| {
            let count = connection.query_row(
                "SELECT COUNT(*) FROM activity_event_idempotency",
                [],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .await
        .expect("event idempotency count")
}

#[test]
fn process_rss_sampler_measures_real_current_process_memory() {
    let sample = ProcessRssSampler::start().finish();

    assert!(sample.baseline_rss_bytes > 0);
    assert!(sample.peak_rss_bytes >= sample.baseline_rss_bytes);
    assert_eq!(
        sample.delta_bytes,
        sample.peak_rss_bytes - sample.baseline_rss_bytes
    );
}

#[tokio::test]
async fn high_volume_rpc_stream_replaces_lagged_subscribers_and_retains_exact_caps() {
    let started_at = Instant::now();
    let process_rss_sampler = ProcessRssSampler::start();
    let fixture = RpcFixture::start(2).await;
    let scope = ActivityScopeSeed::thread(
        "thread:activity-rpc-load",
        "activity-rpc-load",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("scope");
    fixture
        .projection
        .ensure_scope(scope.clone())
        .await
        .expect("scope persistence");

    let mut socket = fixture.connect().await;
    request(
        &mut socket,
        "101",
        "subscribeActivity",
        json!({ "_tag": "thread", "threadId": "activity-rpc-load" }),
    )
    .await;
    assert!(matches!(
        next_message(&mut socket).await,
        ServerMessage::Chunk { ref values, .. } if values[0]["kind"] == "snapshot"
    ));
    assert!(
        fixture
            .projection
            .activity_stream_receiver_count_for_integration_test()
            > 0
    );

    seed_capped_rosters(&fixture.projection, &scope).await;
    append_actor_event_batches(&fixture.projection, &scope).await;
    let mut writers = JoinSet::new();
    for writer_index in 0..QUEUED_WRITER_COUNT {
        let projection = fixture.projection.clone();
        let scope_id = scope.scope_id.clone();
        writers.spawn(async move {
            projection
                .apply(
                    &scope_id,
                    format!("queue:{writer_index}"),
                    vec![ProviderActivityMutation::AppendEntry(load_entry(
                        &format!("queue-entry:{writer_index}"),
                        "actor:load:00",
                        writer_index,
                    ))],
                    timestamp(writer_index),
                )
                .await
                .expect("queued writer")
        });
    }
    while let Some(result) = writers.join_next().await {
        let deltas = result.expect("writer task");
        assert!(
            deltas
                .iter()
                .all(|delta| delta.changes.len() <= MUTATION_BATCH_LIMIT)
        );
    }

    assert_direct_detail_pages(&fixture.database).await;

    let mut sampled_journal_peak = 0;
    let mut sampled_event_idempotency_peak = 0;
    for event_index in 0..=JOURNAL_ROW_LIMIT as usize {
        fixture
            .projection
            .apply(
                &scope.scope_id,
                format!("retention:{event_index}"),
                vec![ProviderActivityMutation::AppendEntry(load_entry(
                    &format!("retention-entry:{event_index}"),
                    "actor:load:00",
                    event_index + 10_000,
                ))],
                timestamp(event_index),
            )
            .await
            .expect("retention event");
        if event_index % MUTATION_BATCH_LIMIT == 0 || event_index == JOURNAL_ROW_LIMIT as usize {
            let (journal_rows, _, _, event_rows, _, _) =
                retention_envelope(&fixture.database, &scope.scope_id).await;
            assert!(journal_rows > 0 && journal_rows <= JOURNAL_ROW_LIMIT);
            assert!(event_rows > 0 && event_rows <= JOURNAL_ROW_LIMIT);
            sampled_journal_peak = sampled_journal_peak.max(journal_rows);
            sampled_event_idempotency_peak = sampled_event_idempotency_peak.max(event_rows);
        }
    }

    let snapshot = fixture
        .projection
        .snapshot(&scope.scope)
        .await
        .expect("snapshot");
    assert_eq!(snapshot.actors.len(), PAGE_LIMIT);
    assert_eq!(snapshot.work_items.len(), PAGE_LIMIT);
    assert!(snapshot.actors_has_more);
    assert!(snapshot.work_items_has_more);

    ack(&mut socket, "101").await;
    let replacement = loop {
        let message = next_message(&mut socket).await;
        if let ServerMessage::Chunk { values, .. } = message
            && values[0]["kind"] == "snapshot"
        {
            break values[0].clone();
        }
        ack(&mut socket, "101").await;
    };
    assert_eq!(replacement["snapshot"]["revision"], snapshot.revision);
    ack(&mut socket, "101").await;
    assert!(
        timeout(Duration::from_millis(100), next_message(&mut socket))
            .await
            .is_err(),
        "replacement snapshot must cover every queued delta"
    );

    socket
        .send(Message::Text(
            json!({ "_tag": "Interrupt", "requestId": "101" })
                .to_string()
                .into(),
        ))
        .await
        .expect("interrupt stream");
    assert!(matches!(
        timeout(Duration::from_secs(1), next_message(&mut socket))
            .await
            .expect("stream exit") ,
        ServerMessage::Exit { ref request_id, .. } if request_id.as_str() == "101"
    ));
    for _ in 0..100 {
        if fixture
            .projection
            .activity_stream_receiver_count_for_integration_test()
            == 0
        {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        fixture
            .projection
            .activity_stream_receiver_count_for_integration_test(),
        0
    );

    assert_rpc_roster_pages(
        &mut socket,
        "102",
        "103",
        &scope,
        "subagents",
        "active",
        ROSTER_RECORDS_PER_BUCKET + ACTOR_COUNT - PAGE_LIMIT,
    )
    .await;
    assert_rpc_roster_pages(
        &mut socket,
        "104",
        "105",
        &scope,
        "subagents",
        "done",
        ROSTER_RECORDS_PER_BUCKET - PAGE_LIMIT,
    )
    .await;
    assert_rpc_roster_pages(
        &mut socket,
        "106",
        "107",
        &scope,
        "backgroundTasks",
        "active",
        ROSTER_RECORDS_PER_BUCKET - PAGE_LIMIT,
    )
    .await;
    assert_rpc_roster_pages(
        &mut socket,
        "108",
        "109",
        &scope,
        "backgroundTasks",
        "done",
        ROSTER_RECORDS_PER_BUCKET - PAGE_LIMIT,
    )
    .await;

    let (journal_rows, journal_min, journal_max, event_rows, event_min, event_max) =
        retention_envelope(&fixture.database, &scope.scope_id).await;
    assert!(snapshot.revision > JOURNAL_ROW_LIMIT as u64);
    assert_eq!(sampled_journal_peak, JOURNAL_ROW_LIMIT);
    assert_eq!(sampled_event_idempotency_peak, JOURNAL_ROW_LIMIT);
    assert_eq!(journal_rows, JOURNAL_ROW_LIMIT);
    assert_eq!(event_rows, JOURNAL_ROW_LIMIT);
    assert!(journal_min > 1, "oldest journal revision must be pruned");
    assert_eq!(journal_max, snapshot.revision as i64);
    assert_eq!(journal_max - journal_min + 1, JOURNAL_ROW_LIMIT);
    assert!(event_min > 1, "oldest idempotency event must be pruned");
    assert_eq!(event_max, snapshot.revision as i64);
    let process_rss_sample = process_rss_sampler.finish();
    print_runtime_memory_diagnostic(
        "activity RPC load",
        process_rss_sample,
        snapshot.actors.len() + snapshot.work_items.len(),
        PAGE_LIMIT,
        journal_rows,
        event_rows,
    );
    // Wall time is observational: this correctness test runs in the parallel package graph.
    println!(
        "activity RPC load: {} queued writers, journal revisions {}..{}; retained snapshot summaries {}, detail entries {}, journal/idempotency peaks {}/{} and final rows {}/{} in {:.1?}",
        QUEUED_WRITER_COUNT,
        journal_min,
        journal_max,
        snapshot.actors.len() + snapshot.work_items.len(),
        PAGE_LIMIT,
        sampled_journal_peak,
        sampled_event_idempotency_peak,
        journal_rows,
        event_rows,
        started_at.elapsed()
    );

    socket.close(None).await.expect("close socket");
    fixture.shutdown().await;
}

async fn seed_capped_rosters(projection: &ActivityProjection, scope: &ActivityScopeSeed) {
    let retained_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("current UTC timestamp");
    let actor_mutations = (0..(ROSTER_RECORDS_PER_BUCKET * 2))
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:seed:{index:03}"),
                None,
                format!("Seed actor {index}"),
                if index < ROSTER_RECORDS_PER_BUCKET {
                    "running"
                } else {
                    "completed"
                },
            )
            .expect("actor")
        })
        .collect::<Vec<_>>();
    let work_item_mutations = (0..(ROSTER_RECORDS_PER_BUCKET * 2))
        .map(|index| {
            ProviderActivityMutation::UpsertWorkItem(
                ActivityWorkItemSummary::try_new(
                    format!("work:seed:{index:03}"),
                    None,
                    format!("Seed work {index}"),
                    "command",
                    Some("vp check"),
                    Some("/workspace"),
                    if index < ROSTER_RECORDS_PER_BUCKET {
                        ActivityLifecycle::Running
                    } else {
                        ActivityLifecycle::Completed
                    },
                    None,
                    retained_at.clone(),
                    retained_at.clone(),
                    if index < ROSTER_RECORDS_PER_BUCKET {
                        None
                    } else {
                        Some(retained_at.as_str())
                    },
                )
                .expect("work item"),
            )
        })
        .collect::<Vec<_>>();
    for (batch_index, mutations) in actor_mutations
        .chunks(MUTATION_BATCH_LIMIT - 1)
        .chain(work_item_mutations.chunks(MUTATION_BATCH_LIMIT - 1))
        .enumerate()
    {
        let deltas = projection
            .apply(
                &scope.scope_id,
                format!("seed:{batch_index}"),
                mutations.to_vec(),
                retained_at.clone(),
            )
            .await
            .expect("seed batch");
        assert!(
            deltas
                .iter()
                .all(|delta| delta.changes.len() <= MUTATION_BATCH_LIMIT)
        );
    }
}

async fn append_actor_event_batches(projection: &ActivityProjection, scope: &ActivityScopeSeed) {
    for actor_index in 0..ACTOR_COUNT {
        let actor_id = format!("actor:load:{actor_index:02}");
        projection
            .apply(
                &scope.scope_id,
                format!("load-actor:{actor_index}"),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        actor_id.clone(),
                        None,
                        format!("Load actor {actor_index}"),
                        "running",
                    )
                    .expect("load actor"),
                ],
                timestamp(actor_index),
            )
            .await
            .expect("load actor");
        let mutations = (0..EVENTS_PER_ACTOR)
            .map(|event_index| {
                ProviderActivityMutation::AppendEntry(load_entry(
                    &format!("load-entry:{actor_index:02}:{event_index:03}"),
                    &actor_id,
                    event_index,
                ))
            })
            .collect::<Vec<_>>();
        let deltas = projection
            .apply(
                &scope.scope_id,
                format!("load-events:{actor_index}"),
                mutations,
                timestamp(actor_index + 100),
            )
            .await
            .expect("load event batch");
        assert!(
            deltas
                .iter()
                .all(|delta| delta.changes.len() <= MUTATION_BATCH_LIMIT)
        );
    }
}

fn load_entry(id: &str, owner_id: &str, index: usize) -> ActivityEntry {
    ActivityEntry::try_new(
        id,
        ActivityRecordKind::Actor,
        owner_id,
        entry_kind(index),
        id,
        None,
        ActivityEntryTone::Info,
        timestamp(index),
    )
    .expect("entry")
}

async fn assert_direct_detail_pages(database: &Database) {
    let repository = ActivityRepository::new(database.clone());
    let scope = ActivityScopeSeed::thread(
        "thread:activity-detail-load",
        "activity-detail-load",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("detail scope");
    repository
        .ensure_scope(scope.clone())
        .await
        .expect("detail scope persistence");
    repository
        .apply_batch(
            &scope.scope_id,
            "detail-owner",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:detail-owner",
                    None,
                    "Detail owner",
                    "running",
                )
                .expect("detail owner"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("detail owner persistence");
    let entries = (0..(PAGE_LIMIT + 50))
        .map(|index| {
            ActivityEntry::try_new(
                format!("entry:detail:{index:03}"),
                ActivityRecordKind::Actor,
                "actor:detail-owner",
                ActivityEntryKind::Commentary,
                format!("Detail entry {index}"),
                None,
                ActivityEntryTone::Info,
                format!("2026-07-22T12:{:02}:{:02}.000Z", index / 60, index % 60),
            )
            .expect("detail entry")
        })
        .collect::<Vec<_>>();
    let scope_id = scope.scope_id.clone();
    database
        .call(move |connection| {
            let transaction = connection.transaction()?;
            for entry in &entries {
                transaction.execute(
                    "INSERT INTO activity_entries (
                       scope_id, entry_id, owner_kind, owner_id, native_sort_key,
                       entry_json, created_at
                     ) VALUES (?, ?, 'actor', 'actor:detail-owner', ?, ?, ?)",
                    rusqlite::params![
                        scope_id,
                        entry.id,
                        entry.created_at,
                        serde_json::to_string(entry).expect("entry JSON"),
                        entry.created_at,
                    ],
                )?;
            }
            transaction.execute(
                "INSERT INTO activity_entry_owners (
                   scope_id, owner_kind, owner_id, entry_count
                 ) VALUES (?, 'actor', 'actor:detail-owner', ?)",
                rusqlite::params![scope_id, entries.len() as i64],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .expect("oversized detail fixture");

    let first = repository
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:detail-owner",
            None,
            PAGE_LIMIT,
        )
        .await
        .expect("first detail page");
    assert_eq!(first.entries.len(), PAGE_LIMIT);
    let cursor = first.next_cursor.expect("detail page cursor");
    let second = repository
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:detail-owner",
            Some(&cursor),
            PAGE_LIMIT,
        )
        .await
        .expect("second detail page");
    assert_eq!(second.entries.len(), 50);
    assert!(second.next_cursor.is_none());
    assert!(first.entries.iter().all(|first_entry| {
        second
            .entries
            .iter()
            .all(|second_entry| first_entry.id != second_entry.id)
    }));
}

async fn assert_rpc_roster_pages<S>(
    socket: &mut WebSocketStream<S>,
    first_request_id: &str,
    second_request_id: &str,
    scope: &ActivityScopeSeed,
    section: &str,
    bucket: &str,
    expected_second_page_length: usize,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let scope_ref = json!({ "_tag": "thread", "threadId": "activity-rpc-load" });
    let first = unary(
        socket,
        first_request_id,
        "activity.listRoster",
        json!({
            "scope": scope_ref,
            "scopeId": scope.scope_id.clone(),
            "section": section,
            "bucket": bucket,
            "limit": PAGE_LIMIT
        }),
    )
    .await
    .expect("first roster page");
    let first_records = first["records"].as_array().expect("first roster records");
    assert_eq!(first_records.len(), PAGE_LIMIT);
    assert_descending_roster_order(first_records);
    let cursor = first["nextCursor"].as_str().expect("first roster cursor");

    let second = unary(
        socket,
        second_request_id,
        "activity.listRoster",
        json!({
            "scope": scope_ref,
            "scopeId": scope.scope_id.clone(),
            "section": section,
            "bucket": bucket,
            "cursor": cursor,
            "limit": PAGE_LIMIT
        }),
    )
    .await
    .expect("second roster page");
    let second_records = second["records"].as_array().expect("second roster records");
    assert_eq!(second_records.len(), expected_second_page_length);
    assert_descending_roster_order(second_records);
    assert!(second["nextCursor"].is_null());
    assert!(first_records.iter().all(|first_record| {
        second_records
            .iter()
            .all(|second_record| first_record["id"] != second_record["id"])
    }));
}

fn assert_descending_roster_order(records: &[Value]) {
    for pair in records.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        let left_updated_at = left["updatedAt"].as_str().expect("left updatedAt");
        let right_updated_at = right["updatedAt"].as_str().expect("right updatedAt");
        let left_id = left["id"].as_str().expect("left id");
        let right_id = right["id"].as_str().expect("right id");
        assert!(
            left_updated_at > right_updated_at
                || (left_updated_at == right_updated_at && left_id > right_id),
            "roster order must be updatedAt DESC, id DESC: {left:?} then {right:?}"
        );
    }
}

struct RpcFixture {
    _directory: TempDir,
    database: Database,
    projection: ActivityProjection,
    handle: coddy_studio_server::ServerHandle,
}

impl RpcFixture {
    async fn start(capacity: usize) -> Self {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let controller = AgentActivityController::new(true);
        let projections = ActivityProjections::with_capacity(
            ActivityRepository::new(database.clone()),
            controller.clone(),
            AgentActivityController::new(true),
            capacity,
        );
        let projection = projections.chat();
        let mut registry = RpcRegistry::empty();
        register_activity_rpc_for_integration_test(&mut registry, projections);
        let directory = tempfile::tempdir().expect("server directory");
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(directory.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            registry,
        )
        .await
        .expect("server");
        Self {
            _directory: directory,
            database,
            projection,
            handle,
        }
    }

    async fn connect(
        &self,
    ) -> WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        connect_async(format!("ws://{}/ws", self.handle.local_addr()))
            .await
            .expect("websocket")
            .0
    }

    async fn shutdown(self) {
        self.handle.shutdown();
        self.handle.join().await.expect("server joins");
    }
}

async fn request<S>(socket: &mut WebSocketStream<S>, id: &str, tag: &str, payload: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": payload,
                "headers": []
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("request");
}

async fn ack<S>(socket: &mut WebSocketStream<S>, request_id: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({ "_tag": "Ack", "requestId": request_id })
                .to_string()
                .into(),
        ))
        .await
        .expect("ack");
}

async fn unary<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
) -> Result<Value, Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    request(socket, id, tag, payload).await;
    match next_message(socket).await {
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Success { value: Some(value) },
            ..
        } => Ok(value),
        ServerMessage::Exit {
            exit: coddy_studio_server::RpcExit::Failure { cause },
            ..
        } => Err(cause
            .into_iter()
            .find_map(|item| match item {
                coddy_studio_server::CauseItem::Fail { error } => Some(error),
                _ => None,
            })
            .expect("RPC error")),
        message => panic!("unexpected unary message: {message:?}"),
    }
}

async fn next_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(3), next_frame_past_heartbeat(socket))
        .await
        .expect("server message timeout")
        .expect("websocket open")
        .expect("websocket frame");
    let Message::Text(text) = frame else {
        panic!("unexpected websocket frame: {frame:?}");
    };
    serde_json::from_str(&text).expect("server message")
}

async fn retention_envelope(database: &Database, scope_id: &str) -> (i64, i64, i64, i64, i64, i64) {
    let scope_id = scope_id.to_owned();
    database
        .call(move |connection| {
            Ok((
                connection.query_row(
                    "SELECT COUNT(*), MIN(revision), MAX(revision)
                     FROM activity_journal WHERE scope_id = ?",
                    [&scope_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
                connection.query_row(
                    "SELECT COUNT(*), MIN(revision), MAX(revision)
                     FROM activity_event_idempotency WHERE scope_id = ?",
                    [&scope_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?,
            ))
        })
        .await
        .map(
            |((journal_rows, journal_min, journal_max), (event_rows, event_min, event_max))| {
                (
                    journal_rows,
                    journal_min,
                    journal_max,
                    event_rows,
                    event_min,
                    event_max,
                )
            },
        )
        .expect("retention envelope")
}
