#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use bibcode_server::{
    CauseItem, RequestId, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
    },
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use bibcode_server::production::git_vcs::{
    GIT_VCS_STREAM_METHODS, GIT_VCS_UNARY_METHODS, GitVcsRpcServices, register_git_vcs_rpc,
};

#[path = "support/isolated_git_config.rs"]
mod isolated_git_config;
use isolated_git_config::IsolatedGitConfig;
#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

fn git_vcs_services(temp: &TempDir) -> GitVcsRpcServices {
    let hosts = Arc::new(bibcode_server::source_control::ProviderHosts::default());
    let repository =
        Arc::new(bibcode_server::git::GitRepository::default().with_provider_hosts(hosts.clone()));
    GitVcsRpcServices::with_repository(repository, hosts)
        .with_hosting_executable_dir_for_integration_test(
            hermetic_providers::missing_hosting_executable_dir(temp.path()),
        )
}

const ISOLATED_GIT_TEST: &str = "BIBCODE_PRODUCTION_GIT_VCS_RPC_ISOLATED";
static ISOLATED_GIT_TEST_LOCK: Mutex<()> = Mutex::new(());
const GIT_STATUS_INTEGRATION_DEADLINE: Duration = Duration::from_secs(15);
const STACKED_ACTION_COMPLETION_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);
const GIT_RPC_RESPONSE_DEADLOCK_BOUND: Duration = Duration::from_secs(60);
const MAX_PARALLEL_GIT_RPC_FIXTURES: usize = 12;

fn start_git_status_integration_deadline() -> (Instant, Instant) {
    let started = Instant::now();
    (started, started + GIT_STATUS_INTEGRATION_DEADLINE)
}

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct GitServerHarness {
    _parallelism_permit: OwnedSemaphorePermit,
    handle: Option<bibcode_server::ServerHandle>,
    socket: Option<TestSocket>,
}

#[tokio::test]
async fn workspace_unavailable_rejects_git_status_and_mutation_before_process_launch() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("server state");
    let workspace = TempDir::new().expect("guarded workspace");
    let availability = WorkspaceAvailabilityRegistry::new();
    assert!(
        availability
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-guarded".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 4,
                path: workspace.path().to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingUnregistered,
            })
            .await
            .expect("physical identity resolves")
    );
    let services = git_vcs_services(&temp).with_availability_registry(availability);
    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, services);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    for (id, method, payload) in [
        ("1", "vcs.refreshStatus", json!({"cwd":workspace.path()})),
        (
            "2",
            "vcs.createRef",
            json!({"cwd":workspace.path(),"name":"blocked"}),
        ),
        ("3", "subscribeVcsStatus", json!({"cwd":workspace.path()})),
        (
            "5",
            "subscribeVcsStatusSummary",
            json!({"cwd":workspace.path()}),
        ),
        (
            "4",
            "git.runStackedAction",
            json!({
                "actionId":"action-1","cwd":workspace.path(),"action":"sync"
            }),
        ),
    ] {
        request(&mut socket, id, method, payload).await;
        let error = failure_value(&mut socket, id).await;
        assert_eq!(error["_tag"], "WorkspaceUnavailableError");
        assert_eq!(error["threadId"], "thread-guarded");
        assert_eq!(error["availability"], "missing-unregistered");
    }

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

impl GitServerHarness {
    async fn start(temp: &TempDir, parallelism_permit: OwnedSemaphorePermit) -> Self {
        let mut registry = RpcRegistry::empty();
        register_git_vcs_rpc(&mut registry, git_vcs_services(temp));
        let handle = ServerRuntime::start_with_registry(test_config(temp), registry)
            .await
            .expect("server starts");
        let (socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket connects");
        Self {
            _parallelism_permit: parallelism_permit,
            handle: Some(handle),
            socket: Some(socket),
        }
    }

    fn socket(&mut self) -> &mut TestSocket {
        self.socket.as_mut().expect("active test socket")
    }

    /// Closes the current socket, as a dropped connection does, and opens a new one.
    async fn reconnect(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None).await;
        }
        let handle = self.handle.as_ref().expect("running server");
        let (socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket reconnects");
        self.socket = Some(socket);
    }

    async fn shutdown(mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None).await;
        }
        if let Some(handle) = self.handle.take() {
            handle.shutdown();
            let _ = handle.join().await;
        }
    }
}

impl Drop for GitServerHarness {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
    }
}

#[test]
fn registrar_owns_the_complete_git_vcs_rpc_surface() {
    assert_eq!(
        GIT_VCS_UNARY_METHODS,
        [
            "shell.openInEditor",
            "vcs.pull",
            "vcs.refreshStatus",
            "vcs.listRefs",
            "vcs.listCommits",
            "vcs.clone",
            "vcs.cancelClone",
            "vcs.createRef",
            "vcs.switchRef",
            "vcs.init",
            "vcs.stageFiles",
            "vcs.unstageFiles",
            "vcs.discardFiles",
            "vcs.generateCommitMessage",
            "git.resolvePullRequest",
            "git.preparePullRequestThread",
            "server.discoverSourceControl",
            "sourceControl.lookupRepository",
            "sourceControl.cloneRepository",
            "sourceControl.publishRepository",
        ]
    );
    assert_eq!(
        GIT_VCS_STREAM_METHODS,
        [
            "subscribeVcsStatus",
            "subscribeVcsStatusSummary",
            "git.runStackedAction"
        ]
    );
}

#[tokio::test]
async fn raw_vcs_create_worktree_is_unavailable_without_git_side_effects() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    commit_file(repository.path(), "tracked.txt", "base\n", "initial");
    let worktree_path = temp.path().join("raw-created");
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "906",
        "vcs.createWorktree",
        json!({
            "cwd": repository.path(),
            "refName": "main",
            "newRefName": "feature/raw-created",
            "baseRefName": "main",
            "path": worktree_path,
        }),
    )
    .await;
    let response = next_server_message(server.socket()).await;
    assert!(
        matches!(
            &response,
            ServerMessage::Defect { defect }
                if defect.as_str().is_some_and(|detail| {
                    detail.contains("Unknown request tag")
                        && detail.contains("vcs.createWorktree")
                })
        ),
        "the retired raw creation method must be unavailable: {response:?}"
    );
    assert!(!worktree_path.exists());
    assert!(
        git_stdout(&repository, &["branch", "--list", "feature/raw-created"])
            .trim()
            .is_empty()
    );
    server.shutdown().await;
}

