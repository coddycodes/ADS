use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tempfile::TempDir;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::git::{
    GitRepository, GitWorktreeInventory, GitWorktreeRecord, host_path_platform,
    worktree_repository_key,
};

use super::fingerprint::{
    CatalogRepositoryFingerprint, FingerprintOutcome, FingerprintRequest,
    read_catalog_repository_fingerprint,
};
use super::service::TokioCatalogFileSystem;
use super::service::{
    CatalogFileSystem, CatalogFingerprintSource, CatalogFuture, CatalogProject,
    CatalogProjectionSource, CatalogServiceOptions, CatalogShallowSignature, CatalogThread,
    DirectoryProbeState, InventorySource, ScanFailure,
};
use super::{
    CatalogRefreshTrigger, CatalogWorkspaceLossObserver, WorkspaceAvailabilityRegistry,
    WorkspaceLossTransition, WorktreeCatalogService,
};

#[test]
fn current_thread_runtime_drop_releases_a_live_catalog_subscription() {
    let (completed_sender, completed_receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        runtime.block_on(async {
            let service = WorktreeCatalogService::with_dependencies(
                Arc::new(FakeProjectionSource::new([project(
                    "project-1",
                    "/repo/main",
                    [],
                )])),
                Arc::new(FakeInventorySource::new([inventory(
                    "/repo/common",
                    [record("/repo/main", true)],
                )])),
                Arc::new(FakeFileSystem::new([
                    ("/repo/main", DirectoryProbeState::Present),
                    ("/repo/common", DirectoryProbeState::Present),
                ])),
                CatalogServiceOptions::default(),
            );
            let subscription = service.subscribe("project-1").await.expect("subscription");
            let (owned_sender, owned_receiver) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _subscription = subscription;
                let _ = owned_sender.send(());
                std::future::pending::<()>().await;
            });
            owned_receiver.await.expect("subscription owner starts");
        });
        drop(runtime);
        let _ = completed_sender.send(());
    });

    completed_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("runtime teardown releases the live catalog subscription");
    worker.join().expect("runtime worker");
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_drains_a_lifecycle_task_aborted_before_its_first_poll() {
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([])),
        Arc::new(FakeInventorySource::new([])),
        Arc::new(FakeFileSystem::new([])),
        CatalogServiceOptions::default(),
    );
    service.register_pending_lifecycle_task_for_test();

    tokio::time::timeout(Duration::from_secs(2), service.shutdown())
        .await
        .expect("shutdown drains a lifecycle task that never started");
}

#[tokio::test]
async fn authoritative_loss_installs_guard_before_the_quiesce_callback() {
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/adopted", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let registry = WorkspaceAvailabilityRegistry::new();
    let callback = Arc::new(GuardOrderingObserver {
        registry: registry.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
        guarded: Arc::new(AtomicBool::new(false)),
    });
    let service = WorktreeCatalogService::with_dependencies_and_availability(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/adopted")],
        )])),
        Arc::new(FakeInventorySource::new([
            inventory(
                "/repo/common",
                [record("/repo/main", true), record("/repo/adopted", false)],
            ),
            inventory(
                "/repo/common",
                [record("/repo/main", true), record("/repo/adopted", false)],
            ),
        ])),
        filesystem.clone(),
        CatalogServiceOptions::default(),
        registry,
    );
    service.set_workspace_loss_observer(callback.clone());
    let subscription = service.subscribe("project-1").await.expect("subscription");

    filesystem.set("/repo/adopted", DirectoryProbeState::Missing);
    let missing = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("authoritative missing refresh");

    assert_eq!(
        missing.adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::MissingRegistered
    );
    assert_eq!(callback.calls.load(Ordering::SeqCst), 1);
    assert!(callback.guarded.load(Ordering::SeqCst));
    drop(subscription);
}

#[tokio::test]
async fn initial_authoritative_missing_snapshot_installs_guard_before_subscription_returns() {
    let registry = WorkspaceAvailabilityRegistry::new();
    let callback = Arc::new(GuardOrderingObserver {
        registry: registry.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
        guarded: Arc::new(AtomicBool::new(false)),
    });
    let service = WorktreeCatalogService::with_dependencies_and_availability(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/adopted")],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/adopted", false)],
        )])),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/adopted", DirectoryProbeState::Missing),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
        registry,
    );
    service.set_workspace_loss_observer(callback.clone());

    let subscription = service.subscribe("project-1").await.expect("subscription");

    assert_eq!(
        subscription.latest().adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::MissingRegistered
    );
    assert_eq!(callback.calls.load(Ordering::SeqCst), 1);
    assert!(callback.guarded.load(Ordering::SeqCst));
}

#[tokio::test]
async fn authoritative_recovery_drains_terminal_signal_without_holding_the_catalog_entry_lock() {
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/adopted", DirectoryProbeState::Missing),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let registry = WorkspaceAvailabilityRegistry::new();
    let service = WorktreeCatalogService::with_dependencies_and_availability(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/adopted")],
        )])),
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory(
                "/repo/common",
                [record("/repo/main", true), record("/repo/adopted", false)],
            )
        }))),
        filesystem.clone(),
        CatalogServiceOptions::default(),
        registry.clone(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let missing = subscription.latest();
    let loss = WorkspaceLossTransition {
        thread_id: "thread-1".to_owned(),
        repository_key: missing.repository_key.clone(),
        generation: missing.generation,
        path: PathBuf::from("/repo/adopted"),
        availability: super::AdoptedWorktreeAvailability::MissingRegistered,
    };
    let permit = registry
        .begin_terminal_signal(&loss)
        .await
        .expect("terminal cleanup owns the missing transition");
    let invalidation_started = registry
        .terminal_signal_invalidation_notification(&loss)
        .await
        .expect("current terminal signal gate");
    let cleanup_release = Arc::new(Notify::new());
    let task_cleanup_release = cleanup_release.clone();
    let cleanup = tokio::spawn(async move {
        task_cleanup_release.notified().await;
        tokio::task::yield_now().await;
        drop(permit);
    });

    filesystem.set("/repo/adopted", DirectoryProbeState::Present);
    let refresh_service = service.clone();
    let refresh = tokio::spawn(async move {
        refresh_service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
    });
    invalidation_started.notified().await;

    assert!(
        service.latest("project-1").await.is_some(),
        "terminal drain must not retain the catalog entry lock"
    );
    assert_eq!(
        registry
            .guard_thread("thread-1")
            .await
            .expect_err("recovery is not visible before terminal cleanup drains")
            .availability,
        super::AdoptedWorktreeAvailability::MissingRegistered
    );
    assert_eq!(
        subscription.latest().adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::MissingRegistered,
        "the recovered watch snapshot is not visible before terminal cleanup drains"
    );
    assert!(!refresh.is_finished());

    cleanup_release.notify_one();
    cleanup.await.expect("cleanup task");
    let recovered = refresh
        .await
        .expect("refresh task")
        .expect("authoritative recovery refresh");
    assert_eq!(
        recovered.adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::Present
    );
    assert_eq!(
        subscription.latest().adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::Present
    );
    assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
}

#[tokio::test]
async fn bootstrap_recovery_drains_terminal_signal_without_holding_the_catalog_registry() {
    let registry = WorkspaceAvailabilityRegistry::new();
    let repository_key = worktree_repository_key(Path::new("/repo/common"), host_path_platform())
        .as_str()
        .to_owned();
    let loss = WorkspaceLossTransition {
        thread_id: "thread-1".to_owned(),
        repository_key,
        generation: 1,
        path: PathBuf::from("/repo/adopted"),
        availability: super::AdoptedWorktreeAvailability::MissingRegistered,
    };
    assert!(
        registry
            .mark_unavailable(loss.clone())
            .await
            .expect("physical identity resolves")
    );
    let permit = registry
        .begin_terminal_signal(&loss)
        .await
        .expect("terminal cleanup owns the missing transition");
    let invalidation_started = registry
        .terminal_signal_invalidation_notification(&loss)
        .await
        .expect("current terminal signal gate");
    let cleanup_release = Arc::new(Notify::new());
    let task_cleanup_release = cleanup_release.clone();
    let cleanup = tokio::spawn(async move {
        task_cleanup_release.notified().await;
        tokio::task::yield_now().await;
        drop(permit);
    });
    let service = WorktreeCatalogService::with_dependencies_and_availability(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/adopted")],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/adopted", false)],
        )])),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/adopted", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
        registry.clone(),
    );
    let subscribe_service = service.clone();
    let subscribing = tokio::spawn(async move { subscribe_service.subscribe("project-1").await });
    invalidation_started.notified().await;

    assert_eq!(
        service.latest("project-1").await,
        None,
        "terminal drain must not retain the catalog registry"
    );
    assert!(!subscribing.is_finished());

    cleanup_release.notify_one();
    cleanup.await.expect("cleanup task");
    let subscription = subscribing
        .await
        .expect("subscription task")
        .expect("subscription after recovery drain");
    assert_eq!(
        subscription.latest().adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::Present
    );
    assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
}

struct GuardOrderingObserver {
    registry: WorkspaceAvailabilityRegistry,
    calls: Arc<AtomicUsize>,
    guarded: Arc<AtomicBool>,
}

impl CatalogWorkspaceLossObserver for GuardOrderingObserver {
    fn observe(&self, transitions: Vec<WorkspaceLossTransition>) -> CatalogFuture<()> {
        assert_eq!(transitions.len(), 1);
        let registry = self.registry.clone();
        let calls = self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(calls, 0, "one callback owns the loss transition");
        let guarded = self.guarded.load(Ordering::SeqCst);
        assert!(!guarded, "callback state starts unset");
        let guarded_flag = self.guarded.clone();
        Box::pin(async move {
            let error = registry
                .guard_thread("thread-1")
                .await
                .expect_err("guard must be visible before callback work starts");
            assert_eq!(
                error.availability,
                super::AdoptedWorktreeAvailability::MissingRegistered
            );
            guarded_flag.store(true, Ordering::SeqCst);
        })
    }
}

#[tokio::test]
async fn anchor_preference_is_primary_then_present_adopted_then_lifetime_common_directory() {
    let projections = Arc::new(FakeProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [thread("thread-1", "/repo/adopted")],
    )]));
    let inventory = Arc::new(FakeInventorySource::new([
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/adopted", false)],
        ),
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/adopted", false)],
        ),
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/adopted", false)],
        ),
    ]));
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/adopted", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        projections,
        inventory.clone(),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );

    let subscription = service
        .subscribe("project-1")
        .await
        .expect("initial catalog subscription");
    filesystem.set("/repo/main", DirectoryProbeState::Missing);
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("adopted-anchor refresh");
    filesystem.set("/repo/adopted", DirectoryProbeState::Missing);
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("lifetime-anchor refresh");

    assert_eq!(
        inventory.calls(),
        [
            PathBuf::from("/repo/main"),
            PathBuf::from("/repo/adopted"),
            PathBuf::from("/repo/common"),
        ]
    );
    drop(subscription);
}

#[tokio::test]
async fn concurrent_first_subscribers_share_one_bootstrap_scan() {
    let inventory = Arc::new(BlockingInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([(
            "/repo/main",
            DirectoryProbeState::Present,
        )])),
        CatalogServiceOptions::default(),
    );

    let first_service = service.clone();
    let first = tokio::spawn(async move { first_service.subscribe("project-1").await });
    wait_for_count(&inventory.calls, 1).await;
    let second_service = service.clone();
    let second = tokio::spawn(async move { second_service.subscribe("project-1").await });
    tokio::task::yield_now().await;
    inventory.release.add_permits(1);

    let first = first
        .await
        .expect("first subscriber task")
        .expect("first subscriber");
    let second = second
        .await
        .expect("second subscriber task")
        .expect("second subscriber");
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.latest().generation, second.latest().generation);
}

#[tokio::test]
async fn initial_latest_marks_a_first_subscriber_refresh_as_seen_exactly_once() {
    let options = CatalogServiceOptions {
        result_ttl: Duration::ZERO,
        ..CatalogServiceOptions::default()
    };
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        }))),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        options,
    );

    let mut subscription = service.subscribe("project-1").await.expect("subscription");
    let initial = subscription.initial_latest();

    assert_eq!(initial.generation, 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), subscription.changed())
            .await
            .is_err(),
        "the snapshot atomically returned by latest must already be marked seen"
    );
}

