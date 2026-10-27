use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use coddy_studio_server::{
    git::{GitRepository, host_path_platform, normalize_worktree_path_key},
    persistence::{Database, ProjectionProject, ProjectionThread, Repositories, run_migrations},
    worktree_catalog::{
        AdoptedWorktreeAvailability, AdoptionValidationErrorReason, CatalogRefreshTrigger,
        CatalogScanStatus, WorkspaceAvailabilityRegistry, WorktreeCatalogService,
        WorktreeDirectoryState,
    },
};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;

const NOW: &str = "2026-08-09T00:00:00.000Z";

#[tokio::test]
async fn removal_mutation_lock_serializes_projects_in_the_same_physical_repository() {
    let fixture = RepositoryFixture::new().await;
    let mut alias = project(&fixture.main);
    alias.project_id = "project-2".to_owned();
    fixture
        .repositories
        .upsert_project(alias)
        .await
        .expect("alias project");
    let service = fixture.service();
    for project_id in ["project-1", "project-2"] {
        service
            .refresh(project_id, CatalogRefreshTrigger::Explicit)
            .await
            .expect("authoritative pin");
    }
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let first_service = service.clone();
    let first_entered = entered.clone();
    let first_release = release.clone();
    let first = tokio::spawn(async move {
        first_service
            .with_project_mutation_lock("project-1", || async move {
                first_entered.notify_one();
                first_release.notified().await;
            })
            .await;
    });
    entered.notified().await;
    let second_entered = Arc::new(AtomicBool::new(false));
    let second_service = service.clone();
    let second_flag = second_entered.clone();
    let second = tokio::spawn(async move {
        second_service
            .with_project_mutation_lock("project-2", || async move {
                second_flag.store(true, Ordering::SeqCst);
            })
            .await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!second_entered.load(Ordering::SeqCst));
    release.notify_one();
    first.await.expect("first mutation");
    second.await.expect("second mutation");
    assert!(second_entered.load(Ordering::SeqCst));
}

#[tokio::test]
async fn runtime_loss_guards_real_missing_worktree_and_exact_recovery_clears_it() {
    let fixture = RepositoryFixture::new().await;
    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/runtime-loss"],
        Some(&fixture.external),
    );
    fixture
        .repositories
        .upsert_thread(workspace_thread(&fixture.external))
        .await
        .expect("adopted workspace projection");
    let availability = WorkspaceAvailabilityRegistry::new();
    let service = fixture.service_with_availability(availability.clone());
    let _subscription = service.subscribe("project-1").await.expect("catalog");
    let external_path = canonical_string(&fixture.external);

    fs::remove_dir_all(&fixture.external).expect("remove external checkout");
    let missing = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("missing refresh");
    assert_eq!(
        adopted_status(&missing, "thread-external").availability,
        AdoptedWorktreeAvailability::MissingRegistered
    );
    let guarded = availability
        .guard_thread("thread-external")
        .await
        .expect_err("authoritative loss installs a guard");
    assert_same_worktree_path(&guarded.path, &external_path);

    let external_argument = fixture.external.to_string_lossy().into_owned();
    git(
        &fixture.main,
        &["worktree", "prune", "--expire", "now"],
        None,
    );
    git(
        &fixture.main,
        &[
            "worktree",
            "add",
            &external_argument,
            "feature/runtime-loss",
        ],
        None,
    );
    let recovered = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("recovery refresh");
    assert_eq!(
        adopted_status(&recovered, "thread-external").availability,
        AdoptedWorktreeAvailability::Present
    );
    assert_eq!(availability.guard_thread("thread-external").await, Ok(()));
}

#[tokio::test]
async fn legacy_unpinned_project_is_pinned_by_a_primary_authoritative_scan() {
    let fixture = RepositoryFixture::new().await;
    let service = fixture.service();

    let subscription = service
        .subscribe("project-1")
        .await
        .expect("trusted primary catalog");
    let repository_key = subscription.latest().repository_key.clone();
    let persisted = fixture
        .repositories
        .get_project("project-1".to_owned())
        .await
        .expect("read project")
        .expect("project exists");

    assert_eq!(
        persisted.worktree_repository_key.as_deref(),
        Some(repository_key.as_str())
    );
}

