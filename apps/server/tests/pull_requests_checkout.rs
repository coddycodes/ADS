#![cfg(unix)]
//! Checkout tests use real local Git repositories and recording provider stubs.
#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use coddy_studio_server::{
    RequestId, RpcRequest,
    git::{GitRepository, StatusBroadcaster},
    orchestration::{EngineOptions, OrchestrationEngine},
    persistence::{Database, Repositories, run_migrations},
    production::{
        pull_requests_rpc::{ConfiguredPullRequestsRpcServices, PullRequestsRpcServices},
        worktree_catalog_rpc::WorktreeCatalogRpcServices,
    },
    pull_requests::{PullRequestsService, host::HostCommandRunner},
    worktree_catalog::{
        CatalogRefreshTrigger, WorkspaceAvailabilityRegistry, WorktreeCatalogService,
    },
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn script(path: &Path, source: &str) {
    executable_fixture::write_executable(path, source);
}
struct Fixture {
    root: TempDir,
    main: PathBuf,
    services: ConfiguredPullRequestsRpcServices,
    catalog: WorktreeCatalogService,
    availability: WorkspaceAvailabilityRegistry,
    managed: WorktreeCatalogRpcServices,
    repositories: Repositories,
    engine: OrchestrationEngine,
}
impl Fixture {
    fn configure_gitlab(&self) {
        script(
            &self.root.path().join("bin/git"),
            "#!/bin/sh\nif [ \"$1 $2 $3\" = 'remote get-url origin' ]; then echo https://gitlab.com/team/repo.git; else exec git \"$@\"; fi\n",
        );
        script(
            &self.root.path().join("bin/gh"),
            &format!(
                r#"#!/bin/sh
case "$1 $2" in
'--version ') echo 'glab version 1.80.0' ;;
'auth status') printf 'gitlab.com\n  ✓ Logged in to gitlab.com as reader\n' ;;
'api projects/team%2Frepo/merge_requests/7') echo '{{"source_branch":"feature"}}' ;;
'mr checkout')
  printf '%s\n' "$PWD" "$@" >> '{log}'
  git fetch origin refs/merge-requests/7/head:feature && git checkout feature ;;
*) echo "Unexpected glab: $*" >&2; exit 9 ;;
esac
"#,
                log = self.root.path().join("checkout.log").display()
            ),
        );
    }

    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin");
        let main = root.path().join("main");
        fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "--initial-branch", "main"]);
        git(
            &origin,
            &["config", "user.email", "checkout@example.invalid"],
        );
        git(&origin, &["config", "user.name", "Checkout Test"]);
        fs::write(origin.join("README"), "main\n").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "main"]);
        git(&origin, &["switch", "-c", "feature"]);
        fs::write(origin.join("feature"), "feature\n").unwrap();
        git(&origin, &["add", "."]);
        git(&origin, &["commit", "-m", "feature"]);
        git(&origin, &["fetch", ".", "feature:refs/pull/7/head"]);
        git(
            &origin,
            &["fetch", ".", "feature:refs/merge-requests/7/head"],
        );
        git(&origin, &["switch", "main"]);
        git(
            root.path(),
            &["clone", origin.to_str().unwrap(), main.to_str().unwrap()],
        );
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(
            &bin.join("git"),
            "#!/bin/sh\nif [ \"$1 $2 $3\" = 'remote get-url origin' ]; then echo https://github.com/owner/repo.git; else exec git \"$@\"; fi\n",
        );
        script(
            &bin.join("gh"),
            &format!(
                r#"#!/bin/sh
case "$1 $2" in
'--version ') echo 'gh version 2.80.0' ;;
'auth status') echo '{{"hosts":{{"github.com":[{{"state":"success","active":true,"host":"github.com","login":"reader"}}]}}}}' ;;
'pr view') echo '{{"headRefName":"feature"}}' ;;
'pr checkout')
  printf '%s\n' "$PWD" "$@" >> '{log}'
  git fetch origin refs/pull/7/head:feature && git checkout feature ;;
