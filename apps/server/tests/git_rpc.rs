#![allow(dead_code)]

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use coddy_studio_server::{
    git, production::host_paths::process_compatible_path, source_control, vcs,
};

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

use git::{
    BoxWorktreeBaseDirectoryFuture, CreateWorktreeInput, GitRepository, OutputPolicy, ProcessError,
    ProcessRequest, ProcessRunner, StatusBroadcaster, VcsStagingArea, VcsStatusStreamEvent,
    VcsWorkingTreeFileStatus, WorktreeBaseDirectoryProvider, git_worktree_prune_impact_digest,
    normalize_worktree_path_key, parse_porcelain_v2_line,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[path = "support/isolated_git_config.rs"]
mod isolated_git_config;
#[path = "support/reexec.rs"]
mod reexec;
use isolated_git_config::IsolatedGitConfig;

#[derive(Clone)]
struct StaticWorktreeBaseDirectory(Option<PathBuf>);

impl WorktreeBaseDirectoryProvider for StaticWorktreeBaseDirectory {
    fn worktree_base_directory<'a>(&'a self) -> BoxWorktreeBaseDirectoryFuture<'a> {
        let value = self.0.clone();
        Box::pin(async move { value })
    }
}

fn repository_with_workspace(path: Option<PathBuf>) -> GitRepository {
    GitRepository::with_worktree_settings(
        Arc::new(StaticWorktreeBaseDirectory(path)),
        Arc::default(),
    )
}

#[test]
fn production_vcs_observation_has_no_periodic_ref_worker() {
    let broadcaster = include_str!("../src/git/broadcaster.rs");
    let tests_start = broadcaster
        .find("\nmod tests {")
        .expect("broadcaster test module remains discoverable");
    let production_broadcaster = &broadcaster[..tests_start];

    for removed_worker_symbol in [
        "currentRef",
        "symbolic-ref",
        "ref_refresh_interval",
        "refresh_ref_for_lifecycle",
        "spawn_remote_and_ref_poller",
    ] {
        assert!(
            !production_broadcaster.contains(removed_worker_symbol),
            "production broadcaster must not restore the periodic ref worker symbol {removed_worker_symbol}"
        );
    }

    let production_git_vcs = include_str!("../src/production/git_vcs.rs");
    assert!(!production_git_vcs.contains("REF_REFRESH_INTERVAL"));
    assert!(!production_git_vcs.contains("Duration::from_secs(3)"));
}

#[test]
fn production_git_manager_has_no_background_poller_or_direct_network_client() {
    let sources = [
        (
            "git/manager/mod.rs",
            include_str!("../src/git/manager/mod.rs"),
        ),
        (
            "git/manager/conflicts.rs",
            include_str!("../src/git/manager/conflicts.rs"),
        ),
        (
            "git/manager/generation.rs",
            include_str!("../src/git/manager/generation.rs"),
        ),
        (
            "git/manager/graph.rs",
            include_str!("../src/git/manager/graph.rs"),
        ),
        (
            "git/manager/guards.rs",
            include_str!("../src/git/manager/guards.rs"),
        ),
        (
            "git/manager/in_progress.rs",
            include_str!("../src/git/manager/in_progress.rs"),
        ),
        (
            "git/manager/merge.rs",
            include_str!("../src/git/manager/merge.rs"),
        ),
        (
            "git/manager/operations.rs",
            include_str!("../src/git/manager/operations.rs"),
        ),
        (
            "git/manager/patch.rs",
            include_str!("../src/git/manager/patch.rs"),
        ),
        (
            "git/manager/refs.rs",
            include_str!("../src/git/manager/refs.rs"),
        ),
        (
            "git/manager/rewrite.rs",
            include_str!("../src/git/manager/rewrite.rs"),
        ),
        (
            "git/manager/stash.rs",
            include_str!("../src/git/manager/stash.rs"),
        ),
        (
            "git/manager/tags.rs",
            include_str!("../src/git/manager/tags.rs"),
        ),
        (
            "source_control/checks.rs",
            include_str!("../src/source_control/checks.rs"),
        ),
    ];

    for (path, source) in sources {
        let production = source
            .split_once("\nmod tests {")
            .map_or(source, |(production, _tests)| production);
        for forbidden in ["interval(", "sleep_until", "https://", "reqwest"] {
            assert!(
                !production.contains(forbidden),
                "production {path} must not contain {forbidden}"
            );
        }
        assert!(
            !(production.contains("tokio::spawn") && production.contains("loop {")),
            "production {path} must not pair tokio::spawn with a loop"
        );
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Coddy Studio Test")
        .env("GIT_AUTHOR_EMAIL", "coddy-studio@example.invalid")
        .env("GIT_COMMITTER_NAME", "Coddy Studio Test")
        .env("GIT_COMMITTER_EMAIL", "coddy-studio@example.invalid")
        .output()
        .expect("git must be installed for real-repository tests");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn has_registered_worktree(cwd: &Path, expected: &Path) -> bool {
    git(cwd, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .any(|path| same_worktree_identity(&path, expected))
}

fn same_worktree_identity(left: &Path, right: &Path) -> bool {
    normalize_worktree_path_key(left, git::host_path_platform())
        == normalize_worktree_path_key(right, git::host_path_platform())
}

fn init_repo() -> TempDir {
    let temp = tempfile::tempdir().expect("temporary repository");
    initialize_repo_at(temp.path());
    temp
}

fn initialize_repo_at(path: &Path) {
    fs::create_dir_all(path).expect("repository directory");
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.name", "Coddy Studio Test"]);
    git(
        path,
        &["config", "user.email", "coddy-studio@example.invalid"],
    );
}

fn commit_file(repo: &Path, path: &str, contents: &str, message: &str) {
    let absolute = repo.join(path);
    if let Some(parent) = absolute.parent() {
        fs::create_dir_all(parent).expect("parent directory");
    }
    fs::write(absolute, contents).expect("fixture file");
    git(repo, &["add", "--", path]);
    git(repo, &["commit", "-m", message]);
}

fn cancellation() -> CancellationToken {
    CancellationToken::new()
}

async fn observe_initial_remote(
    snapshot: Option<VcsStatusStreamEvent>,
    subscription: &mut git::StatusSubscription,
) {
    match snapshot {
        Some(VcsStatusStreamEvent::Snapshot {
            remote: Some(_), ..
        }) => {}
        Some(VcsStatusStreamEvent::Snapshot { remote: None, .. }) => {
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(15), subscription.recv()).await,
                Ok(Some(VcsStatusStreamEvent::RemoteUpdated { .. }))
            ));
        }
        event => panic!("expected initial VCS snapshot, got {event:?}"),
    }
}

#[cfg(any(unix, windows))]
fn provider_cli_fixture(directory: &Path, command: &str) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        let path = directory.join(command);
        let script = r#"#!/bin/sh
case "$*" in
  *fail*) exit 7 ;;
  *invalid*) printf '%s\n' 'not-json'; exit 0 ;;
