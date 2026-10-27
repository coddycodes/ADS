use std::{
    any::Any,
    panic::{AssertUnwindSafe, resume_unwind},
    path::{Path, PathBuf},
    time::Duration,
};

use coddy_studio_server::production::orchestration_rpc::register_orchestration_rpc_with_availability;
use coddy_studio_server::{
    RpcExit, RpcRegistry, ServerConfig, ServerHandle, ServerMessage, ServerRuntime,
    orchestration::{EngineOptions, OrchestrationCommand, OrchestrationEngine, load_snapshot},
    persistence::{CheckpointDiffBlob, Database, NewOrchestrationEvent, run_migrations},
    production::{
        host_paths::process_compatible_path, orchestration_rpc::register_orchestration_rpc,
    },
    worktree_catalog::{
        AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
    },
};
use futures_util::{FutureExt, SinkExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const CREATED_AT: &str = "2026-07-14T10:00:00.000Z";
const RPC_RESPONSE_DEADLINE: Duration = Duration::from_secs(2);
const REAL_GIT_RPC_RESPONSE_DEADLINE: Duration = Duration::from_secs(30);

struct Harness {
    _temp: TempDir,
    engine: OrchestrationEngine,
    registry: RpcRegistry,
    handle: ServerHandle,
}

impl Harness {
    fn workspace_root(&self, name: &str) -> String {
        let path = self._temp.path().join(name);
        std::fs::create_dir_all(&path).expect("workspace root");
        path.to_string_lossy().replace('\\', "/")
    }

    async fn connect(&self) -> WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>> {
        connect_async(format!("ws://{}/ws", self.handle.local_addr()))
            .await
            .expect("WebSocket connects")
            .0
    }

    async fn shutdown(self) -> Result<(), coddy_studio_server::ServerError> {
        self.handle.shutdown();
        let server_result = self.handle.join().await;
        self.engine.shutdown().await;
        server_result
    }
}

async fn finish_test(harness: Harness, outcome: Result<(), Box<dyn Any + Send>>) {
    let shutdown_result = harness.shutdown().await;
    match outcome {
        Ok(()) => shutdown_result.expect("server joins"),
        Err(panic) => {
            if let Err(error) = shutdown_result {
                eprintln!("server failed to join while unwinding test: {error}");
            }
            resume_unwind(panic);
        }
    }
}

fn test_config(temp: &TempDir) -> ServerConfig {
    ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
}

fn tempdir_in_home() -> (TempDir, String) {
    let home = dirs::home_dir()
        .and_then(|path| std::fs::canonicalize(path).ok())
        .expect("canonical home directory");
    let temp = TempDir::new_in(&home).expect("temporary directory in home");
    let relative = temp
        .path()
        .strip_prefix(&home)
        .expect("temporary directory is inside home")
        .to_string_lossy()
        .replace('\\', "/");
    (temp, format!("~/{relative}"))
}

fn canonical_workspace_root(path: &Path) -> String {
    process_compatible_path(std::fs::canonicalize(path).expect("canonical workspace root"))
        .to_string_lossy()
        .into_owned()
}

async fn harness() -> Harness {
    let temp = TempDir::new().expect("temporary base directory");
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
        .expect("engine starts");
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc(&mut registry, engine.clone());
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry.clone())
        .await
        .expect("server starts");
    Harness {
        _temp: temp,
        engine,
        registry,
        handle,
    }
}

#[tokio::test]
async fn workspace_unavailable_rejects_turn_and_generic_owner_delete_without_retiring_guard() {
    let temp = TempDir::new().expect("temporary base directory");
    let workspace = temp.path().join("guarded-workspace");
    std::fs::create_dir(&workspace).expect("workspace");
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
        .expect("engine starts");
    engine
        .dispatch(
            serde_json::from_value(create_project(
                "project-guarded",
                &workspace.to_string_lossy(),
            ))
            .expect("project command"),
        )
        .await
        .expect("project created");
    engine
        .dispatch(
            serde_json::from_value(create_thread(
                "thread-guarded",
                "project-guarded",
                "Guarded thread",
            ))
            .expect("thread command"),
        )
        .await
        .expect("thread created");
    let availability = WorkspaceAvailabilityRegistry::new();
    assert!(
        availability
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-guarded".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 5,
                path: workspace,
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("physical identity resolves")
    );
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_availability(
        &mut registry,
        engine.clone(),
        availability.clone(),
    );
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects")
        .0;

    rpc_request(&mut socket, "1", "orchestration.dispatchCommand", json!({
        "type":"thread.turn.start",
        "commandId":"blocked-turn",
        "threadId":"thread-guarded",
        "message":{"messageId":"message-1","role":"user","text":"must not persist","attachments":[]},
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":CREATED_AT,
    })).await;
    let error = expect_failure(&mut socket, "1").await;
    assert_eq!(error["_tag"], "WorkspaceUnavailableError");
    assert!(
        engine
            .repositories()
            .get_command_receipt("blocked-turn".to_owned())
            .await
            .expect("receipt query")
            .is_none()
    );

    rpc_request(
        &mut socket,
        "2",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.delete",
            "commandId":"delete-guarded-thread",
            "threadId":"thread-guarded",
            "createdAt":CREATED_AT,
        }),
    )
    .await;
    let delete_error = expect_failure(&mut socket, "2").await;
    assert_invalid_request(
        &delete_error,
        "orchestration.dispatchCommand",
        "dedicated server-resolved",
    );
    assert!(
        engine
            .repositories()
            .get_thread("thread-guarded".to_owned())
            .await
            .expect("thread query")
            .is_some_and(|thread| thread.deleted_at.is_none()),
        "generic deletion must leave the adopted owner intact"
    );
    assert!(
        engine
            .repositories()
            .get_command_receipt("delete-guarded-thread".to_owned())
            .await
            .expect("receipt query")
            .is_none(),
        "public rejection happens before durable command admission"
    );
    assert!(
        availability.guard_thread("thread-guarded").await.is_err(),
        "generic rejection cannot silently retire the adopted workspace guard"
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    engine.shutdown().await;
}

