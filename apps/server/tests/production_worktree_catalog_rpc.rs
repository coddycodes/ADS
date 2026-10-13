use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Condvar, Mutex as StdMutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bibcode_server::{
    CauseItem, RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime,
    git::{
        GitCommandError, GitPrunableWorktree, GitRepository, GitWorktreeInventory,
        GitWorktreeRecord, GitWorktreeRemovalInspection, StatusBroadcaster, VcsStatusStreamEvent,
        host_path_platform, normalize_worktree_path_key,
    },
    orchestration::{
        EngineOptions, OrchestrationCommand, OrchestrationEngine, canonical_command_digest,
        engine::TestHooks,
    },
    persistence::{
        CommandReceipt, Database, OrchestrationEvent, ProjectionProject, ProjectionThread,
        Repositories, run_migrations,
    },
    production::git_vcs::{GitVcsRpcServices, register_git_vcs_rpc},
    production::orchestration_rpc::{register_orchestration_rpc, shell_snapshot},
    production::worktree_catalog_rpc::{
        WorktreeCatalogOperationRuntime, WorktreeCatalogRpcServices,
        WorktreeRemovalCleanupAdmission, WorktreeRemovalCleanupAdmissionError,
        WorktreeRemovalCleanupAdmissionFuture, WorktreeRemovalGit, WorktreeRemovalGitFuture,
        WorktreeRemovalQuiesceFuture, WorktreeRemovalQuiesceLease, WorktreeRemovalQuiesceRequest,
        WorktreeRemovalQuiescer, compact_eligible_baseline, register_worktree_catalog_rpc,
    },
    worktree_catalog::{
        CatalogScanStatus, CatalogSubscription, WorkspaceAvailabilityRegistry,
        WorktreeAdoptionState, WorktreeCatalogService, WorktreeCatalogSnapshot, WorktreeDescriptor,
        WorktreeDirectoryState, WorktreeRegistrationState,
    },
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    time::timeout,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const PURE_RPC_RESPONSE_DEADLINE: Duration = Duration::from_secs(10);
const REAL_GIT_RPC_RESPONSE_DEADLINE: Duration = Duration::from_secs(30);
const ENGINE_HANDOFF_DEADLOCK_BOUND: Duration = Duration::from_secs(30);
const MANAGED_WORKTREE_ROLLBACK_INTEGRATION_DEADLINE: Duration = Duration::from_secs(60);
const WORKTREE_REMOVAL_INTEGRATION_DEADLINE: Duration = Duration::from_secs(60);

#[tokio::test]
async fn dedicated_worktree_creation_rejects_client_selected_filesystem_authority() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let client_selected = fixture.root.path().join("client-selected");
    let main = fixture.main.clone();

    request(
        fixture.socket(),
        "9900",
        "worktree.createManaged",
        json!({
            "commandId":"managed-authority-command",
            "projectId":"project-1",
            "threadId":"managed-authority-thread",
            "title":"Managed authority",
            "refName":"main",
            "newRefName":"feature/client-selected",
            "baseRefName":"main",
            "cwd":main,
            "path":client_selected,
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;

    let message = next_pure_server_message(fixture.socket()).await;
    let ServerMessage::Exit {
        request_id,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected raw filesystem authority to be rejected: {message:?}");
    };
    assert_eq!(request_id.as_str(), "9900");
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error } if error["_tag"] == "RpcRequestInvalid"
    )));
    assert!(!client_selected.exists());
    assert!(
        fixture
            .repositories
            .get_thread("managed-authority-thread".to_owned())
            .await
            .expect("thread read")
            .is_none()
    );
    assert!(
        !git_output(
            &fixture.main,
            &["branch", "--list", "feature/client-selected"]
        )
        .contains("feature/client-selected")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn dedicated_create_panel_and_retarget_resolve_workspace_authority_server_side() {
    let hooks = TestHooks::default();
    let mut fixture = CatalogRpcFixture::new_with_removal_services_and_options(
        false,
        Arc::new(TestNoopQuiescer),
        None,
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let mut catalog_subscription = fixture
        .catalog
        .subscribe("project-1")
        .await
        .expect("catalog subscription");
    let initial_catalog = catalog_subscription.initial_latest();
    request(
        fixture.socket(),
        "9901",
        "worktree.createManaged",
        json!({
            "commandId":"managed-create-command",
            "projectId":"project-1",
            "threadId":"managed-thread",
            "title":"Managed workspace",
            "refName":"main",
            "newRefName":"feature/managed-create",
            "baseRefName":"main",
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    let managed = success_value(fixture.socket(), "9901").await;
    let managed_path = managed["path"].as_str().expect("managed path").to_owned();
    assert!(Path::new(&managed_path).is_dir());
    assert_eq!(managed["refName"], "feature/managed-create");
    let owner = fixture
        .repositories
        .get_thread("managed-thread".to_owned())
        .await
        .expect("managed owner read")
        .expect("managed owner exists");
    assert_eq!(owner.kind, "workspace");
    assert_eq!(owner.worktree_path.as_deref(), Some(managed_path.as_str()));

    request(
        fixture.socket(),
        "9902",
        "worktree.createPanel",
        json!({
            "commandId":"managed-panel-command",
            "hostThreadId":"managed-thread",
            "threadId":"managed-panel",
            "title":"Managed panel",
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    assert_eq!(
        success_value(fixture.socket(), "9902").await["threadId"],
        "managed-panel"
    );
    let panel = fixture
        .repositories
        .get_thread("managed-panel".to_owned())
        .await
        .expect("panel read")
        .expect("panel exists");
    assert_eq!(panel.kind, "panel");
    assert_eq!(panel.project_id, "project-1");
    assert_eq!(panel.worktree_path.as_deref(), Some(managed_path.as_str()));
    assert_eq!(panel.branch.as_deref(), Some("feature/managed-create"));
    assert_eq!(panel.host_thread_id.as_deref(), Some("managed-thread"));
    let shell = shell_snapshot(&fixture.engine, false)
        .await
        .expect("shell snapshot");
    let shell_thread = |id: &str| {
        shell["threads"]
            .as_array()
            .expect("shell threads")
            .iter()
            .find(|thread| thread["id"] == id)
            .cloned()
            .expect("shell thread")
    };
    assert_eq!(
        shell_thread("managed-panel")["hostThreadId"],
        "managed-thread"
    );
    assert!(shell_thread("managed-thread").get("hostThreadId").is_none());

    // The explicit refresh below is intentionally allowed to coalesce with an
    // in-flight catalog scan. Settle the managed-create invalidation first so
    // the external worktree is not created behind an already-running scan.
    let _managed_catalog =
        wait_for_catalog_generation(&mut catalog_subscription, initial_catalog.generation).await;
    let target =
        fixture.create_named_external_worktree("retarget-target", "feature/retarget-target");
    request(
        fixture.socket(),
        "9903",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "9903").await;
    let descriptor = snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|descriptor| {
            descriptor["path"]
                .as_str()
                .is_some_and(|path| same_worktree_identity(Path::new(path), &target))
        })
        .expect("retarget descriptor");
    let target_path = descriptor["path"]
        .as_str()
        .expect("retarget descriptor path")
        .to_owned();
    let mut old_status = fixture
        .status_broadcaster
        .subscribe(PathBuf::from(&managed_path), CancellationToken::new())
        .await
        .expect("old worktree status subscription");
    let mut new_status = fixture
        .status_broadcaster
        .subscribe(PathBuf::from(&target_path), CancellationToken::new())
        .await
        .expect("new worktree status subscription");
    assert!(matches!(
        old_status.recv().await,
        Some(VcsStatusStreamEvent::Snapshot { .. })
    ));
    assert!(matches!(
        new_status.recv().await,
        Some(VcsStatusStreamEvent::Snapshot { .. })
    ));
    fs::write(
        Path::new(&managed_path).join("README.md"),
        "old path changed\n",
    )
    .expect("dirty old worktree");
    fs::write(
        Path::new(&target_path).join("README.md"),
        "new path changed\n",
    )
    .expect("dirty new worktree");
    let retarget_payload = json!({
        "commandId":"managed-retarget-command",
        "projectId":"project-1",
        "threadId":"managed-thread",
        "worktreeKey":descriptor["worktreeKey"],
        "expectedGeneration":snapshot["generation"]
    });
    let pause = hooks.pause_before_next_command_persist();
    request(
        fixture.socket(),
        "9904",
        "worktree.retarget",
        retarget_payload.clone(),
    )
    .await;
    timeout(ENGINE_HANDOFF_DEADLOCK_BOUND, pause.wait_until_entered())
        .await
        .expect("retarget reaches its pre-persistence boundary");
    assert_eq!(
        catalog_subscription.latest().generation,
        snapshot["generation"].as_u64().expect("seed generation"),
        "retarget admission must not invalidate the catalog before its durable receipt"
    );
    assert!(
        !fixture
            .repositories
            .get_command_receipt("managed-retarget-command".to_owned())
            .await
            .expect("retarget receipt read")
            .is_some_and(|receipt| receipt.status == "accepted")
    );
    pause.release();
    assert_eq!(
        success_value(fixture.socket(), "9904").await["threadId"],
        "managed-thread"
    );
    let retargeted_catalog = wait_for_catalog_generation(
        &mut catalog_subscription,
        snapshot["generation"].as_u64().expect("seed generation"),
    )
    .await;
    let retarget_receipt = fixture
        .repositories
        .get_command_receipt("managed-retarget-command".to_owned())
        .await
        .expect("retarget receipt read")
        .expect("accepted retarget receipt");
    assert_eq!(retarget_receipt.status, "accepted");
    assert_eq!(
        retarget_receipt.payload_digest.as_deref(),
        Some(
            canonical_command_digest(&retarget_payload)
                .expect("retarget digest")
                .as_str()
        )
    );
    // The accepted receipt does not mean the owner row already carries the
    // retargeted checkout: a generation bump can arrive before the healthy
    // snapshot observer writes the branch, and a Focus refresh may reuse a
    // retained fingerprinted snapshot. Force one real refresh and await its
    // observer instead of polling background reconciliation on a timer.
    request(
        fixture.socket(),
        "9905",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let reconciled = success_value(fixture.socket(), "9905").await;
    assert!(
        reconciled["generation"]
            .as_u64()
            .expect("reconciled generation")
            >= retargeted_catalog.generation
    );
    let retargeted = fixture
        .repositories
        .get_thread("managed-thread".to_owned())
        .await
        .expect("retargeted owner read")
        .expect("retargeted owner exists");
    assert_eq!(
        retargeted.worktree_path.as_deref(),
        Some(target_path.as_str())
    );
    assert_eq!(
        retargeted.branch.as_deref(),
        Some("feature/retarget-target")
    );
    request(
        fixture.socket(),
        "9906",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1", "reason":"focus"}),
    )
    .await;
    let focused = success_value(fixture.socket(), "9906").await;
    assert!(
        focused["generation"].as_u64().expect("focus generation")
            >= reconciled["generation"]
                .as_u64()
                .expect("reconciled generation")
    );
    let focused_worktrees = focused["worktrees"].as_array().expect("focused worktrees");
    let focused_managed = focused_worktrees
        .iter()
        .find(|worktree| {
            worktree["path"].as_str().is_some_and(|path| {
                same_worktree_identity(Path::new(path), Path::new(&managed_path))
            })
        })
        .expect("managed checkout after immediate Focus");
    let focused_target = focused_worktrees
        .iter()
        .find(|worktree| {
            worktree["path"].as_str().is_some_and(|path| {
                same_worktree_identity(Path::new(path), Path::new(&target_path))
            })
        })
        .expect("retargeted checkout after immediate Focus");
    assert_eq!(focused_managed["eligibleForAdoption"], false);
    assert_eq!(focused_target["eligibleForAdoption"], false);
    timeout(Duration::from_secs(15), async {
        let old = async {
            loop {
                if matches!(
                    old_status.recv().await,
                    Some(VcsStatusStreamEvent::LocalUpdated { local })
                        if local.has_working_tree_changes
                ) {
                    break;
                }
            }
        };
        let new = async {
            loop {
                if matches!(
                    new_status.recv().await,
                    Some(VcsStatusStreamEvent::LocalUpdated { local })
                        if local.has_working_tree_changes
                ) {
                    break;
                }
            }
        };
        tokio::join!(old, new);
    })
    .await
    .expect("retarget notifies old and new status owners");
    drop(catalog_subscription);
    fixture.shutdown().await;
}

#[tokio::test]
async fn managed_creation_rolls_back_exact_server_owned_worktree_when_projection_rejects() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let existing_thread_id = fixture
        .repositories
        .list_threads_by_project("project-1".to_owned())
        .await
        .expect("project threads")
        .into_iter()
        .find(|thread| thread.deleted_at.is_none())
        .expect("project default thread")
        .thread_id;
    let before = git_output(&fixture.main, &["worktree", "list", "--porcelain"]);

    request(
        fixture.socket(),
        "9905",
        "worktree.createManaged",
        json!({
            "commandId":"managed-rollback-command",
            "projectId":"project-1",
            "threadId":existing_thread_id,
            "title":"Rejected duplicate",
            "refName":"main",
            "newRefName":"feature/managed-rollback",
            "baseRefName":"main",
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    let message = next_managed_worktree_rollback_message(fixture.socket()).await;
    let ServerMessage::Exit {
        request_id,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected duplicate owner projection to fail: {message:?}");
    };
    assert_eq!(request_id.as_str(), "9905");
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error }
            if error["_tag"] == "WorktreeAdoptionError"
                && error["reason"] == "orchestration-failed"
    )));
    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"]),
        before,
        "rollback must leave exactly the pre-existing registrations"
    );
    assert!(
        git_output(
            &fixture.main,
            &["branch", "--list", "feature/managed-rollback"]
        )
        .trim()
        .is_empty(),
        "rollback must remove only the newly-created branch"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn managed_creation_rolls_back_the_exact_automatically_suffixed_branch() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let existing_thread_id = fixture
        .repositories
        .list_threads_by_project("project-1".to_owned())
        .await
        .expect("project threads")
        .into_iter()
        .find(|thread| thread.deleted_at.is_none())
        .expect("project default thread")
        .thread_id;
    let before = git_output(&fixture.main, &["worktree", "list", "--porcelain"]);

    request(
        fixture.socket(),
        "9906",
        "worktree.createManaged",
        json!({
            "commandId":"managed-suffixed-rollback-command",
            "projectId":"project-1",
            "threadId":existing_thread_id,
            "title":"Rejected duplicate suffix",
            "refName":"main",
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    let message = next_managed_worktree_rollback_message(fixture.socket()).await;
    let ServerMessage::Exit {
        request_id,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected duplicate owner projection to fail: {message:?}");
    };
    assert_eq!(request_id.as_str(), "9906");
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error }
            if error["_tag"] == "WorktreeAdoptionError"
                && error["reason"] == "orchestration-failed"
    )));
    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"]),
        before,
        "rollback must leave exactly the pre-existing registrations"
    );
    assert!(
        git_output(&fixture.main, &["branch", "--list", "main-2"])
            .trim()
            .is_empty(),
        "rollback must delete the exact automatically-created suffix branch"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn pull_request_branch_creation_persists_the_exact_worktree_owner_atomically() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    git(&fixture.main, &["branch", "feature/pull-request"], None);

    request(
        fixture.socket(),
        "9907",
        "worktree.createManaged",
        json!({
            "commandId":"pull-request-managed-owner-command",
            "projectId":"project-1",
            "threadId":"pull-request-managed-thread",
            "title":"feature/pull-request",
            "refName":"feature/pull-request",
            "newRefName":null,
            "baseRefName":null,
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default"
            }
        }),
    )
    .await;
    let created = success_value(fixture.socket(), "9907").await;
    assert_eq!(created["threadId"], "pull-request-managed-thread");
    assert_eq!(created["refName"], "feature/pull-request");
    let created_path = created["path"].as_str().expect("managed path");
    assert!(Path::new(created_path).is_dir());

    let owner = fixture
        .repositories
        .get_thread("pull-request-managed-thread".to_owned())
        .await
        .expect("owner read")
        .expect("owner exists");
    assert_eq!(owner.kind, "workspace");
    assert_eq!(owner.branch.as_deref(), Some("feature/pull-request"));
    assert_eq!(owner.worktree_path.as_deref(), Some(created_path));
    fixture.shutdown().await;
}

#[tokio::test]
async fn stream_delivers_initial_and_latest_snapshots_refreshes_and_cancels() {
    let mut fixture = CatalogRpcFixture::new(false).await;

    request(
        fixture.socket(),
        "1",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let initial = next_chunk(fixture.socket(), "1").await;
    let initial_generation = initial["generation"].as_u64().expect("initial generation");
    assert_eq!(initial["authoritative"], true);
    assert_eq!(initial["worktrees"].as_array().expect("worktrees").len(), 1);
    ack(fixture.socket(), "1").await;
    assert!(
        timeout(
            Duration::from_millis(100),
            next_frame_past_heartbeat(fixture.socket())
        )
        .await
        .is_err(),
        "the initial latest value must not be emitted twice"
    );

    fixture.create_external_worktree();
    request(
        fixture.socket(),
        "2",
        "vcs.refreshWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;

    let mut refreshed = None;
    let mut refresh_result = None;
    while refreshed.is_none() || refresh_result.is_none() {
        match next_server_message(fixture.socket()).await {
            ServerMessage::Chunk { request_id, values } if request_id.as_str() == "1" => {
                let value = values.into_iter().next().expect("catalog value");
                ack(fixture.socket(), "1").await;
                if value["authoritative"] == true
                    && value["generation"].as_u64().unwrap_or(0) > initial_generation
                {
                    refreshed = Some(value);
                }
            }
            ServerMessage::Exit {
                request_id,
                exit: RpcExit::Success { value: Some(value) },
            } if request_id.as_str() == "2" => refresh_result = Some(value),
            other => panic!("unexpected catalog refresh message: {other:?}"),
        }
    }
    let refreshed = refreshed.expect("latest replacement");
    let refresh_result = refresh_result.expect("explicit refresh result");
    assert_eq!(refreshed, refresh_result);
    assert_eq!(
        refreshed["worktrees"].as_array().expect("worktrees").len(),
        2
    );

    send_json(
        fixture.socket(),
        json!({ "_tag": "Interrupt", "requestId": "1" }),
    )
    .await;
    let exit = next_server_message(fixture.socket()).await;
    assert!(matches!(
        exit,
        ServerMessage::Exit { request_id, .. } if request_id.as_str() == "1"
    ));

    request(
        fixture.socket(),
        "3",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let resubscribed = next_chunk(fixture.socket(), "3").await;
    assert!(
        resubscribed["generation"].as_u64().expect("generation")
            > refreshed["generation"].as_u64().expect("generation"),
        "cancelling the stream must release the catalog subscription so the next subscriber performs a first-subscriber refresh"
    );
    send_json(
        fixture.socket(),
        json!({ "_tag": "Interrupt", "requestId": "3" }),
    )
    .await;
    let _exit = next_server_message(fixture.socket()).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn project_missing_is_a_typed_stream_and_refresh_failure() {
    let mut fixture = CatalogRpcFixture::new(false).await;

    request(
        fixture.socket(),
        "10",
        "vcs.refreshWorktreeCatalog",
        json!({ "projectId": "missing" }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "10", "project-not-found").await;

    request(
        fixture.socket(),
        "11",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "missing" }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "11", "project-not-found").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn refresh_accepts_legacy_omission_and_supported_reasons() {
    let mut fixture = CatalogRpcFixture::new(false).await;

    for (request_id, payload) in [
        ("561", json!({"projectId":"project-1"})),
        ("562", json!({"projectId":"project-1","reason":"explicit"})),
        ("563", json!({"projectId":"project-1","reason":"focus"})),
    ] {
        request(
            fixture.socket(),
            request_id,
            "vcs.refreshWorktreeCatalog",
            payload,
        )
        .await;
        assert_eq!(
            success_value(fixture.socket(), request_id).await["authoritative"],
            true
        );
    }

    fixture.shutdown().await;
}

#[tokio::test]
async fn refresh_rejects_an_unknown_reason_as_an_invalid_request() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    request(
        fixture.socket(),
        "564",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1","reason":"scheduled"}),
    )
    .await;

    let message = next_pure_server_message(fixture.socket()).await;
    let ServerMessage::Exit {
        request_id,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected invalid refresh request: {message:?}");
    };
    assert_eq!(request_id.as_str(), "564");
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error }
            if error["_tag"] == "RpcRequestInvalid"
                && error["method"] == "vcs.refreshWorktreeCatalog"
    )));

    fixture.shutdown().await;
}

#[tokio::test]
async fn stream_replaces_ack_lagged_updates_with_the_latest_catalog_generation() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    request(
        fixture.socket(),
        "12",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let initial = next_chunk(fixture.socket(), "12").await;
    let initial_generation = initial["generation"].as_u64().expect("generation");

    fixture.create_named_external_worktree("lag-one", "feature/lag-one");
    request(
        fixture.socket(),
        "13",
        "vcs.refreshWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let _first_refresh = success_value(fixture.socket(), "13").await;
    fixture.create_named_external_worktree("lag-two", "feature/lag-two");
    request(
        fixture.socket(),
        "14",
        "vcs.refreshWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let latest_refresh = success_value(fixture.socket(), "14").await;

    ack(fixture.socket(), "12").await;
    let delivered = next_chunk(fixture.socket(), "12").await;
    assert_eq!(delivered, latest_refresh);
    assert_eq!(
        delivered["generation"].as_u64().expect("generation"),
        initial_generation + 2
    );
    assert_eq!(
        delivered["worktrees"].as_array().expect("worktrees").len(),
        3
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn interrupt_during_catalog_subscribe_bootstrap_exits_without_a_snapshot() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    send_json(
        fixture.socket(),
        json!([
            {
                "_tag": "Request",
                "id": "15",
                "tag": "subscribeWorktreeCatalog",
                "payload": { "projectId": "project-1" },
                "headers": []
            },
            { "_tag": "Interrupt", "requestId": "15" }
        ]),
    )
    .await;

    let message = next_server_message(fixture.socket()).await;
    let ServerMessage::Exit {
        request_id,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected interrupt exit");
    };
    assert_eq!(request_id.as_str(), "15");
    assert!(
        cause
            .iter()
            .any(|item| matches!(item, CauseItem::Interrupt { .. }))
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            next_frame_past_heartbeat(fixture.socket())
        )
        .await
        .is_err(),
        "cancelled bootstrap must not publish a late snapshot"
    );
    assert!(
        fixture.catalog.latest("project-1").await.is_none(),
        "cancelled bootstrap must not leave a catalog entry behind"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn policy_rejects_stale_acknowledgement_and_persists_hidden_and_shown_controls() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "20",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let snapshot = next_chunk(fixture.socket(), "20").await;
    let generation = snapshot["generation"].as_u64().expect("generation");
    let candidate_path = snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["eligibleForAdoption"] == true)
        .and_then(|worktree| worktree["path"].as_str())
        .expect("eligible path")
        .to_owned();
    ack(fixture.socket(), "20").await;

    request(
        fixture.socket(),
        "21",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId": "policy-stale",
            "projectId": "project-1",
            "acknowledgeGeneration": generation.saturating_sub(1)
        }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "21", "stale-generation").await;

    request(
        fixture.socket(),
        "22",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId": "policy-shown",
            "projectId": "project-1",
            "visibility": "shown",
            "acknowledgeGeneration": generation,
            "dismissInitialPrompt": true
        }),
    )
    .await;
    let shown = success_value(fixture.socket(), "22").await;
    assert_eq!(shown["visibility"], "shown");
    assert!(shown["initialPromptDismissedAt"].as_str().is_some());
    assert_eq!(shown["baselinePaths"], json!([candidate_path]));

    request(
        fixture.socket(),
        "23",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId": "policy-hidden",
            "projectId": "project-1",
            "visibility": "hidden",
            "dismissInitialPrompt": false
        }),
    )
    .await;
    let hidden = success_value(fixture.socket(), "23").await;
    assert_eq!(hidden["visibility"], "hidden");
    assert_eq!(hidden["initialPromptDismissedAt"], Value::Null);
    assert_eq!(hidden["baselinePaths"], shown["baselinePaths"]);

    let persisted = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists");
    assert_eq!(persisted.worktree_discovery, hidden);
    fixture.shutdown().await;
}

/// A slow client still holds generation G (its stream ack has not round-tripped) when an
/// unrelated refresh publishes G+1 with the same candidates. Hiding must acknowledge what the
/// user saw instead of failing with "Catalog generation G is no longer authoritative".
#[tokio::test]
async fn policy_acknowledges_a_generation_whose_candidates_are_unchanged() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "30",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let seen = next_chunk(fixture.socket(), "30").await;
    let seen_generation = seen["generation"].as_u64().expect("generation");
    let candidate_path = eligible_candidate(&seen)["path"]
        .as_str()
        .expect("eligible path")
        .to_owned();

    request(
        fixture.socket(),
        "31",
        "vcs.refreshWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let refreshed = success_value(fixture.socket(), "31").await;
    assert_eq!(refreshed["generation"].as_u64(), Some(seen_generation + 1));
    assert_eq!(refreshed["worktrees"], seen["worktrees"]);

    request(
        fixture.socket(),
        "32",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId": "policy-unchanged-candidates",
            "projectId": "project-1",
            "acknowledgeGeneration": seen_generation,
            "dismissInitialPrompt": true
        }),
    )
    .await;
    let hidden = success_value(fixture.socket(), "32").await;
    assert_eq!(hidden["baselinePaths"], json!([candidate_path]));
    assert!(hidden["initialPromptDismissedAt"].as_str().is_some());
    fixture.shutdown().await;
}