esac
case "$(basename "$0"):$*" in
  gh:*create*) printf '%s\n' 'https://github.com/example/repo/pull/42' ;;
  gh:*) printf '%s\n' '{"number":42,"title":"GitHub PR","url":"https://github.test/42","baseRefName":"main","headRefName":"feature","state":"OPEN"}' ;;
  glab:*) printf '%s\n' '{"iid":43,"title":"GitLab MR","web_url":"https://gitlab.test/43","target_branch":"main","source_branch":"feature","state":"opened"}' ;;
  az:*list*) printf '%s\n' '[{"pullRequestId":44,"title":"Azure PR","url":"https://azure.test/44","targetRefName":"refs/heads/main","sourceRefName":"refs/heads/feature","status":"active"}]' ;;
  az:*) printf '%s\n' '{"pullRequestId":44,"title":"Azure PR","url":"https://azure.test/44","targetRefName":"refs/heads/main","sourceRefName":"refs/heads/feature","status":"active"}' ;;
esac
"#;
        executable_fixture::write_executable(&path, script);
        path
    }
    #[cfg(windows)]
    {
        let path = directory.join(format!("{command}.cmd"));
        let success = match command {
            "gh" => {
                r#"echo %* | %SystemRoot%\System32\findstr.exe /C:"create" >nul && (
  echo https://github.com/example/repo/pull/42
  exit /b 0
)
echo {"number":42,"title":"GitHub PR","url":"https://github.test/42","baseRefName":"main","headRefName":"feature","state":"OPEN"}"#
            }
            "glab" => {
                r#"echo {"iid":43,"title":"GitLab MR","web_url":"https://gitlab.test/43","target_branch":"main","source_branch":"feature","state":"opened"}"#
            }
            "az" => {
                r#"echo %* | %SystemRoot%\System32\findstr.exe /C:"list" >nul && (
  echo [{"pullRequestId":44,"title":"Azure PR","url":"https://azure.test/44","targetRefName":"refs/heads/main","sourceRefName":"refs/heads/feature","status":"active"}]
  exit /b 0
)
echo {"pullRequestId":44,"title":"Azure PR","url":"https://azure.test/44","targetRefName":"refs/heads/main","sourceRefName":"refs/heads/feature","status":"active"}"#
            }
            _ => unreachable!("unsupported provider fixture"),
        };
        let script = format!(
            "@echo off\r\n\
             echo %* | %SystemRoot%\\System32\\findstr.exe /C:\"fail\" >nul && exit /b 7\r\n\
             echo %* | %SystemRoot%\\System32\\findstr.exe /C:\"invalid\" >nul && (\r\n\
             echo not-json\r\n\
             exit /b 0\r\n\
             )\r\n\
             {success}\r\n"
        );
        executable_fixture::write_executable(&path, script);
        path
    }
}

#[tokio::test]
async fn provider_cli_service_covers_success_exit_and_payload_failures_in_public_build() {
    use source_control::{
        CreatePullRequestInput, ProviderKind, PullRequestService, ResolvePullRequestInput,
    };

    let temporary = tempfile::tempdir().expect("provider CLI directory");
    let github = provider_cli_fixture(temporary.path(), "gh");
    let gitlab = provider_cli_fixture(temporary.path(), "glab");
    let azure = provider_cli_fixture(temporary.path(), "az");
    let service = PullRequestService::with_provider_commands(
        github.to_string_lossy(),
        gitlab.to_string_lossy(),
        azure.to_string_lossy(),
    );
    let cancellation = CancellationToken::new();
    let azure_current = service
        .resolve_current(
            ResolvePullRequestInput {
                cwd: temporary.path().to_path_buf(),
                provider: ProviderKind::AzureDevops,
                reference: "feature".to_owned(),
            },
            &cancellation,
        )
        .await
        .unwrap();
    assert_eq!(azure_current.number, 44);

    for (provider, expected) in [
        (ProviderKind::Github, 42),
        (ProviderKind::Gitlab, 43),
        (ProviderKind::AzureDevops, 44),
    ] {
        let input = |reference: &str| ResolvePullRequestInput {
            cwd: temporary.path().to_path_buf(),
            provider,
            reference: reference.to_owned(),
        };
        assert_eq!(
            service
                .resolve(input(&expected.to_string()), &cancellation)
                .await
                .unwrap()
                .number,
            expected
        );
        assert!(service.resolve(input("fail"), &cancellation).await.is_err());
        assert!(
            service
                .resolve(input("invalid"), &cancellation)
                .await
                .is_err()
        );

        let create = |head_branch: &str| CreatePullRequestInput {
            cwd: temporary.path().to_path_buf(),
            provider,
            base_branch: "main".to_owned(),
            head_branch: head_branch.to_owned(),
            title: "Fixture".to_owned(),
            body: "Body".to_owned(),
        };
        assert_eq!(
            service
                .create(create("feature"), &cancellation)
                .await
                .unwrap()
                .number,
            expected
        );
        assert!(service.create(create("fail"), &cancellation).await.is_err());
    }
}

#[tokio::test]
async fn unborn_head_can_be_unstaged_and_has_empty_history() {
    let repo = init_repo();
    fs::write(repo.path().join("new.txt"), "new\n").expect("new file");
    let repository = GitRepository::default();

    repository
        .stage_files(repo.path(), &["new.txt".into()], &cancellation())
        .await
        .expect("stage on unborn branch");
    let staged = repository
        .local_status(repo.path(), &cancellation())
        .await
        .expect("status");
    assert!(staged.working_tree.files.iter().any(|file| {
        file.path == "new.txt"
            && file.area == Some(VcsStagingArea::Staged)
            && file.status == Some(VcsWorkingTreeFileStatus::Added)
    }));

    repository
        .unstage_files(repo.path(), &["new.txt".into()], &cancellation())
        .await
        .expect("unborn unstage");
    let unstaged = repository
        .local_status(repo.path(), &cancellation())
        .await
        .expect("status after unstage");
    assert!(
        unstaged
            .working_tree
            .files
            .iter()
            .any(|file| { file.path == "new.txt" && file.area == Some(VcsStagingArea::Untracked) })
    );
    assert!(
        repository
            .list_commits(repo.path(), 30, 0, &cancellation())
            .await
            .expect("unborn history")
            .commits
            .is_empty()
    );
}

#[tokio::test]
async fn corrupt_history_is_reported_instead_of_rendered_as_empty() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let sha = git(repo.path(), &["rev-parse", "HEAD"]);
    fs::remove_file(
        repo.path()
            .join(".git/objects")
            .join(&sha[..2])
            .join(&sha[2..]),
    )
    .expect("remove loose commit object");

    let error = GitRepository::default()
        .list_commits(repo.path(), 30, 0, &cancellation())
        .await
        .expect_err("corrupt history must be visible");
    assert_eq!(error.tag, "GitCommandError");
    assert_eq!(error.operation.as_ref(), "GitVcsDriver.listCommits");
    assert_ne!(
        error.detail.as_ref(),
        "Git process exited with a non-zero status."
    );
}

#[test]
fn porcelain_v2_preserves_current_rename_and_copy_paths() {
    let renamed = parse_porcelain_v2_line(
        "2 R. N... 100644 100644 100644 abcdef0 abcdef1 R100 src/new name.rs\tsrc/old name.rs",
    )
    .expect("rename record");
    assert_eq!(renamed.path, "src/new name.rs");
    assert_eq!(renamed.index_status, VcsWorkingTreeFileStatus::Renamed);

    let copied = parse_porcelain_v2_line(
        "2 C. N... 100644 100644 100644 abcdef0 abcdef1 C075 copied file.txt\toriginal file.txt",
    )
    .expect("copy record");
    assert_eq!(copied.path, "copied file.txt");
    assert_eq!(copied.index_status, VcsWorkingTreeFileStatus::Copied);
}

