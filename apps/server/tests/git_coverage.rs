use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use coddy_studio_server::git::{
    CreateWorktreeInput, GitRepository, GitWorktreeRecord, OutputPolicy, ProcessError,
    ProcessRequest, ProcessRunner, ProviderKind, PullStatus, StatusBroadcaster, VcsStagingArea,
    VcsStatusStreamEvent, VcsWorkingTreeFileStatus, git_worktree_prune_impact_digest,
    normalize_worktree_path_key, parse_numstat, parse_porcelain_v2_line, resolve_numstat_new_path,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[path = "support/isolated_git_config.rs"]
mod isolated_git_config;
use isolated_git_config::IsolatedGitConfig;

const PROCESS_FIXTURE_MODE: &str = "CODDY_STUDIO_GIT_COVERAGE_PROCESS_MODE";
const PROCESS_FIXTURE_ROLE: &str = "CODDY_STUDIO_GIT_COVERAGE_PROCESS_ROLE";
const PROCESS_FIXTURE_ROOT_READY: &str = "CODDY_STUDIO_GIT_COVERAGE_ROOT_READY";
const PROCESS_FIXTURE_CHILD_READY: &str = "CODDY_STUDIO_GIT_COVERAGE_CHILD_READY";
const PROCESS_FIXTURE_GRANDCHILD_READY: &str = "CODDY_STUDIO_GIT_COVERAGE_GRANDCHILD_READY";
const PROCESS_FIXTURE_ROOT_SURVIVED: &str = "CODDY_STUDIO_GIT_COVERAGE_ROOT_SURVIVED";
const PROCESS_FIXTURE_CHILD_SURVIVED: &str = "CODDY_STUDIO_GIT_COVERAGE_CHILD_SURVIVED";
const PROCESS_FIXTURE_GRANDCHILD_SURVIVED: &str = "CODDY_STUDIO_GIT_COVERAGE_GRANDCHILD_SURVIVED";
const PROCESS_FIXTURE_RELEASE: &str = "CODDY_STUDIO_GIT_COVERAGE_RELEASE";
const ISOLATED_GIT_TEST: &str = "CODDY_STUDIO_GIT_COVERAGE_ISOLATED";
static ISOLATED_GIT_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn process_fixture() {
    let Some(mode) = std::env::var_os(PROCESS_FIXTURE_MODE) else {
        return;
    };
    match mode.to_string_lossy().as_ref() {
        "stdin" => {
            let mut input = Vec::new();
            std::io::stdin()
                .read_to_end(&mut input)
                .expect("read fixture stdin");
            std::io::stdout()
                .write_all(b"fixture-stdin:")
                .expect("write fixture prefix");
            std::io::stdout()
                .write_all(&input)
                .expect("write fixture stdin echo");
        }
        "stdout" => {
            std::io::stdout()
                .write_all(&vec![b'x'; 4096])
                .expect("write fixture stdout");
        }
        "stderr" => {
            std::io::stderr()
                .write_all(&vec![b'y'; 4096])
                .expect("write fixture stderr");
        }
        "nonzero" => {
            println!("fixture nonzero stdout");
            eprintln!("fixture nonzero stderr");
            std::process::exit(7);
        }
        "park" => loop {
            std::thread::park();
        },
        "tree" => run_process_tree_fixture(),
        other => panic!("unknown process fixture mode: {other}"),
    }
}

fn run_process_tree_fixture() {
    let role = std::env::var(PROCESS_FIXTURE_ROLE).unwrap_or_else(|_| "root".to_string());
    let (ready_key, survived_key) = match role.as_str() {
        "root" => {
            spawn_process_tree_role("child");
            (PROCESS_FIXTURE_ROOT_READY, PROCESS_FIXTURE_ROOT_SURVIVED)
        }
        "child" => {
            spawn_process_tree_role("grandchild");
            (PROCESS_FIXTURE_CHILD_READY, PROCESS_FIXTURE_CHILD_SURVIVED)
        }
        "grandchild" => (
            PROCESS_FIXTURE_GRANDCHILD_READY,
            PROCESS_FIXTURE_GRANDCHILD_SURVIVED,
        ),
        other => panic!("unknown process fixture role: {other}"),
    };
    fs::write(
        std::env::var_os(ready_key).expect("tree fixture ready path"),
        "ready",
    )
    .expect("write tree fixture ready file");
    let release = PathBuf::from(
        std::env::var_os(PROCESS_FIXTURE_RELEASE).expect("tree fixture release path"),
    );
    while !release.is_file() {
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(
        std::env::var_os(survived_key).expect("tree fixture survived path"),
        "survived",
    )
    .expect("write tree fixture survived file");
    loop {
        std::thread::park();
    }
}

// The fixture deliberately transfers the child to the process-tree cleanup test. Waiting here
// would prevent the child and grandchild roles from reaching their ready state.
#[allow(clippy::zombie_processes)]
fn spawn_process_tree_role(role: &str) {
    Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "process_fixture",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROCESS_FIXTURE_MODE, "tree")
        .env(PROCESS_FIXTURE_ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn process tree fixture role");
}

fn cancellation() -> CancellationToken {
    CancellationToken::new()
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Coddy Studio Coverage")
        .env("GIT_AUTHOR_EMAIL", "coverage@example.invalid")
        .env("GIT_COMMITTER_NAME", "Coddy Studio Coverage")
        .env("GIT_COMMITTER_EMAIL", "coverage@example.invalid")
        .output()
        .expect("git must be installed for integration tests");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn init_repo() -> TempDir {
    let repo = tempfile::tempdir().expect("temporary repository");
    git(repo.path(), &["init", "-b", "main"]);
    git(
        repo.path(),
        &["config", "user.name", "Coddy Studio Coverage"],
    );
    git(
        repo.path(),
        &["config", "user.email", "coverage@example.invalid"],
    );
    repo
}

fn commit_file(repo: &Path, relative: &str, contents: &str, message: &str) {
    let path = repo.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, contents).expect("write fixture file");
    git(repo, &["add", "--", relative]);
    git(repo, &["commit", "-m", message]);
}

fn linked_worktree_record(
    inventory: &coddy_studio_server::git::GitWorktreeInventory,
    path: &Path,
) -> GitWorktreeRecord {
    let identity = test_worktree_identity_key(path);
    inventory
        .records
        .iter()
        .find(|record| test_worktree_identity_key(&record.path) == identity)
        .cloned()
        .expect("linked worktree record")
}

fn has_registered_worktree(cwd: &Path, expected: &Path) -> bool {
    let expected = test_worktree_identity_key(expected);
    git(cwd, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .any(|path| test_worktree_identity_key(&path) == expected)
}

fn test_worktree_identity_key(path: &Path) -> String {
    let mut ancestor = path;
    let mut missing = Vec::<OsString>::new();
    let canonical = loop {
        match fs::canonicalize(ancestor) {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                break canonical;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = ancestor
                    .file_name()
                    .unwrap_or_else(|| panic!("missing worktree path has no existing ancestor"));
                missing.push(component.to_os_string());
                ancestor = ancestor
                    .parent()
                    .unwrap_or_else(|| panic!("missing worktree path has no parent"));
            }
            Err(error) => panic!(
                "worktree identity path '{}' could not be canonicalized: {error}",
                path.display()
            ),
        }
    };
    normalize_worktree_path_key(&canonical, coddy_studio_server::git::host_path_platform())
}

fn local_file_url(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", normalized.trim_start_matches("//?/"))
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

#[test]
fn git_fixtures_ignore_hostile_user_global_config() {
    let fixture = tempfile::tempdir().expect("hostile global config fixture");
    let hooks = fixture.path().join("hooks");
    fs::create_dir(&hooks).expect("hostile hooks directory");
    fs::write(hooks.join("pre-commit"), "#!/bin/sh\nexit 71\n").expect("hostile pre-commit hook");
    let config = fixture.path().join("global.gitconfig");
    fs::write(
        &config,
        format!(
            "[commit]\n\tgpgSign = true\n[core]\n\thooksPath = {}\n[url \"file:///C:/coddy-studio-missing/\"]\n\tinsteadOf = file:///\n",
            hooks.to_string_lossy().replace('\\', "/")
        ),
    )
    .expect("hostile global config");

    let read_config = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("read hostile global config");
        assert!(output.status.success(), "git config {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    assert_eq!(
        read_config(&["config", "--global", "--get", "commit.gpgSign"]),
        "true"
    );
    assert_eq!(
        read_config(&["config", "--global", "--get", "core.hooksPath"]),
        hooks.to_string_lossy().replace('\\', "/")
    );
    let rewrites = read_config(&[
        "config",
        "--global",
        "--get-regexp",
        r"^url\..*\.insteadOf$",
    ]);
    assert!(rewrites.contains("file:///"));
    assert!(rewrites.contains("file:///C:/coddy-studio-missing/"));

    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args([
            "--exact",
            "local_clone_push_and_pull_cover_upstream_transitions",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove(ISOLATED_GIT_TEST)
        .output()
        .expect("run workflow under hostile global config");
    assert!(
        output.status.success(),
        "isolated Git workflow failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn process_request(mode: &str) -> ProcessRequest {
    ProcessRequest {
        operation: format!("git.coverage.{mode}"),
        command: std::env::current_exe().expect("current test executable"),
        args: vec![
            "--exact".into(),
            "process_fixture".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ],
        cwd: std::env::current_dir().expect("current directory"),
        env: vec![(PROCESS_FIXTURE_MODE.into(), mode.into())],
        stdin: None,
        timeout: Duration::from_secs(10),
        max_output_bytes: 8192,
        output_policy: OutputPolicy::Truncate,
        append_truncation_marker: false,
        allow_non_zero_exit: false,
    }
}

#[tokio::test]
async fn process_runner_writes_stdin_and_returns_both_streams() {
    let mut request = process_request("stdin");
    request.stdin = Some(b"native git input\n".to_vec());

    let output = ProcessRunner
        .run(request, &cancellation())
        .await
        .expect("stdin fixture succeeds");

    assert_eq!(output.exit_code, 0);
    assert!(output.stdout.contains("fixture-stdin:native git input\n"));
    assert!(!output.stdout_truncated);
    assert!(!output.stderr_truncated);
}

#[tokio::test]
async fn process_runner_reports_output_limits_for_each_stream() {
    for (mode, expected_stream) in [("stdout", "stdout"), ("stderr", "stderr")] {
        let mut request = process_request(mode);
        request.max_output_bytes = 1024;
        request.output_policy = OutputPolicy::Error;

        let error = ProcessRunner
            .run(request, &cancellation())
            .await
            .expect_err("oversized output must fail");

        match error {
            ProcessError::OutputLimit {
                stream,
                max_bytes,
                observed_bytes,
                ..
            } => {
                assert_eq!(stream, expected_stream);
                assert_eq!(max_bytes, 1024);
                assert!(observed_bytes >= 4096);
            }
            other => panic!("expected {expected_stream} output limit, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn process_runner_truncates_without_a_marker_when_disabled() {
    let mut request = process_request("stdout");
    request.max_output_bytes = 256;

    let output = ProcessRunner
        .run(request, &cancellation())
        .await
        .expect("truncated process succeeds");

    assert!(output.stdout_truncated);
    assert_eq!(output.stdout.len(), 256);
    assert!(!output.stdout.ends_with("[truncated]"));
}

#[tokio::test]
async fn process_runner_timeout_and_precancellation_are_distinct() {
    let mut timeout_request = process_request("park");
    timeout_request.timeout = Duration::ZERO;
    let timeout = ProcessRunner
        .run(timeout_request, &cancellation())
        .await
        .expect_err("parked process times out");
    assert!(matches!(
        timeout,
        ProcessError::Timeout { timeout_ms: 0, .. }
    ));
    assert!(!timeout.is_cancelled());

    let token = cancellation();
    token.cancel();
    let cancelled = ProcessRunner
        .run(process_request("park"), &token)
        .await
        .expect_err("pre-cancelled process is interrupted");
    assert!(cancelled.is_cancelled());
}

#[tokio::test]
async fn process_runner_cancellation_kills_child_and_grandchild() {
    let temp = tempfile::tempdir().expect("temporary process-tree directory");
    let root_ready = temp.path().join("root.ready");
    let child_ready = temp.path().join("child.ready");
    let grandchild_ready = temp.path().join("grandchild.ready");
    let root_survived = temp.path().join("root.survived");
    let child_survived = temp.path().join("child.survived");
    let grandchild_survived = temp.path().join("grandchild.survived");
    let release = temp.path().join("release");

    let mut request = process_request("tree");
    request.env.extend([
        (PROCESS_FIXTURE_ROLE.into(), "root".into()),
        (
            PROCESS_FIXTURE_ROOT_READY.into(),
            root_ready.as_os_str().to_owned(),
        ),
        (
            PROCESS_FIXTURE_CHILD_READY.into(),
            child_ready.as_os_str().to_owned(),
        ),
        (
            PROCESS_FIXTURE_GRANDCHILD_READY.into(),
            grandchild_ready.as_os_str().to_owned(),
        ),
        (
            PROCESS_FIXTURE_ROOT_SURVIVED.into(),
            root_survived.as_os_str().to_owned(),
        ),
        (
            PROCESS_FIXTURE_CHILD_SURVIVED.into(),
            child_survived.as_os_str().to_owned(),
        ),
        (
            PROCESS_FIXTURE_GRANDCHILD_SURVIVED.into(),
            grandchild_survived.as_os_str().to_owned(),
        ),
        (
            PROCESS_FIXTURE_RELEASE.into(),
            release.as_os_str().to_owned(),
        ),
    ]);
    let token = cancellation();
    let task_token = token.clone();
    let task = tokio::spawn(async move { ProcessRunner.run(request, &task_token).await });

    wait_for_path(&root_ready).await;
    wait_for_path(&child_ready).await;
    wait_for_path(&grandchild_ready).await;
    token.cancel();

    let error = task
        .await
        .expect("join process-tree cancellation")
        .expect_err("process tree should be cancelled");
    assert!(error.is_cancelled());
    fs::write(&release, "release").expect("write process-tree release");
    tokio::time::sleep(Duration::from_secs(1)).await;
    for sentinel in [&root_survived, &child_survived, &grandchild_survived] {
        assert!(
            !sentinel.exists(),
            "cancelled process survived long enough to write {}",
            sentinel.display()
        );
    }
}

#[tokio::test]
async fn process_runner_preserves_nonzero_output_and_can_allow_the_exit() {
    let error = ProcessRunner
        .run(process_request("nonzero"), &cancellation())
        .await
        .expect_err("nonzero fixture fails by default");
    match error {
        ProcessError::NonZeroExit {
            exit_code,
            stdout_length,
            stderr_length,
            stdout,
            stderr,
            ..
        } => {
            assert_eq!(exit_code, 7);
            assert!(stdout_length >= "fixture nonzero stdout".len());
            assert!(stderr_length >= "fixture nonzero stderr".len());
            assert!(stdout.contains("fixture nonzero stdout"));
            assert!(stderr.contains("fixture nonzero stderr"));
        }
        other => panic!("expected nonzero exit, got {other:?}"),
    }

    let mut allowed = process_request("nonzero");
    allowed.allow_non_zero_exit = true;
    let output = ProcessRunner
        .run(allowed, &cancellation())
        .await
        .expect("explicitly allowed nonzero exit succeeds");
    assert_eq!(output.exit_code, 7);
    assert!(output.stderr.contains("fixture nonzero stderr"));
}

#[tokio::test]
async fn process_runner_spawn_error_identifies_the_operation_and_command() {
    let temp = tempfile::tempdir().expect("temporary process directory");
    let missing = temp.path().join("missing-process.exe");
    let mut request = process_request("stdin");
    request.operation = "git.coverage.spawn".into();
    request.command = missing.clone();
    request.cwd = temp.path().to_path_buf();

    let error = ProcessRunner
        .run(request, &cancellation())
        .await
        .expect_err("missing executable does not spawn");

    match error {
        ProcessError::Spawn {
            operation,
            command,
            source,
        } => {
            assert_eq!(operation, "git.coverage.spawn");
            assert_eq!(PathBuf::from(command), missing);
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected spawn error, got {other:?}"),
    }
}

#[test]
fn porcelain_parser_rejects_malformed_records_and_accepts_conflicts() {
    assert_eq!(parse_porcelain_v2_line(""), None);
    assert_eq!(parse_porcelain_v2_line("x unsupported"), None);
    assert_eq!(
        parse_porcelain_v2_line("1 .. N... 100644 100644 100644 abc def"),
        None
    );

    let deleted =
        parse_porcelain_v2_line("1 D. N... 100644 000000 000000 abcdef0 0000000 deleted.txt")
            .expect("deleted record");
    assert_eq!(deleted.index_status, VcsWorkingTreeFileStatus::Deleted);
    assert!(deleted.index_changed);
    assert!(!deleted.worktree_changed);

    let conflict = parse_porcelain_v2_line(
        "u UU N... 100644 100644 100644 100644 aaaaaaa bbbbbbb ccccccc conflicted.txt",
    )
    .expect("unmerged record");
    assert_eq!(conflict.path, "conflicted.txt");
    assert!(conflict.index_changed);
    assert!(conflict.worktree_changed);
}

#[test]
fn numstat_parser_handles_binary_malformed_and_rename_paths() {
    assert_eq!(
        resolve_numstat_new_path("src/{old => new}/file.rs"),
        "src/new/file.rs"
    );
    assert_eq!(
        resolve_numstat_new_path("src/{unchanged}/file.rs"),
        "src/{unchanged}/file.rs"
    );

    let stats = parse_numstat(concat!(
        "-\t-\tassets/image.bin\n12\t",
        "3\tsrc/{old => new}.rs\ninvalid\n4\t2\t\n"
    ));
    assert_eq!(stats["assets/image.bin"], (0, 0));
    assert_eq!(stats["src/new.rs"], (12, 3));
    assert_eq!(stats.len(), 2);
}

#[tokio::test]
async fn non_repository_operations_return_defaults_and_reject_unsafe_pathspecs() {
    if relaunch_with_isolated_git_config(
        "non_repository_operations_return_defaults_and_reject_unsafe_pathspecs",
    ) {
        return;
    }
    let directory = tempfile::tempdir().expect("non-repository directory");
    let repository = GitRepository::default();
    let token = cancellation();

    assert!(
        !repository
            .is_repository(directory.path(), &token)
            .await
            .expect("repository probe")
    );
    assert_eq!(
        repository
            .repository_root(directory.path(), &token)
            .await
            .expect("repository root"),
        None
    );
    let local = repository
        .local_status(directory.path(), &token)
        .await
        .expect("non-repository local status");
    assert!(!local.is_repo);
    assert!(local.working_tree.files.is_empty());
    assert_eq!(
        repository
            .remote_status(directory.path(), &token)
            .await
            .expect("non-repository remote status"),
        None
    );
    assert_eq!(
        repository
            .refresh_remote_status(directory.path(), &token)
            .await
            .expect("non-repository refresh"),
        None
    );

    let status = repository
        .status(directory.path(), &token)
        .await
        .expect("combined non-repository status");
    assert!(!status.local.is_repo);
    assert!(!status.remote.has_upstream);
    assert_eq!(status.remote.ahead_of_default_count, Some(0));

    let refs = repository
        .list_refs(directory.path(), None, 0, 20, true, None, &token)
        .await
        .expect("non-repository refs");
    assert!(!refs.is_repo);
    assert!(refs.refs.is_empty());
    let commits = repository
        .list_commits(directory.path(), 20, 0, &token)
        .await
        .expect("non-repository history");
    assert!(commits.commits.is_empty());

    repository
        .stage_files(directory.path(), &[], &token)
        .await
        .expect("empty stage is a no-op");
    repository
        .unstage_files(directory.path(), &[], &token)
        .await
        .expect("empty unstage is a no-op");

    for path in [
        " ".to_owned(),
        "../escape.txt".to_owned(),
        directory
            .path()
            .join("absolute.txt")
            .to_string_lossy()
            .into_owned(),
    ] {
        let error = repository
            .stage_files(directory.path(), &[path], &token)
            .await
            .expect_err("unsafe pathspec is rejected");
        assert_eq!(error.operation.as_ref(), "GitVcsDriver.stageFiles");
        assert!(error.diagnostics.is_none());
        assert!(error.detail.contains("repository-relative"));
        assert!(error.to_string().contains("Git command failed"));
    }
}

#[tokio::test]
async fn status_reports_provider_staging_areas_and_default_branch_delta() {
    if relaunch_with_isolated_git_config(
        "status_reports_provider_staging_areas_and_default_branch_delta",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    git(repo.path(), &["switch", "-c", "feature/status"]);
    commit_file(repo.path(), "feature.txt", "feature\n", "feature commit");
    git(
        repo.path(),
        &["remote", "add", "origin", "git@github.com:acme/project.git"],
    );
    git(
        repo.path(),
        &["config", "branch.feature/status.remote", "origin"],
    );
    git(
        repo.path(),
        &[
            "config",
            "branch.feature/status.merge",
            "refs/heads/feature/status",
        ],
    );

    fs::write(
        repo.path().join("tracked.txt"),
        "changed\nwith another line\n",
    )
    .expect("unstaged edit");
    fs::write(repo.path().join("staged.txt"), "staged\n").expect("staged file");
    git(repo.path(), &["add", "staged.txt"]);
    fs::write(repo.path().join("untracked.txt"), "one\ntwo")
        .expect("untracked file without final newline");

    let status = GitRepository::default()
        .status(repo.path(), &cancellation())
        .await
        .expect("combined repository status");

    assert!(status.local.is_repo);
    assert_eq!(status.local.ref_name.as_deref(), Some("feature/status"));
    assert_eq!(status.local.default_ref_name.as_deref(), Some("main"));
    assert!(!status.local.is_default_ref);
    assert!(status.local.has_primary_remote);
    let provider = status
        .local
        .source_control_provider
        .expect("recognized origin provider");
    assert_eq!(provider.kind, ProviderKind::Github);
    assert_eq!(provider.base_url, "https://github.com");
    assert!(!status.remote.has_upstream);
    assert_eq!(status.remote.ahead_of_default_count, Some(1));

    let files = &status.local.working_tree.files;
    assert!(files.iter().any(|file| {
        file.path == "staged.txt"
            && file.area == Some(VcsStagingArea::Staged)
            && file.status == Some(VcsWorkingTreeFileStatus::Added)
    }));
    assert!(files.iter().any(|file| {
        file.path == "tracked.txt"
            && file.area == Some(VcsStagingArea::Unstaged)
            && file.insertions == 2
            && file.deletions == 1
    }));
    assert!(files.iter().any(|file| {
        file.path == "untracked.txt"
            && file.area == Some(VcsStagingArea::Untracked)
            && file.insertions == 2
    }));
}

#[tokio::test]
async fn provider_detection_covers_supported_origin_url_shapes() {
    if relaunch_with_isolated_git_config("provider_detection_covers_supported_origin_url_shapes") {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let cases = [
        (
            "ssh://git@gitlab.example/team/repo.git",
            ProviderKind::Gitlab,
        ),
        (
            "https://dev.azure.com/acme/project/_git/repo",
            ProviderKind::AzureDevops,
        ),
        ("git@bitbucket.org:acme/repo.git", ProviderKind::Bitbucket),
    ];

    for (index, (url, expected)) in cases.into_iter().enumerate() {
        if index > 0 {
            git(repo.path(), &["remote", "remove", "origin"]);
        }
        git(repo.path(), &["remote", "add", "origin", url]);
        let local = repository
            .local_status(repo.path(), &cancellation())
            .await
            .expect("provider status");
        assert_eq!(
            local.source_control_provider.map(|provider| provider.kind),
            Some(expected)
        );
    }

    git(repo.path(), &["remote", "remove", "origin"]);
    git(
        repo.path(),
        &["remote", "add", "origin", "ssh://example.invalid/acme/repo"],
    );
    assert!(
        repository
            .local_status(repo.path(), &cancellation())
            .await
            .expect("unknown provider status")
            .source_control_provider
            .is_none()
    );
}

#[tokio::test]
async fn history_paginates_real_commits_and_preserves_metadata() {
    if relaunch_with_isolated_git_config("history_paginates_real_commits_and_preserves_metadata") {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "one.txt", "one\n", "first");
    commit_file(repo.path(), "two.txt", "two\n", "second");
    commit_file(repo.path(), "three.txt", "three\n", "third");
    let repository = GitRepository::default();

    let first_page = repository
        .list_commits(repo.path(), 2, 0, &cancellation())
        .await
        .expect("first history page");
    assert_eq!(
        first_page
            .commits
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        ["third", "second"]
    );
    assert_eq!(first_page.next_cursor, Some(2));
    assert!(first_page.commits.iter().all(|commit| {
        commit.sha.len() == 40
            && !commit.short_sha.is_empty()
            && commit.author_name == "Coddy Studio Coverage"
            && commit.authored_at_ms > 0
    }));

    let second_page = repository
        .list_commits(repo.path(), 2, 2, &cancellation())
        .await
        .expect("second history page");
    assert_eq!(second_page.commits.len(), 1);
    assert_eq!(second_page.commits[0].subject, "first");
    assert_eq!(second_page.next_cursor, None);
}

#[tokio::test]
async fn local_clone_push_and_pull_cover_upstream_transitions() {
    if relaunch_with_isolated_git_config("local_clone_push_and_pull_cover_upstream_transitions") {
        return;
    }
    let root = tempfile::tempdir().expect("local remote fixture");
    let remote = root.path().join("remote.git");
    fs::create_dir(&remote).expect("bare remote directory");
    git(&remote, &["init", "--bare", "--initial-branch=main"]);

    let publisher = root.path().join("publisher");
    fs::create_dir(&publisher).expect("publisher directory");
    git(&publisher, &["init", "-b", "main"]);
    git(
        &publisher,
        &["config", "user.name", "Coddy Studio Coverage"],
    );
    git(
        &publisher,
        &["config", "user.email", "coverage@example.invalid"],
    );
    commit_file(&publisher, "tracked.txt", "base\n", "initial");
    let remote_url = local_file_url(&remote);
    git(&publisher, &["remote", "add", "origin", &remote_url]);
    git(&publisher, &["push", "-u", "origin", "main"]);

    let clones = root.path().join("clones");
    fs::create_dir(&clones).expect("clone parent");
    let repository = GitRepository::default();
    let consumer = repository
        .clone_repository(&remote_url, &clones, Some("consumer"), &cancellation())
        .await
        .expect("local clone");
    git(&consumer, &["config", "user.name", "Coddy Studio Coverage"]);
    git(
        &consumer,
        &["config", "user.email", "coverage@example.invalid"],
    );

    let unchanged = repository
        .pull_current_branch(&consumer, &cancellation())
        .await
        .expect("up-to-date pull");
    assert_eq!(unchanged.status, PullStatus::SkippedUpToDate);
    assert_eq!(unchanged.upstream_ref.as_deref(), Some("origin/main"));

    commit_file(
        &publisher,
        "tracked.txt",
        "remote change\n",
        "remote update",
    );
    git(&publisher, &["push"]);
    let pulled = repository
        .pull_current_branch(&consumer, &cancellation())
        .await
        .expect("fast-forward pull");
    assert_eq!(pulled.status, PullStatus::Pulled);
    assert_eq!(
        fs::read_to_string(consumer.join("tracked.txt"))
            .expect("pulled contents")
            .replace("\r\n", "\n"),
        "remote change\n"
    );

    repository
        .create_ref(&consumer, "feature/local-push", true, &cancellation())
        .await
        .expect("create feature branch");
    fs::write(consumer.join("feature.txt"), "feature\n").expect("feature file");
    repository
        .commit(&consumer, "feature commit", None, false, &cancellation())
        .await
        .expect("feature commit")
        .expect("commit sha");
    assert_eq!(
        repository
            .push_current_branch(&consumer, &cancellation())
            .await
            .expect("set upstream push"),
        "feature/local-push"
    );
    assert_eq!(
        git(&consumer, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/feature/local-push"
    );

    let remote_refs = repository
        .list_refs(
            &consumer,
            Some("feature"),
            0,
            200,
            true,
            Some("remote"),
            &cancellation(),
        )
        .await
        .expect("remote refs");
    assert!(
        remote_refs
            .refs
            .iter()
            .any(|reference| reference.name == "origin/feature/local-push")
    );

    let derived_parent = root.path().join("derived");
    fs::create_dir(&derived_parent).expect("derived clone parent");
    let derived = repository
        .clone_repository(&remote_url, &derived_parent, None, &cancellation())
        .await
        .expect("derived-name clone");
    assert_eq!(
        derived.file_name().and_then(|name| name.to_str()),
        Some("remote")
    );
}

#[tokio::test]
async fn existing_branch_worktree_can_be_force_removed_when_dirty() {
    if relaunch_with_isolated_git_config("existing_branch_worktree_can_be_force_removed_when_dirty")
    {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    repository
        .create_ref(repo.path(), "feature/existing", false, &cancellation())
        .await
        .expect("create branch without switching");
    assert_eq!(
        repository
            .rename_ref(
                repo.path(),
                "feature/existing",
                "feature/existing",
                &cancellation(),
            )
            .await
            .expect("same-name rename"),
        "feature/existing"
    );

    let worktree_root = tempfile::tempdir().expect("worktree parent");
    let worktree_path = worktree_root.path().join("existing");
    let created = repository
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "feature/existing".into(),
                new_ref_name: None,
                base_ref_name: None,
                path: Some(worktree_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("worktree from existing branch");
    assert_eq!(created.worktree.ref_name, "feature/existing");
    fs::write(worktree_path.join("dirty.txt"), "dirty\n").expect("dirty worktree file");

    repository
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect("force-remove dirty worktree");
    assert!(!worktree_path.exists());
    repository
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect("worktree removal is retryable after a later thread-delete failure");
    assert_eq!(
        repository
            .switch_ref(repo.path(), "feature/existing", &cancellation())
            .await
            .expect("switch to existing branch")
            .as_deref(),
        Some("feature/existing")
    );
}

#[tokio::test]
async fn create_worktree_reuses_a_local_branch_that_appeared_after_remote_selection() {
    if relaunch_with_isolated_git_config(
        "create_worktree_reuses_a_local_branch_that_appeared_after_remote_selection",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let head = git(repo.path(), &["rev-parse", "HEAD"]);
    git(
        repo.path(),
        &[
            "update-ref",
            "refs/remotes/origin/feature/remote-only",
            &head,
        ],
    );

    // The dialog selected the remote-only ref, then another Git client created
    // the matching local branch before the RPC reached the server.
    git(
        repo.path(),
        &[
            "branch",
            "feature/remote-only",
            "origin/feature/remote-only",
        ],
    );

    let worktree_root = tempfile::tempdir().expect("worktree parent");
    let worktree_path = worktree_root.path().join("remote-only");
    let created = GitRepository::default()
        .create_worktree(
            CreateWorktreeInput {
                cwd: repo.path().to_path_buf(),
                ref_name: "origin/feature/remote-only".into(),
                new_ref_name: Some("feature/remote-only".into()),
                base_ref_name: Some("origin/feature/remote-only".into()),
                path: Some(worktree_path.clone()),
            },
            &cancellation(),
        )
        .await
        .expect("reuse the newly visible local branch");

    assert_eq!(created.worktree.ref_name, "feature/remote-only");
    assert_eq!(
        git(&worktree_path, &["branch", "--show-current"]),
        "feature/remote-only"
    );
    assert_eq!(
        git(repo.path(), &["worktree", "list", "--porcelain"])
            .matches("branch refs/heads/feature/remote-only")
            .count(),
        1
    );
}

#[tokio::test]
async fn worktree_inventory_returns_authoritative_primary_and_linked_records() {
    if relaunch_with_isolated_git_config(
        "worktree_inventory_returns_authoritative_primary_and_linked_records",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("linked checkout");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/inventory",
            "--",
            &linked.to_string_lossy(),
        ],
    );

    let inventory = GitRepository::default()
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("worktree inventory");

    assert!(inventory.nul_delimited);
    assert_eq!(inventory.common_dir, repo.path().join(".git"));
    assert_eq!(inventory.records.len(), 2);
    let git_paths = git(repo.path(), &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    assert_eq!(git_paths.len(), 2);
    assert!(inventory.records[0].is_primary);
    assert_eq!(inventory.records[0].path, git_paths[0]);
    assert!(!inventory.records[1].is_primary);
    assert_eq!(inventory.records[1].path, git_paths[1]);
    assert_eq!(
        inventory.records[1].branch.as_deref(),
        Some("feature/inventory")
    );
}

#[tokio::test]
async fn worktree_removal_inspection_counts_tracked_and_untracked_paths_separately() {
    if relaunch_with_isolated_git_config(
        "worktree_removal_inspection_counts_tracked_and_untracked_paths_separately",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("counted");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/counts",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("worktree inventory");
    let record = linked_worktree_record(&inventory, &linked);

    let clean = repository
        .inspect_worktree_removal(repo.path(), &record, &cancellation())
        .await
        .expect("clean removal inspection");
    assert_eq!(clean.tracked_change_count, 0);
    assert_eq!(clean.untracked_file_count, 0);

    fs::write(linked.join("tracked.txt"), "changed\n").expect("modify tracked file");
    let tracked = repository
        .inspect_worktree_removal(repo.path(), &record, &cancellation())
        .await
        .expect("tracked-dirty removal inspection");
    assert_eq!(tracked.tracked_change_count, 1);
    assert_eq!(tracked.untracked_file_count, 0);

    fs::write(linked.join("untracked.txt"), "new\n").expect("write untracked file");
    let both = repository
        .inspect_worktree_removal(repo.path(), &record, &cancellation())
        .await
        .expect("combined removal inspection");
    assert_eq!(both.tracked_change_count, 1);
    assert_eq!(both.untracked_file_count, 1);

    git(&linked, &["checkout", "--", "tracked.txt"]);
    let untracked = repository
        .inspect_worktree_removal(repo.path(), &record, &cancellation())
        .await
        .expect("untracked-dirty removal inspection");
    assert_eq!(untracked.tracked_change_count, 0);
    assert_eq!(untracked.untracked_file_count, 1);
}

#[tokio::test]
async fn worktree_removal_rejects_primary_and_bare_records() {
    if relaunch_with_isolated_git_config("worktree_removal_rejects_primary_and_bare_records") {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let primary = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("primary inventory")
        .records
        .into_iter()
        .next()
        .expect("primary record");
    let primary_error = repository
        .inspect_worktree_removal(repo.path(), &primary, &cancellation())
        .await
        .expect_err("primary worktree is protected");
    assert!(
        primary_error
            .detail
            .to_ascii_lowercase()
            .contains("primary")
    );

    let bare_parent = tempfile::tempdir().expect("bare repository parent");
    let bare = bare_parent.path().join("bare.git");
    git(
        bare_parent.path(),
        &["init", "--bare", &bare.to_string_lossy()],
    );
    let bare_record = repository
        .worktree_inventory(&bare, &cancellation())
        .await
        .expect("bare inventory")
        .records
        .into_iter()
        .next()
        .expect("bare record");
    let bare_error = repository
        .inspect_worktree_removal(&bare, &bare_record, &cancellation())
        .await
        .expect_err("bare worktree is protected");
    assert!(bare_error.detail.to_ascii_lowercase().contains("bare"));
}

#[tokio::test]
async fn worktree_removal_rejects_locked_record_with_its_reason() {
    if relaunch_with_isolated_git_config("worktree_removal_rejects_locked_record_with_its_reason") {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("locked");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/locked-removal",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    git(
        repo.path(),
        &[
            "worktree",
            "lock",
            "--reason",
            "keep for audit",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("locked inventory");
    let record = linked_worktree_record(&inventory, &linked);

    let error = repository
        .remove_worktree_verified(repo.path(), &record, true, &cancellation())
        .await
        .expect_err("locked worktree removal fails closed");
    assert!(error.detail.contains("keep for audit"));
    assert!(has_registered_worktree(repo.path(), &linked));
}

#[tokio::test]
async fn worktree_removal_rejects_repository_and_record_identity_drift() {
    if relaunch_with_isolated_git_config(
        "worktree_removal_rejects_repository_and_record_identity_drift",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let other = init_repo();
    commit_file(other.path(), "README.md", "other\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("identity");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/identity",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("worktree inventory");
    let record = linked_worktree_record(&inventory, &linked);

    let repository_error = repository
        .inspect_worktree_removal(other.path(), &record, &cancellation())
        .await
        .expect_err("record from another repository is rejected");
    assert!(
        repository_error
            .detail
            .to_ascii_lowercase()
            .contains("repository")
    );

    let mut changed = record.clone();
    changed.path = repo.path().join("changed-identity");
    let identity_error = repository
        .inspect_worktree_removal(repo.path(), &changed, &cancellation())
        .await
        .expect_err("changed record path is rejected");
    assert!(
        identity_error
            .detail
            .to_ascii_lowercase()
            .contains("record")
    );
}

#[tokio::test]
async fn worktree_removal_rejects_an_unregistered_replacement_directory() {
    if relaunch_with_isolated_git_config(
        "worktree_removal_rejects_an_unregistered_replacement_directory",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("replacement");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/replacement",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("worktree inventory");
    let record = linked_worktree_record(&inventory, &linked);
    git(
        repo.path(),
        &[
            "worktree",
            "remove",
            "--force",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    fs::create_dir(&linked).expect("unrelated replacement directory");
    fs::write(linked.join("keep.txt"), "unrelated\n").expect("replacement contents");

    repository
        .remove_worktree_verified(repo.path(), &record, true, &cancellation())
        .await
        .expect_err("unregistered replacement is never removed");
    assert_eq!(
        fs::read_to_string(linked.join("keep.txt")).expect("replacement survives"),
        "unrelated\n"
    );
}

#[tokio::test]
async fn worktree_removal_rejects_a_registered_path_replaced_by_another_repository() {
    if relaunch_with_isolated_git_config(
        "worktree_removal_rejects_a_registered_path_replaced_by_another_repository",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("replacement-repository");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/repository-replacement",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("worktree inventory");
    let record = linked_worktree_record(&inventory, &linked);
    fs::rename(&linked, parent.path().join("moved-original"))
        .expect("move original registered checkout");
    fs::create_dir(&linked).expect("create replacement repository root");
    git(&linked, &["init", "-b", "unrelated"]);
    commit_file(&linked, "KEEP", "unrelated\n", "unrelated repository");

    let error = repository
        .inspect_worktree_removal(repo.path(), &record, &cancellation())
        .await
        .expect_err("replacement repository cannot satisfy registered identity");
    assert!(error.detail.to_ascii_lowercase().contains("repository"));
    assert_eq!(
        fs::read_to_string(linked.join("KEEP")).expect("replacement survives"),
        "unrelated\n"
    );
    assert!(has_registered_worktree(repo.path(), &record.path));
}

#[tokio::test]
async fn worktree_removal_requires_dirty_confirmation_and_preserves_the_branch() {
    if relaunch_with_isolated_git_config(
        "worktree_removal_requires_dirty_confirmation_and_preserves_the_branch",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("dirty");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/preserved",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    fs::write(linked.join("tracked.txt"), "dirty\n").expect("dirty tracked file");
    let repository = GitRepository::default();
    let inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("worktree inventory");
    let record = linked_worktree_record(&inventory, &linked);

    let dirty_error = repository
        .remove_worktree_verified(repo.path(), &record, false, &cancellation())
        .await
        .expect_err("dirty worktree requires confirmation");
    assert!(dirty_error.detail.to_ascii_lowercase().contains("dirty"));
    assert!(has_registered_worktree(repo.path(), &linked));

    repository
        .remove_worktree_verified(repo.path(), &record, true, &cancellation())
        .await
        .expect("confirmed dirty removal");
    assert!(!linked.exists());
    assert_eq!(
        git(repo.path(), &["branch", "--list", "feature/preserved"]),
        "feature/preserved"
    );
}

#[tokio::test]
async fn worktree_prune_preview_lists_every_affected_registration_in_normalized_order() {
    if relaunch_with_isolated_git_config(
        "worktree_prune_preview_lists_every_affected_registration_in_normalized_order",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let second = parent.path().join("z-second");
    let first = parent.path().join("a first");
    for (path, branch) in [(&second, "feature/second"), (&first, "feature/first")] {
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                branch,
                "--",
                &path.to_string_lossy(),
            ],
        );
    }
    let repository = GitRepository::default();
    let registered = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("registered inventory");
    let first_record = linked_worktree_record(&registered, &first);
    let second_record = linked_worktree_record(&registered, &second);
    fs::rename(&first, parent.path().join("moved-first")).expect("make first registration stale");
    fs::rename(&second, parent.path().join("moved-second"))
        .expect("make second registration stale");

    let impact = repository
        .preview_worktree_prune(repo.path(), &cancellation())
        .await
        .expect("prune preview");
    assert_eq!(impact.len(), 2);
    assert!(impact.iter().all(|entry| !entry.prune_reason.is_empty()));
    assert!(impact.iter().all(|entry| !entry.locked));
    assert!(impact.iter().all(|entry| entry.lock_reason.is_none()));
    let keys = impact
        .iter()
        .map(|entry| {
            normalize_worktree_path_key(&entry.path, coddy_studio_server::git::host_path_platform())
        })
        .collect::<Vec<_>>();
    let mut expected = vec![first_record.path, second_record.path]
        .into_iter()
        .map(|path| {
            normalize_worktree_path_key(&path, coddy_studio_server::git::host_path_platform())
        })
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(keys, expected);

    let mut reversed = impact.clone();
    reversed.reverse();
    assert_eq!(
        git_worktree_prune_impact_digest(&impact),
        git_worktree_prune_impact_digest(&reversed),
        "confirmed impact digest is independent of preview iteration order"
    );
    let mut reason_changed = impact.clone();
    reason_changed[0].prune_reason.push_str("-changed");
    assert_ne!(
        git_worktree_prune_impact_digest(&impact),
        git_worktree_prune_impact_digest(&reason_changed),
        "confirmed impact digest binds each path to its exact prune reason"
    );
}

#[tokio::test]
async fn worktree_prune_rejects_impact_drift_before_mutation_then_accepts_the_fresh_digest() {
    if relaunch_with_isolated_git_config(
        "worktree_prune_rejects_impact_drift_before_mutation_then_accepts_the_fresh_digest",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let first = parent.path().join("first");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/prune-first",
            "--",
            &first.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let registered = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("registered inventory");
    let registered_first = linked_worktree_record(&registered, &first);
    fs::rename(&first, parent.path().join("moved-first")).expect("make first registration stale");
    let stale_inventory = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("stale inventory");
    let target = linked_worktree_record(&stale_inventory, &registered_first.path);
    let confirmed = repository
        .preview_worktree_prune(repo.path(), &cancellation())
        .await
        .expect("initial prune preview");
    let confirmed_digest = git_worktree_prune_impact_digest(&confirmed);

    let second = parent.path().join("second");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/prune-second",
            "--",
            &second.to_string_lossy(),
        ],
    );
    let registered_second = fs::canonicalize(&second).expect("canonical second registration");
    fs::rename(&second, parent.path().join("moved-second"))
        .expect("introduce a new stale registration");

    let error = repository
        .prune_worktrees_verified(repo.path(), &target, &confirmed_digest, &cancellation())
        .await
        .expect_err("impact drift rejects repository-wide prune");
    assert!(error.detail.to_ascii_lowercase().contains("impact"));
    assert!(has_registered_worktree(repo.path(), &registered_first.path));
    assert!(has_registered_worktree(repo.path(), &registered_second));

    let fresh_impact = repository
        .preview_worktree_prune(repo.path(), &cancellation())
        .await
        .expect("fresh prune preview");
    let fresh_digest = git_worktree_prune_impact_digest(&fresh_impact);
    repository
        .prune_worktrees_verified(repo.path(), &target, &fresh_digest, &cancellation())
        .await
        .expect("matching fresh impact permits prune");
    assert!(!has_registered_worktree(
        repo.path(),
        &registered_first.path
    ));
    assert!(!has_registered_worktree(repo.path(), &registered_second));
    assert_eq!(
        git(repo.path(), &["branch", "--list", "feature/prune-first"]),
        "feature/prune-first"
    );
    assert_eq!(
        git(repo.path(), &["branch", "--list", "feature/prune-second"]),
        "feature/prune-second"
    );
}

#[tokio::test]
async fn worktree_prune_rejects_a_locked_missing_registration_with_its_reason() {
    if relaunch_with_isolated_git_config(
        "worktree_prune_rejects_a_locked_missing_registration_with_its_reason",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let parent = tempfile::tempdir().expect("worktree parent");
    let linked = parent.path().join("locked-stale");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/locked-stale",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    git(
        repo.path(),
        &[
            "worktree",
            "lock",
            "--reason",
            "retained by operator",
            "--",
            &linked.to_string_lossy(),
        ],
    );
    let repository = GitRepository::default();
    let registered = repository
        .worktree_inventory(repo.path(), &cancellation())
        .await
        .expect("locked inventory");
    let target = linked_worktree_record(&registered, &linked);
    fs::rename(&linked, parent.path().join("moved-locked"))
        .expect("make locked registration missing");

    let error = repository
        .prune_worktrees_verified(repo.path(), &target, "untrusted", &cancellation())
        .await
        .expect_err("locked target is never pruned");
    assert!(error.detail.contains("retained by operator"));
    assert!(has_registered_worktree(repo.path(), &target.path));
}

#[cfg(windows)]
#[tokio::test]
async fn git_can_deregister_a_linked_worktree_while_its_directory_is_quarantined() {
    let repository = init_repo();
    commit_file(repository.path(), "README.md", "base\n", "initial");
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = worktree_parent.path().join("target");
    let quarantine = worktree_parent.path().join("quarantine");
    git(
        repository.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/quarantine-probe",
            &worktree.to_string_lossy(),
        ],
    );
    fs::rename(&worktree, &quarantine).expect("quarantine worktree directory");
    fs::create_dir(&worktree).expect("create removal tombstone");
    fs::copy(quarantine.join(".git"), worktree.join(".git"))
        .expect("copy linked-worktree identity into tombstone");

    git(
        repository.path(),
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );

    assert!(quarantine.join(".git").exists());
    assert!(!worktree.exists());
    assert!(
        !git(repository.path(), &["worktree", "list", "--porcelain"])
            .replace('\\', "/")
            .contains(&worktree.to_string_lossy().replace('\\', "/"))
    );
}

#[tokio::test]
async fn protected_removal_recovers_a_deregistered_durable_quarantine() {
    let repository_root = init_repo();
    commit_file(repository_root.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = worktree_parent.path().join("recoverable");
    git(
        repository_root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/recoverable-quarantine",
            &worktree.to_string_lossy(),
        ],
    );
    let nonce = repository
        .prepare_worktree_removal_identity(repository_root.path(), &worktree, &cancellation())
        .await
        .expect("prepare durable identity");
    let git_link = fs::read_to_string(worktree.join(".git")).expect("linked-worktree Git file");
    let raw_admin_path = git_link
        .trim()
        .strip_prefix("gitdir: ")
        .expect("linked-worktree Git directory");
    let admin_path = Path::new(raw_admin_path);
    let admin_path = if admin_path.is_absolute() {
        admin_path.to_path_buf()
    } else {
        worktree.join(admin_path)
    };
    let quarantine = worktree_parent
        .path()
        .join(format!(".recoverable.coddy-studio-removal-{nonce}"));
    fs::rename(&worktree, &quarantine).expect("simulate committed quarantine rename");
    fs::create_dir(&worktree).expect("simulate removal tombstone");
    fs::write(worktree.join(".coddy-studio-removal-tombstone"), &nonce)
        .expect("write tombstone identity");
    fs::copy(quarantine.join(".git"), worktree.join(".git")).expect("copy tombstone Git link");
    let admin_quarantine = repository_root
        .path()
        .join(".git")
        .join("coddy-studio-worktree-removals")
        .join(&nonce);
    fs::create_dir_all(admin_quarantine.parent().expect("admin quarantine parent"))
        .expect("admin quarantine parent");
    fs::rename(&admin_path, &admin_quarantine)
        .expect("simulate committed administrative quarantine");
    assert!(quarantine.join("README.md").exists());

    repository
        .remove_worktree_with_identity(
            repository_root.path(),
            &worktree,
            true,
            &nonce,
            &cancellation(),
        )
        .await
        .expect("retry cleans the nonce-bound quarantine");

    assert!(!quarantine.exists());
    assert!(!admin_quarantine.exists());
}

#[tokio::test]
async fn protected_removal_repairs_a_marker_only_tombstone_after_a_crash() {
    let repository_root = init_repo();
    commit_file(repository_root.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = worktree_parent.path().join("marker-only");
    git(
        repository_root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/marker-only-tombstone",
            &worktree.to_string_lossy(),
        ],
    );
    let nonce = repository
        .prepare_worktree_removal_identity(repository_root.path(), &worktree, &cancellation())
        .await
        .expect("prepare durable identity");
    let quarantine = worktree_parent
        .path()
        .join(format!(".marker-only.coddy-studio-removal-{nonce}"));
    fs::rename(&worktree, &quarantine).expect("simulate committed quarantine rename");
    fs::create_dir(&worktree).expect("simulate partial tombstone");
    fs::write(worktree.join(".coddy-studio-removal-tombstone"), &nonce)
        .expect("write tombstone marker before simulated crash");

    repository
        .remove_worktree_with_identity(
            repository_root.path(),
            &worktree,
            true,
            &nonce,
            &cancellation(),
        )
        .await
        .expect("retry repairs tombstone and completes removal");

    assert!(!worktree.exists());
    assert!(!quarantine.exists());
    assert_eq!(
        repository
            .switch_ref(
                repository_root.path(),
                "feature/marker-only-tombstone",
                &cancellation(),
            )
            .await
            .expect("deregistered branch is unlocked")
            .as_deref(),
        Some("feature/marker-only-tombstone")
    );
}

#[tokio::test]
async fn protected_removal_preserves_an_unidentified_empty_tombstone() {
    let repository_root = init_repo();
    commit_file(repository_root.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = worktree_parent.path().join("empty-tombstone");
    git(
        repository_root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/empty-tombstone",
            &worktree.to_string_lossy(),
        ],
    );
    let nonce = repository
        .prepare_worktree_removal_identity(repository_root.path(), &worktree, &cancellation())
        .await
        .expect("prepare durable identity");
    let git_link = fs::read_to_string(worktree.join(".git")).expect("linked-worktree Git file");
    let admin_path = Path::new(
        git_link
            .trim()
            .strip_prefix("gitdir: ")
            .expect("linked-worktree Git directory"),
    );
    let admin_path = if admin_path.is_absolute() {
        admin_path.to_path_buf()
    } else {
        worktree.join(admin_path)
    };
    let quarantine = worktree_parent
        .path()
        .join(format!(".empty-tombstone.coddy-studio-removal-{nonce}"));
    fs::rename(&worktree, &quarantine).expect("simulate committed quarantine rename");
    fs::create_dir(&worktree).expect("simulate removal tombstone");
    fs::write(worktree.join(".coddy-studio-removal-tombstone"), &nonce)
        .expect("write tombstone identity");
    fs::copy(quarantine.join(".git"), worktree.join(".git")).expect("copy tombstone Git link");
    let admin_quarantine = repository_root
        .path()
        .join(".git")
        .join("coddy-studio-worktree-removals")
        .join(&nonce);
    fs::create_dir_all(admin_quarantine.parent().expect("admin quarantine parent"))
        .expect("admin quarantine parent");
    fs::rename(&admin_path, &admin_quarantine)
        .expect("simulate committed administrative quarantine");
    fs::remove_file(worktree.join(".git")).expect("simulate tombstone Git-link cleanup");
    fs::remove_file(worktree.join(".coddy-studio-removal-tombstone"))
        .expect("simulate tombstone marker cleanup");

    repository
        .remove_worktree_with_identity(
            repository_root.path(),
            &worktree,
            true,
            &nonce,
            &cancellation(),
        )
        .await
        .expect_err("an empty path without its nonce marker must fail closed");

    assert!(worktree.is_dir());
    assert!(quarantine.join("README.md").exists());
    assert!(admin_quarantine.exists());
}

#[tokio::test]
async fn protected_removal_recovers_an_empty_quarantine_after_cleanup() {
    let repository_root = init_repo();
    commit_file(repository_root.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = worktree_parent.path().join("empty-quarantine");
    git(
        repository_root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/empty-quarantine",
            &worktree.to_string_lossy(),
        ],
    );
    let nonce = repository
        .prepare_worktree_removal_identity(repository_root.path(), &worktree, &cancellation())
        .await
        .expect("prepare durable identity");
    let git_link = fs::read_to_string(worktree.join(".git")).expect("linked-worktree Git file");
    let admin_path = Path::new(
        git_link
            .trim()
            .strip_prefix("gitdir: ")
            .expect("linked-worktree Git directory"),
    );
    let admin_path = if admin_path.is_absolute() {
        admin_path.to_path_buf()
    } else {
        worktree.join(admin_path)
    };
    let quarantine = worktree_parent
        .path()
        .join(format!(".empty-quarantine.coddy-studio-removal-{nonce}"));
    fs::rename(&worktree, &quarantine).expect("simulate committed quarantine rename");
    let admin_quarantine = repository_root
        .path()
        .join(".git")
        .join("coddy-studio-worktree-removals")
        .join(&nonce);
    fs::create_dir_all(admin_quarantine.parent().expect("admin quarantine parent"))
        .expect("admin quarantine parent");
    fs::rename(&admin_path, &admin_quarantine)
        .expect("simulate committed administrative quarantine");
    fs::remove_dir_all(&quarantine).expect("simulate quarantine content cleanup");
    fs::create_dir(&quarantine).expect("leave the empty quarantine root after a crash");

    repository
        .remove_worktree_with_identity(
            repository_root.path(),
            &worktree,
            true,
            &nonce,
            &cancellation(),
        )
        .await
        .expect("retry completes from an empty quarantine");

    assert!(!quarantine.exists());
    assert!(!admin_quarantine.exists());
}

#[tokio::test]
async fn prepared_removal_identity_cannot_delete_a_replacement_worktree() {
    let repository_root = init_repo();
    commit_file(repository_root.path(), "README.md", "base\n", "initial");
    let repository = GitRepository::default();
    let worktree_parent = tempfile::tempdir().expect("worktree parent");
    let worktree = worktree_parent.path().join("replaceable");
    git(
        repository_root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/original-removal-identity",
            &worktree.to_string_lossy(),
        ],
    );
    let nonce = repository
        .prepare_worktree_removal_identity(repository_root.path(), &worktree, &cancellation())
        .await
        .expect("prepare original removal identity");
    git(
        repository_root.path(),
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    git(
        repository_root.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/replacement-removal-identity",
            &worktree.to_string_lossy(),
        ],
    );
    let sentinel = worktree.join("replacement-sentinel.txt");
    fs::write(&sentinel, "replacement survives\n").expect("replacement sentinel");

    repository
        .remove_worktree_with_identity(
            repository_root.path(),
            &worktree,
            true,
            &nonce,
            &cancellation(),
        )
        .await
        .expect_err("the original identity must not admit the replacement");

    assert_eq!(
        fs::read_to_string(&sentinel).expect("replacement remains"),
        "replacement survives\n"
    );
    assert!(
        git(repository_root.path(), &["worktree", "list", "--porcelain"])
            .replace('\\', "/")
            .to_lowercase()
            .contains(&worktree.to_string_lossy().replace('\\', "/").to_lowercase())
    );
}

#[tokio::test]
async fn managed_orphan_cleanup_remains_supported_by_protected_removal() {
    let fixture = tempfile::tempdir().expect("fixture root");
    let repository_path = fixture.path().join("repository");
    fs::create_dir(&repository_path).expect("repository directory");
    git(&repository_path, &["init", "-b", "main"]);
    commit_file(&repository_path, "README.md", "base\n", "initial");
    let orphan = fixture
        .path()
        .join(".coddy-studio-worktrees")
        .join("repository")
        .join("orphan");
    fs::create_dir_all(&orphan).expect("managed orphan directory");
    fs::write(orphan.join("sentinel.txt"), "managed orphan\n").expect("orphan sentinel");

    GitRepository::default()
        .remove_worktree(&repository_path, &orphan, true, &cancellation())
        .await
        .expect("managed orphan cleanup");

    assert!(!orphan.exists());
}

#[cfg(windows)]
#[tokio::test]
async fn worktree_removal_finishes_after_windows_releases_a_file_handle() {
    use std::os::windows::fs::OpenOptionsExt;

    if relaunch_with_isolated_git_config(
        "worktree_removal_finishes_after_windows_releases_a_file_handle",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let worktree_root = tempfile::tempdir().expect("worktree parent");
    let worktree_path = worktree_root.path().join("locked");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/locked",
            &worktree_path.to_string_lossy(),
        ],
    );
    let long_directory = (0..6).fold(worktree_path.clone(), |path, index| {
        path.join(format!("segment-{index}-{}", "x".repeat(40)))
    });
    fs::create_dir_all(&long_directory).expect("long worktree descendant");
    let locked_path = long_directory.join("locked.txt");
    assert!(
        locked_path.to_string_lossy().len() > 260,
        "fixture must cross the legacy Windows path limit"
    );
    fs::write(&locked_path, "locked\n").expect("locked fixture");
    let locked_file = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&locked_path)
        .expect("exclusive worktree file handle");
    let release_handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(locked_file);
    });

    let result = GitRepository::default()
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await;
    release_handle.join().expect("release locked file handle");

    result.expect("remove worktree after the file handle is released");
    assert!(!worktree_path.exists());
}

#[cfg(windows)]
#[tokio::test]
async fn worktree_removal_remains_retryable_after_cleanup_outlives_the_retry_window() {
    use std::os::windows::fs::OpenOptionsExt;

    if relaunch_with_isolated_git_config(
        "worktree_removal_remains_retryable_after_cleanup_outlives_the_retry_window",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "README.md", "base\n", "initial");
    let worktree_root = tempfile::tempdir().expect("worktree parent");
    let worktree_path = worktree_root.path().join("retryable");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/retryable",
            &worktree_path.to_string_lossy(),
        ],
    );
    let long_directory = (0..6).fold(worktree_path.clone(), |path, index| {
        path.join(format!("segment-{index}-{}", "x".repeat(40)))
    });
    fs::create_dir_all(&long_directory).expect("long worktree descendant");
    let locked_path = long_directory.join("locked.txt");
    assert!(
        locked_path.to_string_lossy().len() > 260,
        "fixture must cross the legacy Windows path limit"
    );
    fs::write(&locked_path, "locked\n").expect("locked fixture");
    let locked_file = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&locked_path)
        .expect("exclusive worktree file handle");
    let repository = GitRepository::default();

    let first_error = repository
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect_err("a locked file must outlive the bounded cleanup retries");
    assert!(first_error.diagnostics.is_none());
    assert!(
        first_error
            .detail
            .contains("is still in use, so removal stopped before deleting any files"),
        "unexpected protected-removal failure: {}",
        first_error.detail
    );
    assert!(worktree_path.exists());
    let registered_path = worktree_path.to_string_lossy().replace('\\', "/");
    assert!(
        git(repo.path(), &["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|path| path
                .replace('\\', "/")
                .eq_ignore_ascii_case(&registered_path)),
        "failed cleanup must retain Git registration for a safe retry"
    );
    drop(locked_file);

    repository
        .remove_worktree(repo.path(), &worktree_path, true, &cancellation())
        .await
        .expect("removal remains retryable after the file handle is released");
    assert!(!worktree_path.exists());
}

#[tokio::test]
async fn modified_submodule_is_reported_and_malformed_metadata_has_diagnostics() {
    if relaunch_with_isolated_git_config(
        "modified_submodule_is_reported_and_malformed_metadata_has_diagnostics",
    ) {
        return;
    }
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
            "modules/child",
        ],
    );
    git(parent.path(), &["commit", "-am", "add submodule"]);

    fs::write(
        parent.path().join("modules/child/lib.txt"),
        "modified in submodule\n",
    )
    .expect("submodule edit");
    let repository = GitRepository::default();
    let status = repository
        .local_status(parent.path(), &cancellation())
        .await
        .expect("submodule status");
    assert!(status.working_tree.files.iter().any(|file| {
        file.path == "modules/child"
            && file.area == Some(VcsStagingArea::Unstaged)
            && file.status == Some(VcsWorkingTreeFileStatus::Modified)
    }));

    fs::write(parent.path().join(".gitmodules"), "[submodule \"broken\"\n")
        .expect("malformed gitmodules");
    let error = repository
        .local_status(parent.path(), &cancellation())
        .await
        .expect_err("malformed submodule metadata must be actionable");
    assert_eq!(
        error.operation.as_ref(),
        "GitVcsDriver.statusDetailsLocal.status"
    );
    assert!(!error.detail.trim().is_empty());
    let diagnostics = error.diagnostics.expect("structured git diagnostics");
    assert!(
        diagnostics
            .exit_code
            .is_some_and(|exit_code| exit_code != 0)
    );
    assert_eq!(diagnostics.argument_count, Some(6));
    assert!(diagnostics.stderr_length.is_some_and(|length| length > 0));
}

#[tokio::test]
async fn broadcaster_cancellation_closes_subscription_and_missing_paths_report_local_status_errors()
{
    if relaunch_with_isolated_git_config(
        "broadcaster_cancellation_closes_subscription_and_missing_paths_report_local_status_errors",
    ) {
        return;
    }
    let repo = init_repo();
    commit_file(repo.path(), "tracked.txt", "base\n", "initial");
    let broadcaster = StatusBroadcaster::new(
        Arc::new(GitRepository::default()),
        Duration::from_secs(3600),
        2,
    );
    let subscription_cancellation = cancellation();
    let mut subscription = broadcaster
        .subscribe(repo.path().to_path_buf(), subscription_cancellation.clone())
        .await
        .expect("status subscription");

    let snapshot = subscription.recv().await.expect("initial snapshot");
    assert!(matches!(
        snapshot,
        VcsStatusStreamEvent::Snapshot { ref local, .. } if local.is_repo
    ));
    assert_eq!(broadcaster.active_poller_count(), 1);

    subscription_cancellation.cancel();
    while subscription.recv().await.is_some() {}
    drop(subscription);

    let missing = repo.path().join("missing/repository");
    let error = match broadcaster.subscribe(missing, cancellation()).await {
        Ok(_) => panic!("missing repository path cannot start a subscription"),
        Err(error) => error,
    };
    assert_eq!(
        error.operation.as_ref(),
        "GitVcsDriver.statusDetailsLocal.status"
    );
    assert!(
        error
            .detail
            .starts_with("Could not start git: a required file or directory was not found")
    );
    assert!(error.detail.contains("repository folder is accessible"));
    assert!(
        !error
            .detail
            .contains(repo.path().to_string_lossy().as_ref())
    );
    assert!(!error.detail.contains("missing/repository"));
    let diagnostics = error.diagnostics.expect("structured launch diagnostics");
    assert_eq!(diagnostics.argument_count, Some(6));
    assert_eq!(diagnostics.exit_code, None);
    assert_eq!(diagnostics.stdout_length, None);
    assert_eq!(diagnostics.stderr_length, None);
}

async fn wait_for_path(path: &Path) {
    for _ in 0..400 {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {}", path.display());
}