#[tokio::test]
async fn policy_rejects_a_generation_that_predates_a_new_candidate() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "33",
        "subscribeWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let seen = next_chunk(fixture.socket(), "33").await;
    let seen_generation = seen["generation"].as_u64().expect("generation");

    fixture.create_named_external_worktree("unseen", "feature/unseen");
    request(
        fixture.socket(),
        "34",
        "vcs.refreshWorktreeCatalog",
        json!({ "projectId": "project-1" }),
    )
    .await;
    let refreshed = success_value(fixture.socket(), "34").await;
    assert_eq!(refreshed["generation"].as_u64(), Some(seen_generation + 1));

    request(
        fixture.socket(),
        "35",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId": "policy-new-candidate",
            "projectId": "project-1",
            "acknowledgeGeneration": seen_generation,
            "dismissInitialPrompt": true
        }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "35", "stale-generation").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn concurrent_policy_controls_merge_without_losing_a_sibling_update() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    send_json(
        fixture.socket(),
        json!([
            {
                "_tag": "Request",
                "id": "30",
                "tag": "worktree.updateDiscoveryPolicy",
                "payload": {
                    "commandId": "policy-concurrent-visibility",
                    "projectId": "project-1",
                    "visibility": "shown"
                },
                "headers": []
            },
            {
                "_tag": "Request",
                "id": "31",
                "tag": "worktree.updateDiscoveryPolicy",
                "payload": {
                    "commandId": "policy-concurrent-dismiss",
                    "projectId": "project-1",
                    "dismissInitialPrompt": true
                },
                "headers": []
            }
        ]),
    )
    .await;
    let mut responses = std::collections::HashMap::new();
    for _ in 0..2 {
        let message = next_server_message(fixture.socket()).await;
        let ServerMessage::Exit {
            request_id,
            exit: RpcExit::Success { value: Some(value) },
        } = message
        else {
            panic!("expected concurrent policy success: {message:?}");
        };
        responses.insert(request_id.as_str().to_owned(), value);
    }
    let first = responses.get("30").expect("visibility response");
    let second = responses.get("31").expect("dismiss response");
    assert!(first["visibility"] == "shown" || second["visibility"] == "shown");

    let persisted = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists")
        .worktree_discovery;
    assert_eq!(persisted["visibility"], "shown");
    assert!(persisted["initialPromptDismissedAt"].as_str().is_some());
    for command_id in ["policy-concurrent-visibility", "policy-concurrent-dismiss"] {
        assert_eq!(
            fixture
                .repositories
                .get_command_receipt(command_id.to_owned())
                .await
                .expect("policy receipt read")
                .expect("accepted policy receipt")
                .status,
            "accepted",
            "different command IDs must retain normal project serialization"
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn policy_claim_precedes_project_lock_and_cannot_deadlock_removal() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "policy-first-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "32").await;
    let command_id = "policy-first-shared-command";
    let removal = removal_payload(command_id, "project-1", &thread_id, &plan);
    let address = fixture.handle.as_ref().expect("server handle").local_addr();
    let mut policy_socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("policy socket")
        .0;

    let database = fixture.repositories.database().clone();
    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("exclusive database queue observer");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new((StdMutex::new(false), Condvar::new()));
    let blocker_release = release.clone();
    let blocker_database = database.clone();
    let blocker = tokio::spawn(async move {
        blocker_database
            .call(move |_| {
                let _ = entered_tx.send(());
                let (released, changed) = blocker_release.as_ref();
                let mut released = released.lock().expect("database blocker mutex");
                while !*released {
                    released = changed
                        .wait(released)
                        .expect("database blocker mutex after wait");
                }
                Ok(())
            })
            .await
    });
    entered_rx.await.expect("database blocker enters");

    request(
        &mut policy_socket,
        "33",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":command_id,
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    timeout(Duration::from_secs(5), async {
        while database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            < 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("policy pauses on its project read while owning the mutation lock");

    request(fixture.socket(), "34", "worktree.remove", removal).await;
    let _old_order_removal_reached_database = timeout(Duration::from_millis(250), async {
        while database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    {
        let (released, changed) = release.as_ref();
        *released.lock().expect("database blocker mutex") = true;
        changed.notify_one();
    }
    blocker
        .await
        .expect("database blocker joins")
        .expect("database blocker succeeds");

    let (policy_exit, removal_exit) = timeout(Duration::from_secs(5), async {
        tokio::join!(
            next_server_message(&mut policy_socket),
            next_server_message(fixture.socket())
        )
    })
    .await
    .expect("policy and removal must complete without a command/project lock cycle");
    assert!(matches!(
        policy_exit,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    ));
    let ServerMessage::Exit {
        exit: RpcExit::Failure { cause },
        ..
    } = removal_exit
    else {
        panic!("expected removal command conflict: {removal_exit:?}");
    };
    assert!(removal_failure_has_reason(&cause, "command-conflict"));
    assert!(
        fixture.external.exists(),
        "losing removal must not mutate Git"
    );
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread exists")
            .deleted_at
            .is_none(),
        "losing removal must not detach the thread"
    );
    let receipt = fixture
        .repositories
        .get_command_receipt(command_id.to_owned())
        .await
        .expect("receipt read")
        .expect("accepted policy receipt");
    assert_eq!(receipt.status, "accepted");
    assert_eq!(
        receipt.payload_digest.as_deref(),
        Some(
            canonical_command_digest(&json!({
                "commandId":command_id,
                "projectId":"project-1",
                "visibility":"shown",
                "acknowledgeGeneration":null,
                "dismissInitialPrompt":null
            }))
            .expect("canonical policy digest")
            .as_str()
        )
    );
    drop(observer);
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_claim_survives_a_cancelled_policy_waiter_and_conflicts_the_next_waiter() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let removal_git = Arc::new(BlockingRemovalGit {
        inner: GitRepository::default(),
        entered: entered_tx,
        release: release.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_removal_git(true, removal_git).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "policy-waiter-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "35").await;
    let command_id = "removal-first-policy-command";
    let removal = removal_payload(command_id, "project-1", &thread_id, &plan);
    request(fixture.socket(), "36", "worktree.remove", removal.clone()).await;
    wait_for_worktree_removal_git_boundary(&mut entered_rx).await;
    assert!(
        !fixture
            .repositories
            .get_command_receipt(command_id.to_owned())
            .await
            .expect("removal receipt read")
            .is_some_and(|receipt| receipt.status == "accepted")
    );

    let address = fixture.handle.as_ref().expect("server handle").local_addr();
    let mut cancelled_socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("cancelled policy socket")
        .0;
    request(
        &mut cancelled_socket,
        "37",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":command_id,
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(200),
            next_frame_past_heartbeat(&mut cancelled_socket)
        )
        .await
        .is_err(),
        "policy must wait without mutating while removal owns the command and project"
    );
    send_json(
        &mut cancelled_socket,
        json!({"_tag":"Interrupt","requestId":"37"}),
    )
    .await;
    assert!(matches!(
        next_server_message(&mut cancelled_socket).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause.iter().any(|item| matches!(item, CauseItem::Interrupt { .. }))
    ));

    let mut next_socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("next policy socket")
        .0;
    request(
        &mut next_socket,
        "38",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":command_id,
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(200),
            next_frame_past_heartbeat(&mut next_socket)
        )
        .await
        .is_err(),
        "the next waiter must remain behind the live removal claimant"
    );
    release.add_permits(1);
    let removal_result = removal_success_value(fixture.socket(), "36").await;
    assert_eq!(removal_result["gitOutcome"], "removed");
    assert_typed_catalog_failure(&mut next_socket, "38", "command-conflict").await;
    let project = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists");
    assert_eq!(project.worktree_discovery["visibility"], "hidden");
    let receipt = fixture
        .repositories
        .get_command_receipt(command_id.to_owned())
        .await
        .expect("receipt read")
        .expect("accepted removal receipt");
    assert_eq!(receipt.status, "accepted");
    assert_eq!(
        receipt.payload_digest.as_deref(),
        Some(
            canonical_command_digest(&removal)
                .expect("removal digest")
                .as_str()
        )
    );
    request(
        fixture.socket(),
        "3501",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let refreshed = success_value(fixture.socket(), "3501").await;
    let removed_key = normalize_worktree_path_key(&fixture.external, host_path_platform());
    let removed = refreshed["worktrees"]
        .as_array()
        .expect("refreshed worktrees")
        .iter()
        .find(|worktree| {
            worktree["path"].as_str().is_some_and(|path| {
                normalize_worktree_path_key(Path::new(path), host_path_platform()) == removed_key
            })
        });
    assert!(removed.is_none_or(|worktree| {
        worktree["directoryState"] == "missing" && worktree["eligibleForAdoption"] == false
    }));
    fixture.shutdown().await;
}

#[tokio::test]
async fn cancelling_policy_while_waiting_for_project_lock_releases_its_command_claim() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(Semaphore::new(0));
    let holder_release = release.clone();
    let catalog = fixture.catalog.clone();
    let holder = tokio::spawn(async move {
        catalog
            .with_project_mutation_lock("project-1", || async move {
                let _ = entered_tx.send(());
                let permit = holder_release
                    .acquire()
                    .await
                    .expect("project lock release");
                permit.forget();
            })
            .await;
    });
    entered_rx.await.expect("project lock holder enters");

    request(
        fixture.socket(),
        "39",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"cancelled-project-wait-policy",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(200),
            next_frame_past_heartbeat(fixture.socket())
        )
        .await
        .is_err(),
        "policy remains queued behind the project lock"
    );
    send_json(
        fixture.socket(),
        json!({"_tag":"Interrupt","requestId":"39"}),
    )
    .await;
    assert!(matches!(
        next_server_message(fixture.socket()).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause.iter().any(|item| matches!(item, CauseItem::Interrupt { .. }))
    ));
    request(
        fixture.socket(),
        "40",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"cancelled-project-wait-policy",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    release.add_permits(1);
    holder.await.expect("project lock holder joins");
    let retry = success_value(fixture.socket(), "40").await;
    assert_eq!(retry["visibility"], "shown");
    assert_eq!(
        fixture
            .repositories
            .get_command_receipt("cancelled-project-wait-policy".to_owned())
            .await
            .expect("receipt read")
            .expect("accepted retry receipt")
            .status,
        "accepted"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn interrupted_policy_handoff_retains_project_serialization_until_terminal_receipt() {
    let hooks = TestHooks::default();
    let mut fixture = CatalogRpcFixture::new_with_removal_services_and_options(
        false,
        Arc::new(TestNoopQuiescer),
        None,
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let pause = hooks.pause_before_next_command_persist();
    request(
        fixture.socket(),
        "41",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"interrupted-policy-handoff",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    timeout(ENGINE_HANDOFF_DEADLOCK_BOUND, pause.wait_until_entered())
        .await
        .expect("first policy envelope reaches its pre-persistence boundary");

    send_json(
        fixture.socket(),
        json!({"_tag":"Interrupt","requestId":"41"}),
    )
    .await;
    assert!(matches!(
        next_server_message(fixture.socket()).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause.iter().any(|item| matches!(item, CauseItem::Interrupt { .. }))
    ));
    request(
        fixture.socket(),
        "42",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"policy-after-interrupted-handoff",
            "projectId":"project-1",
            "dismissInitialPrompt":true
        }),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(200),
            next_frame_past_heartbeat(fixture.socket())
        )
        .await
        .is_err(),
        "the sibling update remains serialized while the first envelope is paused"
    );
    pause.release();

    let sibling = success_value(fixture.socket(), "42").await;
    assert_eq!(sibling["visibility"], "shown");
    assert!(sibling["initialPromptDismissedAt"].as_str().is_some());
    let persisted = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists")
        .worktree_discovery;
    assert_eq!(persisted["visibility"], "shown");
    assert!(persisted["initialPromptDismissedAt"].as_str().is_some());
    for command_id in [
        "interrupted-policy-handoff",
        "policy-after-interrupted-handoff",
    ] {
        assert_eq!(
            fixture
                .repositories
                .get_command_receipt(command_id.to_owned())
                .await
                .expect("receipt read")
                .expect("accepted policy receipt")
                .status,
            "accepted"
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn adoption_interrupt_after_engine_handoff_retains_catalog_lifecycle_until_terminal_receipt()
{
    let hooks = TestHooks::default();
    let mut fixture = CatalogRpcFixture::new_with_removal_services_and_options(
        true,
        Arc::new(TestNoopQuiescer),
        None,
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    request(
        fixture.socket(),
        "43",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "43").await;
    let candidate = eligible_candidate(&snapshot).clone();
    let pause = hooks.pause_before_next_command_persist();
    request(
        fixture.socket(),
        "44",
        "worktree.adopt",
        adoption_payload("interrupted-adoption-handoff", &candidate, &snapshot),
    )
    .await;
    timeout(ENGINE_HANDOFF_DEADLOCK_BOUND, pause.wait_until_entered())
        .await
        .expect("adoption envelope reaches the engine boundary");
    send_json(
        fixture.socket(),
        json!({"_tag":"Interrupt","requestId":"44"}),
    )
    .await;
    assert!(matches!(
        next_server_message(fixture.socket()).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause.iter().any(|item| matches!(item, CauseItem::Interrupt { .. }))
    ));

    let catalog = fixture.catalog.clone();
    let probe = tokio::spawn(async move {
        catalog
            .with_project_mutation_lock("project-1", || async {})
            .await;
    });
    assert!(
        timeout(Duration::from_millis(200), probe).await.is_err(),
        "the server-owned adoption operation must retain project/repository locks after the RPC waiter is interrupted"
    );

    pause.release();
    timeout(Duration::from_secs(5), async {
        loop {
            if fixture
                .repositories
                .get_command_receipt("interrupted-adoption-handoff".to_owned())
                .await
                .expect("receipt read")
                .is_some_and(|receipt| receipt.status == "accepted")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("adoption reaches a terminal durable receipt");
    let owners = fixture
        .repositories
        .list_threads_by_project("project-1".to_owned())
        .await
        .expect("project threads");
    assert!(owners.iter().any(|thread| {
        thread.kind == "workspace"
            && thread.deleted_at.is_none()
            && thread.worktree_path.as_deref().is_some_and(|path| {
                candidate["path"].as_str().is_some_and(|candidate| {
                    same_worktree_identity(Path::new(path), Path::new(candidate))
                })
            })
    }));
    fixture.shutdown().await;
}

#[tokio::test]
async fn detach_interrupt_after_engine_handoff_retains_removal_lifecycle_until_terminal_receipt() {
    let hooks = TestHooks::default();
    let quiescer = Arc::new(RecordingPendingQuiescer::default());
    let mut fixture = CatalogRpcFixture::new_with_removal_services_and_options(
        true,
        quiescer.clone(),
        None,
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "detach-handoff-adopt").await;
    let pause = hooks.pause_before_next_command_persist();
    request(
        fixture.socket(),
        "45",
        "worktree.removeFromBibCode",
        json!({
            "commandId":"interrupted-detach-handoff",
            "projectId":"project-1",
            "threadId":thread_id
        }),
    )
    .await;
    timeout(ENGINE_HANDOFF_DEADLOCK_BOUND, pause.wait_until_entered())
        .await
        .expect("detach envelope reaches the engine boundary");
    send_json(
        fixture.socket(),
        json!({"_tag":"Interrupt","requestId":"45"}),
    )
    .await;
    assert!(matches!(
        next_server_message(fixture.socket()).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause.iter().any(|item| matches!(item, CauseItem::Interrupt { .. }))
    ));
    assert_eq!(quiescer.call_count(), 1);
    assert!(!quiescer.last_cancellation().is_cancelled());
    assert_eq!(
        fixture
            .availability
            .guard_thread(&thread_id)
            .await
            .expect_err("detach remains guarded while persistence is paused")
            .state,
        bibcode_server::worktree_catalog::WorkspaceGuardState::Removing
    );
    let catalog = fixture.catalog.clone();
    let probe = tokio::spawn(async move {
        catalog
            .with_project_mutation_lock("project-1", || async {})
            .await;
    });
    assert!(timeout(Duration::from_millis(200), probe).await.is_err());

    pause.release();
    wait_for_accepted_receipt(&fixture.repositories, "interrupted-detach-handoff").await;
    fixture.operations.shutdown().await;
    assert!(
        fixture
            .repositories
            .get_thread(thread_id.clone())
            .await
            .expect("thread read")
            .expect("thread remains projected")
            .deleted_at
            .is_some()
    );
    assert!(!quiescer.last_cancellation().is_cancelled());
    fixture
        .availability
        .guard_thread(&thread_id)
        .await
        .expect("terminal detach clears the guard");
    assert!(
        fixture.external.exists(),
        "detach does not delete Git files"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destructive_remove_socket_close_after_engine_handoff_retains_lifecycle_until_terminal_receipt()
 {
    let hooks = TestHooks::default();
    let quiescer = Arc::new(RecordingPendingQuiescer::default());
    let removal_git = Arc::new(ImmediateFilesystemRemovalGit::default());
    let mut fixture = CatalogRpcFixture::new_with_removal_services_and_options(
        true,
        quiescer.clone(),
        Some(removal_git.clone()),
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-handoff-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "46").await;
    let pause = hooks.pause_before_next_command_persist();
    request(
        fixture.socket(),
        "47",
        "worktree.remove",
        removal_payload(
            "closed-socket-removal-handoff",
            "project-1",
            &thread_id,
            &plan,
        ),
    )
    .await;
    if timeout(
        WORKTREE_REMOVAL_INTEGRATION_DEADLINE,
        pause.wait_until_entered(),
    )
    .await
    .is_err()
    {
        let response = timeout(
            Duration::from_millis(250),
            next_server_message(fixture.socket()),
        )
        .await;
        panic!(
            "removal did not reach the engine after Git mutation; Git stages: {:?}; response: {response:?}",
            removal_git.call_counts()
        );
    }
    assert!(
        !fixture.external.exists(),
        "Git mutation completed before the pause"
    );
    fixture
        .socket()
        .close(None)
        .await
        .expect("close the request transport");
    assert!(!quiescer.last_cancellation().is_cancelled());
    assert_eq!(
        fixture
            .availability
            .guard_thread(&thread_id)
            .await
            .expect_err("removal stays guarded while persistence is paused")
            .state,
        bibcode_server::worktree_catalog::WorkspaceGuardState::Removing
    );
    let catalog = fixture.catalog.clone();
    let probe = tokio::spawn(async move {
        catalog
            .with_project_mutation_lock("project-1", || async {})
            .await;
    });
    assert!(timeout(Duration::from_millis(200), probe).await.is_err());

    pause.release();
    wait_for_accepted_receipt(&fixture.repositories, "closed-socket-removal-handoff").await;
    fixture.operations.shutdown().await;
    assert!(
        fixture
            .repositories
            .get_thread(thread_id.clone())
            .await
            .expect("thread read")
            .expect("thread remains projected")
            .deleted_at
            .is_some()
    );
    assert!(!quiescer.last_cancellation().is_cancelled());
    fixture
        .availability
        .guard_thread(&thread_id)
        .await
        .expect("terminal removal clears the guard");
    fixture.shutdown().await;
}

#[tokio::test]
async fn interrupted_policy_receipt_lookup_releases_claim_without_late_mutation() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let database = fixture.repositories.database().clone();
    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("exclusive database queue observer");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new((StdMutex::new(false), Condvar::new()));
    let blocker_release = release.clone();
    let blocker_database = database.clone();
    let blocker = tokio::spawn(async move {
        blocker_database
            .call(move |_| {
                let _ = entered_tx.send(());
                let (released, changed) = blocker_release.as_ref();
                let mut released = released.lock().expect("database blocker mutex");
                while !*released {
                    released = changed
                        .wait(released)
                        .expect("database blocker mutex after wait");
                }
                Ok(())
            })
            .await
    });
    entered_rx.await.expect("database blocker enters");

    request(
        fixture.socket(),
        "43",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"cancelled-policy-receipt-lookup",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    timeout(Duration::from_secs(5), async {
        while database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            < 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("policy receipt lookup queues behind the blocked database worker");
    send_json(
        fixture.socket(),
        json!({"_tag":"Interrupt","requestId":"43"}),
    )
    .await;
    assert!(matches!(
        next_server_message(fixture.socket()).await,
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } if cause.iter().any(|item| matches!(item, CauseItem::Interrupt { .. }))
    ));

    request(
        fixture.socket(),
        "44",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"cancelled-policy-receipt-lookup",
            "projectId":"project-1",
            "dismissInitialPrompt":true
        }),
    )
    .await;
    timeout(Duration::from_secs(5), async {
        while database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            < 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the retry acquires the released command claim and queues its receipt lookup");
    {
        let (released, changed) = release.as_ref();
        *released.lock().expect("database blocker mutex") = true;
        changed.notify_one();
    }
    blocker
        .await
        .expect("database blocker joins")
        .expect("database blocker succeeds");
    let retry = success_value(fixture.socket(), "44").await;
    assert_eq!(retry["visibility"], "hidden");
    assert!(retry["initialPromptDismissedAt"].as_str().is_some());
    let persisted = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists")
        .worktree_discovery;
    assert_eq!(persisted["visibility"], "hidden");
    assert!(persisted["initialPromptDismissedAt"].as_str().is_some());
    drop(observer);
    fixture.shutdown().await;
}

#[tokio::test]
async fn legacy_policy_replay_rejects_an_accepted_receipt_from_another_project() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let other_root = fixture.root.path().join("legacy-other-project");
    fs::create_dir(&other_root).expect("other project root");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create",
                "commandId":"legacy-other-project-create",
                "projectId":"legacy-other-project",
                "title":"Legacy Other",
                "workspaceRoot":other_root,
                "defaultModelSelection":null,
                "createdAt":"2026-08-10T01:00:00Z"
            }))
            .expect("other project command"),
        )
        .await
        .expect("other project created");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.meta.update",
                "commandId":"legacy-cross-project-policy",
                "projectId":"legacy-other-project",
                "title":"Legacy Other Updated"
            }))
            .expect("legacy other-project command"),
        )
        .await
        .expect("legacy other-project command accepted");

    request(
        fixture.socket(),
        "45",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"legacy-cross-project-policy",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "45", "command-conflict").await;
    assert_eq!(
        fixture
            .repositories
            .get_project("project-1".to_owned())
            .await
            .expect("project read")
            .expect("project exists")
            .worktree_discovery["visibility"],
        "hidden"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn legacy_policy_replay_rejects_an_unproven_project_meta_command_family() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.meta.update",
                "commandId":"legacy-cross-family-policy",
                "projectId":"project-1",
                "title":"Legacy Generic Metadata"
            }))
            .expect("legacy project metadata command"),
        )
        .await
        .expect("legacy project metadata command accepted");

    request(
        fixture.socket(),
        "46",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"legacy-cross-family-policy",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "46", "command-conflict").await;
    let project = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists");
    assert_eq!(project.title, "Legacy Generic Metadata");
    assert_eq!(project.worktree_discovery["visibility"], "hidden");
    fixture.shutdown().await;
}

#[tokio::test]
async fn legacy_policy_replay_rejects_an_adoption_result_with_matching_policy_payload() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    fixture
        .engine
        .dispatch(OrchestrationCommand::WorktreeAdoptResolved {
            command_id: "legacy-adoption-as-policy".to_owned(),
            project_id: "project-1".to_owned(),
            worktree_key: "legacy-adoption-worktree".to_owned(),
            path: fixture.external.to_string_lossy().into_owned(),
            branch: Some("feature/external".to_owned()),
            head: None,
            model_selection: json!({"instanceId":"codex","model":"gpt-5.4"}),
            runtime_mode: "full-access".to_owned(),
            interaction_mode: "default".to_owned(),
        })
        .await
        .expect("legacy adoption accepted");

    request(
        fixture.socket(),
        "47",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"legacy-adoption-as-policy",
            "projectId":"project-1",
            "visibility":"hidden"
        }),
    )
    .await;
    assert_typed_catalog_failure(fixture.socket(), "47", "command-conflict").await;
    assert_eq!(
        fixture
            .repositories
            .get_project("project-1".to_owned())
            .await
            .expect("project read")
            .expect("project exists")
            .worktree_discovery["visibility"],
        "hidden"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn legacy_policy_replay_preserves_a_provable_exact_policy_event() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let mut expected_policy = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists")
        .worktree_discovery;
    expected_policy["visibility"] = json!("shown");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.meta.update",
                "commandId":"legacy-exact-policy",
                "projectId":"project-1",
                "worktreeDiscovery":expected_policy
            }))
            .expect("legacy policy command"),
        )
        .await
        .expect("legacy policy command accepted");

    request(
        fixture.socket(),
        "48",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"legacy-exact-policy",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert_eq!(success_value(fixture.socket(), "48").await, expected_policy);
    assert_eq!(
        fixture
            .repositories
            .get_command_receipt("legacy-exact-policy".to_owned())
            .await
            .expect("receipt read")
            .expect("legacy receipt")
            .payload_digest,
        None
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn legacy_policy_replay_resumes_a_digestless_reserved_receipt_after_restart() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let mut expected_policy = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists")
        .worktree_discovery;
    expected_policy["visibility"] = json!("shown");
    fixture
        .repositories
        .reserve_command_receipt(CommandReceipt {
            command_id: "legacy-reserved-policy".to_owned(),
            aggregate_kind: "project".to_owned(),
            aggregate_id: "project-1".to_owned(),
            accepted_at: "2026-08-10T01:00:00Z".to_owned(),
            result_sequence: 0,
            status: "reserved".to_owned(),
            error: None,
            payload_digest: None,
        })
        .await
        .expect("legacy policy reservation");

    request(
        fixture.socket(),
        "49",
        "worktree.updateDiscoveryPolicy",
        json!({
            "commandId":"legacy-reserved-policy",
            "projectId":"project-1",
            "visibility":"shown"
        }),
    )
    .await;
    assert_eq!(success_value(fixture.socket(), "49").await, expected_policy);
    let receipt = fixture
        .repositories
        .get_command_receipt("legacy-reserved-policy".to_owned())
        .await
        .expect("receipt read")
        .expect("legacy receipt");
    assert_eq!(receipt.command_id, "legacy-reserved-policy");
    assert_eq!(receipt.aggregate_kind, "project");
    assert_eq!(receipt.aggregate_id, "project-1");
    assert!(!receipt.accepted_at.is_empty());
    assert!(receipt.result_sequence > 0);
    assert_eq!(receipt.status, "accepted");
    assert_eq!(receipt.error, None);
    assert_eq!(receipt.payload_digest, None);
    assert_eq!(
        fixture
            .repositories
            .get_project("project-1".to_owned())
            .await
            .expect("project read")
            .expect("project exists")
            .worktree_discovery,
        expected_policy
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn adopt_race_converges_to_one_thread_without_creating_a_git_worktree() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "90",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "90").await;
    let candidate = eligible_candidate(&snapshot);
    let before_inventory = git_output(&fixture.main, &["worktree", "list", "--porcelain"]);
    let payload = |command_id: &str| {
        json!({
            "commandId":command_id,
            "projectId":"project-1",
            "worktreeKey":candidate["worktreeKey"],
            "expectedGeneration":snapshot["generation"],
            "threadDefaults":{
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"plan"
            }
        })
    };
    send_json(
        fixture.socket(),
        json!([
            {"_tag":"Request","id":"91","tag":"worktree.adopt","payload":payload("adopt-race-1"),"headers":[]},
            {"_tag":"Request","id":"92","tag":"worktree.adopt","payload":payload("adopt-race-2"),"headers":[]}
        ]),
    )
    .await;

    let mut results = Vec::new();
    for _ in 0..2 {
        let message = next_server_message(fixture.socket()).await;
        let ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(value) },
            ..
        } = message
        else {
            panic!("expected adoption success: {message:?}");
        };
        results.push(value);
    }
    assert_eq!(results[0]["threadId"], results[1]["threadId"]);
    let mut dispositions = results
        .iter()
        .map(|result| result["disposition"].as_str().expect("disposition"))
        .collect::<Vec<_>>();
    dispositions.sort_unstable();
    assert_eq!(dispositions, vec!["created", "existing"]);
    let threads = fixture
        .repositories
        .list_threads_by_project("project-1".to_owned())
        .await
        .expect("threads");
    assert_eq!(
        threads
            .iter()
            .filter(|thread| {
                thread.kind == "workspace"
                    && thread.deleted_at.is_none()
                    && thread.worktree_path.as_deref().is_some_and(|path| {
                        same_worktree_identity(Path::new(path), &fixture.external)
                    })
            })
            .count(),
        1
    );
    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"]),
        before_inventory
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn adopt_public_admission_replays_exactly_and_conflicts_on_changed_payload() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "900",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "900").await;
    let candidate = eligible_candidate(&snapshot).clone();
    request(
        fixture.socket(),
        "901",
        "worktree.adopt",
        adoption_payload("adopt-admission-owner", &candidate, &snapshot),
    )
    .await;
    let owner = success_value(fixture.socket(), "901").await;
    request(
        fixture.socket(),
        "902",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let owned_snapshot = success_value(fixture.socket(), "902").await;
    let owned_candidate = descriptor_for_key(&owned_snapshot, &candidate["worktreeKey"]);
    let payload = adoption_payload("adopt-admitted", owned_candidate, &owned_snapshot);
    request(fixture.socket(), "903", "worktree.adopt", payload.clone()).await;
    let first = success_value(fixture.socket(), "903").await;
    assert_eq!(first["threadId"], owner["threadId"]);
    assert_eq!(first["disposition"], "existing");
    request(
        fixture.socket(),
        "919",
        "worktree.removeFromBibCode",
        json!({
            "commandId":"delete-admitted-owner",
            "projectId":"project-1",
            "threadId":first["threadId"]
        }),
    )
    .await;
    success_value(fixture.socket(), "919").await;
    let event_count = fixture
        .repositories
        .read_events_from_sequence(0, 512)
        .await
        .expect("event read")
        .len();

    request(fixture.socket(), "904", "worktree.adopt", payload.clone()).await;
    assert_eq!(
        success_value(fixture.socket(), "904").await,
        first,
        "an identical public retry must replay the accepted result exactly"
    );

    let mut changed_project = payload.clone();
    changed_project["projectId"] = json!("different-project");
    let mut changed_key = payload.clone();
    changed_key["worktreeKey"] = json!("different-worktree-key");
    let mut changed_generation = payload.clone();
    changed_generation["expectedGeneration"] = json!(
        payload["expectedGeneration"]
            .as_u64()
            .expect("expected generation")
            + 1
    );
    let mut changed_defaults = payload.clone();
    changed_defaults["threadDefaults"]["interactionMode"] = json!("plan");
    let mutations = [
        ("project", changed_project),
        ("worktree-key", changed_key),
        ("expected-generation", changed_generation),
        ("thread-defaults", changed_defaults),
    ];
    let mut outcomes = Vec::new();
    let external_path = candidate["path"]
        .as_str()
        .expect("candidate path")
        .to_owned();
    for (index, (field, changed_payload)) in mutations.into_iter().enumerate() {
        let request_id = (905 + index).to_string();
        request(
            fixture.socket(),
            &request_id,
            "worktree.adopt",
            changed_payload,
        )
        .await;
        let (outcome, wire_value) = adoption_outcome(fixture.socket(), &request_id).await;
        assert!(
            !wire_value.to_string().contains(&external_path),
            "a command conflict must not expose the server-resolved checkout path"
        );
        outcomes.push((field, outcome));
    }
    assert_eq!(
        outcomes,
        vec![
            ("project", "command-conflict".to_owned()),
            ("worktree-key", "command-conflict".to_owned()),
            ("expected-generation", "command-conflict".to_owned()),
            ("thread-defaults", "command-conflict".to_owned()),
        ]
    );
    assert_eq!(
        fixture
            .repositories
            .read_events_from_sequence(0, 512)
            .await
            .expect("event read")
            .len(),
        event_count,
        "replay and conflicting retries must not append orchestration effects"
    );
    assert!(
        fixture
            .repositories
            .get_command_receipt("adopt-admitted".to_owned())
            .await
            .expect("receipt read")
            .expect("receipt")
            .payload_digest
            .is_some(),
        "the public adoption receipt must retain the canonical public payload digest"
    );
    let changed = fixture
        .repositories
        .database()
        .call(|connection| {
            Ok(connection.execute(
                "UPDATE orchestration_events SET metadata_json = '{\"worktreeKey\":\"corrupt\",\"adoptionResult\":\"malformed\"}' WHERE command_id = 'adopt-admitted' AND event_type = 'project.meta-updated'",
                [],
            )?)
        })
        .await
        .expect("corrupt adoption receipt metadata");
    assert_eq!(changed, 1);
    request(fixture.socket(), "909", "worktree.adopt", payload).await;
    let (outcome, wire_value) = adoption_outcome(fixture.socket(), "909").await;
    assert_eq!(outcome, "internal");
    assert!(!wire_value.to_string().contains(&external_path));
    assert_eq!(
        fixture
            .repositories
            .read_events_from_sequence(0, 512)
            .await
            .expect("event read")
            .len(),
        event_count,
        "a malformed durable result must fail closed without replacement effects"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn adopt_stale_generation_forces_refresh_then_revalidates() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "93",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let stale = success_value(fixture.socket(), "93").await;
    let candidate = eligible_candidate(&stale).clone();
    request(
        fixture.socket(),
        "94",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let current = success_value(fixture.socket(), "94").await;
    assert!(current["generation"].as_u64() > stale["generation"].as_u64());

    request(
        fixture.socket(),
        "95",
        "worktree.adopt",
        adoption_payload("adopt-stale", &candidate, &stale),
    )
    .await;
    let adopted = success_value(fixture.socket(), "95").await;
    assert_eq!(adopted["disposition"], "created");
    assert_eq!(
        fixture
            .catalog
            .latest("project-1")
            .await
            .expect("latest")
            .generation,
        current["generation"].as_u64().expect("current generation") + 1,
        "stale adoption must force exactly a fresh revalidation scan before mutating"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn adopt_external_disappearance_fails_without_creating_a_thread() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "96",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "96").await;
    let candidate = eligible_candidate(&snapshot).clone();
    fs::remove_dir_all(&fixture.external).expect("external checkout disappears");

    request(
        fixture.socket(),
        "97",
        "worktree.adopt",
        adoption_payload("adopt-missing", &candidate, &snapshot),
    )
    .await;
    assert_typed_adoption_failure(fixture.socket(), "97", "workspace-missing").await;
    let threads = fixture
        .repositories
        .list_threads_by_project("project-1".to_owned())
        .await
        .expect("threads");
    assert!(threads.iter().all(|thread| thread.kind != "workspace"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn adopt_restores_archived_then_returns_the_active_thread() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "98",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let initial = success_value(fixture.socket(), "98").await;
    let candidate = eligible_candidate(&initial).clone();
    request(
        fixture.socket(),
        "99",
        "worktree.adopt",
        adoption_payload("adopt-first", &candidate, &initial),
    )
    .await;
    let created = success_value(fixture.socket(), "99").await;
    let thread_id = created["threadId"].as_str().expect("thread id").to_owned();
    fixture
        .engine
        .dispatch(OrchestrationCommand::ThreadArchive {
            command_id: "archive-adopted".to_owned(),
            thread_id: thread_id.clone(),
        })
        .await
        .expect("archive");
    request(
        fixture.socket(),
        "100",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let archived = success_value(fixture.socket(), "100").await;
    let archived_descriptor = descriptor_for_key(&archived, &candidate["worktreeKey"]);
    assert_eq!(archived_descriptor["adoptionState"], "archived");
    request(
        fixture.socket(),
        "101",
        "worktree.adopt",
        adoption_payload("adopt-restore", archived_descriptor, &archived),
    )
    .await;
    let restored = success_value(fixture.socket(), "101").await;
    assert_eq!(
        restored,
        json!({"threadId":thread_id,"disposition":"restored"})
    );

    request(
        fixture.socket(),
        "102",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let active = success_value(fixture.socket(), "102").await;
    let active_descriptor = descriptor_for_key(&active, &candidate["worktreeKey"]);
    request(
        fixture.socket(),
        "103",
        "worktree.adopt",
        adoption_payload("adopt-existing", active_descriptor, &active),
    )
    .await;
    let existing = success_value(fixture.socket(), "103").await;
    assert_eq!(
        existing,
        json!({"threadId":thread_id,"disposition":"existing"})
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn adopted_branch_reconciliation_is_deterministic_and_healthy_only() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    request(
        fixture.socket(),
        "104",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let initial = success_value(fixture.socket(), "104").await;
    let candidate = eligible_candidate(&initial).clone();
    request(
        fixture.socket(),
        "105",
        "worktree.adopt",
        adoption_payload("adopt-for-branch-reconcile", &candidate, &initial),
    )
    .await;
    let adopted = success_value(fixture.socket(), "105").await;
    let thread_id = adopted["threadId"].as_str().expect("thread id").to_owned();

    git(
        &fixture.external,
        &["checkout", "-b", "feature/reconciled"],
        None,
    );
    let head = git_output(&fixture.external, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    request(
        fixture.socket(),
        "106",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let refreshed = success_value(fixture.socket(), "106").await;
    assert_eq!(
        descriptor_for_key(&refreshed, &candidate["worktreeKey"])["branch"],
        "feature/reconciled"
    );
    let durable = fixture
        .repositories
        .get_thread(thread_id.clone())
        .await
        .expect("thread read")
        .expect("thread exists");
    assert_eq!(durable.branch.as_deref(), Some("feature/reconciled"));
    let reconciliation_events = branch_reconciliation_events(&fixture.repositories).await;
    assert_eq!(reconciliation_events.len(), 1);
    let expected_command_id =
        branch_reconciliation_command_id(&thread_id, Some("feature/reconciled"), Some(&head));
    assert_eq!(
        reconciliation_events[0].event.command_id.as_deref(),
        Some(expected_command_id.as_str())
    );
    assert!(!expected_command_id.contains(candidate["path"].as_str().expect("candidate path")));

    request(
        fixture.socket(),
        "107",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let _unchanged = success_value(fixture.socket(), "107").await;
    assert_eq!(
        branch_reconciliation_events(&fixture.repositories)
            .await
            .len(),
        1,
        "an unchanged healthy snapshot must not dispatch a second command"
    );

    git(
        &fixture.external,
        &["checkout", "-b", "feature/degraded"],
        None,
    );
    let common_dir = fixture.main.join(".git");
    let unavailable = fixture.main.join(".git-unavailable");
    fs::rename(&common_dir, &unavailable).expect("make Git metadata unavailable");
    request(
        fixture.socket(),
        "108",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let degraded = success_value(fixture.socket(), "108").await;
    assert_eq!(degraded["authoritative"], false);
    assert_eq!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread exists")
            .branch
            .as_deref(),
        Some("feature/reconciled")
    );
    assert_eq!(
        branch_reconciliation_events(&fixture.repositories)
            .await
            .len(),
        1,
        "a degraded retained snapshot must never emit branch metadata"
    );
    fs::rename(&unavailable, &common_dir).expect("restore Git metadata");
    fixture.shutdown().await;
}

#[tokio::test]
async fn managed_creation_reuses_a_free_local_branch_after_terminal_catalog_invalidation() {
    assert_managed_creation_terminal_invalidation(true).await;
}

#[tokio::test]
async fn managed_creation_suffixes_an_occupied_branch_after_terminal_catalog_invalidation() {
    assert_managed_creation_terminal_invalidation(false).await;
}

#[tokio::test]
async fn raw_vcs_remove_rejects_an_adopted_target_without_mutating_git_or_ownership() {
    let mut fixture = CatalogRpcFixture::new(false).await;
    let adopted_cwd = fixture.create_named_external_worktree("adopted-cwd", "feature/adopted-cwd");
    let removed_target =
        fixture.create_named_external_worktree("removed-target", "feature/removed-target");
    fixture
        .create_project_with_thread(
            "project-target",
            removed_target.clone(),
            removed_target.clone(),
        )
        .await;
    let projection = fixture
        .repositories
        .load_worktree_catalog_projection("project-target".to_owned(), 512)
        .await
        .expect("projection read")
        .expect("target project");
    assert_eq!(
        projection.threads[0].worktree_path.as_deref(),
        Some(removed_target.to_string_lossy().as_ref())
    );
    fixture
        .create_project(
            "project-target",
            fixture.root.path().join("missing-primary"),
        )
        .await;
    fixture
        .repositories
        .database()
        .call(|connection| {
            connection.execute(
                "DELETE FROM project_worktree_repository_pins WHERE project_id = 'project-target'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("remove durable pin for target-only association");

    request(
        fixture.socket(),
        "71",
        "vcs.removeWorktree",
        json!({ "cwd": adopted_cwd, "path": removed_target, "force": true }),
    )
    .await;
    let message = next_server_message(fixture.socket()).await;
    assert!(
        matches!(
            message,
            ServerMessage::Defect { ref defect }
                if defect.as_str().is_some_and(|detail| {
                    detail.contains("Unknown request tag") && detail.contains("vcs.removeWorktree")
                })
        ),
        "the retired raw method must be unavailable: {message:?}"
    );
    assert!(
        removed_target.exists(),
        "a public raw target cannot delete an adopted worktree"
    );
    assert!(
        fixture
            .repositories
            .get_thread(projection.threads[0].thread_id.clone())
            .await
            .expect("owner read")
            .expect("owner remains")
            .deleted_at
            .is_none(),
        "a rejected raw removal cannot retire or corrupt its owner"
    );
    fixture.shutdown().await;
}

#[test]
fn authoritative_baseline_compaction_filters_deduplicates_and_caps_exactly() {
    let mut worktrees = (0..514)
        .map(|index| descriptor(format!("/repo/worktree-{index:03}"), true))
        .collect::<Vec<_>>();
    worktrees.insert(2, descriptor("/repo/worktree-001".to_owned(), true));
    worktrees.insert(3, descriptor("/repo/ineligible".to_owned(), false));
    let snapshot = WorktreeCatalogSnapshot {
        repository_key: "repository-1".to_owned(),
        generation: 9,
        authoritative: true,
        observed_at: "2026-08-09T00:00:00Z".to_owned(),
        scan_status: CatalogScanStatus::Ready,
        worktrees,
        adopted_workspaces: Vec::new(),
    };

    let compacted = compact_eligible_baseline(&snapshot).expect("authoritative baseline");

    assert_eq!(compacted.len(), 512);
    assert_eq!(compacted[0], "/repo/worktree-000");
    assert_eq!(compacted[1], "/repo/worktree-001");
    assert_eq!(compacted[2], "/repo/worktree-002");
    assert_eq!(compacted[511], "/repo/worktree-511");
    assert!(!compacted.iter().any(|path| path == "/repo/ineligible"));
}

fn descriptor(path: String, eligible_for_adoption: bool) -> WorktreeDescriptor {
    WorktreeDescriptor {
        worktree_key: format!("key-{path}"),
        path,
        branch: Some("feature".to_owned()),
        head: Some("abc123".to_owned()),
        is_primary: false,
        is_bare: false,
        locked: false,
        lock_reason: None,
        registration_state: WorktreeRegistrationState::Registered,
        directory_state: WorktreeDirectoryState::Present,
        adoption_state: WorktreeAdoptionState::None,
        adopted_thread_id: None,
        eligible_for_adoption,
    }
}

#[tokio::test]
async fn removal_present_clean_is_verified_detached_branch_preserving_and_idempotent() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-clean").await;
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    assert_eq!(plan["availability"], "present");
    assert_eq!(plan["trackedChangeCount"], 0);
    assert_eq!(plan["untrackedFileCount"], 0);
    let payload = json!({
        "commandId":"remove-clean",
        "projectId":"project-1",
        "threadId":thread_id,
        "mode":"delete-git-worktree",
        "expectedGeneration":plan["generation"],
        "planToken":plan["planToken"],
        "forceDirty":false,
        "confirmRepositoryWidePrune":false
    });
    request(fixture.socket(), "1203", "worktree.remove", payload.clone()).await;
    let result = removal_success_value(fixture.socket(), "1203").await;
    assert_eq!(result["threadRemoved"], true);
    assert_eq!(result["gitOutcome"], "removed");
    assert_eq!(result["orphanCleanupPending"], false);
    assert!(!fixture.external.exists());
    assert!(
        git_output(&fixture.main, &["branch", "--list", "feature/external"])
            .contains("feature/external"),
        "removal preserves the local branch"
    );
    let thread = fixture
        .repositories
        .get_thread(thread_id.clone())
        .await
        .expect("thread read")
        .expect("thread projection");
    assert!(thread.deleted_at.is_some());

    request(fixture.socket(), "1204", "worktree.remove", payload.clone()).await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1204").await,
        result
    );
    let mut changed = payload;
    changed["forceDirty"] = json!(true);
    request(fixture.socket(), "1205", "worktree.remove", changed).await;
    assert_typed_removal_failure(fixture.socket(), "1205", "command-conflict").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_command_reservation_allows_only_one_cross_repository_git_mutation() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let removal_git = Arc::new(BlockingRemovalGit {
        inner: GitRepository::default(),
        entered: entered_tx,
        release: release.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_removal_git(true, removal_git).await;
    let first_thread = adopt_external_for_removal(&mut fixture, "reserve-adopt-first").await;

    let second_main = fixture.root.path().join("second-main");
    let second_external = fixture.root.path().join("second-external");
    fs::create_dir(&second_main).expect("second primary directory");
    git(&second_main, &["init", "--initial-branch", "main"], None);
    git(
        &second_main,
        &["config", "user.email", "rpc@example.invalid"],
        None,
    );
    git(&second_main, &["config", "user.name", "RPC Test"], None);
    fs::write(second_main.join("README.md"), "second fixture\n").expect("second fixture file");
    git(&second_main, &["add", "README.md"], None);
    git(&second_main, &["commit", "-m", "initial"], None);
    git(
        &second_main,
        &["worktree", "add", "-b", "feature/second"],
        Some(&second_external),
    );
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create",
                "commandId":"reserve-project-second",
                "projectId":"project-2",
                "title":"Second Catalog RPC",
                "workspaceRoot":second_main,
                "defaultModelSelection":null,
                "createdAt":"2026-08-09T00:01:00Z"
            }))
            .expect("second project command"),
        )
        .await
        .expect("second project created");
    let second_thread = adopt_project_worktree_for_removal(
        &mut fixture,
        "project-2",
        "reserve-adopt-second",
        "1260",
    )
    .await;

    let first_plan = removal_plan(&mut fixture, "project-1", &first_thread, "1262").await;
    let second_plan = removal_plan(&mut fixture, "project-2", &second_thread, "1263").await;
    let address = fixture.handle.as_ref().expect("server handle").local_addr();
    let mut first_socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("first removal socket")
        .0;
    let mut second_socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("second removal socket")
        .0;
    request(
        &mut first_socket,
        "1264",
        "worktree.remove",
        removal_payload(
            "shared-removal-command",
            "project-1",
            &first_thread,
            &first_plan,
        ),
    )
    .await;
    request(
        &mut second_socket,
        "1265",
        "worktree.remove",
        removal_payload(
            "shared-removal-command",
            "project-2",
            &second_thread,
            &second_plan,
        ),
    )
    .await;

    let first_boundary = wait_for_worktree_removal_git_boundary(&mut entered_rx).await;
    let second_boundary = timeout(Duration::from_millis(200), entered_rx.recv()).await;
    release.add_permits(2);
    assert!(
        second_boundary.is_err(),
        "only one command payload may cross Git; first={first_boundary:?}, second={second_boundary:?}"
    );

    let first_exit = next_worktree_removal_message(&mut first_socket).await;
    let second_exit = next_worktree_removal_message(&mut second_socket).await;
    let (winner_project, winner_thread, winner_path, loser_thread, loser_path) =
        match (&first_exit, &second_exit) {
            (
                ServerMessage::Exit {
                    exit: RpcExit::Success { .. },
                    ..
                },
                ServerMessage::Exit {
                    exit: RpcExit::Failure { cause },
                    ..
                },
            ) if removal_failure_has_reason(cause, "command-conflict") => (
                "project-1",
                first_thread.as_str(),
                fixture.external.as_path(),
                second_thread.as_str(),
                second_external.as_path(),
            ),
            (
                ServerMessage::Exit {
                    exit: RpcExit::Failure { cause },
                    ..
                },
                ServerMessage::Exit {
                    exit: RpcExit::Success { .. },
                    ..
                },
            ) if removal_failure_has_reason(cause, "command-conflict") => (
                "project-2",
                second_thread.as_str(),
                second_external.as_path(),
                first_thread.as_str(),
                fixture.external.as_path(),
            ),
            exits => panic!("expected one success and one command conflict: {exits:?}"),
        };
    assert!(
        !winner_path.exists(),
        "winner {winner_project} must mutate Git"
    );
    assert!(loser_path.exists(), "loser must not mutate Git");
    assert!(
        fixture
            .repositories
            .get_thread(winner_thread.to_owned())
            .await
            .expect("winner read")
            .expect("winner thread")
            .deleted_at
            .is_some()
    );
    assert!(
        fixture
            .repositories
            .get_thread(loser_thread.to_owned())
            .await
            .expect("loser read")
            .expect("loser thread")
            .deleted_at
            .is_none()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn generic_command_claim_blocks_removal_before_git_and_keeps_terminal_receipt() {
    let hooks = TestHooks::default();
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let removal_git = Arc::new(BlockingRemovalGit {
        inner: GitRepository::default(),
        entered: entered_tx,
        release: release.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_engine_options_and_removal_git(
        true,
        removal_git,
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "receipt-race-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1266").await;
    let removal = removal_payload(
        "shared-generic-removal-command",
        "project-1",
        &thread_id,
        &plan,
    );
    let generic: OrchestrationCommand = serde_json::from_value(json!({
        "type":"thread.meta.update",
        "commandId":"shared-generic-removal-command",
        "threadId":thread_id,
        "title":"must-roll-back"
    }))
    .expect("generic command");
    let pause = hooks.pause_before_next_command_persist();
    let engine = fixture.engine.clone();
    let generic_task = tokio::spawn(async move { engine.dispatch(generic).await });
    pause.wait_until_entered().await;

    request(fixture.socket(), "1267", "worktree.remove", removal.clone()).await;
    assert!(
        timeout(Duration::from_millis(200), entered_rx.recv())
            .await
            .is_err(),
        "removal must wait behind the generic command claim before Git"
    );
    pause.release();

    generic_task
        .await
        .expect("generic join")
        .expect("generic claimant commits first");
    assert_typed_removal_failure(fixture.socket(), "1267", "command-conflict").await;
    assert!(fixture.external.exists());
    let receipt = fixture
        .repositories
        .get_command_receipt("shared-generic-removal-command".to_owned())
        .await
        .expect("receipt read")
        .expect("accepted receipt");
    assert_eq!(receipt.status, "accepted");
    assert_eq!(receipt.payload_digest, None);
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_command_claim_blocks_generic_dispatch_through_git_and_detach() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let removal_git = Arc::new(BlockingRemovalGit {
        inner: GitRepository::default(),
        entered: entered_tx,
        release: release.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_removal_git(true, removal_git).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "removal-first-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1268").await;
    let removal = removal_payload(
        "removal-first-shared-command",
        "project-1",
        &thread_id,
        &plan,
    );
    request(fixture.socket(), "1269", "worktree.remove", removal.clone()).await;
    wait_for_worktree_removal_git_boundary(&mut entered_rx).await;

    let address = fixture.handle.as_ref().expect("server handle").local_addr();
    let mut generic_socket = connect_async(format!("ws://{address}/ws"))
        .await
        .expect("generic socket")
        .0;
    request(
        &mut generic_socket,
        "1270",
        "orchestration.dispatchCommand",
        json!({
            "type":"thread.meta.update",
            "commandId":"removal-first-shared-command",
            "threadId":thread_id,
            "title":"must-not-commit"
        }),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(200),
            next_frame_past_heartbeat(&mut generic_socket)
        )
        .await
        .is_err(),
        "generic dispatch must wait while removal owns Git and detach"
    );
    release.add_permits(1);
    let removal_result = removal_success_value(fixture.socket(), "1269").await;
    assert_eq!(removal_result["gitOutcome"], "removed");
    match next_server_message(&mut generic_socket).await {
        ServerMessage::Exit {
            exit: RpcExit::Failure { cause },
            ..
        } => assert!(
            cause.iter().any(|item| match item {
                CauseItem::Fail { error } => error["message"]
                    .as_str()
                    .is_some_and(|message| message.to_ascii_lowercase().contains("conflict")),
                _ => false,
            }),
            "generic loser must report a command conflict: {cause:?}"
        ),
        other => panic!("expected generic command conflict after removal: {other:?}"),
    }
    let receipt = fixture
        .repositories
        .get_command_receipt("removal-first-shared-command".to_owned())
        .await
        .expect("receipt read")
        .expect("accepted removal receipt");
    assert_eq!(receipt.status, "accepted");
    assert_eq!(
        receipt.payload_digest.as_deref(),
        Some(
            canonical_command_digest(&removal)
                .expect("removal digest")
                .as_str()
        )
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_fences_cross_project_owner_create_and_retarget_through_detach() {
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let removal_git = Arc::new(BlockingRemovalGit {
        inner: GitRepository::default(),
        entered: entered_tx,
        release: release.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_removal_git(true, removal_git).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "ownership-fence-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1268").await;
    let second_root = fixture.root.path().join("ownership-project-two");
    let previous_path = second_root.join("previous-worktree");
    fs::create_dir(&second_root).expect("second project root");
    fs::create_dir(&previous_path).expect("previous workspace path");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create",
                "commandId":"ownership-project-two-create",
                "projectId":"ownership-project-two",
                "title":"Ownership Project Two",
                "workspaceRoot":second_root,
                "defaultModelSelection":null,
                "createdAt":"2026-08-10T00:01:00Z"
            }))
            .expect("second project command"),
        )
        .await
        .expect("second project");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create",
                "commandId":"retarget-owner-create",
                "threadId":"retarget-owner",
                "projectId":"ownership-project-two",
                "title":"Retarget Owner",
                "kind":"workspace",
                "modelSelection":{},
                "runtimeMode":"full-access",
                "interactionMode":"default",
                "branch":null,
                "worktreePath":previous_path,
                "createdAt":"2026-08-10T00:01:01Z"
            }))
            .expect("retarget owner command"),
        )
        .await
        .expect("retarget owner");

    request(
        fixture.socket(),
        "1269",
        "worktree.remove",
        removal_payload("ownership-fence-remove", "project-1", &thread_id, &plan),
    )
    .await;
    wait_for_worktree_removal_git_boundary(&mut entered_rx).await;

    let cancelled_engine = fixture.engine.clone();
    let cancelled_target = fixture.external.clone();
    let mut cancelled = tokio::spawn(async move {
        cancelled_engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.create",
                    "commandId":"cancelled-racing-owner-create-command",
                    "threadId":"cancelled-racing-owner-create",
                    "projectId":"ownership-project-two",
                    "title":"Cancelled Racing Owner",
                    "modelSelection":{},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":cancelled_target,
                    "createdAt":"2026-08-10T00:01:02Z"
                }))
                .expect("cancelled racing create command"),
            )
            .await
    });
    assert!(
        timeout(Duration::from_millis(100), &mut cancelled)
            .await
            .is_err(),
        "cancelled owner create must first wait behind removal"
    );
    cancelled.abort();
    assert!(
        cancelled
            .await
            .expect_err("cancelled owner waiter stops")
            .is_cancelled()
    );
    assert!(
        fixture
            .repositories
            .get_thread("cancelled-racing-owner-create".to_owned())
            .await
            .expect("cancelled owner read")
            .is_none()
    );

    let create_engine = fixture.engine.clone();
    let target = fixture.external.clone();
    let mut create = tokio::spawn(async move {
        create_engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.create",
                    "commandId":"racing-owner-create-command",
                    "threadId":"racing-owner-create",
                    "projectId":"ownership-project-two",
                    "title":"Racing Owner",
                    "modelSelection":{},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":target,
                    "createdAt":"2026-08-10T00:01:02Z"
                }))
                .expect("racing create command"),
            )
            .await
    });
    let retarget_engine = fixture.engine.clone();
    let target = fixture.external.clone();
    let mut retarget = tokio::spawn(async move {
        retarget_engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.meta.update",
                    "commandId":"racing-owner-retarget-command",
                    "threadId":"retarget-owner",
                    "worktreePath":target
                }))
                .expect("racing retarget command"),
            )
            .await
    });
    let bootstrap_engine = fixture.engine.clone();
    let alias_segment = fixture
        .external
        .parent()
        .expect("external parent")
        .join("canonical-alias-segment");
    fs::create_dir(&alias_segment).expect("canonical alias segment");
    let target_alias = alias_segment
        .join("..")
        .join(fixture.external.file_name().expect("external file name"));
    let mut bootstrap = tokio::spawn(async move {
        bootstrap_engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.turn.start",
                    "commandId":"racing-bootstrap-owner-command",
                    "threadId":"racing-bootstrap-owner",
                    "message":{
                        "messageId":"racing-bootstrap-message",
                        "role":"user",
                        "text":"bootstrap",
                        "attachments":[]
                    },
                    "modelSelection":{},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "bootstrap":{"createThread":{
                        "projectId":"ownership-project-two",
                        "title":"Racing Bootstrap Owner",
                        "modelSelection":{},
                        "runtimeMode":"full-access",
                        "interactionMode":"default",
                        "branch":null,
                        "worktreePath":target_alias,
                        "createdAt":"2026-08-10T00:01:03Z"
                    }},
                    "createdAt":"2026-08-10T00:01:03Z"
                }))
                .expect("racing bootstrap command"),
            )
            .await
    });
    let create_early = timeout(Duration::from_millis(100), &mut create).await;
    let retarget_early = timeout(Duration::from_millis(100), &mut retarget).await;
    let bootstrap_early = timeout(Duration::from_millis(100), &mut bootstrap).await;
    let create_early_debug = format!("{create_early:?}");
    let retarget_early_debug = format!("{retarget_early:?}");
    let bootstrap_early_debug = format!("{bootstrap_early:?}");
    let create_waited = create_early.is_err();
    let retarget_waited = retarget_early.is_err();
    let bootstrap_waited = bootstrap_early.is_err();
    release.add_permits(1);
    let removal_result = removal_success_value(fixture.socket(), "1269").await;
    let create_result = match create_early {
        Ok(joined) => joined.expect("create joins"),
        Err(_) => create.await.expect("create joins after removal"),
    };
    let retarget_result = match retarget_early {
        Ok(joined) => joined.expect("retarget joins"),
        Err(_) => retarget.await.expect("retarget joins after removal"),
    };
    let bootstrap_result = match bootstrap_early {
        Ok(joined) => joined.expect("bootstrap joins"),
        Err(_) => bootstrap.await.expect("bootstrap joins after removal"),
    };

    assert!(
        create_waited,
        "owner create must wait behind removal: {create_early_debug}"
    );
    assert!(
        retarget_waited,
        "owner retarget must wait behind removal: {retarget_early_debug}"
    );
    assert!(
        bootstrap_waited,
        "bootstrap owner create through a canonical alias must wait behind removal: {bootstrap_early_debug}"
    );
    assert!(create_result.is_err(), "stale owner create must revalidate");
    assert!(
        retarget_result.is_err(),
        "stale owner retarget must revalidate"
    );
    assert!(
        bootstrap_result.is_err(),
        "stale bootstrap owner create must revalidate"
    );
    assert_eq!(removal_result["gitOutcome"], "removed");
    assert!(
        fixture
            .repositories
            .get_thread("racing-owner-create".to_owned())
            .await
            .expect("racing owner read")
            .is_none()
    );
    let retargeted = fixture
        .repositories
        .get_thread("retarget-owner".to_owned())
        .await
        .expect("retarget owner read")
        .expect("retarget owner remains");
    assert_ne!(
        retargeted.worktree_path.as_deref(),
        Some(fixture.external.to_string_lossy().as_ref())
    );
    assert!(
        fixture
            .repositories
            .get_thread("racing-bootstrap-owner".to_owned())
            .await
            .expect("bootstrap owner read")
            .is_none()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn owner_mutation_that_wins_the_fence_invalidates_removal_before_git() {
    let hooks = TestHooks::default();
    let remove_calls = Arc::new(AtomicUsize::new(0));
    let mut fixture = CatalogRpcFixture::new_with_engine_options_and_removal_git(
        true,
        Arc::new(CountingRemovalGit {
            inner: GitRepository::default(),
            remove_calls: remove_calls.clone(),
        }),
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "ownership-winner-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1271").await;
    let second_root = fixture.root.path().join("ownership-winner-project");
    let previous_path = second_root.join("previous-worktree");
    fs::create_dir(&second_root).expect("second project root");
    fs::create_dir(&previous_path).expect("previous workspace path");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create",
                "commandId":"ownership-winner-project-create",
                "projectId":"ownership-winner-project",
                "title":"Ownership Winner",
                "workspaceRoot":second_root,
                "defaultModelSelection":null,
                "createdAt":"2026-08-10T00:02:00Z"
            }))
            .expect("second project command"),
        )
        .await
        .expect("second project");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"thread.create",
                "commandId":"ownership-winner-thread-create",
                "threadId":"ownership-winner-thread",
                "projectId":"ownership-winner-project",
                "title":"Ownership Winner Thread",
                "kind":"workspace",
                "modelSelection":{},
                "runtimeMode":"full-access",
                "interactionMode":"default",
                "branch":null,
                "worktreePath":previous_path,
                "createdAt":"2026-08-10T00:02:01Z"
            }))
            .expect("owner thread command"),
        )
        .await
        .expect("owner thread");

    let pause = hooks.pause_before_next_command_persist();
    let engine = fixture.engine.clone();
    let target = fixture.external.clone();
    let mutation = tokio::spawn(async move {
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.meta.update",
                    "commandId":"ownership-winner-retarget",
                    "threadId":"ownership-winner-thread",
                    "worktreePath":target
                }))
                .expect("retarget command"),
            )
            .await
    });
    pause.wait_until_entered().await;
    request(
        fixture.socket(),
        "1272",
        "worktree.remove",
        removal_payload("ownership-loser-remove", "project-1", &thread_id, &plan),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(100),
            next_frame_past_heartbeat(fixture.socket())
        )
        .await
        .is_err(),
        "removal waits behind the owner mutation fence"
    );
    pause.release();
    mutation
        .await
        .expect("owner mutation joins")
        .expect("owner mutation wins");

    assert_typed_removal_failure(fixture.socket(), "1272", "ownership-conflict").await;
    assert_eq!(remove_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.external.exists());
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_none()
    );
    fixture.shutdown().await;
}