#[tokio::test]
async fn projects_sharing_one_repository_receive_only_their_own_joined_threads() {
    let inventory = Arc::new(FakeInventorySource::new((0..2).map(|_| {
        inventory(
            "/repo/common",
            [
                record("/repo/main", true),
                record("/repo/a", false),
                record("/repo/b", false),
            ],
        )
    })));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/main", [thread("thread-a", "/repo/a")]),
            project("project-b", "/repo/main", [thread("thread-b", "/repo/b")]),
        ])),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/a", DirectoryProbeState::Present),
            ("/repo/b", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );

    let project_a = service.subscribe("project-a").await.expect("project A");
    let project_b = service.subscribe("project-b").await.expect("project B");

    assert_eq!(
        project_a
            .latest()
            .adopted_workspaces
            .iter()
            .map(|workspace| workspace.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["thread-a"]
    );
    assert_eq!(
        project_b
            .latest()
            .adopted_workspaces
            .iter()
            .map(|workspace| workspace.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["thread-b"]
    );
    assert_eq!(
        project_a.latest().repository_key,
        project_b.latest().repository_key
    );
}

#[tokio::test]
async fn concurrent_same_repository_refreshes_share_observation_without_crossing_streams() {
    let inventory = Arc::new(PausingAfterTwoInventorySource::new(inventory(
        "/repo/common",
        [
            record("/repo/main", true),
            record("/repo/a", false),
            record("/repo/b", false),
        ],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/main", [thread("thread-a", "/repo/a")]),
            project("project-b", "/repo/main", [thread("thread-b", "/repo/b")]),
        ])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/a", DirectoryProbeState::Present),
            ("/repo/b", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let project_a = service.subscribe("project-a").await.expect("project A");
    let project_b = service.subscribe("project-b").await.expect("project B");
    let refresh_a = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-a", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    let refresh_b = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-b", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 3).await;
    inventory.release.add_permits(1);

    let refreshed_a = refresh_a.await.expect("A task").expect("A refresh");
    let refreshed_b = refresh_b.await.expect("B task").expect("B refresh");
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
    assert_eq!(refreshed_a.adopted_workspaces[0].thread_id, "thread-a");
    assert_eq!(refreshed_b.adopted_workspaces[0].thread_id, "thread-b");
    assert_eq!(
        project_a.latest().adopted_workspaces[0].thread_id,
        "thread-a"
    );
    assert_eq!(
        project_b.latest().adopted_workspaces[0].thread_id,
        "thread-b"
    );
}

#[tokio::test]
async fn shared_observation_never_substitutes_for_validating_a_different_anchor() {
    let inventory = Arc::new(PausingThirdAnchorInventorySource::new());
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/a-main", []),
            project("project-b", "/repo/b-main", []),
        ])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/a-main", DirectoryProbeState::Present),
            ("/repo/b-main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let project_a = service.subscribe("project-a").await.expect("project A");
    let _project_b = service.subscribe("project-b").await.expect("project B");
    let authoritative_a = project_a.latest();
    inventory.replacement_enabled.store(true, Ordering::SeqCst);

    let valid_refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-b", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 3).await;
    let replacement_refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-a", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    tokio::task::yield_now().await;
    inventory.release.add_permits(1);

    valid_refresh
        .await
        .expect("valid refresh task")
        .expect("valid anchor remains authoritative");
    let error = replacement_refresh
        .await
        .expect("replacement refresh task")
        .expect_err("different replacement anchor must be validated and rejected");
    let degraded_a = service
        .latest("project-a")
        .await
        .expect("project A catalog");
    assert_eq!(
        error.reason,
        super::CatalogErrorReason::RepositoryUnavailable
    );
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 4);
    assert!(!degraded_a.authoritative);
    assert_eq!(degraded_a.worktrees, authoritative_a.worktrees);
}

#[tokio::test]
async fn cancelled_unary_project_stops_waiting_while_an_aliased_project_keeps_shared_observation_alive()
 {
    let inventory = Arc::new(PausingAfterTwoInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/main", []),
            project("project-b", "/repo/main", []),
        ])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let project_a = service.subscribe("project-a").await.expect("project A");
    let _project_b = service.subscribe("project-b").await.expect("project B");
    let refresh_b = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-b", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 3).await;
    let refresh_a = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-a", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    tokio::task::yield_now().await;

    drop(project_a);
    assert!(
        !refresh_a.is_finished(),
        "the still-active unary request owns its catalog lifecycle after the subscription detaches"
    );
    refresh_a.abort();
    assert!(
        refresh_a
            .await
            .expect_err("A unary refresh is cancelled")
            .is_cancelled()
    );
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
    assert!(!refresh_b.is_finished());

    inventory.release.add_permits(1);
    refresh_b.await.expect("B task").expect("B refresh");
}

#[tokio::test]
async fn active_mutation_worker_releases_its_view_while_an_alias_owns_the_shared_observation() {
    let inventory = Arc::new(PausingAfterTwoInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/main", []),
            project("project-b", "/repo/main", []),
        ])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let project_a = service.subscribe("project-a").await.expect("project A");
    let project_b = service.subscribe("project-b").await.expect("project B");
    let observation_requests = service.repository_observation_request_count_for_test();

    service.invalidate_after_mutation("project-a").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    wait_for_repository_observation_requests(&service, observation_requests + 1).await;
    wait_for_count(&inventory.calls, 3).await;
    let refresh_b = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-b", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_repository_observation_requests(&service, observation_requests + 2).await;

    drop(project_a);
    wait_for_active_mutation_refresh_workers(&service, 0).await;
    let detached_snapshot = service
        .latest("project-a")
        .await
        .expect("detached project A catalog");
    let reattach_a = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-a").await })
    };
    wait_for_repository_observation_requests(&service, observation_requests + 3).await;
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
    let awaiting_shared_observation = service
        .latest("project-a")
        .await
        .expect("project A remains retained");
    assert_eq!(awaiting_shared_observation.generation, 1);
    assert!(!awaiting_shared_observation.authoritative);
    assert_eq!(
        awaiting_shared_observation.worktrees,
        detached_snapshot.worktrees
    );
    assert_eq!(
        awaiting_shared_observation.adopted_workspaces,
        detached_snapshot.adopted_workspaces
    );

    inventory.release.add_permits(1);
    let refreshed_b = refresh_b
        .await
        .expect("B refresh task")
        .expect("B consumes the shared observation");
    let reattached = reattach_a
        .await
        .expect("A reattach task")
        .expect("fresh A lifecycle");

    assert_eq!(refreshed_b.generation, 2);
    assert_eq!(project_b.latest().generation, 2);
    assert_eq!(reattached.latest().generation, 2);
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
}

#[tokio::test]
async fn repository_mutation_invalidation_fans_out_to_shared_views_only() {
    let shared = inventory("/repo/common", [record("/repo/main", true)]);
    let unrelated = inventory("/other/common", [record("/other/main", true)]);
    let inventory = Arc::new(FakeInventorySource::new([
        shared.clone(),
        shared.clone(),
        unrelated,
        shared.clone(),
        shared,
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/main", []),
            project("project-b", "/repo/main", []),
            project("project-c", "/other/main", []),
        ])),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
            ("/other/main", DirectoryProbeState::Present),
            ("/other/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let _project_a = service.subscribe("project-a").await.expect("project A");
    let _project_b = service.subscribe("project-b").await.expect("project B");
    let project_c = service.subscribe("project-c").await.expect("project C");

    service
        .invalidate_repository_after_mutation("project-a")
        .await;
    wait_for_mutation_refresh_worker_starts(&service, 2).await;

    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 2);
    assert_eq!(project_c.latest().generation, 1);
}

#[tokio::test]
async fn four_repository_scans_run_concurrently_while_a_fifth_waits() {
    let projects =
        (0..5).map(|index| project(&format!("project-{index}"), &format!("/repo-{index}"), []));
    let states = (0..5)
        .map(|index| (format!("/repo-{index}"), DirectoryProbeState::Present))
        .collect::<Vec<_>>();
    let filesystem = Arc::new(FakeFileSystem::from_owned(states));
    let inventory = Arc::new(ConcurrentInventorySource::default());
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new(projects)),
        inventory.clone(),
        filesystem,
        CatalogServiceOptions::default(),
    );
    let tasks = (0..5)
        .map(|index| {
            let service = service.clone();
            tokio::spawn(async move { service.subscribe(&format!("project-{index}")).await })
        })
        .collect::<Vec<_>>();

    wait_for_count(&inventory.active, 4).await;
    tokio::task::yield_now().await;
    assert_eq!(inventory.active.load(Ordering::SeqCst), 4);
    assert_eq!(inventory.max_active.load(Ordering::SeqCst), 4);
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 4);
    inventory.release.add_permits(5);
    for task in tasks {
        task.await
            .expect("catalog task")
            .expect("catalog subscription");
    }
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 5);
    assert_eq!(inventory.max_active.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn directory_probes_are_capped_at_eight_and_time_out_to_unknown() {
    let filesystem = Arc::new(BlockingProbeFileSystem::default());
    let records = std::iter::once(record("/repo/main", true))
        .chain((0..20).map(|index| record(&format!("/repo/worktree-{index}"), false)));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            records,
        )])),
        filesystem.clone(),
        Arc::new(CatalogServiceOptions::default()).as_ref().clone(),
    );
    let service_task = service.clone();
    let subscription = tokio::spawn(async move { service_task.subscribe("project-1").await });

    wait_for_count(&filesystem.active, 8).await;
    assert_eq!(filesystem.max_active.load(Ordering::SeqCst), 8);
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    let subscription = subscription
        .await
        .expect("subscription task")
        .expect("catalog subscription");
    assert!(
        subscription
            .latest()
            .worktrees
            .iter()
            .skip(1)
            .all(|worktree| worktree.directory_state == super::WorktreeDirectoryState::Unknown)
    );
}

#[tokio::test]
async fn watch_subscription_delivers_only_the_latest_snapshot_after_lag() {
    let inventory =
        Arc::new(FakeInventorySource::new((0..21).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let mut subscription = service.subscribe("project-1").await.expect("subscription");
    for _ in 0..20 {
        service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
            .expect("refresh");
    }

    let latest = subscription.changed().await.expect("latest catalog value");
    assert_eq!(latest.generation, 21);
    assert_eq!(subscription.latest().generation, 21);
}

#[tokio::test]
async fn failed_scan_retains_the_last_authoritative_arrays_and_only_degrades_health() {
    let first = inventory(
        "/repo/common",
        [record("/repo/main", true), record("/repo/external", false)],
    );
    let inventory = Arc::new(FakeInventorySource::new_results([
        Ok(first),
        Err(ScanFailure {
            reason: super::CatalogDegradedReason::GitFailed,
            message: "git worktree list failed".to_owned(),
        }),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/external")],
        )])),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/external", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let authoritative = subscription.latest();

    let degraded = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("failed observation still returns retained catalog");

    assert!(!degraded.authoritative);
    assert_eq!(degraded.generation, authoritative.generation);
    assert_eq!(degraded.worktrees, authoritative.worktrees);
    assert_eq!(
        degraded.adopted_workspaces,
        authoritative.adopted_workspaces
    );
    assert!(matches!(
        degraded.scan_status,
        super::CatalogScanStatus::Degraded {
            reason: super::CatalogDegradedReason::GitFailed,
            ..
        }
    ));
}

#[tokio::test]
async fn warm_repository_identity_mismatch_retains_arrays_and_publishes_degraded_health() {
    let inventory = Arc::new(FakeInventorySource::new([
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/external", false)],
        ),
        inventory(
            "/replacement/common",
            [
                record("/repo/main", true),
                record("/repo/replacement", false),
            ],
        ),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/external")],
        )])),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/external", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let authoritative = subscription.latest();

    let error = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect_err("replacement repository is rejected");
    let degraded = service.latest("project-1").await.expect("retained catalog");

    assert_eq!(
        error.reason,
        super::CatalogErrorReason::RepositoryUnavailable
    );
    assert!(!degraded.authoritative);
    assert_eq!(degraded.generation, authoritative.generation);
    assert_eq!(degraded.worktrees, authoritative.worktrees);
    assert_eq!(
        degraded.adopted_workspaces,
        authoritative.adopted_workspaces
    );
    assert!(matches!(
        degraded.scan_status,
        super::CatalogScanStatus::Degraded {
            reason: super::CatalogDegradedReason::AnchorUnavailable,
            ..
        }
    ));
}

