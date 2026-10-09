#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    panic::AssertUnwindSafe,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{FutureExt, SinkExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

use bibcode_server::production::server_terminal::{
    JsonFuture, JsonStream, ProductionServerControl, ServerTerminalServices,
    register_server_terminal_rpc,
};
use bibcode_server::{
    CauseItem, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime, cloud,
    diagnostics, provider_usage,
    terminal::{self, PtyBackend, PtyExit, PtyProcess, PtySpawnInput},
    worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
    },
};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const TERMINAL_RPC_INTEGRATION_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct GateTestPty {
    identity: diagnostics::ProcessIdentity,
    killed: AtomicBool,
    writes: std::sync::Mutex<Vec<String>>,
    output: tokio::sync::broadcast::Sender<String>,
    exit: tokio::sync::watch::Sender<Option<PtyExit>>,
}

impl GateTestPty {
    fn new() -> Self {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        assert_ne!(generation, 0, "synthetic PTY generation wrapped to zero");
        let (output, _) = tokio::sync::broadcast::channel(8);
        let (exit, _) = tokio::sync::watch::channel(None);
        Self {
            identity: diagnostics::ProcessIdentity {
                pid: 42_424,
                started_at: generation,
            },
            killed: AtomicBool::new(false),
            writes: std::sync::Mutex::new(Vec::new()),
            output,
            exit,
        }
    }
}

impl PtyProcess for GateTestPty {
    fn pid(&self) -> u32 {
        self.identity.pid
    }

    fn process_identity(&self) -> Option<diagnostics::ProcessIdentity> {
        Some(self.identity)
    }

    fn write(&self, data: &str) -> Result<(), String> {
        self.writes
            .lock()
            .expect("input writes")
            .push(data.to_owned());
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

    fn subscribe_output(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.output.subscribe()
    }

    fn subscribe_exit(&self) -> tokio::sync::watch::Receiver<Option<PtyExit>> {
        self.exit.subscribe()
    }
}

#[tokio::test]
async fn ordered_input_rpc_delivers_in_order_and_binds_lease_to_physical_socket() {
    let temp = TempDir::new().expect("temp");
    let process = Arc::new(GateTestPty::new());
    let (release, released) = std::sync::mpsc::channel();
    release.send(()).expect("allow spawn");
    let backend = Arc::new(GateTestBackend {
        process: process.clone(),
        started: std::sync::Mutex::new(None),
        release: std::sync::Mutex::new(released),
    });
    let manager =
        terminal::TerminalManager::new(backend, terminal::TerminalManagerOptions::default());
    let services = fixture_services_with_manager(manager.clone(), empty_usage());
    let mut rpc = RpcRegistry::empty();
    register_server_terminal_rpc(&mut rpc, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), rpc)
        .await
        .expect("server");
    let mut socket = open_socket(handle.local_addr()).await;
    let mut other = open_socket(handle.local_addr()).await;
    assert_success(
        request(
            &mut socket,
            "1",
            "terminal.open",
            json!({
                "threadId":"ordered", "terminalId":"term", "cwd":temp.path().to_string_lossy(),
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
            }),
        )
        .await,
    );
    let lease = success_value(
        request(
            &mut socket,
            "2",
            "terminal.beginInput",
            json!({"threadId":"ordered", "terminalId":"term", "attachmentSequence":0}),
        )
        .await,
    );
    let frame = |sequence, data| json!({"threadId":"ordered", "terminalId":"term", "inputId":lease["inputId"], "sequence":sequence, "data":data});
    assert_error_tag(
        request(&mut other, "1", "terminal.writeInput", frame(0, "foreign")).await,
        "TerminalInputError",
    );
    send_request(&mut socket, "3", "terminal.writeInput", frame(1, "b")).await;
    send_request(&mut socket, "4", "terminal.writeInput", frame(0, "a")).await;
    let mut sequences = Vec::new();
    for _ in 0..2 {
        sequences.push(
            success_value(next_message(&mut socket).await)["sequence"]
                .as_u64()
                .expect("sequence"),
        );
    }
    sequences.sort_unstable();
    assert_eq!(sequences, [0, 1]);
    assert_eq!(*process.writes.lock().expect("input writes"), ["a", "b"]);
    assert_error_tag(
        request(
            &mut socket,
            "5",
            "terminal.writeInput",
            frame(0, "duplicate"),
        )
        .await,
        "TerminalInputError",
    );
    assert_eq!(*process.writes.lock().expect("input writes"), ["a", "b"]);
    socket.close(None).await.expect("close socket");
    assert_error_tag(
        request(
            &mut other,
            "2",
            "terminal.writeInput",
            frame(2, "after disconnect"),
        )
        .await,
        "TerminalInputError",
    );
    other.close(None).await.expect("close other");
    manager.shutdown().await;
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn ordered_input_rpc_reserves_capacity_for_control_requests() {
    let temp = TempDir::new().expect("temp");
    let (started, mut starts) = mpsc::channel(64);
    let mut registry = RpcRegistry::empty();
    registry.register_unary("test.hold", move |_request, cancellation| {
        let started = started.clone();
        async move {
            started.send(()).await.expect("started observer");
            cancellation.cancelled().await;
            Ok(Value::Null)
        }
    });
    registry.register_unary("terminal.writeInput", |_request, _cancellation| async {
        Ok(Value::Null)
    });
    registry.register_unary("test.control", |_request, _cancellation| async {
        Ok(json!({"ready":true}))
    });
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server");
    let mut socket = open_socket(handle.local_addr()).await;
    for id in 1..=56 {
        send_request(&mut socket, &id.to_string(), "test.hold", json!({})).await;
    }
    for _ in 0..56 {
        tokio::time::timeout(Duration::from_secs(2), starts.recv())
            .await
            .expect("request starts")
            .expect("start signal");
    }
    let rejected =
        failure_value(request(&mut socket, "57", "terminal.writeInput", json!({})).await);
    assert_eq!(rejected["_tag"], "TerminalInputError");
    assert_eq!(rejected["code"], "capacity");
    let control = success_value(request(&mut socket, "58", "test.control", json!({})).await);
    assert_eq!(control["ready"], true);
    socket.close(None).await.expect("close");
    handle.shutdown();
    handle.join().await.expect("join");
}

#[test]
fn gate_test_ptys_distinguish_same_pid_generations() {
    let first = GateTestPty::new();
    let replacement = GateTestPty::new();
    assert_eq!(first.identity.pid, replacement.identity.pid);
    assert_ne!(first.identity.started_at, 0);
    assert!(replacement.identity.started_at > first.identity.started_at);
    assert_ne!(first.identity, replacement.identity);
}

#[derive(Debug)]
struct GateTestBackend {
    process: Arc<GateTestPty>,
    started: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PtyBackend for GateTestBackend {
    fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        if let Some(started) = self.started.lock().expect("started lock").take() {
            started.send(()).expect("spawn-started receiver");
        }
        self.release
            .lock()
            .expect("release lock")
            .recv_timeout(Duration::from_secs(5))
            .expect("spawn release");
        Ok(self.process.clone())
    }
}

#[derive(Debug)]
struct FixtureControl;

impl ProductionServerControl for FixtureControl {
    fn call(
        &self,
        method: &'static str,
        payload: Value,
        _cancellation: CancellationToken,
    ) -> JsonFuture {
        Box::pin(async move {
            Ok(match method {
                "server.getConfig" => json!({ "source": "rust", "payload": payload }),
                "server.getSettings" => json!({ "automaticGitFetchInterval": 180000 }),
                _ => json!({ "method": method, "payload": payload }),
            })
        })
    }