#[tokio::test]
async fn real_rename_status_uses_the_new_path() {
    let repo = init_repo();
    commit_file(repo.path(), "old name.txt", "old\n", "initial");
    git(repo.path(), &["mv", "old name.txt", "new name.txt"]);

    let status = GitRepository::default()
        .local_status(repo.path(), &cancellation())
        .await
        .expect("rename status");
    assert!(status.working_tree.files.iter().any(|file| {
        file.path == "new name.txt"
            && file.area == Some(VcsStagingArea::Staged)
            && file.status == Some(VcsWorkingTreeFileStatus::Renamed)
    }));
}

#[tokio::test]
async fn discard_preserves_staged_snapshot_and_removes_only_selected_untracked_paths() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    fs::write(repo.path().join("tracked.txt"), "staged\n").expect("staged edit");
    git(repo.path(), &["add", "tracked.txt"]);
    fs::write(repo.path().join("tracked.txt"), "unstaged\n").expect("unstaged edit");
    fs::create_dir_all(repo.path().join("selected/nested")).expect("selected directory");
    fs::write(repo.path().join("selected/nested/file.txt"), "remove\n").expect("selected file");
    fs::write(repo.path().join("keep.txt"), "keep\n").expect("unrelated file");

    let repository = GitRepository::default();
    repository
        .discard_files(
            repo.path(),
            &["tracked.txt".into(), "selected/nested/file.txt".into()],
            &cancellation(),
        )
        .await
        .expect("safe discard");

    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt"))
            .expect("tracked contents")
            .replace("\r\n", "\n"),
        "staged\n"
    );
    assert!(!repo.path().join("selected/nested/file.txt").exists());
    assert!(repo.path().join("keep.txt").exists());
    assert!(git(repo.path(), &["diff", "--cached", "--", "tracked.txt"]).contains("+staged"));

    repository
        .discard_files(repo.path(), &[], &cancellation())
        .await
        .expect("empty discard is a no-op");
    assert!(repo.path().join("keep.txt").exists());
}

#[tokio::test]
async fn worktree_lifecycle_is_reflected_in_ref_listing() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let worktree_parent = tempfile::tempdir().expect("temporary worktree parent");
    let worktree_path = worktree_parent.path().join("feature-worktree");
    let repository = GitRepository::default();

    let created = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: Some("feature/worktree".into()),
                base_ref_name: Some("main".into()),
                path: Some(worktree_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("create worktree");
    let refs = repository
        .list_refs(repo.path(), None, 0, 100, true, None, &cancellation())
        .await
        .expect("list refs");
    let feature = refs
        .refs
        .iter()
        .find(|reference| reference.name == "feature/worktree")
        .expect("feature ref");
    assert_eq!(
        feature
            .worktree_path
            .as_deref()
            .map(|path| path.replace('\\', "/")),
        Some(created.worktree.path.replace('\\', "/"))
    );

    repository
        .remove_worktree(repo.path(), &worktree_path, false, &cancellation())
        .await
        .expect("remove worktree");
    assert!(!worktree_path.exists());
}

#[tokio::test]
async fn worktree_verified_targeted_cleanup_removes_only_the_missing_registration() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let parent = tempfile::tempdir().expect("temporary worktree parent");
    let worktree_path = parent.path().join("stale-target");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/stale-target",
            "--",
            &worktree_path.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let registered = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("registered inventory");
    let registered_path = fs::canonicalize(&worktree_path).expect("canonical registered path");
    fs::rename(&worktree_path, parent.path().join("unrelated-moved-copy"))
        .expect("make registration stale without deleting files");
    let stale = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("stale inventory")
        .records
        .into_iter()
        .find(|record| same_worktree_identity(&record.path, &registered_path))
        .expect("stale target record");
    assert!(
        registered
            .records
            .iter()
            .any(|record| same_worktree_identity(&record.path, &stale.path))
    );

    let inspection = repository
        .inspect_worktree_removal(repo.path(), &stale, &cancellation())
        .await
        .expect("missing target skips dirty inspection");
    assert_eq!(inspection.tracked_change_count, 0);
    assert_eq!(inspection.untracked_file_count, 0);
    repository
        .remove_worktree_verified(repo.path(), &stale, false, &cancellation())
        .await
        .expect("targeted stale registration cleanup");

    assert!(!has_registered_worktree(repo.path(), &registered_path));
    assert_eq!(
        git(repo.path(), &["branch", "--list", "feature/stale-target"]),
        "feature/stale-target"
    );
    assert!(
        parent
            .path()
            .join("unrelated-moved-copy/README.md")
            .is_file()
    );
}

#[tokio::test]
async fn worktree_repository_wide_prune_requires_the_exact_public_preview_digest() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let parent = tempfile::tempdir().expect("temporary worktree parent");
    let worktree_path = parent.path().join("stale-prune");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/stale-prune",
            "--",
            &worktree_path.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let registered_path = fs::canonicalize(&worktree_path).expect("canonical registered path");
    fs::rename(&worktree_path, parent.path().join("moved-prune-copy"))
        .expect("make registration stale without deleting files");
    let target = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("stale inventory")
        .records
        .into_iter()
        .find(|record| same_worktree_identity(&record.path, &registered_path))
        .expect("stale target record");
    let impact = repository
        .preview_worktree_prune(repo.path(), &cancellation())
        .await
        .expect("public prune preview");

    repository
        .prune_worktrees_verified(repo.path(), &target, "wrong-digest", &cancellation())
        .await
        .expect_err("unconfirmed impact cannot prune");
    assert!(has_registered_worktree(repo.path(), &registered_path));
    repository
        .prune_worktrees_verified(
            repo.path(),
            &target,
            &git_worktree_prune_impact_digest(&impact),
            &cancellation(),
        )
        .await
        .expect("confirmed public preview permits prune");
    assert!(!has_registered_worktree(repo.path(), &registered_path));
}

#[tokio::test]
async fn current_local_branch_creates_the_first_available_suffixed_branch() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let worktree_parent = tempfile::tempdir().expect("temporary worktree parent");
    let worktree_path = worktree_parent.path().join("main-sibling");

    let created = GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(worktree_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("checked-out main creates a sibling branch");

    assert_eq!(created.worktree.ref_name, "main-2");
    assert_eq!(git(&worktree_path, &["branch", "--show-current"]), "main-2");
    assert_eq!(
        git(&worktree_path, &["rev-parse", "HEAD"]),
        git(repo.path(), &["rev-parse", "main"])
    );
}