#[tokio::test]
async fn warm_identity_reconciliation_error_restores_retained_snapshot_and_finishes_once() {
    let registry = WorkspaceAvailabilityRegistry::new();
    let inventory = Arc::new(FakeInventorySource::new((0..2).map(|_| {
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/external", false)],
        )
    })));
    let options = CatalogServiceOptions::default();
    let expected_backoff = options.poll_interval;
    let service = WorktreeCatalogService::with_dependencies_and_availability(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [thread("thread-1", "/repo/external")],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/external", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        options,
        registry.clone(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let authoritative = subscription.latest();
    let before = service
        .refresh_bookkeeping_for_test("project-1")
        .expect("warm refresh bookkeeping");

    registry.fail_identity_resolution_for_test(std::io::ErrorKind::PermissionDenied);
    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    wait_for_active_mutation_refresh_workers(&service, 0).await;

    let degraded = subscription.latest();
    let after = service
        .refresh_bookkeeping_for_test("project-1")
        .expect("settled refresh bookkeeping");
    assert_eq!(inventory.calls().len(), 2, "the failed refresh runs once");
    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
    assert_eq!(
        service.max_active_mutation_refresh_worker_count_for_test(),
        1
    );
    assert_eq!(
        after.completed_refreshes,
        before.completed_refreshes + 1,
        "the refresh is finalized once"
    );
    assert_eq!(
        after.last_refresh_result,
        Some(Err(super::CatalogErrorReason::RepositoryUnavailable)),
        "coalesced waiters retain the terminal catalog error"
    );
    assert_eq!(
        after.pending_mutation_refresh_epoch, None,
        "the completed mutation epoch is cleared"
    );
    assert_eq!(after.failure_backoff, expected_backoff);
    assert!(
        after.retry_scheduled,
        "the degraded refresh schedules one retry"
    );
    assert!(!degraded.authoritative);
    assert_eq!(degraded.generation, authoritative.generation);
    assert_eq!(degraded.worktrees, authoritative.worktrees);
    assert_eq!(
        degraded.adopted_workspaces,
        authoritative.adopted_workspaces
    );
    assert!(matches!(
        degraded.scan_status,
        super::CatalogScanStatus::Degraded {
            reason: super::CatalogDegradedReason::AnchorUnavailable,
            ..
        }
    ));
}

#[tokio::test]
async fn mutation_epoch_rejects_a_stale_in_flight_result() {
    let inventory = Arc::new(PausingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    drop(subscription);
    let refresh_service = service.clone();
    let refresh = tokio::spawn(async move {
        refresh_service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
    });
    wait_for_count(&inventory.calls, 2).await;
    service.invalidate_after_mutation("project-1").await;
    inventory.release.add_permits(1);

    let error = refresh
        .await
        .expect("refresh task")
        .expect_err("pre-mutation result must be rejected");
    assert_eq!(error.reason, super::CatalogErrorReason::StaleGeneration);
    let latest = service.latest("project-1").await.expect("latest");
    assert_eq!(latest.generation, 1);
    assert!(latest.authoritative);
    assert!(matches!(
        latest.scan_status,
        super::CatalogScanStatus::Ready
    ));
}

#[tokio::test]
async fn concurrent_refresh_waiters_receive_the_same_stale_generation_result() {
    let inventory = Arc::new(PausingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    drop(subscription);
    let first = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;
    let second = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    tokio::task::yield_now().await;

    service.invalidate_after_mutation("project-1").await;
    inventory.release.add_permits(2);

    let first = first
        .await
        .expect("first refresh task")
        .expect_err("first refresh is stale");
    let second = second
        .await
        .expect("second refresh task")
        .expect_err("coalesced refresh shares stale result");
    assert_eq!(first.reason, super::CatalogErrorReason::StaleGeneration);
    assert_eq!(second, first);
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn repeated_mutation_invalidations_recover_after_a_coalesced_stale_completion() {
    let inventory = Arc::new(PausingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let mut subscription = service.subscribe("project-1").await.expect("subscription");
    let old_refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;

    service.invalidate_after_mutation("project-1").await;
    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    inventory.release.add_permits(1);

    let stale = old_refresh
        .await
        .expect("old refresh task")
        .expect_err("pre-mutation refresh is stale");
    assert_eq!(stale.reason, super::CatalogErrorReason::StaleGeneration);
    wait_for_count(&inventory.calls, 3).await;
    inventory.release.add_permits(1);

    let published = loop {
        let snapshot = subscription.changed().await.expect("catalog publication");
        if snapshot.authoritative && snapshot.generation == 2 {
            break snapshot;
        }
    };
    assert_eq!(published.generation, 2);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn delayed_mutation_invalidations_use_one_lifecycle_worker_and_one_recovery_scan() {
    let inventory = Arc::new(PausingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let mut subscription = service.subscribe("project-1").await.expect("subscription");
    let old_refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;
    let delayed_worker_release = service.pause_mutation_refresh_starts_after_for_test(1);

    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    service.invalidate_after_mutation("project-1").await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    inventory.release.add_permits(1);

    let stale = old_refresh
        .await
        .expect("old refresh task")
        .expect_err("pre-mutation refresh is stale");
    assert_eq!(stale.reason, super::CatalogErrorReason::StaleGeneration);
    wait_for_count(&inventory.calls, 3).await;
    inventory.release.add_permits(1);
    let published = loop {
        let snapshot = subscription.changed().await.expect("catalog publication");
        if snapshot.authoritative && snapshot.generation == 2 {
            break snapshot;
        }
    };
    assert_eq!(published.generation, 2);

    delayed_worker_release.add_permits(4);
    inventory.release.add_permits(4);
    wait_for_active_mutation_refresh_workers(&service, 0).await;

    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
    assert_eq!(
        service.max_active_mutation_refresh_worker_count_for_test(),
        1
    );
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn mutation_during_recovery_uses_one_worker_and_one_serialized_follow_up_scan() {
    let inventory = Arc::new(PausingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let mut subscription = service.subscribe("project-1").await.expect("subscription");
    let old_refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;

    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    inventory.release.add_permits(1);
    let stale = old_refresh
        .await
        .expect("old refresh task")
        .expect_err("pre-mutation refresh is stale");
    assert_eq!(stale.reason, super::CatalogErrorReason::StaleGeneration);
    wait_for_count(&inventory.calls, 3).await;

    service.invalidate_after_mutation("project-1").await;
    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
    inventory.release.add_permits(1);
    wait_for_count(&inventory.calls, 4).await;
    inventory.release.add_permits(1);

    let published = loop {
        let snapshot = subscription.changed().await.expect("catalog publication");
        if snapshot.authoritative && snapshot.generation == 2 {
            break snapshot;
        }
    };
    wait_for_active_mutation_refresh_workers(&service, 0).await;
    assert_eq!(published.generation, 2);
    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
    assert_eq!(
        service.max_active_mutation_refresh_worker_count_for_test(),
        1
    );
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn final_unsubscribe_clears_a_pending_mutation_worker_before_it_can_scan_or_publish() {
    let inventory =
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let probe_calls = filesystem.probe_calls().len();
    let worker_release = service.pause_mutation_refresh_starts_after_for_test(0);

    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    drop(subscription);
    worker_release.add_permits(1);
    wait_for_active_mutation_refresh_workers(&service, 0).await;

    assert_eq!(inventory.calls().len(), 1);
    assert_eq!(filesystem.probe_calls().len(), probe_calls);
    let latest = service.latest("project-1").await.expect("retained catalog");
    assert!(latest.authoritative);
    assert_eq!(latest.generation, 1);
}

#[tokio::test]
async fn final_unsubscribe_aborts_a_mutation_worker_blocked_in_projection_before_reattach() {
    let projections = Arc::new(PausingSecondProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [],
    )]));
    let inventory =
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        projections.clone(),
        inventory.clone(),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let probe_calls = filesystem.probe_calls().len();

    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    wait_for_count(&projections.calls, 2).await;
    drop(subscription);

    wait_for_active_mutation_refresh_workers(&service, 0).await;
    assert_eq!(inventory.calls().len(), 1);
    assert_eq!(filesystem.probe_calls().len(), probe_calls);

    let reattach = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    wait_for_count(&projections.calls, 3).await;
    let reattached = reattach
        .await
        .expect("reattach task")
        .expect("fresh lifecycle does not wait on old projection work");

    assert!(reattached.latest().authoritative);
    assert_eq!(reattached.latest().generation, 2);
    assert_eq!(inventory.calls().len(), 2);
    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
}

#[tokio::test]
async fn reattach_does_not_inherit_a_pending_mutation_worker_from_the_old_lifecycle() {
    let inventory =
        Arc::new(FakeInventorySource::new((0..3).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let first = service
        .subscribe("project-1")
        .await
        .expect("first lifecycle");
    let old_worker_release = service.pause_mutation_refresh_starts_after_for_test(0);

    service.invalidate_after_mutation("project-1").await;
    wait_for_mutation_refresh_worker_starts(&service, 1).await;
    drop(first);
    let reattached = service.subscribe("project-1").await.expect("new lifecycle");
    assert_eq!(reattached.latest().generation, 2);
    assert_eq!(inventory.calls().len(), 2);

    old_worker_release.add_permits(1);
    wait_for_active_mutation_refresh_workers(&service, 0).await;

    assert_eq!(inventory.calls().len(), 2);
    assert_eq!(reattached.latest().generation, 2);
    assert_eq!(service.mutation_refresh_worker_start_count_for_test(), 1);
}

#[tokio::test(start_paused = true)]
async fn focus_refresh_uses_the_one_second_result_ttl() {
    let inventory =
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("coalesced focus refresh");
    assert_eq!(inventory.calls().len(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("expired focus refresh");
    assert_eq!(inventory.calls().len(), 2);
    drop(subscription);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_matching_focus_skips_inventory_until_reconciliation() {
    let inventory = Arc::new(FakeInventorySource::new((0..2).map(|_| {
        inventory(
            "/repo/common",
            [
                record("/repo/main", true),
                record("/repo/managed", false),
                record("/repo/adopted", false),
            ],
        )
    })));
    let projections = Arc::new(FakeProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [],
    )]));
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/managed", DirectoryProbeState::Present),
        ("/repo/adopted", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let fingerprint = Arc::new(FakeFingerprintSource::new([
        FingerprintOutcome::Known(known.clone()),
        FingerprintOutcome::Known(known),
    ]));
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        projections.clone(),
        inventory.clone(),
        filesystem.clone(),
        fingerprint,
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");

    tokio::time::advance(Duration::from_secs(1)).await;
    let seeded = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("fingerprint-seeding scan");
    service
        .note_managed_creation("project-1", Path::new("/repo/managed"))
        .await;
    projections.set_project(project(
        "project-1",
        "/repo/main",
        [thread("thread-adopted", "/repo/adopted")],
    ));
    filesystem.set("/repo/adopted", DirectoryProbeState::Missing);
    tokio::time::advance(Duration::from_secs(1)).await;
    let rejoined = service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("matching fingerprint refresh");

    assert_eq!(inventory.calls().len(), 2);
    assert_eq!(rejoined.generation, seeded.generation + 1);
    assert!(!descriptor(&rejoined, "/repo/managed").eligible_for_adoption);
    assert_eq!(
        rejoined.adopted_workspaces[0].availability,
        super::AdoptedWorktreeAvailability::MissingRegistered
    );
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint_fixture);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_matching_focus_reuses_a_bare_repository_observation() {
    let bare = GitWorktreeRecord {
        path: PathBuf::from("/repo/bare.git"),
        head: None,
        branch: None,
        is_primary: true,
        is_bare: true,
        locked: false,
        lock_reason: None,
        is_prunable: false,
        prunable_reason: None,
    };
    let inventory = Arc::new(FakeInventorySource::new(
        (0..3).map(|_| inventory("/repo/bare.git", [bare.clone()])),
    ));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let fingerprint = Arc::new(FakeFingerprintSource::new([
        FingerprintOutcome::Known(known.clone()),
        FingerprintOutcome::Known(known),
    ]));
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/bare.git",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([(
            "/repo/bare.git",
            DirectoryProbeState::Present,
        )])),
        fingerprint.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("bare fingerprint-seeding scan");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("matching bare fingerprint refresh");

    assert_eq!(inventory.calls().len(), 2);
    let requests = fingerprint.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].primary_path, Path::new("/repo/bare.git"));
    assert_eq!(
        requests[0].known_worktree_paths,
        [PathBuf::from("/repo/bare.git")]
    );
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint_fixture);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_changed_or_unknown_focus_runs_inventory() {
    let (first_fixture, first) = known_test_fingerprint().await;
    let (second_fixture, second) = known_test_fingerprint().await;
    for outcomes in [
        vec![
            FingerprintOutcome::Known(first.clone()),
            FingerprintOutcome::Known(second.clone()),
        ],
        vec![
            FingerprintOutcome::Known(first.clone()),
            FingerprintOutcome::Unknown(super::fingerprint::FingerprintFailure::Unreadable),
        ],
    ] {
        let inventory =
            Arc::new(FakeInventorySource::new((0..3).map(|_| {
                inventory("/repo/common", [record("/repo/main", true)])
            })));
        let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
            Arc::new(FakeProjectionSource::new([project(
                "project-1",
                "/repo/main",
                [],
            )])),
            inventory.clone(),
            Arc::new(FakeFileSystem::new([
                ("/repo/main", DirectoryProbeState::Present),
                ("/repo/common", DirectoryProbeState::Present),
            ])),
            Arc::new(FakeFingerprintSource::new(outcomes)),
            CatalogServiceOptions::default(),
        );
        let subscription = service.subscribe("project-1").await.expect("subscription");
        tokio::time::advance(Duration::from_secs(1)).await;
        service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
            .expect("fingerprint-seeding scan");
        tokio::time::advance(Duration::from_secs(1)).await;
        service
            .refresh("project-1", CatalogRefreshTrigger::Focus)
            .await
            .expect("non-matching fingerprint refresh");
        assert_eq!(inventory.calls().len(), 3);
        drop(subscription);
        shutdown_fingerprint_service(&service).await;
    }
    drop(first);
    drop(second);
    drop(first_fixture);
    drop(second_fixture);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_matching_focus_forces_inventory_at_five_minutes() {
    let inventory =
        Arc::new(FakeInventorySource::new((0..3).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        Arc::new(FakeFingerprintSource::new([
            FingerprintOutcome::Known(known.clone()),
            FingerprintOutcome::Known(known),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("fingerprint-seeding scan");

    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("five-minute reconciliation");

    assert_eq!(inventory.calls().len(), 3);
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint_fixture);
}

#[tokio::test(start_paused = true)]
#[ignore = "native fleet performance evidence; run explicitly"]
async fn fingerprint_focus_fleet_reconciles_every_five_minutes_for_thirty_minutes() {
    const REPOSITORY_COUNT: usize = 10;
    const WORKLOAD_SECONDS: u64 = 30 * 60;
    const RECONCILIATION_SECONDS: u64 = 5 * 60;

    let mut fixtures = Vec::with_capacity(REPOSITORY_COUNT);
    let mut primary_paths = Vec::with_capacity(REPOSITORY_COUNT);
    let mut common_dirs = Vec::with_capacity(REPOSITORY_COUNT);
    for _ in 0..REPOSITORY_COUNT {
        let (fixture, primary_path, common_dir) = test_repository_fixture();
        fixtures.push(fixture);
        primary_paths.push(primary_path);
        common_dirs.push(common_dir);
    }
    let managed_path = fixtures[0].path().join("managed");
    run_git(
        &primary_paths[0],
        &[
            "worktree",
            "add",
            "-b",
            "feature/managed",
            managed_path.to_str().expect("managed path is UTF-8"),
        ],
    );
    let adopted_path = fixtures[0].path().join("adopted");
    run_git(
        &primary_paths[0],
        &[
            "worktree",
            "add",
            "-b",
            "feature/adopted",
            adopted_path.to_str().expect("adopted path is UTF-8"),
        ],
    );
    std::fs::rename(&adopted_path, fixtures[0].path().join("adopted-displaced"))
        .expect("make registered worktree path unavailable");

    let projects = (0..REPOSITORY_COUNT).map(|index| {
        project(
            &format!("project-{index}"),
            primary_paths[index]
                .to_str()
                .expect("fleet primary path is UTF-8"),
            [],
        )
    });
    let projections = Arc::new(FakeProjectionSource::new(projects));
    let reconciliation_count = WORKLOAD_SECONDS / RECONCILIATION_SECONDS;
    let inventory = Arc::new(CountingInventorySource::new(
        super::service::native_inventory_source_for_test(Arc::new(GitRepository::default())),
    ));
    let fingerprint = Arc::new(CountingNativeFingerprintSource::default());
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        projections.clone(),
        inventory.clone(),
        Arc::new(TokioCatalogFileSystem),
        fingerprint.clone(),
        CatalogServiceOptions {
            poll_interval: Duration::from_secs(60 * 60),
            ..CatalogServiceOptions::default()
        },
    );

    let mut subscriptions = Vec::with_capacity(REPOSITORY_COUNT);
    for index in 0..REPOSITORY_COUNT {
        subscriptions.push(
            service
                .subscribe(&format!("project-{index}"))
                .await
                .expect("fleet subscription"),
        );
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    for index in 0..REPOSITORY_COUNT {
        service
            .refresh(&format!("project-{index}"), CatalogRefreshTrigger::Explicit)
            .await
            .expect("fleet fingerprint-seeding scan");
    }
    assert_eq!(inventory.count(), REPOSITORY_COUNT * 2);

    service
        .note_managed_creation("project-0", &managed_path)
        .await;
    projections.set_project(project(
        "project-0",
        primary_paths[0]
            .to_str()
            .expect("fleet primary path is UTF-8"),
        [thread(
            "thread-adopted",
            adopted_path.to_str().expect("adopted path is UTF-8"),
        )],
    ));
    for elapsed_seconds in 1..=WORKLOAD_SECONDS {
        tokio::time::advance(Duration::from_secs(1)).await;
        for index in 0..REPOSITORY_COUNT {
            let snapshot = service
                .refresh(&format!("project-{index}"), CatalogRefreshTrigger::Focus)
                .await
                .expect("fleet focus refresh");
            if elapsed_seconds == 1 && index == 0 {
                assert!(
                    !snapshot
                        .worktrees
                        .iter()
                        .find(|worktree| worktree.branch.as_deref() == Some("feature/managed"))
                        .expect("managed worktree descriptor")
                        .eligible_for_adoption
                );
                assert_eq!(
                    adopted(&snapshot, "thread-adopted").availability,
                    super::AdoptedWorktreeAvailability::MissingRegistered
                );
            }
        }
        let reconciliations = elapsed_seconds / RECONCILIATION_SECONDS;
        assert_eq!(
            inventory.count(),
            REPOSITORY_COUNT * (2 + reconciliations as usize),
            "only each exact five-minute boundary runs fleet inventory"
        );
    }
    assert_eq!(
        fingerprint.count(),
        REPOSITORY_COUNT * (WORKLOAD_SECONDS as usize + 1)
    );

    run_git(
        &primary_paths[0],
        &["config", "coddy-studio.fingerprint", "changed"],
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-0", CatalogRefreshTrigger::Focus)
        .await
        .expect("real repository mutation runs inventory immediately");
    assert_eq!(
        inventory.count(),
        REPOSITORY_COUNT * (2 + reconciliation_count as usize) + 1
    );

    std::fs::write(
        common_dirs[1].join("config.worktree"),
        vec![b'#'; 64 * 1024 + 1],
    )
    .expect("oversized optional fingerprint input");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("native Unknown fingerprint runs inventory immediately");
    assert_eq!(
        inventory.count(),
        REPOSITORY_COUNT * (2 + reconciliation_count as usize) + 2
    );
    assert_eq!(
        fingerprint.count(),
        REPOSITORY_COUNT * (WORKLOAD_SECONDS as usize + 1) + 2
    );

    drop(subscriptions);
    shutdown_fingerprint_service(&service).await;
    drop(service);
    drop(fingerprint);
    drop(fixtures);
}

#[tokio::test]
async fn fingerprint_captured_before_inventory_cannot_hide_mid_scan_mutation() {
    let inventory = Arc::new(PausingSecondOnlyInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let fingerprint = Arc::new(MutableFingerprintSource::new(FingerprintOutcome::Known(
        known,
    )));
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        fingerprint.clone(),
        CatalogServiceOptions {
            result_ttl: Duration::from_millis(10),
            ..CatalogServiceOptions::default()
        },
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let scan_service = service.clone();
    let scan = tokio::spawn(async move {
        scan_service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), wait_for_count(&inventory.calls, 2))
        .await
        .expect("second inventory reached its bounded pause");

    let worker_release = service.pause_mutation_refresh_starts_after_for_test(0);
    service.invalidate_after_mutation("project-1").await;
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_mutation_refresh_worker_starts(&service, 1),
    )
    .await
    .expect("mutation refresh worker reached its bounded pause");
    inventory.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(5), scan)
        .await
        .expect("stale scan completed after release")
        .expect("scan task")
        .expect_err("mid-scan invalidation rejects the stale generation");
    assert_eq!(error.reason, super::CatalogErrorReason::StaleGeneration);
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("post-mutation focus refresh");

    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
    worker_release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_active_mutation_refresh_workers(&service, 0),
    )
    .await
    .expect("mutation refresh worker drained");
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint);
    drop(fingerprint_fixture);
}

#[tokio::test]
async fn fingerprint_invalidation_drops_a_pending_native_lease_and_blocks_late_publication() {
    let (repository_fixture, primary, common_dir) = test_repository_fixture();
    let repository_inventory = GitWorktreeInventory {
        common_dir: common_dir.clone(),
        records: vec![GitWorktreeRecord {
            path: primary.clone(),
            head: Some("abc123".to_owned()),
            branch: Some("main".to_owned()),
            is_primary: true,
            is_bare: false,
            locked: false,
            lock_reason: None,
            is_prunable: false,
            prunable_reason: None,
        }],
        nul_delimited: true,
    };
    let inventory = Arc::new(FakeInventorySource::new(
        (0..3).map(|_| repository_inventory.clone()),
    ));
    let filesystem = Arc::new(PausingNextShallowFileSystem::new([
        (primary.clone(), DirectoryProbeState::Present),
        (common_dir.clone(), DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            primary.to_str().expect("UTF-8 fixture path"),
            [],
        )])),
        inventory.clone(),
        filesystem.clone(),
        CatalogServiceOptions {
            poll_interval: Duration::from_secs(60 * 60),
            result_ttl: Duration::from_millis(10),
            ..CatalogServiceOptions::default()
        },
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::sleep(Duration::from_millis(20)).await;
    filesystem.pause_next();
    let scan_service = service.clone();
    let scan = tokio::spawn(async move {
        scan_service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), filesystem.wait_until_paused())
        .await
        .expect("native fingerprint reached pending-publication pause");

    let moved_common_dir = repository_fixture.path().join("common-dir-moved");
    #[cfg(windows)]
    assert!(
        std::fs::rename(&common_dir, &moved_common_dir).is_err(),
        "the pending native fingerprint must retain its common-directory lease"
    );
    let worker_release = service.pause_mutation_refresh_starts_after_for_test(0);
    service.invalidate_after_mutation("project-1").await;
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_mutation_refresh_worker_starts(&service, 1),
    )
    .await
    .expect("mutation worker reached its bounded pause");
    std::fs::rename(&common_dir, &moved_common_dir)
        .expect("invalidation releases the pending common-directory lease");
    std::fs::rename(&moved_common_dir, &common_dir).expect("restore common directory");

    filesystem.release();
    let error = tokio::time::timeout(Duration::from_secs(10), scan)
        .await
        .expect("invalidated native scan completed after release")
        .expect("native scan task")
        .expect_err("pending native scan is stale after invalidation");
    assert_eq!(error.reason, super::CatalogErrorReason::StaleGeneration);
    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("post-invalidation Focus performs a real scan");
    assert_eq!(inventory.calls().len(), 3);

    worker_release.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_active_mutation_refresh_workers(&service, 0),
    )
    .await
    .expect("mutation refresh worker drained");
    let moved_after_shutdown = repository_fixture.path().join("published-common-dir-moved");
    #[cfg(windows)]
    assert!(
        std::fs::rename(&common_dir, &moved_after_shutdown).is_err(),
        "the published fingerprint remains repository-owned before shutdown"
    );
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(service);
    drop(filesystem);
    std::fs::rename(&common_dir, &moved_after_shutdown)
        .expect("repository shutdown drops the published common-directory lease");
    std::fs::rename(&moved_after_shutdown, &common_dir).expect("restore after shutdown");
    drop(repository_fixture);
}

#[tokio::test]
async fn fingerprint_final_owner_release_drops_lease_before_eviction_and_forces_a_real_scan() {
    let (repository_fixture, primary, common_dir) = test_repository_fixture();
    let repository_inventory = GitWorktreeInventory {
        common_dir: common_dir.clone(),
        records: vec![GitWorktreeRecord {
            path: primary.clone(),
            head: Some("abc123".to_owned()),
            branch: Some("main".to_owned()),
            is_primary: true,
            is_bare: false,
            locked: false,
            lock_reason: None,
            is_prunable: false,
            prunable_reason: None,
        }],
        nul_delimited: true,
    };
    let inventory = Arc::new(FakeInventorySource::new(
        (0..3).map(|_| repository_inventory.clone()),
    ));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            primary.to_str().expect("UTF-8 fixture path"),
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::from_owned([
            (
                primary.to_string_lossy().into_owned(),
                DirectoryProbeState::Present,
            ),
            (
                common_dir.to_string_lossy().into_owned(),
                DirectoryProbeState::Present,
            ),
        ])),
        CatalogServiceOptions {
            idle_eviction: Duration::from_secs(60),
            poll_interval: Duration::from_secs(60 * 60),
            result_ttl: Duration::from_millis(10),
            ..CatalogServiceOptions::default()
        },
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::sleep(Duration::from_millis(20)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("seed native fingerprint while subscriber owns repository");
    assert_eq!(inventory.calls().len(), 2);

    let moved_common_dir = repository_fixture
        .path()
        .join("final-owner-common-dir-moved");
    #[cfg(windows)]
    assert!(
        std::fs::rename(&common_dir, &moved_common_dir).is_err(),
        "the active repository owner retains the published fingerprint lease"
    );
    drop(subscription);
    std::fs::rename(&common_dir, &moved_common_dir)
        .expect("final subscriber release drops the lease before idle eviction");
    std::fs::rename(&moved_common_dir, &common_dir).expect("restore common directory");

    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("immediate unary reattachment performs a real scan");
    assert_eq!(inventory.calls().len(), 3);
    std::fs::rename(&common_dir, &moved_common_dir)
        .expect("final unary release also drops the newly published lease");
    std::fs::rename(&moved_common_dir, &common_dir).expect("restore after unary release");

    shutdown_fingerprint_service(&service).await;
    drop(service);
    drop(repository_fixture);
}

#[tokio::test]
async fn fingerprint_observation_cannot_repopulate_proof_after_final_owner_release() {
    let (repository_fixture, primary, common_dir) = test_repository_fixture();
    let repository_inventory = GitWorktreeInventory {
        common_dir: common_dir.clone(),
        records: vec![GitWorktreeRecord {
            path: primary.clone(),
            head: Some("abc123".to_owned()),
            branch: Some("main".to_owned()),
            is_primary: true,
            is_bare: false,
            locked: false,
            lock_reason: None,
            is_prunable: false,
            prunable_reason: None,
        }],
        nul_delimited: true,
    };
    let inventory = Arc::new(FakeInventorySource::new(
        (0..4).map(|_| repository_inventory.clone()),
    ));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            primary.to_str().expect("UTF-8 fixture path"),
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::from_owned([
            (
                primary.to_string_lossy().into_owned(),
                DirectoryProbeState::Present,
            ),
            (
                common_dir.to_string_lossy().into_owned(),
                DirectoryProbeState::Present,
            ),
        ])),
        CatalogServiceOptions {
            idle_eviction: Duration::from_secs(60),
            poll_interval: Duration::from_secs(60 * 60),
            result_ttl: Duration::from_millis(10),
            ..CatalogServiceOptions::default()
        },
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let repository_key = subscription.latest().repository_key.clone();
    tokio::time::sleep(Duration::from_millis(20)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("seed native fingerprint");
    let (publication_entered, publication_resume, publication_finished) =
        service.pause_next_repository_observation_publication_for_test(&repository_key);
    let scan_service = service.clone();
    let scan = tokio::spawn(async move {
        scan_service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), publication_entered.acquire())
        .await
        .expect("physical observation reached publication pause")
        .expect("publication pause remains open")
        .forget();

    drop(subscription);
    scan.abort();
    let aborted = scan.await.expect_err("refresh caller is aborted");
    assert!(aborted.is_cancelled());
    assert_eq!(
        service.repository_fingerprint_ownership_for_test("project-1"),
        Some((0, false, false))
    );

    publication_resume.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), publication_finished.acquire())
        .await
        .expect("physical observation completed publication attempt")
        .expect("publication completion remains open")
        .forget();
    assert_eq!(
        service.repository_fingerprint_ownership_for_test("project-1"),
        Some((0, false, false))
    );
    let moved_common_dir = repository_fixture
        .path()
        .join("post-owner-race-common-dir-moved");
    std::fs::rename(&common_dir, &moved_common_dir)
        .expect("resumed ownerless observation cannot retain a fingerprint lease");
    std::fs::rename(&moved_common_dir, &common_dir).expect("restore common directory");

    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("immediate new lifecycle performs a real scan");
    assert_eq!(inventory.calls().len(), 4);
    std::fs::rename(&common_dir, &moved_common_dir)
        .expect("final unary release drops the new lifecycle proof");
    std::fs::rename(&moved_common_dir, &common_dir).expect("restore after unary release");

    shutdown_fingerprint_service(&service).await;
    drop(service);
    drop(repository_fixture);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_matching_non_focus_triggers_always_run_inventory() {
    let inventory =
        Arc::new(FakeInventorySource::new((0..7).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        Arc::new(MutableFingerprintSource::new(FingerprintOutcome::Known(
            known,
        ))),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("fingerprint-seeding scan");

    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::FirstSubscriber)
        .await
        .expect("post-TTL first-subscriber refresh");

    for trigger in [
        CatalogRefreshTrigger::Explicit,
        CatalogRefreshTrigger::MetadataChanged,
        CatalogRefreshTrigger::AvailabilityChanged,
        CatalogRefreshTrigger::Mutation,
    ] {
        service
            .refresh("project-1", trigger)
            .await
            .expect("non-focus authoritative refresh");
    }

    assert_eq!(inventory.calls().len(), 7);
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint_fixture);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_never_extends_a_degraded_observation() {
    let healthy = inventory("/repo/common", [record("/repo/main", true)]);
    let inventory = Arc::new(FakeInventorySource::new_results([
        Ok(healthy.clone()),
        Ok(healthy.clone()),
        Err(ScanFailure {
            reason: super::CatalogDegradedReason::GitFailed,
            message: "injected inventory failure".to_owned(),
        }),
        Ok(healthy),
    ]));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        Arc::new(MutableFingerprintSource::new(FingerprintOutcome::Known(
            known,
        ))),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("fingerprint-seeding scan");
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("degraded observation");
    tokio::time::advance(Duration::from_secs(1)).await;

    service
        .refresh("project-1", CatalogRefreshTrigger::Focus)
        .await
        .expect("failure recovery scan");

    assert_eq!(inventory.calls().len(), 4);
    drop(subscription);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint_fixture);
}

#[tokio::test(start_paused = true)]
async fn fingerprint_uses_repository_epoch_and_retained_inventory_paths() {
    let inventory = Arc::new(FakeInventorySource::new((0..3).map(|_| {
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/linked", false)],
        )
    })));
    let (fingerprint_fixture, known) = known_test_fingerprint().await;
    let fingerprint = Arc::new(FakeFingerprintSource::new(
        (0..3).map(|_| FingerprintOutcome::Known(known.clone())),
    ));
    let service = WorktreeCatalogService::with_dependencies_and_fingerprint(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/linked", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        fingerprint.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::time::advance(Duration::from_secs(1)).await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("fingerprint-seeding scan");
    drop(subscription);

    service.invalidate_after_mutation("project-1").await;
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("post-invalidation scan");

    let requests = fingerprint.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].common_dir, Path::new("/repo/common"));
    assert_eq!(requests[0].primary_path, Path::new("/repo/main"));
    assert_eq!(
        requests[0].known_worktree_paths,
        [PathBuf::from("/repo/main"), PathBuf::from("/repo/linked")]
    );
    assert_eq!(requests[0].repository_lifecycle_epoch, 1);
    assert_eq!(requests[0].mutation_epoch, 0);
    assert_eq!(requests[1].repository_lifecycle_epoch, 2);
    assert_eq!(requests[1].mutation_epoch, 1);
    shutdown_fingerprint_service(&service).await;
    drop(fingerprint);
    drop(known);
    drop(fingerprint_fixture);
}

#[tokio::test(start_paused = true)]
async fn managed_creation_suppression_expires_after_thirty_seconds() {
    let inventory = Arc::new(FakeInventorySource::new((0..3).map(|_| {
        inventory(
            "/repo/common",
            [record("/repo/main", true), record("/repo/managed", false)],
        )
    })));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/managed", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    service
        .note_managed_creation("project-1", Path::new("/repo/managed"))
        .await;
    let suppressed = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("suppressed refresh");
    assert!(!descriptor(&suppressed, "/repo/managed").eligible_for_adoption);

    tokio::time::advance(Duration::from_secs(30)).await;
    let expired = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("post-suppression refresh");
    assert!(descriptor(&expired, "/repo/managed").eligible_for_adoption);
    drop(subscription);
}

#[tokio::test]
async fn joins_active_archived_panel_deleted_missing_and_conflicting_threads_on_the_server() {
    let projections = Arc::new(FakeProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [
            thread("active", "/repo/active"),
            archived_thread("archived", "/repo/archived"),
            panel_thread("panel", "/repo/panel"),
            deleted_thread("deleted", "/repo/deleted"),
            thread("missing-registered", "/repo/missing"),
            thread("missing-unregistered", "/repo/absent"),
        ],
    )]));
    let inventory = Arc::new(FakeInventorySource::new((0..2).map(|_| {
        inventory(
            "/repo/common",
            [
                record("/repo/main", true),
                record("/repo/active", false),
                record("/repo/archived", false),
                record("/repo/panel", false),
                record("/repo/deleted", false),
                record("/repo/missing", false),
            ],
        )
    })));
    let service = WorktreeCatalogService::with_dependencies(
        projections.clone(),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/active", DirectoryProbeState::Present),
            ("/repo/archived", DirectoryProbeState::Present),
            ("/repo/panel", DirectoryProbeState::Present),
            ("/repo/deleted", DirectoryProbeState::Present),
            ("/repo/missing", DirectoryProbeState::Missing),
            ("/repo/absent", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let snapshot = subscription.latest();
    assert_eq!(
        descriptor(&snapshot, "/repo/active")
            .adopted_thread_id
            .as_deref(),
        Some("active")
    );
    assert_eq!(
        descriptor(&snapshot, "/repo/archived").adoption_state,
        super::WorktreeAdoptionState::Archived
    );
    assert!(descriptor(&snapshot, "/repo/panel").eligible_for_adoption);
    assert!(descriptor(&snapshot, "/repo/deleted").eligible_for_adoption);
    assert_eq!(
        adopted(&snapshot, "missing-registered").availability,
        super::AdoptedWorktreeAvailability::MissingRegistered
    );
    assert_eq!(
        adopted(&snapshot, "missing-unregistered").availability,
        super::AdoptedWorktreeAvailability::MissingUnregistered
    );

    projections.set_project(project(
        "project-1",
        "/repo/main",
        [
            thread("active", "/repo/active"),
            thread("conflict", "/repo/active"),
        ],
    ));
    let error = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect_err("duplicate canonical ownership must be explicit");
    assert_eq!(error.reason, super::CatalogErrorReason::Internal);
    let retained = service.latest("project-1").await.expect("retained catalog");
    assert!(retained.authoritative);
    assert!(matches!(
        retained.scan_status,
        super::CatalogScanStatus::Ready
    ));
    assert_eq!(retained.worktrees, snapshot.worktrees);
}

#[tokio::test]
async fn catalog_joins_an_owner_and_inventory_record_by_physical_workspace_identity() {
    let root = tempfile::tempdir().expect("catalog identity root");
    let main = root.path().join("main");
    let physical = root.path().join("physical-worktree");
    let common_dir = root.path().join("common");
    std::fs::create_dir(&main).expect("main checkout");
    std::fs::create_dir(&physical).expect("physical worktree");
    std::fs::create_dir(&common_dir).expect("common Git directory");
    let alias = root.path().join("worktree-alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&physical, &alias).expect("worktree alias");
    #[cfg(windows)]
    junction::create(&physical, &alias).expect("worktree junction alias");
    let projections = Arc::new(FakeProjectionSource::new([(
        "project-1".to_owned(),
        CatalogProject {
            workspace_root: main.clone(),
            baseline_paths: Vec::new(),
            threads: vec![CatalogThread {
                thread_id: "owner".to_owned(),
                kind: "workspace".to_owned(),
                worktree_path: Some(alias.clone()),
                branch: Some("feature".to_owned()),
                archived: false,
                deleted: false,
            }],
            repository_key: None,
        },
    )]));
    let inventory = GitWorktreeInventory {
        common_dir,
        records: vec![
            GitWorktreeRecord {
                path: main,
                head: Some("abc123".to_owned()),
                branch: Some("main".to_owned()),
                is_primary: true,
                is_bare: false,
                locked: false,
                lock_reason: None,
                is_prunable: false,
                prunable_reason: None,
            },
            GitWorktreeRecord {
                path: physical.clone(),
                head: Some("def456".to_owned()),
                branch: Some("feature".to_owned()),
                is_primary: false,
                is_bare: false,
                locked: false,
                lock_reason: None,
                is_prunable: false,
                prunable_reason: None,
            },
        ],
        nul_delimited: true,
    };
    let service = WorktreeCatalogService::with_dependencies(
        projections,
        Arc::new(FakeInventorySource::new([inventory])),
        Arc::new(TokioCatalogFileSystem),
        CatalogServiceOptions::default(),
    );

    let subscription = service.subscribe("project-1").await.expect("catalog");
    let canonical = std::fs::canonicalize(&physical).expect("canonical physical worktree");
    let snapshot = subscription.latest();
    let descriptor = snapshot
        .worktrees
        .iter()
        .find(|worktree| {
            std::fs::canonicalize(&worktree.path).is_ok_and(|candidate| candidate == canonical)
        })
        .expect("physical worktree descriptor");
    assert_eq!(descriptor.path, physical.to_string_lossy());
    assert_eq!(descriptor.adopted_thread_id.as_deref(), Some("owner"));
    assert!(!descriptor.eligible_for_adoption);
    #[cfg(windows)]
    junction::delete(&alias).expect("remove worktree junction alias");
}

#[tokio::test]
async fn current_owner_path_wins_over_previous_snapshot_fallback_after_retarget() {
    let projections = Arc::new(FakeProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [thread("owner", "/repo/old")],
    )]));
    let observed_inventory = inventory(
        "/repo/common",
        [
            record("/repo/main", true),
            record("/repo/old", false),
            record("/repo/target", false),
        ],
    );
    let service = WorktreeCatalogService::with_dependencies(
        projections.clone(),
        Arc::new(FakeInventorySource::new([
            observed_inventory.clone(),
            observed_inventory,
        ])),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/old", DirectoryProbeState::Present),
            ("/repo/target", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("catalog");
    let initial = subscription.latest();
    assert_eq!(
        descriptor(&initial, "/repo/old")
            .adopted_thread_id
            .as_deref(),
        Some("owner")
    );

    projections.set_project(project(
        "project-1",
        "/repo/main",
        [thread("owner", "/repo/target")],
    ));
    let retargeted = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("retargeted catalog");

    assert_eq!(
        descriptor(&retargeted, "/repo/old").adopted_thread_id,
        None,
        "the previous-snapshot fallback must not reclaim the old path"
    );
    assert_eq!(
        descriptor(&retargeted, "/repo/target")
            .adopted_thread_id
            .as_deref(),
        Some("owner")
    );
    assert_eq!(adopted(&retargeted, "owner").path, "/repo/target");
}

#[tokio::test]
async fn concurrent_refresh_waiters_receive_the_same_ownership_conflict() {
    let projections = Arc::new(FakeProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [thread("owner", "/repo/external")],
    )]));
    let inventory = Arc::new(PausingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true), record("/repo/external", false)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        projections.clone(),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/external", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let _subscription = service.subscribe("project-1").await.expect("subscription");
    projections.set_project(project(
        "project-1",
        "/repo/main",
        [
            thread("owner", "/repo/external"),
            thread("conflict", "/repo/external"),
        ],
    ));

    let first = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;
    let second = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    tokio::task::yield_now().await;
    inventory.release.add_permits(1);

    let first = first
        .await
        .expect("first refresh task")
        .expect_err("first refresh must report the ownership conflict");
    let second = second
        .await
        .expect("second refresh task")
        .expect_err("coalesced refresh must report the same ownership conflict");
    assert_eq!(second, first);
}