#[tokio::test]
async fn pinned_project_restarts_from_a_same_repository_adopted_anchor() {
    let fixture = RepositoryFixture::new().await;
    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/restart"],
        Some(&fixture.external),
    );
    fixture
        .repositories
        .upsert_thread(workspace_thread(&fixture.external))
        .await
        .expect("adopted workspace");
    let service = fixture.service();
    let subscription = service.subscribe("project-1").await.expect("initial pin");
    let repository_key = subscription.latest().repository_key.clone();
    drop(subscription);
    drop(service);

    let mut unavailable_primary = project(&fixture.main);
    unavailable_primary.workspace_root = fixture
        .root
        .path()
        .join("missing-primary")
        .to_string_lossy()
        .into_owned();
    fixture
        .repositories
        .upsert_project(unavailable_primary)
        .await
        .expect("primary becomes unavailable without clearing the pin");

    let restarted = fixture.service();
    let restarted_subscription = restarted
        .subscribe("project-1")
        .await
        .expect("pinned adopted cold-start anchor");
    assert_eq!(
        restarted_subscription.latest().repository_key,
        repository_key
    );
    assert_eq!(
        adopted_status(&restarted_subscription.latest(), "thread-external").availability,
        AdoptedWorktreeAvailability::Present
    );
}

#[tokio::test]
async fn pinned_project_rejects_a_replacement_repository_at_an_adopted_path() {
    let fixture = RepositoryFixture::new().await;
    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/replacement"],
        Some(&fixture.external),
    );
    fixture
        .repositories
        .upsert_thread(workspace_thread(&fixture.external))
        .await
        .expect("adopted workspace");
    let service = fixture.service();
    let subscription = service.subscribe("project-1").await.expect("initial pin");
    drop(subscription);
    drop(service);

    let external_argument = fixture.external.to_string_lossy().into_owned();
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &external_argument],
        None,
    );
    fs::create_dir(&fixture.external).expect("replacement directory");
    git(
        &fixture.external,
        &["init", "--initial-branch", "replacement"],
        None,
    );
    let mut unavailable_primary = project(&fixture.main);
    unavailable_primary.workspace_root = fixture
        .root
        .path()
        .join("missing-primary")
        .to_string_lossy()
        .into_owned();
    fixture
        .repositories
        .upsert_project(unavailable_primary)
        .await
        .expect("primary unavailable");

    let restarted = fixture.service();
    let error = match restarted.subscribe("project-1").await {
        Err(error) => error,
        Ok(_) => panic!("replacement repository must fail the durable identity pin"),
    };
    assert_eq!(
        error.reason,
        coddy_studio_server::worktree_catalog::CatalogErrorReason::RepositoryUnavailable
    );
    assert!(restarted.latest("project-1").await.is_none());
}

#[tokio::test]
async fn real_repository_tracks_external_create_delete_prune_and_exact_path_recovery() {
    let fixture = RepositoryFixture::new().await;
    let service = fixture.service();
    let mut subscription = service
        .subscribe("project-1")
        .await
        .expect("initial catalog");
    assert_eq!(subscription.latest().worktrees.len(), 1);

    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/external"],
        Some(&fixture.external),
    );
    let discovered = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = subscription.changed().await.expect("catalog poll update");
            if snapshot.authoritative && snapshot.worktrees.len() == 2 {
                break snapshot;
            }
        }
    })
    .await
    .expect("external worktree appears within the discovery target");
    let external_path = canonical_string(&fixture.external);
    let discovered_external = discovered
        .worktrees
        .iter()
        .find(|worktree| same_worktree_path(&worktree.path, &external_path))
        .expect("external descriptor");
    assert!(discovered_external.eligible_for_adoption);

    fixture
        .repositories
        .upsert_thread(workspace_thread(&fixture.external))
        .await
        .expect("adopted workspace projection");
    let adopted = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("adopted join");
    assert_eq!(
        adopted_status(&adopted, "thread-external").availability,
        AdoptedWorktreeAvailability::Present
    );

    fs::remove_dir_all(&fixture.external).expect("delete external checkout directory");
    let missing_registered = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("missing registered refresh");
    assert_eq!(
        missing_registered
            .worktrees
            .iter()
            .find(|worktree| same_worktree_path(&worktree.path, &external_path))
            .expect("stale registered descriptor")
            .directory_state,
        WorktreeDirectoryState::Missing
    );
    assert_eq!(
        adopted_status(&missing_registered, "thread-external").availability,
        AdoptedWorktreeAvailability::MissingRegistered
    );

    git(
        &fixture.main,
        &["worktree", "prune", "--expire", "now"],
        None,
    );
    let missing_unregistered = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("pruned refresh");
    assert!(
        missing_unregistered
            .worktrees
            .iter()
            .all(|worktree| !same_worktree_path(&worktree.path, &external_path))
    );
    assert_eq!(
        adopted_status(&missing_unregistered, "thread-external").availability,
        AdoptedWorktreeAvailability::MissingUnregistered
    );

    let external_argument = fixture.external.to_string_lossy().into_owned();
    git(
        &fixture.main,
        &["worktree", "add", &external_argument, "feature/external"],
        None,
    );
    let recovered = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("exact path recovery");
    assert_eq!(
        adopted_status(&recovered, "thread-external").availability,
        AdoptedWorktreeAvailability::Present
    );
}