#[derive(Clone, Copy)]
enum RacingOwnerCreation {
    OmittedKind,
    TurnBootstrap,
}

async fn assert_owner_creation_that_wins_invalidates_removal(kind: RacingOwnerCreation) {
    let hooks = TestHooks::default();
    let remove_calls = Arc::new(AtomicUsize::new(0));
    let mut fixture = CatalogRpcFixture::new_with_engine_options_and_removal_git(
        true,
        Arc::new(CountingRemovalGit {
            inner: GitRepository::default(),
            remove_calls: remove_calls.clone(),
        }),
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "creation-winner-adopt").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1273").await;
    let second_root = fixture.root.path().join("creation-winner-project");
    fs::create_dir(&second_root).expect("second project root");
    fixture
        .engine
        .dispatch(
            serde_json::from_value(json!({
                "type":"project.create",
                "commandId":"creation-winner-project-create",
                "projectId":"creation-winner-project",
                "title":"Creation Winner",
                "workspaceRoot":second_root,
                "defaultModelSelection":null,
                "createdAt":"2026-08-10T00:03:00Z"
            }))
            .expect("second project command"),
        )
        .await
        .expect("second project");
    let owner_id = match kind {
        RacingOwnerCreation::OmittedKind => "creation-winner-omitted",
        RacingOwnerCreation::TurnBootstrap => "creation-winner-bootstrap",
    };
    let target = match kind {
        RacingOwnerCreation::OmittedKind => fixture.external.clone(),
        RacingOwnerCreation::TurnBootstrap => {
            let alias_segment = fixture
                .external
                .parent()
                .expect("external parent")
                .join("winner-alias-segment");
            fs::create_dir(&alias_segment).expect("winner alias segment");
            alias_segment
                .join("..")
                .join(fixture.external.file_name().expect("external file name"))
        }
    };
    let command = match kind {
        RacingOwnerCreation::OmittedKind => serde_json::from_value(json!({
            "type":"thread.create",
            "commandId":"creation-winner-omitted-command",
            "threadId":owner_id,
            "projectId":"creation-winner-project",
            "title":"Omitted Kind Owner",
            "modelSelection":{},
            "runtimeMode":"full-access",
            "interactionMode":"default",
            "branch":null,
            "worktreePath":target,
            "createdAt":"2026-08-10T00:03:01Z"
        })),
        RacingOwnerCreation::TurnBootstrap => serde_json::from_value(json!({
            "type":"thread.turn.start",
            "commandId":"creation-winner-bootstrap-command",
            "threadId":owner_id,
            "message":{
                "messageId":"creation-winner-bootstrap-message",
                "role":"user",
                "text":"bootstrap",
                "attachments":[]
            },
            "modelSelection":{},
            "runtimeMode":"full-access",
            "interactionMode":"default",
            "bootstrap":{"createThread":{
                "projectId":"creation-winner-project",
                "title":"Bootstrap Owner",
                "modelSelection":{},
                "runtimeMode":"full-access",
                "interactionMode":"default",
                "branch":null,
                "worktreePath":target,
                "createdAt":"2026-08-10T00:03:01Z"
            }},
            "createdAt":"2026-08-10T00:03:01Z"
        })),
    }
    .expect("owner creation command");

    let pause = hooks.pause_before_next_command_persist();
    let engine = fixture.engine.clone();
    let owner = tokio::spawn(async move { engine.dispatch(command).await });
    pause.wait_until_entered().await;
    request(
        fixture.socket(),
        "1274",
        "worktree.remove",
        removal_payload("creation-winner-remove", "project-1", &thread_id, &plan),
    )
    .await;
    assert!(
        timeout(
            Duration::from_millis(100),
            next_frame_past_heartbeat(fixture.socket())
        )
        .await
        .is_err(),
        "removal must wait behind the owner creation fence"
    );
    assert_eq!(remove_calls.load(Ordering::SeqCst), 0);
    pause.release();
    owner
        .await
        .expect("owner creation joins")
        .expect("owner creation wins");

    assert_typed_removal_failure(fixture.socket(), "1274", "ownership-conflict").await;
    assert_eq!(remove_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.external.exists());
    let owner = fixture
        .repositories
        .get_thread(owner_id.to_owned())
        .await
        .expect("owner read")
        .expect("owner persists after rejected removal");
    assert_eq!(owner.kind, "workspace");
    assert!(owner.deleted_at.is_none());
    fixture.shutdown().await;
}