    fn subscribe(&self, method: &'static str, _cancellation: CancellationToken) -> JsonStream {
        let (sender, receiver) = mpsc::channel(2);
        sender
            .try_send(Ok(vec![json!({ "source": "rust", "method": method })]))
            .expect("fixture stream has capacity");
        receiver
    }
}

type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[tokio::test]
async fn workspace_unavailable_rejects_terminal_starts_and_write_but_allows_close() {
    let temp = TempDir::new().expect("temporary directory");
    let registry = WorkspaceAvailabilityRegistry::new();
    assert!(
        registry
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-guarded".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 3,
                path: temp.path().to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("physical identity resolves")
    );
    let services = fixture_services().with_availability_registry(registry);
    let mut rpc = RpcRegistry::empty();
    register_server_terminal_rpc(&mut rpc, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), rpc)
        .await
        .expect("server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);
    let client = socket.as_mut().expect("socket");
    let start = json!({
        "threadId":"thread-guarded",
        "terminalId":"terminal-1",
        "cwd":temp.path().to_string_lossy(),
        "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                },
        "cols":80,
        "rows":24,
    });

    for (id, method, payload) in [
        ("1", "terminal.open", start.clone()),
        ("2", "terminal.restart", start.clone()),
        (
            "3",
            "terminal.write",
            json!({
                "threadId":"thread-guarded","terminalId":"terminal-1","data":"secret"
            }),
        ),
        (
            "4",
            "terminal.attach",
            json!({
                "threadId":"thread-guarded",
                "terminalId":"terminal-1",
                "cwd":temp.path().to_string_lossy(),
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                },
                "restartIfNotRunning":true,
            }),
        ),
    ] {
        let error = failure_value(request(client, id, method, payload).await);
        assert_eq!(error["_tag"], "WorkspaceUnavailableError");
        assert_eq!(error["threadId"], "thread-guarded");
    }
    assert_success(
        request(
            client,
            "5",
            "terminal.close",
            json!({
                "threadId":"thread-guarded","terminalId":"terminal-1"
            }),
        )
        .await,
    );

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_unavailable_quiesce_retains_transcript_for_read_only_attach() {
    let temp = TempDir::new().expect("temporary directory");
    let availability = WorkspaceAvailabilityRegistry::new();
    let services = fixture_services().with_availability_registry(availability.clone());
    let quiescer = services.clone();
    let mut rpc = RpcRegistry::empty();
    register_server_terminal_rpc(&mut rpc, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), rpc)
        .await
        .expect("server starts");
    let mut writer = Some(open_socket(handle.local_addr()).await);
    let writer_socket = writer.as_mut().expect("writer");
    assert_success(
        request(
            writer_socket,
            "1",
            "terminal.open",
            json!({
                "threadId":"thread-history",
                "terminalId":"terminal-history",
                "cwd":temp.path().to_string_lossy(),
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                },
                "cols":80,
                "rows":24,
            }),
        )
        .await,
    );

    let mut reader = Some(open_socket(handle.local_addr()).await);
    let reader_socket = reader.as_mut().expect("reader");
    send_request(
        reader_socket,
        "1",
        "terminal.attach",
        json!({
            "threadId":"thread-history","terminalId":"terminal-history"
        }),
    )
    .await;
    let _initial = next_chunk_and_ack(reader_socket, "1").await;
    assert_success(
        request(
            writer_socket,
            "2",
            "terminal.write",
            json!({
                "threadId":"thread-history",
                "terminalId":"terminal-history",
                "data":"retained-history\r",
            }),
        )
        .await,
    );
    let _output = next_terminal_event_and_ack(reader_socket, "1", "retained output", |value| {
        value["type"] == "output"
            && value["data"]
                .as_str()
                .is_some_and(|data| data.contains("retained-history"))
    })
    .await;

    assert!(
        availability
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-history".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 8,
                path: temp.path().to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("physical identity resolves")
    );
    quiescer
        .quiesce_thread_terminals_for_workspace_loss("thread-history")
        .await
        .expect("thread terminal quiesce");

    let mut history_reader = Some(open_socket(handle.local_addr()).await);
    let history_socket = history_reader.as_mut().expect("history reader");
    send_request(
        history_socket,
        "1",
        "terminal.attach",
        json!({
            "threadId":"thread-history","terminalId":"terminal-history"
        }),
    )
    .await;
    let snapshot = next_chunk_and_ack(history_socket, "1").await;
    assert_eq!(snapshot[0]["snapshot"]["status"], "exited");
    assert!(
        snapshot[0]["snapshot"]["history"]
            .as_str()
            .expect("history")
            .contains("retained-history")
    );

    for socket in [&mut reader, &mut history_reader, &mut writer] {
        close_socket(socket).await;
    }
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_loss_fences_inflight_terminal_spawn_before_publication() {
    let temp = TempDir::new().expect("temporary directory");
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let process = Arc::new(GateTestPty::new());
    let manager = terminal::TerminalManager::new(
        Arc::new(GateTestBackend {
            process: process.clone(),
            started: std::sync::Mutex::new(Some(started_tx)),
            release: std::sync::Mutex::new(release_rx),
        }),
        terminal::TerminalManagerOptions::default(),
    );
    let availability = WorkspaceAvailabilityRegistry::new();
    let services = fixture_services_with_manager(manager, empty_usage())
        .with_availability_registry(availability.clone());
    let quiescer = services.clone();
    let mut rpc = RpcRegistry::empty();
    register_server_terminal_rpc(&mut rpc, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), rpc)
        .await
        .expect("server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);
    let client = socket.as_mut().expect("socket");
    send_request(
        client,
        "1",
        "terminal.open",
        json!({
            "threadId":"panel-racing",
            "terminalId":"terminal-racing",
            "cwd":temp.path().to_string_lossy(),
            "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                },
            "cols":80,
            "rows":24,
        }),
    )
    .await;
    tokio::task::spawn_blocking(move || {
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("terminal spawn reaches publication barrier");
    })
    .await
    .expect("spawn barrier waiter");

    let loss = WorkspaceLossTransition {
        thread_id: "workspace-owner".to_owned(),
        repository_key: "repository-1".to_owned(),
        generation: 9,
        path: temp.path().to_path_buf(),
        availability: AdoptedWorktreeAvailability::MissingRegistered,
    };
    assert!(
        availability
            .mark_unavailable(loss.clone())
            .await
            .expect("physical identity resolves")
    );
    let cleanup_services = quiescer.clone();
    let cleanup = tokio::spawn(async move {
        cleanup_services
            .quiesce_thread_terminals_for_workspace_loss("panel-racing")
            .await
            .expect("visible panel terminals quiesce promptly");
    });
    cleanup
        .await
        .expect("loss cleanup finishes without admission drain");
    assert!(!process.killed.load(Ordering::Acquire));

    release_tx.send(()).expect("release terminal spawn");
    let error = failure_value(next_message(client).await);
    assert_eq!(error["_tag"], "WorkspaceUnavailableError");
    // The loss error can arrive before the blocking spawn returns and drops its PTY.
    tokio::time::timeout(TERMINAL_RPC_INTEGRATION_DEADLINE, async {
        while !process.killed.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the uncommitted PTY must be killed when spawn returns after loss");
    assert!(
        !quiescer
            .terminal_exists("panel-racing", "terminal-racing")
            .await
    );

    let error = failure_value(
        request(
            client,
            "2",
            "terminal.open",
            json!({
                "threadId":"panel-racing",
                "terminalId":"terminal-after-loss",
                "cwd":temp.path().to_string_lossy(),
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                },
                "cols":80,
                "rows":24,
            }),
        )
        .await,
    );
    assert_eq!(error["_tag"], "WorkspaceUnavailableError");

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn independent_agent_activity_settings_control_routed_rpc_gates_and_trace() {
    let temp = TempDir::new().expect("temporary directory");
    let config = test_config(&temp);
    let trace_path = config.state_dir().join("logs/server.trace.ndjson");
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({"enableAgentActivity": false}),
    );
    let handle = ServerRuntime::start(config)
        .await
        .expect("production server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);

    let result = AssertUnwindSafe(async {
        let socket = socket.as_mut().expect("socket");
        let disabled = failure_value(
            request(
                socket,
                "1",
                "activity.getSnapshot",
                json!({ "_tag": "thread", "threadId": "production-toggle" }),
            )
            .await,
        );
        assert_eq!(disabled["reason"], "featureDisabled");

        let terminal_disabled = failure_value(
            request(
                socket,
                "2",
                "activity.getSnapshot",
                json!({
                    "_tag": "terminal",
                    "threadId": "production-toggle",
                    "terminalId": "terminal-toggle"
                }),
            )
            .await,
        );
        assert_eq!(terminal_disabled["reason"], "featureDisabled");

        let terminal_enabled = success_value(
            request(
                socket,
                "3",
                "server.updateSettings",
                json!({ "patch": { "enableTerminalAgentActivity": true } }),
            )
            .await,
        );
        assert_eq!(terminal_enabled["enableTerminalAgentActivity"], true);
        let terminal_missing = failure_value(
            request(
                socket,
                "4",
                "activity.getSnapshot",
                json!({
                    "_tag": "terminal",
                    "threadId": "production-toggle",
                    "terminalId": "terminal-toggle"
                }),
            )
            .await,
        );
        assert_eq!(terminal_missing["reason"], "notFound");
        let chat_still_disabled = failure_value(
            request(
                socket,
                "5",
                "activity.getSnapshot",
                json!({ "_tag": "thread", "threadId": "production-toggle" }),
            )
            .await,
        );
        assert_eq!(chat_still_disabled["reason"], "featureDisabled");

        let chat_enabled = success_value(
            request(
                socket,
                "6",
                "server.updateSettings",
                json!({ "patch": { "enableChatAgentActivity": true } }),
            )
            .await,
        );
        assert_eq!(chat_enabled["enableChatAgentActivity"], true);
        let missing = failure_value(
            request(
                socket,
                "7",
                "activity.getSnapshot",
                json!({ "_tag": "thread", "threadId": "production-toggle" }),
            )
            .await,
        );
        assert_eq!(missing["reason"], "notFound");

        let disabled_again = success_value(
            request(
                socket,
                "8",
                "server.updateSettings",
                json!({ "patch": { "enableChatAgentActivity": false } }),
            )
            .await,
        );
        assert_eq!(disabled_again["enableChatAgentActivity"], false);
        let disabled = failure_value(
            request(
                socket,
                "9",
                "activity.getSnapshot",
                json!({ "_tag": "thread", "threadId": "production-toggle" }),
            )
            .await,
        );
        assert_eq!(disabled["reason"], "featureDisabled");

        let diagnostics =
            success_value(request(socket, "10", "server.getTraceDiagnostics", json!({})).await);
        assert!(
            diagnostics["recordCount"]
                .as_u64()
                .is_some_and(|count| count >= 5)
        );
        let spans = diagnostics["topSpansByCount"]
            .as_array()
            .expect("trace span summaries");
        assert!(spans.iter().any(|span| {
            span["name"] == "agent_activity_change_requested" && span["count"] == 3
        }));
        assert!(spans.iter().any(|span| {
            span["name"] == "agent_activity_disabled"
                && span["count"].as_u64().is_some_and(|count| count >= 2)
        }));
        assert!(
            spans
                .iter()
                .any(|span| span["name"] == "agent_activity_enabled" && span["count"] == 2)
        );

        let records = std::fs::read_to_string(&trace_path)
            .expect("production transition trace")
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("valid transition trace"))
            .filter(|record| {
                matches!(
                    record["name"].as_str(),
                    Some(
                        "agent_activity_change_requested"
                            | "agent_activity_enabled"
                            | "agent_activity_disabled"
                    )
                )
            })
            .collect::<Vec<_>>();
        let requested_sources = records
            .iter()
            .filter(|record| record["name"] == "agent_activity_change_requested")
            .flat_map(|record| record["events"].as_array().expect("trace events"))
            .map(|event| {
                event["attributes"]["source"]
                    .as_str()
                    .expect("bounded source label")
            })
            .collect::<Vec<_>>();
        assert_eq!(requested_sources, vec!["terminal", "chat", "chat"]);
        for record in records.iter().filter(|record| {
            matches!(
                record["name"].as_str(),
                Some("agent_activity_enabled" | "agent_activity_disabled")
            )
        }) {
            let attributes = record["events"][0]["attributes"]
                .as_object()
                .expect("effective transition attributes");
            assert_eq!(attributes["unavailableObservers"], 0);
            let epochs = attributes["terminalObservationEpochs"]
                .as_object()
                .expect("fixed provider epoch object");
            assert_eq!(
                epochs.keys().map(String::as_str).collect::<BTreeSet<_>>(),
                BTreeSet::from(["claude", "codex", "opencode"])
            );
            assert!(epochs.values().all(Value::is_u64));
        }
    })
    .catch_unwind()
    .await;

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn production_profiler_status_follows_the_setting_live() {
    let temp = TempDir::new().expect("temporary directory");
    let config = test_config(&temp);
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
    let handle = ServerRuntime::start(config)
        .await
        .expect("production server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);

    let result = AssertUnwindSafe(async {
        let socket = socket.as_mut().expect("socket");
        // Request ids must be unique unsigned decimal strings.
        let mut next_id = 1_u32..;
        let mut id = move || next_id.next().expect("ids never run out").to_string();
        let off =
            success_value(request(socket, &id(), "server.getProfilerStatus", json!({})).await);
        assert_eq!(off["enabled"], false);
        assert_eq!(off["capabilities"]["storage"], "ok");

        for enabled in [true, false] {
            let updated = success_value(
                request(
                    socket,
                    &id(),
                    "server.updateSettings",
                    json!({ "patch": { "enableProfiler": enabled } }),
                )
                .await,
            );
            assert_eq!(updated["enableProfiler"], enabled);
            let mut status = Value::Null;
            for _ in 0..100 {
                status = success_value(
                    request(socket, &id(), "server.getProfilerStatus", json!({})).await,
                );
                if status["enabled"] == enabled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                status["enabled"], enabled,
                "the toggle applies without a restart"
            );
            assert_eq!(status["recordingSince"].is_string(), enabled);
            assert_eq!(status["capabilities"]["storage"], "ok");
        }
    })
    .catch_unwind()
    .await;

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn a_failed_production_start_releases_the_profiler_directory() {
    let temp = TempDir::new().expect("temporary directory");
    let config = test_config(&temp);
    hermetic_providers::write_hermetic_settings(&config.state_dir(), json!({}));
    let key_pair = config.state_dir().join("environment-jwt.json");
    std::fs::write(&key_pair, b"not a key pair").expect("corrupt key pair");
    assert!(
        ServerRuntime::start(config.clone()).await.is_err(),
        "a corrupt key pair fails startup after the production runtime exists"
    );
    std::fs::remove_file(&key_pair).expect("remove corrupt key pair");

    // The failed start's profiler tears down in the background; a retry then owns the directory.
    let mut storage = Value::Null;
    for _ in 0..20 {
        let handle = ServerRuntime::start(config.clone())
            .await
            .expect("production server starts");
        let mut socket = Some(open_socket(handle.local_addr()).await);
        let status = success_value(
            request(
                socket.as_mut().expect("socket"),
                "1",
                "server.getProfilerStatus",
                json!({}),
            )
            .await,
        );
        close_socket(&mut socket).await;
        handle.shutdown();
        handle.join().await.expect("server joins");
        storage = status["capabilities"]["storage"].clone();
        if storage == "ok" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(storage, "ok", "the failed start kept the profiler lock");
}

#[tokio::test]
async fn registrar_serves_concrete_server_and_terminal_metadata_rpcs() {
    let temp = TempDir::new().expect("temporary directory");
    let services = fixture_services();
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);
    let result = AssertUnwindSafe(async {
        let socket = socket.as_mut().expect("socket");

        send_request(socket, "1", "server.getConfig", json!({})).await;
        assert_eq!(
            success_value(next_message(socket).await),
            json!({
                "source": "rust",
                "payload": {}
            })
        );

        send_request(socket, "2", "server.getProviderUsage", json!({})).await;
        let usage = success_value(next_message(socket).await);
        assert_eq!(usage["isFetching"], false);
        assert_eq!(usage["providers"][0]["provider"], "claude");
        assert_eq!(usage["providers"][1]["provider"], "codex");

        send_request(socket, "3", "subscribeTerminalMetadata", json!({})).await;
        let chunk = next_message(socket).await;
        assert!(matches!(
            chunk,
            ServerMessage::Chunk { values, .. }
                if values == vec![json!({ "type": "snapshot", "terminals": [] })]
        ));
        send_ack(socket, "3").await;

        let terminal_payload = json!({
            "threadId": "thread-1",
            "terminalId": "term-1",
            "cwd": temp.path().to_string_lossy(),
            "cols": 120,
            "rows": 30,
            "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
        });
        send_request(socket, "4", "terminal.open", terminal_payload.clone()).await;
        let first = next_message(socket).await;
        let second = next_message(socket).await;
        assert_terminal_open_and_metadata_upsert([first, second]);

        send_request(socket, "5", "terminal.open", terminal_payload).await;
        assert!(matches!(
            next_message(socket).await,
            ServerMessage::Exit {
                exit: RpcExit::Success { .. },
                ..
            }
        ));
    })
    .catch_unwind()
    .await;

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn provider_usage_refresh_rpc_forces_a_fetch_only_when_requested() {
    let temp = TempDir::new().expect("temporary directory");
    let calls = Arc::new(AtomicUsize::new(0));
    let now = time::OffsetDateTime::now_utc();
    let usage = provider_usage::ProviderUsageService::new(
        vec![provider_usage::ProviderUsageFetcher {
            provider: provider_usage::ProviderUsageProvider::Claude,
            fetch: Arc::new({
                let calls = calls.clone();
                move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async move {
                        Ok(provider_usage::ProviderUsageSnapshot {
                            provider: provider_usage::ProviderUsageProvider::Claude,
                            status: provider_usage::ProviderUsageStatus::Ok,
                            session: None,
                            weekly: None,
                            fable_weekly: None,
                            plan_type: None,
                            rate_limit_reset_credits: None,
                            updated_at: now,
                            error: None,
                            metadata: BTreeMap::new(),
                        })
                    })
                }
            }),
        }],
        Arc::new(move || now),
    );
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, fixture_services_with_usage(usage));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);

    let result = AssertUnwindSafe(async {
        let socket = socket.as_mut().expect("socket");
        success_value(
            request(
                socket,
                "1",
                "server.refreshProviderUsage",
                json!({ "providers": ["claude"] }),
            )
            .await,
        );
        success_value(
            request(
                socket,
                "2",
                "server.refreshProviderUsage",
                json!({ "providers": ["claude"] }),
            )
            .await,
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        success_value(
            request(
                socket,
                "3",
                "server.refreshProviderUsage",
                json!({ "providers": ["claude"], "force": true }),
            )
            .await,
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    })
    .catch_unwind()
    .await;

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn terminal_command_activity_hint_decodes_strictly_without_entering_launch_env() {
    let temp = TempDir::new().expect("temporary directory");
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, fixture_services());
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);

    let result = AssertUnwindSafe(async {
        let socket = socket.as_mut().expect("socket");
        let env_probe = temp.path().join("activity-env-probe.txt");
        let (executable, args) = environment_probe_command(&env_probe);
        let valid = json!({
            "threadId": "thread-command-activity",
            "terminalId": "term-valid",
            "cwd": temp.path().to_string_lossy(),
            "env": {
                "COMMAND_ONLY": "command",
                "RUNTIME_ONLY": "runtime",
                "SHARED": "runtime",
            },
            "command": {
                "executable": executable,
                "args": args,
                "activity": {
                    "driverKind": "codex",
                    "providerInstanceId": "activity_instance_marker",
                },
            },
        });
        assert_success(request(socket, "1", "terminal.open", valid).await);
        let env_probe_contents = read_file_with_retry(&env_probe).await;
        assert_eq!(env_probe_contents, "command|runtime|runtime|clean");
        assert_success(
            request(
                socket,
                "2",
                "terminal.close",
                json!({
                    "threadId": "thread-command-activity",
                    "terminalId": "term-valid",
                }),
            )
            .await,
        );

        let invalid_activities = [
            json!({
                "driverKind": "unknown.driver",
                "providerInstanceId": "codex",
            }),
            json!({
                "driverKind": "codex",
                "providerInstanceId": "",
            }),
            json!({
                "driverKind": "codex",
                "providerInstanceId": "x".repeat(65),
            }),
            json!({
                "driverKind": "codex",
            }),
            json!({
                "driverKind": "codex",
                "providerInstanceId": "codex",
                "token": "secret",
            }),
            json!({
                "driverKind": "codex",
                "providerInstanceId": "codex",
                "secret": "secret",
            }),
            Value::Null,
        ];
        for (index, activity) in invalid_activities.into_iter().enumerate() {
            let invalid_probe = temp.path().join(format!("invalid-activity-{index}.txt"));
            let (executable, args) = environment_probe_command(&invalid_probe);
            assert_error_tag(
                request(
                    socket,
                    &format!("{}", index + 3),
                    "terminal.open",
                    json!({
                        "threadId": "thread-command-activity",
                        "terminalId": format!("term-invalid-{index}"),
                        "cwd": temp.path().to_string_lossy(),
                        "command": {
                            "executable": executable,
                            "args": args,
                            "activity": activity,
                        },
                    }),
                )
                .await,
                "RpcRequestInvalid",
            );
        }
    })
    .catch_unwind()
    .await;

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn provider_usage_reset_rpc_rejects_blank_request_ids_with_typed_error() {
    let temp = TempDir::new().expect("temporary directory");
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, fixture_services());
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");
    let mut socket = Some(open_socket(handle.local_addr()).await);

    let result = request(
        socket.as_mut().expect("socket"),
        "1",
        "server.consumeCodexRateLimitReset",
        json!({ "requestId": " \t\r\n " }),
    )
    .await;
    match result {
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause: ref items },
            ..
        } => {
            let [CauseItem::Fail { error }] = items.as_slice() else {
                panic!("expected a single failure cause, got {items:?}");
            };
            assert_eq!(error["_tag"], "ServerProviderUsageResetError");
            assert_eq!(error["message"], "Codex reset request ID is required.");
        }
        other => panic!("expected typed reset failure, got {other:?}"),
    }

    close_socket(&mut socket).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn terminal_rpc_attach_tracks_activity_and_cleans_up_running_child_processes() {
    let temp = TempDir::new().expect("temporary directory");
    let services = fixture_services();
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");

    let mut events = Some(open_socket(handle.local_addr()).await);
    let mut metadata = Some(open_socket(handle.local_addr()).await);
    let mut attach = Some(open_socket(handle.local_addr()).await);
    let mut control = Some(open_socket(handle.local_addr()).await);

    let result = AssertUnwindSafe(async {
        let events = events.as_mut().expect("events socket");
        let metadata = metadata.as_mut().expect("metadata socket");
        let attach = attach.as_mut().expect("attach socket");
        let control = control.as_mut().expect("control socket");

        send_request(events, "1", "subscribeTerminalEvents", json!({})).await;
        send_request(metadata, "1", "subscribeTerminalMetadata", json!({})).await;
        assert_eq!(
            next_chunk_and_ack(metadata, "1").await,
            vec![json!({ "type": "snapshot", "terminals": [] })]
        );

        send_request(
            attach,
            "1",
            "terminal.attach",
            json!({
                "threadId": "thread-activity",
                "terminalId": "term-activity",
                "cwd": temp.path().to_string_lossy(),
                "cols": 120,
                "rows": 30,
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
            }),
        )
        .await;
        let snapshot = next_chunk_and_ack(attach, "1").await;
        assert_eq!(snapshot[0]["type"], "snapshot");
        assert_eq!(snapshot[0]["snapshot"]["terminalId"], "term-activity");
        assert_eq!(snapshot[0]["snapshot"]["status"], "running");

        let started = next_terminal_event_and_ack(events, "1", "started", |value| {
            value["type"] == "started" && value["terminalId"] == "term-activity"
        })
        .await;
        assert_eq!(started["snapshot"]["status"], "running");

        let upsert =
            next_matching_chunk_value(metadata, "1", "initial running metadata upsert", |value| {
                value["type"] == "upsert" && value["terminal"]["terminalId"] == "term-activity"
            })
            .await;
        assert_eq!(upsert["terminal"]["status"], "running");

        maybe_prime_terminal(control, "thread-activity", "term-activity").await;
        assert_success(
            request(
                control,
                "2",
                "terminal.write",
                json!({
                    "threadId": "thread-activity",
                    "terminalId": "term-activity",
                    "data": long_running_command(),
                }),
            )
            .await,
        );

        let output = next_terminal_event_and_ack(attach, "1", "marked output", |value| {
            value["type"] == "output"
                && value["data"]
                    .as_str()
                    .is_some_and(|data| data.contains(long_running_output_marker()))
        })
        .await;
        assert!(
            output["data"]
                .as_str()
                .is_some_and(|data| data.contains(long_running_output_marker())),
            "long-running child should emit observable output"
        );

        let activity = next_terminal_event_and_ack(events, "1", "running activity", |value| {
            value["type"] == "activity"
                && value["terminalId"] == "term-activity"
                && value["hasRunningSubprocess"] == true
                && value["label"] == long_running_label()
        })
        .await;
        assert_eq!(activity["label"].as_str(), Some(long_running_label()));

        let metadata_activity = next_matching_chunk_value(
            metadata,
            "1",
            "running subprocess metadata upsert",
            |value| {
                value["type"] == "upsert"
                    && value["terminal"]["terminalId"] == "term-activity"
                    && value["terminal"]["hasRunningSubprocess"] == true
                    && value["terminal"]["label"] == long_running_label()
            },
        )
        .await;
        assert_eq!(
            metadata_activity["terminal"]["label"].as_str(),
            Some(long_running_label())
        );

        assert_success(
            request(
                control,
                "3",
                "terminal.close",
                json!({
                    "threadId": "thread-activity",
                    "terminalId": "term-activity",
                }),
            )
            .await,
        );

        let closed = next_terminal_event_and_ack(attach, "1", "closed", |value| {
            value["type"] == "closed" && value["terminalId"] == "term-activity"
        })
        .await;
        assert_eq!(closed["threadId"], "thread-activity");
        let removed =
            next_expected_chunk_value_and_ack(metadata, "1", "metadata remove", |value| {
                value["type"] == "remove" && value["terminalId"] == "term-activity"
            })
            .await;
        assert_eq!(removed["threadId"], "thread-activity");

        assert_error_tag(
            request(
                control,
                "4",
                "terminal.write",
                json!({
                    "threadId": "thread-activity",
                    "terminalId": "term-activity",
                    "data": "echo should-not-run\r\n",
                }),
            )
            .await,
            "TerminalSessionLookupError",
        );
    })
    .catch_unwind()
    .await;

    close_socket(&mut attach).await;
    close_socket(&mut metadata).await;
    close_socket(&mut events).await;
    close_socket(&mut control).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn terminal_rpc_clear_resize_restart_exit_and_restart_if_not_running_round_trip() {
    let temp = TempDir::new().expect("temporary directory");
    let services = fixture_services();
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");

    let mut events = Some(open_socket(handle.local_addr()).await);
    let mut metadata = Some(open_socket(handle.local_addr()).await);
    let mut attach = Some(open_socket(handle.local_addr()).await);
    let mut control = Some(open_socket(handle.local_addr()).await);
    let mut reattach = None;
    let mut post_restart_attach = None;
    let mut failing_attach = None;
    let mut restarting_attach = None;

    let result = AssertUnwindSafe(async {
        let events = events.as_mut().expect("events socket");
        let metadata = metadata.as_mut().expect("metadata socket");
        let attach = attach.as_mut().expect("attach socket");
        let control = control.as_mut().expect("control socket");

        send_request(events, "1", "subscribeTerminalEvents", json!({})).await;
        send_request(metadata, "1", "subscribeTerminalMetadata", json!({})).await;
        assert_eq!(
            next_chunk_and_ack(metadata, "1").await,
            vec![json!({ "type": "snapshot", "terminals": [] })]
        );

        let opened = success_value(
            request(
                control,
                "2",
                "terminal.open",
                json!({
                    "threadId": "thread-restart",
                    "terminalId": "term-restart",
                    "cwd": temp.path().to_string_lossy(),
                    "cols": 151,
                    "rows": 50,
                    "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
                }),
            )
            .await,
        );
        assert_eq!(opened["status"], "running");
        assert_eq!(opened["size"]["cols"], 151);
        assert_eq!(opened["size"]["rows"], 50);
        assert!(opened["size"]["sizeClaim"].is_null());
        let first_pid = opened["pid"].as_u64().expect("terminal pid");

        let started = next_terminal_event_and_ack(events, "1", "started", |value| {
            value["type"] == "started" && value["terminalId"] == "term-restart"
        })
        .await;
        assert_eq!(started["snapshot"]["status"], "running");
        let running_metadata =
            next_matching_chunk_value(metadata, "1", "opened terminal metadata upsert", |value| {
                value["type"] == "upsert" && value["terminal"]["terminalId"] == "term-restart"
            })
            .await;
        assert_eq!(running_metadata["terminal"]["status"], "running");

        send_request(
            attach,
            "1",
            "terminal.attach",
            json!({
                "threadId": "thread-restart",
                "terminalId": "term-restart",
                "sizeClaim": "rpc-renderer",
            }),
        )
        .await;
        let attached = next_chunk_and_ack(attach, "1").await;
        assert_eq!(attached[0]["snapshot"]["pid"].as_u64(), Some(first_pid));

        maybe_prime_terminal(control, "thread-restart", "term-restart").await;
        let token = "BIBCODE_RPC_CLEAR_RESTART";
        assert_success(
            request(
                control,
                "3",
                "terminal.write",
                json!({
                    "threadId": "thread-restart",
                    "terminalId": "term-restart",
                    "data": shell_echo_command(token),
                }),
            )
            .await,
        );
        let output = next_terminal_event_and_ack(attach, "1", "echo output", |value| {
            value["type"] == "output"
                && value["data"]
                    .as_str()
                    .is_some_and(|data| data.contains(token))
        })
        .await;
        assert!(
            output["data"]
                .as_str()
                .is_some_and(|data| data.contains(token))
        );

        assert_success(
            request(
                control,
                "4",
                "terminal.clear",
                json!({
                    "threadId": "thread-restart",
                    "terminalId": "term-restart",
                }),
            )
            .await,
        );
        let cleared = next_terminal_event_and_ack(attach, "1", "cleared", |value| {
            value["type"] == "cleared" && value["terminalId"] == "term-restart"
        })
        .await;
        assert_eq!(cleared["threadId"], "thread-restart");
        let cleared_sequence = cleared["sequence"]
            .as_u64()
            .expect("cleared event sequence");
        let cleared_event = next_terminal_event_and_ack(events, "1", "cleared", |value| {
            value["type"] == "cleared" && value["terminalId"] == "term-restart"
        })
        .await;
        assert_eq!(cleared_event["threadId"], "thread-restart");

        reattach = Some(open_socket(handle.local_addr()).await);
        let reattach_socket = reattach.as_mut().expect("reattach socket");
        send_request(
            reattach_socket,
            "1",
            "terminal.attach",
            json!({
                "threadId": "thread-restart",
                "terminalId": "term-restart",
            }),
        )
        .await;
        let cleared_snapshot = next_chunk_and_ack(reattach_socket, "1").await;
        let snapshot = &cleared_snapshot[0]["snapshot"];
        let snapshot_sequence = snapshot["sequence"]
            .as_u64()
            .expect("reattached snapshot sequence");
        assert!(snapshot_sequence >= cleared_sequence);
        if snapshot["history"] != "" {
            assert!(
                snapshot_sequence > cleared_sequence,
                "non-empty history must contain output received after terminal.clear"
            );
        }

        assert_success(
            request(
                control,
                "5",
                "terminal.resize",
                json!({
                    "threadId": "thread-restart",
                    "terminalId": "term-restart",
                    "cols": 100,
                    "rows": 24,
                    "sizeClaim": "rpc-renderer",
                }),
            )
            .await,
        );
        let resized =
            next_terminal_event_and_ack(attach, "1", "resized", |value| value["type"] == "resized")
                .await;
        assert_eq!(resized["size"]["cols"], 100);
        assert_eq!(resized["size"]["rows"], 24);
        assert_eq!(resized["size"]["sizeClaim"], "rpc-renderer");
        assert_error_tag(
            request(
                control,
                "6",
                "terminal.restart",
                json!({
                    "threadId": "thread-restart",
                    "terminalId": "term-restart",
                    "cwd": temp.path().to_string_lossy(),
                    "rows": 24,
                    "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
                }),
            )
            .await,
            "RpcRequestInvalid",
        );

        let restarted = success_value(
            request(
                control,
                "7",
                "terminal.restart",
                json!({
                    "threadId": "thread-restart",
                    "terminalId": "term-restart",
                    "cwd": temp.path().to_string_lossy(),
                    "cols": 80,
                    "rows": 20,
                    "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
                }),
            )
            .await,
        );
        assert_eq!(restarted["status"], "running");
        assert_ne!(restarted["pid"].as_u64(), Some(first_pid));

        let (attach_restarted, attach_saw_exit) =
            next_restart_event_and_ack(attach, "1", "thread-restart", "term-restart").await;
        assert_eq!(attach_restarted["snapshot"]["status"], "running");
        let (restarted_event, events_saw_exit) =
            next_restart_event_and_ack(events, "1", "thread-restart", "term-restart").await;
        assert_eq!(events_saw_exit, attach_saw_exit);
        assert_eq!(restarted_event["snapshot"]["status"], "running");
        let (restarted_metadata, metadata_saw_exit) =
            next_restart_metadata_upsert(metadata, "1", "thread-restart", "term-restart").await;
        assert_eq!(metadata_saw_exit, events_saw_exit);
        assert_eq!(restarted_metadata["terminal"]["status"], "running");

        post_restart_attach = Some(open_socket(handle.local_addr()).await);
        let post_restart_attach_socket = post_restart_attach
            .as_mut()
            .expect("post-restart attach socket");
        send_request(
            post_restart_attach_socket,
            "2",
            "terminal.attach",
            json!({
                "threadId": "thread-restart",
                "terminalId": "term-restart",
            }),
        )
        .await;
        let restarted_snapshot = next_chunk_and_ack(post_restart_attach_socket, "2").await;
        let restarted_pid = restarted["pid"].as_u64().expect("restarted terminal pid");
        assert_eq!(
            restarted_snapshot[0]["snapshot"]["pid"].as_u64(),
            Some(restarted_pid)
        );
        let diagnostics =
            success_value(request(control, "8", "server.getProcessDiagnostics", json!({})).await);
        let restarted_process_key = diagnostics["processes"]
            .as_array()
            .expect("diagnostic processes")
            .iter()
            .find(|process| process["pid"].as_u64() == Some(restarted_pid))
            .and_then(|process| process["processKey"].as_str())
            .expect("restarted terminal process key");

        let signal_result = success_value(
            request(
                control,
                "9",
                "server.signalProcess",
                json!({
                    "pid": restarted_pid,
                    "processKey": restarted_process_key,
                    "signal": "SIGKILL",
                }),
            )
            .await,
        );
        let signal_supported = signal_result["signaled"] == true;
        assert_eq!(
            signal_supported,
            cfg!(any(target_os = "linux", windows)),
            "only identity-bound platform signal implementations may report success"
        );
        if !signal_supported {
            assert_success(
                request(
                    control,
                    "10",
                    "terminal.write",
                    json!({
                        "threadId": "thread-restart",
                        "terminalId": "term-restart",
                        "data": "exit 0\r\n",
                    }),
                )
                .await,
            );
        }
        let expected_exit_code = if signal_supported {
            expected_killed_exit_code()
        } else {
            json!(0)
        };

        let exited =
            next_terminal_event_and_ack(post_restart_attach_socket, "2", "exited", |value| {
                value["type"] == "exited" && value["terminalId"] == "term-restart"
            })
            .await;
        assert_eq!(exited["exitCode"], expected_exit_code);
        assert_eq!(exited["exitSignal"], expected_killed_exit_signal());
        let exited_metadata =
            next_matching_chunk_value(metadata, "1", "exited terminal metadata upsert", |value| {
                value["type"] == "upsert"
                    && value["terminal"]["terminalId"] == "term-restart"
                    && value["terminal"]["status"] == "exited"
            })
            .await;
        assert_eq!(exited_metadata["terminal"]["pid"], Value::Null);
        assert_eq!(exited_metadata["terminal"]["exitCode"], expected_exit_code);

        failing_attach = Some(open_socket(handle.local_addr()).await);
        let failing_attach_socket = failing_attach.as_mut().expect("failing attach socket");
        send_request(
            failing_attach_socket,
            "1",
            "terminal.attach",
            json!({
                "threadId": "thread-restart",
                "terminalId": "term-restart",
                "restartIfNotRunning": true,
            }),
        )
        .await;
        assert_error_tag(
            next_message(failing_attach_socket).await,
            "TerminalNotRunningError",
        );

        restarting_attach = Some(open_socket(handle.local_addr()).await);
        let restarting_attach_socket = restarting_attach
            .as_mut()
            .expect("restarting attach socket");
        send_request(
            restarting_attach_socket,
            "1",
            "terminal.attach",
            json!({
                "threadId": "thread-restart",
                "terminalId": "term-restart",
                "cwd": temp.path().to_string_lossy(),
                "restartIfNotRunning": true,
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                },
                "cols": 90,
                "rows": 25,
            }),
        )
        .await;
        let attach_restart_snapshot = next_chunk_and_ack(restarting_attach_socket, "1").await;
        assert_eq!(attach_restart_snapshot[0]["snapshot"]["status"], "running");
        let (attach_restart, _) =
            next_restart_event_and_ack(events, "1", "thread-restart", "term-restart").await;
        assert_eq!(attach_restart["snapshot"]["status"], "running");
        assert_eq!(
            attach_restart["snapshot"]["pid"],
            attach_restart_snapshot[0]["snapshot"]["pid"]
        );
        let attach_restart_metadata =
            next_expected_chunk_value_and_ack(metadata, "1", "running metadata upsert", |value| {
                value["type"] == "upsert"
                    && value["terminal"]["terminalId"] == "term-restart"
                    && value["terminal"]["status"] == "running"
                    && value["terminal"]["pid"] == attach_restart_snapshot[0]["snapshot"]["pid"]
            })
            .await;
        assert_eq!(attach_restart_metadata["terminal"]["status"], "running");
    })
    .catch_unwind()
    .await;

    close_socket(&mut reattach).await;
    close_socket(&mut post_restart_attach).await;
    close_socket(&mut restarting_attach).await;
    close_socket(&mut failing_attach).await;
    close_socket(&mut attach).await;
    close_socket(&mut metadata).await;
    close_socket(&mut events).await;
    close_socket(&mut control).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn server_terminal_auxiliary_rpcs_surface_runtime_state_validation_and_interrupts() {
    let temp = TempDir::new().expect("temporary directory");
    let services = fixture_services();
    let mut registry = RpcRegistry::empty();
    register_server_terminal_rpc(&mut registry, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("Rust server starts");

    let mut control = Some(open_socket(handle.local_addr()).await);
    let mut cloud_install = Some(open_socket(handle.local_addr()).await);
    let mut interruptible_attach = Some(open_socket(handle.local_addr()).await);

    let result = AssertUnwindSafe(async {
        let control = control.as_mut().expect("control socket");
        let cloud_install = cloud_install.as_mut().expect("cloud install socket");
        let interruptible_attach = interruptible_attach
            .as_mut()
            .expect("interruptible attach socket");

        let settings = success_value(request(control, "1", "server.getSettings", json!({})).await);
        assert_eq!(settings["automaticGitFetchInterval"], 180000);

        let diagnostics =
            success_value(request(control, "2", "server.getProcessDiagnostics", json!({})).await);
        assert_eq!(
            diagnostics["serverPid"].as_u64(),
            Some(u64::from(std::process::id()))
        );
        assert!(
            diagnostics["totals"]["combined"]["processCount"]
                .as_u64()
                .is_some()
        );

        let history = success_value(
            request(
                control,
                "3",
                "server.getProcessResourceHistory",
                json!({
                    "windowMs": 5_000,
                    "bucketMs": 1_000,
                }),
            )
            .await,
        );
        assert_eq!(history["windowMs"], 5_000);
        assert_eq!(history["bucketMs"], 1_000);

        let profiler =
            success_value(request(control, "10", "server.getProfilerStatus", json!({})).await);
        assert_eq!(profiler["enabled"], false);
        assert_eq!(profiler["capabilities"]["storage"], "ok");
        assert_eq!(profiler["findings"], json!([]));
        let reported = success_value(
            request(control, "11", "server.reportProfilerClientBatch", json!({})).await,
        );
        assert_eq!(reported["accepted"], false);

        let refreshed = success_value(
            request(
                control,
                "4",
                "server.refreshProviderUsage",
                json!({
                    "providers": ["claude"],
                }),
            )
            .await,
        );
        assert_eq!(refreshed["providers"][0]["provider"], "claude");
        assert_error_tag(
            request(
                control,
                "5",
                "server.refreshProviderUsage",
                json!({
                    "providers": ["invalid-provider"],
                }),
            )
            .await,
            "RpcRequestInvalid",
        );

        let relay_status =
            success_value(request(control, "6", "cloud.getRelayClientStatus", json!({})).await);
        assert_eq!(relay_status["status"], "missing");
        assert_eq!(relay_status["version"], "1.0.0");

        send_request(cloud_install, "1", "cloud.installRelayClient", json!({})).await;
        let progress = next_chunk_and_ack(cloud_install, "1").await;
        assert_eq!(
            progress,
            vec![json!({ "type": "progress", "stage": "download" })]
        );
        let complete = next_chunk_and_ack(cloud_install, "1").await;
        assert_eq!(
            complete,
            vec![json!({
                "type": "complete",
                "status": { "status": "missing", "version": "1.0.0" }
            })]
        );
        assert!(matches!(
            next_message(cloud_install).await,
            ServerMessage::Exit {
                exit: RpcExit::Success { value: None },
                ..
            }
        ));

        send_request(
            interruptible_attach,
            "1",
            "terminal.attach",
            json!({
                "threadId": "thread-interrupt",
                "terminalId": "term-interrupt",
                "cwd": temp.path().to_string_lossy(),
                "cols": 120,
                "rows": 30,
                "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(temp.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(temp.path()),
                }
            }),
        )
        .await;
        let snapshot = next_chunk(interruptible_attach, "1").await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0]["type"], "snapshot");
        send_interrupt(interruptible_attach, "1").await;
        assert!(matches!(
            next_message(interruptible_attach).await,
            ServerMessage::Exit {
                request_id,
                exit: RpcExit::Failure { cause },
            } if request_id.as_str() == "1"
                && cause == vec![CauseItem::Interrupt { fiber_id: None }]
        ));

        assert_error_tag(
            request(
                control,
                "7",
                "server.signalProcess",
                json!({
                    "pid": std::process::id(),
                    "processKey": format!("{}:0", std::process::id()),
                    "signal": "SIGHUP",
                }),
            )
            .await,
            "RpcRequestInvalid",
        );
        let not_signaled = success_value(
            request(
                control,
                "8",
                "server.signalProcess",
                json!({
                    "pid": u32::MAX,
                    "processKey": format!("{}:999", u32::MAX),
                    "signal": "SIGINT",
                }),
            )
            .await,
        );
        assert_eq!(not_signaled["signaled"], false);
        assert_eq!(not_signaled["pid"], u32::MAX);
        let server_process_key = diagnostics["processes"]
            .as_array()
            .expect("diagnostic processes")
            .iter()
            .find(|process| process["pid"].as_u64() == Some(u64::from(std::process::id())))
            .and_then(|process| process["processKey"].as_str())
            .expect("server process key");
        let core_not_signaled = success_value(
            request(
                control,
                "9",
                "server.signalProcess",
                json!({
                    "pid": std::process::id(),
                    "processKey": server_process_key,
                    "signal": "SIGINT",
                }),
            )
            .await,
        );
        assert_eq!(core_not_signaled["signaled"], false);
    })
    .catch_unwind()
    .await;

    close_socket(&mut interruptible_attach).await;
    close_socket(&mut cloud_install).await;
    close_socket(&mut control).await;
    handle.shutdown();
    handle.join().await.expect("server joins");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn assert_terminal_open_and_metadata_upsert(messages: [ServerMessage; 2]) {
    assert!(messages.iter().any(|message| matches!(
        message,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    )));
    assert!(messages.iter().any(|message| matches!(
        message,
        ServerMessage::Chunk { values, .. }
            if values.iter().any(|value|
                value["type"] == "upsert"
                    && value["terminal"]["terminalId"] == "term-1"
                    && value["terminal"]["status"] == "running"
            )
    )));
}

#[test]
fn registrar_source_contains_every_owned_rpc_name() {
    let source = include_str!("../src/production/server_terminal.rs");
    for method in [
        "terminal.open",
        "terminal.attach",
        "terminal.write",
        "terminal.resize",
        "terminal.clear",
        "terminal.restart",
        "terminal.close",
        "subscribeTerminalEvents",
        "subscribeTerminalMetadata",
        "server.getConfig",
        "server.getProcessDiagnostics",
        "server.getProcessResourceHistory",
        "server.getProfilerStatus",
        "server.reportProfilerClientBatch",
        "server.getProviderUsage",
        "server.consumeCodexRateLimitReset",
        "server.getSettings",
        "server.getTraceDiagnostics",
        "server.refreshProviders",
        "server.refreshProviderUsage",
        "server.removeKeybinding",
        "server.signalProcess",
        "server.updateProvider",
        "server.updateSettings",
        "server.upsertKeybinding",
        "subscribeServerConfig",
        "subscribeServerLifecycle",
        "subscribeDiscoveredLocalServers",
        "cloud.getRelayClientStatus",
        "cloud.installRelayClient",
    ] {
        assert!(source.contains(method), "registrar is missing {method}");
    }
}

fn fixture_services() -> ServerTerminalServices {
    fixture_services_with_usage(empty_usage())
}

fn empty_usage() -> provider_usage::ProviderUsageService {
    provider_usage::ProviderUsageService::new(Vec::new(), Arc::new(time::OffsetDateTime::now_utc))
}

fn fixture_services_with_usage(
    usage: provider_usage::ProviderUsageService,
) -> ServerTerminalServices {
    fixture_services_with_manager(
        terminal::TerminalManager::new(
            Arc::new(terminal::PortablePtyBackend),
            terminal::TerminalManagerOptions {
                subprocess_poll_interval: Duration::from_millis(100),
                ..terminal::TerminalManagerOptions::default()
            },
        ),
        usage,
    )
}

fn fixture_services_with_manager(
    terminal: terminal::TerminalManager,
    usage: provider_usage::ProviderUsageService,
) -> ServerTerminalServices {
    let sampler = Arc::new(diagnostics::NativeProcessSampler::default());
    let resource_sampler = Arc::new(diagnostics::NativeResourceSampler::new(
        sampler.clone(),
        diagnostics::ProcessAttributionRegistry::new(),
        Arc::new(diagnostics::NotApplicableUiProcessObserver),
    ));
    let monitor = Arc::new(diagnostics::DiagnosticsMonitor::new(
        resource_sampler.clone(),
        Duration::from_secs(60),
    ));
    let relay = cloud::RelayClientService::new(
        || async {
            cloud::RelayClientStatus::Missing {
                version: "1.0.0".into(),
            }
        },
        |report| async move {
            report(cloud::RelayClientInstallEvent::Progress {
                stage: "download".into(),
            })
            .await?;
            Ok(cloud::RelayClientStatus::Missing {
                version: "1.0.0".into(),
            })
        },
    );
    ServerTerminalServices::new(
        terminal,
        sampler,
        resource_sampler,
        monitor,
        usage,
        relay,
        Arc::new(FixtureControl),
    )
}

fn test_config(temp: &TempDir) -> ServerConfig {
    ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
}

async fn send_request(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: &str,
    tag: &str,
    payload: Value,
) {
    let message =
        json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .expect("request sends");
}

async fn send_ack(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request_id: &str,
) {
    let message = json!({ "_tag": "Ack", "requestId": request_id });
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .expect("acknowledgement sends");
}

async fn send_interrupt(socket: &mut TestSocket, request_id: &str) {
    let message = json!({ "_tag": "Interrupt", "requestId": request_id });
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .expect("interrupt sends");
}

async fn next_message(socket: &mut TestSocket) -> ServerMessage {
    next_message_for(socket, "server message").await
}

async fn next_message_for(socket: &mut TestSocket, expected: &str) -> ServerMessage {
    let message = tokio::time::timeout(
        TERMINAL_RPC_INTEGRATION_DEADLINE,
        next_frame_past_heartbeat(socket),
    )
    .await
    .unwrap_or_else(|error| panic!("response timeout while waiting for {expected}: {error}"))
    .expect("socket remains open")
    .expect("valid socket message");
    let Message::Text(text) = message else {
        panic!("expected text message")
    };
    serde_json::from_str(&text).expect("valid server message")
}

fn success_value(message: ServerMessage) -> Value {
    match message {
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } => value,
        other => panic!("expected successful RPC response, got {other:?}"),
    }
}