#[tokio::test]
async fn configured_workspace_owns_default_and_occupied_branch_destinations() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let workspace = tempfile::tempdir().expect("workspace");
    let canonical_workspace = fs::canonicalize(workspace.path()).expect("canonical workspace");
    let repository = repository_with_workspace(Some(workspace.path().to_path_buf()));
    let cancellation = cancellation();

    let first = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".to_owned(),
                new_ref_name: None,
                base_ref_name: None,
                path: None,
            },
            &cancellation,
        )
        .await
        .expect("configured worktree");

    assert_eq!(first.worktree.ref_name, "main-2");
    let canonical_worktree =
        fs::canonicalize(&first.worktree.path).expect("canonical created worktree");
    assert!(canonical_worktree.starts_with(&canonical_workspace));
    assert_eq!(
        canonical_worktree.parent().and_then(Path::parent),
        Some(canonical_workspace.as_path())
    );

    repository
        .remove_worktree(
            repo.path(),
            Path::new(&first.worktree.path),
            true,
            &cancellation,
        )
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn unavailable_configured_workspace_fails_without_project_adjacent_fallback() {
    let repository_parent = tempfile::tempdir().expect("temporary repository parent");
    let repo = repository_parent.path().join("repository");
    initialize_repo_at(&repo);
    let missing = repo.join("missing-workspace");
    let repository = repository_with_workspace(Some(missing));

    let error = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.clone(),
                ref_name: "main".to_owned(),
                new_ref_name: None,
                base_ref_name: None,
                path: None,
            },
            &cancellation(),
        )
        .await
        .expect_err("missing workspace");

    assert!(error.detail.contains("Workspace"));
    assert!(error.detail.contains("Settings → General"));
    assert!(
        !repo
            .parent()
            .expect("temporary repository parent")
            .join(".coddy-studio-worktrees")
            .exists()
    );
}

#[tokio::test]
async fn configured_workspace_child_creation_failures_use_the_workspace_error() {
    let repository_parent = tempfile::tempdir().expect("temporary repository parent");
    let repo = repository_parent.path().join("repository");
    initialize_repo_at(&repo);
    commit_file(&repo, "README.md", "hello\n", "initial");
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(
        workspace.path().join("repository"),
        "blocks child creation\n",
    )
    .expect("blocking repository-directory fixture");
    let repository = repository_with_workspace(Some(workspace.path().to_path_buf()));

    let error = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.clone(),
                ref_name: "main".to_owned(),
                new_ref_name: Some("feature/blocked".to_owned()),
                base_ref_name: Some("main".to_owned()),
                path: None,
            },
            &cancellation(),
        )
        .await
        .expect_err("configured workspace cannot create its repository directory");

    let expected_detail = format!(
        "Workspace {} is unavailable. Choose another folder in Settings → General.",
        workspace.path().to_string_lossy().replace('\\', "/")
    );
    assert_eq!(error.detail.as_ref(), expected_detail);
    let occupied_error = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.clone(),
                ref_name: "main".to_owned(),
                new_ref_name: None,
                base_ref_name: None,
                path: None,
            },
            &cancellation(),
        )
        .await
        .expect_err("configured workspace cannot reserve an occupied-branch destination");
    assert_eq!(occupied_error.detail.as_ref(), expected_detail);
    assert!(
        !repo
            .parent()
            .expect("temporary repository parent")
            .join(".coddy-studio-worktrees")
            .exists()
    );
    assert!(git(&repo, &["branch", "--list", "feature/blocked"]).is_empty());
}

#[tokio::test]
async fn occupied_suffix_branches_and_default_paths_are_skipped() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(repo.path(), &["branch", "main-2"]);
    let repo_name = repo.path().file_name().expect("temporary repository name");
    let default_root = repo
        .path()
        .parent()
        .expect("temporary repository parent")
        .join(".coddy-studio-worktrees")
        .join(repo_name);
    fs::create_dir_all(default_root.join("main-3")).expect("colliding default path");

    let created = GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: None,
            },
            &cancellation(),
        )
        .await
        .expect("available suffix is selected");

    assert_eq!(created.worktree.ref_name, "main-4");
    assert!(
        created
            .worktree
            .path
            .replace('\\', "/")
            .ends_with("/main-4")
    );
    GitRepository::default()
        .remove_worktree(
            repo.path(),
            Path::new(&created.worktree.path),
            true,
            &cancellation(),
        )
        .await
        .expect("remove generated worktree");
    fs::remove_dir_all(default_root).expect("remove default worktree fixtures");
}

#[tokio::test]
async fn unoccupied_local_branch_is_checked_out_without_creating_a_sibling() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(repo.path(), &["branch", "feature/free"]);
    let worktree_parent = tempfile::tempdir().expect("temporary worktree parent");
    let worktree_path = worktree_parent.path().join("free");

    let created = GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "feature/free".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(worktree_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("free local branch is reused");

    assert_eq!(created.worktree.ref_name, "feature/free");
    assert_eq!(
        git(&worktree_path, &["branch", "--show-current"]),
        "feature/free"
    );
    assert!(git(repo.path(), &["branch", "--list", "feature/free-2"]).is_empty());
}

#[tokio::test]
async fn remote_ref_remains_an_exact_detached_worktree_source() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/feature", "HEAD"],
    );
    let worktree_parent = tempfile::tempdir().expect("temporary worktree parent");
    let worktree_path = worktree_parent.path().join("remote-feature");

    let created = GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "origin/feature".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(worktree_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("remote ref creates from the exact ref");

    assert_eq!(created.worktree.ref_name, "origin/feature");
    assert!(git(&worktree_path, &["branch", "--show-current"]).is_empty());
    assert_eq!(
        git(&worktree_path, &["rev-parse", "HEAD"]),
        git(repo.path(), &["rev-parse", "origin/feature"])
    );
}

#[tokio::test]
async fn failed_occupied_branch_fallback_preserves_preexisting_suffix_branches() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(repo.path(), &["branch", "main-2"]);
    let blocked_parent = repo.path().join("not-a-directory");
    fs::write(&blocked_parent, "blocked\n").expect("blocking path fixture");

    GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(blocked_parent.join("worktree")),
            },
            &cancellation(),
        )
        .await
        .expect_err("invalid worktree path fails");

    assert_eq!(git(repo.path(), &["branch", "--list", "main-2"]), "main-2");
    assert!(git(repo.path(), &["branch", "--list", "main-3"]).is_empty());
}

#[tokio::test]
async fn occupied_branch_fallback_does_not_claim_a_preexisting_explicit_path() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let worktree_parent = tempfile::tempdir().expect("temporary worktree parent");
    let preexisting_path = worktree_parent.path().join("already-there");
    fs::create_dir(&preexisting_path).expect("pre-existing empty path");

    GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(preexisting_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect_err("an explicit path must be absent before Coddy Studio can own it");

    assert!(preexisting_path.is_dir());
    assert!(
        fs::read_dir(&preexisting_path)
            .expect("pre-existing path remains readable")
            .next()
            .is_none()
    );
    assert!(git(repo.path(), &["branch", "--list", "main-2"]).is_empty());
}