#[tokio::test]
async fn omitted_kind_owner_creation_that_wins_invalidates_removal_before_git() {
    assert_owner_creation_that_wins_invalidates_removal(RacingOwnerCreation::OmittedKind).await;
}

#[tokio::test]
async fn turn_bootstrap_owner_creation_that_wins_invalidates_removal_before_git() {
    assert_owner_creation_that_wins_invalidates_removal(RacingOwnerCreation::TurnBootstrap).await;
}

#[tokio::test]
async fn removal_dirty_requires_confirmation_and_detach_only_ignores_quiesce_outcome() {
    let quiescer = Arc::new(RecordingPendingQuiescer::default());
    let mut fixture = CatalogRpcFixture::new_with_quiescer(true, quiescer.clone()).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-dirty").await;
    let mut status = fixture
        .status_broadcaster
        .subscribe(fixture.external.clone(), CancellationToken::new())
        .await
        .expect("detached worktree status subscription");
    assert!(matches!(
        status.recv().await,
        Some(VcsStatusStreamEvent::Snapshot { local, .. })
            if !local.has_working_tree_changes
    ));
    fs::write(fixture.external.join("dirty.txt"), "dirty\n").expect("dirty file");
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    assert_eq!(plan["untrackedFileCount"], 1);
    request(
        fixture.socket(),
        "1203",
        "worktree.remove",
        json!({
            "commandId":"remove-dirty",
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"delete-git-worktree",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":false,
            "confirmRepositoryWidePrune":false
        }),
    )
    .await;
    assert_typed_removal_failure(fixture.socket(), "1203", "dirty-confirmation-required").await;
    assert_eq!(quiescer.call_count(), 0);
    assert!(
        fixture
            .repositories
            .get_thread(thread_id.clone())
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_none()
    );

    fs::write(fixture.external.join("drift.txt"), "drift\n").expect("plan drift file");
    request(
        fixture.socket(),
        "1204",
        "worktree.remove",
        json!({
            "commandId":"remove-dirty-drifted",
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
    assert_typed_removal_failure(fixture.socket(), "1204", "stale-plan").await;
    assert_eq!(quiescer.call_count(), 0);

    request(
        fixture.socket(),
        "1205",
        "worktree.removeFromBibCode",
        json!({
            "commandId":"remove-detach-only",
            "projectId":"project-1",
            "threadId":thread_id
        }),
    )
    .await;
    let result = success_value(fixture.socket(), "1205").await;
    assert_eq!(result["gitOutcome"], "not-requested");
    assert_eq!(result["orphanCleanupPending"], true);
    assert!(!quiescer.last_cancellation().is_cancelled());
    assert!(fixture.external.exists());
    timeout(Duration::from_secs(15), async {
        loop {
            if matches!(
                status.recv().await,
                Some(VcsStatusStreamEvent::LocalUpdated { local })
                    if local.has_working_tree_changes
            ) {
                break;
            }
        }
    })
    .await
    .expect("terminal detach notifies the retained worktree status owner");
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_plan_and_execute_use_catalog_trusted_fallback_when_primary_is_missing() {
    let anchors = Arc::new(StdMutex::new(Vec::new()));
    let removal_git = Arc::new(RecordingAnchorRemovalGit {
        inner: GitRepository::default(),
        anchors: anchors.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_removal_git(true, removal_git).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "fallback-anchor-adopt").await;
    let mut project = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists");
    project.workspace_root = fixture
        .root
        .path()
        .join("missing-primary")
        .to_string_lossy()
        .into_owned();
    fixture
        .repositories
        .upsert_project(project)
        .await
        .expect("move durable project root away from the pinned repository");

    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1210").await;
    request(
        fixture.socket(),
        "1211",
        "worktree.remove",
        removal_payload("fallback-anchor-remove", "project-1", &thread_id, &plan),
    )
    .await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1211").await["gitOutcome"],
        "removed"
    );
    let calls = anchors.lock().expect("anchor recording").clone();
    assert!(!calls.is_empty());
    let expected_anchor =
        fs::canonicalize(fixture.main.join(".git")).expect("canonical common dir");
    assert!(
        calls.iter().all(|(_, anchor)| anchor == &expected_anchor),
        "recorded anchors {calls:?}, expected {expected_anchor:?}"
    );
    assert!(calls.iter().any(|(operation, _)| *operation == "inventory"));
    assert!(calls.iter().any(|(operation, _)| *operation == "inspect"));
    assert!(calls.iter().any(|(operation, _)| *operation == "remove"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_reselects_a_same_repository_trusted_anchor_after_quiesce() {
    let anchors = Arc::new(StdMutex::new(Vec::new()));
    let switch_paths = Arc::new(StdMutex::new(None));
    let removal_git = Arc::new(RecordingAnchorRemovalGit {
        inner: GitRepository::default(),
        anchors: anchors.clone(),
    });
    let quiescer = Arc::new(SwitchingAnchorQuiescer {
        paths: switch_paths.clone(),
    });
    let mut fixture =
        CatalogRpcFixture::new_with_removal_services(true, quiescer, Some(removal_git)).await;
    let target_thread = adopt_external_for_removal(&mut fixture, "reselect-target-adopt").await;
    let sibling = fixture.create_named_external_worktree(
        "reselect-trusted-sibling",
        "feature/reselect-trusted-sibling",
    );
    request(
        fixture.socket(),
        "1212",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "1212").await;
    let sibling_descriptor = snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|descriptor| {
            descriptor["path"]
                .as_str()
                .is_some_and(|path| same_worktree_identity(Path::new(path), &sibling))
        })
        .expect("trusted sibling descriptor")
        .clone();
    let sibling_path = sibling_descriptor["path"]
        .as_str()
        .expect("trusted sibling path")
        .to_owned();
    request(
        fixture.socket(),
        "1213",
        "worktree.adopt",
        adoption_payload("reselect-sibling-adopt", &sibling_descriptor, &snapshot),
    )
    .await;
    success_value(fixture.socket(), "1213").await;

    let mut project = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("project read")
        .expect("project exists");
    project.workspace_root = fixture
        .root
        .path()
        .join("reselect-missing-primary")
        .to_string_lossy()
        .into_owned();
    fixture
        .repositories
        .upsert_project(project)
        .await
        .expect("move durable project root away from the pinned repository");
    let hidden_sibling = fixture.root.path().join("reselect-hidden-sibling");
    *switch_paths.lock().expect("switch paths") = Some((sibling.clone(), hidden_sibling));

    let plan = removal_plan(&mut fixture, "project-1", &target_thread, "1214").await;
    let plan_call_count = anchors.lock().expect("plan anchors").len();
    assert!(
        anchors
            .lock()
            .expect("plan anchors")
            .iter()
            .all(|(_, anchor)| anchor == &PathBuf::from(&sibling_path)),
        "planning must use the present same-repository sibling"
    );
    request(
        fixture.socket(),
        "1215",
        "worktree.remove",
        removal_payload("reselect-anchor-remove", "project-1", &target_thread, &plan),
    )
    .await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1215").await["gitOutcome"],
        "removed"
    );
    let common_dir = fs::canonicalize(fixture.main.join(".git")).expect("canonical common dir");
    let calls = anchors.lock().expect("mutation anchors").clone();
    assert!(calls.len() > plan_call_count);
    assert!(
        calls
            .iter()
            .skip(plan_call_count)
            .any(|(operation, anchor)| *operation == "remove" && anchor == &common_dir),
        "the Git mutation must use the anchor reselected after quiesce: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|(operation, anchor)| *operation != "remove" || anchor == &common_dir),
        "no destructive Git call may use the now-missing pre-quiesce anchor: {calls:?}"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_after_git_succeeded_before_detach_is_safe_missing_unregistered_cleanup() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-crash").await;
    git(
        &fixture.main,
        &["worktree", "remove", "--force", "--"],
        Some(&fixture.external),
    );
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    assert_eq!(plan["availability"], "missing-unregistered");
    request(
        fixture.socket(),
        "1203",
        "worktree.remove",
        json!({
            "commandId":"remove-after-crash",
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"cleanup-stale-registration",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":false,
            "confirmRepositoryWidePrune":false
        }),
    )
    .await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1203").await["gitOutcome"],
        "cleaned"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn reserved_removal_resumes_after_git_succeeded_before_detach() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-reserved-crash").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1270").await;
    let payload = removal_payload("remove-reserved-crash", "project-1", &thread_id, &plan);
    fixture
        .repositories
        .reserve_command_receipt(CommandReceipt {
            command_id: "remove-reserved-crash".to_owned(),
            aggregate_kind: "project".to_owned(),
            aggregate_id: "project-1".to_owned(),
            accepted_at: "2026-08-09T00:02:00Z".to_owned(),
            result_sequence: 0,
            status: "prepared".to_owned(),
            error: None,
            payload_digest: Some(canonical_command_digest(&payload).expect("payload digest")),
        })
        .await
        .expect("durable removal reservation");
    git(
        &fixture.main,
        &["worktree", "remove"],
        Some(&fixture.external),
    );

    request(fixture.socket(), "1271", "worktree.remove", payload.clone()).await;
    let result = removal_success_value(fixture.socket(), "1271").await;
    assert_eq!(result["gitOutcome"], "removed");
    assert_eq!(result["threadRemoved"], true);
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_some()
    );

    request(fixture.socket(), "1272", "worktree.remove", payload).await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1272").await,
        result
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn successful_removal_retry_preserves_a_replacement_at_the_same_path() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id =
        adopt_external_for_removal(&mut fixture, "remove-adopt-replacement-retry").await;
    let plan = removal_plan(&mut fixture, "project-1", &thread_id, "1273").await;
    let payload = removal_payload("remove-replacement-retry", "project-1", &thread_id, &plan);

    request(fixture.socket(), "1274", "worktree.remove", payload.clone()).await;
    let result = removal_success_value(fixture.socket(), "1274").await;
    assert_eq!(result["gitOutcome"], "removed");
    assert!(!fixture.external.exists(), "original worktree was removed");

    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/replacement-after-removal"],
        Some(&fixture.external),
    );
    let replacement = fixture.external.join("replacement-sentinel.txt");
    fs::write(&replacement, "replacement must survive\n").expect("replacement sentinel");

    request(fixture.socket(), "1275", "worktree.remove", payload).await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1275").await,
        result
    );
    assert_eq!(
        fs::read_to_string(&replacement).expect("stale retry preserves replacement"),
        "replacement must survive\n"
    );
    assert!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"])
            .replace('\\', "/")
            .contains(&fixture.external.to_string_lossy().replace('\\', "/")),
        "replacement remains registered as a Git worktree"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_missing_registration_targeted_cleanup_succeeds_without_repository_prune() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-missing").await;
    fs::remove_dir_all(&fixture.external).expect("simulate missing worktree directory");
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    assert_eq!(plan["availability"], "missing-registered");
    assert!(
        plan["pruneImpact"]
            .as_array()
            .is_some_and(|impact| !impact.is_empty())
    );
    let payload = json!({
        "commandId":"remove-missing-unconfirmed",
        "projectId":"project-1",
        "threadId":thread_id,
        "mode":"cleanup-stale-registration",
        "expectedGeneration":plan["generation"],
        "planToken":plan["planToken"],
        "forceDirty":false,
        "confirmRepositoryWidePrune":false
    });
    request(fixture.socket(), "1203", "worktree.remove", payload).await;
    let result = removal_success_value(fixture.socket(), "1203").await;
    assert_eq!(result["gitOutcome"], "cleaned");
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_some()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_missing_fallback_prune_requires_confirmation_then_cleans() {
    let mut fixture = CatalogRpcFixture::new_with_removal_git(
        true,
        Arc::new(TargetedFailingGit {
            inner: GitRepository::default(),
        }),
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-prune").await;
    fs::remove_dir_all(&fixture.external).expect("simulate missing worktree directory");
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    let mut payload = json!({
        "commandId":"remove-prune-unconfirmed",
        "projectId":"project-1",
        "threadId":thread_id,
        "mode":"cleanup-stale-registration",
        "expectedGeneration":plan["generation"],
        "planToken":plan["planToken"],
        "forceDirty":false,
        "confirmRepositoryWidePrune":false
    });
    request(fixture.socket(), "1203", "worktree.remove", payload.clone()).await;
    assert_typed_removal_failure(fixture.socket(), "1203", "prune-confirmation-required").await;
    payload["commandId"] = json!("remove-prune-confirmed");
    payload["confirmRepositoryWidePrune"] = json!(true);
    request(fixture.socket(), "1204", "worktree.remove", payload).await;
    assert_eq!(
        removal_success_value(fixture.socket(), "1204").await["gitOutcome"],
        "cleaned"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_locked_missing_registration_is_protected_but_detach_only_remains_available() {
    let mut fixture = CatalogRpcFixture::new(true).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-locked").await;
    git(
        &fixture.main,
        &["worktree", "lock", "--reason", "keep", "--"],
        Some(&fixture.external),
    );
    fs::remove_dir_all(&fixture.external).expect("simulate missing locked worktree directory");
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    assert_eq!(plan["locked"], true);
    request(
        fixture.socket(),
        "1203",
        "worktree.remove",
        json!({
            "commandId":"remove-locked",
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"cleanup-stale-registration",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":false,
            "confirmRepositoryWidePrune":true
        }),
    )
    .await;
    assert_typed_removal_failure(fixture.socket(), "1203", "locked").await;
    request(
        fixture.socket(),
        "1204",
        "worktree.removeFromBibCode",
        json!({"commandId":"detach-locked","projectId":"project-1","threadId":thread_id}),
    )
    .await;
    assert_eq!(
        success_value(fixture.socket(), "1204").await["gitOutcome"],
        "not-requested"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_present_verified_mutation_failure_keeps_thread_and_worktree() {
    let mut fixture = CatalogRpcFixture::new_with_removal_git(
        true,
        Arc::new(FailingMutationGit {
            inner: GitRepository::default(),
        }),
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-fail-present").await;
    let mut status = fixture
        .status_broadcaster
        .subscribe(fixture.external.clone(), CancellationToken::new())
        .await
        .expect("failing removal status subscription");
    assert!(matches!(
        status.recv().await,
        Some(VcsStatusStreamEvent::Snapshot { local, .. })
            if !local.has_working_tree_changes
    ));
    fs::write(fixture.external.join("dirty.txt"), "dirty before removal\n")
        .expect("dirty failing removal fixture");
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    request(
        fixture.socket(),
        "1203",
        "worktree.remove",
        json!({
            "commandId":"remove-fail-present",
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
    assert_typed_removal_failure(fixture.socket(), "1203", "git-failed").await;
    timeout(Duration::from_secs(15), async {
        loop {
            if matches!(
                status.recv().await,
                Some(VcsStatusStreamEvent::LocalUpdated { local })
                    if local.has_working_tree_changes
            ) {
                break;
            }
        }
    })
    .await
    .expect("terminal Git failure still fences and refreshes the status owner");
    assert!(fixture.external.exists());
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_none()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_duplicate_canonical_owner_conflicts_before_git_mutation() {
    let remove_calls = Arc::new(AtomicUsize::new(0));
    let git = Arc::new(CountingRemovalGit {
        inner: GitRepository::default(),
        remove_calls: remove_calls.clone(),
    });
    let mut fixture = CatalogRpcFixture::new_with_removal_git(true, git).await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-owner-conflict").await;
    request(
        fixture.socket(),
        "1250",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1250").await;
    let mut duplicate = fixture
        .repositories
        .get_thread(thread_id.clone())
        .await
        .expect("owner read")
        .expect("canonical owner");
    duplicate.thread_id = "duplicate-workspace-owner".to_owned();
    duplicate.created_at = "2026-08-09T00:00:03Z".to_owned();
    fixture
        .repositories
        .upsert_thread(duplicate)
        .await
        .expect("duplicate owner projection");

    request(
        fixture.socket(),
        "1251",
        "worktree.remove",
        json!({
            "commandId":"remove-owner-conflict",
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"delete-git-worktree",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":false,
            "confirmRepositoryWidePrune":false
        }),
    )
    .await;

    assert_typed_removal_failure(fixture.socket(), "1251", "ownership-conflict").await;
    assert_eq!(remove_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.external.exists());
    for owner in [&thread_id, "duplicate-workspace-owner"] {
        assert!(
            fixture
                .repositories
                .get_thread(owner.to_owned())
                .await
                .expect("owner read")
                .expect("owner projection")
                .deleted_at
                .is_none()
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn removal_missing_targeted_and_prune_failure_detaches_with_bounded_failed_outcome() {
    let mut fixture = CatalogRpcFixture::new_with_removal_git(
        true,
        Arc::new(FailingMutationGit {
            inner: GitRepository::default(),
        }),
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "remove-adopt-fail-missing").await;
    fs::remove_dir_all(&fixture.external).expect("simulate missing worktree");
    request(
        fixture.socket(),
        "1202",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1202").await;
    request(
        fixture.socket(),
        "1203",
        "worktree.remove",
        json!({
            "commandId":"remove-fail-missing",
            "projectId":"project-1",
            "threadId":thread_id,
            "mode":"cleanup-stale-registration",
            "expectedGeneration":plan["generation"],
            "planToken":plan["planToken"],
            "forceDirty":false,
            "confirmRepositoryWidePrune":true
        }),
    )
    .await;
    let result = removal_success_value(fixture.socket(), "1203").await;
    assert_eq!(result["gitOutcome"], "failed");
    assert!(
        result["detail"]
            .as_str()
            .is_some_and(|detail| detail.len() <= 2_048)
    );
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_some()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn cleanup_capacity_rejection_has_no_removal_side_effects() {
    let remove_calls = Arc::new(AtomicUsize::new(0));
    let quiesce_calls = Arc::new(AtomicUsize::new(0));
    let mut fixture = CatalogRpcFixture::new_with_removal_services(
        true,
        Arc::new(CapacityRejectingQuiescer {
            quiesce_calls: quiesce_calls.clone(),
        }),
        Some(Arc::new(CountingRemovalGit {
            inner: GitRepository::default(),
            remove_calls: remove_calls.clone(),
        })),
    )
    .await;
    let thread_id = adopt_external_for_removal(&mut fixture, "capacity-adopt").await;
    request(
        fixture.socket(),
        "1204",
        "worktree.getRemovalPlan",
        json!({"projectId":"project-1","threadId":thread_id}),
    )
    .await;
    let plan = success_value(fixture.socket(), "1204").await;
    request(
        fixture.socket(),
        "1205",
        "worktree.remove",
        removal_payload("capacity-remove", "project-1", &thread_id, &plan),
    )
    .await;
    assert_typed_removal_failure(fixture.socket(), "1205", "cleanup-capacity").await;

    assert_eq!(remove_calls.load(Ordering::SeqCst), 0);
    assert_eq!(quiesce_calls.load(Ordering::SeqCst), 0);
    assert!(fixture.external.exists());
    assert!(
        fixture
            .repositories
            .get_command_receipt("capacity-remove".to_owned())
            .await
            .expect("receipt read")
            .is_none(),
        "capacity rejection must happen before durable reservation"
    );
    assert!(
        fixture
            .repositories
            .get_thread(thread_id)
            .await
            .expect("thread read")
            .expect("thread")
            .deleted_at
            .is_none()
    );
    fixture.shutdown().await;
}

struct CapacityRejectingQuiescer {
    quiesce_calls: Arc<AtomicUsize>,
}

impl WorktreeRemovalQuiescer for CapacityRejectingQuiescer {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> bibcode_server::production::worktree_catalog_rpc::WorktreeRemovalLiveSessionsFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn admit_cleanup(&self) -> WorktreeRemovalCleanupAdmissionFuture {
        Box::pin(async { Err(WorktreeRemovalCleanupAdmissionError::Capacity) })
    }

    fn quiesce(
        &self,
        _admission: WorktreeRemovalCleanupAdmission,
        _request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        self.quiesce_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { WorktreeRemovalQuiesceLease::complete() })
    }
}

#[derive(Default)]
struct RecordingPendingQuiescer {
    cancellations: std::sync::Mutex<Vec<CancellationToken>>,
}

impl RecordingPendingQuiescer {
    fn call_count(&self) -> usize {
        self.cancellations.lock().expect("cancellation lock").len()
    }

    fn last_cancellation(&self) -> CancellationToken {
        self.cancellations
            .lock()
            .expect("cancellation lock")
            .last()
            .expect("quiesce cancellation")
            .clone()
    }
}

impl WorktreeRemovalQuiescer for RecordingPendingQuiescer {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> bibcode_server::production::worktree_catalog_rpc::WorktreeRemovalLiveSessionsFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn quiesce(
        &self,
        _admission: WorktreeRemovalCleanupAdmission,
        _request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        let cancellation = CancellationToken::new();
        self.cancellations
            .lock()
            .expect("cancellation lock")
            .push(cancellation.clone());
        Box::pin(async { WorktreeRemovalQuiesceLease::pending(cancellation) })
    }
}

struct CatalogRpcFixture {
    _parallelism_permit: OwnedSemaphorePermit,
    root: TempDir,
    main: PathBuf,
    external: PathBuf,
    repositories: Repositories,
    catalog: WorktreeCatalogService,
    status_broadcaster: StatusBroadcaster,
    availability: WorkspaceAvailabilityRegistry,
    operations: WorktreeCatalogOperationRuntime,
    engine: OrchestrationEngine,
    handle: Option<bibcode_server::ServerHandle>,
    socket: Option<TestSocket>,
}

impl CatalogRpcFixture {
    async fn new(with_external: bool) -> Self {
        Self::new_with_removal_services(with_external, Arc::new(TestNoopQuiescer), None).await
    }

    async fn new_with_quiescer(
        with_external: bool,
        quiescer: Arc<dyn WorktreeRemovalQuiescer>,
    ) -> Self {
        Self::new_with_removal_services(with_external, quiescer, None).await
    }

    async fn new_with_removal_git(with_external: bool, git: Arc<dyn WorktreeRemovalGit>) -> Self {
        Self::new_with_removal_services(with_external, Arc::new(TestNoopQuiescer), Some(git)).await
    }

    async fn new_with_engine_options_and_removal_git(
        with_external: bool,
        git: Arc<dyn WorktreeRemovalGit>,
        options: EngineOptions,
    ) -> Self {
        Self::new_with_removal_services_and_options(
            with_external,
            Arc::new(TestNoopQuiescer),
            Some(git),
            options,
        )
        .await
    }

    async fn new_with_removal_services(
        with_external: bool,
        quiescer: Arc<dyn WorktreeRemovalQuiescer>,
        removal_git: Option<Arc<dyn WorktreeRemovalGit>>,
    ) -> Self {
        Self::new_with_removal_services_and_options(
            with_external,
            quiescer,
            removal_git,
            EngineOptions::default(),
        )
        .await
    }

    async fn new_with_removal_services_and_options(
        with_external: bool,
        quiescer: Arc<dyn WorktreeRemovalQuiescer>,
        removal_git: Option<Arc<dyn WorktreeRemovalGit>>,
        engine_options: EngineOptions,
    ) -> Self {
        let parallelism_permit = catalog_rpc_fixture_parallelism()
            .acquire_owned()
            .await
            .expect("catalog RPC fixture parallelism remains open");
        let root = tempfile::tempdir().expect("fixture root");
        let main = root.path().join("main");
        let external = root.path().join("external");
        fs::create_dir(&main).expect("primary directory");
        git(&main, &["init", "--initial-branch", "main"], None);
        git(
            &main,
            &["config", "user.email", "rpc@example.invalid"],
            None,
        );
        git(&main, &["config", "user.name", "RPC Test"], None);
        fs::write(main.join("README.md"), "rpc fixture\n").expect("fixture file");
        git(&main, &["add", "README.md"], None);
        git(&main, &["commit", "-m", "initial"], None);
        if with_external {
            git(
                &main,
                &["worktree", "add", "-b", "feature/external"],
                Some(&external),
            );
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, engine_options)
            .await
            .expect("orchestration");
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type": "project.create",
                    "commandId": "project-create",
                    "projectId": "project-1",
                    "title": "Catalog RPC",
                    "workspaceRoot": main,
                    "defaultModelSelection": null,
                    "createdAt": "2026-08-09T00:00:00Z"
                }))
                .expect("project command"),
            )
            .await
            .expect("project created");
        let repositories = engine.repositories();
        let git_repository = Arc::new(GitRepository::default());
        let status_broadcaster =
            StatusBroadcaster::new(git_repository.clone(), Duration::from_secs(3_600), 8);
        let availability = WorkspaceAvailabilityRegistry::new();
        let catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            git_repository.clone(),
            availability.clone(),
        );
        let mut registry = RpcRegistry::empty();
        let removal_services = WorktreeCatalogRpcServices::new(catalog.clone(), engine.clone())
            .with_status_broadcaster(status_broadcaster.clone())
            .with_removal_quiescer(quiescer);
        let removal_services = removal_git.map_or(removal_services.clone(), |git| {
            removal_services.with_removal_git(git)
        });
        let operations = removal_services.operation_runtime();
        register_worktree_catalog_rpc(&mut registry, removal_services);
        register_orchestration_rpc(&mut registry, engine.clone());
        register_git_vcs_rpc(
            &mut registry,
            GitVcsRpcServices::with_repository(git_repository.clone(), Arc::default()),
        );
        let server_state = root.path().join("server");
        let config = ServerConfig::new(server_state)
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
            _parallelism_permit: parallelism_permit,
            root,
            main,
            external,
            repositories,
            catalog,
            status_broadcaster,
            availability,
            operations,
            engine,
            handle: Some(handle),
            socket: Some(socket),
        }
    }

    fn socket(&mut self) -> &mut TestSocket {
        self.socket.as_mut().expect("active socket")
    }

    fn create_external_worktree(&self) {
        git(
            &self.main,
            &["worktree", "add", "-b", "feature/external"],
            Some(&self.external),
        );
    }

    async fn create_project(&self, project_id: &str, workspace_root: PathBuf) {
        self.repositories
            .upsert_project(ProjectionProject {
                project_id: project_id.to_owned(),
                title: project_id.to_owned(),
                workspace_root: workspace_root.to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([]),
                worktree_discovery: json!({}),
                worktree_repository_key: None,
                created_at: "2026-08-09T00:00:01Z".to_owned(),
                updated_at: "2026-08-09T00:00:01Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("project projection created");
    }

    async fn create_project_with_thread(
        &self,
        project_id: &str,
        workspace_root: PathBuf,
        worktree_path: PathBuf,
    ) {
        self.create_project(project_id, workspace_root).await;
        self.repositories
            .upsert_thread(ProjectionThread {
                thread_id: format!("thread-{project_id}"),
                project_id: project_id.to_owned(),
                title: project_id.to_owned(),
                kind: "default".to_owned(),
                model_selection: json!({"instanceId":"codex","model":"gpt-5.4"}),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                branch: Some("feature/removed-target".to_owned()),
                worktree_path: Some(worktree_path.to_string_lossy().into_owned()),
                latest_turn_id: None,
                created_at: "2026-08-09T00:00:02Z".to_owned(),
                updated_at: "2026-08-09T00:00:02Z".to_owned(),
                archived_at: None,
                latest_user_message_at: None,
                pending_approval_count: 0,
                pending_user_input_count: 0,
                has_actionable_proposed_plan: 0,
                unresolved_delivery_state: None,
                unresolved_delivery_detail: None,
                deleted_at: None,
                host_thread_id: None,
            })
            .await
            .expect("project thread projection created");
    }

    fn create_named_external_worktree(&self, directory: &str, branch: &str) -> PathBuf {
        let path = self.root.path().join(directory);
        git(&self.main, &["worktree", "add", "-b", branch], Some(&path));
        path
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

fn catalog_rpc_fixture_parallelism() -> Arc<Semaphore> {
    const MAX_PARALLEL_FIXTURES: usize = 4;
    static PARALLELISM: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(PARALLELISM.get_or_init(|| Arc::new(Semaphore::new(MAX_PARALLEL_FIXTURES))))
}

struct TestNoopQuiescer;

impl WorktreeRemovalQuiescer for TestNoopQuiescer {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> bibcode_server::production::worktree_catalog_rpc::WorktreeRemovalLiveSessionsFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn quiesce(
        &self,
        _admission: WorktreeRemovalCleanupAdmission,
        _request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        Box::pin(async { WorktreeRemovalQuiesceLease::complete() })
    }
}

struct SwitchingAnchorQuiescer {
    paths: Arc<StdMutex<Option<(PathBuf, PathBuf)>>>,
}

impl WorktreeRemovalQuiescer for SwitchingAnchorQuiescer {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> bibcode_server::production::worktree_catalog_rpc::WorktreeRemovalLiveSessionsFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn quiesce(
        &self,
        _admission: WorktreeRemovalCleanupAdmission,
        _request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        let paths = self.paths.clone();
        Box::pin(async move {
            let (visible, hidden) = paths
                .lock()
                .expect("switch paths")
                .take()
                .expect("configured anchor switch");
            fs::rename(&visible, &hidden).expect("make the pre-quiesce anchor unavailable");
            WorktreeRemovalQuiesceLease::complete()
        })
    }
}

struct RecordingAnchorRemovalGit {
    inner: GitRepository,
    anchors: Arc<StdMutex<Vec<(&'static str, PathBuf)>>>,
}

#[derive(Clone, Default)]
struct ImmediateFilesystemRemovalGit {
    inventory_calls: Arc<AtomicUsize>,
    inspect_calls: Arc<AtomicUsize>,
    remove_calls: Arc<AtomicUsize>,
}

impl ImmediateFilesystemRemovalGit {
    fn call_counts(&self) -> (usize, usize, usize) {
        (
            self.inventory_calls.load(Ordering::SeqCst),
            self.inspect_calls.load(Ordering::SeqCst),
            self.remove_calls.load(Ordering::SeqCst),
        )
    }
}

impl WorktreeRemovalGit for ImmediateFilesystemRemovalGit {
    fn inventory(
        &self,
        anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        self.inventory_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let external = anchor
                .parent()
                .expect("test repository parent")
                .join("external");
            Ok(GitWorktreeInventory {
                common_dir: anchor.join(".git"),
                records: vec![
                    GitWorktreeRecord {
                        path: anchor,
                        head: None,
                        branch: Some("refs/heads/main".to_owned()),
                        is_primary: true,
                        is_bare: false,
                        locked: false,
                        lock_reason: None,
                        is_prunable: false,
                        prunable_reason: None,
                    },
                    GitWorktreeRecord {
                        path: external,
                        head: None,
                        branch: Some("refs/heads/feature/external".to_owned()),
                        is_primary: false,
                        is_bare: false,
                        locked: false,
                        lock_reason: None,
                        is_prunable: false,
                        prunable_reason: None,
                    },
                ],
                nul_delimited: true,
            })
        })
    }

    fn inspect(
        &self,
        _anchor: PathBuf,
        _record: GitWorktreeRecord,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        self.inspect_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(GitWorktreeRemovalInspection::default()) })
    }

    fn preview_prune(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn remove(
        &self,
        _anchor: PathBuf,
        record: GitWorktreeRecord,
        _force_dirty: bool,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        self.remove_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            tokio::fs::remove_dir_all(&record.path)
                .await
                .map_err(|error| GitCommandError {
                    tag: "GitCommandError",
                    operation: "test.removeWorktree".into(),
                    command: "test filesystem removal".into(),
                    cwd: record.path.to_string_lossy().into_owned().into(),
                    diagnostics: None,
                    detail: error.to_string().into(),
                })
        })
    }

    fn prune(
        &self,
        _anchor: PathBuf,
        _record: GitWorktreeRecord,
        _expected_impact_digest: String,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        Box::pin(async {
            Err(GitCommandError {
                tag: "GitCommandError",
                operation: "test.pruneWorktrees".into(),
                command: "unexpected test prune".into(),
                cwd: "<test>".into(),
                diagnostics: None,
                detail: "the immediate removal fixture does not support pruning".into(),
            })
        })
    }
}

impl RecordingAnchorRemovalGit {
    fn record(&self, operation: &'static str, anchor: &Path) {
        self.anchors
            .lock()
            .expect("anchor recording")
            .push((operation, anchor.to_path_buf()));
    }
}

impl WorktreeRemovalGit for RecordingAnchorRemovalGit {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        self.record("inventory", &anchor);
        WorktreeRemovalGit::inventory(&self.inner, anchor, cancellation)
    }

    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        self.record("inspect", &anchor);
        WorktreeRemovalGit::inspect(&self.inner, anchor, record, cancellation)
    }

    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        self.record("preview-prune", &anchor);
        WorktreeRemovalGit::preview_prune(&self.inner, anchor, cancellation)
    }

    fn remove(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        force_dirty: bool,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        self.record("remove", &anchor);
        WorktreeRemovalGit::remove(&self.inner, anchor, record, force_dirty, cancellation)
    }

    fn prune(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        expected_impact_digest: String,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        self.record("prune", &anchor);
        WorktreeRemovalGit::prune(
            &self.inner,
            anchor,
            record,
            expected_impact_digest,
            cancellation,
        )
    }
}

#[derive(Clone)]
struct BlockingRemovalGit {
    inner: GitRepository,
    entered: mpsc::UnboundedSender<PathBuf>,
    release: Arc<Semaphore>,
}

impl WorktreeRemovalGit for BlockingRemovalGit {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        WorktreeRemovalGit::inventory(&self.inner, anchor, cancellation)
    }

    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        WorktreeRemovalGit::inspect(&self.inner, anchor, record, cancellation)
    }

    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        WorktreeRemovalGit::preview_prune(&self.inner, anchor, cancellation)
    }

    fn remove(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        force_dirty: bool,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        let inner = self.inner.clone();
        let entered = self.entered.clone();
        let release = self.release.clone();
        Box::pin(async move {
            entered.send(anchor.clone()).expect("Git boundary receiver");
            let permit = release.acquire().await.expect("Git boundary release");
            permit.forget();
            WorktreeRemovalGit::remove(&inner, anchor, record, force_dirty, cancellation).await
        })
    }

    fn prune(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        expected_impact_digest: String,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        WorktreeRemovalGit::prune(
            &self.inner,
            anchor,
            record,
            expected_impact_digest,
            cancellation,
        )
    }
}

#[derive(Clone)]
struct CountingRemovalGit {
    inner: GitRepository,
    remove_calls: Arc<AtomicUsize>,
}

impl WorktreeRemovalGit for CountingRemovalGit {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        WorktreeRemovalGit::inventory(&self.inner, anchor, cancellation)
    }

    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        WorktreeRemovalGit::inspect(&self.inner, anchor, record, cancellation)
    }

    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        WorktreeRemovalGit::preview_prune(&self.inner, anchor, cancellation)
    }

    fn remove(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        force_dirty: bool,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        self.remove_calls.fetch_add(1, Ordering::SeqCst);
        WorktreeRemovalGit::remove(&self.inner, anchor, record, force_dirty, cancellation)
    }

    fn prune(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        expected_impact_digest: String,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        WorktreeRemovalGit::prune(
            &self.inner,
            anchor,
            record,
            expected_impact_digest,
            cancellation,
        )
    }
}

#[derive(Clone)]
struct FailingMutationGit {
    inner: GitRepository,
}

impl FailingMutationGit {
    fn error(operation: &str) -> GitCommandError {
        GitCommandError {
            tag: "GitCommandError",
            operation: operation.into(),
            command: "git worktree mutation".into(),
            cwd: "<server-resolved>".into(),
            diagnostics: None,
            detail: "Git reported success but the exact registration survived verification.".into(),
        }
    }
}

impl WorktreeRemovalGit for FailingMutationGit {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        WorktreeRemovalGit::inventory(&self.inner, anchor, cancellation)
    }

    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        WorktreeRemovalGit::inspect(&self.inner, anchor, record, cancellation)
    }

    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        WorktreeRemovalGit::preview_prune(&self.inner, anchor, cancellation)
    }

    fn remove(
        &self,
        _anchor: PathBuf,
        _record: GitWorktreeRecord,
        _force_dirty: bool,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        Box::pin(async { Err(Self::error("GitVcsDriver.removeWorktreeVerified.verify")) })
    }

    fn prune(
        &self,
        _anchor: PathBuf,
        _record: GitWorktreeRecord,
        _expected_impact_digest: String,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        Box::pin(async { Err(Self::error("GitVcsDriver.pruneWorktreesVerified.verify")) })
    }
}