#[tokio::test]
async fn registers_native_vcs_handlers_with_unchanged_wire_shapes() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    let cwd = repository.path().to_string_lossy().into_owned();
    request(&mut socket, "1", "vcs.init", json!({ "cwd": cwd })).await;
    assert_success_eq(&mut socket, "1", Value::Null).await;

    request(
        &mut socket,
        "2",
        "vcs.listRefs",
        json!({ "cwd": cwd, "limit": 25 }),
    )
    .await;
    let result = success_value(&mut socket, "2").await;
    assert_eq!(result["isRepo"], true);
    assert_eq!(result["refs"], json!([]));
    assert_eq!(result["nextCursor"], Value::Null);
    assert_eq!(result["totalCount"], 0);

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn generate_commit_message_is_empty_when_there_are_no_changes() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    let (_parallelism_permit, handle, mut socket) =
        start_git_server(&temp, parallelism_permit).await;

    request(
        &mut socket,
        "6",
        "vcs.generateCommitMessage",
        json!({ "cwd": repository.path().to_string_lossy() }),
    )
    .await;
    assert_success_eq(&mut socket, "6", json!({ "message": "" })).await;

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn vcs_status_stream_is_bounded_and_cancellable() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .status()
        .expect("git starts")
        .success()
        .then_some(())
        .expect("git init succeeds");

    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    request(
        &mut socket,
        "7",
        "subscribeVcsStatus",
        json!({ "cwd": repository.path().to_string_lossy() }),
    )
    .await;
    let snapshot = next_server_message(&mut socket).await;
    assert!(matches!(
        snapshot,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "7"
                && values.len() == 1
                && values[0]["_tag"] == "snapshot"
                && values[0]["local"]["isRepo"] == true
    ));

    send_json(
        &mut socket,
        json!({ "_tag": "Interrupt", "requestId": "7" }),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit { request_id, exit: RpcExit::Failure { cause } }
            if request_id.as_str() == "7"
                && cause == vec![CauseItem::Interrupt { fiber_id: None }]
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn vcs_summary_stream_publishes_the_lightweight_shape_and_is_cancellable() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    commit_file(repository.path(), "tracked.txt", "base\n", "initial");
    fs::write(repository.path().join("tracked.txt"), "changed\n").expect("dirty fixture");

    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    request(
        &mut socket,
        "71",
        "subscribeVcsStatusSummary",
        json!({ "cwd": repository.path() }),
    )
    .await;
    let summary = next_server_message(&mut socket).await;
    assert!(
        matches!(
            &summary,
            ServerMessage::Chunk { request_id, values }
                if request_id.as_str() == "71"
                    && values.len() == 1
                    && values[0]["isRepo"] == true
                    && values[0]["refName"] == "main"
                    && values[0]["detachedHead"].is_null()
                    && values[0]["hasWorkingTreeChanges"] == true
                    && values[0]["pr"].is_null()
                    && values[0]["stale"] == false
                    && values[0].get("workingTree").is_none()
        ),
        "unexpected summary response: {summary:?}"
    );

    send_json(
        &mut socket,
        json!({ "_tag": "Interrupt", "requestId": "71" }),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit { request_id, exit: RpcExit::Failure { cause } }
            if request_id.as_str() == "71"
                && cause == vec![CauseItem::Interrupt { fiber_id: None }]
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn automatic_git_fetch_setting_changes_apply_without_restart() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "automatic_git_fetch_setting_changes_apply_without_restart",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary Git fixture");
    let remote = root.path().join("remote.git");
    fs::create_dir(&remote).expect("bare remote directory");
    run_git_in(&remote, &["init", "--bare", "--initial-branch=main"]);

    let publisher = root.path().join("publisher");
    fs::create_dir(&publisher).expect("publisher directory");
    initialize_repository_in(&publisher);
    commit_file(&publisher, "tracked.txt", "base\n", "initial");
    let remote_url = local_file_url(&remote);
    run_git_in(&publisher, &["remote", "add", "origin", &remote_url]);
    run_git_in(&publisher, &["push", "-u", "origin", "main"]);

    run_git_in(root.path(), &["clone", "--quiet", &remote_url, "consumer"]);
    let consumer = root.path().join("consumer");
    let initial_remote_ref = git_stdout_in(&consumer, &["rev-parse", "origin/main"]);

    let config = test_config(&temp);
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({
            "automaticGitFetchInterval": 0,
            "enableProviderUpdateChecks": false,
            "providers": {
                "codex": { "enabled": false },
                "claudeAgent": { "enabled": false },
                "cursor": { "enabled": false },
                "grok": { "enabled": false },
                "opencode": { "enabled": false }
            }
        }),
    );
    let handle = ServerRuntime::start(config).await.expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    request(
        &mut socket,
        "8",
        "subscribeVcsStatus",
        json!({ "cwd": consumer }),
    )
    .await;
    let _snapshot = next_server_message(&mut socket).await;
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "8" })).await;
    tokio::time::timeout(Duration::from_secs(10), next_server_message(&mut socket))
        .await
        .expect("initial remote status is published");
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "8" })).await;

    commit_file(
        &publisher,
        "tracked.txt",
        "remote change\n",
        "remote update",
    );
    run_git_in(&publisher, &["push"]);
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert_eq!(
        git_stdout_in(&consumer, &["rev-parse", "origin/main"]),
        initial_remote_ref,
        "a zero automatic fetch interval must not update the remote-tracking ref"
    );

    request(
        &mut socket,
        "9",
        "server.updateSettings",
        json!({ "patch": { "automaticGitFetchInterval": 100 } }),
    )
    .await;
    let _updated_settings = success_value(&mut socket, "9").await;

    timeout(GIT_STATUS_INTEGRATION_DEADLINE, async {
        loop {
            let message = next_server_message(&mut socket).await;
            send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "8" })).await;
            if let ServerMessage::Chunk { request_id, values } = message
                && request_id.as_str() == "8"
                && values[0]["_tag"] == "remoteUpdated"
            {
                break;
            }
        }
    })
    .await
    .expect("changing the automatic fetch interval must publish remote reconciliation");
    assert_ne!(
        git_stdout_in(&consumer, &["rev-parse", "origin/main"]),
        initial_remote_ref,
        "changing the automatic fetch interval must take effect without restarting"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn native_watcher_publishes_external_worktree_and_head_changes_to_status_subscribers() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    commit_file(repository.path(), "tracked.txt", "base\n", "initial");
    let (_parallelism_permit, handle, mut status_socket) =
        start_git_server(&temp, parallelism_permit).await;
    let cwd = repository.path().to_string_lossy();

    request(
        &mut status_socket,
        "701",
        "subscribeVcsStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let snapshot = next_server_message(&mut status_socket).await;
    assert!(matches!(
        snapshot,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "701"
                && values[0]["_tag"] == "snapshot"
                && values[0]["local"]["refName"] == "main"
    ));
    send_json(
        &mut status_socket,
        json!({ "_tag": "Ack", "requestId": "701" }),
    )
    .await;
    let initial_remote =
        next_server_message_with_timeout(&mut status_socket, Duration::from_secs(45)).await;
    assert!(matches!(
        initial_remote,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "701" && values[0]["_tag"] == "remoteUpdated"
    ));
    send_json(
        &mut status_socket,
        json!({ "_tag": "Ack", "requestId": "701" }),
    )
    .await;

    use std::io::Write;
    let mut tracked = fs::OpenOptions::new()
        .append(true)
        .open(repository.path().join("tracked.txt"))
        .expect("open tracked worktree file");
    tracked
        .write_all(b"changed externally\n")
        .expect("size-changing external worktree edit");
    tracked.sync_all().expect("durable external worktree edit");
    drop(tracked);
    let dirty_update = timeout(GIT_STATUS_INTEGRATION_DEADLINE, async {
        loop {
            let message = next_server_message(&mut status_socket).await;
            send_json(
                &mut status_socket,
                json!({ "_tag": "Ack", "requestId": "701" }),
            )
            .await;
            if let ServerMessage::Chunk { request_id, values } = message
                && request_id.as_str() == "701"
                && values[0]["_tag"] == "localUpdated"
                && values[0]["local"]["hasWorkingTreeChanges"] == true
            {
                break values;
            }
        }
    })
    .await
    .expect("native watcher should publish an external worktree edit");
    assert!(
        dirty_update[0]["local"]["workingTree"]["files"]
            .as_array()
            .is_some_and(|files| files.iter().any(|file| file["path"] == "tracked.txt"))
    );

    run_git(&repository, &["add", "--", "tracked.txt"]);
    let index_update = timeout(GIT_STATUS_INTEGRATION_DEADLINE, async {
        loop {
            let message = next_server_message(&mut status_socket).await;
            send_json(
                &mut status_socket,
                json!({ "_tag": "Ack", "requestId": "701" }),
            )
            .await;
            if let ServerMessage::Chunk { request_id, values } = message
                && request_id.as_str() == "701"
                && values[0]["_tag"] == "localUpdated"
                && values[0]["local"]["workingTree"]["files"]
                    .as_array()
                    .is_some_and(|files| {
                        files
                            .iter()
                            .any(|file| file["path"] == "tracked.txt" && file["area"] == "staged")
                    })
            {
                break values;
            }
        }
    })
    .await
    .expect("native watcher should publish an external index change");
    assert_eq!(
        index_update[0]["local"]["workingTree"]["files"][0]["area"],
        "staged"
    );

    run_git(
        &repository,
        &["switch", "--quiet", "-c", "feature/external"],
    );
    let head_update = timeout(GIT_STATUS_INTEGRATION_DEADLINE, async {
        loop {
            let message = next_server_message(&mut status_socket).await;
            send_json(
                &mut status_socket,
                json!({ "_tag": "Ack", "requestId": "701" }),
            )
            .await;
            if let ServerMessage::Chunk { request_id, values } = message
                && request_id.as_str() == "701"
                && values[0]["_tag"] == "localUpdated"
                && values[0]["local"]["refName"] == "feature/external"
            {
                break values;
            }
        }
    })
    .await
    .expect("native watcher should publish an external HEAD/index change");
    assert_eq!(head_update[0]["local"]["refName"], "feature/external");

    status_socket
        .close(None)
        .await
        .expect("close status socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn project_file_save_publishes_git_status_without_waiting_for_the_fallback_poller() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    commit_file(repository.path(), "tracked.txt", "base\n", "initial");

    let handle = ServerRuntime::start(test_config(&temp))
        .await
        .expect("production server starts");
    let (mut status_socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("status WebSocket connects");
    let (mut write_socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("write WebSocket connects");
    let cwd = repository.path().to_string_lossy();

    request(
        &mut status_socket,
        "703",
        "subscribeVcsStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let snapshot = next_server_message_for(
        &mut status_socket,
        "initial clean VCS status snapshot after subscription",
    )
    .await;
    assert!(matches!(
        snapshot,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "703"
                && values[0]["_tag"] == "snapshot"
                && values[0]["local"]["hasWorkingTreeChanges"] == false
    ));
    send_json(
        &mut status_socket,
        json!({ "_tag": "Ack", "requestId": "703" }),
    )
    .await;

    let initial_remote = next_server_message_for(
        &mut status_socket,
        "initial remote VCS status after subscription",
    )
    .await;
    assert!(matches!(
        initial_remote,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "703" && values[0]["_tag"] == "remoteUpdated"
    ));
    send_json(
        &mut status_socket,
        json!({ "_tag": "Ack", "requestId": "703" }),
    )
    .await;

    let (publication_started, publication_deadline) = start_git_status_integration_deadline();
    let local_update = timeout_at(publication_deadline, async {

        request(
            &mut write_socket,
            "704",
            "projects.writeFile",
            json!({
                "cwd": cwd,
                "relativePath": "tracked.txt",
                "contents": "changed in editor\n"
            }),
        )
        .await;
        assert_success_eq_for(
            &mut write_socket,
            "704",
            json!({ "relativePath": "tracked.txt" }),
            "projects.writeFile success after mutation notification",
        )
        .await;

        loop {
            let message = next_server_message_for(
                &mut status_socket,
                "event-driven dirty local VCS status after project file save",
            )
            .await;
            if let ServerMessage::Chunk { request_id, values } = message
                && request_id.as_str() == "703"
                && values[0]["_tag"] == "localUpdated"
                && values[0]["local"]["hasWorkingTreeChanges"] == true
            {
                break values;
            }
            send_json(
                &mut status_socket,
                json!({ "_tag": "Ack", "requestId": "703" }),
            )
            .await;
        }
    })
    .await
    .expect(
        "a successful project file save should publish Git status within one integration deadline before the 60-second safety read",
    );
    eprintln!(
        "event-driven project file Git status published in {:?}",
        publication_started.elapsed()
    );
    assert_eq!(
        local_update[0]["local"]["workingTree"]["files"][0]["path"],
        "tracked.txt"
    );

    write_socket
        .close(None)
        .await
        .expect("close write WebSocket");
    status_socket
        .close(None)
        .await
        .expect("close status WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_commit_stream_finishes_with_a_decodable_success_event() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "BiBCode Test"],
        vec!["config", "user.email", "bibcode@example.invalid"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
    }
    std::fs::write(repository.path().join("committed.txt"), "committed\n")
        .expect("write commit fixture");
    assert!(
        std::process::Command::new("git")
            .args(["add", "committed.txt"])
            .current_dir(repository.path())
            .status()
            .expect("git add starts")
            .success()
    );

    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let cwd = repository.path().to_string_lossy();
    request(
        &mut socket,
        "8",
        "git.runStackedAction",
        json!({
            "actionId": "wire-action-1",
            "cwd": cwd,
            "action": "commit",
            "commitMessage": "test: commit over stream",
            "commitStagedIndexAsIs": true,
        }),
    )
    .await;

    let started = next_server_message(&mut socket).await;
    assert!(matches!(
        started,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "8"
                && values.len() == 1
                && values[0]["kind"] == "action_started"
                && values[0]["actionId"] == "wire-action-1"
                && values[0]["cwd"] == cwd.as_ref()
    ));
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "8" })).await;
    let finished = next_server_message(&mut socket).await;
    assert!(matches!(
        finished,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "8"
                && values.len() == 1
                && values[0]["kind"] == "action_finished"
                && values[0]["result"]["action"] == "commit"
                && values[0]["result"]["commit"]["status"] == "created"
                && values[0]["result"]["toast"]["cta"]["kind"] == "none"
    ));
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "8" })).await;
    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Exit { request_id, exit: RpcExit::Success { value: None } }
            if request_id.as_str() == "8"
    ));

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_commit_generates_a_message_when_the_ui_leaves_it_empty() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "BiBCode Test"],
        vec!["config", "user.email", "bibcode@example.invalid"],
        vec!["config", "core.autocrlf", "false"],
        vec!["config", "diff.mnemonicPrefix", "true"],
        vec!["config", "diff.noprefix", "false"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
    }
    std::fs::write(repository.path().join("generated.txt"), "generated\n")
        .expect("write commit fixture");
    assert!(
        std::process::Command::new("git")
            .args(["add", "generated.txt"])
            .current_dir(repository.path())
            .status()
            .expect("git add starts")
            .success()
    );

    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let cwd = repository.path().to_string_lossy();
    request(
        &mut socket,
        "9",
        "git.runStackedAction",
        json!({
            "actionId": "wire-action-generated",
            "cwd": cwd,
            "action": "commit",
            "commitStagedIndexAsIs": true,
        }),
    )
    .await;

    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "9"
                && values.len() == 1
                && values[0]["kind"] == "action_started"
    ));
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "9" })).await;
    let finished = next_server_message(&mut socket).await;
    assert!(
        matches!(
            &finished,
            ServerMessage::Chunk { request_id, values }
                if request_id.as_str() == "9"
                    && values.len() == 1
                    && values[0]["kind"] == "action_finished"
                    && values[0]["result"]["commit"]["status"] == "created"
                    && values[0]["result"]["commit"]["subject"] == "Update generated.txt"
        ),
        "expected a created commit with a generated subject, got {finished:?}"
    );

    let subject = std::process::Command::new("git")
        .args(["log", "-1", "--pretty=%s"])
        .current_dir(repository.path())
        .output()
        .expect("git log starts");
    assert!(subject.status.success());
    assert_eq!(
        String::from_utf8_lossy(&subject.stdout).trim(),
        "Update generated.txt"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_feature_branch_commit_creates_and_switches_the_branch_first() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    for args in [
        vec!["init", "--quiet", "--initial-branch=main"],
        vec!["config", "user.name", "BiBCode Test"],
        vec!["config", "user.email", "bibcode@example.invalid"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
    }
    std::fs::write(repository.path().join("base.txt"), "base\n").expect("write base fixture");
    for args in [
        vec!["add", "base.txt"],
        vec!["commit", "--quiet", "-m", "base"],
        vec![
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
    }
    let original_branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repository.path())
        .output()
        .expect("read original branch");
    let original_branch = String::from_utf8_lossy(&original_branch.stdout)
        .trim()
        .to_owned();
    std::fs::write(repository.path().join("feature.txt"), "feature\n")
        .expect("write feature fixture");
    assert!(
        std::process::Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(repository.path())
            .status()
            .expect("git add starts")
            .success()
    );

    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let cwd = repository.path().to_string_lossy();
    request(
        &mut socket,
        "10",
        "git.runStackedAction",
        json!({
            "actionId": "wire-action-feature",
            "cwd": cwd,
            "action": "commit",
            "commitMessage": "Update feature.txt",
            "featureBranch": true,
            "commitStagedIndexAsIs": true,
        }),
    )
    .await;

    assert!(matches!(
        next_server_message_for(&mut socket, "feature-branch action_started").await,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "10"
                && values.len() == 1
                && values[0]["kind"] == "action_started"
                && values[0]["phases"] == json!(["branch", "commit"])
    ));
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "10" })).await;
    let finished = next_server_message_for(&mut socket, "feature-branch action_finished").await;
    let ServerMessage::Chunk { request_id, values } = finished else {
        panic!("expected action_finished chunk");
    };
    assert_eq!(request_id.as_str(), "10");
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["kind"], "action_finished");
    assert_eq!(values[0]["result"]["branch"]["status"], "created");
    let created_branch = values[0]["result"]["branch"]["name"]
        .as_str()
        .expect("created branch name");
    assert_eq!(created_branch, "feature/update-feature-txt");
    assert_ne!(created_branch, original_branch);

    let current_branch = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repository.path())
        .output()
        .expect("read current branch");
    assert!(current_branch.status.success());
    assert_eq!(
        String::from_utf8_lossy(&current_branch.stdout).trim(),
        created_branch
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_commit_as_is_preserves_newer_unstaged_edits() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.name", "BiBCode Test"],
        vec!["config", "user.email", "bibcode@example.invalid"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
    }
    let tracked = repository.path().join("tracked.txt");
    std::fs::write(&tracked, "base\n").expect("write base fixture");
    for args in [
        vec!["add", "tracked.txt"],
        vec!["commit", "--quiet", "-m", "base"],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .expect("git starts")
                .success()
        );
    }
    std::fs::write(&tracked, "staged\n").expect("write staged fixture");
    assert!(
        std::process::Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(repository.path())
            .status()
            .expect("git add starts")
            .success()
    );
    std::fs::write(&tracked, "unstaged\n").expect("write unstaged fixture");

    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(&temp));
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let cwd = repository.path().to_string_lossy();
    request(
        &mut socket,
        "11",
        "git.runStackedAction",
        json!({
            "actionId": "wire-action-index-as-is",
            "cwd": cwd,
            "action": "commit",
            "commitMessage": "test: preserve the staged snapshot",
            "filePaths": ["tracked.txt"],
            "commitStagedIndexAsIs": true,
        }),
    )
    .await;

    assert!(matches!(
        next_server_message(&mut socket).await,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "11"
                && values.len() == 1
                && values[0]["kind"] == "action_started"
    ));
    send_json(&mut socket, json!({ "_tag": "Ack", "requestId": "11" })).await;
    assert!(matches!(
        next_server_message_with_timeout(
            &mut socket,
            STACKED_ACTION_COMPLETION_INTEGRATION_DEADLINE,
        )
        .await,
        ServerMessage::Chunk { request_id, values }
            if request_id.as_str() == "11"
                && values.len() == 1
                && values[0]["kind"] == "action_finished"
    ));

    let committed = std::process::Command::new("git")
        .args(["show", "HEAD:tracked.txt"])
        .current_dir(repository.path())
        .output()
        .expect("git show starts");
    assert!(committed.status.success());
    assert_eq!(String::from_utf8_lossy(&committed.stdout), "staged\n");
    assert_eq!(
        std::fs::read_to_string(&tracked).expect("read worktree"),
        "unstaged\n"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_selected_commit_excludes_unrelated_staged_files() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    std::fs::write(repository.path().join("a.txt"), "base a\n").expect("write base a");
    std::fs::write(repository.path().join("b.txt"), "base b\n").expect("write base b");
    run_git(&repository, &["add", "-A"]);
    run_git(&repository, &["commit", "--quiet", "-m", "base"]);

    std::fs::write(repository.path().join("a.txt"), "selected a\n").expect("write selected a");
    std::fs::write(repository.path().join("b.txt"), "unrelated b\n").expect("write unrelated b");
    run_git(&repository, &["add", "b.txt"]);

    let (_parallelism_permit, handle, mut socket) =
        start_git_server(&temp, parallelism_permit).await;
    let result = run_stacked_action(
        &mut socket,
        "12",
        json!({
            "actionId": "wire-action-selected",
            "cwd": repository.path().to_string_lossy(),
            "action": "commit",
            "commitMessage": "test: commit only the selection",
            "filePaths": ["a.txt"],
        }),
    )
    .await;
    assert_eq!(result["kind"], "action_finished");
    assert_eq!(result["result"]["commit"]["status"], "created");

    assert_eq!(
        git_stdout(&repository, &["show", "HEAD:a.txt"]),
        "selected a\n"
    );
    assert_eq!(git_stdout(&repository, &["show", "HEAD:b.txt"]), "base b\n");
    assert_eq!(
        git_stdout(&repository, &["status", "--short"]),
        " M b.txt\n"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_commit_without_paths_stages_all_changes() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    std::fs::write(repository.path().join("tracked.txt"), "base\n").expect("write base");
    run_git(&repository, &["add", "tracked.txt"]);
    run_git(&repository, &["commit", "--quiet", "-m", "base"]);
    std::fs::write(repository.path().join("tracked.txt"), "updated\n").expect("update tracked");
    std::fs::write(repository.path().join("untracked.txt"), "new\n").expect("write untracked");

    let (_parallelism_permit, handle, mut socket) =
        start_git_server(&temp, parallelism_permit).await;
    let result = run_stacked_action(
        &mut socket,
        "13",
        json!({
            "actionId": "wire-action-all",
            "cwd": repository.path().to_string_lossy(),
            "action": "commit",
            "commitMessage": "test: commit all changes",
        }),
    )
    .await;
    assert_eq!(result["kind"], "action_finished");
    assert_eq!(result["result"]["commit"]["status"], "created");
    assert_eq!(
        git_stdout(&repository, &["show", "HEAD:tracked.txt"]),
        "updated\n"
    );
    assert_eq!(
        git_stdout(&repository, &["show", "HEAD:untracked.txt"]),
        "new\n"
    );
    assert_eq!(git_stdout(&repository, &["status", "--short"]), "");

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_clean_commit_is_a_successful_no_op() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    std::fs::write(repository.path().join("tracked.txt"), "base\n").expect("write base");
    run_git(&repository, &["add", "tracked.txt"]);
    run_git(&repository, &["commit", "--quiet", "-m", "base"]);

    let (_parallelism_permit, handle, mut socket) =
        start_git_server(&temp, parallelism_permit).await;
    let result = run_stacked_action(
        &mut socket,
        "14",
        json!({
            "actionId": "wire-action-clean",
            "cwd": repository.path().to_string_lossy(),
            "action": "commit",
        }),
    )
    .await;
    assert_eq!(result["kind"], "action_finished");
    assert_eq!(result["result"]["commit"]["status"], "skipped_no_changes");
    assert_eq!(
        git_stdout(&repository, &["log", "--oneline"])
            .lines()
            .count(),
        1
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn stacked_feature_branch_rejects_a_clean_worktree_without_creating_a_branch() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    std::fs::write(repository.path().join("tracked.txt"), "base\n").expect("write base");
    run_git(&repository, &["add", "tracked.txt"]);
    run_git(&repository, &["commit", "--quiet", "-m", "base"]);
    let original_branch = git_stdout(&repository, &["branch", "--show-current"]);

    let (_parallelism_permit, handle, mut socket) =
        start_git_server(&temp, parallelism_permit).await;
    let result = run_stacked_action(
        &mut socket,
        "15",
        json!({
            "actionId": "wire-action-clean-feature",
            "cwd": repository.path().to_string_lossy(),
            "action": "commit",
            "featureBranch": true,
        }),
    )
    .await;
    assert_eq!(result["kind"], "action_failed");
    assert!(
        result["message"]
            .as_str()
            .expect("failure message")
            .contains("no changes to commit")
    );
    assert_eq!(
        git_stdout(&repository, &["branch", "--show-current"]),
        original_branch
    );
    assert_eq!(
        git_stdout(&repository, &["branch", "--format=%(refname:short)"])
            .lines()
            .count(),
        1
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

/// A bare remote with `main` and `develop`, and a clone whose `feature-x` was made the way a
/// terminal makes it, `git checkout -b feature-x origin/develop`, so it tracks
/// `origin/develop`, with one commit on top. Returns the remote, the clone, and the commit
/// `develop` points at.
fn renamed_upstream_fixture(root: &Path) -> (PathBuf, PathBuf, String) {
    let remote = root.join("remote.git");
    fs::create_dir(&remote).expect("create bare remote directory");
    run_git_in(&remote, &["init", "--bare", "--initial-branch=main"]);
    let remote_url = local_file_url(&remote);

    let publisher = root.join("publisher");
    fs::create_dir(&publisher).expect("create publisher directory");
    initialize_repository_in(&publisher);
    commit_file(&publisher, "tracked.txt", "base\n", "initial");
    run_git_in(&publisher, &["remote", "add", "origin", &remote_url]);
    run_git_in(
        &publisher,
        &["push", "--quiet", "origin", "main", "main:develop"],
    );
    let develop = git_stdout_in(&remote, &["rev-parse", "refs/heads/develop"]);

    let consumer = root.join("consumer");
    run_git_in(
        root,
        &["clone", "--quiet", &remote_url, &consumer.to_string_lossy()],
    );
    initialize_repository_identity(&consumer);
    // Git's default auto-tracking: the new branch tracks the remote branch it starts from.
    run_git_in(
        &consumer,
        &["checkout", "--quiet", "-b", "feature-x", "origin/develop"],
    );
    assert_eq!(
        git_stdout_in(&consumer, &["config", "branch.feature-x.merge"]).trim(),
        "refs/heads/develop"
    );
    commit_file(&consumer, "feature.txt", "feature\n", "feature work");
    (remote, consumer, develop)
}

/// #157: under Git's default `push.default=simple`, a bare `git push` refuses a branch whose
/// upstream has another name. Push publishes it under its own name and tracks that branch,
/// and leaves the upstream it was made from untouched.
#[tokio::test]
async fn stacked_push_publishes_a_branch_whose_upstream_has_another_name() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "stacked_push_publishes_a_branch_whose_upstream_has_another_name",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let (remote, consumer, develop) = renamed_upstream_fixture(root.path());
    let head = git_stdout_in(&consumer, &["rev-parse", "HEAD"]);

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    let outcome = run_stacked_action(
        server.socket(),
        "157",
        json!({
            "actionId": "push-renamed-upstream",
            "cwd": consumer.to_string_lossy(),
            "action": "push",
        }),
    )
    .await;
    assert_eq!(outcome["kind"], "action_finished", "{outcome}");
    assert_eq!(outcome["result"]["push"]["status"], "pushed");
    assert_eq!(outcome["result"]["push"]["branch"], "feature-x");

    assert_eq!(
        git_stdout_in(&remote, &["rev-parse", "refs/heads/feature-x"]),
        head,
        "the branch is published under its own name"
    );
    assert_eq!(
        git_stdout_in(&remote, &["rev-parse", "refs/heads/develop"]),
        develop,
        "the upstream the branch was made from is unchanged"
    );
    assert_eq!(
        git_stdout_in(&consumer, &["rev-parse", "--abbrev-ref", "@{upstream}"]).trim(),
        "origin/feature-x",
        "the branch now tracks what was pushed, so status counts against it"
    );
    assert_eq!(
        git_stdout_in(&consumer, &["rev-list", "--count", "@{upstream}..HEAD"]).trim(),
        "0"
    );
    server.shutdown().await;
}

/// #157: an explicitly configured `push.default` is the user's choice, so Push stays a bare
/// `git push` and Git applies it: `upstream` pushes to the differently named upstream.
#[tokio::test]
async fn stacked_push_honours_an_explicit_push_default() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config("stacked_push_honours_an_explicit_push_default") {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let (remote, consumer, _) = renamed_upstream_fixture(root.path());
    run_git_in(&consumer, &["config", "push.default", "upstream"]);
    let head = git_stdout_in(&consumer, &["rev-parse", "HEAD"]);

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    let outcome = run_stacked_action(
        server.socket(),
        "157",
        json!({
            "actionId": "push-explicit-default",
            "cwd": consumer.to_string_lossy(),
            "action": "push",
        }),
    )
    .await;
    assert_eq!(outcome["kind"], "action_finished", "{outcome}");
    assert_eq!(
        git_stdout_in(&remote, &["rev-parse", "refs/heads/develop"]),
        head
    );
    assert_eq!(
        git_stdout_in(&remote, &["for-each-ref", "refs/heads/feature-x"]),
        ""
    );
    assert_eq!(
        git_stdout_in(&consumer, &["config", "branch.feature-x.merge"]).trim(),
        "refs/heads/develop"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn stacked_create_pr_rejects_missing_targets_and_dirty_worktree_before_mutations() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    std::fs::write(repository.path().join("tracked.txt"), "base\n").expect("write base");
    run_git(&repository, &["add", "tracked.txt"]);
    run_git(&repository, &["commit", "--quiet", "-m", "base"]);
    std::fs::write(repository.path().join("tracked.txt"), "dirty\n").expect("dirty worktree");

    let (_parallelism_permit, handle, mut socket) =
        start_git_server(&temp, parallelism_permit).await;
    for (request_id, action) in [("14", "create_pr"), ("15", "commit_push_pr")] {
        let failed = run_stacked_action(
            &mut socket,
            request_id,
            json!({
                "actionId": format!("missing-target-{action}"),
                "cwd": repository.path().to_string_lossy(),
                "action": action,
                "featureBranch": action == "commit_push_pr",
            }),
        )
        .await;
        assert_eq!(failed["kind"], "action_failed");
        assert_eq!(
            failed["message"],
            "Select a target branch before creating a pull request."
        );
        send_json(
            &mut socket,
            json!({ "_tag": "Ack", "requestId": request_id }),
        )
        .await;
        assert!(matches!(
            next_server_message(&mut socket).await,
            ServerMessage::Exit { .. }
        ));
        assert_eq!(
            git_stdout(&repository, &["log", "-1", "--format=%s"]).trim(),
            "base"
        );
        assert_eq!(
            std::fs::read_to_string(repository.path().join("tracked.txt")).unwrap(),
            "dirty\n"
        );
    }
    let result = run_stacked_action(
        &mut socket,
        "16",
        json!({
            "actionId": "wire-action-dirty-pr",
            "pullRequestBaseBranch": "release/next",
            "cwd": repository.path().to_string_lossy(),
            "action": "create_pr",
        }),
    )
    .await;
    assert_eq!(result["kind"], "action_failed");
    assert_eq!(
        result["message"],
        "Commit local changes before creating a PR."
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn list_refs_commits_and_ref_lifecycle_round_trip_over_rpc() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config("list_refs_commits_and_ref_lifecycle_round_trip_over_rpc")
    {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let remote = root.path().join("refs-remote.git");
    fs::create_dir(&remote).expect("create bare remote directory");
    run_git_in(&remote, &["init", "--bare", "--initial-branch=main"]);

    let repository = root.path().join("refs-repository");
    fs::create_dir(&repository).expect("create repository directory");
    initialize_repository_in(&repository);
    commit_file(&repository, "first.txt", "one\n", "first");
    commit_file(&repository, "second.txt", "two\n", "second");
    commit_file(&repository, "third.txt", "three\n", "third");
    let remote_url = local_file_url(&remote);
    run_git_in(&repository, &["remote", "add", "origin", &remote_url]);
    run_git_in(&repository, &["push", "-u", "origin", "main"]);
    run_git_in(&repository, &["switch", "-c", "feature/one"]);
    commit_file(&repository, "feature.txt", "feature\n", "feature work");
    run_git_in(&repository, &["push", "-u", "origin", "feature/one"]);
    run_git_in(&repository, &["switch", "main"]);

    let worktree_parent = root.path().join("worktrees");
    fs::create_dir(&worktree_parent).expect("create worktree parent");
    let worktree_path = worktree_parent.join("feature-worktree");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    let cwd = repository.to_string_lossy();

    request(
        server.socket(),
        "201",
        "vcs.listCommits",
        json!({ "cwd": cwd, "limit": 2 }),
    )
    .await;
    let first_page = success_value(server.socket(), "201").await;
    assert_eq!(
        first_page["commits"]
            .as_array()
            .expect("first history page")
            .len(),
        2
    );
    assert_eq!(first_page["commits"][0]["subject"], "third");
    assert_eq!(first_page["commits"][1]["subject"], "second");
    assert_eq!(first_page["nextCursor"], 2);

    request(
        server.socket(),
        "202",
        "vcs.listCommits",
        json!({ "cwd": cwd, "limit": 2, "cursor": 2 }),
    )
    .await;
    let second_page = success_value(server.socket(), "202").await;
    assert_eq!(
        second_page["commits"]
            .as_array()
            .expect("second page")
            .len(),
        1
    );
    assert_eq!(second_page["commits"][0]["subject"], "first");
    assert_eq!(second_page["nextCursor"], Value::Null);

    request(
        server.socket(),
        "203",
        "vcs.createRef",
        json!({ "cwd": cwd, "refName": "feature/rpc-created", "switchRef": true }),
    )
    .await;
    assert_success_eq(
        server.socket(),
        "203",
        json!({ "refName": "feature/rpc-created" }),
    )
    .await;
    assert_eq!(
        git_stdout_in(&repository, &["branch", "--show-current"]).trim(),
        "feature/rpc-created"
    );

    request(
        server.socket(),
        "204",
        "vcs.switchRef",
        json!({ "cwd": cwd, "refName": "main" }),
    )
    .await;
    assert_success_eq(server.socket(), "204", json!({ "refName": "main" })).await;
    assert_eq!(
        git_stdout_in(&repository, &["branch", "--show-current"]).trim(),
        "main"
    );

    run_git_in(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "feature/worktree",
            worktree_path.to_str().expect("UTF-8 worktree path"),
            "main",
        ],
    );
    assert!(worktree_path.exists());

    request(
        server.socket(),
        "206",
        "vcs.listRefs",
        json!({ "cwd": cwd, "limit": 2 }),
    )
    .await;
    let paged_refs = success_value(server.socket(), "206").await;
    assert_eq!(paged_refs["isRepo"], true);
    assert_eq!(paged_refs["hasPrimaryRemote"], true);
    assert_eq!(paged_refs["nextCursor"], 2);
    assert_eq!(paged_refs["totalCount"], 4);
    let first_ref_names = paged_refs["refs"]
        .as_array()
        .expect("first ref page")
        .iter()
        .map(|reference| reference["name"].as_str().expect("first-page ref name"))
        .collect::<Vec<_>>();
    assert_eq!(first_ref_names, vec!["main", "feature/one"]);

    request(
        server.socket(),
        "207",
        "vcs.listRefs",
        json!({ "cwd": cwd, "cursor": 2, "limit": 2 }),
    )
    .await;
    let remaining_refs = success_value(server.socket(), "207").await;
    assert_eq!(remaining_refs["isRepo"], true);
    assert_eq!(remaining_refs["hasPrimaryRemote"], true);
    assert_eq!(remaining_refs["nextCursor"], Value::Null);
    assert_eq!(remaining_refs["totalCount"], paged_refs["totalCount"]);
    let remaining_ref_names = remaining_refs["refs"]
        .as_array()
        .expect("remaining ref page")
        .iter()
        .map(|reference| reference["name"].as_str().expect("remaining-page ref name"))
        .collect::<Vec<_>>();
    assert_eq!(
        remaining_ref_names,
        vec!["feature/rpc-created", "feature/worktree"]
    );
    assert!(
        first_ref_names
            .iter()
            .all(|name| !remaining_ref_names.contains(name))
    );

    request(
        server.socket(),
        "208",
        "vcs.listRefs",
        json!({ "cwd": cwd, "query": "feature/", "refKind": "local", "limit": 20 }),
    )
    .await;
    let local_feature_refs = success_value(server.socket(), "208").await;
    let local_names = local_feature_refs["refs"]
        .as_array()
        .expect("local feature refs")
        .iter()
        .map(|reference| reference["name"].as_str().expect("local ref name"))
        .collect::<Vec<_>>();
    assert!(local_names.contains(&"feature/one"));
    assert!(local_names.contains(&"feature/rpc-created"));
    assert!(local_names.contains(&"feature/worktree"));
    assert!(
        local_feature_refs["refs"]
            .as_array()
            .expect("local refs array")
            .iter()
            .all(|reference| reference["isRemote"] == false)
    );
    let worktree_ref = local_feature_refs["refs"]
        .as_array()
        .expect("local refs array")
        .iter()
        .find(|reference| reference["name"] == "feature/worktree")
        .expect("worktree ref listed");
    assert_eq!(
        canonical_path(
            worktree_ref["worktreePath"]
                .as_str()
                .expect("listed worktree path"),
        ),
        canonical_path(&worktree_path)
    );

    request(
        server.socket(),
        "209",
        "vcs.listRefs",
        json!({
            "cwd": cwd,
            "query": "feature/",
            "refKind": "remote",
            "includeMatchingRemoteRefs": true,
            "limit": 20
        }),
    )
    .await;
    let remote_feature_refs = success_value(server.socket(), "209").await;
    let remote_names = remote_feature_refs["refs"]
        .as_array()
        .expect("remote feature refs")
        .iter()
        .map(|reference| reference["name"].as_str().expect("remote ref name"))
        .collect::<Vec<_>>();
    assert_eq!(remote_names, vec!["origin/feature/one"]);
    assert_eq!(remote_feature_refs["refs"][0]["isRemote"], true);
    assert_eq!(remote_feature_refs["refs"][0]["remoteName"], "origin");

    server.shutdown().await;
}

#[tokio::test]
async fn stage_unstage_discard_and_invalid_pathspecs_round_trip_over_rpc() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "stage_unstage_discard_and_invalid_pathspecs_round_trip_over_rpc",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let repository = root.path().join("stage-repository");
    fs::create_dir(&repository).expect("create repository directory");
    initialize_repository_in(&repository);
    commit_file(&repository, "tracked.txt", "base\n", "base");
    fs::write(repository.join("tracked.txt"), "staged\n").expect("write staged tracked file");
    fs::create_dir_all(repository.join("selected/nested")).expect("create selected fixture");
    fs::write(repository.join("selected/nested/file.txt"), "remove me\n")
        .expect("write selected fixture");
    fs::write(repository.join("keep.txt"), "keep\n").expect("write keep fixture");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    let cwd = repository.to_string_lossy();

    request(
        server.socket(),
        "301",
        "vcs.stageFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt"] }),
    )
    .await;
    assert_success_eq(server.socket(), "301", Value::Null).await;

    request(
        server.socket(),
        "302",
        "vcs.refreshStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let staged_status = success_value(server.socket(), "302").await;
    let staged_files = staged_status["workingTree"]["files"]
        .as_array()
        .expect("staged file list");
    assert!(staged_files.iter().any(|file| {
        file["path"] == "tracked.txt" && file["area"] == "staged" && file["status"] == "modified"
    }));
    assert!(
        staged_files.iter().any(|file| {
            file["path"] == "selected/nested/file.txt" && file["area"] == "untracked"
        })
    );

    request(
        server.socket(),
        "303",
        "vcs.unstageFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt"] }),
    )
    .await;
    assert_success_eq(server.socket(), "303", Value::Null).await;

    request(
        server.socket(),
        "304",
        "vcs.refreshStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let unstaged_status = success_value(server.socket(), "304").await;
    let unstaged_files = unstaged_status["workingTree"]["files"]
        .as_array()
        .expect("unstaged file list");
    assert!(unstaged_files.iter().any(|file| {
        file["path"] == "tracked.txt" && file["area"] == "unstaged" && file["status"] == "modified"
    }));
    assert!(
        unstaged_files.iter().any(|file| {
            file["path"] == "selected/nested/file.txt" && file["area"] == "untracked"
        })
    );

    request(
        server.socket(),
        "305",
        "vcs.discardFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt", "selected/nested/file.txt"] }),
    )
    .await;
    assert_success_eq(server.socket(), "305", Value::Null).await;
    assert_eq!(
        fs::read_to_string(repository.join("tracked.txt")).expect("tracked contents"),
        "base\n"
    );
    assert!(!repository.join("selected/nested/file.txt").exists());
    assert!(repository.join("keep.txt").exists());

    request(
        server.socket(),
        "306",
        "vcs.refreshStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let discarded_status = success_value(server.socket(), "306").await;
    let discarded_files = discarded_status["workingTree"]["files"]
        .as_array()
        .expect("discarded file list");
    assert!(
        discarded_files
            .iter()
            .all(|file| file["path"] != "tracked.txt")
    );
    assert!(
        discarded_files
            .iter()
            .all(|file| file["path"] != "selected/nested/file.txt")
    );
    assert!(
        discarded_files
            .iter()
            .any(|file| file["path"] == "keep.txt" && file["area"] == "untracked")
    );

    request(
        server.socket(),
        "307",
        "vcs.stageFiles",
        json!({ "cwd": cwd, "filePaths": ["../escape.txt"] }),
    )
    .await;
    let invalid_pathspec = failure_value(server.socket(), "307").await;
    assert_eq!(invalid_pathspec["_tag"], "GitCommandError");
    assert_eq!(invalid_pathspec["operation"], "GitVcsDriver.stageFiles");
    assert!(
        invalid_pathspec["detail"]
            .as_str()
            .expect("invalid pathspec detail")
            .contains("repository-relative")
    );

    server.shutdown().await;
}

