//! The worktree audit against a real repository, catalog and removal RPC. The profiler handle
//! is process-global, so everything runs in one `#[tokio::test]`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, OnceLock},
    time::Duration,
};

use coddy_studio_server::{
    CauseItem, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    diagnostics::{
        ProcessAttributionRegistry,
        profiler::{
            self,
            findings::fingerprint,
            runtime::ProfilerRuntime,
            worktree_audit::{AuditSources, WorktreeAuditDetector, scan_once},
        },
    },
    git::GitRepository,
    orchestration::{EngineOptions, OrchestrationEngine},
    persistence::{Database, Repositories, run_migrations},
    production::worktree_catalog_rpc::{
        WorktreeCatalogOperationRuntime, WorktreeCatalogRpcServices, register_worktree_catalog_rpc,
    },
    worktree_catalog::{WorkspaceAvailabilityRegistry, WorktreeCatalogService},
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{sync::watch, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

#[path = "support/isolated_git_config.rs"]
mod isolated_git_config;
#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use isolated_git_config::IsolatedGitConfig;
use websocket_frames::next_frame_past_heartbeat;

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const RPC_DEADLINE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn the_audit_and_removal_outcomes_reach_the_profiler() {
    let mut fixture = Fixture::new().await;
    let orphan = fixture.managed.join("orphan");
    fs::create_dir_all(&orphan).expect("orphan directory");

    // As in production: the detector starts before the server binds the catalog, and scans
    // as soon as it is bound rather than ten minutes later.
    let sources = Arc::new(OnceLock::new());
    let state = tempfile::tempdir().expect("profiler state");
    let (_enabled, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(
        state.path(),
        rx,
        vec![Box::new(WorktreeAuditDetector::new(
            sources.clone(),
            ProcessAttributionRegistry::new(),
        ))],
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(sources.set(fixture.sources()).is_ok());
    let orphan_finding = wait_for_finding(&runtime, "worktree.orphan-dir").await;
    assert_eq!(orphan_finding["severity"], "warn");
    assert!(orphan.exists(), "the scan is read-only");
    let events = shutdown_and_read_events(runtime, state.path()).await;
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "worktree.audit" && event["compared"] == true)
    );

    // From here each scan runs on demand, so every check sees exactly one scan's result.
    let state = tempfile::tempdir().expect("profiler state");
    let (_enabled, rx) = watch::channel(true);
    let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);
    timeout(RPC_DEADLINE, async {
        while runtime.status()["enabled"] != true {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the profiler turns on");
    let sources = fixture.sources();
    let processes = ProcessAttributionRegistry::new();

    let thread_id = adopt(&mut fixture).await;
    fs::write(fixture.worktree.join("dirty.txt"), "dirty\n").expect("dirty file");
    remove(&mut fixture, &thread_id, "remove-dirty", false, "2").await;
    assert_eq!(
        failure_reason(fixture.socket(), "3").await,
        "dirty-confirmation-required"
    );
    let failed = wait_for_finding(&runtime, "worktree.removal-failed").await;
    assert_eq!(failed["severity"], "warn");

    // Work admitted in the worktree that does not drain within the removal's bound.
    let admitted = fixture
        .availability
        .acquire_admission(&thread_id, [fixture.worktree.as_path()])
        .await
        .expect("admission");
    remove(&mut fixture, &thread_id, "remove-undrained", true, "4").await;
    assert_eq!(
        failure_reason(fixture.socket(), "5").await,
        "session-running"
    );
    drop(admitted);
    assert_eq!(
        removal_failures(&runtime),
        2,
        "a drain timeout is its own finding"
    );

    // A client that interrupts the removal once the server-owned operation is waiting for
    // admitted work to drain: the operation records the outcome, as an event only.
    let admitted = fixture
        .availability
        .acquire_admission(&thread_id, [fixture.worktree.as_path()])
        .await
        .expect("admission");
    remove(&mut fixture, &thread_id, "remove-interrupted", true, "6").await;
    timeout(RPC_DEADLINE, async {
        while fixture.availability.guard_thread(&thread_id).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the removal holds its removing guard");
    // Past the plan, which takes milliseconds, and into the drain, which takes 5 s.
    tokio::time::sleep(Duration::from_secs(1)).await;
    send(
        fixture.socket(),
        json!({ "_tag": "Interrupt", "requestId": "7" }),
    )
    .await;
    assert!(matches!(
        exit(fixture.socket(), "7").await,
        RpcExit::Failure { .. }
    ));
    drop(admitted);
    timeout(RPC_DEADLINE, async {
        while fixture.availability.guard_thread(&thread_id).await.is_err() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the interrupted removal ends");

    request(
        fixture.socket(),
        "8",
        "worktree.removeFromCoddyStudio",
        json!({"commandId":"detach","projectId":"project-1","threadId":thread_id}),
    )
    .await;
    success_value(fixture.socket(), "8").await;
    assert_eq!(
        removal_failures(&runtime),
        2,
        "an interrupted removal is not a finding"
    );
    // An exact retry replays the accepted result: not a new outcome.
    request(
        fixture.socket(),
        "9",
        "worktree.removeFromCoddyStudio",
        json!({"commandId":"detach","projectId":"project-1","threadId":thread_id}),
    )
    .await;
    success_value(fixture.socket(), "9").await;

    // The detached worktree stays registered and on disk: the user's own, not a leftover.
    scan_once(&sources, &processes).await;
    assert!(
        !finding_kinds(&runtime).contains(&"worktree.registration-mismatch".to_owned()),
        "{:?}",
        finding_kinds(&runtime)
    );

    // A registration whose directory is gone is a leftover.
    let leftover = fixture.managed.join("leftover");
    git(
        &fixture.git_config,
        &fixture.main,
        &[
            "worktree",
            "add",
            "-b",
            "feature/leftover",
            &leftover.to_string_lossy(),
        ],
    );
    fs::remove_dir_all(&leftover).expect("remove the leftover's directory");
    scan_once(&sources, &processes).await;
    assert_eq!(
        finding(&runtime, "worktree.registration-mismatch")["fingerprint"],
        fingerprint(
            "worktree.registration-mismatch",
            &["unowned-registration".to_owned()]
        )
    );

    // An owned process whose working directory was removed under it.
    #[cfg(target_os = "linux")]
    {
        let doomed = fixture.managed.join("doomed");
        fs::create_dir(&doomed).expect("doomed directory");
        let mut child = Command::new("sleep")
            .arg("60")
            .current_dir(&doomed)
            .spawn()
            .expect("child process");
        fs::remove_dir(&doomed).expect("remove the child's working directory");
        scan_once(&sources, &processes).await;
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            finding(&runtime, "worktree.held-open")["fingerprint"],
            fingerprint("worktree.held-open", &["removed".to_owned()])
        );
    }

    let events = shutdown_and_read_events(runtime, state.path()).await;
    let removals: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "worktree.removal")
        .map(|event| {
            assert!(event["durationMs"].is_u64());
            (
                event["mode"].as_str().unwrap_or_default(),
                event["ok"].as_bool().unwrap_or_default(),
                event["reason"].as_str(),
                event["drainTimedOut"].as_bool().unwrap_or_default(),
                event["cancelled"].as_bool().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        removals.len(),
        4,
        "the replay is not recorded: {removals:?}"
    );
    for expected in [
        (
            "delete-git-worktree",
            false,
            Some("dirty-confirmation-required"),
            false,
            false,
        ),
        (
            "delete-git-worktree",
            false,
            Some("session-running"),
            true,
            false,
        ),
        ("detach-only", true, None, false, false),
    ] {
        assert!(removals.contains(&expected), "{expected:?} in {removals:?}");
    }
    assert!(
        removals
            .iter()
            .any(|&(mode, ok, _, _, cancelled)| mode == "delete-git-worktree" && !ok && cancelled),
        "{removals:?}"
    );
    // Each failure finding cites the `worktree.removal` event of its occurrence.
    let removal_ids: Vec<&Value> = events
        .iter()
        .filter(|event| event["kind"] == "worktree.removal")
        .map(|event| &event["eventId"])
        .collect();
    let cited: Vec<&Value> = events
        .iter()
        .filter(|line| line["kind"] == "worktree.removal-failed" && line.get("evidence").is_some())
        .map(|finding| &finding["evidence"]["eventIds"][0])
        .collect();
    assert!(!cited.is_empty());
    assert!(
        cited
            .iter()
            .all(|id| id.is_string() && removal_ids.contains(id)),
        "{cited:?} not among {removal_ids:?}"
    );
    let scans: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "worktree.audit")
        .collect();
    assert!(!scans.is_empty());
    // Windows reads no working directories, so its scans report that as null.
    let processes_read = if cfg!(windows) {
        Value::Null
    } else {
        json!(true)
    };
    assert!(
        scans
            .iter()
            .all(|scan| scan["compared"] == true && scan["processesRead"] == processes_read),
        "{scans:?}"
    );
    fixture.shutdown().await;
}

fn finding(runtime: &ProfilerRuntime, kind: &str) -> Value {
    runtime.status()["findings"]
        .as_array()
        .and_then(|findings| findings.iter().find(|finding| finding["kind"] == kind))
        .cloned()
        .unwrap_or_else(|| panic!("no {kind} finding: {:?}", finding_kinds(runtime)))
}

async fn wait_for_finding(runtime: &ProfilerRuntime, kind: &str) -> Value {
    timeout(RPC_DEADLINE, async {
        loop {
            if let Some(finding) = runtime.status()["findings"]
                .as_array()
                .and_then(|findings| findings.iter().find(|finding| finding["kind"] == kind))
            {
                return finding.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} finding: {:?}", finding_kinds(runtime)))
}

fn removal_failures(runtime: &ProfilerRuntime) -> usize {
    finding_kinds(runtime)
        .iter()
        .filter(|kind| *kind == "worktree.removal-failed")
        .count()
}

fn finding_kinds(runtime: &ProfilerRuntime) -> Vec<String> {
    runtime.status()["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|finding| finding["kind"].as_str().map(str::to_owned))
        .collect()
}

/// Shutdown flushes the writer, so every queued event is on disk afterwards.
async fn shutdown_and_read_events(runtime: ProfilerRuntime, state_dir: &Path) -> Vec<Value> {
    runtime.shutdown().await;
    fs::read_dir(profiler::profiler_dir(state_dir))
        .expect("profiler directory")
        .filter_map(|entry| fs::read_to_string(entry.expect("entry").path()).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .collect()
}

/// Plans a Git deletion of the worktree (request `id`) and sends it (request `id + 1`).
async fn remove(
    fixture: &mut Fixture,
    thread_id: &str,
    command_id: &str,
    force_dirty: bool,
    id: &str,
) {
    request(
        fixture.socket(),
        id,
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), id).await;
    let next = (id.parse::<u32>().expect("numeric request ID") + 1).to_string();
    request(
        fixture.socket(),
        &next,
        "worktree.remove",
        json!({
            "commandId":command_id,
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"delete-git-worktree",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":force_dirty,
            "confirmRepositoryWidePrune":false
        }),
    )
    .await;
}

async fn adopt(fixture: &mut Fixture) -> String {
    request(
        fixture.socket(),
        "0",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "0").await;
    let candidate = snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["eligibleForAdoption"] == true)
        .expect("eligible candidate")
        .clone();
    request(
        fixture.socket(),
        "1",
        "worktree.adopt",
        json!({
            "commandId":"adopt-worktree",
            "projectId":"project-1",
            "worktreeKey":candidate["worktreeKey"],
            "expectedGeneration":snapshot["generation"],
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    success_value(fixture.socket(), "1").await["threadId"]
        .as_str()
        .expect("adopted thread ID")
        .to_owned()
}

/// A repository with one unadopted worktree where Coddy Studio creates its own, its project, the
/// catalog and the removal RPC.
struct Fixture {
    git_config: IsolatedGitConfig,
    _root: TempDir,
    main: PathBuf,
    /// Where Coddy Studio creates the repository's worktrees: `<root>/.coddy-studio-worktrees/main`.
    managed: PathBuf,
    worktree: PathBuf,
    repositories: Repositories,
    catalog: WorktreeCatalogService,
    availability: WorkspaceAvailabilityRegistry,
    operations: WorktreeCatalogOperationRuntime,
    engine: OrchestrationEngine,
    handle: Option<coddy_studio_server::ServerHandle>,
    socket: Option<TestSocket>,
}

impl Fixture {
    async fn new() -> Self {
        let git_config = IsolatedGitConfig::new();
        let git = |cwd: &Path, args: &[&str]| git(&git_config, cwd, args);
        let root = tempfile::tempdir().expect("fixture root");
        let main = root.path().join("main");
        let managed = root.path().join(".coddy-studio-worktrees/main");
        let worktree = managed.join("feature-external");
        fs::create_dir(&main).expect("primary directory");
        git(&main, &["init", "--initial-branch", "main"]);
        git(&main, &["config", "user.email", "audit@example.invalid"]);
        git(&main, &["config", "user.name", "Audit Test"]);
        fs::write(main.join("README.md"), "audit fixture\n").expect("fixture file");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "initial"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/external",
                &worktree.to_string_lossy(),
            ],
        );

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("orchestration");
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type": "project.create",
                    "commandId": "project-create",
                    "projectId": "project-1",
                    "title": "Audit",
                    "workspaceRoot": main,
                    "defaultModelSelection": null,
                    "createdAt": "2026-10-09T00:00:00Z"
                }))
                .expect("project command"),
            )
            .await
            .expect("project created");
        let repositories = engine.repositories();
        let availability = WorkspaceAvailabilityRegistry::new();
        let catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            Arc::new(GitRepository::default()),
            availability.clone(),
        );
        let services = WorktreeCatalogRpcServices::new(catalog.clone(), engine.clone());
        let operations = services.operation_runtime();
        let mut registry = RpcRegistry::empty();
        register_worktree_catalog_rpc(&mut registry, services);
        let config = ServerConfig::new(root.path().join("server"))
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        let handle = ServerRuntime::start_with_registry(config, registry)
            .await
            .expect("server");
        let socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket")
            .0;
        Self {
            git_config,
            _root: root,
            main,
            managed,
            worktree,
            repositories,
            catalog,
            availability,
            operations,
            engine,
            handle: Some(handle),
            socket: Some(socket),
        }
    }

    fn sources(&self) -> AuditSources {
        AuditSources {
            catalog: self.catalog.clone(),
            repositories: self.repositories.clone(),
        }
    }

    fn socket(&mut self) -> &mut TestSocket {
        self.socket.as_mut().expect("active socket")
    }

    async fn shutdown(mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None).await;
        }
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            handle.join().await.expect("server shutdown");
        }
        self.operations.shutdown().await;
        self.engine.shutdown().await;
    }
}

async fn request(socket: &mut TestSocket, id: &str, tag: &str, payload: Value) {
    send(
        socket,
        json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] }),
    )
    .await;
}