#[derive(Clone)]
struct TargetedFailingGit {
    inner: GitRepository,
}

impl WorktreeRemovalGit for TargetedFailingGit {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        WorktreeRemovalGit::inventory(&self.inner, anchor, cancellation)
    }

    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        WorktreeRemovalGit::inspect(&self.inner, anchor, record, cancellation)
    }

    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        WorktreeRemovalGit::preview_prune(&self.inner, anchor, cancellation)
    }

    fn remove(
        &self,
        _anchor: PathBuf,
        _record: GitWorktreeRecord,
        _force_dirty: bool,
        _cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        Box::pin(async { Err(FailingMutationGit::error("targeted-cleanup")) })
    }

    fn prune(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        expected_impact_digest: String,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        WorktreeRemovalGit::prune(
            &self.inner,
            anchor,
            record,
            expected_impact_digest,
            cancellation,
        )
    }
}

impl Drop for CatalogRpcFixture {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.shutdown();
        }
        let _ = &self.root;
    }
}

async fn request(socket: &mut TestSocket, id: &str, tag: &str, payload: Value) {
    send_json(
        socket,
        json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] }),
    )
    .await;
}

async fn ack(socket: &mut TestSocket, request_id: &str) {
    send_json(socket, json!({ "_tag": "Ack", "requestId": request_id })).await;
}