fn failure_value(message: ServerMessage) -> Value {
    match message {
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } => match cause.as_slice() {
            [CauseItem::Fail { error }] => error.clone(),
            other => panic!("expected one failure value, got {other:?}"),
        },
        other => panic!("expected failed RPC response, got {other:?}"),
    }
}

async fn open_socket(local_addr: SocketAddr) -> TestSocket {
    connect_async(format!("ws://{local_addr}/ws"))
        .await
        .expect("WebSocket connects")
        .0
}

async fn close_socket(socket: &mut Option<TestSocket>) {
    if let Some(socket) = socket.as_mut() {
        let _ = socket.close(None).await;
    }
}

async fn request(socket: &mut TestSocket, id: &str, tag: &str, payload: Value) -> ServerMessage {
    send_request(socket, id, tag, payload).await;
    next_message_for(socket, &format!("{tag} response (request {id})")).await
}

fn assert_success(message: ServerMessage) {
    assert!(matches!(
        message,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    ));
}

fn assert_error_tag(message: ServerMessage, expected_tag: &str) {
    match message {
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause: ref items },
            ..
        } => {
            let [CauseItem::Fail { error }] = items.as_slice() else {
                panic!("expected a single failure cause, got {items:?}");
            };
            assert_eq!(error["_tag"], expected_tag);
        }
        other => panic!("expected failure response, got {other:?}"),
    }
}