#[tokio::test]
async fn filesystem_probe_only_reports_directories_as_present() {
    let root = tempfile::tempdir().expect("probe fixture");
    let directory = root.path().join("directory");
    let file = root.path().join("file");
    std::fs::create_dir(&directory).expect("probe directory");
    std::fs::write(&file, "not a directory").expect("probe file");
    let filesystem = super::service::TokioCatalogFileSystem;

    assert_eq!(
        filesystem.probe(directory).await,
        DirectoryProbeState::Present
    );
    assert_eq!(filesystem.probe(file).await, DirectoryProbeState::Unknown);
    assert_eq!(
        filesystem.probe(root.path().join("missing")).await,
        DirectoryProbeState::Missing
    );
}

#[tokio::test(start_paused = true)]
async fn polling_reads_only_shallow_common_git_metadata_and_known_paths_then_stops_and_evicts() {
    let inventory = Arc::new(FakeInventorySource::new([inventory(
        "/repo/common",
        [record("/repo/main", true), record("/repo/external", false)],
    )]));
    let filesystem = Arc::new(FakeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/external", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    tokio::task::yield_now().await;
    assert_eq!(service.active_poller_count_for_test(), 1);
    let calls = filesystem.shallow_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, PathBuf::from("/repo/common"));
    assert_eq!(
        calls[0].1.iter().cloned().collect::<HashSet<_>>(),
        [PathBuf::from("/repo/main"), PathBuf::from("/repo/external")]
            .into_iter()
            .collect()
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        inventory.calls().len(),
        1,
        "unchanged signatures must not run Git"
    );

    drop(subscription);
    let poll_calls = filesystem.shallow_calls().len();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.active_poller_count_for_test(), 0);
    assert_eq!(filesystem.shallow_calls().len(), poll_calls);
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.entry_count_for_test(), 0);
}

