//! Ordered terminal input against a persisted workspace, over a real socket.
//!
//! `terminal.beginInput` resolves the thread's workspace from persistence and keeps that
//! admission for the input session; `terminal.writeInput` must then never wait for the
//! database (#82), while workspace loss still seals the session.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::SinkExt as _;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

use super::{
    JsonFuture, JsonStream, ProductionServerControl, ServerTerminalServices,
    register_server_terminal_rpc,
};
use crate::{
    CauseItem, RpcExit, RpcRegistry, RpcResult, RpcStreamChunk, ServerConfig, ServerHandle,
    ServerMessage, ServerRuntime,
    cloud::{RelayClientInstallEvent, RelayClientService, RelayClientStatus},
    diagnostics::{
        DiagnosticsMonitor, NativeProcessSampler, NativeResourceSampler,
        NotApplicableUiProcessObserver, ProcessAttributionRegistry, ProcessIdentity,
    },
    persistence::{
        Database, NewOrchestrationEvent, OrchestrationEvent, ProjectionProject, ProjectionThread,
        Repositories, run_migrations,
    },
    provider_usage::ProviderUsageService,
    terminal::{PtyBackend, PtyExit, PtyProcess, PtySpawnInput, TerminalManager},
    worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
    },
};

const THREAD: &str = "thread-input";
const TERMINAL: &str = "term";
const DEADLINE: Duration = Duration::from_secs(5);

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
struct RecordingPty {
    identity: ProcessIdentity,
    writes: Mutex<Vec<String>>,
    output: broadcast::Sender<String>,
    exit: watch::Sender<Option<PtyExit>>,
}

impl PtyProcess for RecordingPty {
    fn pid(&self) -> u32 {
        self.identity.pid
    }

    fn process_identity(&self) -> Option<ProcessIdentity> {
        Some(self.identity)
    }

    fn write(&self, data: &str) -> Result<(), String> {
        self.writes.lock().expect("writes").push(data.to_owned());
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

    fn wait_for_process_tree_exit(&self, _timeout: Duration) -> Result<Option<bool>, String> {
        Ok(Some(true))
    }

    fn subscribe_output(&self) -> broadcast::Receiver<String> {
        self.output.subscribe()
    }

    fn subscribe_exit(&self) -> watch::Receiver<Option<PtyExit>> {
        self.exit.subscribe()
    }
}

#[derive(Debug, Default)]
struct RecordingBackend {
    process: Mutex<Option<Arc<RecordingPty>>>,
}

impl RecordingBackend {
    fn writes(&self) -> Vec<String> {
        self.process
            .lock()
            .expect("process")
            .as_ref()
            .map(|process| process.writes.lock().expect("writes").clone())
            .unwrap_or_default()
    }
}

impl PtyBackend for RecordingBackend {
    fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let (output, _) = broadcast::channel(8);
        let (exit, _) = watch::channel(None);
        let process = Arc::new(RecordingPty {
            identity: ProcessIdentity {
                pid: 52_525,
                started_at: NEXT.fetch_add(1, Ordering::Relaxed),
            },
            writes: Mutex::new(Vec::new()),
            output,
            exit,
        });
        *self.process.lock().expect("process") = Some(process.clone());
        Ok(process)
    }
}

#[derive(Debug)]
struct NullControl;

impl ProductionServerControl for NullControl {
    fn call(
        &self,
        _method: &'static str,
        _payload: Value,
        _cancellation: CancellationToken,
    ) -> JsonFuture {
        Box::pin(async { Ok(Value::Null) as RpcResult })
    }