async fn send_json(socket: &mut TestSocket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send message");
}

async fn next_server_message(socket: &mut TestSocket) -> ServerMessage {
    next_server_message_with_deadline(socket, REAL_GIT_RPC_RESPONSE_DEADLINE).await
}

async fn next_pure_server_message(socket: &mut TestSocket) -> ServerMessage {
    next_server_message_with_deadline(socket, PURE_RPC_RESPONSE_DEADLINE).await
}

async fn next_managed_worktree_rollback_message(socket: &mut TestSocket) -> ServerMessage {
    next_server_message_with_deadline(socket, MANAGED_WORKTREE_ROLLBACK_INTEGRATION_DEADLINE).await
}

async fn next_worktree_removal_message(socket: &mut TestSocket) -> ServerMessage {
    next_server_message_with_deadline(socket, WORKTREE_REMOVAL_INTEGRATION_DEADLINE).await
}

async fn next_server_message_with_deadline(
    socket: &mut TestSocket,
    deadline: Duration,
) -> ServerMessage {
    let message = timeout(deadline, next_frame_past_heartbeat(socket))
        .await
        .expect("server response timeout")
        .expect("WebSocket open")
        .expect("WebSocket frame");
    let Message::Text(text) = message else {
        panic!("expected text frame: {message:?}");
    };
    serde_json::from_str(&text).expect("server message")
}