#[tokio::test(start_paused = true)]
async fn final_unsubscribe_interrupts_an_in_progress_shallow_signature() {
    let filesystem = Arc::new(BlockingAfterFirstShallowFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true)],
        )])),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    wait_for_count(&filesystem.active, 1).await;
    drop(subscription);
    wait_for_count(&filesystem.interrupted, 1).await;

    assert_eq!(service.active_poller_count_for_test(), 0);
    assert_eq!(filesystem.active.load(Ordering::SeqCst), 0);
    assert_eq!(filesystem.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn final_catalog_user_cancels_in_progress_git_without_publishing_a_result() {
    let inventory = Arc::new(CancellationAwareInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;

    drop(subscription);
    tokio::task::yield_now().await;
    assert_eq!(inventory.cancellations.load(Ordering::SeqCst), 0);
    assert!(
        !refresh.is_finished(),
        "the unary refresh remains an active catalog user after final unsubscribe"
    );
    refresh.abort();
    assert!(
        refresh
            .await
            .expect_err("unary refresh task is cancelled")
            .is_cancelled()
    );
    wait_for_count(&inventory.cancellations, 1).await;
    assert_eq!(
        service
            .latest("project-1")
            .await
            .expect("latest")
            .generation,
        1
    );

    let recovered = service
        .subscribe("project-1")
        .await
        .expect("a later subscriber restarts cancelled work");
    assert!(recovered.latest().authoritative);
    assert_eq!(recovered.latest().generation, 2);
}

#[tokio::test]
async fn final_catalog_user_interrupts_in_progress_directory_probes() {
    let filesystem = Arc::new(SwitchableBlockingProbeFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/external", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory(
                "/repo/common",
                [record("/repo/main", true), record("/repo/external", false)],
            )
        }))),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    filesystem.block.store(true, Ordering::SeqCst);
    let refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&filesystem.active, 1).await;

    drop(subscription);
    tokio::task::yield_now().await;
    assert_eq!(filesystem.interrupted.load(Ordering::SeqCst), 0);
    assert!(
        !refresh.is_finished(),
        "the unary refresh remains an active catalog user after final unsubscribe"
    );
    refresh.abort();
    assert!(
        refresh
            .await
            .expect_err("unary refresh task is cancelled")
            .is_cancelled()
    );
    wait_for_count(&filesystem.interrupted, 1).await;
    assert_eq!(filesystem.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        service
            .latest("project-1")
            .await
            .expect("latest")
            .generation,
        1
    );
}