*) echo "Unexpected gh: $*" >&2; exit 9 ;;
esac
"#,
                log = root.path().join("checkout.log").display()
            ),
        );
        let database = Database::open_in_memory().await.unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap();
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .unwrap();
        engine.dispatch(serde_json::from_value(json!({"type":"project.create", "commandId":"create-project", "projectId":"project", "title":"Checkout", "workspaceRoot":main, "defaultModelSelection":null, "createdAt":"2026-09-21T00:00:00Z"})).unwrap()).await.unwrap();
        let repositories = engine.repositories();
        let repository = Arc::new(GitRepository::default());
        let broadcaster =
            StatusBroadcaster::new(repository.clone(), std::time::Duration::from_secs(3600), 8);
        let availability = WorkspaceAvailabilityRegistry::new();
        let catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            repository,
            availability.clone(),
        );
        let managed = WorktreeCatalogRpcServices::new(catalog.clone(), engine.clone())
            .with_status_broadcaster(broadcaster);
        let mut services = PullRequestsRpcServices::with_dependencies(
            root.path().join("state"),
            repositories.clone(),
            Arc::default(),
        )
        .with_worktrees(managed.clone());
        services.service = PullRequestsService::with_runner(
            HostCommandRunner::new(root.path().join("state")).with_commands(
                bin.join("gh"),
                bin.join("gh"),
                bin.join("git"),
            ),
        );
        Self {
            root,
            main,
            services,
            catalog,
            availability,
            managed,
            repositories,
            engine,
        }
    }
    async fn checkout(&self, target: Value) -> Result<Value, Value> {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.services.mutation_unary(
                RpcRequest {
                    id: RequestId::try_from("1").unwrap(),
                    tag: "pullRequests.checkout".into(),
                    payload: json!({"cwd":self.main, "number":7, "target":target}),
                    headers: vec![],
                    trace_id: None,
                    span_id: None,
                    sampled: None,
                },
                CancellationToken::new(),
            ),
        )
        .await
        .expect("checkout settles within 30 seconds")
    }
    fn called(&self) -> bool {
        self.root.path().join("checkout.log").exists()
    }
    async fn finish(self) {
        self.managed.operation_runtime().shutdown().await;
        self.engine.shutdown().await;
    }
}