#[tokio::test]
async fn primary_checkout_loss_never_publishes_an_authoritative_empty_catalog() {
    let fixture = RepositoryFixture::new().await;
    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/fallback"],
        Some(&fixture.external),
    );
    fixture
        .repositories
        .upsert_thread(workspace_thread(&fixture.external))
        .await
        .expect("adopted fallback workspace");
    let service = fixture.service();
    let subscription = service.subscribe("project-1").await.expect("catalog");
    let authoritative = subscription.latest();
    assert_eq!(authoritative.worktrees.len(), 2);

    let displaced = fixture.root.path().join("primary-displaced");
    fs::rename(&fixture.main, &displaced).expect("displace primary checkout and common Git dir");
    let degraded = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("primary loss degrades catalog");

    assert!(!degraded.authoritative);
    assert!(matches!(
        degraded.scan_status,
        CatalogScanStatus::Degraded { .. }
    ));
    assert_eq!(degraded.generation, authoritative.generation);
    assert_eq!(degraded.worktrees, authoritative.worktrees);
    assert_eq!(
        degraded.adopted_workspaces,
        authoritative.adopted_workspaces
    );
}

#[tokio::test]
async fn git_failure_retains_the_preceding_snapshot_until_recovery() {
    let fixture = RepositoryFixture::new().await;
    let service = fixture.service();
    let subscription = service.subscribe("project-1").await.expect("catalog");
    let authoritative = subscription.latest();
    let common_dir = fixture.main.join(".git");
    let unavailable = fixture.main.join(".git-unavailable");
    fs::rename(&common_dir, &unavailable).expect("make Git metadata unavailable");

    let degraded = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("Git failure publishes degraded retention");
    assert!(!degraded.authoritative);
    assert_eq!(degraded.worktrees, authoritative.worktrees);
    fs::rename(&unavailable, &common_dir).expect("restore Git metadata");

    let recovered = service
        .refresh("project-1", CatalogRefreshTrigger::Explicit)
        .await
        .expect("Git recovery");
    assert!(recovered.authoritative);
    assert!(matches!(recovered.scan_status, CatalogScanStatus::Ready));
    assert_eq!(recovered.generation, authoritative.generation + 1);
}

#[tokio::test]
async fn adoption_revalidation_proves_presence_and_repository_membership_with_real_git() {
    let fixture = RepositoryFixture::new().await;
    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/adoption"],
        Some(&fixture.external),
    );
    let service = fixture.service();
    let subscription = service.subscribe("project-1").await.expect("catalog");
    let snapshot = subscription.latest();
    let candidate = snapshot
        .worktrees
        .iter()
        .find(|worktree| worktree.eligible_for_adoption)
        .expect("candidate");

    let resolved = service
        .resolve_adoption_candidate("project-1", &candidate.worktree_key)
        .await
        .expect("fresh candidate");
    assert_same_worktree_path(&resolved.path, canonical_string(&fixture.external));
    assert_eq!(resolved.branch.as_deref(), Some("feature/adoption"));

    fs::remove_dir_all(&fixture.external).expect("external checkout disappears");
    let missing = service
        .resolve_adoption_candidate("project-1", &candidate.worktree_key)
        .await
        .expect_err("missing directory fails closed");
    assert_eq!(
        missing.reason,
        AdoptionValidationErrorReason::WorkspaceMissing
    );
}

#[tokio::test]
async fn adoption_revalidation_rejects_a_replacement_repository_at_the_registered_path() {
    let fixture = RepositoryFixture::new().await;
    git(
        &fixture.main,
        &["worktree", "add", "-b", "feature/adoption-replacement"],
        Some(&fixture.external),
    );
    let service = fixture.service();
    let subscription = service.subscribe("project-1").await.expect("catalog");
    let snapshot = subscription.latest();
    let candidate = snapshot
        .worktrees
        .iter()
        .find(|worktree| worktree.eligible_for_adoption)
        .expect("candidate");
    let candidate_key = candidate.worktree_key.clone();
    let external_argument = fixture.external.to_string_lossy().into_owned();
    git(
        &fixture.main,
        &["worktree", "remove", "--force", &external_argument],
        None,
    );
    fs::create_dir(&fixture.external).expect("replacement directory");
    git(
        &fixture.external,
        &["init", "--initial-branch", "replacement"],
        None,
    );

    let replacement = service
        .resolve_adoption_candidate("project-1", &candidate_key)
        .await
        .expect_err("replacement repository fails membership fence");
    assert_eq!(
        replacement.reason,
        AdoptionValidationErrorReason::RepositoryMismatch
    );
}