async fn next_chunk(socket: &mut TestSocket, request_id: &str) -> Value {
    let message = next_server_message(socket).await;
    let ServerMessage::Chunk {
        request_id: actual,
        values,
    } = message
    else {
        panic!("expected chunk: {message:?}");
    };
    assert_eq!(actual.as_str(), request_id);
    values.into_iter().next().expect("chunk value")
}

async fn success_value(socket: &mut TestSocket, request_id: &str) -> Value {
    success_value_with_deadline(socket, request_id, REAL_GIT_RPC_RESPONSE_DEADLINE).await
}

async fn removal_success_value(socket: &mut TestSocket, request_id: &str) -> Value {
    success_value_with_deadline(socket, request_id, WORKTREE_REMOVAL_INTEGRATION_DEADLINE).await
}

async fn success_value_with_deadline(
    socket: &mut TestSocket,
    request_id: &str,
    deadline: Duration,
) -> Value {
    let message = next_server_message_with_deadline(socket, deadline).await;
    let ServerMessage::Exit {
        request_id: actual,
        exit: RpcExit::Success { value: Some(value) },
    } = message
    else {
        panic!("expected unary success: {message:?}");
    };
    assert_eq!(actual.as_str(), request_id);
    value
}

async fn wait_for_accepted_receipt(repositories: &Repositories, command_id: &str) {
    timeout(Duration::from_secs(5), async {
        loop {
            if repositories
                .get_command_receipt(command_id.to_owned())
                .await
                .expect("receipt read")
                .is_some_and(|receipt| receipt.status == "accepted")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("command reaches a terminal durable receipt");
}

async fn assert_typed_removal_failure(
    socket: &mut TestSocket,
    request_id: &str,
    expected_reason: &str,
) {
    let message = next_worktree_removal_message(socket).await;
    let ServerMessage::Exit {
        request_id: actual,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected typed removal failure: {message:?}");
    };
    assert_eq!(actual.as_str(), request_id);
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error }
            if error["_tag"] == "WorktreeRemovalError" && error["reason"] == expected_reason
    )));
}