#[tokio::test(start_paused = true)]
async fn aborting_subscription_during_poller_initialization_releases_its_reservation() {
    let filesystem = Arc::new(BlockingShallowFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true)],
        )])),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscribing = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    wait_for_count(&filesystem.shallow_calls, 1).await;

    subscribing.abort();
    match subscribing.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("subscription task must be aborted"),
    }
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;

    assert_eq!(service.active_poller_count_for_test(), 0);
    assert_eq!(service.entry_count_for_test(), 0);
}

#[tokio::test(start_paused = true)]
async fn second_subscriber_keeps_initialization_and_polling_alive_when_the_first_aborts() {
    let filesystem = Arc::new(BlockingFirstShallowFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let inventory =
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        })));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let first = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    wait_for_count(&filesystem.calls, 1).await;
    let second = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    tokio::task::yield_now().await;

    first.abort();
    match first.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("first subscription must be aborted"),
    }
    filesystem.release.add_permits(1);
    let mut second = second
        .await
        .expect("second subscription task")
        .expect("second subscriber completes shared initialization");
    assert_eq!(service.active_poller_count_for_test(), 1);

    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..100 {
        if inventory.calls().len() >= 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let observed = loop {
        let snapshot = second.changed().await.expect("ongoing poll publication");
        if snapshot.authoritative && snapshot.generation == 2 {
            break snapshot;
        }
    };
    assert!(observed.authoritative);
    assert_eq!(observed.generation, 2);
    assert_eq!(inventory.calls().len(), 2);
}

#[tokio::test]
async fn immediate_reattach_ignores_a_cancelled_refresh_from_the_prior_lifecycle() {
    let inventory = Arc::new(DelayedCancellationInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let first = service
        .subscribe("project-1")
        .await
        .expect("first lifecycle");
    let stale_refresh = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 2).await;
    drop(first);
    assert!(
        !stale_refresh.is_finished(),
        "the unary request keeps the prior lifecycle active after unsubscribe"
    );
    stale_refresh.abort();
    wait_for_count(&inventory.cancelled, 1).await;

    let reattach = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    tokio::task::yield_now().await;
    assert!(!reattach.is_finished(), "old refresh is still unwinding");
    inventory.release.add_permits(1);

    assert!(
        stale_refresh
            .await
            .expect_err("prior unary refresh task is cancelled")
            .is_cancelled()
    );
    let current = reattach
        .await
        .expect("reattach task")
        .expect("new lifecycle restarts work");
    assert!(current.latest().authoritative);
    assert_eq!(current.latest().generation, 2);
    assert_eq!(inventory.calls.load(Ordering::SeqCst), 3);
    assert_eq!(service.active_poller_count_for_test(), 1);
}

#[tokio::test(start_paused = true)]
async fn unary_refresh_participates_in_idle_lifecycle_and_evicts_after_final_release() {
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        }))),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );

    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("unary refresh");
    assert_eq!(service.entry_count_for_test(), 1);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.entry_count_for_test(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        service.entry_count_for_test(),
        0,
        "the final unary user must schedule the same pointer-checked idle eviction as a subscriber"
    );
}

#[tokio::test(start_paused = true)]
async fn unary_reuse_at_the_idle_deadline_survives_until_the_cancelled_user_releases() {
    let inventory = Arc::new(PausingAfterTwoInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("initial unary refresh");
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;

    let reusing = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.calls, 3).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        service.entry_count_for_test(),
        1,
        "an active unary user cancels the old pointer-checked eviction"
    );

    reusing.abort();
    assert!(
        reusing
            .await
            .expect_err("unary task is cancelled")
            .is_cancelled()
    );
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.entry_count_for_test(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        service.entry_count_for_test(),
        0,
        "cancelling the final unary user starts a fresh full idle window"
    );
}

#[tokio::test(start_paused = true)]
async fn subscriber_attachment_at_the_idle_deadline_cannot_join_an_evicted_entry() {
    let filesystem = Arc::new(BlockingAfterFirstShallowFileSystem::new([
        ("/repo/main", DirectoryProbeState::Present),
        ("/repo/common", DirectoryProbeState::Present),
    ]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true)],
        )])),
        filesystem.clone(),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    drop(subscription);
    tokio::time::advance(Duration::from_secs(59)).await;

    let attaching = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    wait_for_count(&filesystem.active, 1).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.entry_count_for_test(), 1);

    attaching.abort();
    match attaching.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("attachment task must be aborted"),
    }
    wait_for_count(&filesystem.interrupted, 1).await;
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.entry_count_for_test(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_reattach_cannot_observe_partially_released_repository_ownership() {
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        }))),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let (entered, resume) = service.pause_next_final_release_for_test();
    let dropping = tokio::spawn(async move { drop(subscription) });
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .expect("release reaches deterministic pause");

    let reattach = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    let attached_before_repository_release = reattach.is_finished();
    resume.wait();
    dropping.await.expect("release task");

    assert!(
        !attached_before_repository_release,
        "reattach must wait until view and repository ownership release atomically"
    );
    let reattached = reattach
        .await
        .expect("reattach task")
        .expect("reattach after atomic release");
    assert!(reattached.latest().authoritative);
    assert_eq!(reattached.latest().generation, 2);
}

#[tokio::test(start_paused = true)]
async fn mutation_lock_survives_view_eviction_and_serializes_same_repository_projects() {
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([
            project("project-a", "/repo/main", []),
            project("project-b", "/repo/main", []),
        ])),
        Arc::new(FakeInventorySource::new((0..2).map(|_| {
            inventory("/repo/common", [record("/repo/main", true)])
        }))),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let project_a = service.subscribe("project-a").await.expect("project A");
    let project_b = service.subscribe("project-b").await.expect("project B");
    drop(project_a);
    drop(project_b);

    let first_entered = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let first = {
        let service = service.clone();
        let first_entered = Arc::clone(&first_entered);
        let release_first = Arc::clone(&release_first);
        tokio::spawn(async move {
            service
                .with_project_mutation_lock("project-a", || async move {
                    first_entered.notify_one();
                    release_first.notified().await;
                })
                .await;
        })
    };
    first_entered.notified().await;
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert_eq!(service.entry_count_for_test(), 0);

    let second_entered = Arc::new(AtomicUsize::new(0));
    let second = {
        let service = service.clone();
        let second_entered = Arc::clone(&second_entered);
        tokio::spawn(async move {
            service
                .with_project_mutation_lock("project-b", || async move {
                    second_entered.fetch_add(1, Ordering::SeqCst);
                })
                .await;
        })
    };
    tokio::task::yield_now().await;
    assert_eq!(second_entered.load(Ordering::SeqCst), 0);

    release_first.notify_one();
    first.await.expect("first mutation task");
    second.await.expect("second mutation task");
    assert_eq!(second_entered.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn project_mutation_lock_remains_stable_while_bootstrap_establishes_the_repository_pin() {
    let projections = Arc::new(FakeProjectionSource::new([project(
        "project-1",
        "/repo/main",
        [],
    )]));
    let service = WorktreeCatalogService::with_dependencies(
        projections.clone(),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true)],
        )])),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let first_entered = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let first = {
        let service = service.clone();
        let first_entered = Arc::clone(&first_entered);
        let release_first = Arc::clone(&release_first);
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        tokio::spawn(async move {
            service
                .with_project_mutation_lock("project-1", || async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    first_entered.notify_one();
                    release_first.notified().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                })
                .await;
        })
    };
    first_entered.notified().await;

    let subscription = service
        .subscribe("project-1")
        .await
        .expect("bootstrap establishes pin");
    assert!(
        projections
            .projects
            .lock()
            .expect("projects")
            .get("project-1")
            .and_then(|project| project.repository_key.as_ref())
            .is_some(),
        "bootstrap must establish the repository pin before request B"
    );
    let second_entered = Arc::new(Notify::new());
    let second = {
        let service = service.clone();
        let second_entered = Arc::clone(&second_entered);
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        tokio::spawn(async move {
            service
                .with_project_mutation_lock("project-1", || async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now, Ordering::SeqCst);
                    active.fetch_sub(1, Ordering::SeqCst);
                    second_entered.notify_one();
                })
                .await;
        })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(25), second_entered.notified())
            .await
            .is_err(),
        "request B must wait on the same stable project lock"
    );

    release_first.notify_one();
    first.await.expect("first request");
    second.await.expect("second request");
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    drop(subscription);
}

#[tokio::test]
async fn shutdown_cancels_inflight_bootstrap_and_prevents_late_entry_insertion() {
    let inventory = Arc::new(ShutdownAwareInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([(
            "/repo/main",
            DirectoryProbeState::Present,
        )])),
        CatalogServiceOptions::default(),
    );
    let subscribing = {
        let service = service.clone();
        tokio::spawn(async move { service.subscribe("project-1").await })
    };
    wait_for_count(&inventory.calls, 1).await;

    service.shutdown().await;

    let error = match subscribing.await.expect("subscription task") {
        Ok(_) => panic!("shutdown must cancel bootstrap"),
        Err(error) => error,
    };
    assert_eq!(
        error.reason,
        super::CatalogErrorReason::RepositoryUnavailable
    );
    assert_eq!(inventory.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(service.entry_count_for_test(), 0);
}

#[tokio::test]
async fn shutdown_drains_a_blocked_repository_observation_without_a_late_result() {
    let inventory = Arc::new(UncooperativeSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let _subscription = service.subscribe("project-1").await.expect("subscription");
    let refreshing = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.active, 1).await;

    service.shutdown().await;
    let active_after_shutdown = inventory.active.load(Ordering::SeqCst);
    inventory.release.add_permits(1);
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }

    assert_eq!(active_after_shutdown, 0, "shutdown must drain the leader");
    assert_eq!(
        inventory.completed.load(Ordering::SeqCst),
        0,
        "a drained observation cannot publish a late private result"
    );
    assert_eq!(service.active_background_task_count_for_test(), 0);
    assert!(refreshing.await.expect("refresh task").is_err());
}

#[tokio::test]
async fn active_background_count_includes_repository_observation_leaders() {
    let inventory = Arc::new(UncooperativeSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let before = service.active_background_task_count_for_test();
    let refreshing = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
        })
    };
    wait_for_count(&inventory.active, 1).await;

    assert_eq!(
        service.active_background_task_count_for_test(),
        before + 1,
        "the repository-owned leader must be visible to lifecycle accounting"
    );

    inventory.release.add_permits(1);
    refreshing.await.expect("refresh task").expect("refresh");
    drop(subscription);
}