#[tokio::test]
async fn missing_registered_worktree_still_owns_its_local_branch() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(repo.path(), &["branch", "feature/stale"]);
    let stale_parent = tempfile::tempdir().expect("stale worktree parent");
    let stale_path = stale_parent.path().join("stale");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            &stale_path.to_string_lossy(),
            "feature/stale",
        ],
    );
    let canonical_stale_path = fs::canonicalize(&stale_path).expect("canonical stale worktree");
    fs::remove_dir_all(&stale_path).expect("simulate a missing registered worktree");

    let repository = GitRepository::default();
    let refs = repository
        .list_refs(repo.path(), None, 0, 100, true, None, &cancellation())
        .await
        .expect("list refs with stale registration");
    let stale_ref = refs
        .refs
        .iter()
        .find(|reference| reference.name == "feature/stale")
        .expect("registered branch remains listed");
    assert_eq!(
        stale_ref
            .worktree_path
            .as_deref()
            .map(|path| path.replace('\\', "/")),
        Some(
            process_compatible_path(canonical_stale_path)
                .to_string_lossy()
                .replace('\\', "/"),
        )
    );

    let replacement_parent = tempfile::tempdir().expect("replacement worktree parent");
    let replacement_path = replacement_parent.path().join("replacement");
    let created = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "feature/stale".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(replacement_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("registered missing worktree triggers sibling fallback");
    assert_eq!(created.worktree.ref_name, "feature/stale-2");
    assert_eq!(
        git(&replacement_path, &["branch", "--show-current"]),
        "feature/stale-2"
    );
}

#[tokio::test]
async fn occupied_branch_relative_path_uses_one_repository_relative_target() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let leaf = format!(
        "coddy-studio-relative-occupied-{}",
        repo.path()
            .file_name()
            .expect("temporary repository name")
            .to_string_lossy()
    );
    let relative_path = PathBuf::from("..").join(&leaf);
    let expected_path = repo
        .path()
        .parent()
        .expect("temporary repository parent")
        .join(&leaf);
    let process_relative_path = std::env::current_dir()
        .expect("process working directory")
        .join(&relative_path);
    assert!(!expected_path.exists());
    assert!(!process_relative_path.exists());
    let repository = GitRepository::default();

    let created = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(relative_path),
            },
            &cancellation(),
        )
        .await
        .expect("relative occupied-branch target creates successfully");

    let canonical_expected = fs::canonicalize(&expected_path).expect("canonical created worktree");
    assert_eq!(
        fs::canonicalize(&created.worktree.path).expect("canonical returned worktree path"),
        canonical_expected
    );
    assert_eq!(created.worktree.ref_name, "main-2");
    assert_eq!(git(&expected_path, &["branch", "--show-current"]), "main-2");
    assert!(has_registered_worktree(repo.path(), &canonical_expected));
    assert!(!process_relative_path.exists());

    repository
        .remove_worktree(repo.path(), &expected_path, true, &cancellation())
        .await
        .expect("remove repository-relative worktree");
    assert!(!expected_path.exists());
    assert!(!has_registered_worktree(repo.path(), &canonical_expected));
}

#[tokio::test]
async fn occupied_branch_relative_path_preserves_a_preexisting_repository_relative_target() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let leaf = format!(
        "coddy-studio-relative-preexisting-{}",
        repo.path()
            .file_name()
            .expect("temporary repository name")
            .to_string_lossy()
    );
    let relative_path = PathBuf::from("..").join(&leaf);
    let expected_path = repo
        .path()
        .parent()
        .expect("temporary repository parent")
        .join(&leaf);
    fs::create_dir(&expected_path).expect("pre-existing repository-relative target");

    GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(relative_path),
            },
            &cancellation(),
        )
        .await
        .expect_err("pre-existing repository-relative target is not owned");

    assert!(expected_path.is_dir());
    assert!(
        fs::read_dir(&expected_path)
            .expect("pre-existing target remains readable")
            .next()
            .is_none()
    );
    assert!(
        !git(repo.path(), &["worktree", "list", "--porcelain"])
            .replace('\\', "/")
            .contains(&expected_path.to_string_lossy().replace('\\', "/"))
    );
    fs::remove_dir(&expected_path).expect("remove preserved fixture directory");
}

#[tokio::test]
async fn direct_existing_and_new_branch_relative_paths_return_the_repository_relative_location() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(repo.path(), &["branch", "feature/free-relative"]);
    let repository = GitRepository::default();
    let repo_name = repo
        .path()
        .file_name()
        .expect("temporary repository name")
        .to_string_lossy();

    for (leaf, ref_name, new_ref_name) in [
        (
            format!("coddy-studio-relative-free-{repo_name}"),
            "feature/free-relative",
            None,
        ),
        (
            format!("coddy-studio-relative-new-{repo_name}"),
            "main",
            Some("feature/new-relative"),
        ),
    ] {
        let relative_path = PathBuf::from("..").join(&leaf);
        let expected_path = repo
            .path()
            .parent()
            .expect("temporary repository parent")
            .join(&leaf);
        let created = repository
            .create_worktree(
                CreateWorktreeInput {
                    cwd: repo.path().to_path_buf(),
                    ref_name: ref_name.into(),
                    new_ref_name: new_ref_name.map(str::to_owned),
                    base_ref_name: None,
                    path: Some(relative_path),
                },
                &cancellation(),
            )
            .await
            .expect("direct relative worktree creation");
        let canonical_expected = fs::canonicalize(&expected_path).expect("canonical worktree");
        assert_eq!(
            fs::canonicalize(&created.worktree.path).expect("canonical returned worktree path"),
            canonical_expected
        );
        assert!(has_registered_worktree(repo.path(), &canonical_expected));
        repository
            .remove_worktree(repo.path(), &expected_path, true, &cancellation())
            .await
            .expect("remove direct relative worktree");
        assert!(!has_registered_worktree(repo.path(), &canonical_expected));
    }
}

#[cfg(windows)]
#[tokio::test]
async fn worktree_creation_accepts_windows_extended_length_repository_paths() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let extended_cwd = fs::canonicalize(repo.path()).expect("canonical repository path");
    assert!(
        extended_cwd.to_string_lossy().starts_with(r"\\?\"),
        "Windows canonicalization must exercise the extended path form"
    );
    let repository = GitRepository::default();

    let created = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: extended_cwd,
                ref_name: "main".into(),
                new_ref_name: Some("feature/extended-path".into()),
                base_ref_name: Some("main".into()),
                path: None,
            },
            &cancellation(),
        )
        .await
        .expect("extended repository paths create worktrees");
    assert!(!created.worktree.path.starts_with("//?/"));
    let worktree_path = Path::new(&created.worktree.path);
    assert!(worktree_path.is_dir());

    repository
        .remove_worktree(repo.path(), worktree_path, true, &cancellation())
        .await
        .expect("remove extended-path worktree");
    git(repo.path(), &["branch", "-D", "feature/extended-path"]);
}

#[tokio::test]
async fn managed_orphaned_worktree_directory_can_be_removed_after_git_registration_is_lost() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let workspace = tempfile::tempdir().expect("managed worktree workspace");
    let repository = repository_with_workspace(Some(workspace.path().to_path_buf()));
    let created = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: Some("feature/orphaned".into()),
                base_ref_name: Some("main".into()),
                path: None,
            },
            &cancellation(),
        )
        .await
        .expect("managed worktree creation");
    let worktree_path = PathBuf::from(created.worktree.path);
    let git_link = worktree_path.join(".git");
    let git_link_contents = fs::read_to_string(&git_link).expect("linked worktree Git file");
    let admin_path = PathBuf::from(
        git_link_contents
            .trim()
            .strip_prefix("gitdir: ")
            .expect("linked worktree Git directory"),
    );
    fs::remove_dir_all(admin_path).expect("simulate lost Git worktree registration");
    fs::remove_file(git_link).expect("simulate the partially removed Git link");
    assert!(!has_registered_worktree(repo.path(), &worktree_path));
    assert!(worktree_path.is_dir());

    repository
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect("managed orphan cleanup");
    assert!(!worktree_path.exists());
}