async fn next_chunk_and_ack(socket: &mut TestSocket, request_id: &str) -> Vec<Value> {
    let values = next_chunk(socket, request_id).await;
    send_ack(socket, request_id).await;
    values
}

async fn next_matching_chunk_value<F>(
    socket: &mut TestSocket,
    request_id: &str,
    expected: &str,
    matches: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    loop {
        let values = next_chunk_for(socket, request_id, expected).await;
        send_ack(socket, request_id).await;
        if let Some(value) = values.into_iter().find(&matches) {
            return value;
        }
    }
}

async fn next_expected_chunk_value_and_ack<F>(
    socket: &mut TestSocket,
    request_id: &str,
    expected: &str,
    matches: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    let values = next_chunk_for(socket, request_id, expected).await;
    let [value] = values.as_slice() else {
        panic!("expected one {expected} chunk value, got {values:?}");
    };
    assert!(matches(value), "expected {expected}, got {value:?}");
    let value = value.clone();
    send_ack(socket, request_id).await;
    value
}

async fn next_terminal_event_and_ack<F>(
    socket: &mut TestSocket,
    request_id: &str,
    expected: &str,
    matches: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    loop {
        let values = next_chunk_for(socket, request_id, expected).await;
        let [value] = values.as_slice() else {
            panic!("expected one terminal event while waiting for {expected}, got {values:?}");
        };
        if matches(value) {
            let value = value.clone();
            send_ack(socket, request_id).await;
            return value;
        }
        assert!(
            matches!(value["type"].as_str(), Some("output" | "activity")),
            "expected {expected}, got unexpected terminal lifecycle event {value:?}"
        );
        send_ack(socket, request_id).await;
    }
}