    fn subscribe(&self, _method: &'static str, _cancellation: CancellationToken) -> JsonStream {
        mpsc::channel::<RpcStreamChunk>(1).1
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    /// The temporary directory's physical path, under which every workspace is made.
    root: PathBuf,
    project_root: PathBuf,
    worktree: PathBuf,
    repositories: Repositories,
    registry: WorkspaceAvailabilityRegistry,
    services: ServerTerminalServices,
    backend: Arc<RecordingBackend>,
    handle: ServerHandle,
    socket: Socket,
    next_id: u64,
}

impl Fixture {
    async fn start() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        // Physical paths, as the availability registry keys its admissions: on macOS the
        // temporary directory sits under the `/var` -> `/private/var` symlink.
        let root = temp
            .path()
            .canonicalize()
            .expect("physical temporary directory");
        let project_root = root.join("project");
        let worktree = root.join("worktree");
        std::fs::create_dir_all(&project_root).expect("project root");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let repositories = Repositories::new(database);
        repositories
            .upsert_project(project_row("project-input", &project_root))
            .await
            .expect("project");
        let fixture_thread = thread_row(None);
        repositories
            .upsert_thread(fixture_thread)
            .await
            .expect("thread");
        let registry = WorkspaceAvailabilityRegistry::new();
        let backend = Arc::new(RecordingBackend::default());
        let manager = TerminalManager::new(backend.clone(), Default::default());
        let services =
            services(manager).with_workspace_admission(registry.clone(), repositories.clone());
        let mut rpc = RpcRegistry::empty();
        register_server_terminal_rpc(&mut rpc, services.clone());
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(temp.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            rpc,
        )
        .await
        .expect("server starts");
        let socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket connects")
            .0;
        let mut fixture = Self {
            _temp: temp,
            root,
            project_root,
            worktree,
            repositories,
            registry,
            services,
            backend,
            handle,
            socket,
            next_id: 0,
        };
        let cwd = fixture.project_root.to_string_lossy().into_owned();
        success(
            fixture
                .request(
                    "terminal.open",
                    json!({"threadId": THREAD, "terminalId": TERMINAL, "cwd": cwd, "env": {}}),
                )
                .await,
        );
        fixture
    }

    async fn begin_input(&mut self, attachment_sequence: u64) -> String {
        success(
            self.request(
                "terminal.beginInput",
                json!({"threadId": THREAD, "terminalId": TERMINAL, "attachmentSequence": attachment_sequence}),
            )
            .await,
        )["inputId"]
            .as_str()
            .expect("input id")
            .to_owned()
    }

    fn frame(input_id: &str, sequence: u64, data: &str) -> Value {
        json!({
            "threadId": THREAD,
            "terminalId": TERMINAL,
            "inputId": input_id,
            "sequence": sequence,
            "data": data,
        })
    }

    async fn send(&mut self, tag: &str, payload: Value) -> String {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let message =
            json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .expect("request sends");
        id
    }

    async fn reply(&mut self) -> ServerMessage {
        let frame = tokio::time::timeout(
            DEADLINE,
            crate::test_support::websocket_frames::next_frame_past_heartbeat(&mut self.socket),
        )
        .await
        .expect("reply before the deadline")
        .expect("socket stays open")
        .expect("valid frame");
        let Message::Text(text) = frame else {
            panic!("expected a text frame, got {frame:?}");
        };
        serde_json::from_str(&text).expect("server message")
    }

    async fn request(&mut self, tag: &str, payload: Value) -> ServerMessage {
        self.send(tag, payload).await;
        self.reply().await
    }