#[tokio::test]
async fn pull_requests_checkout_clean_tree() {
    let f = Fixture::new().await;
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap();
    assert_eq!(
        result,
        json!({"kind":"checked_out","cwd":f.main,"branch":"feature"})
    );
    assert_eq!(git(&f.main, &["branch", "--show-current"]), "feature");
    let log = fs::read_to_string(f.root.path().join("checkout.log")).unwrap();
    assert!(log.contains("--repo\ngithub.com/owner/repo\n"), "{log}");
    f.finish().await;
}
#[tokio::test]
async fn pull_requests_checkout_dirty_tree_blocks_before_cli() {
    let f = Fixture::new().await;
    fs::write(f.main.join("README"), "uncommitted\n").unwrap();
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "blocked");
    assert_eq!(result["reason"]["code"], "dirty-working-tree");
    assert!(!f.called());
    assert_eq!(git(&f.main, &["branch", "--show-current"]), "main");
    f.finish().await;
}
#[tokio::test]
async fn pull_requests_checkout_occupied_branch_names_worktree() {
    let f = Fixture::new().await;
    let occupied = f.root.path().join("occupied worktree");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            occupied.to_str().unwrap(),
            "origin/feature",
        ],
    );
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap();
    assert_eq!(result["reason"]["code"], "worktree-checked-out");
    assert!(
        result["reason"]["message"]
            .as_str()
            .unwrap()
            .contains(occupied.to_str().unwrap())
    );
    assert!(!f.called());
    f.finish().await;
}
#[tokio::test]
async fn pull_requests_checkout_new_worktree_has_catalog_owner() {
    let f = Fixture::new().await;
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "worktree_created");
    assert_eq!(result["branch"], "feature");
    let cwd = Path::new(result["cwd"].as_str().unwrap());
    assert_eq!(git(cwd, &["branch", "--show-current"]), "feature");
    assert_eq!(
        fs::read_to_string(cwd.join("feature")).unwrap(),
        "feature\n"
    );
    let thread = f
        .repositories
        .get_thread(result["worktreeId"].as_str().unwrap().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thread.worktree_path.as_deref(), cwd.to_str());
    assert_eq!(thread.kind, "workspace");
    let snapshot = f
        .catalog
        .refresh("project", CatalogRefreshTrigger::Explicit)
        .await
        .unwrap();
    assert!(
        snapshot
            .worktrees
            .iter()
            .any(|w| w.path == cwd.to_str().unwrap())
    );
    assert!(!f.called());
    assert_eq!(git(&f.main, &["branch", "--show-current"]), "main");
    f.finish().await;
}
#[tokio::test]
async fn pull_requests_checkout_collision_preserves_branch_and_suffixes() {
    let f = Fixture::new().await;
    git(&f.main, &["branch", "feature", "main"]);
    let original = git(&f.main, &["rev-parse", "feature"]);
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "worktree_created");
    assert_eq!(result["branch"], "feature-pr-7");
    assert_eq!(git(&f.main, &["rev-parse", "feature"]), original);
    assert_eq!(
        fs::read_to_string(Path::new(result["cwd"].as_str().unwrap()).join("feature")).unwrap(),
        "feature\n"
    );
    assert!(!f.called());
    f.finish().await;
}
#[tokio::test]
async fn pull_requests_checkout_rejects_unowned_target() {
    let f = Fixture::new().await;
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.root.path().join("origin")}))
        .await
        .unwrap();
    assert_eq!(result["reason"]["code"], "project-mismatch");
    assert!(!f.called());
    f.finish().await;
}
#[tokio::test]
async fn pull_requests_checkout_in_progress_blocks_before_cli() {
    let f = Fixture::new().await;
    fs::write(
        f.main.join(".git/MERGE_HEAD"),
        format!("{}\n", git(&f.main, &["rev-parse", "origin/feature"])),
    )
    .unwrap();
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap();
    assert_eq!(result["reason"]["code"], "merge-in-progress");
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_target_guards_use_the_target_directory() {
    let f = Fixture::new().await;
    let other = f.root.path().join("other checkout");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "other",
            other.to_str().unwrap(),
            "main",
        ],
    );
    fs::write(f.main.join("README"), "source can stay dirty\n").unwrap();
    let result = f
        .checkout(json!({"kind":"checkout","cwd":other}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "checked_out");
    assert_eq!(result["cwd"], json!(other));
    assert_eq!(git(&f.main, &["branch", "--show-current"]), "main");
    assert_eq!(git(&other, &["branch", "--show-current"]), "feature");
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_rejects_another_project_even_in_same_repository() {
    let f = Fixture::new().await;
    let other = f.root.path().join("other project");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "other",
            other.to_str().unwrap(),
            "main",
        ],
    );
    f.engine.dispatch(serde_json::from_value(json!({"type":"project.create","commandId":"create-other","projectId":"other-project","title":"Other","workspaceRoot":other,"defaultModelSelection":null,"createdAt":"2026-09-21T00:00:00Z"})).unwrap()).await.unwrap();
    let result = f
        .checkout(json!({"kind":"checkout","cwd":other}))
        .await
        .unwrap();
    assert_eq!(result["reason"]["code"], "project-mismatch");
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_uses_catalog_lock_without_waiting() {
    let f = Fixture::new().await;
    f.catalog
        .refresh("project", CatalogRefreshTrigger::Explicit)
        .await
        .unwrap();
    f.catalog
        .with_project_mutation_lock("project", || async {
            let result = f
                .checkout(json!({"kind":"checkout","cwd":f.main}))
                .await
                .unwrap();
            assert_eq!(result["reason"]["code"], "operation-in-flight");
            assert!(!f.called());
        })
        .await;
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_new_worktree_keeps_dirty_source_and_occupied_branch() {
    let f = Fixture::new().await;
    let occupied = f.root.path().join("occupied");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            occupied.to_str().unwrap(),
            "origin/feature",
        ],
    );
    fs::write(f.main.join("README"), "keep my work\n").unwrap();
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "worktree_created");
    assert_eq!(result["branch"], "feature-pr-7");
    assert_eq!(git(&occupied, &["branch", "--show-current"]), "feature");
    assert_eq!(
        fs::read_to_string(f.main.join("README")).unwrap(),
        "keep my work\n"
    );
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_custom_name_and_repeated_collision_preserve_all_tips() {
    let f = Fixture::new().await;
    git(&f.main, &["branch", "verify", "main"]);
    git(&f.main, &["branch", "verify-pr-7", "main"]);
    let result = f
        .checkout(json!({"kind":"worktree","branchName":"verify"}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "verify-pr-7-2");
    assert_eq!(
        git(&f.main, &["rev-parse", "verify"]),
        git(&f.main, &["rev-parse", "main"])
    );
    assert_eq!(
        git(&f.main, &["rev-parse", "verify-pr-7"]),
        git(&f.main, &["rev-parse", "main"])
    );
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_rejects_branch_revision_expressions() {
    let f = Fixture::new().await;
    for name in ["@{-1}", "--force", " ", "feature:main"] {
        let result = f
            .checkout(json!({"kind":"worktree","branchName":name}))
            .await;
        assert!(result.is_err(), "{name}: {result:?}");
    }
    assert_eq!(
        git(&f.main, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_current_branch_refreshes_the_pr_tip() {
    let f = Fixture::new().await;
    git(&f.main, &["switch", "-c", "feature", "main"]);
    let gh = f.root.path().join("bin/gh");
    let source = fs::read_to_string(&gh).unwrap().replace("git fetch origin refs/pull/7/head:feature && git checkout feature", "git fetch origin refs/pull/7/head && git checkout feature && git merge --ff-only FETCH_HEAD");
    script(&gh, &source);
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "checked_out");
    assert_eq!(
        git(&f.main, &["rev-parse", "HEAD"]),
        git(&f.main, &["rev-parse", "origin/feature"])
    );
    assert!(f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_divergent_current_branch_is_not_reported_as_verified() {
    let f = Fixture::new().await;
    git(&f.main, &["switch", "-c", "feature", "main"]);
    git(
        &f.main,
        &["config", "user.email", "checkout@example.invalid"],
    );
    git(&f.main, &["config", "user.name", "Checkout Test"]);
    fs::write(f.main.join("local"), "local commit\n").unwrap();
    git(&f.main, &["add", "local"]);
    git(&f.main, &["commit", "-m", "keep local commit"]);
    let original = git(&f.main, &["rev-parse", "HEAD"]);
    let gh = f.root.path().join("bin/gh");
    let source = fs::read_to_string(&gh).unwrap().replace("git fetch origin refs/pull/7/head:feature && git checkout feature", "git fetch origin refs/pull/7/head && git checkout feature && git merge --ff-only FETCH_HEAD");
    script(&gh, &source);
    assert!(
        f.checkout(json!({"kind":"checkout","cwd":f.main}))
            .await
            .is_err()
    );
    assert_eq!(git(&f.main, &["rev-parse", "HEAD"]), original);
    assert!(f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_shutdown_uses_declared_error_contract() {
    let f = Fixture::new().await;
    f.managed.operation_runtime().shutdown().await;
    let error = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap_err();
    assert_eq!(error["_tag"], "PullRequestsOperationError");
    assert_eq!(error["operation"], "pullRequests.checkout");
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_rejects_registered_path_retargeted_to_foreign_repository() {
    let f = Fixture::new().await;
    let other = f.root.path().join("retargeted");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "other",
            other.to_str().unwrap(),
            "main",
        ],
    );
    f.catalog
        .refresh("project", CatalogRefreshTrigger::Explicit)
        .await
        .unwrap();
    fs::write(
        other.join(".git"),
        format!("gitdir: {}\n", f.root.path().join("origin/.git").display()),
    )
    .unwrap();
    let result = f
        .checkout(json!({"kind":"checkout","cwd":other}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "blocked");
    assert_eq!(result["reason"]["code"], "project-mismatch");
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_gitlab_pins_repository_and_head_ref() {
    for new_worktree in [false, true] {
        let f = Fixture::new().await;
        f.configure_gitlab();
        let result = f
            .checkout(if new_worktree {
                json!({"kind":"worktree","branchName":null})
            } else {
                json!({"kind":"checkout","cwd":f.main})
            })
            .await
            .unwrap();
        assert_eq!(result["branch"], "feature");
        if new_worktree {
            assert_eq!(result["kind"], "worktree_created");
            assert!(!f.called());
        } else {
            let log = fs::read_to_string(f.root.path().join("checkout.log")).unwrap();
            assert!(
                log.contains("mr\ncheckout\n7\n-b\nfeature\n--repo\ngitlab.com/team/repo\n"),
                "{log}"
            );
        }
        f.finish().await;
    }
}

#[tokio::test]
async fn pull_requests_checkout_prewrite_cancellation_reaps_read_and_releases_catalog_lock() {
    let f = Fixture::new().await;
    let gh = f.root.path().join("bin/gh");
    let original = fs::read_to_string(&gh).unwrap();
    let started = f.root.path().join("started");
    let slow = original.replace(
        "'pr view')",
        &format!("'pr view')\n  touch '{}'\n  sleep 60\n", started.display()),
    );
    script(&gh, &slow);
    let cancellation = CancellationToken::new();
    let request = RpcRequest {
        id: RequestId::try_from("2").unwrap(),
        tag: "pullRequests.checkout".into(),
        payload: json!({"cwd":f.main,"number":7,"target":{"kind":"checkout","cwd":f.main}}),
        headers: vec![],
        trace_id: None,
        span_id: None,
        sampled: None,
    };
    let services = f.services.clone();
    let operation = services.mutation_unary(request, cancellation.clone());
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => panic!("checkout ended before cancellation: {result:?}"),
        () = async { tokio::time::timeout(std::time::Duration::from_secs(10), async { while !started.exists() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; } }).await.expect("CLI started"); } => {}
    }
    cancellation.cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(10), operation)
        .await
        .expect("cancelled checkout settles")
        .unwrap_err();
    assert_eq!(error["code"], "timeout");
    assert_eq!(
        error["message"],
        "Checkout stopped before any Git changes; refresh to try again"
    );
    assert!(!f.called());
    script(&gh, &original);
    let result = f
        .checkout(json!({"kind":"checkout","cwd":f.main}))
        .await
        .unwrap();
    assert_eq!(result["kind"], "checked_out");
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_worktree_does_not_trust_shared_fetch_head() {
    let f = Fixture::new().await;
    script(
        &f.root.path().join("bin/git"),
        r#"#!/bin/sh
if [ "$1 $2 $3" = 'remote get-url origin' ]; then
  echo https://github.com/owner/repo.git
elif [ "$1" = fetch ]; then
  git "$@" || exit $?
  # Another fetch can replace FETCH_HEAD even while the app's catalog lock is held.
  git fetch origin main
else
  exec git "$@"
fi
"#,
    );
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    let cwd = Path::new(result["cwd"].as_str().unwrap());
    assert_eq!(
        git(cwd, &["rev-parse", "HEAD"]),
        git(&f.main, &["rev-parse", "origin/feature"])
    );
    f.finish().await;
}

struct CheckoutWriteGate {
    started: PathBuf,
    release: PathBuf,
    completed: PathBuf,
}
impl Drop for CheckoutWriteGate {
    fn drop(&mut self) {
        let _ = fs::write(&self.release, "release");
    }
}
impl Fixture {
    fn request(&self, target: Value) -> RpcRequest {
        RpcRequest {
            id: RequestId::try_from("9").unwrap(),
            tag: "pullRequests.checkout".into(),
            payload: json!({"cwd":self.main,"number":7,"target":target}),
            headers: vec![],
            trace_id: None,
            span_id: None,
            sampled: None,
        }
    }
    fn pause_checkout_write(&self, fetch: bool) -> CheckoutWriteGate {
        let gate = CheckoutWriteGate {
            started: self.root.path().join("write-started"),
            release: self.root.path().join("release-write"),
            completed: self.root.path().join("write-completed"),
        };
        let pause = format!(
            "touch .git/index.lock\ntouch '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\nsleep 0.02\nrm .git/index.lock\nprintf 'write completed\\n' > '{}'",
            gate.started.display(),
            gate.release.display(),
            gate.completed.display()
        );
        if fetch {
            script(
                &self.root.path().join("bin/git"),
                &format!(
                    r#"#!/bin/sh
if [ "$1 $2 $3" = 'remote get-url origin' ]; then echo https://github.com/owner/repo.git
elif [ "$1" = fetch ]; then
{pause}
exec git "$@"
else exec git "$@"; fi
"#
                ),
            );
        } else {
            let path = self.root.path().join("bin/gh");
            let original = fs::read_to_string(&path).unwrap();
            script(
                &path,
                &original
                    .replace("'pr checkout')", &format!("'pr checkout')\n{pause}"))
                    .replace("'mr checkout')", &format!("'mr checkout')\n{pause}")),
            );
        }
        gate
    }
}
async fn await_file(path: &Path) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture process reached the gate");
}

#[tokio::test]
async fn pull_requests_checkout_cancelled_wait_keeps_write_lock_and_admission_until_completion() {
    assert_cancelled_write_completes(false, false, false).await;
}
#[tokio::test]
async fn pull_requests_checkout_cancelled_fetch_completes_managed_creation() {
    assert_cancelled_write_completes(true, false, false).await;
}
#[tokio::test]
async fn pull_requests_checkout_gitlab_cancelled_wait_finishes_checkout() {
    assert_cancelled_write_completes(false, true, false).await;
}
#[tokio::test]
async fn pull_requests_checkout_admission_loss_alone_detaches_wait() {
    assert_cancelled_write_completes(false, false, true).await;
}
async fn assert_cancelled_write_completes(new_worktree: bool, gitlab: bool, loss_only: bool) {
    let f = Fixture::new().await;
    if gitlab {
        f.configure_gitlab();
    }
    let gate = f.pause_checkout_write(new_worktree);
    let cancellation = CancellationToken::new();
    let request = f.request(if new_worktree {
        json!({"kind":"worktree","branchName":null})
    } else {
        json!({"kind":"checkout","cwd":f.main})
    });
    let services = f.services.clone();
    let token = cancellation.clone();
    let mut waiter = tokio::spawn(async move { services.mutation_unary(request, token).await });
    await_file(&gate.started).await;
    if !loss_only {
        cancellation.cancel();
    }
    // Removal can revoke admission, but cannot finish draining an active write.
    let removal = f
        .availability
        .mark_removing("checkout-owner", &f.main)
        .await
        .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_millis(250), &mut waiter).await;
    let identity = removal.identity();
    let admission_held = f.availability.has_removal_admissions(&identity);
    let lock_wait = f.catalog.with_project_mutation_lock("project", || async {});
    tokio::pin!(lock_wait);
    let lock_held = tokio::time::timeout(std::time::Duration::from_millis(50), &mut lock_wait)
        .await
        .is_err();
    // The runtime's shutdown must drain the write instead of aborting it.
    let runtime = f.managed.operation_runtime();
    let shutdown = runtime.shutdown();
    tokio::pin!(shutdown);
    let shutdown_waited = tokio::time::timeout(std::time::Duration::from_millis(50), &mut shutdown)
        .await
        .is_err();
    fs::write(&gate.release, "release").unwrap();
    if shutdown_waited {
        tokio::time::timeout(std::time::Duration::from_secs(10), &mut shutdown)
            .await
            .expect("write drains on shutdown");
    }
    assert!(
        gate.completed.exists(),
        "client cancellation killed an admitted Git write"
    );
    assert!(
        !f.main.join(".git/index.lock").exists(),
        "write left a stale index lock"
    );
    assert!(
        admission_held,
        "availability admission was released before the write completed"
    );
    assert!(
        lock_held,
        "repository lock was released before the write completed"
    );
    assert!(shutdown_waited, "shutdown did not retain the running write");
    assert_eq!(
        reply
            .expect("cancel stops waiting without stopping the write")
            .unwrap()
            .unwrap_err()["message"],
        "Checkout continues in the background; refresh to see the result"
    );
    assert!(!f.availability.has_removal_admissions(&identity));
    removal.release();
    if new_worktree {
        let snapshot = f
            .catalog
            .refresh("project", CatalogRefreshTrigger::Explicit)
            .await
            .unwrap();
        let worktree = snapshot
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some("feature"))
            .expect("completed worktree visible on refresh");
        assert!(worktree.adopted_thread_id.is_some());
        assert_eq!(
            git(Path::new(&worktree.path), &["branch", "--show-current"]),
            "feature"
        );
    } else {
        assert_eq!(git(&f.main, &["branch", "--show-current"]), "feature");
        assert_eq!(git(&f.main, &["status", "--porcelain"]), "");
    }
    f.engine.shutdown().await;
}

#[tokio::test]
async fn pull_requests_checkout_write_outlives_sixty_second_mutation_budget() {
    assert_write_outlives_budget(false).await;
}
#[tokio::test]
async fn pull_requests_checkout_fetch_outlives_sixty_second_mutation_budget() {
    assert_write_outlives_budget(true).await;
}
async fn assert_write_outlives_budget(new_worktree: bool) {
    let f = Fixture::new().await;
    let gate = f.pause_checkout_write(new_worktree);
    let request = f.request(if new_worktree {
        json!({"kind":"worktree","branchName":null})
    } else {
        json!({"kind":"checkout","cwd":f.main})
    });
    let services = f.services.clone();
    let mut waiter = tokio::spawn(async move {
        services
            .mutation_unary(request, CancellationToken::new())
            .await
    });
    await_file(&gate.started).await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    tokio::time::resume();
    let reply = tokio::time::timeout(std::time::Duration::from_millis(250), &mut waiter).await;
    fs::write(&gate.release, "release").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        f.managed.operation_runtime().shutdown(),
    )
    .await
    .expect("write completes after the wait deadline");
    assert_eq!(
        reply
            .expect("deadline stops waiting without stopping the write")
            .unwrap()
            .unwrap_err()["message"],
        "Checkout continues in the background; refresh to see the result"
    );
    let cwd = if new_worktree {
        let snapshot = f
            .catalog
            .refresh("project", CatalogRefreshTrigger::Explicit)
            .await
            .unwrap();
        PathBuf::from(
            &snapshot
                .worktrees
                .iter()
                .find(|w| w.branch.as_deref() == Some("feature"))
                .unwrap()
                .path,
        )
    } else {
        f.main.clone()
    };
    assert!(gate.completed.exists());
    assert!(!f.main.join(".git/index.lock").exists());
    assert_eq!(git(&cwd, &["branch", "--show-current"]), "feature");
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_managed_add_outlives_its_default_git_deadline() {
    let f = Fixture::new().await;
    let gate = CheckoutWriteGate {
        started: f.root.path().join("add-started"),
        release: f.root.path().join("release-add"),
        completed: f.root.path().join("add-completed"),
    };
    let hooks = f.root.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    script(
        &hooks.join("post-checkout"),
        &format!(
            r#"#!/bin/sh
touch '{}'
while [ ! -f '{}' ]; do sleep 0.01; done
printf 'worktree add finished\n' > '{}'
"#,
            gate.started.display(),
            gate.release.display(),
            gate.completed.display()
        ),
    );
    git(
        &f.main,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let services = f.services.clone();
    let request = f.request(json!({"kind":"worktree","branchName":null}));
    let mut waiter = tokio::spawn(async move {
        services
            .mutation_unary(request, CancellationToken::new())
            .await
    });
    await_file(&gate.started).await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    tokio::time::resume();
    let reply = tokio::time::timeout(std::time::Duration::from_millis(250), &mut waiter).await;
    fs::write(&gate.release, "release").unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        f.managed.operation_runtime().shutdown(),
    )
    .await
    .expect("managed creation drains");
    assert_eq!(
        reply.expect("deadline stops waiting").unwrap().unwrap_err()["message"],
        "Checkout continues in the background; refresh to see the result"
    );
    assert!(
        gate.completed.exists(),
        "worktree add was killed at its old Git deadline"
    );
    let snapshot = f
        .catalog
        .refresh("project", CatalogRefreshTrigger::Explicit)
        .await
        .unwrap();
    assert_eq!(
        snapshot.worktrees.len(),
        2,
        "deadline must not leave an orphaned worktree"
    );
    let worktree = snapshot
        .worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("feature"))
        .unwrap();
    let cwd = Path::new(&worktree.path);
    assert_eq!(git(cwd, &["branch", "--show-current"]), "feature");
    let git_dir = git(cwd, &["rev-parse", "--absolute-git-dir"]);
    assert!(!Path::new(&git_dir).join("index.lock").exists());
    assert!(
        f.repositories
            .get_thread(worktree.adopted_thread_id.as_ref().unwrap().clone())
            .await
            .unwrap()
            .is_some()
    );
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_current_matching_branch_gets_pr_suffix() {
    let f = Fixture::new().await;
    git(&f.main, &["switch", "-c", "feature", "origin/feature"]);
    let original = git(&f.main, &["rev-parse", "HEAD"]);
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "feature-pr-7");
    assert_eq!(git(&f.main, &["branch", "--show-current"]), "feature");
    assert_eq!(git(&f.main, &["rev-parse", "HEAD"]), original);
    let cwd = Path::new(result["cwd"].as_str().unwrap());
    assert_eq!(git(cwd, &["branch", "--show-current"]), "feature-pr-7");
    assert_eq!(git(cwd, &["rev-parse", "HEAD"]), original);
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_skips_occupied_pr_suffix_and_reuses_free_matching_suffix() {
    let f = Fixture::new().await;
    git(&f.main, &["switch", "-c", "feature", "origin/feature"]);
    let occupied = f.root.path().join("occupied-pr-suffix");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "feature-pr-7",
            occupied.to_str().unwrap(),
            "origin/feature",
        ],
    );
    git(&f.main, &["branch", "feature-pr-7-2", "origin/feature"]);
    let tip = git(&f.main, &["rev-parse", "feature-pr-7-2"]);
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "feature-pr-7-2");
    assert_eq!(
        git(&occupied, &["branch", "--show-current"]),
        "feature-pr-7"
    );
    assert_eq!(git(&f.main, &["branch", "--show-current"]), "feature");
    assert_eq!(
        git(
            Path::new(result["cwd"].as_str().unwrap()),
            &["rev-parse", "HEAD"]
        ),
        tip
    );
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_reuses_an_unoccupied_matching_head_branch() {
    let f = Fixture::new().await;
    git(&f.main, &["branch", "feature", "origin/feature"]);
    let tip = git(&f.main, &["rev-parse", "feature"]);
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "feature");
    assert_eq!(git(&f.main, &["rev-parse", "feature"]), tip);
    assert_eq!(
        git(
            Path::new(result["cwd"].as_str().unwrap()),
            &["branch", "--show-current"]
        ),
        "feature"
    );
    assert!(!f.called());
    f.finish().await;
}

#[tokio::test]
async fn pull_requests_checkout_missing_worktree_registration_still_gets_pr_suffix() {
    let f = Fixture::new().await;
    let missing = f.root.path().join("missing-feature");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            missing.to_str().unwrap(),
            "origin/feature",
        ],
    );
    fs::remove_dir_all(&missing).unwrap();
    let tip = git(&f.main, &["rev-parse", "feature"]);
    let result = f
        .checkout(json!({"kind":"worktree","branchName":null}))
        .await
        .unwrap();
    assert_eq!(result["branch"], "feature-pr-7");
    assert_eq!(git(&f.main, &["rev-parse", "feature"]), tip);
    assert!(!missing.exists());
    assert!(!f.called());
    f.finish().await;
}