async fn next_restart_event_and_ack(
    socket: &mut TestSocket,
    request_id: &str,
    thread_id: &str,
    terminal_id: &str,
) -> (Value, bool) {
    let mut saw_exit = false;

    loop {
        let values = next_chunk_for(socket, request_id, "restarted terminal event").await;
        let [value] = values.as_slice() else {
            panic!("expected one terminal event while waiting for restarted, got {values:?}");
        };
        let matching_terminal =
            value["threadId"] == thread_id && value["terminalId"] == terminal_id;

        match value["type"].as_str() {
            Some("output" | "activity") => {}
            Some("exited") if matching_terminal => saw_exit = true,
            Some("restarted") if matching_terminal => {
                let restarted = value.clone();
                send_ack(socket, request_id).await;
                return (restarted, saw_exit);
            }
            _ => {
                panic!("expected exited or restarted for {thread_id}/{terminal_id}, got {value:?}")
            }
        }
        send_ack(socket, request_id).await;
    }
}

async fn next_restart_metadata_upsert(
    socket: &mut TestSocket,
    request_id: &str,
    thread_id: &str,
    terminal_id: &str,
) -> (Value, bool) {
    let mut saw_exit = false;

    loop {
        let values =
            next_chunk_for(socket, request_id, "running metadata upsert after restart").await;
        let [value] = values.as_slice() else {
            panic!("expected one metadata event while waiting for running upsert, got {values:?}");
        };
        let matching_terminal = value["type"] == "upsert"
            && value["terminal"]["threadId"] == thread_id
            && value["terminal"]["terminalId"] == terminal_id;

        match value["terminal"]["status"].as_str() {
            Some("exited") if matching_terminal => saw_exit = true,
            Some("running") if matching_terminal => {
                let upsert = value.clone();
                send_ack(socket, request_id).await;
                return (upsert, saw_exit);
            }
            _ => panic!(
                "expected exited or running metadata upsert for {thread_id}/{terminal_id}, got {value:?}"
            ),
        }
        send_ack(socket, request_id).await;
    }
}