#[tokio::test]
async fn unregistered_directories_without_managed_orphan_proof_are_preserved() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let workspace = tempfile::tempdir().expect("managed worktree workspace");
    let repository = repository_with_workspace(Some(workspace.path().to_path_buf()));
    let unrelated_root = tempfile::tempdir().expect("unrelated parent");
    let outside_workspace = unrelated_root.path().join("unrelated");
    fs::create_dir(&outside_workspace).expect("unrelated directory");
    fs::write(outside_workspace.join("keep.txt"), "keep\n").expect("unrelated fixture");
    let repo_name = repo.path().file_name().expect("repository name");
    let repository_directory = workspace.path().join(repo_name);
    fs::create_dir(&repository_directory).expect("managed repository directory");
    let managed_repository = repository_directory.join("existing-repository");
    fs::create_dir(&managed_repository).expect("managed candidate");
    fs::create_dir(managed_repository.join(".git")).expect("repository marker");
    fs::write(managed_repository.join("keep.txt"), "keep\n").expect("managed fixture");

    for path in [&outside_workspace, &managed_repository] {
        repository
            .remove_worktree(repo.path(), path, true, &cancellation())
            .await
            .expect_err("unregistered directory is not a proven managed orphan");
        assert_eq!(
            fs::read_to_string(path.join("keep.txt")).expect("directory must remain"),
            "keep\n"
        );
    }
}

#[tokio::test]
async fn forced_worktree_removal_never_precleans_the_primary_checkout() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");

    GitRepository::default()
        .remove_worktree(repo.path(), repo.path(), true, &cancellation())
        .await
        .expect_err("Git must reject removing the primary checkout");
    assert_eq!(
        fs::read_to_string(repo.path().join("README.md")).expect("primary checkout remains"),
        "hello\n"
    );
}

#[tokio::test]
async fn stale_registered_path_replaced_without_a_git_link_is_not_precleaned() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree_path = worktree_parent.path().join("replaced");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/replaced",
            &worktree_path.to_string_lossy(),
        ],
    );
    fs::remove_dir_all(&worktree_path).expect("remove the original linked worktree");
    fs::create_dir(&worktree_path).expect("replacement directory");
    fs::write(worktree_path.join("keep.txt"), "keep\n").expect("replacement fixture");

    GitRepository::default()
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect_err("a replacement directory is not the registered linked worktree");
    assert_eq!(
        fs::read_to_string(worktree_path.join("keep.txt")).expect("replacement remains"),
        "keep\n"
    );
    fs::write(
        worktree_path.join(".git"),
        "gitdir: C:/missing-coddy-studio-worktree-admin\n",
    )
    .expect("malformed linked-worktree marker");
    GitRepository::default()
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect_err("an unverified Git file does not prove linked-worktree ownership");
    assert_eq!(
        fs::read_to_string(worktree_path.join("keep.txt")).expect("replacement still remains"),
        "keep\n"
    );
}

#[tokio::test]
async fn failed_worktree_creation_removes_only_the_branch_created_by_the_attempt() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    let blocked_parent = repo.path().join("not-a-directory");
    fs::write(&blocked_parent, "blocked\n").expect("blocking path fixture");
    let repository = GitRepository::default();

    repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: Some("feature/rollback-created-branch".into()),
                base_ref_name: Some("main".into()),
                path: Some(blocked_parent.join("worktree")),
            },
            &cancellation(),
        )
        .await
        .expect_err("invalid worktree path fails");

    assert!(
        git(
            repo.path(),
            &["branch", "--list", "feature/rollback-created-branch"]
        )
        .is_empty(),
        "a failed worktree attempt must not leak its newly created branch"
    );
}

#[tokio::test]
async fn failed_worktree_creation_preserves_a_preexisting_branch() {
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "hello\n", "initial");
    git(repo.path(), &["branch", "feature/preexisting-branch"]);
    let blocked_parent = repo.path().join("not-a-directory");
    fs::write(&blocked_parent, "blocked\n").expect("blocking path fixture");

    GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "main".into(),
                new_ref_name: Some("feature/preexisting-branch".into()),
                base_ref_name: Some("main".into()),
                path: Some(blocked_parent.join("worktree")),
            },
            &cancellation(),
        )
        .await
        .expect_err("duplicate branch cannot create a worktree");

    assert_eq!(
        git(
            repo.path(),
            &["branch", "--list", "feature/preexisting-branch"]
        ),
        "feature/preexisting-branch"
    );
}

#[tokio::test]
async fn commit_context_preserves_canonical_paths_for_staged_and_working_changes() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    fs::write(repo.path().join("tracked.txt"), "rust change\n").expect("working change");
    let repository = GitRepository::default();

    for staged in [false, true] {
        if staged {
            git(repo.path(), &["add", "tracked.txt"]);
        }
        for (mnemonic, no_prefix, source, destination) in [
            ("true", "false", "a/", "b/"),
            ("false", "true", "a/", "b/"),
            ("false", "false", "before/", "after/"),
        ] {
            git(repo.path(), &["config", "diff.mnemonicPrefix", mnemonic]);
            git(repo.path(), &["config", "diff.noprefix", no_prefix]);
            git(repo.path(), &["config", "diff.srcPrefix", source]);
            git(repo.path(), &["config", "diff.dstPrefix", destination]);
            let config_path = repo.path().join(".git/config");
            let config_before = fs::read(&config_path).expect("repository config");

            let context = repository
                .commit_context(repo.path(), &cancellation())
                .await
                .expect("commit context");

            assert!(
                context.contains("diff --git a/tracked.txt b/tracked.txt"),
                "staged={staged}, mnemonic={mnemonic}, no_prefix={no_prefix}: {context}"
            );
            assert!(context.contains("+rust change"));
            assert_eq!(
                fs::read(config_path).expect("config after commit context"),
                config_before,
                "commit context must not rewrite Git display settings"
            );
        }
    }
}

#[tokio::test]
async fn branch_commit_context_and_history_workflow_uses_real_git_state() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let repository = GitRepository::default();
    repository
        .create_ref(repo.path(), "feature/rust", true, &cancellation())
        .await
        .expect("create and switch feature ref");
    fs::write(repo.path().join("tracked.txt"), "rust change\n").expect("feature edit");
    let context = repository
        .commit_context(repo.path(), &cancellation())
        .await
        .expect("AI commit context");
    assert!(context.contains("rust change"));
    let sha = repository
        .commit(
            repo.path(),
            "feat: port git to rust",
            Some(&["tracked.txt".into()]),
            false,
            &cancellation(),
        )
        .await
        .expect("commit selected file")
        .expect("selected file produced a commit");
    assert_eq!(sha.len(), 40);
    repository
        .rename_ref(
            repo.path(),
            "feature/rust",
            "feature/native-git",
            &cancellation(),
        )
        .await
        .expect("rename current ref");

    let history = repository
        .list_commits(repo.path(), 1, 0, &cancellation())
        .await
        .expect("paginated history");
    assert_eq!(history.commits[0].subject, "feat: port git to rust");
    assert_eq!(history.next_cursor, Some(1));
    let refs = repository
        .list_refs(
            repo.path(),
            Some("native"),
            0,
            100,
            false,
            None,
            &cancellation(),
        )
        .await
        .expect("filtered refs");
    assert_eq!(refs.total_count, 1);
    assert!(refs.refs[0].current);
}