async fn send(socket: &mut TestSocket, message: Value) {
    socket
        .send(Message::Text(message.to_string().into()))
        .await
        .expect("send message");
}

async fn exit(socket: &mut TestSocket, request_id: &str) -> RpcExit {
    let message = timeout(RPC_DEADLINE, next_frame_past_heartbeat(socket))
        .await
        .expect("server response timeout")
        .expect("WebSocket open")
        .expect("WebSocket frame");
    let Message::Text(text) = message else {
        panic!("expected a text frame: {message:?}");
    };
    let ServerMessage::Exit {
        request_id: actual,
        exit,
    } = serde_json::from_str(&text).expect("server message")
    else {
        panic!("expected an exit: {text}");
    };
    assert_eq!(actual.as_str(), request_id);
    exit
}

async fn success_value(socket: &mut TestSocket, request_id: &str) -> Value {
    match exit(socket, request_id).await {
        RpcExit::Success { value: Some(value) } => value,
        other => panic!("expected success: {other:?}"),
    }
}

async fn failure_reason(socket: &mut TestSocket, request_id: &str) -> String {
    let RpcExit::Failure { cause } = exit(socket, request_id).await else {
        panic!("expected a failure");
    };
    cause
        .iter()
        .find_map(|item| match item {
            CauseItem::Fail { error } => error["reason"].as_str().map(str::to_owned),
            _ => None,
        })
        .expect("a typed failure reason")
}

/// Runs a fixture Git command without the developer's configuration or Git environment.
fn git(config: &IsolatedGitConfig, cwd: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    config.apply_to_command(&mut command);
    let output = command.output().expect("run Git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