#[tokio::test]
async fn status_reports_each_conflict_once_and_the_merge_in_progress() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "status_reports_each_conflict_once_and_the_merge_in_progress",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let repository = root.path().join("conflict-repository");
    fs::create_dir(&repository).expect("create repository directory");
    initialize_repository_in(&repository);
    commit_file(&repository, "tracked.txt", "base\n", "base");
    run_git_in(&repository, &["switch", "-q", "-c", "feature"]);
    commit_file(&repository, "tracked.txt", "feature\n", "feature");
    run_git_in(&repository, &["switch", "-q", "-"]);
    commit_file(&repository, "tracked.txt", "main\n", "main");
    let merge = git_command(&repository, &["merge", "feature"])
        .output()
        .expect("merge");
    assert!(!merge.status.success(), "the fixture merge must conflict");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    let cwd = repository.to_string_lossy();
    request(
        server.socket(),
        "401",
        "vcs.refreshStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let status = success_value(server.socket(), "401").await;
    let entries: Vec<_> = status["workingTree"]["files"]
        .as_array()
        .expect("file list")
        .iter()
        .filter(|file| file["path"] == "tracked.txt")
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "one entry per conflicted path: {entries:?}"
    );
    assert_eq!(entries[0]["status"], "conflicted");
    assert_eq!(entries[0]["area"], "unstaged");
    assert_eq!(status["operationInProgress"], "merge");

    fs::write(repository.join("tracked.txt"), "resolved\n").expect("resolve");
    run_git_in(&repository, &["add", "tracked.txt"]);
    request(
        server.socket(),
        "402",
        "vcs.refreshStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let staged = success_value(server.socket(), "402").await;
    assert_eq!(staged["operationInProgress"], "merge");
    assert!(
        staged["workingTree"]["files"]
            .as_array()
            .expect("file list")
            .iter()
            .all(|file| file["status"] != "conflicted")
    );

    run_git_in(&repository, &["commit", "-q", "--no-edit"]);
    request(
        server.socket(),
        "403",
        "vcs.refreshStatus",
        json!({ "cwd": cwd }),
    )
    .await;
    let done = success_value(server.socket(), "403").await;
    assert!(done.get("operationInProgress").is_none());

    server.shutdown().await;
}