#[test]
fn structured_runner_child_fixture() {
    if std::env::var_os("CODDY_STUDIO_PROCESS_FIXTURE").is_none() {
        return;
    }
    if std::env::var_os("CODDY_STUDIO_PROCESS_FIXTURE_ERROR").is_some() {
        eprintln!("fatal: bad config line 3 in .gitmodules");
        panic!("requested fixture failure");
    }
    let bytes = std::env::var("CODDY_STUDIO_PROCESS_FIXTURE_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if bytes > 0 {
        print!("{}", "x".repeat(bytes));
    } else {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn fixture_request() -> ProcessRequest {
    ProcessRequest {
        operation: "git.test.fixture".into(),
        command: std::env::current_exe().expect("current test executable"),
        args: vec![
            "--exact".into(),
            "structured_runner_child_fixture".into(),
            "--nocapture".into(),
        ],
        cwd: std::env::current_dir().expect("current directory"),
        env: vec![("CODDY_STUDIO_PROCESS_FIXTURE".into(), "1".into())],
        stdin: None,
        timeout: Duration::from_secs(10),
        max_output_bytes: 1024,
        output_policy: OutputPolicy::Truncate,
        append_truncation_marker: true,
        allow_non_zero_exit: false,
    }
}

#[tokio::test]
async fn process_runner_is_bounded_and_cancellable() {
    let runner = ProcessRunner;
    let mut bounded = fixture_request();
    bounded
        .env
        .push(("CODDY_STUDIO_PROCESS_FIXTURE_BYTES".into(), "4096".into()));
    let output = runner
        .run(bounded, &cancellation())
        .await
        .expect("bounded process");
    assert!(output.stdout_truncated);
    assert!(output.stdout.len() <= 1024 + "\n\n[truncated]".len());

    let token = cancellation();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let error = runner
        .run(fixture_request(), &token)
        .await
        .expect_err("cancelled process must fail");
    assert!(error.is_cancelled());

    let mut failed = fixture_request();
    failed
        .env
        .push(("CODDY_STUDIO_PROCESS_FIXTURE_ERROR".into(), "1".into()));
    let error = runner
        .run(failed, &cancellation())
        .await
        .expect_err("non-zero process must retain bounded stderr");
    match error {
        ProcessError::NonZeroExit { stderr, .. } => {
            assert!(stderr.contains("bad config line 3 in .gitmodules"));
        }
        other => panic!("expected non-zero exit, got {other:?}"),
    }
}

#[tokio::test]
async fn status_subscription_starts_with_snapshot_deduplicates_and_stops_last_poller() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_secs(60),
        4,
    );

    let mut first = broadcaster
        .subscribe(repo.path().to_path_buf(), cancellation())
        .await
        .expect("first subscription");
    let mut second = broadcaster
        .subscribe(repo.path().to_path_buf(), cancellation())
        .await
        .expect("second subscription");
    let first_snapshot = first.recv().await;
    let second_snapshot = second.recv().await;
    assert_eq!(broadcaster.active_poller_count(), 1);
    observe_initial_remote(first_snapshot, &mut first).await;
    observe_initial_remote(second_snapshot, &mut second).await;

    broadcaster
        .refresh_local(repo.path(), &cancellation())
        .await
        .expect("unchanged refresh");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), first.recv())
            .await
            .is_err()
    );

    fs::write(repo.path().join("tracked.txt"), "changed\n").expect("working tree edit");
    broadcaster
        .refresh_local(repo.path(), &cancellation())
        .await
        .expect("changed refresh");
    assert!(matches!(
        first.recv().await,
        Some(VcsStatusStreamEvent::LocalUpdated { .. })
    ));
    assert!(matches!(
        second.recv().await,
        Some(VcsStatusStreamEvent::LocalUpdated { .. })
    ));

    drop(first);
    assert_eq!(broadcaster.active_poller_count(), 1);
    drop(second);
    assert_eq!(broadcaster.active_poller_count(), 0);
}

#[tokio::test]
async fn status_subscription_observes_external_working_tree_changes() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_millis(50),
        4,
    );
    let mut subscription = broadcaster
        .subscribe(repo.path().to_path_buf(), cancellation())
        .await
        .expect("status subscription");
    assert!(matches!(
        subscription.recv().await,
        Some(VcsStatusStreamEvent::Snapshot { .. })
    ));

    fs::write(repo.path().join("tracked.txt"), "changed externally\n")
        .expect("external working tree edit");

    let local = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(VcsStatusStreamEvent::LocalUpdated { local }) = subscription.recv().await {
                break local;
            }
        }
    })
    .await
    .expect("status subscription should observe an external working tree edit");
    assert!(local.has_working_tree_changes);
    assert!(
        local
            .working_tree
            .files
            .iter()
            .any(|file| file.path == "tracked.txt")
    );
}

#[tokio::test]
async fn status_subscription_watcher_observes_external_branch_changes_without_full_refresh() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_secs(3600),
        4,
    );
    let mut subscription = broadcaster
        .subscribe(repo.path().to_path_buf(), cancellation())
        .await
        .expect("status subscription");
    assert!(matches!(
        subscription.recv().await,
        Some(VcsStatusStreamEvent::Snapshot { .. })
    ));

    git(
        repo.path(),
        &["switch", "--quiet", "-c", "feature/external"],
    );

    let local = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(VcsStatusStreamEvent::LocalUpdated { local }) = subscription.recv().await
                && local.ref_name.as_deref() == Some("feature/external")
            {
                break local;
            }
        }
    })
    .await
    .expect("watcher should observe an external branch change");
    assert_eq!(local.ref_name.as_deref(), Some("feature/external"));
}

const SUBMODULE_WATCHER_TEST: &str =
    "status_subscription_watcher_observes_commits_and_checkouts_inside_a_submodule";

// Re-exec with an isolated global Git configuration, which the server's status
// reads inherit: host commit signing, hooks, or `diff.ignoreSubmodules` must not
// decide what this test observes.
#[test]
fn status_subscription_watcher_observes_commits_and_checkouts_inside_a_submodule() {
    if let Some(isolated) = reexec::enter(SUBMODULE_WATCHER_TEST, "isolated-git-config") {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Tokio runtime")
            .block_on(submodule_commits_and_checkouts_refresh_superproject_status());
        isolated.complete();
        return;
    }
    let config = IsolatedGitConfig::new();
    reexec::run(
        SUBMODULE_WATCHER_TEST,
        "isolated-git-config",
        None,
        |command| {
            config.apply_to_command(command);
        },
    );
}