#[tokio::test]
async fn workspace_unavailable_resolves_a_persisted_panel_path_before_turn_admission() {
    let temp = TempDir::new().expect("temporary base directory");
    let workspace = temp.path().join("guarded-workspace");
    std::fs::create_dir(&workspace).expect("workspace");
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
        .expect("engine starts");
    engine
        .dispatch(
            serde_json::from_value(create_project(
                "project-panel-guard",
                &temp.path().to_string_lossy(),
            ))
            .expect("project command"),
        )
        .await
        .expect("project created");
    for (thread_id, kind, path) in [
        (
            "workspace-owner",
            "workspace",
            workspace.to_string_lossy().into_owned(),
        ),
        (
            "panel-alias",
            "panel",
            workspace
                .join(".")
                .join("alias")
                .join("..")
                .to_string_lossy()
                .into_owned(),
        ),
    ] {
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.create",
                    "commandId":format!("create-{thread_id}"),
                    "threadId":thread_id,
                    "projectId":"project-panel-guard",
                    "title":thread_id,
                    "kind":kind,
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":"feature/panel-guard",
                    "worktreePath":path,
                    "createdAt":CREATED_AT,
                }))
                .expect("thread command"),
            )
            .await
            .expect("thread created");
    }
    let availability = WorkspaceAvailabilityRegistry::new();
    assert!(
        availability
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "workspace-owner".to_owned(),
                repository_key: "repository-1".to_owned(),
                generation: 6,
                path: workspace,
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("physical identity resolves")
    );
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_availability(&mut registry, engine.clone(), availability);
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry)
        .await
        .expect("server starts");
    let mut socket = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects")
        .0;

    rpc_request(&mut socket, "1", "orchestration.dispatchCommand", json!({
        "type":"thread.turn.start",
        "commandId":"blocked-panel-turn",
        "threadId":"panel-alias",
        "message":{"messageId":"panel-message","role":"user","text":"must not persist","attachments":[]},
        "runtimeMode":"full-access",
        "interactionMode":"default",
        "createdAt":CREATED_AT,
    })).await;
    let error = expect_failure(&mut socket, "1").await;
    assert_eq!(error["_tag"], "WorkspaceUnavailableError");
    assert_eq!(error["threadId"], "workspace-owner");
    assert!(
        engine
            .repositories()
            .get_command_receipt("blocked-panel-turn".to_owned())
            .await
            .expect("receipt query")
            .is_none(),
        "panel loss must reject before durable turn admission",
    );

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    engine.shutdown().await;
}

async fn harness_with_historical_workspace() -> (Harness, PathBuf) {
    let temp = TempDir::new().expect("temporary base directory");
    let historical_workspace = temp.path().join("historical-workspace");
    std::fs::create_dir(&historical_workspace).expect("historical workspace");
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
        .expect("engine starts");
    engine
        .dispatch(
            serde_json::from_value::<OrchestrationCommand>(json!({
                "type": "project.create",
                "commandId": "create-historical-workspace",
                "projectId": "historical-workspace",
                "title": "Historical Workspace",
                "workspaceRoot": format!("{}/.", historical_workspace.display()),
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }))
            .expect("historical command"),
        )
        .await
        .expect("historical project");
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc(&mut registry, engine.clone());
    let handle = ServerRuntime::start_with_registry(test_config(&temp), registry.clone())
        .await
        .expect("server starts");
    (
        Harness {
            _temp: temp,
            engine,
            registry,
            handle,
        },
        historical_workspace,
    )
}

fn create_project(project_id: &str, workspace_root: &str) -> Value {
    json!({
        "type": "project.create",
        "commandId": format!("create-{project_id}"),
        "projectId": project_id,
        "title": format!("Project {project_id}"),
        "workspaceRoot": workspace_root,
        "defaultModelSelection": null,
        "createdAt": CREATED_AT,
    })
}

fn create_thread(thread_id: &str, project_id: &str, title: &str) -> Value {
    json!({
        "type": "thread.create",
        "commandId": format!("create-{thread_id}"),
        "threadId": thread_id,
        "projectId": project_id,
        "title": title,
        "kind": "workspace",
        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "branch": format!("branch-{thread_id}"),
        "worktreePath": format!("C:/worktrees/{thread_id}"),
        "createdAt": CREATED_AT,
    })
}

fn create_public_thread(thread_id: &str, project_id: &str, title: &str) -> Value {
    json!({
        "type": "thread.create",
        "commandId": format!("create-{thread_id}"),
        "threadId": thread_id,
        "projectId": project_id,
        "title": title,
        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
        "runtimeMode": "full-access",
        "interactionMode": "default",
        "createdAt": CREATED_AT,
    })
}