    fn loss(&self, path: &Path) -> WorkspaceLossTransition {
        WorkspaceLossTransition {
            thread_id: THREAD.to_owned(),
            repository_key: "repository-input".to_owned(),
            generation: 1,
            path: path.to_path_buf(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        }
    }

    async fn shutdown(mut self) {
        let _ = self.socket.close(None).await;
        self.handle.shutdown();
        self.handle.join().await.expect("server joins");
    }
}

fn project_row(project_id: &str, workspace_root: &Path) -> ProjectionProject {
    ProjectionProject {
        project_id: project_id.to_owned(),
        title: "project".to_owned(),
        workspace_root: workspace_root.to_string_lossy().into_owned(),
        default_model_selection: None,
        scripts: json!([]),
        worktree_discovery: json!({}),
        worktree_repository_key: None,
        created_at: "2026-10-12T00:00:00Z".to_owned(),
        updated_at: "2026-10-12T00:00:00Z".to_owned(),
        deleted_at: None,
        repository_identity: None,
    }
}

fn thread_row(worktree_path: Option<String>) -> ProjectionThread {
    thread_row_in(THREAD, "project-input", worktree_path)
}

fn thread_row_in(
    thread_id: &str,
    project_id: &str,
    worktree_path: Option<String>,
) -> ProjectionThread {
    ProjectionThread {
        thread_id: thread_id.to_owned(),
        project_id: project_id.to_owned(),
        title: "thread".to_owned(),
        kind: "workspace".to_owned(),
        model_selection: json!({}),
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        branch: None,
        worktree_path,
        latest_turn_id: None,
        created_at: "2026-10-12T00:00:00Z".to_owned(),
        updated_at: "2026-10-12T00:00:00Z".to_owned(),
        archived_at: None,
        latest_user_message_at: None,
        pending_approval_count: 0,
        pending_user_input_count: 0,
        has_actionable_proposed_plan: 0,
        unresolved_delivery_state: None,
        unresolved_delivery_detail: None,
        deleted_at: None,
        host_thread_id: None,
    }
}

fn services(manager: TerminalManager) -> ServerTerminalServices {
    let sampler = Arc::new(NativeProcessSampler::default());
    let resource_sampler = Arc::new(NativeResourceSampler::new(
        sampler.clone(),
        ProcessAttributionRegistry::new(),
        Arc::new(NotApplicableUiProcessObserver),
    ));
    let monitor = Arc::new(DiagnosticsMonitor::new(
        resource_sampler.clone(),
        Duration::from_secs(60),
    ));
    let usage = ProviderUsageService::new(Vec::new(), Arc::new(time::OffsetDateTime::now_utc));
    let relay = RelayClientService::new(
        || async {
            RelayClientStatus::Missing {
                version: "1.0.0".to_owned(),
            }
        },
        |_report: Arc<dyn Fn(RelayClientInstallEvent) -> _ + Send + Sync>| async {
            Ok(RelayClientStatus::Missing {
                version: "1.0.0".to_owned(),
            })
        },
    );
    ServerTerminalServices::new(
        manager,
        sampler,
        resource_sampler,
        monitor,
        usage,
        relay,
        Arc::new(NullControl),
    )
}

fn success(message: ServerMessage) -> Value {
    match message {
        ServerMessage::Exit {
            exit: RpcExit::Success { value },
            ..
        } => value.unwrap_or(Value::Null),
        other => panic!("expected success, got {other:?}"),
    }
}

fn failure(message: ServerMessage) -> Value {
    match message {
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } => match cause.as_slice() {
            [CauseItem::Fail { error }] => error.clone(),
            other => panic!("expected one failure, got {other:?}"),
        },
        other => panic!("expected failure, got {other:?}"),
    }
}

/// Occupies the single SQLite worker until the returned sender fires or `hold` elapses.
async fn park_database(repositories: &Repositories, hold: Duration) -> tokio::task::JoinHandle<()> {
    let (started, parked) = tokio::sync::oneshot::channel();
    let repositories = repositories.clone();
    let task = tokio::spawn(async move {
        repositories
            .database()
            .call(move |_connection| {
                let _ = started.send(());
                std::thread::sleep(hold);
                Ok(())
            })
            .await
            .expect("parked database call");
    });
    parked.await.expect("database worker is parked");
    task
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn write_input_does_not_wait_for_a_busy_database_once_input_begins() {
    let mut fixture = Fixture::start().await;
    let input = fixture.begin_input(0).await;
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "a"))
            .await,
    );

    let parked = park_database(&fixture.repositories, Duration::from_millis(1_500)).await;
    let started = Instant::now();
    let reply = tokio::time::timeout(
        Duration::from_millis(500),
        fixture.request("terminal.writeInput", Fixture::frame(&input, 1, "b")),
    )
    .await
    .expect("a keystroke is acknowledged while the database worker is busy");
    let elapsed = started.elapsed();
    success(reply);
    assert!(
        elapsed < Duration::from_millis(500),
        "writeInput waited {elapsed:?} for the database"
    );
    assert_eq!(fixture.backend.writes(), ["a", "b"]);
    parked.await.expect("parked call finishes");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_loss_cancels_pending_and_later_input_and_releases_the_admission() {
    let mut fixture = Fixture::start().await;
    let input = fixture.begin_input(0).await;
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "a"))
            .await,
    );
    // Sequence 2 waits for the missing sequence 1, so it is in flight when the loss lands.
    fixture
        .send("terminal.writeInput", Fixture::frame(&input, 2, "late"))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let loss = fixture.loss(&fixture.project_root);
    assert!(
        fixture
            .registry
            .mark_unavailable(loss.clone())
            .await
            .expect("physical identity resolves")
    );

    let pending = failure(fixture.reply().await);
    assert_eq!(pending["_tag"], "WorkspaceUnavailableError", "{pending}");
    assert_eq!(pending["threadId"], THREAD);
    let later = failure(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 1, "b"))
            .await,
    );
    assert!(
        later["_tag"] == "WorkspaceUnavailableError" || later["_tag"] == "TerminalInputError",
        "{later}"
    );
    assert_eq!(fixture.backend.writes(), ["a"], "no input after the loss");
    tokio::time::timeout(
        Duration::from_secs(1),
        fixture.registry.wait_for_transition_admissions(&loss),
    )
    .await
    .expect("the input session releases its workspace admission after the loss");
    let restarted = failure(
        fixture
            .request(
                "terminal.beginInput",
                json!({"threadId": THREAD, "terminalId": TERMINAL, "attachmentSequence": 1}),
            )
            .await,
    );
    assert_eq!(
        restarted["_tag"], "WorkspaceUnavailableError",
        "{restarted}"
    );
    fixture.shutdown().await;
}