#[tokio::test]
async fn panicking_repository_observation_reports_internal_and_unregisters() {
    let inventory = Arc::new(PanickingSecondInventorySource::new(inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory,
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let before = service.active_background_task_count_for_test();

    let error = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect_err("panicking leader must fail the refresh");

    assert_eq!(error.reason, super::CatalogErrorReason::Internal);
    assert_eq!(service.active_background_task_count_for_test(), before);
    drop(subscription);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn immediate_lifecycle_tasks_cannot_finish_before_registration() {
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([])),
        Arc::new(FakeInventorySource::new([])),
        Arc::new(FakeFileSystem::new([])),
        CatalogServiceOptions::default(),
    );

    tokio::time::timeout(
        Duration::from_secs(5),
        service.complete_immediate_lifecycle_tasks_for_test(1_000),
    )
    .await
    .expect("completed tasks must unregister without leaving shutdown-stalling handles");

    assert_eq!(service.active_background_task_count_for_test(), 0);
    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn shutdown_serializes_with_final_release_eviction_registration() {
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        Arc::new(FakeInventorySource::new([inventory(
            "/repo/common",
            [record("/repo/main", true)],
        )])),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let subscription = service.subscribe("project-1").await.expect("subscription");
    let (entered, resume) = service.pause_next_eviction_registration_for_test();
    let dropping = std::thread::spawn(move || drop(subscription));
    tokio::task::spawn_blocking(move || entered.wait())
        .await
        .expect("release reaches eviction registration");

    let (runtime_probe, runtime_observed) = std::sync::mpsc::sync_channel(1);
    tokio::spawn(async move {
        let _ = runtime_probe.send(());
    });
    if runtime_observed
        .recv_timeout(Duration::from_secs(1))
        .is_err()
    {
        resume.wait();
        dropping.join().expect("subscription drop thread");
        panic!("a paused subscription release must not consume the only Tokio worker");
    }

    let mut shutting_down = Box::pin(service.shutdown());
    let first_shutdown_poll = std::future::poll_fn(|context| {
        std::task::Poll::Ready(shutting_down.as_mut().poll(context))
    })
    .await;
    let shutdown_pending = first_shutdown_poll.is_pending();
    assert!(
        service.shutdown_started_for_test(),
        "shutdown must reach its terminal transition before release resumes"
    );
    resume.wait();
    dropping.join().expect("subscription drop thread");
    if shutdown_pending {
        shutting_down.await;
    }
    service.shutdown().await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }

    assert_eq!(
        service.eviction_task_registration_count_for_test(),
        0,
        "terminal shutdown must prevent a paused release from registering eviction"
    );
    assert_eq!(service.active_background_task_count_for_test(), 0);
    assert_eq!(service.entry_count_for_test(), 0);
}

#[tokio::test]
async fn shutdown_closes_live_subscriptions_is_idempotent_and_rejects_new_work() {
    let inventory = Arc::new(FakeInventorySource::new([inventory(
        "/repo/common",
        [record("/repo/main", true)],
    )]));
    let service = WorktreeCatalogService::with_dependencies(
        Arc::new(FakeProjectionSource::new([project(
            "project-1",
            "/repo/main",
            [],
        )])),
        inventory.clone(),
        Arc::new(FakeFileSystem::new([
            ("/repo/main", DirectoryProbeState::Present),
            ("/repo/common", DirectoryProbeState::Present),
        ])),
        CatalogServiceOptions::default(),
    );
    let mut subscription = service.subscribe("project-1").await.expect("subscription");

    service.shutdown().await;
    service.shutdown().await;

    assert_eq!(subscription.changed().await, None);
    drop(subscription);
    assert_eq!(service.active_background_task_count_for_test(), 0);
    assert!(service.subscribe("project-1").await.is_err());
    assert!(
        service
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
            .is_err()
    );
    service.invalidate_after_mutation("project-1").await;
    service
        .note_managed_creation("project-1", Path::new("/repo/new"))
        .await;
    assert_eq!(inventory.calls().len(), 1, "shutdown must be terminal");
    assert_eq!(service.entry_count_for_test(), 0);
}

#[derive(Clone)]
struct FakeProjectionSource {
    projects: Arc<Mutex<HashMap<String, CatalogProject>>>,
}

impl FakeProjectionSource {
    fn new(projects: impl IntoIterator<Item = (String, CatalogProject)>) -> Self {
        Self {
            projects: Arc::new(Mutex::new(projects.into_iter().collect())),
        }
    }

    fn set_project(&self, project: (String, CatalogProject)) {
        self.projects
            .lock()
            .expect("fake projects")
            .insert(project.0, project.1);
    }
}

impl CatalogProjectionSource for FakeProjectionSource {
    fn load(
        &self,
        project_id: String,
    ) -> CatalogFuture<Result<Option<CatalogProject>, super::CatalogError>> {
        let project = self
            .projects
            .lock()
            .expect("fake projects")
            .get(&project_id)
            .cloned();
        Box::pin(async move { Ok(project) })
    }

    fn pin_repository_key(
        &self,
        project_id: String,
        repository_key: String,
    ) -> CatalogFuture<Result<Option<super::service::CatalogPinOutcome>, super::CatalogError>> {
        let projects = Arc::clone(&self.projects);
        Box::pin(async move {
            let mut projects = projects.lock().expect("fake projects");
            let Some(project) = projects.get_mut(&project_id) else {
                return Ok(None);
            };
            let outcome = match project.repository_key.as_deref() {
                None => {
                    project.repository_key = Some(repository_key);
                    super::service::CatalogPinOutcome::Established
                }
                Some(pinned) if pinned == repository_key => {
                    super::service::CatalogPinOutcome::Matched
                }
                Some(pinned) => super::service::CatalogPinOutcome::Mismatch {
                    pinned_repository_key: pinned.to_owned(),
                },
            };
            Ok(Some(outcome))
        })
    }
}

#[derive(Clone)]
struct PausingSecondProjectionSource {
    inner: FakeProjectionSource,
    calls: Arc<AtomicUsize>,
}

impl PausingSecondProjectionSource {
    fn new(projects: impl IntoIterator<Item = (String, CatalogProject)>) -> Self {
        Self {
            inner: FakeProjectionSource::new(projects),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl CatalogProjectionSource for PausingSecondProjectionSource {
    fn load(
        &self,
        project_id: String,
    ) -> CatalogFuture<Result<Option<CatalogProject>, super::CatalogError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let load = self.inner.load(project_id);
        Box::pin(async move {
            if call == 2 {
                std::future::pending::<()>().await;
            }
            load.await
        })
    }

    fn pin_repository_key(
        &self,
        project_id: String,
        repository_key: String,
    ) -> CatalogFuture<Result<Option<super::service::CatalogPinOutcome>, super::CatalogError>> {
        self.inner.pin_repository_key(project_id, repository_key)
    }
}

struct FakeInventorySource {
    inventories: Mutex<VecDeque<Result<GitWorktreeInventory, ScanFailure>>>,
    calls: Mutex<Vec<PathBuf>>,
}

struct CountingInventorySource {
    inner: Arc<dyn InventorySource>,
    calls: AtomicUsize,
}

impl CountingInventorySource {
    fn new(inner: Arc<dyn InventorySource>) -> Self {
        Self {
            inner,
            calls: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl InventorySource for CountingInventorySource {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("native inventory runtime")
                    .block_on(inner.inventory(anchor, cancellation))
            })
            .await
            .expect("native inventory worker")
        })
    }
}

#[derive(Default)]
struct CountingNativeFingerprintSource {
    calls: AtomicUsize,
}

impl CountingNativeFingerprintSource {
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl CatalogFingerprintSource for CountingNativeFingerprintSource {
    fn read(&self, request: FingerprintRequest) -> CatalogFuture<FingerprintOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(read_catalog_repository_fingerprint(request))
    }
}

struct FakeFingerprintSource {
    outcomes: Mutex<VecDeque<FingerprintOutcome>>,
    requests: Mutex<Vec<FingerprintRequest>>,
}

impl FakeFingerprintSource {
    fn new(outcomes: impl IntoIterator<Item = FingerprintOutcome>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<FingerprintRequest> {
        self.requests.lock().expect("fingerprint requests").clone()
    }
}

impl CatalogFingerprintSource for FakeFingerprintSource {
    fn read(&self, request: FingerprintRequest) -> CatalogFuture<FingerprintOutcome> {
        self.requests
            .lock()
            .expect("fingerprint requests")
            .push(request);
        let outcome = self
            .outcomes
            .lock()
            .expect("fingerprint outcomes")
            .pop_front()
            .expect("configured fingerprint outcome");
        Box::pin(async move { outcome })
    }
}

struct MutableFingerprintSource {
    outcome: Mutex<FingerprintOutcome>,
}

impl MutableFingerprintSource {
    fn new(outcome: FingerprintOutcome) -> Self {
        Self {
            outcome: Mutex::new(outcome),
        }
    }
}

impl CatalogFingerprintSource for MutableFingerprintSource {
    fn read(&self, _request: FingerprintRequest) -> CatalogFuture<FingerprintOutcome> {
        let outcome = self.outcome.lock().expect("mutable fingerprint").clone();
        Box::pin(async move { outcome })
    }
}

fn test_repository_fixture() -> (TempDir, PathBuf, PathBuf) {
    let fixture = TempDir::new().expect("fingerprint fixture");
    let primary = fixture.path().join("primary");
    std::fs::create_dir(&primary).expect("fingerprint primary directory");
    for arguments in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "Coddy Studio Test"],
        vec!["config", "user.email", "coddy-studio-test@example.invalid"],
    ] {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&primary)
            .output()
            .expect("fingerprint fixture Git");
        assert!(output.status.success(), "fingerprint fixture Git failed");
    }
    std::fs::write(primary.join("README.md"), "fixture\n").expect("fingerprint fixture file");
    for arguments in [vec!["add", "README.md"], vec!["commit", "-m", "fixture"]] {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&primary)
            .output()
            .expect("fingerprint fixture Git");
        assert!(output.status.success(), "fingerprint fixture Git failed");
    }
    let primary = std::fs::canonicalize(primary).expect("canonical primary directory");
    let common_dir = std::fs::canonicalize(primary.join(".git")).expect("common directory");
    (fixture, primary, common_dir)
}

fn run_git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .expect("fleet Git command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn known_test_fingerprint() -> (TempDir, CatalogRepositoryFingerprint) {
    let (fixture, primary, common_dir) = test_repository_fixture();
    let fingerprint = tokio::time::timeout(
        Duration::from_secs(10),
        read_catalog_repository_fingerprint(FingerprintRequest {
            common_dir,
            primary_path: primary.clone(),
            known_worktree_paths: vec![primary],
            repository_lifecycle_epoch: 1,
            mutation_epoch: 0,
            cancellation: CancellationToken::new(),
        }),
    )
    .await
    .expect("native test fingerprint completed within ten seconds");
    let FingerprintOutcome::Known(fingerprint) = fingerprint else {
        panic!("fingerprint fixture must produce a known proof");
    };
    (fixture, fingerprint)
}

async fn shutdown_fingerprint_service(service: &WorktreeCatalogService) {
    tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .expect("fingerprint test service shutdown completed within five seconds");
}

impl FakeInventorySource {
    fn new(inventories: impl IntoIterator<Item = GitWorktreeInventory>) -> Self {
        Self {
            inventories: Mutex::new(inventories.into_iter().map(Ok).collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<PathBuf> {
        self.calls.lock().expect("inventory calls").clone()
    }

    fn new_results(
        inventories: impl IntoIterator<Item = Result<GitWorktreeInventory, ScanFailure>>,
    ) -> Self {
        Self {
            inventories: Mutex::new(inventories.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl InventorySource for FakeInventorySource {
    fn inventory(
        &self,
        anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        self.calls.lock().expect("inventory calls").push(anchor);
        let inventory = self
            .inventories
            .lock()
            .expect("fake inventories")
            .pop_front()
            .expect("configured fake inventory");
        Box::pin(async move { inventory })
    }
}

#[derive(Clone)]
struct FakeFileSystem {
    states: Arc<Mutex<HashMap<PathBuf, DirectoryProbeState>>>,
    probe_calls: Arc<Mutex<Vec<PathBuf>>>,
    shallow_calls: Arc<Mutex<Vec<ShallowCall>>>,
}

type ShallowCall = (PathBuf, Vec<PathBuf>);

impl FakeFileSystem {
    fn new(states: impl IntoIterator<Item = (&'static str, DirectoryProbeState)>) -> Self {
        Self {
            states: Arc::new(Mutex::new(
                states
                    .into_iter()
                    .map(|(path, state)| (PathBuf::from(path), state))
                    .collect(),
            )),
            probe_calls: Arc::new(Mutex::new(Vec::new())),
            shallow_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn from_owned(states: impl IntoIterator<Item = (String, DirectoryProbeState)>) -> Self {
        Self {
            states: Arc::new(Mutex::new(
                states
                    .into_iter()
                    .map(|(path, state)| (PathBuf::from(path), state))
                    .collect(),
            )),
            probe_calls: Arc::new(Mutex::new(Vec::new())),
            shallow_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn set(&self, path: impl AsRef<Path>, state: DirectoryProbeState) {
        self.states
            .lock()
            .expect("probe states")
            .insert(path.as_ref().to_path_buf(), state);
    }

    fn shallow_calls(&self) -> Vec<(PathBuf, Vec<PathBuf>)> {
        self.shallow_calls
            .lock()
            .expect("shallow signature calls")
            .clone()
    }

    fn probe_calls(&self) -> Vec<PathBuf> {
        self.probe_calls.lock().expect("probe calls").clone()
    }
}

struct BlockingInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

struct ShutdownAwareInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
}

impl ShutdownAwareInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            cancelled: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl InventorySource for ShutdownAwareInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = self.response.clone();
        let cancelled = Arc::clone(&self.cancelled);
        Box::pin(async move {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    cancelled.fetch_add(1, Ordering::SeqCst);
                    Err(ScanFailure {
                        reason: super::CatalogDegradedReason::GitFailed,
                        message: "shutdown cancelled inventory".to_owned(),
                    })
                }
                () = std::future::pending() => Ok(response),
            }
        })
    }
}

struct CancellationAwareInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    cancellations: Arc<AtomicUsize>,
}

impl CancellationAwareInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            cancellations: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl InventorySource for CancellationAwareInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        let cancellations = Arc::clone(&self.cancellations);
        Box::pin(async move {
            if call != 2 {
                return Ok(response);
            }
            cancellation.cancelled().await;
            cancellations.fetch_add(1, Ordering::SeqCst);
            Err(ScanFailure {
                reason: super::CatalogDegradedReason::GitFailed,
                message: "inventory cancelled".to_owned(),
            })
        })
    }
}

impl BlockingInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl InventorySource for BlockingInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let response = self.response.clone();
        let calls = Arc::clone(&self.calls);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            release.acquire().await.expect("inventory release").forget();
            Ok(response)
        })
    }
}

struct ConcurrentInventorySource {
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

impl Default for ConcurrentInventorySource {
    fn default() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl InventorySource for ConcurrentInventorySource {
    fn inventory(
        &self,
        anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let calls = Arc::clone(&self.calls);
        let active = Arc::clone(&self.active);
        let max_active = Arc::clone(&self.max_active);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now_active, Ordering::SeqCst);
            release.acquire().await.expect("inventory release").forget();
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(inventory(
                &format!("{}/.git", anchor.display()),
                [GitWorktreeRecord {
                    path: anchor,
                    head: Some("abc123".to_owned()),
                    branch: Some("main".to_owned()),
                    is_primary: true,
                    is_bare: false,
                    locked: false,
                    lock_reason: None,
                    is_prunable: false,
                    prunable_reason: None,
                }],
            ))
        })
    }
}