#[tokio::test]
async fn clone_pull_and_worktree_lifecycle_round_trip_over_rpc() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config("clone_pull_and_worktree_lifecycle_round_trip_over_rpc") {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let remote = root.path().join("clone-remote.git");
    fs::create_dir(&remote).expect("create clone remote directory");
    run_git_in(&remote, &["init", "--bare", "--initial-branch=main"]);

    let publisher = root.path().join("publisher");
    fs::create_dir(&publisher).expect("create publisher directory");
    initialize_repository_in(&publisher);
    commit_file(&publisher, "tracked.txt", "base\n", "initial");
    let remote_url = local_file_url(&remote);
    run_git_in(&publisher, &["remote", "add", "origin", &remote_url]);
    run_git_in(&publisher, &["push", "-u", "origin", "main"]);

    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("create clone parent");
    let worktree_parent = root.path().join("worktrees");
    fs::create_dir(&worktree_parent).expect("create worktree parent");
    let worktree_path = worktree_parent.join("feature-pull-worktree");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "401",
        "vcs.clone",
        json!({
            "url": remote_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let clone_result = success_value(server.socket(), "401").await;
    let consumer = PathBuf::from(
        clone_result["path"]
            .as_str()
            .expect("clone destination path"),
    );
    assert!(consumer.exists());
    initialize_repository_identity(&consumer);

    let consumer_cwd = consumer.to_string_lossy();
    request(
        server.socket(),
        "402",
        "vcs.pull",
        json!({ "cwd": consumer_cwd }),
    )
    .await;
    let skipped_pull = success_value(server.socket(), "402").await;
    assert_eq!(skipped_pull["status"], "skipped_up_to_date");
    assert_eq!(skipped_pull["refName"], "main");
    assert_eq!(skipped_pull["upstreamRef"], "origin/main");

    commit_file(
        &publisher,
        "tracked.txt",
        "remote change\n",
        "remote update",
    );
    run_git_in(&publisher, &["push"]);

    request(
        server.socket(),
        "403",
        "vcs.pull",
        json!({ "cwd": consumer_cwd }),
    )
    .await;
    let pulled = success_value(server.socket(), "403").await;
    assert_eq!(pulled["status"], "pulled");
    assert_eq!(
        fs::read_to_string(consumer.join("tracked.txt"))
            .expect("pulled tracked contents")
            .replace("\r\n", "\n"),
        "remote change\n"
    );

    run_git_in(
        &consumer,
        &[
            "worktree",
            "add",
            "-b",
            "feature/pull-worktree",
            worktree_path.to_str().expect("UTF-8 worktree path"),
            "main",
        ],
    );
    assert!(worktree_path.exists());
    fs::write(worktree_path.join("dirty.txt"), "dirty\n").expect("dirty worktree file");

    request(
        server.socket(),
        "405",
        "vcs.removeWorktree",
        json!({ "cwd": consumer_cwd, "path": worktree_path, "force": true }),
    )
    .await;
    let response = next_server_message(server.socket()).await;
    assert!(
        matches!(
            response,
            ServerMessage::Defect { defect }
                if defect.as_str().is_some_and(|detail| {
                    detail.contains("Unknown request tag")
                        && detail.contains("vcs.removeWorktree")
                })
        ),
        "the retired raw removal method must be unavailable"
    );
    assert!(worktree_path.exists());
    assert!(consumer.is_dir());
    assert_eq!(
        git_stdout_in(&consumer, &["rev-parse", "--is-inside-work-tree"]).trim(),
        "true"
    );
    let worktree_metadata = git_stdout_in(&consumer, &["worktree", "list", "--porcelain"]);
    let listed_worktrees = worktree_metadata
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .collect::<Vec<_>>();
    assert_eq!(listed_worktrees.len(), 2);
    assert_eq!(
        canonical_path(listed_worktrees[0]),
        canonical_path(&consumer)
    );

    server.shutdown().await;
}

#[tokio::test]
async fn clone_retry_reuses_an_existing_repository_with_the_same_origin() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "clone_retry_reuses_an_existing_repository_with_the_same_origin",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let remote = TempDir::new().expect("clone remote");
    initialize_repository(&remote);
    commit_file(remote.path(), "tracked.txt", "base\n", "initial");
    let remote_url = local_file_url(remote.path());
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    request(
        server.socket(),
        "406",
        "vcs.clone",
        json!({
            "url": remote_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let first = success_value(server.socket(), "406").await;
    let destination = PathBuf::from(first["path"].as_str().expect("first clone path"));
    fs::write(destination.join("local-only.txt"), "preserve me\n").expect("write retry sentinel");

    request(
        server.socket(),
        "407",
        "vcs.clone",
        json!({
            "url": remote_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let retried = success_value(server.socket(), "407").await;

    assert_eq!(
        canonical_path(retried["path"].as_str().expect("retried clone path")),
        canonical_path(&destination)
    );
    assert_eq!(
        fs::read_to_string(destination.join("local-only.txt")).expect("retry sentinel"),
        "preserve me\n"
    );
    assert_eq!(
        git_stdout_in(&destination, &["remote", "get-url", "origin"]).trim(),
        remote_url
    );

    server.shutdown().await;
}

#[tokio::test]
async fn clone_retry_reuses_an_existing_repository_with_an_instead_of_alias_origin() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "clone_retry_reuses_an_existing_repository_with_an_instead_of_alias_origin",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let remote = TempDir::new().expect("clone remote");
    initialize_repository(&remote);
    commit_file(remote.path(), "tracked.txt", "base\n", "initial");
    let canonical_url = local_file_url(remote.path());
    let alias_url = "bibcode-alias://clone-retry-origin";
    let instead_of_key = format!("url.{canonical_url}.insteadOf");
    run_git_in(
        remote.path(),
        &["config", "--global", "--add", &instead_of_key, alias_url],
    );
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    request(
        server.socket(),
        "407",
        "vcs.clone",
        json!({
            "url": alias_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let first = success_value(server.socket(), "407").await;
    let destination = PathBuf::from(first["path"].as_str().expect("first clone path"));
    fs::write(destination.join("local-only.txt"), "preserve me\n").expect("write retry sentinel");

    assert_eq!(
        git_stdout_in(&destination, &["config", "--get", "remote.origin.url"]).trim(),
        alias_url
    );
    assert_eq!(
        git_stdout_in(&destination, &["remote", "get-url", "origin"]).trim(),
        canonical_url
    );

    request(
        server.socket(),
        "408",
        "vcs.clone",
        json!({
            "url": alias_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let retried = success_value(server.socket(), "408").await;

    assert_eq!(
        canonical_path(retried["path"].as_str().expect("retried clone path")),
        canonical_path(&destination)
    );
    assert_eq!(
        fs::read_to_string(destination.join("local-only.txt")).expect("retry sentinel"),
        "preserve me\n"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn clone_rejects_an_existing_non_repository_destination() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config("clone_rejects_an_existing_non_repository_destination") {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let remote = TempDir::new().expect("clone remote");
    initialize_repository(&remote);
    commit_file(remote.path(), "tracked.txt", "base\n", "initial");
    let clone_parent = root.path().join("clones");
    let destination = clone_parent.join("consumer");
    fs::create_dir_all(&destination).expect("existing empty destination");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    request(
        server.socket(),
        "408",
        "vcs.clone",
        json!({
            "url": local_file_url(remote.path()),
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let error = failure_value(server.socket(), "408").await;

    assert_eq!(error["_tag"], "GitCommandError");
    let detail = error["detail"].as_str().expect("clone rejection detail");
    // The message names the folder, where it is, and the next step.
    assert!(
        detail.starts_with("A folder named consumer already exists at ")
            && detail.contains("clones")
            && detail.ends_with(
                " and is not a Git repository. Remove it or choose another parent folder."
            ),
        "unexpected clone rejection: {detail}"
    );
    assert!(!destination.join(".git").exists());

    server.shutdown().await;
}

#[tokio::test]
async fn clone_rejects_an_existing_repository_with_a_different_origin() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "clone_rejects_an_existing_repository_with_a_different_origin",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let first_remote = TempDir::new().expect("first clone remote");
    initialize_repository(&first_remote);
    commit_file(first_remote.path(), "tracked.txt", "first\n", "first");
    let second_remote = TempDir::new().expect("second clone remote");
    initialize_repository(&second_remote);
    commit_file(second_remote.path(), "tracked.txt", "second\n", "second");
    let first_url = local_file_url(first_remote.path());
    let second_url = local_file_url(second_remote.path());
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    request(
        server.socket(),
        "409",
        "vcs.clone",
        json!({
            "url": first_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let cloned = success_value(server.socket(), "409").await;
    let destination = PathBuf::from(cloned["path"].as_str().expect("clone path"));

    request(
        server.socket(),
        "410",
        "vcs.clone",
        json!({
            "url": second_url,
            "parentDir": clone_parent,
            "directoryName": "consumer"
        }),
    )
    .await;
    let error = failure_value(server.socket(), "410").await;

    assert_eq!(error["_tag"], "GitCommandError");
    assert!(
        error["detail"]
            .as_str()
            .expect("clone rejection detail")
            .contains("different origin")
    );
    assert_eq!(
        git_stdout_in(&destination, &["remote", "get-url", "origin"]).trim(),
        first_url
    );

    server.shutdown().await;
}

#[tokio::test]
async fn clone_resolves_a_home_relative_parent_before_running_git() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config("clone_resolves_a_home_relative_parent_before_running_git")
    {
        return;
    }

    let home = dirs::home_dir()
        .and_then(|path| fs::canonicalize(path).ok())
        .expect("canonical home directory");
    let home_temp = TempDir::new_in(&home).expect("temporary directory in home");
    let relative = home_temp
        .path()
        .strip_prefix(&home)
        .expect("temporary directory is inside home")
        .to_string_lossy()
        .replace('\\', "/");
    let clone_parent = home_temp.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");

    let server_temp = TempDir::new().expect("temporary server directory");
    let remote = TempDir::new().expect("clone remote");
    initialize_repository(&remote);
    commit_file(remote.path(), "tracked.txt", "base\n", "initial");

    let mut server = GitServerHarness::start(&server_temp, parallelism_permit).await;
    request(
        server.socket(),
        "411",
        "vcs.clone",
        json!({
            "url": local_file_url(remote.path()),
            "parentDir": format!("~/{relative}/clones"),
            "directoryName": "consumer"
        }),
    )
    .await;
    let clone_result = success_value(server.socket(), "411").await;
    let cloned = PathBuf::from(
        clone_result["path"]
            .as_str()
            .expect("clone destination path"),
    );
    assert_eq!(
        canonical_path(&cloned),
        canonical_path(clone_parent.join("consumer"))
    );
    assert!(cloned.join(".git").is_dir());

    request(
        server.socket(),
        "412",
        "vcs.clone",
        json!({
            "url": local_file_url(remote.path()),
            "parentDir": format!("~\\{}\\clones", relative.replace('/', "\\")),
            "directoryName": "consumer-backslash"
        }),
    )
    .await;
    let backslash_clone_result = success_value(server.socket(), "412").await;
    let backslash_cloned = PathBuf::from(
        backslash_clone_result["path"]
            .as_str()
            .expect("backslash clone destination path"),
    );
    assert_eq!(
        canonical_path(&backslash_cloned),
        canonical_path(clone_parent.join("consumer-backslash"))
    );
    assert!(backslash_cloned.join(".git").is_dir());

    server.shutdown().await;
}

#[tokio::test]
async fn interrupting_a_clone_stops_git_and_removes_the_destination_it_created() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "interrupting_a_clone_stops_git_and_removes_the_destination_it_created",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    // An explicit empty proxy keeps Git on the loopback remote even when the host sets one.
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    // The remote accepts Git's connection and never answers, so the clone runs until the
    // client interrupts it.
    let remote = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stalled remote listener");
    let url = format!(
        "http://{}/stalled.git",
        remote.local_addr().expect("stalled remote address")
    );
    let vcs_destination = clone_parent.join("interrupted");
    let source_control_destination = clone_parent.join("interrupted-source-control");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    for (ids, method, payload, destination) in [
        (
            ["421", "423"],
            "vcs.clone",
            json!({ "url": url, "parentDir": clone_parent, "directoryName": "interrupted" }),
            &vcs_destination,
        ),
        (
            ["422", "424"],
            "sourceControl.cloneRepository",
            json!({ "remoteUrl": url, "destinationPath": source_control_destination }),
            &source_control_destination,
        ),
    ] {
        let [id, retry_id] = ids;
        request(server.socket(), id, method, payload.clone()).await;
        let (mut connection, _) = timeout(GIT_RPC_RESPONSE_DEADLOCK_BOUND, remote.accept())
            .await
            .expect("Git connects to the remote")
            .expect("accept Git's connection");
        assert!(destination.is_dir(), "{method} created its destination");

        interrupt_clone(server.socket(), id, method).await;
        // Clone again right after Cancel: the retry waits for the interrupted clone's
        // cleanup and then starts its own transfer instead of finding a half-removed folder.
        request(server.socket(), retry_id, method, payload).await;
        let (mut retry_connection, _) = tokio::select! {
            accepted = timeout(GIT_RPC_RESPONSE_DEADLOCK_BOUND, remote.accept()) => accepted
                .unwrap_or_else(|_| panic!("the {method} retry never reached its transfer"))
                .expect("accept the retry's connection"),
            reply = next_server_message(server.socket()) => {
                panic!("the {method} retry must reach its own transfer, got {reply:?}")
            }
        };
        // Git holds the connection until the runner stops its process group.
        wait_for_connection_close(&mut connection, method).await;
        assert!(
            destination.is_dir(),
            "the {method} retry owns the destination"
        );

        interrupt_clone(server.socket(), retry_id, method).await;
        wait_for_connection_close(&mut retry_connection, method).await;
        let deadline = Instant::now() + Duration::from_secs(20);
        while destination.exists() {
            assert!(
                Instant::now() < deadline,
                "{method} left its destination behind"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert!(clone_parent.is_dir());

    server.shutdown().await;
}

async fn interrupt_clone(socket: &mut TestSocket, id: &str, method: &str) {
    send_json(socket, json!({ "_tag": "Interrupt", "requestId": id })).await;
    let interrupted = next_server_message(socket).await;
    assert!(
        matches!(
            &interrupted,
            ServerMessage::Exit { request_id, exit: RpcExit::Failure { cause } }
                if request_id.as_str() == id
                    && cause == &vec![CauseItem::Interrupt { fiber_id: None }]
        ),
        "{method} request {id} must end as interrupted, got {interrupted:?}"
    );
}

async fn wait_for_connection_close(connection: &mut tokio::net::TcpStream, method: &str) {
    use tokio::io::AsyncReadExt;

    timeout(Duration::from_secs(20), async {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = connection.read(&mut buffer).await {
            if read == 0 {
                break;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("interrupting {method} did not stop Git"));
}

async fn stalled_http_remote() -> (tokio::net::TcpListener, String) {
    let remote = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stalled remote listener");
    let url = format!(
        "http://{}/stalled.git",
        remote.local_addr().expect("stalled remote address")
    );
    (remote, url)
}

async fn accept_git(remote: &tokio::net::TcpListener) -> tokio::net::TcpStream {
    timeout(GIT_RPC_RESPONSE_DEADLOCK_BOUND, remote.accept())
        .await
        .expect("Git connects to the remote")
        .expect("accept Git's connection")
        .0
}

/// Git still holds its connection two seconds later: the clone is running.
async fn assert_connection_stays_open(connection: &mut tokio::net::TcpStream, context: &str) {
    use tokio::io::AsyncReadExt;

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buffer = [0_u8; 4096];
    loop {
        match timeout_at(deadline, connection.read(&mut buffer)).await {
            Err(_) => return,
            Ok(Ok(0) | Err(_)) => panic!("{context}: Git's connection closed"),
            Ok(Ok(_)) => {}
        }
    }
}

async fn assert_no_reply(socket: &mut TestSocket, context: &str) {
    if let Ok(message) = timeout(
        Duration::from_millis(500),
        next_frame_past_heartbeat(socket),
    )
    .await
    {
        panic!("{context}: expected no reply yet, got {message:?}");
    }
}

async fn next_exits(socket: &mut TestSocket, count: usize) -> HashMap<String, RpcExit> {
    let mut exits = HashMap::new();
    while exits.len() < count {
        match next_server_message(socket).await {
            ServerMessage::Exit { request_id, exit } => {
                exits.insert(request_id.as_str().to_owned(), exit);
            }
            message => panic!("expected an Exit, got {message:?}"),
        }
    }
    exits
}

fn failure_of(exit: &RpcExit) -> Value {
    match exit {
        RpcExit::Failure { cause } => match cause.as_slice() {
            [CauseItem::Fail { error }] => error.clone(),
            cause => panic!("expected one failure, got {cause:?}"),
        },
        exit => panic!("expected failure, got {exit:?}"),
    }
}

fn success_of(exit: &RpcExit) -> Value {
    match exit {
        RpcExit::Success { value } => value.clone().unwrap_or(Value::Null),
        exit => panic!("expected success, got {exit:?}"),
    }
}

fn with_flags(payload: &Value, attach: bool, detach: bool) -> Value {
    let mut payload = payload.clone();
    payload["attach"] = json!(attach);
    payload["detach"] = json!(detach);
    payload
}

#[tokio::test]
async fn production_runtime_pins_hosting_discovery_to_the_configured_directory() {
    let _parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "production_runtime_pins_hosting_discovery_to_the_configured_directory",
    ) {
        return;
    }

    // The optional audit fakes log one file per process. This test runs alone
    // in a child, so its PID excludes calls from parallel tests and binaries.
    let hosting_call_count = || {
        let calls = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fakes/calls");
        let entries = match fs::read_dir(calls) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
            Err(error) => panic!("read hosting audit directory: {error}"),
        };
        let parent = format!("ppid={}", std::process::id());
        entries
            .map(|entry| {
                let contents = fs::read_to_string(entry.expect("audit entry").path())
                    .expect("read hosting audit record");
                contents
                    .lines()
                    .filter(|line| {
                        line.split('\t').any(|field| field == parent)
                            && line.split('\t').any(|field| {
                                ["name=gh", "name=glab", "name=az", "name=jj"].contains(&field)
                            })
                    })
                    .count()
            })
            .sum::<usize>()
    };
    let calls_before = hosting_call_count();
    let temp = TempDir::new().expect("temporary server directory");
    let config = test_config(&temp).with_hosting_executable_dir_for_integration_test(
        hermetic_providers::missing_hosting_executable_dir(temp.path()),
    );
    let handle = ServerRuntime::start(config)
        .await
        .expect("production server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    request(&mut socket, "1", "server.discoverSourceControl", json!({})).await;
    let discovery = success_value(&mut socket, "1").await;
    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");

    assert_eq!(
        hosting_call_count(),
        calls_before,
        "production runtime must not execute hosting CLIs from PATH"
    );
    for (collection, executable) in [
        ("versionControlSystems", "jj"),
        ("sourceControlProviders", "gh"),
        ("sourceControlProviders", "glab"),
        ("sourceControlProviders", "az"),
    ] {
        let item = discovery[collection]
            .as_array()
            .expect("discovery entries")
            .iter()
            .find(|item| item["executable"] == executable)
            .expect("hosting discovery entry");
        assert_eq!(item["status"], "missing", "{executable}");
        assert_eq!(item["version"]["_tag"], "None", "{executable}");
        assert_eq!(
            item["detail"]["_tag"], "Some",
            "{executable} was not spawned"
        );
    }
    let git = discovery["versionControlSystems"]
        .as_array()
        .expect("VCS discovery entries")
        .iter()
        .find(|item| item["kind"] == "git")
        .expect("Git discovery entry");
    assert_eq!(git["status"], "available", "real Git remains available");
}

#[tokio::test]
async fn source_control_discovery_and_typed_errors_are_deterministic() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "source_control_discovery_and_typed_errors_are_deterministic",
    ) {
        return;
    }

    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let remote = root.path().join("source-control-remote.git");
    fs::create_dir(&remote).expect("create source-control remote directory");
    run_git_in(&remote, &["init", "--bare", "--initial-branch=main"]);

    let repository = root.path().join("publish-repository");
    fs::create_dir(&repository).expect("create publish repository directory");
    initialize_repository_in(&repository);
    commit_file(&repository, "tracked.txt", "base\n", "initial");

    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;
    let cwd = repository.to_string_lossy();

    request(
        server.socket(),
        "501",
        "server.discoverSourceControl",
        json!({}),
    )
    .await;
    let discovery = success_value(server.socket(), "501").await;
    let git_vcs = discovery["versionControlSystems"]
        .as_array()
        .expect("discovered VCS entries")
        .iter()
        .find(|item| item["kind"] == "git")
        .expect("git VCS discovery item");
    assert_eq!(git_vcs["label"], "Git");
    assert_eq!(git_vcs["executable"], "git");
    assert_eq!(git_vcs["implemented"], true);
    assert_eq!(git_vcs["status"], "available");
    assert_eq!(git_vcs["version"]["_tag"], "Some");

    request(
        server.socket(),
        "502",
        "sourceControl.cloneRepository",
        json!({
            "remoteUrl": local_file_url(&remote),
            "destinationPath": root.path().join("source-control-clone"),
        }),
    )
    .await;
    let cloned = success_value(server.socket(), "502").await;
    let cloned_cwd = PathBuf::from(cloned["cwd"].as_str().expect("cloned cwd"));
    assert!(cloned_cwd.exists());
    assert_eq!(cloned["repository"], Value::Null);

    request(
        server.socket(),
        "503",
        "sourceControl.lookupRepository",
        json!({ "provider": "bitbucket", "repository": "acme/repo" }),
    )
    .await;
    let lookup_error = failure_value(server.socket(), "503").await;
    assert_eq!(lookup_error["_tag"], "SourceControlRepositoryError");
    assert_eq!(lookup_error["provider"], "bitbucket");
    assert_eq!(lookup_error["operation"], "lookupRepository");

    request(
        server.socket(),
        "504",
        "sourceControl.cloneRepository",
        json!({ "destinationPath": root.path().join("missing-remote") }),
    )
    .await;
    let clone_error = failure_value(server.socket(), "504").await;
    assert_eq!(clone_error["_tag"], "SourceControlRepositoryError");
    assert_eq!(clone_error["operation"], "cloneRepository");
    assert!(
        clone_error["detail"]
            .as_str()
            .expect("clone error detail")
            .contains("clone URL")
    );

    request(
        server.socket(),
        "505",
        "sourceControl.publishRepository",
        json!({
            "cwd": cwd,
            "provider": "github",
            "repository": "acme/repo",
            "visibility": "friends-only"
        }),
    )
    .await;
    let publish_error = failure_value(server.socket(), "505").await;
    assert_eq!(publish_error["_tag"], "SourceControlRepositoryError");
    assert_eq!(publish_error["provider"], "github");
    assert_eq!(publish_error["operation"], "publishRepository");
    assert!(
        publish_error["detail"]
            .as_str()
            .expect("publish error detail")
            .contains("private or public")
    );

    request(
        server.socket(),
        "506",
        "vcs.init",
        json!({ "cwd": root.path().join("jj-repository"), "kind": "jj" }),
    )
    .await;
    let invalid_kind = failure_value(server.socket(), "506").await;
    assert_eq!(invalid_kind["_tag"], "GitCommandError");
    assert_eq!(invalid_kind["operation"], "vcs.init");
    assert!(
        invalid_kind["detail"]
            .as_str()
            .expect("invalid VCS kind detail")
            .contains("Only the git VCS driver")
    );

    request(
        server.socket(),
        "507",
        "vcs.listRefs",
        json!({ "cursor": "bad" }),
    )
    .await;
    let malformed = failure_value(server.socket(), "507").await;
    assert_eq!(malformed["_tag"], "RpcRequestInvalid");
    assert_eq!(malformed["method"], "vcs.listRefs");

    request(
        server.socket(),
        "508",
        "shell.openInEditor",
        json!({ "cwd": cwd, "editor": "unknown-editor" }),
    )
    .await;
    let unknown_editor = failure_value(server.socket(), "508").await;
    assert_eq!(unknown_editor["_tag"], "ExternalLauncherUnknownEditorError");
    assert_eq!(unknown_editor["editor"], "unknown-editor");

    request(
        server.socket(),
        "509",
        "sourceControl.lookupRepository",
        json!({ "provider": "gitlab", "repository": "acme/repo" }),
    )
    .await;
    let missing_gitlab_cli = failure_value(server.socket(), "509").await;
    assert_eq!(missing_gitlab_cli["_tag"], "SourceControlRepositoryError");
    assert_eq!(missing_gitlab_cli["provider"], "gitlab");

    request(
        server.socket(),
        "510",
        "sourceControl.lookupRepository",
        json!({ "provider": "github", "repository": "://" }),
    )
    .await;
    let invalid_github_repository = failure_value(server.socket(), "510").await;
    assert_eq!(
        invalid_github_repository["_tag"],
        "SourceControlRepositoryError"
    );
    assert_eq!(invalid_github_repository["provider"], "github");

    request(
        server.socket(),
        "511",
        "sourceControl.cloneRepository",
        json!({
            "remoteUrl": local_file_url(&remote),
            "destinationPath": "/",
        }),
    )
    .await;
    let parentless_destination = failure_value(server.socket(), "511").await;
    assert_eq!(parentless_destination["_tag"], "RpcRequestInvalid");

    request(
        server.socket(),
        "512",
        "sourceControl.publishRepository",
        json!({
            "cwd": cwd,
            "provider": "gitlab",
            "repository": "acme/repo",
            "visibility": "private",
        }),
    )
    .await;
    let unsupported_publisher = failure_value(server.socket(), "512").await;
    assert_eq!(
        unsupported_publisher["_tag"],
        "SourceControlRepositoryError"
    );
    assert_eq!(unsupported_publisher["provider"], "gitlab");

    server.shutdown().await;
}

#[tokio::test]
async fn pull_request_rpc_adapters_execute_resolution_and_preparation_paths() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    commit_file(repository.path(), "tracked.txt", "base\n", "initial");
    let cwd = repository.path().to_string_lossy();
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "601",
        "git.resolvePullRequest",
        json!({"cwd":cwd,"reference":"current"}),
    )
    .await;
    let resolution = failure_value(server.socket(), "601").await;
    assert_eq!(resolution["_tag"], "SourceControlProviderError");

    request(
        server.socket(),
        "602",
        "git.preparePullRequestThread",
        json!({
            "cwd":cwd,
            "reference":"current",
            "mode":"local",
        }),
    )
    .await;
    let preparation = failure_value(server.socket(), "602").await;
    assert_eq!(preparation["_tag"], "SourceControlProviderError");

    server.shutdown().await;
}

#[tokio::test]
async fn pull_request_preparation_rejects_client_directed_worktree_ownership_fields() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    let temp = TempDir::new().expect("temporary server directory");
    let repository = TempDir::new().expect("temporary repository");
    initialize_repository(&repository);
    commit_file(repository.path(), "tracked.txt", "base\n", "initial");
    let cwd = repository.path().to_string_lossy();
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    for (request_id, payload) in [
        (
            "603",
            json!({"cwd":cwd.clone(),"reference":"current","mode":"worktree"}),
        ),
        (
            "604",
            json!({
                "cwd":cwd.clone(),
                "reference":"current",
                "mode":"local",
                "threadId":"caller-selected-owner"
            }),
        ),
    ] {
        request(
            server.socket(),
            request_id,
            "git.preparePullRequestThread",
            payload,
        )
        .await;
        let error = failure_value(server.socket(), request_id).await;
        assert_eq!(error["_tag"], "RpcRequestInvalid");
    }

    assert_eq!(
        git_stdout(&repository, &["branch", "--show-current"]).trim(),
        "main"
    );
    server.shutdown().await;
}

fn initialize_repository(repository: &TempDir) {
    initialize_repository_in(repository.path());
}

fn run_git(repository: &TempDir, args: &[&str]) {
    run_git_in(repository.path(), args);
}

fn run_git_in(cwd: &Path, args: &[&str]) {
    assert!(
        git_command(cwd, args)
            .status()
            .expect("git starts")
            .success(),
        "git {args:?} failed"
    );
}

fn git_stdout(repository: &TempDir, args: &[&str]) -> String {
    git_stdout_in(repository.path(), args)
}

fn git_stdout_in(cwd: &Path, args: &[&str]) -> String {
    let output = git_command(cwd, args).output().expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is UTF-8")
}

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "BiBCode Test")
        .env("GIT_AUTHOR_EMAIL", "bibcode@example.invalid")
        .env("GIT_COMMITTER_NAME", "BiBCode Test")
        .env("GIT_COMMITTER_EMAIL", "bibcode@example.invalid");
    command
}

fn initialize_repository_in(cwd: &Path) {
    run_git_in(cwd, &["init", "--quiet", "-b", "main"]);
    initialize_repository_identity(cwd);
}

fn initialize_repository_identity(cwd: &Path) {
    run_git_in(cwd, &["config", "user.name", "BiBCode Test"]);
    run_git_in(cwd, &["config", "user.email", "bibcode@example.invalid"]);
    run_git_in(cwd, &["config", "core.autocrlf", "false"]);
}

fn commit_file(cwd: &Path, relative: &str, contents: &str, message: &str) {
    let path = cwd.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent directory");
    }
    fs::write(&path, contents).expect("write fixture file");
    run_git_in(cwd, &["add", "--", relative]);
    run_git_in(cwd, &["commit", "--quiet", "-m", message]);
}

fn local_file_url(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", normalized.trim_start_matches("//?/"))
}

fn canonical_path(path: impl AsRef<Path>) -> PathBuf {
    fs::canonicalize(path).expect("canonical test path")
}

fn relaunch_with_isolated_git_config(test_name: &str) -> bool {
    if std::env::var_os(ISOLATED_GIT_TEST).is_some() {
        return false;
    }

    let _relaunch_guard = ISOLATED_GIT_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = IsolatedGitConfig::new();

    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    config.apply_to_command(&mut command);
    let output = command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(ISOLATED_GIT_TEST, "1")
        .output()
        .expect("run test with isolated Git config");
    assert!(
        output.status.success(),
        "isolated test {test_name} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

async fn start_git_server(
    temp: &TempDir,
    parallelism_permit: OwnedSemaphorePermit,
) -> (
    OwnedSemaphorePermit,
    bibcode_server::ServerHandle,
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) {
    let mut registry = RpcRegistry::empty();
    register_git_vcs_rpc(&mut registry, git_vcs_services(temp));
    let handle = ServerRuntime::start_with_registry(test_config(temp), registry)
        .await
        .expect("server starts");
    let (socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    (parallelism_permit, handle, socket)
}

fn git_rpc_fixture_parallelism() -> Arc<Semaphore> {
    static PARALLELISM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(PARALLELISM.get_or_init(|| Arc::new(Semaphore::new(MAX_PARALLEL_GIT_RPC_FIXTURES))))
}

async fn acquire_git_rpc_fixture() -> OwnedSemaphorePermit {
    git_rpc_fixture_parallelism()
        .acquire_owned()
        .await
        .expect("Git RPC fixture parallelism remains open")
}

async fn run_stacked_action<S>(
    socket: &mut WebSocketStream<S>,
    request_id: &str,
    payload: Value,
) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    request(socket, request_id, "git.runStackedAction", payload).await;
    let started = next_server_message(socket).await;
    let ServerMessage::Chunk { values, .. } = started else {
        panic!("expected the stacked action to start, got {started:?}");
    };
    assert_eq!(values[0]["kind"], "action_started");
    // An event already queued behind the start shares its chunk.
    if let Some(event) = values.into_iter().nth(1) {
        return event;
    }
    send_json(socket, json!({ "_tag": "Ack", "requestId": request_id })).await;
    let event = next_server_message(socket).await;
    let ServerMessage::Chunk { values, .. } = event else {
        panic!("expected stacked action event, got {event:?}");
    };
    values.into_iter().next().expect("stacked action event")
}

fn test_config(temp: &TempDir) -> ServerConfig {
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    config
}

async fn request<S>(socket: &mut WebSocketStream<S>, id: &str, tag: &str, payload: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_json(
        socket,
        json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] }),
    )
    .await;
}

async fn assert_success_eq<S>(socket: &mut WebSocketStream<S>, id: &str, expected: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    assert_eq!(success_value(socket, id).await, expected);
}

async fn assert_success_eq_for<S>(
    socket: &mut WebSocketStream<S>,
    id: &str,
    expected: Value,
    context: &str,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    assert_eq!(success_value_for(socket, id, context).await, expected);
}

async fn failure_value<S>(socket: &mut WebSocketStream<S>, id: &str) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match next_server_message(socket).await {
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Failure { cause },
        } if request_id == RequestId::try_from(id).expect("request id") => {
            let [CauseItem::Fail { error }] = cause.as_slice() else {
                panic!("expected a single failure cause for {id}, got {cause:?}");
            };
            error.clone()
        }
        message => panic!("expected failed response for {id}, got {message:?}"),
    }
}

async fn success_value<S>(socket: &mut WebSocketStream<S>, id: &str) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match next_server_message(socket).await {
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Success { value },
        } if request_id == RequestId::try_from(id).expect("request id") => {
            value.unwrap_or(Value::Null)
        }
        message => panic!("expected successful response for {id}, got {message:?}"),
    }
}

async fn success_value_for<S>(socket: &mut WebSocketStream<S>, id: &str, context: &str) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match next_server_message_for(socket, context).await {
        ServerMessage::Exit {
            request_id,
            exit: RpcExit::Success { value },
        } if request_id == RequestId::try_from(id).expect("request id") => {
            value.unwrap_or(Value::Null)
        }
        message => panic!("expected successful response for {id}, got {message:?}"),
    }
}

async fn send_json<S>(socket: &mut WebSocketStream<S>, value: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send WebSocket message");
}

async fn next_server_message<S>(socket: &mut WebSocketStream<S>) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    next_server_message_with_timeout(socket, GIT_RPC_RESPONSE_DEADLOCK_BOUND).await
}

async fn next_server_message_for<S>(socket: &mut WebSocketStream<S>, context: &str) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(
        GIT_RPC_RESPONSE_DEADLOCK_BOUND,
        next_frame_past_heartbeat(socket),
    )
    .await
    .unwrap_or_else(|_| panic!("WebSocket response timeout while waiting for {context}"))
    .expect("WebSocket remains open")
    .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected text WebSocket message while waiting for {context}, got {frame:?}");
    };
    serde_json::from_str(&text).expect("valid server RPC message")
}

async fn next_server_message_with_timeout<S>(
    socket: &mut WebSocketStream<S>,
    timeout_duration: Duration,
) -> ServerMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(timeout_duration, next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected text WebSocket message, got {frame:?}");
    };
    serde_json::from_str(&text).expect("valid server RPC message")
}

#[tokio::test]
async fn a_detached_clone_survives_a_socket_close_and_an_interrupt_and_attach_shares_its_outcome() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "a_detached_clone_survives_a_socket_close_and_an_interrupt_and_attach_shares_its_outcome",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    // An explicit empty proxy keeps Git on the loopback remote even when the host sets one.
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let (remote, url) = stalled_http_remote().await;
    let destination = clone_parent.join("detached");
    let clone = json!({ "url": url, "parentDir": clone_parent, "directoryName": "detached" });
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "701",
        "vcs.clone",
        with_flags(&clone, false, true),
    )
    .await;
    let mut connection = accept_git(&remote).await;
    assert!(destination.is_dir(), "the clone created its destination");

    // The socket drops. The detached clone keeps running on the server.
    server.reconnect().await;
    assert_connection_stays_open(&mut connection, "after the socket closed").await;
    assert!(destination.is_dir());

    // A new socket re-attaches. An Interrupt ends only that wait.
    let attach = with_flags(&clone, true, true);
    request(server.socket(), "702", "vcs.clone", attach.clone()).await;
    assert_no_reply(server.socket(), "a joined attach waits for the clone").await;
    interrupt_clone(server.socket(), "702", "vcs.clone").await;
    assert_connection_stays_open(&mut connection, "after the attach was interrupted").await;

    // Join again, then end the transfer from the remote's side: the attach shares the failure.
    request(server.socket(), "703", "vcs.clone", attach.clone()).await;
    assert_no_reply(server.socket(), "the second attach waits").await;
    drop(connection);
    let failure = failure_value(server.socket(), "703").await;
    assert_eq!(failure["_tag"], "GitCommandError");
    assert!(
        !destination.exists(),
        "the outcome was published after the cleanup"
    );

    // Within five minutes, another socket's re-attach gets the same failure.
    server.reconnect().await;
    request(server.socket(), "704", "vcs.clone", attach).await;
    assert_eq!(failure_value(server.socket(), "704").await, failure);

    server.shutdown().await;
}

#[tokio::test]
async fn without_detach_a_socket_close_still_stops_the_clone_and_removes_its_folder() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "without_detach_a_socket_close_still_stops_the_clone_and_removes_its_folder",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let (remote, url) = stalled_http_remote().await;
    let destination = clone_parent.join("legacy");
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "711",
        "vcs.clone",
        json!({ "url": url, "parentDir": clone_parent, "directoryName": "legacy" }),
    )
    .await;
    let mut connection = accept_git(&remote).await;
    server.reconnect().await;
    wait_for_connection_close(&mut connection, "a socket close without detach").await;
    let deadline = Instant::now() + Duration::from_secs(20);
    while destination.exists() {
        assert!(
            Instant::now() < deadline,
            "the created folder was left behind"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    server.shutdown().await;
}

#[tokio::test]
async fn attach_reports_nothing_a_finished_clone_an_incomplete_one_and_busy() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "attach_reports_nothing_a_finished_clone_an_incomplete_one_and_busy",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let source = TempDir::new().expect("clone source");
    initialize_repository(&source);
    commit_file(source.path(), "tracked.txt", "base\n", "initial");
    let source_url = local_file_url(source.path());
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    // Nothing there: not-in-progress, and attach never creates the folder.
    let nothing =
        json!({ "url": source_url, "parentDir": clone_parent, "directoryName": "nothing" });
    request(
        server.socket(),
        "721",
        "vcs.clone",
        with_flags(&nothing, true, true),
    )
    .await;
    let error = failure_value(server.socket(), "721").await;
    assert_eq!(error["_tag"], "GitCloneOperationError");
    assert_eq!(error["reason"], "not-in-progress");
    assert!(!clone_parent.join("nothing").exists());

    // A finished clone: a later attach from a new socket gets its path from the disk.
    let finished =
        json!({ "url": source_url, "parentDir": clone_parent, "directoryName": "finished" });
    request(
        server.socket(),
        "722",
        "vcs.clone",
        with_flags(&finished, false, true),
    )
    .await;
    let cloned = success_value(server.socket(), "722").await;
    server.reconnect().await;
    request(
        server.socket(),
        "723",
        "vcs.clone",
        with_flags(&finished, true, true),
    )
    .await;
    assert_eq!(success_value(server.socket(), "723").await, cloned);

    // A leftover with the same origin but no commit: the incomplete-clone error.
    let leftover = clone_parent.join("leftover");
    fs::create_dir(&leftover).expect("leftover");
    initialize_repository_in(&leftover);
    run_git_in(&leftover, &["remote", "add", "origin", &source_url]);
    let incomplete =
        json!({ "url": source_url, "parentDir": clone_parent, "directoryName": "leftover" });
    request(
        server.socket(),
        "724",
        "vcs.clone",
        with_flags(&incomplete, true, true),
    )
    .await;
    let error = failure_value(server.socket(), "724").await;
    assert_eq!(error["_tag"], "GitCommandError");
    assert!(
        error["detail"]
            .as_str()
            .expect("detail")
            .contains("An incomplete clone exists at")
    );
    assert!(leftover.join(".git").is_dir(), "attach keeps the leftover");

    // Another URL into a running clone's folder: busy at once, never naming the running URL.
    let (remote, stalled_url) = stalled_http_remote().await;
    let running = json!({ "url": stalled_url, "parentDir": clone_parent, "directoryName": "busy" });
    request(
        server.socket(),
        "725",
        "vcs.clone",
        with_flags(&running, false, true),
    )
    .await;
    let connection = accept_git(&remote).await;
    let other = json!({ "url": source_url, "parentDir": clone_parent, "directoryName": "busy" });
    for (id, payload) in [
        ("726", with_flags(&other, false, true)),
        ("727", with_flags(&other, true, true)),
    ] {
        request(server.socket(), id, "vcs.clone", payload).await;
        let error = failure_value(server.socket(), id).await;
        assert_eq!(error["reason"], "busy");
        assert!(!error.to_string().contains(&stalled_url));
    }
    // End the running clone from the remote's side.
    drop(connection);
    let exits = next_exits(server.socket(), 1).await;
    assert_eq!(failure_of(&exits["725"])["_tag"], "GitCommandError");

    server.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn source_control_clone_through_a_symlinked_parent_joins_the_running_clone() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "source_control_clone_through_a_symlinked_parent_joins_the_running_clone",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let link = root.path().join("clones-link");
    std::os::unix::fs::symlink(&clone_parent, &link).expect("symlinked parent");
    let (remote, url) = stalled_http_remote().await;
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    let clone = json!({ "url": url, "parentDir": clone_parent, "directoryName": "joined" });
    request(
        server.socket(),
        "731",
        "vcs.clone",
        with_flags(&clone, false, true),
    )
    .await;
    let connection = accept_git(&remote).await;

    request(
        server.socket(),
        "732",
        "sourceControl.cloneRepository",
        json!({ "remoteUrl": url, "destinationPath": link.join("joined") }),
    )
    .await;
    assert_no_reply(
        server.socket(),
        "the aliased clone joins instead of failing",
    )
    .await;
    assert!(
        timeout(Duration::from_millis(500), remote.accept())
            .await
            .is_err(),
        "a join starts no second transfer"
    );

    // End the transfer from the remote's side: both callers get the one outcome.
    drop(connection);
    let exits = next_exits(server.socket(), 2).await;
    let failure = failure_of(&exits["731"]);
    assert_eq!(failure["_tag"], "GitCommandError");
    let joined = failure_of(&exits["732"]);
    assert_eq!(joined["_tag"], "SourceControlRepositoryError");
    assert_eq!(joined["operation"], "cloneRepository");
    assert_eq!(
        joined["detail"], failure["detail"],
        "the join shared the clone's outcome"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn cancel_clone_from_a_new_socket_stops_a_detached_clone_and_removes_only_its_folder() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "cancel_clone_from_a_new_socket_stops_a_detached_clone_and_removes_only_its_folder",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let (remote, url) = stalled_http_remote().await;
    let destination = clone_parent.join("detached");
    let clone = json!({ "url": url, "parentDir": clone_parent, "directoryName": "detached" });
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "741",
        "vcs.clone",
        with_flags(&clone, false, true),
    )
    .await;
    let mut connection = accept_git(&remote).await;
    server.reconnect().await;
    let attach = with_flags(&clone, true, true);
    request(server.socket(), "742", "vcs.clone", attach.clone()).await;
    assert_no_reply(server.socket(), "the attach waits for the clone").await;

    request(server.socket(), "743", "vcs.cancelClone", clone.clone()).await;
    wait_for_connection_close(&mut connection, "vcs.cancelClone").await;
    let exits = next_exits(server.socket(), 2).await;
    assert_eq!(success_of(&exits["743"]), json!({ "cancelled": true }));
    let cancelled = failure_of(&exits["742"]);
    assert_eq!(cancelled["_tag"], "GitCloneOperationError");
    assert_eq!(cancelled["reason"], "cancelled");
    assert!(
        !destination.exists(),
        "the cancel answered after the cleanup"
    );
    assert!(clone_parent.is_dir(), "only the created folder was removed");

    // Idempotent, and a re-attach within five minutes gets the real reason.
    request(server.socket(), "744", "vcs.cancelClone", clone).await;
    assert_success_eq(server.socket(), "744", json!({ "cancelled": false })).await;
    request(server.socket(), "745", "vcs.clone", attach).await;
    assert_eq!(
        failure_value(server.socket(), "745").await["reason"],
        "cancelled"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn cancel_clone_answers_only_its_declared_shapes() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config("cancel_clone_answers_only_its_declared_shapes") {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let (remote, url) = stalled_http_remote().await;
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    // Nothing live: exactly {"cancelled": false}.
    let none = json!({ "url": url, "parentDir": clone_parent, "directoryName": "none" });
    request(server.socket(), "751", "vcs.cancelClone", none).await;
    assert_eq!(
        success_value(server.socket(), "751").await,
        json!({ "cancelled": false })
    );

    // An unresolvable parent: a GitCommandError with exactly the handler's fields.
    let missing = json!({ "url": url, "parentDir": root.path().join("missing-parent") });
    request(server.socket(), "752", "vcs.cancelClone", missing).await;
    let error = failure_value(server.socket(), "752").await;
    let mut keys = error
        .as_object()
        .expect("an error object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["_tag", "command", "cwd", "detail", "operation"]);
    assert_eq!(error["_tag"], "GitCommandError");
    assert_eq!(error["operation"], "vcs.cancelClone");
    assert_eq!(error["command"], "git");

    // A live clone: exactly {"cancelled": true}.
    let live = json!({ "url": url, "parentDir": clone_parent, "directoryName": "live" });
    request(
        server.socket(),
        "753",
        "vcs.clone",
        with_flags(&live, false, true),
    )
    .await;
    let mut connection = accept_git(&remote).await;
    request(server.socket(), "754", "vcs.cancelClone", live).await;
    wait_for_connection_close(&mut connection, "vcs.cancelClone").await;
    let exits = next_exits(server.socket(), 2).await;
    assert_eq!(success_of(&exits["754"]), json!({ "cancelled": true }));
    assert_eq!(failure_of(&exits["753"])["reason"], "cancelled");

    server.shutdown().await;
}

#[tokio::test]
async fn a_clone_folder_name_that_is_a_path_is_refused_and_creates_nothing() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "a_clone_folder_name_that_is_a_path_is_refused_and_creates_nothing",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let source = TempDir::new().expect("clone source");
    initialize_repository(&source);
    commit_file(source.path(), "tracked.txt", "base\n", "initial");
    let source_url = local_file_url(source.path());
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    request(
        server.socket(),
        "771",
        "vcs.clone",
        json!({ "url": source_url, "parentDir": clone_parent, "directoryName": "a/../b" }),
    )
    .await;
    let error = failure_value(server.socket(), "771").await;
    assert_eq!(error["_tag"], "GitCommandError");
    assert_eq!(
        error["detail"],
        "The folder name \"a/../b\" must be a single name, without slashes or \"..\"."
    );
    assert!(
        !clone_parent.join("a").exists(),
        "no folder is created for the name's first part"
    );
    assert!(
        !clone_parent.join("b").exists(),
        "nothing is cloned where the path leads"
    );

    // A URL that yields no folder name is refused, and the URL, which may embed credentials, is
    // never echoed.
    let nameless = "https://user:secret@example.test/org/.git";
    request(
        server.socket(),
        "772",
        "vcs.clone",
        json!({ "url": nameless, "parentDir": clone_parent }),
    )
    .await;
    let error = failure_value(server.socket(), "772").await;
    assert_eq!(error["_tag"], "GitCommandError");
    assert_eq!(
        error["detail"],
        "Could not work out a folder name from the repository URL. Check that the URL ends with the repository's name."
    );
    assert!(
        !error.to_string().contains("secret"),
        "the URL is never echoed: {error}"
    );

    // sourceControl.cloneRepository checks the name before it creates the parent folder. A
    // destination ending in ".." has no folder name, so the name comes from the URL.
    let unnamed_parent = clone_parent.join("new-parent");
    request(
        server.socket(),
        "773",
        "sourceControl.cloneRepository",
        json!({ "remoteUrl": nameless, "destinationPath": unnamed_parent.join("..") }),
    )
    .await;
    let error = failure_value(server.socket(), "773").await;
    assert_eq!(error["_tag"], "SourceControlRepositoryError");
    assert_eq!(
        error["detail"],
        "Could not work out a folder name from the repository URL. Check that the URL ends with the repository's name."
    );
    assert!(
        !error.to_string().contains("secret"),
        "the URL is never echoed: {error}"
    );
    assert!(
        !unnamed_parent.exists(),
        "a refused clone creates no parent folder"
    );
    assert_eq!(
        fs::read_dir(&clone_parent).expect("clone parent").count(),
        0,
        "nothing was created in the parent folder"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn a_cancel_through_a_folder_name_alias_is_refused_and_the_live_clone_keeps_running() {
    let parallelism_permit = acquire_git_rpc_fixture().await;
    if relaunch_with_isolated_git_config(
        "a_cancel_through_a_folder_name_alias_is_refused_and_the_live_clone_keeps_running",
    ) {
        return;
    }
    let temp = TempDir::new().expect("temporary server directory");
    let root = TempDir::new().expect("temporary fixture root");
    run_git_in(root.path(), &["config", "--global", "http.proxy", ""]);
    let clone_parent = root.path().join("clones");
    fs::create_dir(&clone_parent).expect("clone parent");
    let (remote, url) = stalled_http_remote().await;
    let mut server = GitServerHarness::start(&temp, parallelism_permit).await;

    let live = json!({ "url": url, "parentDir": clone_parent, "directoryName": "b" });
    request(
        server.socket(),
        "781",
        "vcs.clone",
        with_flags(&live, false, true),
    )
    .await;
    let mut connection = accept_git(&remote).await;

    // "a/../b" names the live clone's folder lexically; the server refuses the path instead of
    // resolving it, so the live clone is untouched.
    request(
        server.socket(),
        "782",
        "vcs.cancelClone",
        json!({ "url": url, "parentDir": clone_parent, "directoryName": "a/../b" }),
    )
    .await;
    let error = failure_value(server.socket(), "782").await;
    assert_eq!(error["_tag"], "GitCommandError");
    assert_eq!(error["operation"], "vcs.cancelClone");
    assert_eq!(
        error["detail"],
        "The folder name \"a/../b\" must be a single name, without slashes or \"..\"."
    );
    assert_connection_stays_open(&mut connection, "after the refused cancel").await;
    assert!(
        clone_parent.join("b").is_dir(),
        "the live clone keeps its folder"
    );

    // End the transfer from the remote's side: the clone fails on its own, not as cancelled.
    drop(connection);
    let failure = failure_value(server.socket(), "781").await;
    assert_eq!(
        failure["_tag"], "GitCommandError",
        "the live clone was not cancelled: {failure}"
    );

    server.shutdown().await;
}