async fn submodule_commits_and_checkouts_refresh_superproject_status() {
    let child = init_repo();
    commit_file(child.path(), "lib.txt", "base\n", "child initial");
    let parent = init_repo();
    commit_file(parent.path(), "README.md", "parent\n", "parent initial");
    git(
        parent.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &child.path().to_string_lossy(),
            "sub",
        ],
    );
    git(parent.path(), &["commit", "-m", "add submodule"]);
    let submodule = parent.path().join("sub");
    // The local safety read is an hour away, so every update below is watcher-driven.
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_secs(3600),
        4,
    );
    let mut subscription = broadcaster
        .subscribe(parent.path().to_path_buf(), cancellation())
        .await
        .expect("status subscription");
    match subscription.recv().await {
        Some(VcsStatusStreamEvent::Snapshot { local, .. }) => {
            assert!(!local.has_working_tree_changes, "{local:?}");
        }
        event => panic!("expected the initial snapshot, got {event:?}"),
    }

    // Both Git commands below write only inside the submodule's Git directory
    // (`.git/modules/sub`), never in the superproject's working tree.
    git(
        &submodule,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "submodule commit",
        ],
    );
    let moved = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(VcsStatusStreamEvent::LocalUpdated { local }) = subscription.recv().await
                && local
                    .working_tree
                    .files
                    .iter()
                    .any(|file| file.path == "sub")
            {
                break local;
            }
        }
    })
    .await
    .expect("a commit inside the submodule refreshes the superproject status");
    assert!(moved.working_tree.files.iter().any(|file| {
        file.path == "sub"
            && file.area == Some(VcsStagingArea::Unstaged)
            && file.status == Some(VcsWorkingTreeFileStatus::Modified)
    }));

    git(&submodule, &["checkout", "--quiet", "--detach", "HEAD~1"]);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(VcsStatusStreamEvent::LocalUpdated { local }) = subscription.recv().await
                && !local.has_working_tree_changes
            {
                break;
            }
        }
    })
    .await
    .expect("a checkout inside the submodule refreshes the superproject status");
}

#[tokio::test]
async fn lagging_status_subscriber_is_bounded_and_releases_its_lifecycle() {
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_secs(60),
        1,
    );
    let subscription = broadcaster
        .subscribe(repo.path().to_path_buf(), cancellation())
        .await
        .expect("bounded subscription");
    fs::write(repo.path().join("tracked.txt"), "changed\n").expect("working tree edit");
    broadcaster.notify_local_change(repo.path()).await;

    tokio::time::timeout(Duration::from_secs(30), async {
        while broadcaster.active_poller_count() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("lagging subscriber is evicted after the changed status publication");

    assert_eq!(broadcaster.active_poller_count(), 0);
    drop(subscription);
}

#[tokio::test]
async fn subscribed_remote_poller_fetches_real_upstream_changes() {
    let root = tempfile::tempdir().expect("remote fixture root");
    let remote = root.path().join("remote.git");
    fs::create_dir(&remote).expect("bare remote directory");
    git(&remote, &["init", "--bare", "--initial-branch=main"]);

    let publisher = root.path().join("publisher");
    fs::create_dir(&publisher).expect("publisher directory");
    git(&publisher, &["init", "-b", "main"]);
    git(&publisher, &["config", "user.name", "Coddy Studio Test"]);
    git(
        &publisher,
        &["config", "user.email", "coddy-studio@example.invalid"],
    );
    commit_file(&publisher, "tracked.txt", "base\n", "initial");
    git(
        &publisher,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&publisher, &["push", "-u", "origin", "main"]);

    let consumer = root.path().join("consumer");
    git(
        root.path(),
        &[
            "clone",
            &remote.to_string_lossy(),
            &consumer.to_string_lossy(),
        ],
    );
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_millis(100),
        4,
    );
    let mut subscription = broadcaster
        .subscribe(consumer.clone(), cancellation())
        .await
        .expect("remote subscription");
    let snapshot = subscription.recv().await;
    observe_initial_remote(snapshot, &mut subscription).await;

    fs::write(publisher.join("tracked.txt"), "remote change\n").expect("remote edit");
    git(&publisher, &["add", "tracked.txt"]);
    git(&publisher, &["commit", "-m", "remote change"]);
    git(&publisher, &["push"]);

    let behind = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote),
            }) = subscription.recv().await
                && remote.behind_count == 1
            {
                break remote;
            }
        }
    })
    .await
    .expect("poller should observe the upstream push");
    assert_eq!(behind.ahead_count, 0);
}

#[test]
fn source_control_parsers_match_cli_shapes() {
    let github = source_control::parse_github_auth_status(
        r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"GitHub.COM","login":"mauro"}]}}"#,
    );
    assert!(github.parsed);
    assert_eq!(github.accounts[0].host, "github.com");
    assert_eq!(github.accounts[0].account, "mauro");

    let gitlab = source_control::parse_gitlab_auth_status(
        "gitlab.com\n  x Logged in to gitlab.com as mauro (keyring)\n",
    );
    assert_eq!(gitlab[0].host, "gitlab.com");
    assert_eq!(gitlab[0].account.as_deref(), Some("mauro"));
    assert_eq!(
        source_control::provider_from_remote("git@github.com:owner/repo.git").kind,
        source_control::ProviderKind::Github
    );
}

#[test]
fn pull_request_json_parsers_match_provider_cli_shapes() {
    let github = source_control::parse_github_pull_request(
        r#"{"number":42,"title":"Ship Rust","url":"https://github.com/acme/repo/pull/42","baseRefName":"main","headRefName":"feature/rust","state":"OPEN"}"#,
    )
    .expect("GitHub pull request");
    assert_eq!(github.number, 42);
    assert_eq!(github.state, source_control::ChangeRequestState::Open);
    assert_eq!(github.head_branch, "feature/rust");

    let gitlab = source_control::parse_gitlab_merge_request(
        r#"{"iid":7,"title":"Ship Rust","web_url":"https://gitlab.com/acme/repo/-/merge_requests/7","target_branch":"main","source_branch":"feature/rust","state":"merged"}"#,
    )
    .expect("GitLab merge request");
    assert_eq!(gitlab.number, 7);
    assert_eq!(gitlab.state, source_control::ChangeRequestState::Merged);
}

#[tokio::test]
async fn source_control_discovery_uses_structured_bounded_probes() {
    let temp = TempDir::new().expect("discovery fixture");
    let discovery =
        source_control::SourceControlDiscovery::with_executable_dir_for_integration_test(
            hermetic_providers::missing_hosting_executable_dir(temp.path()),
        )
        .discover(
            std::env::current_dir().expect("current directory"),
            &cancellation(),
        )
        .await;
    let git = discovery
        .version_control_systems
        .iter()
        .find(|item| item.kind == source_control::VcsDiscoveryKind::Git)
        .expect("Git discovery item");
    assert_eq!(git.status, source_control::DiscoveryStatus::Available);
    assert!(git.implemented);

    let wire = serde_json::to_value(discovery).expect("serialize discovery result");
    assert_eq!(wire["versionControlSystems"][0]["version"]["_id"], "Option");
}

#[test]
fn wire_models_keep_effect_contract_field_names_and_tags() {
    let value = serde_json::to_value(VcsStatusStreamEvent::LocalUpdated {
        local: git::VcsStatusLocalResult::non_repository(),
    })
    .expect("serialize status event");
    assert_eq!(value["_tag"], "localUpdated");
    assert_eq!(value["local"]["isRepo"], false);
    assert_eq!(
        value["local"]["workingTree"]["files"],
        serde_json::json!([])
    );
}

#[test]
fn owned_vcs_facade_exposes_git_capabilities() {
    let facade = vcs::VcsService::default();
    assert_eq!(facade.capabilities().kind, vcs::VcsDriverKind::Git);
    assert!(facade.capabilities().supports_worktrees);
}