fn orchestration_event(event_type: &str, aggregate_id: &str, payload: Value) -> OrchestrationEvent {
    OrchestrationEvent {
        sequence: 1,
        event: NewOrchestrationEvent {
            event_id: format!("event-{event_type}"),
            event_type: event_type.to_owned(),
            aggregate_kind: "thread".to_owned(),
            aggregate_id: aggregate_id.to_owned(),
            occurred_at: "2026-10-12T00:00:00Z".to_owned(),
            command_id: None,
            causation_event_id: None,
            correlation_id: None,
            payload,
            metadata: json!({}),
        },
    }
}

/// Whether some admission covers `path` (an admission scope other than this thread).
fn admits(fixture: &Fixture, path: &Path) -> bool {
    fixture
        .registry
        .has_transition_admissions(&WorkspaceLossTransition {
            thread_id: "thread-probe".to_owned(),
            ..fixture.loss(path)
        })
}

/// Waits until the input session holds an admission of `admitted` and none of `released`.
async fn wait_for_admission_of(fixture: &Fixture, admitted: &Path, released: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !admits(fixture, admitted) || admits(fixture, released) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the input session follows the thread's workspace");
}

async fn move_thread_to_worktree(fixture: &Fixture) -> String {
    let worktree = fixture.worktree.to_string_lossy().into_owned();
    fixture
        .repositories
        .upsert_thread(thread_row(Some(worktree.clone())))
        .await
        .expect("thread moves to its worktree");
    worktree
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_workspace_is_readmitted_without_interrupting_input() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    let input = fixture.begin_input(0).await;
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "a"))
            .await,
    );
    let worktree = move_thread_to_worktree(&fixture).await;
    events
        .send(orchestration_event(
            "thread.meta-updated",
            THREAD,
            json!({"threadId": THREAD, "worktreePath": worktree}),
        ))
        .expect("event delivered");
    wait_for_admission_of(&fixture, &fixture.worktree, &fixture.project_root).await;
    // The same session keeps typing, in order, with no new begin.
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 1, "b"))
            .await,
    );

    // A moved project root may change any thread's workspace; the session stays live.
    events
        .send(orchestration_event(
            "project.meta-updated",
            "project-other",
            json!({"projectId": "project-other", "workspaceRoot": "/elsewhere"}),
        ))
        .expect("event delivered");
    events
        .send(orchestration_event(
            "thread.meta-updated",
            THREAD,
            json!({"threadId": THREAD, "title": "renamed"}),
        ))
        .expect("event delivered");
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 2, "c"))
            .await,
    );
    assert_eq!(fixture.backend.writes(), ["a", "b", "c"]);

    // Another thread's removal of the new workspace reaches the session through its path.
    let removal = fixture
        .registry
        .mark_removing("thread-other", &fixture.worktree)
        .await
        .expect("physical identity resolves");
    let removed = failure(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 3, "d"))
            .await,
    );
    assert!(
        removed["_tag"] == "WorkspaceUnavailableError" || removed["_tag"] == "TerminalInputError",
        "{removed}"
    );
    assert_eq!(fixture.backend.writes(), ["a", "b", "c"]);
    tokio::time::timeout(
        Duration::from_secs(1),
        fixture
            .registry
            .wait_for_removal_admissions(&removal.identity()),
    )
    .await
    .expect("the input session releases its admission for the removal");
    drop(removal);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_changed_workspace_that_cannot_be_admitted_seals_input() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    let input = fixture.begin_input(0).await;
    let removal = fixture
        .registry
        .mark_removing("thread-other", &fixture.worktree)
        .await
        .expect("physical identity resolves");
    let worktree = move_thread_to_worktree(&fixture).await;
    events
        .send(orchestration_event(
            "thread.meta-updated",
            THREAD,
            json!({"threadId": THREAD, "worktreePath": worktree}),
        ))
        .expect("event delivered");
    // The session is sealed and releases its admission of the old workspace.
    tokio::time::timeout(
        Duration::from_secs(1),
        fixture
            .registry
            .wait_for_transition_admissions(&fixture.loss(&fixture.project_root)),
    )
    .await
    .expect("the sealed session releases its admission");
    let sealed = failure(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "lost"))
            .await,
    );
    assert_eq!(sealed["_tag"], "TerminalInputError", "{sealed}");
    assert!(fixture.backend.writes().is_empty());
    // Reconnecting reports why.
    let unavailable = failure(
        fixture
            .request(
                "terminal.beginInput",
                json!({"threadId": THREAD, "terminalId": TERMINAL, "attachmentSequence": 1}),
            )
            .await,
    );
    assert_eq!(
        unavailable["_tag"], "WorkspaceUnavailableError",
        "{unavailable}"
    );
    drop(removal);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_change_while_input_begins_is_readmitted() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    // Hold the worker so the begin reads the old workspace, then the change commits and is
    // announced before the session registers.
    let parked = park_database(&fixture.repositories, Duration::from_millis(600)).await;
    let begin = fixture
        .send(
            "terminal.beginInput",
            json!({"threadId": THREAD, "terminalId": TERMINAL, "attachmentSequence": 0}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let moved = {
        let repositories = fixture.repositories.clone();
        let worktree = fixture.worktree.to_string_lossy().into_owned();
        tokio::spawn(async move {
            repositories
                .upsert_thread(thread_row(Some(worktree)))
                .await
                .expect("thread moves to its worktree");
        })
    };
    events
        .send(orchestration_event(
            "thread.meta-updated",
            THREAD,
            json!({"threadId": THREAD, "worktreePath": fixture.worktree}),
        ))
        .expect("event delivered");
    let reply = fixture.reply().await;
    let ServerMessage::Exit { request_id, .. } = &reply else {
        panic!("expected the begin reply, got {reply:?}");
    };
    assert_eq!(request_id.as_str(), begin);
    let input = success(reply)["inputId"]
        .as_str()
        .expect("input id")
        .to_owned();
    parked.await.expect("parked call finishes");
    moved.await.expect("thread moved");
    wait_for_admission_of(&fixture, &fixture.worktree, &fixture.project_root).await;
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "a"))
            .await,
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_waits_for_a_pending_readmission_and_never_writes_to_a_refused_workspace() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    let input = fixture.begin_input(0).await;
    let removal = fixture
        .registry
        .mark_removing("thread-other", &fixture.worktree)
        .await
        .expect("physical identity resolves");
    let worktree = move_thread_to_worktree(&fixture).await;
    // The re-admission resolves on a busy worker while the user keeps typing.
    let parked = park_database(&fixture.repositories, Duration::from_millis(400)).await;
    events
        .send(orchestration_event(
            "thread.meta-updated",
            THREAD,
            json!({"threadId": THREAD, "worktreePath": worktree}),
        ))
        .expect("event delivered");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let refused = failure(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "typed"))
            .await,
    );
    assert!(
        refused["_tag"] == "WorkspaceUnavailableError" || refused["_tag"] == "TerminalInputError",
        "{refused}"
    );
    assert!(
        fixture.backend.writes().is_empty(),
        "nothing is written while the new workspace is being resolved and refused"
    );
    parked.await.expect("parked call finishes");
    drop(removal);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_begin_whose_changed_workspace_cannot_be_admitted_reports_why() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    let removal = fixture
        .registry
        .mark_removing("thread-other", &fixture.worktree)
        .await
        .expect("physical identity resolves");
    let parked = park_database(&fixture.repositories, Duration::from_millis(600)).await;
    fixture
        .send(
            "terminal.beginInput",
            json!({"threadId": THREAD, "terminalId": TERMINAL, "attachmentSequence": 0}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let moved = {
        let repositories = fixture.repositories.clone();
        let worktree = fixture.worktree.to_string_lossy().into_owned();
        tokio::spawn(async move {
            repositories
                .upsert_thread(thread_row(Some(worktree)))
                .await
                .expect("thread moves to its worktree");
        })
    };
    events
        .send(orchestration_event(
            "thread.meta-updated",
            THREAD,
            json!({"threadId": THREAD, "worktreePath": fixture.worktree}),
        ))
        .expect("event delivered");
    let refused = failure(fixture.reply().await);
    assert_eq!(refused["_tag"], "WorkspaceUnavailableError", "{refused}");
    parked.await.expect("parked call finishes");
    moved.await.expect("thread moved");
    drop(removal);
    fixture.shutdown().await;
}

/// Opens a terminal for `thread_id` in `cwd` and begins its input; returns the input id.
async fn begin_thread_input(fixture: &mut Fixture, thread_id: &str, cwd: &Path) -> String {
    success(
        fixture
            .request(
                "terminal.open",
                json!({"threadId": thread_id, "terminalId": TERMINAL, "cwd": cwd, "env": {}}),
            )
            .await,
    );
    success(
        fixture
            .request(
                "terminal.beginInput",
                json!({"threadId": thread_id, "terminalId": TERMINAL, "attachmentSequence": 0}),
            )
            .await,
    )["inputId"]
        .as_str()
        .expect("input id")
        .to_owned()
}

fn project_moved(project_id: &str, workspace_root: &Path) -> OrchestrationEvent {
    let mut event = orchestration_event(
        "project.meta-updated",
        project_id,
        json!({"projectId": project_id, "workspaceRoot": workspace_root}),
    );
    event.event.aggregate_kind = "project".to_owned();
    event
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_project_root_leaves_other_projects_and_own_worktrees_alone() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    let other_root = fixture.root.join("other-project");
    std::fs::create_dir_all(&other_root).expect("other project root");
    let worktree = fixture.worktree.clone();
    fixture
        .repositories
        .upsert_project(project_row("project-other", &other_root))
        .await
        .expect("other project");
    for (thread_id, project_id, worktree_path) in [
        ("thread-other-project", "project-other", None),
        (
            "thread-own-worktree",
            "project-input",
            Some(worktree.to_string_lossy().into_owned()),
        ),
    ] {
        fixture
            .repositories
            .upsert_thread(thread_row_in(thread_id, project_id, worktree_path))
            .await
            .expect("thread");
    }
    let other = begin_thread_input(&mut fixture, "thread-other-project", &other_root).await;
    let own = begin_thread_input(&mut fixture, "thread-own-worktree", &worktree).await;

    // Re-admitting either session would wait for this busy worker.
    let parked = park_database(&fixture.repositories, Duration::from_millis(800)).await;
    let moved_root = fixture.root.join("moved-project");
    events
        .send(project_moved("project-input", &moved_root))
        .expect("event delivered");
    tokio::time::sleep(Duration::from_millis(50)).await;
    for (thread_id, input_id) in [
        ("thread-other-project", &other),
        ("thread-own-worktree", &own),
    ] {
        let reply = tokio::time::timeout(
            Duration::from_millis(300),
            fixture.request(
                "terminal.writeInput",
                json!({
                    "threadId": thread_id, "terminalId": TERMINAL, "inputId": input_id,
                    "sequence": 0, "data": "k",
                }),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{thread_id} input waited for another project's root"));
        success(reply);
    }
    parked.await.expect("parked call finishes");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_project_root_readmits_the_threads_that_inherit_it() {
    let mut fixture = Fixture::start().await;
    let (events, receiver) = broadcast::channel(16);
    fixture.services.watch_workspace_changes(receiver);
    let input = fixture.begin_input(0).await;
    let moved_root = fixture.root.join("moved-project");
    std::fs::create_dir_all(&moved_root).expect("moved root");
    fixture
        .repositories
        .upsert_project(project_row("project-input", &moved_root))
        .await
        .expect("project root moves");
    events
        .send(project_moved("project-input", &moved_root))
        .expect("event delivered");
    wait_for_admission_of(&fixture, &moved_root, &fixture.project_root).await;
    success(
        fixture
            .request("terminal.writeInput", Fixture::frame(&input, 0, "a"))
            .await,
    );
    assert_eq!(fixture.backend.writes(), ["a"]);
    fixture.shutdown().await;
}

/// Measures keystroke round trips over a real authenticated WebSocket while the SQLite worker or
/// the connection's outbound lane is busy (#82). Prints percentiles; asserts nothing.
///
/// `CODDY_STUDIO_MEASURE_AUTH=0` runs the same session unauthenticated, isolating the per-request
/// session refresh the read loop performs before it spawns a handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement; run with --ignored --nocapture"]
async fn measure_keystroke_round_trips_under_database_and_outbound_load() {
    use axum::{Router, extract::ws::WebSocketUpgrade, routing::get};

    use crate::{
        auth::{AuthService, ClientMetadata, SecretStore, SessionTransport},
        rpc::{RpcSessionContext, run_session},
    };

    let authenticated =
        std::env::var("CODDY_STUDIO_MEASURE_AUTH").map_or(true, |value| value != "0");
    let mut fixture = Fixture::start().await;
    let temp = tempfile::tempdir().expect("auth state");
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_desktop("measure-seed")
        .expect("desktop config");
    let auth = AuthService::new_with_persistence(
        &config,
        vec![7_u8; 32],
        SecretStore::new(temp.path().join("secrets"))
            .await
            .expect("secret store"),
        fixture.repositories.clone(),
    )
    .await
    .expect("auth service");
    let issued = auth
        .exchange_bootstrap(
            "measure-seed",
            None,
            ClientMetadata::default(),
            None,
            SessionTransport::Plain,
        )
        .await
        .expect("desktop session");

    let mut rpc = RpcRegistry::empty();
    register_server_terminal_rpc(&mut rpc, fixture.services.clone());
    let chunk_bytes = std::env::var("CODDY_STUDIO_MEASURE_CHUNK_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1024 * 1024);
    // A subscription that always has one large chunk ready, like a busy VCS-status or
    // shell-snapshot stream. The method name only borrows a declared scope.
    rpc.register_stream("subscribeServerConfig", move |_request, cancellation| {
        let (sender, receiver) = mpsc::channel(1);
        tokio::spawn(async move {
            let payload = "x".repeat(chunk_bytes);
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    sent = sender.send(Ok(vec![json!({"type":"bulk","payload": payload})])) => {
                        if sent.is_err() { return; }
                    }
                }
            }
        });
        receiver
    });
    let shutdown = CancellationToken::new();
    let principal = issued.principal.clone();
    let session_auth = auth.clone();
    let session_shutdown = shutdown.clone();
    let app = Router::new().route(
        "/ws",
        get(move |upgrade: WebSocketUpgrade| {
            let rpc = rpc.clone();
            let context = if authenticated {
                RpcSessionContext::authenticated(principal.clone(), session_auth.clone())
            } else {
                RpcSessionContext::unauthenticated()
            };
            let shutdown = session_shutdown.child_token();
            async move {
                upgrade
                    .max_message_size(64 * 1024 * 1024)
                    .on_upgrade(move |socket| run_session(socket, rpc, context, shutdown))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    fixture.socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("WebSocket connects")
        .0;
    let cwd = fixture.project_root.to_string_lossy().into_owned();
    success(
        fixture
            .request(
                "terminal.open",
                json!({"threadId": THREAD, "terminalId": "measured", "cwd": cwd, "env": {}}),
            )
            .await,
    );
    let input = success(
        fixture
            .request(
                "terminal.beginInput",
                json!({"threadId": THREAD, "terminalId": "measured", "attachmentSequence": 0}),
            )
            .await,
    )["inputId"]
        .as_str()
        .expect("input id")
        .to_owned();
    let frame = |sequence: u64| json!({"threadId": THREAD, "terminalId": "measured", "inputId": input, "sequence": sequence, "data": "k"});
    let samples = 40_u64;
    let mut sequence = 0_u64;

    // Reads until `id`'s reply, acknowledging bulk chunks so the stream keeps one chunk queued.
    async fn round_trip(fixture: &mut Fixture, tag: &str, payload: Value) -> Duration {
        let started = Instant::now();
        let id = fixture.send(tag, payload).await;
        loop {
            let frame = tokio::time::timeout(
                Duration::from_secs(30),
                crate::test_support::websocket_frames::next_frame_past_heartbeat(
                    &mut fixture.socket,
                ),
            )
            .await
            .expect("frame")
            .expect("open")
            .expect("valid");
            let Message::Text(text) = frame else { continue };
            let value: Value = serde_json::from_str(&text).expect("json");
            if value["_tag"] == "Chunk" {
                let ack = json!({"_tag": "Ack", "requestId": value["requestId"]});
                fixture
                    .socket
                    .send(Message::Text(ack.to_string().into()))
                    .await
                    .expect("ack");
                continue;
            }
            if value["_tag"] == "Exit" && value["requestId"] == id.as_str() {
                assert_eq!(value["exit"]["_tag"], "Success", "{value}");
                return started.elapsed();
            }
        }
    }

    fn report(label: &str, mut samples: Vec<Duration>) {
        samples.sort_unstable();
        let at = |q: f64| samples[((samples.len() as f64 - 1.0) * q).round() as usize];
        println!(
            "{label}: n={} p50={:.1}ms p90={:.1}ms max={:.1}ms",
            samples.len(),
            at(0.5).as_secs_f64() * 1e3,
            at(0.9).as_secs_f64() * 1e3,
            at(1.0).as_secs_f64() * 1e3,
        );
    }

    let mut idle = Vec::new();
    for _ in 0..samples {
        idle.push(round_trip(&mut fixture, "terminal.writeInput", frame(sequence)).await);
        sequence += 1;
    }
    report("idle", idle);

    // Back-to-back heavy queries keep the single SQLite worker busy.
    let busy = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let query_ms = Arc::new(Mutex::new(Vec::new()));
    let load = {
        let busy = busy.clone();
        let query_ms = query_ms.clone();
        let repositories = fixture.repositories.clone();
        tokio::spawn(async move {
            while busy.load(Ordering::Acquire) {
                let started = Instant::now();
                repositories
                    .database()
                    .call(|connection| {
                        let sum: i64 = connection.query_row(
                            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 3000000) SELECT sum(x) FROM c",
                            [],
                            |row| row.get(0),
                        )?;
                        Ok(sum)
                    })
                    .await
                    .expect("heavy query");
                query_ms
                    .lock()
                    .expect("query timings")
                    .push(started.elapsed());
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut database_busy = Vec::new();
    for _ in 0..samples {
        database_busy.push(round_trip(&mut fixture, "terminal.writeInput", frame(sequence)).await);
        sequence += 1;
    }
    busy.store(false, Ordering::Release);
    load.await.expect("load stops");
    report(
        "heavy query (the load itself)",
        query_ms.lock().expect("timings").clone(),
    );
    report("writeInput while the database is busy", database_busy);

    let bulk = fixture.send("subscribeServerConfig", json!({})).await;
    let mut outbound_busy = Vec::new();
    for _ in 0..samples {
        outbound_busy.push(round_trip(&mut fixture, "terminal.writeInput", frame(sequence)).await);
        sequence += 1;
    }
    report(
        &format!("writeInput behind a {chunk_bytes}-byte subscription chunk stream"),
        outbound_busy,
    );
    let interrupt = json!({"_tag": "Interrupt", "requestId": bulk});
    let _ = fixture
        .socket
        .send(Message::Text(interrupt.to_string().into()))
        .await;
    println!("authenticated={authenticated}");
    shutdown.cancel();
    server.abort();
}