#[derive(Default)]
struct BlockingProbeFileSystem {
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

struct BlockingShallowFileSystem {
    states: HashMap<PathBuf, DirectoryProbeState>,
    shallow_calls: Arc<AtomicUsize>,
}

struct BlockingAfterFirstShallowFileSystem {
    states: HashMap<PathBuf, DirectoryProbeState>,
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    interrupted: Arc<AtomicUsize>,
}

struct BlockingFirstShallowFileSystem {
    states: HashMap<PathBuf, DirectoryProbeState>,
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

struct PausingNextShallowFileSystem {
    states: HashMap<PathBuf, DirectoryProbeState>,
    pause_next: AtomicBool,
    paused: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl PausingNextShallowFileSystem {
    fn new(states: impl IntoIterator<Item = (PathBuf, DirectoryProbeState)>) -> Self {
        Self {
            states: states.into_iter().collect(),
            pause_next: AtomicBool::new(false),
            paused: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }

    fn pause_next(&self) {
        assert!(
            !self.pause_next.swap(true, Ordering::SeqCst),
            "only one shallow operation may be paused at a time"
        );
    }

    async fn wait_until_paused(&self) {
        self.paused
            .acquire()
            .await
            .expect("pause notification")
            .forget();
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

impl BlockingFirstShallowFileSystem {
    fn new(states: impl IntoIterator<Item = (&'static str, DirectoryProbeState)>) -> Self {
        Self {
            states: states
                .into_iter()
                .map(|(path, state)| (PathBuf::from(path), state))
                .collect(),
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl BlockingAfterFirstShallowFileSystem {
    fn new(states: impl IntoIterator<Item = (&'static str, DirectoryProbeState)>) -> Self {
        Self {
            states: states
                .into_iter()
                .map(|(path, state)| (PathBuf::from(path), state))
                .collect(),
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            interrupted: Arc::new(AtomicUsize::new(0)),
        }
    }
}

struct PendingOperationGuard {
    active: Arc<AtomicUsize>,
    interrupted: Arc<AtomicUsize>,
}

impl PendingOperationGuard {
    fn new(active: Arc<AtomicUsize>, interrupted: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self {
            active,
            interrupted,
        }
    }
}

impl Drop for PendingOperationGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.interrupted.fetch_add(1, Ordering::SeqCst);
    }
}

impl BlockingShallowFileSystem {
    fn new(states: impl IntoIterator<Item = (&'static str, DirectoryProbeState)>) -> Self {
        Self {
            states: states
                .into_iter()
                .map(|(path, state)| (PathBuf::from(path), state))
                .collect(),
            shallow_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl CatalogFileSystem for BlockingShallowFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        let state = self
            .states
            .get(&path)
            .copied()
            .unwrap_or(DirectoryProbeState::Missing);
        Box::pin(async move { state })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        _common_dir: PathBuf,
        _known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        self.shallow_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

impl CatalogFileSystem for BlockingAfterFirstShallowFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        let state = self
            .states
            .get(&path)
            .copied()
            .unwrap_or(DirectoryProbeState::Missing);
        Box::pin(async move { state })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        _common_dir: PathBuf,
        _known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            return Box::pin(async { CatalogShallowSignature::default() });
        }
        let active = Arc::clone(&self.active);
        let interrupted = Arc::clone(&self.interrupted);
        Box::pin(async move {
            let _guard = PendingOperationGuard::new(active, interrupted);
            std::future::pending().await
        })
    }
}

impl CatalogFileSystem for BlockingFirstShallowFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        let state = self
            .states
            .get(&path)
            .copied()
            .unwrap_or(DirectoryProbeState::Missing);
        Box::pin(async move { state })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        _common_dir: PathBuf,
        _known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if call == 1 {
                release.acquire().await.expect("shallow release").forget();
            }
            CatalogShallowSignature {
                metadata: u64::try_from(call).expect("test call count fits u64"),
                availability: 0,
            }
        })
    }
}

impl CatalogFileSystem for PausingNextShallowFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        let state = self
            .states
            .get(&path)
            .copied()
            .unwrap_or(DirectoryProbeState::Missing);
        Box::pin(async move { state })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        _common_dir: PathBuf,
        _known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        let should_pause = self.pause_next.swap(false, Ordering::SeqCst);
        let paused = Arc::clone(&self.paused);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if should_pause {
                paused.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("shallow operation release")
                    .forget();
            }
            CatalogShallowSignature::default()
        })
    }
}

struct SwitchableBlockingProbeFileSystem {
    states: HashMap<PathBuf, DirectoryProbeState>,
    block: AtomicBool,
    active: Arc<AtomicUsize>,
    interrupted: Arc<AtomicUsize>,
}

impl SwitchableBlockingProbeFileSystem {
    fn new(states: impl IntoIterator<Item = (&'static str, DirectoryProbeState)>) -> Self {
        Self {
            states: states
                .into_iter()
                .map(|(path, state)| (PathBuf::from(path), state))
                .collect(),
            block: AtomicBool::new(false),
            active: Arc::new(AtomicUsize::new(0)),
            interrupted: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl CatalogFileSystem for SwitchableBlockingProbeFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        let state = self
            .states
            .get(&path)
            .copied()
            .unwrap_or(DirectoryProbeState::Missing);
        if !self.block.load(Ordering::SeqCst) || path == Path::new("/repo/main") {
            return Box::pin(async move { state });
        }
        let active = Arc::clone(&self.active);
        let interrupted = Arc::clone(&self.interrupted);
        Box::pin(async move {
            let _guard = PendingOperationGuard::new(active, interrupted);
            std::future::pending().await
        })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        _common_dir: PathBuf,
        _known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        Box::pin(async { CatalogShallowSignature::default() })
    }
}

impl CatalogFileSystem for BlockingProbeFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        let active = Arc::clone(&self.active);
        let max_active = Arc::clone(&self.max_active);
        Box::pin(async move {
            if path == Path::new("/repo/main") {
                return DirectoryProbeState::Present;
            }
            let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now_active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            DirectoryProbeState::Present
        })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        _common_dir: PathBuf,
        _known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        Box::pin(async { CatalogShallowSignature::default() })
    }
}

struct PausingSecondInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

struct PausingSecondOnlyInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

struct DelayedCancellationInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

impl DelayedCancellationInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            cancelled: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl InventorySource for DelayedCancellationInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        let cancelled = Arc::clone(&self.cancelled);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if call == 2 {
                cancellation.cancelled().await;
                cancelled.fetch_add(1, Ordering::SeqCst);
                release
                    .acquire()
                    .await
                    .expect("cancel unwind release")
                    .forget();
                return Err(ScanFailure {
                    reason: super::CatalogDegradedReason::GitFailed,
                    message: "cancelled prior lifecycle".to_owned(),
                });
            }
            Ok(response)
        })
    }
}

struct PausingAfterTwoInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

struct UncooperativeSecondInventorySource {
    response: GitWorktreeInventory,
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
}

struct PanickingSecondInventorySource {
    response: GitWorktreeInventory,
    calls: AtomicUsize,
}

struct ActiveInventoryGuard {
    active: Arc<AtomicUsize>,
}

impl Drop for ActiveInventoryGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

struct PausingThirdAnchorInventorySource {
    calls: Arc<AtomicUsize>,
    release: Arc<Semaphore>,
    replacement_enabled: AtomicBool,
}

impl PausingThirdAnchorInventorySource {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
            replacement_enabled: AtomicBool::new(false),
        }
    }
}

impl InventorySource for PausingThirdAnchorInventorySource {
    fn inventory(
        &self,
        anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let release = Arc::clone(&self.release);
        let replacement =
            self.replacement_enabled.load(Ordering::SeqCst) && anchor == Path::new("/repo/a-main");
        Box::pin(async move {
            if call == 3 {
                release
                    .acquire()
                    .await
                    .expect("valid anchor release")
                    .forget();
            }
            let common_dir = if replacement {
                "/replacement/common"
            } else {
                "/repo/common"
            };
            Ok(inventory(
                common_dir,
                [record(&anchor.to_string_lossy(), true)],
            ))
        })
    }
}

impl PausingAfterTwoInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl UncooperativeSecondInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl PanickingSecondInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: AtomicUsize::new(0),
        }
    }
}

impl InventorySource for PanickingSecondInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        Box::pin(async move {
            assert!(call == 1, "injected repository observation panic");
            Ok(response)
        })
    }
}

impl InventorySource for UncooperativeSecondInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        let active = Arc::clone(&self.active);
        let completed = Arc::clone(&self.completed);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if call > 1 {
                active.fetch_add(1, Ordering::SeqCst);
                let _active = ActiveInventoryGuard { active };
                release
                    .acquire()
                    .await
                    .expect("repository observation release")
                    .forget();
                completed.fetch_add(1, Ordering::SeqCst);
            }
            Ok(response)
        })
    }
}

impl InventorySource for PausingAfterTwoInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if call > 2 {
                release
                    .acquire()
                    .await
                    .expect("shared scan release")
                    .forget();
            }
            Ok(response)
        })
    }
}

impl PausingSecondInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl PausingSecondOnlyInventorySource {
    fn new(response: GitWorktreeInventory) -> Self {
        Self {
            response,
            calls: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}

impl InventorySource for PausingSecondOnlyInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if call == 2 {
                release
                    .acquire()
                    .await
                    .expect("second scan release")
                    .forget();
            }
            Ok(response)
        })
    }
}

impl InventorySource for PausingSecondInventorySource {
    fn inventory(
        &self,
        _anchor: PathBuf,
        _cancellation: CancellationToken,
    ) -> CatalogFuture<Result<GitWorktreeInventory, ScanFailure>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let response = self.response.clone();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            if call > 1 {
                release
                    .acquire()
                    .await
                    .expect("second scan release")
                    .forget();
            }
            Ok(response)
        })
    }
}

async fn wait_for_count(value: &AtomicUsize, expected: usize) {
    for _ in 0..10_000 {
        if value.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("counter did not reach {expected}");
}

async fn wait_for_mutation_refresh_worker_starts(
    service: &WorktreeCatalogService,
    expected: usize,
) {
    for _ in 0..10_000 {
        if service.mutation_refresh_worker_start_count_for_test() >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("mutation refresh worker start count did not reach {expected}");
}

async fn wait_for_active_mutation_refresh_workers(
    service: &WorktreeCatalogService,
    expected: usize,
) {
    for _ in 0..10_000 {
        if service.active_mutation_refresh_worker_count_for_test() == expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("active mutation refresh worker count did not reach {expected}");
}

async fn wait_for_repository_observation_requests(
    service: &WorktreeCatalogService,
    expected: usize,
) {
    for _ in 0..10_000 {
        if service.repository_observation_request_count_for_test() >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("repository observation request count did not reach {expected}");
}

impl CatalogFileSystem for FakeFileSystem {
    fn probe(&self, path: PathBuf) -> CatalogFuture<DirectoryProbeState> {
        self.probe_calls
            .lock()
            .expect("probe calls")
            .push(path.clone());
        let state = self
            .states
            .lock()
            .expect("probe states")
            .get(&path)
            .copied()
            .unwrap_or(DirectoryProbeState::Missing);
        Box::pin(async move { state })
    }

    fn canonicalize(&self, path: PathBuf) -> CatalogFuture<Result<PathBuf, std::io::Error>> {
        Box::pin(async move { Ok(path) })
    }

    fn shallow_signature(
        &self,
        common_dir: PathBuf,
        known_paths: Vec<PathBuf>,
    ) -> CatalogFuture<CatalogShallowSignature> {
        self.shallow_calls
            .lock()
            .expect("shallow signature calls")
            .push((common_dir, known_paths));
        Box::pin(async { CatalogShallowSignature::default() })
    }
}

fn project(
    project_id: &str,
    workspace_root: &str,
    threads: impl IntoIterator<Item = CatalogThread>,
) -> (String, CatalogProject) {
    (
        project_id.to_owned(),
        CatalogProject {
            workspace_root: PathBuf::from(workspace_root),
            baseline_paths: Vec::new(),
            threads: threads.into_iter().collect(),
            repository_key: None,
        },
    )
}

fn thread(thread_id: &str, worktree_path: &str) -> CatalogThread {
    CatalogThread {
        thread_id: thread_id.to_owned(),
        kind: "workspace".to_owned(),
        worktree_path: Some(PathBuf::from(worktree_path)),
        branch: Some("feature".to_owned()),
        archived: false,
        deleted: false,
    }
}

fn archived_thread(thread_id: &str, worktree_path: &str) -> CatalogThread {
    CatalogThread {
        archived: true,
        ..thread(thread_id, worktree_path)
    }
}

fn panel_thread(thread_id: &str, worktree_path: &str) -> CatalogThread {
    CatalogThread {
        kind: "panel".to_owned(),
        ..thread(thread_id, worktree_path)
    }
}

fn deleted_thread(thread_id: &str, worktree_path: &str) -> CatalogThread {
    CatalogThread {
        deleted: true,
        ..thread(thread_id, worktree_path)
    }
}

fn descriptor<'a>(
    snapshot: &'a super::WorktreeCatalogSnapshot,
    path: &str,
) -> &'a super::WorktreeDescriptor {
    snapshot
        .worktrees
        .iter()
        .find(|worktree| worktree.path == path)
        .expect("worktree descriptor")
}

fn adopted<'a>(
    snapshot: &'a super::WorktreeCatalogSnapshot,
    thread_id: &str,
) -> &'a super::AdoptedWorktreeStatus {
    snapshot
        .adopted_workspaces
        .iter()
        .find(|workspace| workspace.thread_id == thread_id)
        .expect("adopted workspace status")
}

fn inventory(
    common_dir: &str,
    records: impl IntoIterator<Item = GitWorktreeRecord>,
) -> GitWorktreeInventory {
    GitWorktreeInventory {
        common_dir: PathBuf::from(common_dir),
        records: records.into_iter().collect(),
        nul_delimited: true,
    }
}

fn record(path: &str, is_primary: bool) -> GitWorktreeRecord {
    GitWorktreeRecord {
        path: PathBuf::from(path),
        head: Some("abc123".to_owned()),
        branch: Some(if is_primary { "main" } else { "feature" }.to_owned()),
        is_primary,
        is_bare: false,
        locked: false,
        lock_reason: None,
        is_prunable: false,
        prunable_reason: None,
    }
}