async fn next_chunk(socket: &mut TestSocket, request_id: &str) -> Vec<Value> {
    next_chunk_for(socket, request_id, "stream chunk").await
}

async fn next_chunk_for(socket: &mut TestSocket, request_id: &str, expected: &str) -> Vec<Value> {
    match next_message_for(socket, &format!("{expected} (request {request_id})")).await {
        ServerMessage::Chunk {
            request_id: actual_request_id,
            values,
        } => {
            assert_eq!(actual_request_id.as_str(), request_id);
            values
        }
        other => panic!("expected chunk for request {request_id}, got {other:?}"),
    }
}

async fn maybe_prime_terminal(socket: &mut TestSocket, thread_id: &str, terminal_id: &str) {
    if cfg!(windows) {
        assert_success(
            request(
                socket,
                "90",
                "terminal.write",
                json!({
                    "threadId": thread_id,
                    "terminalId": terminal_id,
                    "data": "\u{1b}[1;1R",
                }),
            )
            .await,
        );
    }
}

fn shell_echo_command(token: &str) -> String {
    if cfg!(windows) {
        format!("echo {token}\r\n")
    } else {
        format!("echo {token}\n")
    }
}

fn long_running_command() -> &'static str {
    if cfg!(windows) {
        "echo BIBCODE_RPC_LONG_RUNNING_READY & ping 127.0.0.1 -n 31\r\n"
    } else {
        "printf 'BIBCODE_RPC_%s\\n' 'LONG_RUNNING_READY'; sleep 30\n"
    }
}