#[tokio::test]
async fn orchestration_rpc_registration_has_the_contract_modes() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let issues = harness
            .registry
            .validate_complete()
            .expect_err("the focused registry intentionally omits non-orchestration RPCs");
        for method in [
            "orchestration.dispatchCommand",
            "orchestration.getArchivedShellSnapshot",
            "orchestration.getFullThreadDiff",
            "orchestration.getTurnDiff",
            "orchestration.replayEvents",
            "orchestration.subscribeShell",
            "orchestration.subscribeThread",
        ] {
            assert!(
                !issues.contains(method),
                "{method} was missing or registered with the wrong mode: {issues}"
            );
        }
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn public_dispatch_rejects_every_generic_worktree_authority_bypass() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let authority_workspace = harness.workspace_root("authority-project");
        harness
            .engine
            .dispatch(
                serde_json::from_value(create_project(
                    "authority-project",
                    &authority_workspace,
                ))
                .expect("project command"),
            )
            .await
            .expect("project created");
        harness
            .engine
            .dispatch(
                serde_json::from_value(create_thread(
                    "adopted-owner",
                    "authority-project",
                    "Adopted owner",
                ))
                .expect("internal owner command"),
            )
            .await
            .expect("internal adopted owner created");
        let default_thread_id = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.project_id == "authority-project" && thread.kind == "default")
            .expect("default thread")
            .thread_id;
        let mut socket = harness.connect().await;
        let rejected = [
            (
                "61",
                json!({
                    "type": "worktree.adopt-resolved",
                    "commandId": "internal-adopt",
                    "projectId": "project-1",
                    "worktreeKey": "worktree-key",
                    "path": "/server/resolved",
                    "branch": "main",
                    "head": "abcdef1",
                    "modelSelection": {"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode": "full-access",
                    "interactionMode": "default"
                }),
            ),
            (
                "62",
                json!({
                    "type": "worktree.branch-reconcile-resolved",
                    "commandId": "internal-reconcile",
                    "projectId": "project-1",
                    "threadId": "thread-1",
                    "branch": "main"
                }),
            ),
            (
                "63",
                json!({
                    "type":"worktree.detach-resolved",
                    "commandId":"internal-detach",
                    "projectId":"authority-project",
                    "threadId":"adopted-owner",
                    "path":"C:/worktrees/adopted-owner",
                    "gitOutcome":"not-requested",
                    "detail":null,
                    "orphanCleanupPending":false
                }),
            ),
            (
                "71",
                json!({
                    "type":"thread.history.import",
                    "commandId":"internal-history-import",
                    "threadId":default_thread_id,
                    "messages":[{
                        "messageId":"forged-history",
                        "role":"assistant",
                        "text":"forged",
                        "createdAt":CREATED_AT
                    }]
                }),
            ),
            (
                "64",
                json!({
                    "type":"project.meta.update",
                    "commandId":"raw-discovery-policy",
                    "projectId":"authority-project",
                    "worktreeDiscovery":{
                        "visibility":"shown",
                        "initialPromptDismissedAt":null,
                        "baselinePaths":["C:/client/chosen-baseline"]
                    }
                }),
            ),
            (
                "65",
                json!({
                    "type":"thread.create",
                    "commandId":"raw-kind-create",
                    "threadId":"raw-panel",
                    "projectId":"authority-project",
                    "title":"Raw panel",
                    "kind":"panel",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":null,
                    "createdAt":CREATED_AT
                }),
            ),
            (
                "66",
                json!({
                    "type":"thread.create",
                    "commandId":"raw-path-create",
                    "threadId":"raw-owner",
                    "projectId":"authority-project",
                    "title":"Raw owner",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":"raw",
                    "worktreePath":"C:/client/chosen-worktree",
                    "createdAt":CREATED_AT
                }),
            ),
            (
                "67",
                json!({
                    "type":"thread.meta.update",
                    "commandId":"raw-retarget",
                    "threadId":default_thread_id,
                    "worktreePath":"C:/client/chosen-retarget"
                }),
            ),
            (
                "68",
                json!({
                    "type":"thread.turn.start",
                    "commandId":"raw-bootstrap-path",
                    "threadId":"raw-bootstrap-owner",
                    "message":{"messageId":"raw-bootstrap-message","role":"user","text":"raw","attachments":[]},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "bootstrap":{
                        "createThread":{
                            "projectId":"authority-project",
                            "title":"Raw bootstrap",
                            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                            "runtimeMode":"full-access",
                            "interactionMode":"default",
                            "branch":"raw",
                            "worktreePath":"C:/client/chosen-bootstrap",
                            "createdAt":CREATED_AT
                        },
                        "prepareWorktree":{
                            "projectCwd":"C:/client/chosen-repository",
                            "baseBranch":"main"
                        }
                    },
                    "createdAt":CREATED_AT
                }),
            ),
            (
                "69",
                json!({
                    "type":"thread.delete",
                    "commandId":"raw-adopted-delete",
                    "threadId":"adopted-owner"
                }),
            ),
            (
                "70",
                json!({
                    "type":"project.delete",
                    "commandId":"raw-project-delete",
                    "projectId":"authority-project",
                    "force":true
                }),
            ),
            (
                "71",
                json!({
                    "type":"thread.create",
                    "commandId":"raw-host-create",
                    "threadId":"raw-hosted",
                    "projectId":"authority-project",
                    "title":"Raw hosted",
                    "hostThreadId":default_thread_id,
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":null,
                    "createdAt":CREATED_AT
                }),
            ),
        ];
        for (request_id, payload) in rejected {
            let command_id = payload["commandId"]
                .as_str()
                .expect("rejected command id")
                .to_owned();
            let expected_message = if command_id.starts_with("internal-") {
                "server-internal"
            } else {
                "dedicated server-resolved"
            };
            rpc_request(
                &mut socket,
                request_id,
                "orchestration.dispatchCommand",
                payload,
            )
            .await;
            let error = expect_failure(&mut socket, request_id).await;
            assert_invalid_request(
                &error,
                "orchestration.dispatchCommand",
                expected_message,
            );
            assert!(
                harness
                    .engine
                    .repositories()
                    .get_command_receipt(command_id)
                    .await
                    .expect("receipt query")
                    .is_none(),
                "a rejected public worktree-authority bypass cannot reserve or persist a receipt"
            );
        }
        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn assistant_completion_replay_preserves_the_declared_message_payload_shape() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let workspace_root = harness.workspace_root("assistant-completion-replay");
        for command in [
            create_project("completion-replay-project", &workspace_root),
            create_thread(
                "completion-replay-thread",
                "completion-replay-project",
                "Completion replay",
            ),
            json!({
                "type": "thread.message.assistant.delta",
                "commandId": "completion-replay-delta",
                "threadId": "completion-replay-thread",
                "messageId": "completion-replay-message",
                "delta": "response",
                "turnId": "completion-replay-turn",
                "createdAt": CREATED_AT,
            }),
            json!({
                "type": "thread.message.assistant.complete",
                "commandId": "completion-replay-complete",
                "threadId": "completion-replay-thread",
                "messageId": "completion-replay-message",
                "turnId": "completion-replay-turn",
                "createdAt": CREATED_AT,
            }),
        ] {
            harness
                .engine
                .dispatch(serde_json::from_value(command).expect("command decodes"))
                .await
                .expect("command dispatches");
        }

        let mut socket = harness.connect().await;
        let replay = unary_success(
            &mut socket,
            "1",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": 0 }),
        )
        .await;
        let completion = replay
            .as_array()
            .expect("replayed events")
            .iter()
            .find(|event| event["commandId"] == "completion-replay-complete")
            .expect("assistant completion event");
        assert_eq!(
            completion["payload"],
            json!({
                "threadId": "completion-replay-thread",
                "messageId": "completion-replay-message",
                "role": "assistant",
                "text": "",
                "turnId": "completion-replay-turn",
                "streaming": false,
                "createdAt": CREATED_AT,
                "updatedAt": CREATED_AT,
            })
        );
        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn project_create_can_initialize_git_before_registration() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let workspace = harness._temp.path().join("git-project");

        dispatch_real_git_command(
            &mut socket,
            "1",
            json!({
                "type": "project.create",
                "commandId": "create-git-project",
                "projectId": "git-project",
                "title": "Git Project",
                "workspaceRoot": workspace,
                "createWorkspaceRootIfMissing": true,
                "initializeGit": true,
                "createdAt": CREATED_AT,
            }),
        )
        .await;

        assert!(workspace.join(".git").is_dir());
        let snapshot = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot");
        assert!(
            snapshot
                .projects
                .iter()
                .any(|project| project.project_id == "git-project")
        );
        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn project_create_replay_skips_filesystem_effects() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let replayed_workspace = harness._temp.path().join("replayed-project");
        let replayed_command = json!({
            "type": "project.create",
            "commandId": "create-replayed-project",
            "projectId": "replayed-project",
            "title": "Replayed Project",
            "workspaceRoot": replayed_workspace,
            "createWorkspaceRootIfMissing": true,
            "initializeGit": false,
            "createdAt": CREATED_AT,
        });

        let first_result = dispatch_command(&mut socket, "21", replayed_command.clone()).await;
        assert!(replayed_workspace.is_dir());
        std::fs::remove_dir(&replayed_workspace).expect("remove replayed workspace");

        let replay_result = dispatch_command(&mut socket, "22", replayed_command).await;
        assert_eq!(replay_result, first_result);
        assert!(
            !replayed_workspace.exists(),
            "accepted-command replay must not recreate the workspace"
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn canonical_duplicate_project_create_skips_git_initialization() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let registered_workspace = harness._temp.path().join("registered-non-git");
        std::fs::create_dir(&registered_workspace).expect("registered workspace");
        let registered_result = dispatch_command(
            &mut socket,
            "23",
            json!({
                "type": "project.create",
                "commandId": "create-registered-non-git",
                "projectId": "registered-non-git",
                "title": "Registered Non Git",
                "workspaceRoot": registered_workspace,
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        let duplicate_result = dispatch_command(
            &mut socket,
            "24",
            json!({
                "type": "project.create",
                "commandId": "create-registered-non-git-duplicate",
                "projectId": "registered-non-git-duplicate",
                "title": "Registered Non Git Duplicate",
                "workspaceRoot": registered_workspace.join("."),
                "createWorkspaceRootIfMissing": false,
                "initializeGit": true,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(
            duplicate_result["projectId"],
            registered_result["projectId"]
        );
        assert!(
            !registered_workspace.join(".git").exists(),
            "canonical duplicate preflight must run before git init"
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn project_create_resolves_home_relative_paths_and_reuses_canonical_duplicates() {
    let harness = harness().await;
    let (home_temp, home_relative_root) = tempdir_in_home();
    let backslash_home_relative_root = home_relative_root.replace('/', "\\");
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let existing = home_temp.path().join("existing");
        std::fs::create_dir(&existing).expect("existing workspace");

        let existing_result = dispatch_command(
            &mut socket,
            "11",
            json!({
                "type": "project.create",
                "commandId": "create-home-existing",
                "projectId": "home-existing",
                "title": "Home Existing",
                "workspaceRoot": format!("{home_relative_root}/existing"),
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(existing_result["projectId"], json!("home-existing"));

        let duplicate_result = dispatch_command(
            &mut socket,
            "12",
            json!({
                "type": "project.create",
                "commandId": "create-home-existing-duplicate",
                "projectId": "home-existing-duplicate",
                "title": "Home Existing Duplicate",
                "workspaceRoot": std::fs::canonicalize(&existing).expect("canonical existing path"),
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(duplicate_result["projectId"], json!("home-existing"));

        let backslash_existing = home_temp.path().join("backslash-existing");
        std::fs::create_dir(&backslash_existing).expect("backslash existing workspace");
        let backslash_existing_result = dispatch_command(
            &mut socket,
            "15",
            json!({
                "type": "project.create",
                "commandId": "create-home-backslash-existing",
                "projectId": "home-backslash-existing",
                "title": "Home Backslash Existing",
                "workspaceRoot": format!("{backslash_home_relative_root}\\backslash-existing"),
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(
            backslash_existing_result["projectId"],
            json!("home-backslash-existing")
        );

        let created = home_temp.path().join("created");
        let created_result = dispatch_real_git_command(
            &mut socket,
            "13",
            json!({
                "type": "project.create",
                "commandId": "create-home-created",
                "projectId": "home-created",
                "title": "Home Created",
                "workspaceRoot": format!("{home_relative_root}/created"),
                "createWorkspaceRootIfMissing": true,
                "initializeGit": true,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(created_result["projectId"], json!("home-created"));
        assert!(created.join(".git").is_dir());

        let backslash_created = home_temp.path().join("backslash-created");
        let backslash_created_result = dispatch_real_git_command(
            &mut socket,
            "16",
            json!({
                "type": "project.create",
                "commandId": "create-home-backslash-created",
                "projectId": "home-backslash-created",
                "title": "Home Backslash Created",
                "workspaceRoot": format!("{backslash_home_relative_root}\\backslash-created"),
                "createWorkspaceRootIfMissing": true,
                "initializeGit": true,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(
            backslash_created_result["projectId"],
            json!("home-backslash-created")
        );
        assert!(backslash_created.join(".git").is_dir());

        let snapshot = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot");
        let canonical_existing = canonical_workspace_root(&existing);
        let canonical_created = canonical_workspace_root(&created);
        assert_eq!(
            snapshot
                .projects
                .iter()
                .filter(|project| project.workspace_root == canonical_existing)
                .count(),
            1
        );
        assert!(
            snapshot
                .projects
                .iter()
                .any(|project| project.workspace_root == canonical_created)
        );
        for expected in [&backslash_existing, &backslash_created] {
            let canonical = canonical_workspace_root(expected);
            assert!(
                snapshot
                    .projects
                    .iter()
                    .any(|project| project.workspace_root == canonical)
            );
        }

        let named_user_path = "~coddy-studio-final-review-user/project";
        rpc_request(
            &mut socket,
            "14",
            "orchestration.dispatchCommand",
            json!({
                "type": "project.create",
                "commandId": "do-not-expand-named-user",
                "projectId": "named-user",
                "title": "Named User",
                "workspaceRoot": named_user_path,
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        let named_user_error = expect_failure(&mut socket, "14").await;
        let named_user_marker = named_user_path
            .split('/')
            .next()
            .expect("named-user path marker");
        assert!(
            named_user_error["message"]
                .as_str()
                .is_some_and(|message| message.contains(named_user_marker)),
            "named-user tilde paths must remain literal: {named_user_error}"
        );
        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn failed_git_initialization_does_not_register_project() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let workspace = harness._temp.path().join("blocked-git-project");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::write(workspace.join(".git"), "not a directory").expect("blocking .git file");

        rpc_request(
            &mut socket,
            "1",
            "orchestration.dispatchCommand",
            json!({
                "type": "project.create",
                "commandId": "create-blocked-git-project",
                "projectId": "blocked-git-project",
                "title": "Blocked Git Project",
                "workspaceRoot": workspace,
                "createWorkspaceRootIfMissing": false,
                "initializeGit": true,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        let error =
            expect_failure_with_deadline(&mut socket, "1", REAL_GIT_RPC_RESPONSE_DEADLINE).await;
        assert_eq!(error["_tag"], json!("InvalidRequest"));

        let snapshot = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot");
        assert!(
            !snapshot
                .projects
                .iter()
                .any(|project| project.project_id == "blocked-git-project")
        );
        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn historical_cached_workspace_roots_participate_in_create_duplicate_preflight() {
    let (harness, historical_workspace) = harness_with_historical_workspace().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let historical_duplicate = dispatch_command(
            &mut socket,
            "31",
            json!({
                "type": "project.create",
                "commandId": "create-historical-workspace-duplicate",
                "projectId": "historical-workspace-duplicate",
                "title": "Historical Workspace Duplicate",
                "workspaceRoot": std::fs::canonicalize(&historical_workspace)
                    .expect("canonical historical workspace"),
                "createWorkspaceRootIfMissing": false,
                "initializeGit": true,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(
            historical_duplicate["projectId"],
            json!("historical-workspace")
        );
        assert!(
            !historical_workspace.join(".git").exists(),
            "historical cached aliases must participate in duplicate preflight"
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn historical_cached_workspace_root_replaced_by_file_does_not_block_unrelated_create() {
    let (harness, historical_workspace) = harness_with_historical_workspace().await;
    std::fs::remove_dir(&historical_workspace).expect("remove historical workspace");
    std::fs::write(&historical_workspace, "stale historical workspace")
        .expect("replace historical workspace with a file");
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let unrelated_workspace = harness._temp.path().join("unrelated-workspace");
        std::fs::create_dir(&unrelated_workspace).expect("unrelated workspace");

        let created = dispatch_command(
            &mut socket,
            "37",
            json!({
                "type": "project.create",
                "commandId": "create-unrelated-workspace",
                "projectId": "unrelated-workspace",
                "title": "Unrelated Workspace",
                "workspaceRoot": unrelated_workspace,
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(created["projectId"], json!("unrelated-workspace"));

        let snapshot = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot after unrelated create");
        assert!(
            snapshot
                .projects
                .iter()
                .any(|project| project.project_id == "unrelated-workspace"),
            "a stale historical root must not reject an unrelated project command"
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn project_metadata_workspace_roots_are_canonical() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let first_workspace = harness._temp.path().join("first-workspace");
        let moved_workspace = harness._temp.path().join("moved-workspace");
        for workspace in [&first_workspace, &moved_workspace] {
            std::fs::create_dir(workspace).expect("workspace");
        }
        dispatch_command(
            &mut socket,
            "32",
            create_project(
                "first-workspace",
                first_workspace.to_string_lossy().as_ref(),
            ),
        )
        .await;

        dispatch_command(
            &mut socket,
            "34",
            json!({
                "type": "project.meta.update",
                "commandId": "move-first-workspace-through-alias",
                "projectId": "first-workspace",
                "workspaceRoot": format!("{}/.", moved_workspace.display()),
            }),
        )
        .await;
        let canonical_moved = canonical_workspace_root(&moved_workspace);
        let snapshot = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot after normalized update");
        assert_eq!(
            snapshot
                .projects
                .iter()
                .find(|project| project.project_id == "first-workspace")
                .expect("first project")
                .workspace_root,
            canonical_moved
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn project_metadata_workspace_root_aliases_and_symlinks_cannot_collide() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let first_workspace = harness._temp.path().join("first-workspace");
        let second_workspace = harness._temp.path().join("second-workspace");
        for workspace in [&first_workspace, &second_workspace] {
            std::fs::create_dir(workspace).expect("workspace");
        }
        dispatch_command(
            &mut socket,
            "32",
            create_project(
                "first-workspace",
                first_workspace.to_string_lossy().as_ref(),
            ),
        )
        .await;
        dispatch_command(
            &mut socket,
            "33",
            create_project(
                "second-workspace",
                second_workspace.to_string_lossy().as_ref(),
            ),
        )
        .await;

        rpc_request(
            &mut socket,
            "35",
            "orchestration.dispatchCommand",
            json!({
                "type": "project.meta.update",
                "commandId": "collide-second-workspace-through-alias",
                "projectId": "second-workspace",
                "workspaceRoot": format!("{}/.", first_workspace.display()),
            }),
        )
        .await;
        let alias_error = expect_failure(&mut socket, "35").await;
        assert!(
            alias_error["message"]
                .as_str()
                .is_some_and(|message| message.to_ascii_lowercase().contains("workspace root")),
            "unexpected collision error: {alias_error}"
        );

        #[cfg(unix)]
        {
            let first_symlink = harness._temp.path().join("first-workspace-link");
            std::os::unix::fs::symlink(&first_workspace, &first_symlink)
                .expect("workspace symlink");
            rpc_request(
                &mut socket,
                "36",
                "orchestration.dispatchCommand",
                json!({
                    "type": "project.meta.update",
                    "commandId": "collide-second-workspace-through-symlink",
                    "projectId": "second-workspace",
                    "workspaceRoot": first_symlink,
                }),
            )
            .await;
            let symlink_error = expect_failure(&mut socket, "36").await;
            assert!(
                symlink_error["message"]
                    .as_str()
                    .is_some_and(|message| message.to_ascii_lowercase().contains("workspace root")),
                "unexpected symlink collision error: {symlink_error}"
            );
        }

        let final_snapshot = load_snapshot(&harness.engine.repositories())
            .await
            .expect("final snapshot");
        assert_eq!(
            final_snapshot
                .projects
                .iter()
                .find(|project| project.project_id == "second-workspace")
                .expect("second project")
                .workspace_root,
            canonical_workspace_root(&second_workspace)
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn canonical_duplicate_receipt_uses_the_unbounded_event_maximum() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;
        let workspace = harness.workspace_root("sequence-workspace");
        dispatch_command(
            &mut socket,
            "41",
            create_project("sequence-workspace", &workspace),
        )
        .await;

        let mut expected_max = 0;
        for index in 0..1_005 {
            expected_max = harness
                .engine
                .repositories()
                .append_event(NewOrchestrationEvent {
                    event_id: format!("sequence-fixture-{index}"),
                    event_type: "test.sequence-fixture".to_owned(),
                    aggregate_kind: "test".to_owned(),
                    aggregate_id: "sequence-fixture".to_owned(),
                    occurred_at: CREATED_AT.to_owned(),
                    command_id: None,
                    causation_event_id: None,
                    correlation_id: None,
                    payload: json!({"index":index}),
                    metadata: json!({}),
                })
                .await
                .expect("sequence fixture event")
                .sequence;
        }
        assert!(expected_max > 1_000);

        let duplicate = dispatch_command(
            &mut socket,
            "42",
            json!({
                "type": "project.create",
                "commandId": "create-sequence-workspace-duplicate",
                "projectId": "sequence-workspace-duplicate",
                "title": "Sequence Workspace Duplicate",
                "workspaceRoot": workspace,
                "createWorkspaceRootIfMissing": false,
                "initializeGit": false,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        assert_eq!(duplicate["sequence"], json!(expected_max));
        assert_eq!(duplicate["projectId"], json!("sequence-workspace"));

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn orchestration_lifecycle_and_query_rpcs_round_trip_real_state() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;

        let project_result = dispatch_command(
            &mut socket,
            "1",
            create_project("project-1", &harness.workspace_root("project-1")),
        )
        .await;
        assert_eq!(project_result["sequence"], json!(2));

        let default_thread_id = load_snapshot(&harness.engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.project_id == "project-1" && thread.kind == "default")
            .expect("default thread")
            .thread_id;

        let created_thread = dispatch_command(
            &mut socket,
            "2",
            create_public_thread("thread-1", "project-1", "Workspace thread"),
        )
        .await;
        assert!(
            created_thread["sequence"].as_i64().unwrap()
                > project_result["sequence"].as_i64().unwrap()
        );

        dispatch_command(
            &mut socket,
            "3",
            json!({
                "type": "thread.meta.update",
                "commandId": "rename-thread-1",
                "threadId": "thread-1",
                "title": "Workspace thread renamed",
                "branch": "feature/rpc",
            }),
        )
        .await;
        rpc_request(
            &mut socket,
            "4",
            "orchestration.dispatchCommand",
            json!({
                "type": "thread.turn.start",
                "commandId": "start-thread-1",
                "threadId": "thread-1",
                "message": {
                    "messageId": "message-1",
                    "role": "user",
                    "text": "hello from rpc",
                    "attachments": []
                },
                "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        let turn_without_delivery = expect_failure(&mut socket, "4").await;
        assert_invalid_request(
            &turn_without_delivery,
            "orchestration.dispatchCommand",
            "requires durable provider delivery",
        );
        dispatch_command(
            &mut socket,
            "5",
            json!({
                "type": "thread.approval.respond",
                "commandId": "approve-thread-1",
                "threadId": "thread-1",
                "requestId": "approval-1",
                "decision": "acceptForSession",
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        dispatch_command(
            &mut socket,
            "6",
            json!({
                "type": "thread.user-input.respond",
                "commandId": "answer-thread-1",
                "threadId": "thread-1",
                "requestId": "user-input-1",
                "answers": {"question": "answer"},
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        dispatch_command(
            &mut socket,
            "7",
            json!({
                "type": "thread.checkpoint.revert",
                "commandId": "revert-thread-1",
                "threadId": "thread-1",
                "turnCount": 0,
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        dispatch_command(
            &mut socket,
            "8",
            json!({
                "type": "thread.session.stop",
                "commandId": "stop-thread-1",
                "threadId": "thread-1",
                "createdAt": CREATED_AT,
            }),
        )
        .await;
        dispatch_command(
            &mut socket,
            "9",
            json!({
                "type": "thread.archive",
                "commandId": "archive-thread-1",
                "threadId": "thread-1",
            }),
        )
        .await;

        harness
            .engine
            .repositories()
            .upsert_checkpoint_diff_blob(CheckpointDiffBlob {
                thread_id: "thread-1".to_owned(),
                from_turn_count: 0,
                to_turn_count: 1,
                diff: "--- before\n+++ after\n@@\n-hello\n+hello from rpc\n".to_owned(),
                created_at: CREATED_AT.to_owned(),
            })
            .await
            .expect("diff blob");

        let archived_snapshot = unary_success(
            &mut socket,
            "10",
            "orchestration.getArchivedShellSnapshot",
            json!({}),
        )
        .await;
        assert!(archived_snapshot["snapshotSequence"].as_i64().unwrap() >= 9);
        assert_eq!(archived_snapshot["projects"].as_array().unwrap().len(), 1);
        assert_eq!(
            archived_snapshot["projects"][0]["worktreeDiscovery"],
            json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]})
        );
        let archived_threads = archived_snapshot["threads"]
            .as_array()
            .expect("archived threads");
        assert_eq!(archived_threads.len(), 1);
        assert_eq!(archived_threads[0]["id"], json!("thread-1"));
        assert_eq!(
            archived_threads[0]["title"],
            json!("Workspace thread renamed")
        );
        assert_eq!(archived_threads[0]["hasPendingApprovals"], json!(false));
        assert!(archived_threads[0]["archivedAt"].is_string());

        let turn_diff = unary_success(
            &mut socket,
            "11",
            "orchestration.getTurnDiff",
            json!({
                "threadId": "thread-1",
                "fromTurnCount": 0,
                "toTurnCount": 1,
            }),
        )
        .await;
        assert_eq!(turn_diff["threadId"], json!("thread-1"));
        assert_eq!(turn_diff["fromTurnCount"], json!(0));
        assert_eq!(turn_diff["toTurnCount"], json!(1));
        assert!(
            turn_diff["diff"]
                .as_str()
                .unwrap()
                .contains("+hello from rpc")
        );

        let full_diff = unary_success(
            &mut socket,
            "12",
            "orchestration.getFullThreadDiff",
            json!({
                "threadId": "thread-1",
                "toTurnCount": 1,
            }),
        )
        .await;
        assert_eq!(full_diff, turn_diff);

        dispatch_command(
            &mut socket,
            "13",
            json!({
                "type": "thread.unarchive",
                "commandId": "unarchive-thread-1",
                "threadId": "thread-1",
            }),
        )
        .await;
        dispatch_command(
            &mut socket,
            "14",
            json!({
                "type": "thread.delete",
                "commandId": "delete-thread-1",
                "threadId": "thread-1",
            }),
        )
        .await;
        dispatch_command(
            &mut socket,
            "15",
            json!({
                "type": "project.delete",
                "commandId": "delete-project-1",
                "projectId": "project-1",
                "force": true,
            }),
        )
        .await;

        let replay_all = unary_success(
            &mut socket,
            "16",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": 0 }),
        )
        .await;
        let replay_all = replay_all.as_array().expect("events");
        assert!(replay_all.len() >= 11);
        assert_eq!(replay_all[0]["type"], json!("project.created"));
        assert_eq!(replay_all[1]["type"], json!("thread.created"));
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.approval-response-requested"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.user-input-response-requested"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.checkpoint-revert-requested"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.session-stop-requested"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.archived"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.unarchived"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("thread.deleted"))
        );
        assert!(
            replay_all
                .iter()
                .any(|event| event["type"] == json!("project.deleted"))
        );
        for event in replay_all {
            let object = event.as_object().expect("event object");
            for field in [
                "sequence",
                "eventId",
                "type",
                "aggregateKind",
                "aggregateId",
                "occurredAt",
                "commandId",
                "causationEventId",
                "correlationId",
                "payload",
                "metadata",
            ] {
                assert!(object.contains_key(field), "missing field {field}");
            }
        }

        let paginated_replay = unary_success(
            &mut socket,
            "17",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": 6 }),
        )
        .await;
        let paginated_replay = paginated_replay.as_array().expect("paged events");
        assert!(!paginated_replay.is_empty());
        assert!(
            paginated_replay[0]["sequence"].as_i64().unwrap() > 6,
            "replay should start after the exclusive cursor"
        );
        assert!(paginated_replay.len() < replay_all.len());

        let archived_after_delete = unary_success(
            &mut socket,
            "18",
            "orchestration.getArchivedShellSnapshot",
            json!({}),
        )
        .await;
        assert!(
            archived_after_delete["threads"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        socket.close(None).await.expect("close WebSocket");
        assert!(
            load_snapshot(&harness.engine.repositories())
                .await
                .expect("final snapshot")
                .threads
                .iter()
                .any(|thread| thread.thread_id == default_thread_id)
        );
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn shell_and_thread_streams_refresh_on_relevant_events_and_interrupt_cleanly() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut command_socket = harness.connect().await;
        dispatch_command(
            &mut command_socket,
            "1",
            create_project(
                "project-streams",
                &harness.workspace_root("project-streams"),
            ),
        )
        .await;
        dispatch_command(
            &mut command_socket,
            "2",
            create_public_thread("thread-watched", "project-streams", "Watched thread"),
        )
        .await;

        let mut shell_socket = harness.connect().await;
        let mut thread_socket = harness.connect().await;
        rpc_request(
            &mut shell_socket,
            "101",
            "orchestration.subscribeShell",
            json!({}),
        )
        .await;
        rpc_request(
            &mut thread_socket,
            "102",
            "orchestration.subscribeThread",
            json!({ "threadId": "thread-watched" }),
        )
        .await;

        let shell_snapshot = expect_snapshot_chunk(&mut shell_socket, "101").await;
        assert_eq!(shell_snapshot["projects"].as_array().unwrap().len(), 1);
        assert_eq!(shell_snapshot["threads"].as_array().unwrap().len(), 2);
        acknowledge(&mut shell_socket, "101").await;

        let thread_snapshot = expect_snapshot_chunk(&mut thread_socket, "102").await;
        assert_eq!(thread_snapshot["thread"]["id"], json!("thread-watched"));
        assert_eq!(thread_snapshot["thread"]["title"], json!("Watched thread"));
        acknowledge(&mut thread_socket, "102").await;

        dispatch_command(
            &mut command_socket,
            "3",
            create_public_thread("thread-unrelated", "project-streams", "Unrelated thread"),
        )
        .await;
        let shell_after_unrelated = expect_snapshot_chunk(&mut shell_socket, "101").await;
        assert!(
            shell_after_unrelated["threads"]
                .as_array()
                .unwrap()
                .iter()
                .any(|thread| thread["id"] == json!("thread-unrelated"))
        );
        acknowledge(&mut shell_socket, "101").await;

        dispatch_command(
            &mut command_socket,
            "4",
            json!({
                "type": "thread.meta.update",
                "commandId": "rename-watched",
                "threadId": "thread-watched",
                "title": "Watched thread updated",
                "branch": "feature/watched",
            }),
        )
        .await;

        let shell_after_relevant = expect_snapshot_chunk(&mut shell_socket, "101").await;
        assert!(
            shell_after_relevant["threads"]
                .as_array()
                .unwrap()
                .iter()
                .any(|thread| {
                    thread["id"] == json!("thread-watched")
                        && thread["title"] == json!("Watched thread updated")
                })
        );
        acknowledge(&mut shell_socket, "101").await;

        let thread_after_relevant = expect_snapshot_chunk(&mut thread_socket, "102").await;
        assert_eq!(
            thread_after_relevant["thread"]["title"],
            json!("Watched thread updated")
        );
        acknowledge(&mut thread_socket, "102").await;

        interrupt_request(&mut thread_socket, "102").await;
        expect_interrupt_exit(&mut thread_socket, "102").await;

        interrupt_request(&mut shell_socket, "101").await;
        expect_interrupt_exit(&mut shell_socket, "101").await;

        shell_socket.close(None).await.expect("close shell socket");
        thread_socket
            .close(None)
            .await
            .expect("close thread socket");
        command_socket
            .close(None)
            .await
            .expect("close command socket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn replay_events_pages_are_byte_bounded_and_match_unpaged_replay() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let repositories = harness.engine.repositories();
        let text = "replay text ".repeat(36);
        for index in 0..3_000 {
            repositories
                .append_event(NewOrchestrationEvent {
                    event_id: format!("replay-page-event-{index}"),
                    event_type: "thread.message-sent".to_owned(),
                    aggregate_kind: "thread".to_owned(),
                    aggregate_id: "replay-thread".to_owned(),
                    occurred_at: CREATED_AT.to_owned(),
                    command_id: None,
                    causation_event_id: None,
                    correlation_id: None,
                    payload: json!({
                        "threadId": "replay-thread",
                        "messageId": format!("replay-message-{index}"),
                        "role": "assistant",
                        "text": text,
                        "turnId": null,
                        "streaming": false,
                        "createdAt": CREATED_AT,
                        "updatedAt": CREATED_AT,
                    }),
                    metadata: json!({}),
                })
                .await
                .expect("append replay event");
        }

        // This harness uses whole frames without a connection message limit;
        // the unpaged ceiling is covered with smaller budgets in the unit tests.
        let mut socket = harness.connect().await;
        let mut events = Vec::new();
        let mut cursor = 0;
        let mut page_count = 0;
        loop {
            page_count += 1;
            assert!(page_count <= 3_000, "every page must advance the cursor");
            let page = unary_success(
                &mut socket,
                &page_count.to_string(),
                "orchestration.replayEvents",
                json!({ "fromSequenceExclusive": cursor, "paged": true }),
            )
            .await;
            let page_events = page["events"].as_array().expect("paged events array");
            let exhausted = page["exhausted"].as_bool().expect("exhausted flag");
            assert!(!page_events.is_empty());
            let page_bytes = serde_json::to_vec(page_events).expect("encode page").len();
            assert!(page_bytes <= 1024 * 1024 || page_events.len() == 1);
            for event in page_events {
                let sequence = event["sequence"].as_i64().expect("event sequence");
                assert!(
                    sequence > cursor,
                    "sequences increase within and across pages"
                );
                cursor = sequence;
            }
            events.extend(page_events.iter().cloned());
            assert_eq!(exhausted, events.len() == 3_000);
            if exhausted {
                break;
            }
        }
        assert!(page_count >= 3);
        assert!(serde_json::to_vec(&events).expect("encode replay").len() >= 5 * 1024 * 1024 / 2);
        let unpaged = unary_success(
            &mut socket,
            "4000",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": 0 }),
        )
        .await;
        assert_eq!(Value::Array(events), unpaged);
        let tail = unary_success(
            &mut socket,
            "4001",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": cursor, "paged": true }),
        )
        .await;
        assert_eq!(tail, json!({ "events": [], "exhausted": true }));
        let explicitly_unpaged = unary_success(
            &mut socket,
            "4002",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": 0, "paged": false }),
        )
        .await;
        assert_eq!(explicitly_unpaged, unpaged);
        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn negative_replay_sequence_is_rejected_instead_of_replaying_from_the_start() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;

        // The contract declares `fromSequenceExclusive` a non-negative integer.
        rpc_request(
            &mut socket,
            "1",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": -1 }),
        )
        .await;
        let replay_error = expect_failure(&mut socket, "1").await;
        assert_invalid_request(
            &replay_error,
            "orchestration.replayEvents",
            "fromSequenceExclusive must be a non-negative integer",
        );
        rpc_request(
            &mut socket,
            "2",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": -1, "paged": true }),
        )
        .await;
        let paged_error = expect_failure(&mut socket, "2").await;
        assert_invalid_request(
            &paged_error,
            "orchestration.replayEvents",
            "fromSequenceExclusive must be a non-negative integer",
        );
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

#[tokio::test]
async fn malformed_and_missing_orchestration_requests_return_typed_failures() {
    let harness = harness().await;
    let outcome = AssertUnwindSafe(async {
        let mut socket = harness.connect().await;

        rpc_request(
            &mut socket,
            "1",
            "orchestration.dispatchCommand",
            json!({
                "type": "thread.create",
                "commandId": "bad-command",
            }),
        )
        .await;
        let dispatch_error = expect_failure(&mut socket, "1").await;
        assert_invalid_request(
            &dispatch_error,
            "orchestration.dispatchCommand",
            "missing field",
        );

        rpc_request(
            &mut socket,
            "2",
            "orchestration.replayEvents",
            json!({ "fromSequenceExclusive": "oops" }),
        )
        .await;
        let replay_error = expect_failure(&mut socket, "2").await;
        assert_invalid_request(&replay_error, "orchestration.replayEvents", "invalid type");

        rpc_request(
            &mut socket,
            "3",
            "orchestration.getTurnDiff",
            json!({
                "threadId": "thread-1",
                "fromTurnCount": 3,
                "toTurnCount": 1,
            }),
        )
        .await;
        let diff_error = expect_failure(&mut socket, "3").await;
        assert_invalid_request(&diff_error, "orchestration.diff", "ordered");

        rpc_request(
            &mut socket,
            "4",
            "orchestration.dispatchCommand",
            json!({
                "type": "thread.archive",
                "commandId": "archive-missing",
                "threadId": "missing-thread",
            }),
        )
        .await;
        let missing_thread = expect_failure(&mut socket, "4").await;
        assert_eq!(
            missing_thread["_tag"],
            json!("OrchestrationDispatchCommandError")
        );
        assert!(
            missing_thread["message"]
                .as_str()
                .unwrap()
                .contains("missing-thread")
        );

        rpc_request(
            &mut socket,
            "5",
            "orchestration.subscribeThread",
            json!({ "threadId": 7 }),
        )
        .await;
        let malformed_stream = expect_failure(&mut socket, "5").await;
        assert_invalid_request(
            &malformed_stream,
            "orchestration.subscribeThread",
            "invalid type",
        );

        rpc_request(
            &mut socket,
            "6",
            "orchestration.subscribeThread",
            json!({ "threadId": "missing-thread" }),
        )
        .await;
        let missing_snapshot = expect_failure(&mut socket, "6").await;
        assert_eq!(
            missing_snapshot["_tag"],
            json!("OrchestrationGetSnapshotError")
        );
        assert!(
            missing_snapshot["message"]
                .as_str()
                .unwrap()
                .contains("missing-thread")
        );

        socket.close(None).await.expect("close WebSocket");
    })
    .catch_unwind()
    .await;
    finish_test(harness, outcome).await;
}

fn assert_invalid_request(error: &Value, method: &str, message_fragment: &str) {
    assert_eq!(error["_tag"], json!("InvalidRequest"));
    assert_eq!(error["method"], json!(method));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains(message_fragment),
        "expected {:?} to contain {:?}",
        error["message"],
        message_fragment
    );
}

async fn dispatch_command(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
    payload: Value,
) -> Value {
    unary_success(socket, request_id, "orchestration.dispatchCommand", payload).await
}

async fn dispatch_real_git_command(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
    payload: Value,
) -> Value {
    rpc_request(socket, request_id, "orchestration.dispatchCommand", payload).await;
    expect_success_with_deadline(socket, request_id, REAL_GIT_RPC_RESPONSE_DEADLINE).await
}

async fn unary_success(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
    tag: &str,
    payload: Value,
) -> Value {
    rpc_request(socket, request_id, tag, payload).await;
    expect_success(socket, request_id).await
}

async fn rpc_request(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
    tag: &str,
    payload: Value,
) {
    send_json(
        socket,
        json!({
            "_tag": "Request",
            "id": request_id,
            "tag": tag,
            "payload": payload,
            "headers": [],
        }),
    )
    .await;
}

async fn acknowledge(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) {
    send_json(
        socket,
        json!({
            "_tag": "Ack",
            "requestId": request_id,
        }),
    )
    .await;
}

async fn interrupt_request(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) {
    send_json(
        socket,
        json!({
            "_tag": "Interrupt",
            "requestId": request_id,
        }),
    )
    .await;
}

async fn expect_success(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) -> Value {
    expect_success_with_deadline(socket, request_id, RPC_RESPONSE_DEADLINE).await
}

async fn expect_success_with_deadline(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
    deadline: Duration,
) -> Value {
    let message = next_server_message_with_deadline(socket, deadline).await;
    match message {
        ServerMessage::Exit {
            request_id: actual_request_id,
            exit: RpcExit::Success { value },
        } => {
            assert_eq!(actual_request_id.as_str(), request_id);
            value.unwrap_or(Value::Null)
        }
        other => panic!("expected success exit for {request_id}, got {other:?}"),
    }
}

async fn expect_failure(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) -> Value {
    expect_failure_with_deadline(socket, request_id, RPC_RESPONSE_DEADLINE).await
}

async fn expect_failure_with_deadline(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
    deadline: Duration,
) -> Value {
    let message = next_server_message_with_deadline(socket, deadline).await;
    match message {
        ServerMessage::Exit {
            request_id: actual_request_id,
            exit: RpcExit::Failure { cause },
        } => {
            assert_eq!(actual_request_id.as_str(), request_id);
            assert_eq!(cause.len(), 1);
            match &cause[0] {
                coddy_studio_server::CauseItem::Fail { error } => error.clone(),
                other => panic!("expected fail cause for {request_id}, got {other:?}"),
            }
        }
        other => panic!("expected failure exit for {request_id}, got {other:?}"),
    }
}

async fn expect_interrupt_exit(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) {
    let message = next_server_message(socket).await;
    match message {
        ServerMessage::Exit {
            request_id: actual_request_id,
            exit: RpcExit::Failure { cause },
        } => {
            assert_eq!(actual_request_id.as_str(), request_id);
            assert_eq!(
                cause,
                vec![coddy_studio_server::CauseItem::Interrupt { fiber_id: None }]
            );
        }
        other => panic!("expected interrupt exit for {request_id}, got {other:?}"),
    }
}

async fn expect_snapshot_chunk(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) -> Value {
    let values = expect_chunk(socket, request_id).await;
    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["kind"], json!("snapshot"));
    values[0]["snapshot"].clone()
}

async fn expect_chunk(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    request_id: &str,
) -> Vec<Value> {
    let message = next_server_message(socket).await;
    match message {
        ServerMessage::Chunk {
            request_id: actual_request_id,
            values,
        } => {
            assert_eq!(actual_request_id.as_str(), request_id);
            values
        }
        other => panic!("expected stream chunk for {request_id}, got {other:?}"),
    }
}

async fn send_json(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    value: Value,
) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send WebSocket message");
}

async fn next_server_message(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) -> ServerMessage {
    next_server_message_with_deadline(socket, RPC_RESPONSE_DEADLINE).await
}

async fn next_server_message_with_deadline(
    socket: &mut WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    deadline: Duration,
) -> ServerMessage {
    let message = timeout(deadline, next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = message else {
        panic!("expected text WebSocket message, got {message:?}");
    };
    serde_json::from_str(&text).expect("valid server RPC message")
}

#[allow(dead_code)]
fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts/fixtures/rpc-wire")
}