async fn wait_for_worktree_removal_git_boundary<T>(entered: &mut mpsc::UnboundedReceiver<T>) -> T {
    timeout(WORKTREE_REMOVAL_INTEGRATION_DEADLINE, entered.recv())
        .await
        .expect("removal reaches Git")
        .expect("Git boundary")
}

async fn adopt_external_for_removal(fixture: &mut CatalogRpcFixture, command_id: &str) -> String {
    request(
        fixture.socket(),
        "1200",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1"}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), "1200").await;
    let candidate = eligible_candidate(&snapshot).clone();
    request(
        fixture.socket(),
        "1201",
        "worktree.adopt",
        adoption_payload(command_id, &candidate, &snapshot),
    )
    .await;
    success_value(fixture.socket(), "1201").await["threadId"]
        .as_str()
        .expect("adopted thread ID")
        .to_owned()
}

async fn adopt_project_worktree_for_removal(
    fixture: &mut CatalogRpcFixture,
    project_id: &str,
    command_id: &str,
    request_id: &str,
) -> String {
    let adopt_request_id = (request_id.parse::<u64>().expect("numeric request ID") + 1).to_string();
    request(
        fixture.socket(),
        request_id,
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":project_id}),
    )
    .await;
    let snapshot = success_value(fixture.socket(), request_id).await;
    let candidate = eligible_candidate(&snapshot).clone();
    let mut payload = adoption_payload(command_id, &candidate, &snapshot);
    payload["projectId"] = json!(project_id);
    request(
        fixture.socket(),
        &adopt_request_id,
        "worktree.adopt",
        payload,
    )
    .await;
    success_value(fixture.socket(), &adopt_request_id).await["threadId"]
        .as_str()
        .expect("adopted thread ID")
        .to_owned()
}

async fn removal_plan(
    fixture: &mut CatalogRpcFixture,
    project_id: &str,
    thread_id: &str,
    request_id: &str,
) -> Value {
    request(
        fixture.socket(),
        request_id,
        "worktree.getRemovalPlan",
        json!({"projectId":project_id,"threadId":thread_id}),
    )
    .await;
    success_value(fixture.socket(), request_id).await
}

fn removal_payload(command_id: &str, project_id: &str, thread_id: &str, plan: &Value) -> Value {
    json!({
        "commandId":command_id,
        "projectId":project_id,
        "threadId":thread_id,
        "mode":"delete-git-worktree",
        "expectedGeneration":plan["generation"],
        "planToken":plan["planToken"],
        "forceDirty":false,
        "confirmRepositoryWidePrune":false
    })
}

fn removal_failure_has_reason(cause: &[CauseItem], expected_reason: &str) -> bool {
    cause.iter().any(|item| {
        matches!(
            item,
            CauseItem::Fail { error }
                if error["_tag"] == "WorktreeRemovalError" && error["reason"] == expected_reason
        )
    })
}

async fn assert_typed_catalog_failure(
    socket: &mut TestSocket,
    request_id: &str,
    expected_reason: &str,
) {
    let message = next_pure_server_message(socket).await;
    let ServerMessage::Exit {
        request_id: actual,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected typed failure: {message:?}");
    };
    assert_eq!(actual.as_str(), request_id);
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error }
            if error["_tag"] == "WorktreeCatalogError"
                && error["reason"] == expected_reason
    )));
}

async fn assert_typed_adoption_failure(
    socket: &mut TestSocket,
    request_id: &str,
    expected_reason: &str,
) {
    let message = next_pure_server_message(socket).await;
    let ServerMessage::Exit {
        request_id: actual,
        exit: RpcExit::Failure { cause },
    } = message
    else {
        panic!("expected typed adoption failure: {message:?}");
    };
    assert_eq!(actual.as_str(), request_id);
    assert!(cause.iter().any(|item| matches!(
        item,
        CauseItem::Fail { error }
            if error["_tag"] == "WorktreeAdoptionError"
                && error["reason"] == expected_reason
                && error["currentGeneration"].as_u64().is_some()
    )));
}

async fn adoption_outcome(socket: &mut TestSocket, request_id: &str) -> (String, Value) {
    let message = next_server_message(socket).await;
    let value = serde_json::to_value(&message).expect("server message value");
    match message {
        ServerMessage::Exit {
            request_id: actual,
            exit: RpcExit::Success { .. },
        } => {
            assert_eq!(actual.as_str(), request_id);
            ("success".to_owned(), value)
        }
        ServerMessage::Exit {
            request_id: actual,
            exit: RpcExit::Failure { cause },
        } => {
            assert_eq!(actual.as_str(), request_id);
            let reason = cause
                .iter()
                .find_map(|item| match item {
                    CauseItem::Fail { error } if error["_tag"] == "WorktreeAdoptionError" => {
                        error["reason"].as_str().map(str::to_owned)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| "untyped-failure".to_owned());
            (reason, value)
        }
        other => panic!("expected adoption exit: {other:?}"),
    }
}

fn eligible_candidate(snapshot: &Value) -> &Value {
    snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["eligibleForAdoption"] == true)
        .expect("eligible candidate")
}

fn descriptor_for_key<'a>(snapshot: &'a Value, key: &Value) -> &'a Value {
    snapshot["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .find(|worktree| worktree["worktreeKey"] == *key)
        .expect("worktree descriptor")
}

fn adoption_payload(command_id: &str, descriptor: &Value, snapshot: &Value) -> Value {
    json!({
        "commandId":command_id,
        "projectId":"project-1",
        "worktreeKey":descriptor["worktreeKey"],
        "expectedGeneration":snapshot["generation"],
        "threadDefaults":{
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "runtimeMode":"full-access",
            "interactionMode":"default"
        }
    })
}

async fn branch_reconciliation_events(repositories: &Repositories) -> Vec<OrchestrationEvent> {
    repositories
        .read_events_from_sequence(0, 512)
        .await
        .expect("event read")
        .into_iter()
        .filter(|event| {
            event.event.event_type == "thread.meta-updated"
                && event
                    .event
                    .command_id
                    .as_deref()
                    .is_some_and(|command_id| command_id.starts_with("worktree-branch-reconcile:"))
        })
        .collect()
}

fn branch_reconciliation_command_id(
    thread_id: &str,
    branch: Option<&str>,
    head: Option<&str>,
) -> String {
    let mut identity = b"bibcode.worktree.branch-reconcile.v1\0".to_vec();
    identity.extend_from_slice(branch.unwrap_or("<detached>").as_bytes());
    identity.push(0);
    identity.extend_from_slice(head.unwrap_or("<unknown>").as_bytes());
    let hash = Sha256::digest(identity);
    let hash = hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("worktree-branch-reconcile:{thread_id}:{hash}")
}

async fn assert_managed_creation_terminal_invalidation(reuse_free_branch: bool) {
    let hooks = TestHooks::default();
    let mut fixture = CatalogRpcFixture::new_with_removal_services_and_options(
        false,
        Arc::new(TestNoopQuiescer),
        None,
        EngineOptions {
            queue_capacity: 16,
            test_hooks: hooks.clone(),
        },
    )
    .await;
    let (requested_ref, expected_ref, command_id, thread_id) = if reuse_free_branch {
        git(&fixture.main, &["branch", "feature/managed-reuse"], None);
        (
            "feature/managed-reuse",
            "feature/managed-reuse",
            "catalog-managed-reuse",
            "catalog-managed-reuse-thread",
        )
    } else {
        (
            "main",
            "main-2",
            "catalog-managed-suffix",
            "catalog-managed-suffix-thread",
        )
    };
    request(
        fixture.socket(),
        "400",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1", "reason":"explicit"}),
    )
    .await;
    let _ = success_value(fixture.socket(), "400").await;
    let create_payload = json!({
        "commandId":command_id,
        "projectId":"project-1",
        "threadId":thread_id,
        "title":expected_ref,
        "refName":requested_ref,
        "newRefName":null,
        "baseRefName":null,
        "threadDefaults":{
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "runtimeMode":"full-access",
            "interactionMode":"default"
        }
    });
    let pause = hooks.pause_before_next_command_persist();
    request(
        fixture.socket(),
        "401",
        "worktree.createManaged",
        create_payload.clone(),
    )
    .await;
    timeout(ENGINE_HANDOFF_DEADLOCK_BOUND, pause.wait_until_entered())
        .await
        .expect("managed creation reaches its pre-persistence boundary");
    assert!(
        !fixture
            .repositories
            .get_command_receipt(command_id.to_owned())
            .await
            .expect("managed creation receipt read")
            .is_some_and(|receipt| receipt.status == "accepted")
    );
    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2,
        "Git mutation precedes durable thread settlement in this integration seam"
    );
    pause.release();

    let created = success_value(fixture.socket(), "401").await;
    assert_eq!(created["threadId"], thread_id);
    assert_eq!(created["refName"], expected_ref);
    let created_path = created["path"].as_str().expect("managed path").to_owned();

    let receipt = fixture
        .repositories
        .get_command_receipt(command_id.to_owned())
        .await
        .expect("managed creation receipt read")
        .expect("accepted managed creation receipt");
    assert_eq!(receipt.status, "accepted");
    assert_eq!(receipt.aggregate_id, thread_id);
    assert_eq!(
        receipt.payload_digest.as_deref(),
        Some(
            canonical_command_digest(&create_payload)
                .expect("managed creation digest")
                .as_str()
        )
    );
    let owner = fixture
        .repositories
        .get_thread(thread_id.to_owned())
        .await
        .expect("managed owner read")
        .expect("managed owner");
    assert_eq!(owner.branch.as_deref(), Some(expected_ref));
    assert_eq!(owner.worktree_path.as_deref(), Some(created_path.as_str()));

    let inventory_before_retry = git_output(&fixture.main, &["worktree", "list", "--porcelain"]);
    request(
        fixture.socket(),
        "402",
        "worktree.createManaged",
        create_payload.clone(),
    )
    .await;
    assert_eq!(success_value(fixture.socket(), "402").await, created);
    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"]),
        inventory_before_retry,
        "retry must resolve from the immutable creation receipt without another checkout"
    );
    let receipt_after_retry = fixture
        .repositories
        .get_command_receipt(command_id.to_owned())
        .await
        .expect("managed retry receipt read")
        .expect("managed retry receipt");
    assert_eq!(receipt_after_retry.accepted_at, receipt.accepted_at);
    assert_eq!(receipt_after_retry.result_sequence, receipt.result_sequence);
    assert_eq!(receipt_after_retry.payload_digest, receipt.payload_digest);

    request(
        fixture.socket(),
        "403",
        "vcs.refreshWorktreeCatalog",
        json!({"projectId":"project-1", "reason":"focus"}),
    )
    .await;
    let focused = success_value(fixture.socket(), "403").await;
    let focused_managed = focused["worktrees"]
        .as_array()
        .expect("focused worktrees")
        .iter()
        .find(|worktree| {
            worktree["path"].as_str().is_some_and(|path| {
                same_worktree_identity(Path::new(path), Path::new(&created_path))
            })
        })
        .expect("managed worktree after immediate Focus");
    assert_eq!(focused_managed["eligibleForAdoption"], false);

    fixture.shutdown().await;
}

async fn wait_for_catalog_generation(
    subscription: &mut CatalogSubscription,
    after_generation: u64,
) -> Arc<WorktreeCatalogSnapshot> {
    timeout(Duration::from_secs(10), async {
        loop {
            let latest = subscription.latest();
            if latest.generation > after_generation {
                return latest;
            }
            subscription
                .changed()
                .await
                .expect("catalog subscription remains open");
        }
    })
    .await
    .expect("catalog generation advances after terminal mutation")
}

fn same_worktree_identity(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).expect("canonical left worktree identity");
    let right = fs::canonicalize(right).expect("canonical right worktree identity");
    normalize_worktree_path_key(&left, host_path_platform())
        == normalize_worktree_path_key(&right, host_path_platform())
}

fn git(cwd: &Path, args: &[&str], final_path: Option<&Path>) {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    if let Some(final_path) = final_path {
        command.arg(final_path);
    }
    let output = command.output().expect("run Git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        command,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run Git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("Git output is UTF-8")
}