fn environment_probe_command(output_path: &std::path::Path) -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell.exe".to_owned(),
            vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                format!(
                    "$activity = if (Get-ChildItem Env: | Where-Object {{ $_.Value -eq 'activity_instance_marker' }}) {{ 'activity' }} else {{ 'clean' }}; Set-Content -NoNewline -Path '{}' -Value \"$env:COMMAND_ONLY|$env:RUNTIME_ONLY|$env:SHARED|$activity\"",
                    output_path.display()
                ),
            ],
        )
    } else {
        (
            "/bin/sh".to_owned(),
            vec![
                "-c".to_owned(),
                format!(
                    "activity=clean; if env | grep -F 'activity_instance_marker' >/dev/null; then activity=activity; fi; printf '%s|%s|%s|%s' \"$COMMAND_ONLY\" \"$RUNTIME_ONLY\" \"$SHARED\" \"$activity\" > '{}'",
                    output_path.display()
                ),
            ],
        )
    }
}

async fn read_file_with_retry(path: &std::path::Path) -> String {
    tokio::time::timeout(TERMINAL_RPC_INTEGRATION_DEADLINE, async {
        loop {
            if let Ok(contents) = tokio::fs::read_to_string(path).await
                && !contents.is_empty()
            {
                return contents;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {}", path.display()))
}

fn long_running_output_marker() -> &'static str {
    "BIBCODE_RPC_LONG_RUNNING_READY"
}

fn long_running_label() -> &'static str {
    if cfg!(windows) { "PING" } else { "sleep" }
}

fn expected_killed_exit_code() -> Value {
    json!(1)
}

fn expected_killed_exit_signal() -> Value {
    Value::Null
}
