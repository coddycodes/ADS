#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::{fs, path::Path, process::Command, time::Duration};

#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use coddy_studio_server::persistence::Repositories;
use coddy_studio_server::{
    RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    git::GitRepository,
    orchestration::{EngineOptions, OrchestrationEngine},
    persistence::{Database, run_migrations},
    production::{
        orchestration_rpc::register_orchestration_rpc,
        worktree_catalog_rpc::{
            WorktreeCatalogRpcServices, WorktreeRemovalCleanupAdmission,
            WorktreeRemovalQuiesceFuture, WorktreeRemovalQuiesceLease,
            WorktreeRemovalQuiesceRequest, WorktreeRemovalQuiescer, register_worktree_catalog_rpc,
        },
    },
    worktree_catalog::WorktreeCatalogService,
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::timeout;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const CREATED_AT: &str = "2026-08-09T00:00:00Z";

#[tokio::test]
#[cfg(unix)]
async fn adopted_external_worktree_uses_normal_rpc_paths_and_survives_the_full_lifecycle() {
    let root = TempDir::new().expect("fixture root");
    let main = root.path().join("main");
    let external = root.path().join("external");
    init_repository(&main);

    let state = TempDir::new().expect("server state");
    let config = ServerConfig::new(state.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let provider_cwd_fifo = state.path().join("provider-cwd.fifo");
    let provider_pid_file = state.path().join("provider.pid");
    let setup_sentinel = state.path().join("adoption-must-not-run-setup");
    let fifo = Command::new("mkfifo")
        .arg(&provider_cwd_fifo)
        .output()
        .expect("create provider cwd FIFO");
    assert!(fifo.status.success(), "mkfifo failed");
    let provider_fixture =
        write_cwd_recording_codex_fixture(state.path(), &provider_cwd_fifo, &provider_pid_file);
    hermetic_providers::write_hermetic_settings(
        &config.state_dir(),
        json!({
            "providerInstances": {
                "codex": {
                    "driver": "codex",
                    "enabled": true,
                    "config": { "binaryPath": provider_fixture }
                }
            }
        }),
    );
    let handle = ServerRuntime::start(config.clone())
        .await
        .expect("production server starts");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects")
        .0;
    let database = Database::open_existing(config.database_path())
        .await
        .expect("projection database");
    let repositories = Repositories::new(database);

    request(
        &mut socket,
        "1",
        "orchestration.dispatchCommand",
        json!({
            "type":"project.create",
            "commandId":"create-project",
            "projectId":"project-1",
            "title":"External lifecycle",
            "workspaceRoot":main,
            "defaultModelSelection":null,
            "createdAt":CREATED_AT
        }),
    )
    .await;
    success(&mut socket, "1").await;
    request(
        &mut socket,
        "19",
        "orchestration.dispatchCommand",
        json!({
            "type":"project.meta.update",
            "commandId":"configure-observable-setup-script",
            "projectId":"project-1",
            "scripts":[{
                "id":"must-not-run-on-adoption",
                "name":"Must not run on adoption",
                "command":format!("printf setup-ran > '{}'", setup_sentinel.to_string_lossy()),
                "runOnWorktreeCreate":true
            }]
        }),
    )
    .await;
    success(&mut socket, "19").await;

    request(
        &mut socket,
        "2",
        "subscribeWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let initial = chunk(&mut socket, "2").await;
    ack(&mut socket, "2").await;
    assert_eq!(initial["worktrees"].as_array().expect("worktrees").len(), 1);

    git(
        &main,
        &["worktree", "add", "-b", "feature/external"],
        Some(&external),
    );
    let discovered = timeout(Duration::from_secs(5), async {
        loop {
            let snapshots = chunk_values(&mut socket, "2").await;
            ack(&mut socket, "2").await;
            if let Some(snapshot) = snapshots.into_iter().find(|snapshot| {
                snapshot["worktrees"]
                    .as_array()
                    .is_some_and(|worktrees| worktrees.len() == 2)
            }) {
                break snapshot;
            }
        }
    })
    .await
    .expect("the subscribed catalog autonomously discovers the worktree within five seconds");
    let candidate = discovered["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["eligibleForAdoption"] == true)
        .expect("eligible external worktree")
        .clone();
    let clean_before_adoption = git_output(&external, &["status", "--porcelain"]);
    let worktrees_before_adoption = git_output(&main, &["worktree", "list", "--porcelain"]);

    request(
        &mut socket,
        "3",
        "worktree.adopt",
        json!({
            "commandId":"adopt-external",
            "projectId":"project-1",
            "worktreeKey":candidate["worktreeKey"],
            "expectedGeneration":discovered["generation"],
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    let adopted = success(&mut socket, "3").await;
    let thread_id = adopted["threadId"].as_str().expect("thread id").to_owned();
    assert_eq!(adopted["disposition"], "created");
    assert_eq!(
        git_output(&external, &["status", "--porcelain"]),
        clean_before_adoption
    );
    assert_eq!(
        git_output(&main, &["worktree", "list", "--porcelain"]),
        worktrees_before_adoption,
        "adoption must not create or re-add a Git worktree"
    );
    let workspace_threads = repositories
        .list_threads_by_project("project-1".to_owned())
        .await
        .expect("project threads")
        .into_iter()
        .filter(|thread| thread.kind == "workspace" && thread.deleted_at.is_none())
        .collect::<Vec<_>>();
    assert_eq!(workspace_threads.len(), 1);
    let adopted_path = PathBuf::from(
        workspace_threads[0]
            .worktree_path
            .as_deref()
            .expect("worktree path"),
    );
    assert_eq!(canonical(&external), adopted_path.to_string_lossy());

    let provider_cwd_gate = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&provider_cwd_fifo)
        .expect("pre-open provider cwd FIFO without a blocking peer wait");
    let mut provider_cwd_unblocker = provider_cwd_gate
        .try_clone()
        .expect("clone provider cwd FIFO gate");
    let mut provider_cwd_reader = BufReader::new(tokio::fs::File::from_std(provider_cwd_gate));
    request(
        &mut socket,
        "26",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.turn.start",
            "commandId":"present-provider-turn",
            "threadId":thread_id,
            "message":{
                "messageId":"present-provider-message",
                "role":"user",
                "text":"record the adopted cwd",
                "attachments":[]
            },
            "runtimeMode":"full-access",
            "interactionMode":"default",
            "createdAt":CREATED_AT
        }),
    )
    .await;
    success(&mut socket, "26").await;
    let mut provider_cwd = String::new();
    match timeout(
        Duration::from_secs(15),
        provider_cwd_reader.read_line(&mut provider_cwd),
    )
    .await
    {
        Ok(read) => {
            assert!(
                read.expect("read provider cwd FIFO") > 0,
                "provider cwd EOF"
            );
        }
        Err(_) => {
            std::io::Write::write_all(&mut provider_cwd_unblocker, b"\n")
                .expect("unblock timed-out provider cwd reader");
            let delivery = repositories
                .get_provider_turn_delivery("present-provider-turn".to_owned())
                .await
                .expect("provider delivery diagnosis");
            panic!("provider fixture did not launch; delivery={delivery:?}");
        }
    }
    assert_eq!(
        provider_cwd.trim(),
        adopted_path.to_string_lossy(),
        "the production provider launch uses the adopted worktree cwd"
    );
    let provider = provider_pid(&provider_pid_file);
    assert!(provider_is_running(provider), "the provider session runs");
    assert!(
        !setup_sentinel.exists(),
        "adoption must not execute a runOnWorktreeCreate project script"
    );

    request(
        &mut socket,
        "4",
        "projects.writeFile",
        json!({"cwd":adopted_path,"relativePath":"rpc-path.txt","contents":"adopted\n"}),
    )
    .await;
    success(&mut socket, "4").await;
    assert_eq!(
        fs::read_to_string(external.join("rpc-path.txt")).expect("RPC-created file"),
        "adopted\n"
    );
    request(
        &mut socket,
        "5",
        "vcs.refreshStatus",
        json!({"cwd":adopted_path}),
    )
    .await;
    let status = success(&mut socket, "5").await;
    assert_eq!(status["refName"], "feature/external");
    request(
        &mut socket,
        "6",
        "terminal.open",
        json!({
            "threadId":thread_id,
            "terminalId":"lifecycle-terminal",
            "cwd":adopted_path,
            "cols":80,
            "rows":24,
            "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(state.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(state.path()),
                }
        }),
    )
    .await;
    success(&mut socket, "6").await;
    let mut terminal_output_socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("terminal output WebSocket connects")
        .0;
    request(
        &mut terminal_output_socket,
        "27",
        "terminal.attach",
        json!({"threadId":thread_id,"terminalId":"lifecycle-terminal"}),
    )
    .await;
    let initial_terminal = chunk(&mut terminal_output_socket, "27").await;
    ack(&mut terminal_output_socket, "27").await;
    assert_eq!(initial_terminal["snapshot"]["status"], "running");
    request(
        &mut socket,
        "30",
        "terminal.write",
        json!({
            "threadId":thread_id,
            "terminalId":"lifecycle-terminal",
            "data":"printf 'CODDY_STUDIO_ACTUAL_CWD=%s\\n' \"$PWD\"\r"
        }),
    )
    .await;
    success(&mut socket, "30").await;
    let expected_cwd = format!("CODDY_STUDIO_ACTUAL_CWD={}", adopted_path.to_string_lossy());
    // A Chunk can carry several events, and a PTY read can split a line between them.
    let mut transcript = String::new();
    timeout(Duration::from_secs(15), async {
        while !transcript.contains(&expected_cwd) {
            let events =
                chunk_values_within_caller_deadline(&mut terminal_output_socket, "27").await;
            ack(&mut terminal_output_socket, "27").await;
            for event in events {
                if event["type"] == "output"
                    && let Some(data) = event["data"].as_str()
                {
                    transcript.push_str(data);
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("terminal process reports its actual cwd within fifteen seconds: {transcript:?}")
    });
    terminal_output_socket
        .close(None)
        .await
        .expect("close terminal output WebSocket");
    let mut terminal_metadata_socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("terminal metadata WebSocket connects")
        .0;
    request(
        &mut terminal_metadata_socket,
        "28",
        "subscribeTerminalMetadata",
        json!({}),
    )
    .await;
    let terminal_metadata = chunk(&mut terminal_metadata_socket, "28").await;
    ack(&mut terminal_metadata_socket, "28").await;
    assert!(
        terminal_metadata["terminals"]
            .as_array()
            .expect("terminal snapshot")
            .iter()
            .any(|terminal| terminal["threadId"] == thread_id
                && terminal["terminalId"] == "lifecycle-terminal"
                && terminal["status"] == "running")
    );
    assert!(
        terminal_metadata["terminals"]
            .as_array()
            .expect("terminal snapshot")
            .iter()
            .all(|terminal| !terminal["terminalId"]
                .as_str()
                .is_some_and(|terminal_id| terminal_id.starts_with("setup-"))),
        "adoption must not launch an observable setup terminal"
    );
    let registrations_before_raw_remove = git_output(&main, &["worktree", "list", "--porcelain"]);
    request(
        &mut socket,
        "31",
        "vcs.removeWorktree",
        json!({"cwd":main,"path":external,"force":true}),
    )
    .await;
    loop {
        match next(&mut socket).await {
            ServerMessage::Defect { defect }
                if defect.as_str().is_some_and(|detail| {
                    detail.contains("Unknown request tag") && detail.contains("vcs.removeWorktree")
                }) =>
            {
                break;
            }
            ServerMessage::Chunk { request_id, .. } => ack(&mut socket, request_id.as_str()).await,
            other => panic!("expected the retired raw removal method to be unavailable: {other:?}"),
        }
    }
    assert!(
        external.is_dir(),
        "raw removal cannot delete the live adopted checkout"
    );
    assert_eq!(
        git_output(&main, &["worktree", "list", "--porcelain"]),
        registrations_before_raw_remove,
        "raw removal cannot mutate Git registration"
    );
    assert!(
        repositories
            .get_thread(thread_id.clone())
            .await
            .expect("owner read after raw removal rejection")
            .is_some_and(|thread| thread.deleted_at.is_none()),
        "raw removal cannot retire the adopted owner"
    );
    assert!(
        provider_is_running(provider),
        "raw removal rejection cannot quiesce the active provider"
    );
    request(
        &mut socket,
        "32",
        "terminal.write",
        json!({"threadId":thread_id,"terminalId":"lifecycle-terminal","data":"\n"}),
    )
    .await;
    success(&mut socket, "32").await;
    request(
        &mut socket,
        "7",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let present_before_degraded = success(&mut socket, "7").await;
    let git_dir = main.join(".git");
    let hidden_git_dir = main.join(".git-observation-unavailable");
    fs::rename(&git_dir, &hidden_git_dir).expect("hide common Git metadata");
    request(
        &mut socket,
        "8",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let degraded = success(&mut socket, "8").await;
    assert_eq!(degraded["authoritative"], false);
    assert_eq!(
        degraded["worktrees"], present_before_degraded["worktrees"],
        "degraded observation retains every authoritative descriptor exactly"
    );
    assert_eq!(
        degraded["adoptedWorkspaces"], present_before_degraded["adoptedWorkspaces"],
        "degraded observation retains every authoritative availability exactly"
    );
    assert!(
        provider_is_running(provider),
        "degraded observation must not invoke provider quiesce"
    );
    assert!(
        repositories
            .list_activities_by_thread(thread_id.clone())
            .await
            .expect("activities")
            .iter()
            .all(|activity| activity.kind != "workspace-unavailable"),
        "degraded observation must not guard or quiesce the workspace"
    );
    request(
        &mut socket,
        "24",
        "terminal.write",
        json!({"threadId":thread_id,"terminalId":"lifecycle-terminal","data":"\n"}),
    )
    .await;
    success(&mut socket, "24").await;
    fs::rename(&hidden_git_dir, &git_dir).expect("restore common Git metadata");

    fs::remove_dir_all(&external).expect("external checkout disappears");
    request(
        &mut socket,
        "9",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let missing = success(&mut socket, "9").await;
    // The server stops the provider whether or not it acknowledged the graceful shutdown in
    // time: a starved provider is killed after Codex's 1 s grace.
    let stop_deadline = tokio::time::Instant::now() + PROVIDER_STOP_BOUND;
    while provider_is_running(provider) && tokio::time::Instant::now() < stop_deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !provider_is_running(provider),
        "authoritative loss stops the provider process"
    );
    request(
        &mut socket,
        "31",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let repeated_missing = success(&mut socket, "31").await;
    assert!(
        repeated_missing["adoptedWorkspaces"]
            .as_array()
            .expect("repeated missing workspaces")
            .iter()
            .any(|workspace| workspace["threadId"] == thread_id
                && workspace["availability"] == "missing-registered")
    );
    assert!(
        !provider_is_running(provider),
        "repeated missing observations leave the provider stopped"
    );
    let terminal_quiesced = timeout(Duration::from_secs(15), async {
        loop {
            let updates =
                chunk_values_within_caller_deadline(&mut terminal_metadata_socket, "28").await;
            ack(&mut terminal_metadata_socket, "28").await;
            if let Some(metadata) = updates.into_iter().find(|metadata| {
                metadata["type"] == "upsert"
                    && metadata["terminal"]["terminalId"] == "lifecycle-terminal"
                    && metadata["terminal"]["status"] == "exited"
            }) {
                break metadata;
            }
        }
    })
    .await
    .expect("authoritative loss quiesces terminal within fifteen seconds");
    assert_eq!(terminal_quiesced["terminal"]["threadId"], thread_id);
    let retained = missing["adoptedWorkspaces"]
        .as_array()
        .expect("adopted workspaces")
        .iter()
        .find(|workspace| workspace["threadId"] == thread_id)
        .expect("retained adopted workspace");
    assert_eq!(retained["availability"], "missing-registered");
    let activities = repositories
        .list_activities_by_thread(thread_id.clone())
        .await
        .expect("activities");
    assert_eq!(
        activities
            .iter()
            .filter(|activity| activity.kind == "workspace-unavailable")
            .count(),
        1,
        "authoritative loss produces one retained warning/quiesce transition"
    );
    request(
        &mut socket,
        "21",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.turn.start",
            "commandId":"blocked-provider-turn",
            "threadId":thread_id,
            "message":{
                "messageId":"blocked-provider-message",
                "role":"user",
                "text":"must not start",
                "attachments":[]
            },
            "runtimeMode":"full-access",
            "interactionMode":"default",
            "createdAt":CREATED_AT
        }),
    )
    .await;
    failure_tag(&mut socket, "21", "WorkspaceUnavailableError").await;
    request(
        &mut socket,
        "22",
        "terminal.open",
        json!({
            "threadId":thread_id,
            "terminalId":"blocked-terminal",
            "cwd":adopted_path,
            "cols":80,
            "rows":24,
            "env": {
                    "HOME": hermetic_providers::isolated_terminal_home(state.path()),
                    "USERPROFILE": hermetic_providers::isolated_terminal_home(state.path()),
                }
        }),
    )
    .await;
    failure_tag(&mut socket, "22", "WorkspaceUnavailableError").await;
    request(
        &mut socket,
        "23",
        "vcs.refreshStatus",
        json!({"cwd":adopted_path}),
    )
    .await;
    failure_tag(&mut socket, "23", "WorkspaceUnavailableError").await;
    request(
        &mut socket,
        "10",
        "projects.writeFile",
        json!({"cwd":adopted_path,"relativePath":"blocked.txt","contents":"blocked"}),
    )
    .await;
    failure_tag(&mut socket, "10", "WorkspaceUnavailableError").await;
    request(
        &mut socket,
        "25",
        "terminal.write",
        json!({"threadId":thread_id,"terminalId":"lifecycle-terminal","data":"\n"}),
    )
    .await;
    failure_tag(&mut socket, "25", "WorkspaceUnavailableError").await;
    let mut terminal_history_socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("terminal history WebSocket connects")
        .0;
    request(
        &mut terminal_history_socket,
        "29",
        "terminal.attach",
        json!({"threadId":thread_id,"terminalId":"lifecycle-terminal"}),
    )
    .await;
    let terminal_history = chunk(&mut terminal_history_socket, "29").await;
    ack(&mut terminal_history_socket, "29").await;
    assert_eq!(terminal_history["snapshot"]["status"], "exited");
    terminal_history_socket
        .close(None)
        .await
        .expect("close terminal history WebSocket");

    git(&main, &["worktree", "remove", "--force"], Some(&external));
    git(
        &main,
        &["worktree", "add", "--force", "-B", "feature/external"],
        Some(&external),
    );
    request(
        &mut socket,
        "11",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let restored = success(&mut socket, "11").await;
    assert_eq!(
        restored["adoptedWorkspaces"]
            .as_array()
            .expect("adopted workspaces")[0]["availability"],
        "present"
    );
    request(
        &mut socket,
        "12",
        "projects.writeFile",
        json!({"cwd":adopted_path,"relativePath":"restored.txt","contents":"restored"}),
    )
    .await;
    success(&mut socket, "12").await;

    request(
        &mut socket,
        "13",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success(&mut socket, "13").await;
    request(
        &mut socket,
        "14",
        "worktree.remove",
        json!({
            "commandId":"remove-external",
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"delete-git-worktree",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":true,
            "confirmRepositoryWidePrune":false
        }),
    )
    .await;
    let removed = success(&mut socket, "14").await;
    assert_eq!(removed["threadRemoved"], true);
    assert!(!external.exists());
    assert!(
        git_output(&main, &["branch", "--list", "feature/external"]).contains("feature/external"),
        "destructive removal preserves the branch"
    );

    let detach_external = root.path().join("detach-external");
    git(
        &main,
        &["worktree", "add", "-b", "feature/detach-external"],
        Some(&detach_external),
    );
    request(
        &mut socket,
        "15",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let detach_snapshot = success(&mut socket, "15").await;
    let detach_candidate = detach_snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["path"] == canonical(&detach_external))
        .expect("detach candidate")
        .clone();
    request(
        &mut socket,
        "16",
        "worktree.adopt",
        json!({
            "commandId":"adopt-detach-external",
            "projectId":"project-1",
            "worktreeKey":detach_candidate["worktreeKey"],
            "expectedGeneration":detach_snapshot["generation"],
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    let detach_adopted = success(&mut socket, "16").await;
    let detach_thread_id = detach_adopted["threadId"]
        .as_str()
        .expect("detach thread id")
        .to_owned();
    fs::remove_dir_all(&detach_external).expect("detach checkout disappears");
    request(
        &mut socket,
        "17",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let detach_missing = success(&mut socket, "17").await;
    assert!(
        detach_missing["adoptedWorkspaces"]
            .as_array()
            .expect("adopted workspaces")
            .iter()
            .any(|workspace| workspace["threadId"] == detach_thread_id
                && workspace["availability"] == "missing-registered")
    );
    request(
        &mut socket,
        "18",
        "worktree.removeFromCoddyStudio",
        json!({
            "commandId":"detach-missing-external",
            "projectId":"project-1",
            "threadId":detach_thread_id
        }),
    )
    .await;
    let detached = success(&mut socket, "18").await;
    assert_eq!(detached["threadRemoved"], true);
    assert_eq!(detached["gitOutcome"], "not-requested");

    terminal_metadata_socket
        .close(None)
        .await
        .expect("close terminal metadata WebSocket");
    socket.close(None).await.expect("close WebSocket");
    drop(terminal_output_socket);
    drop(terminal_history_socket);
    drop(terminal_metadata_socket);
    drop(socket);
    handle.shutdown();
    timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("server joins within five seconds")
        .expect("server joins");
}

#[tokio::test]
async fn detach_succeeds_when_cleanup_cannot_complete() {
    let root = TempDir::new().expect("fixture root");
    let main = root.path().join("main");
    let external = root.path().join("external");
    init_repository(&main);
    git(
        &main,
        &["worktree", "add", "-b", "feature/cleanup-pending"],
        Some(&external),
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
                "type":"project.create",
                "commandId":"create-cleanup-project",
                "projectId":"project-1",
                "title":"Cleanup pending",
                "workspaceRoot":main,
                "defaultModelSelection":null,
                "createdAt":CREATED_AT
            }))
            .expect("project command"),
        )
        .await
        .expect("project created");
    let repositories = engine.repositories();
    let catalog = WorktreeCatalogService::new(
        std::sync::Arc::new(repositories.clone()),
        std::sync::Arc::new(GitRepository::default()),
    );
    let mut registry = RpcRegistry::empty();
    register_worktree_catalog_rpc(
        &mut registry,
        WorktreeCatalogRpcServices::new(catalog.clone(), engine.clone())
            .with_removal_quiescer(std::sync::Arc::new(PendingCleanup)),
    );
    register_orchestration_rpc(&mut registry, engine.clone());
    let state = TempDir::new().expect("server state");
    let config = ServerConfig::new(state.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let handle = ServerRuntime::start_with_registry(config, registry)
        .await
        .expect("RPC server starts");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects")
        .0;

    request(
        &mut socket,
        "1",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success(&mut socket, "1").await;
    let candidate = snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["eligibleForAdoption"] == true)
        .expect("eligible candidate")
        .clone();
    request(
        &mut socket,
        "2",
        "worktree.adopt",
        json!({
            "commandId":"adopt-cleanup-pending",
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
    let adopted = success(&mut socket, "2").await;
    let thread_id = adopted["threadId"].as_str().expect("thread id");
    request(
        &mut socket,
        "3",
        "worktree.removeFromCoddyStudio",
        json!({
            "commandId":"detach-cleanup-pending",
            "projectId":"project-1",
            "threadId":thread_id
        }),
    )
    .await;
    let detached = success(&mut socket, "3").await;
    assert_eq!(detached["threadRemoved"], true);
    assert_eq!(detached["gitOutcome"], "not-requested");
    assert_eq!(detached["orphanCleanupPending"], true);
    assert!(
        external.exists(),
        "detach never mutates the external worktree"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    engine.shutdown().await;
}

struct PendingCleanup;

impl WorktreeRemovalQuiescer for PendingCleanup {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> coddy_studio_server::production::worktree_catalog_rpc::WorktreeRemovalLiveSessionsFuture
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn quiesce(
        &self,
        _admission: WorktreeRemovalCleanupAdmission,
        _request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        Box::pin(async { WorktreeRemovalQuiesceLease::pending(CancellationToken::new()) })
    }
}

fn init_repository(main: &Path) {
    fs::create_dir(main).expect("primary worktree");
    git(main, &["init", "--initial-branch", "main"], None);
    git(main, &["config", "user.email", "rpc@example.invalid"], None);
    git(main, &["config", "user.name", "RPC Test"], None);
    fs::write(main.join("README.md"), "fixture\n").expect("fixture file");
    git(main, &["add", "README.md"], None);
    git(main, &["commit", "-m", "initial"], None);
}

#[cfg(unix)]
fn write_cwd_recording_codex_fixture(
    directory: &Path,
    cwd_fifo: &Path,
    pid_file: &Path,
) -> PathBuf {
    let executable = directory.join("codex-cwd-fixture.sh");
    let script = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'codex-cli 0.0.0\n'
  exit 0
fi
cwd=$(pwd)
printf '%s\n' "$$" > "__CODDY_STUDIO_PID_FILE__"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":%s,"result":{"userAgent":"fixture"}}\n' "$id" ;;
    *'"method":"thread/start"'*) printf '%s\n' "$cwd" > "__CODDY_STUDIO_CWD_FIFO__"; printf '{"id":%s,"result":{"cwd":"%s","model":"gpt-5","thread":{"id":"%s"}}}\n' "$id" "$cwd" "$cwd" ;;
    *'"method":"mcpServerStatus/list"'*) printf '{"id":%s,"result":{"data":[],"nextCursor":null}}\n' "$id" ;;
    *'"method":"thread/goal/set"'*) printf '{"id":%s,"result":{"goal":{"status":"active"}}}\n' "$id" ;;
    *'"method":"turn/start"'*) printf '{"id":%s,"result":{"turn":{"id":"fixture-turn"}}}\n{"method":"item/started","emittedAtMs":1001,"params":{"threadId":"%s","turnId":"fixture-turn","item":{"id":"cwd-observer","type":"collabAgentToolCall","tool":"spawnAgent","status":"inProgress","senderThreadId":"%s","receiverThreadIds":["cwd-observer"],"agentsStates":{"cwd-observer":{"status":"running","message":null}}},"startedAtMs":1001}}\n' "$id" "$cwd" "$cwd" ;;
    *'"method":"turn/interrupt"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"shutdown"'*) printf '{"id":%s,"result":null}\n' "$id" ;;
  esac
done
"#
    .replace("__CODDY_STUDIO_CWD_FIFO__", &cwd_fifo.to_string_lossy())
    .replace("__CODDY_STUDIO_PID_FILE__", &pid_file.to_string_lossy());
    executable_fixture::write_executable(&executable, script);
    executable
}

/// The server stops a lost workspace's provider within one cleanup attempt (its 5 s graceful
/// timeout, which contains Codex's 1 s graceful shutdown and the kill that follows), or in the
/// retry it schedules 1 s after an attempt that timed out.
#[cfg(unix)]
const PROVIDER_STOP_BOUND: Duration = Duration::from_secs(5 + 1 + 5);

/// The provider fixture's process id, which it records when it starts.
#[cfg(unix)]
fn provider_pid(pid_file: &Path) -> libc::pid_t {
    fs::read_to_string(pid_file)
        .expect("provider pid file")
        .trim()
        .parse()
        .expect("provider pid")
}

/// Whether the process exists; the server reaps the provider when it stops it.
#[cfg(unix)]
fn provider_is_running(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let signalled = unsafe { libc::kill(pid, 0) } == 0;
    signalled || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn git(cwd: &Path, args: &[&str], path: Option<&Path>) {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    if let Some(path) = path {
        command.arg(path);
    }
    let output = command.output().expect("Git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("Git command");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).expect("Git UTF-8")
}

#[cfg(unix)]
fn canonical(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonical fixture path")
        .to_string_lossy()
        .into_owned()
}

async fn request(socket: &mut TestSocket, id: &str, tag: &str, payload: Value) {
    socket
        .send(Message::Text(
            json!({"_tag":"Request","id":id,"tag":tag,"payload":payload,"headers":[]})
                .to_string()
                .into(),
        ))
        .await
        .expect("send RPC request");
}

async fn ack(socket: &mut TestSocket, request_id: &str) {
    socket
        .send(Message::Text(
            json!({"_tag":"Ack","requestId":request_id})
                .to_string()
                .into(),
        ))
        .await
        .expect("send stream acknowledgement");
}

async fn next(socket: &mut TestSocket) -> ServerMessage {
    timeout(Duration::from_secs(10), next_within_caller_deadline(socket))
        .await
        .expect("bounded RPC response")
}

/// The next message, with no deadline of its own: for a step whose caller bounds every frame,
/// such as waiting for a real shell to print, which the 10 s per-frame bound would cut short.
async fn next_within_caller_deadline(socket: &mut TestSocket) -> ServerMessage {
    let message = next_frame_past_heartbeat(socket)
        .await
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = message else {
        panic!("expected text frame: {message:?}");
    };
    serde_json::from_str(&text).expect("valid server message")
}

/// The first value of the next Chunk, for a stream whose first Chunk starts with its snapshot.
#[cfg(unix)]
async fn chunk(socket: &mut TestSocket, request_id: &str) -> Value {
    chunk_values(socket, request_id)
        .await
        .into_iter()
        .next()
        .expect("stream value")
}

/// Every value of the next Chunk: the server coalesces queued events into one Chunk.
#[cfg(unix)]
async fn chunk_values(socket: &mut TestSocket, request_id: &str) -> Vec<Value> {
    chunk_frames(socket, request_id, true).await
}

/// Every value of the next Chunk, with no per-frame deadline: the caller bounds the whole step.
#[cfg(unix)]
async fn chunk_values_within_caller_deadline(
    socket: &mut TestSocket,
    request_id: &str,
) -> Vec<Value> {
    chunk_frames(socket, request_id, false).await
}

#[cfg(unix)]
async fn chunk_frames(
    socket: &mut TestSocket,
    request_id: &str,
    frame_deadline: bool,
) -> Vec<Value> {
    loop {
        let message = if frame_deadline {
            next(socket).await
        } else {
            next_within_caller_deadline(socket).await
        };
        match message {
            ServerMessage::Chunk {
                request_id: actual,
                values,
            } if actual.as_str() == request_id => {
                return values;
            }
            ServerMessage::Chunk { request_id, .. } => ack(socket, request_id.as_str()).await,
            other => panic!("expected {request_id} stream chunk: {other:?}"),
        }
    }
}

async fn success(socket: &mut TestSocket, request_id: &str) -> Value {
    loop {
        match next(socket).await {
            ServerMessage::Exit {
                request_id: actual,
                exit: RpcExit::Success { value },
            } if actual.as_str() == request_id => return value.unwrap_or(Value::Null),
            ServerMessage::Chunk { request_id, .. } => ack(socket, request_id.as_str()).await,
            other => panic!("expected {request_id} success: {other:?}"),
        }
    }
}

#[cfg(unix)]
async fn failure_tag(socket: &mut TestSocket, request_id: &str, tag: &str) {
    loop {
        match next(socket).await {
            ServerMessage::Exit {
                request_id: actual,
                exit: RpcExit::Failure { cause },
            } if actual.as_str() == request_id => {
                assert!(
                    cause.iter().any(|item| match item {
                        coddy_studio_server::CauseItem::Fail { error } => error["_tag"] == tag,
                        _ => false,
                    }),
                    "expected {tag} in {cause:?}"
                );
                return;
            }
            ServerMessage::Chunk { request_id, .. } => ack(socket, request_id.as_str()).await,
            other => panic!("expected {request_id} failure: {other:?}"),
        }
    }
}