struct RepositoryFixture {
    root: TempDir,
    main: PathBuf,
    external: PathBuf,
    repositories: Arc<Repositories>,
}

impl RepositoryFixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().expect("repository fixture root");
        let main = root.path().join("main");
        let external = root.path().join("external");
        fs::create_dir(&main).expect("primary checkout directory");
        git(&main, &["init", "--initial-branch", "main"], None);
        git(
            &main,
            &["config", "user.email", "catalog@example.invalid"],
            None,
        );
        git(&main, &["config", "user.name", "Catalog Test"], None);
        fs::write(main.join("README.md"), "catalog fixture\n").expect("fixture file");
        git(&main, &["add", "README.md"], None);
        git(&main, &["commit", "-m", "initial"], None);

        let database = Database::open_in_memory().await.expect("catalog database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("catalog migrations");
        let repositories = Arc::new(Repositories::new(database));
        repositories
            .upsert_project(project(&main))
            .await
            .expect("catalog project projection");
        Self {
            root,
            main,
            external,
            repositories,
        }
    }

    fn service(&self) -> WorktreeCatalogService {
        WorktreeCatalogService::new(
            Arc::clone(&self.repositories),
            Arc::new(GitRepository::default()),
        )
    }

    fn service_with_availability(
        &self,
        availability: WorkspaceAvailabilityRegistry,
    ) -> WorktreeCatalogService {
        WorktreeCatalogService::new_with_availability_registry(
            Arc::clone(&self.repositories),
            Arc::new(GitRepository::default()),
            availability,
        )
    }
}

fn project(main: &Path) -> ProjectionProject {
    ProjectionProject {
        project_id: "project-1".to_owned(),
        title: "Catalog fixture".to_owned(),
        workspace_root: main.to_string_lossy().into_owned(),
        default_model_selection: None,
        scripts: json!([]),
        worktree_discovery: json!({
            "visibility": "hidden",
            "initialPromptDismissedAt": null,
            "baselinePaths": []
        }),
        worktree_repository_key: None,
        created_at: NOW.to_owned(),
        updated_at: NOW.to_owned(),
        deleted_at: None,
        repository_identity: None,
    }
}

fn workspace_thread(path: &Path) -> ProjectionThread {
    ProjectionThread {
        thread_id: "thread-external".to_owned(),
        project_id: "project-1".to_owned(),
        title: "External".to_owned(),
        kind: "workspace".to_owned(),
        model_selection: json!({}),
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        branch: Some("feature/external".to_owned()),
        worktree_path: Some(canonical_string(path)),
        latest_turn_id: None,
        created_at: NOW.to_owned(),
        updated_at: NOW.to_owned(),
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

fn adopted_status<'a>(
    snapshot: &'a coddy_studio_server::worktree_catalog::WorktreeCatalogSnapshot,
    thread_id: &str,
) -> &'a coddy_studio_server::worktree_catalog::AdoptedWorktreeStatus {
    snapshot
        .adopted_workspaces
        .iter()
        .find(|workspace| workspace.thread_id == thread_id)
        .expect("adopted workspace status")
}

fn canonical_string(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonical fixture path")
        .to_string_lossy()
        .into_owned()
}

fn assert_same_worktree_path(left: impl AsRef<Path>, right: impl AsRef<Path>) {
    assert!(
        same_worktree_path(left.as_ref(), right.as_ref()),
        "worktree paths do not identify the same location: left={:?}, right={:?}",
        left.as_ref(),
        right.as_ref()
    );
}

fn same_worktree_path(left: impl AsRef<Path>, right: impl AsRef<Path>) -> bool {
    let platform = host_path_platform();
    normalize_worktree_path_key(left.as_ref(), platform)
        == normalize_worktree_path_key(right.as_ref(), platform)
}

fn git(cwd: &Path, args: &[&str], final_path: Option<&Path>) {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    if let Some(final_path) = final_path {
        command.arg(final_path);
    }
    let output = command.output().expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        command,
        String::from_utf8_lossy(&output.stderr)
    );
}
