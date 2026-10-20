//! Git Manager domain modules shared by the read, guard, and operation phases.

pub mod conflicts;
mod generation;
pub mod graph;
pub mod guards;
pub mod in_progress;
pub mod merge;
pub mod operations;
pub mod patch;
pub mod refs;
pub mod rewrite;
pub mod stash;
pub mod tags;

#[cfg(test)]
mod telemetry {
    use std::{
        ffi::OsStr,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use tokio_util::sync::CancellationToken;

    use super::{conflicts::ConflictSide, tags};
    use crate::{
        git::{
            GitProcessRunner, GitRepository, ProcessOutput, ProcessRequest,
            repository::BoxGitProcessFuture,
        },
        source_control::PullRequestService,
        test_support::TestSandbox,
    };

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PARENT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[derive(Default)]
    struct RecordingGitRunner {
        requests: Mutex<Vec<ProcessRequest>>,
    }

    impl RecordingGitRunner {
        fn requests(&self) -> Vec<ProcessRequest> {
            self.requests.lock().expect("request lock").clone()
        }

        fn clear(&self) {
            self.requests.lock().expect("request lock").clear();
        }
    }

    impl GitProcessRunner for RecordingGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            let stdout = match request.operation.as_str() {
                "GitManager.commit.sha" => format!("{SHA}\n"),
                "GitManager.undoCommit.head" => {
                    format!("{SHA}\0{PARENT_SHA}\0Local commit\n")
                }
                "GitManager.tags.list" => format!("{SHA} refs/tags/release\n"),
                _ => String::new(),
            };
            self.requests.lock().expect("request lock").push(request);
            Box::pin(async move {
                Ok(ProcessOutput {
                    exit_code: 0,
                    stdout,
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                })
            })
        }
    }

    fn assert_environment(request: &ProcessRequest, key: &str, value: &str) {
        assert!(
            request.env.iter().any(|(actual_key, actual_value)| {
                actual_key == OsStr::new(key) && actual_value == OsStr::new(value)
            }),
            "{} must set {key}={value}",
            request.operation
        );
    }

    fn assert_non_interactive_git(request: &ProcessRequest) {
        assert_eq!(
            request.command,
            PathBuf::from("git"),
            "{} spawned a non-git process",
            request.operation
        );
        for (key, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ASKPASS", ""),
            ("SSH_ASKPASS_REQUIRE", "never"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ] {
            assert_environment(request, key, value);
        }
    }

    #[tokio::test]
    async fn every_git_manager_read_and_operation_is_non_interactive_git() {
        let runner = Arc::new(RecordingGitRunner::default());
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let sandbox = TestSandbox::new("git-manager-telemetry");
        let cwd = sandbox.root();
        let cancellation = CancellationToken::new();

        repository
            .git_manager_resolve_tips(cwd, &cancellation)
            .await
            .expect("resolve tips read");
        repository
            .git_manager_validate_tips(cwd, &[SHA.to_owned()], &cancellation)
            .await
            .expect("validate tips read");
        repository
            .git_manager_log_page(cwd, &[SHA.to_owned()], false, 0, 100, &cancellation)
            .await
            .expect("history read");
        repository
            .git_manager_refs(cwd, &cancellation)
            .await
            .expect("refs read");
        repository
            .git_manager_worktrees(cwd, &cancellation)
            .await
            .expect("worktrees read");
        repository
            .git_manager_ahead_behind(cwd, "main", "origin/main", &cancellation)
            .await
            .expect("ahead/behind read");
        repository
            .git_manager_head_ref(cwd, &cancellation)
            .await
            .expect("HEAD ref read");
        repository
            .git_manager_head_sha(cwd, &cancellation)
            .await
            .expect("HEAD sha read");
        repository
            .git_manager_status(cwd, &cancellation)
            .await
            .expect("status read");
        repository
            .git_manager_conflicted_paths(cwd, &cancellation)
            .await
            .expect("conflict read");
        repository
            .git_manager_remotes(cwd, &cancellation)
            .await
            .expect("remotes read");
        repository
            .git_manager_git_dir(cwd, &cancellation)
            .await
            .expect("git dir read");
        repository
            .git_manager_in_progress_paths(cwd, &cancellation)
            .await
            .expect("in-progress read");
        repository
            .git_manager_count_commit_range(cwd, SHA, PARENT_SHA, &cancellation)
            .await
            .expect("in-progress count read");
        repository
            .git_manager_signal_refs(cwd, &cancellation)
            .await
            .expect("signal read");
        repository
            .git_manager_default_ref(cwd, Some("main"), &cancellation)
            .await
            .expect("default ref read");
        repository
            .git_manager_working_tree_diff(cwd, "tracked.txt", false, &cancellation)
            .await
            .expect("working-tree diff read");
        repository
            .git_manager_untracked_paths(cwd, "untracked.txt", &cancellation)
            .await
            .expect("untracked-path read");
        repository
            .git_manager_untracked_diff(cwd, "untracked.txt", &cancellation)
            .await
            .expect("untracked diff read");
        repository
            .git_manager_commit_diff(cwd, SHA, "tracked.txt", &cancellation)
            .await
            .expect("commit diff read");
        repository
            .git_manager_stash_list(cwd, &cancellation)
            .await
            .expect("stash list read");
        repository
            .git_manager_stash_file_list(cwd, "stash@{0}", &cancellation)
            .await
            .expect("stash files read");
        repository
            .git_manager_stash_diff(cwd, "stash@{0}", &cancellation)
            .await
            .expect("stash diff read");
        repository
            .git_manager_resolve_merge_tip(cwd, "topic", &cancellation)
            .await
            .expect("merge tip read");
        repository
            .git_manager_merge_ahead_behind(cwd, SHA, PARENT_SHA, &cancellation)
            .await
            .expect("merge comparison read");
        repository
            .git_manager_merge_tree(cwd, SHA, PARENT_SHA, None, &cancellation)
            .await
            .expect("merge tree read");
        repository
            .git_manager_merge_tree(cwd, SHA, PARENT_SHA, Some(SHA), &cancellation)
            .await
            .expect("merge tree read with attribute source");
        repository
            .git_manager_git_version(cwd, &cancellation)
            .await
            .expect("git version read");
        repository
            .git_manager_is_ancestor(cwd, SHA, PARENT_SHA, &cancellation)
            .await
            .expect("ancestor read");
        repository
            .git_manager_commit_gpg_sign(cwd, &cancellation)
            .await
            .expect("signing setting read");
        repository
            .git_manager_symbolic_ref(cwd, "refs/heads/release", &cancellation)
            .await
            .expect("symbolic ref read");
        tags::list_tags(&repository, cwd, &cancellation)
            .await
            .expect("tag read");
        repository
            .git_manager_head_tags(cwd, &cancellation)
            .await
            .expect("HEAD tag read");

        let read_requests = runner.requests();
        assert!(!read_requests.is_empty());
        for request in &read_requests {
            assert_non_interactive_git(request);
            assert_environment(request, "GIT_OPTIONAL_LOCKS", "0");
        }
        runner.clear();

        repository
            .commit_with_options(
                cwd,
                &[
                    "commit".to_owned(),
                    "--allow-empty".to_owned(),
                    "-F".to_owned(),
                    "-".to_owned(),
                ],
                b"Local commit\n",
                true,
                &cancellation,
            )
            .await
            .expect("commit operation");
        repository
            .undo_head_commit(cwd, &cancellation)
            .await
            .expect("undo operation");
        repository
            .stage_files(cwd, &["tracked.txt".to_owned()], &cancellation)
            .await
            .expect("stage operation");
        repository
            .unstage_files(cwd, &["tracked.txt".to_owned()], &cancellation)
            .await
            .expect("unstage operation");
        repository
            .git_manager_intent_to_add(cwd, "tracked.txt", &cancellation)
            .await
            .expect("intent-to-add operation");
        repository
            .git_manager_clear_intent_to_add(cwd, "tracked.txt", &cancellation)
            .await
            .expect("clear intent-to-add operation");
        for (operation, cached, reverse) in [
            ("GitManager.stagePartial.apply", true, false),
            ("GitManager.unstagePartial.apply", true, true),
            ("GitManager.discardPartial.apply", false, true),
        ] {
            repository
                .git_manager_apply_partial_patch(
                    operation,
                    cwd,
                    b"patch".to_vec(),
                    cached,
                    reverse,
                    &cancellation,
                )
                .await
                .expect("partial-selection operation");
        }
        repository
            .git_manager_tracked_paths(cwd, &["tracked.txt".to_owned()], &cancellation)
            .await
            .expect("discard tracked-path read");
        repository
            .git_manager_restore_tracked_paths(cwd, &["tracked.txt".to_owned()], &cancellation)
            .await
            .expect("discard restore operation");
        repository
            .git_manager_create_branch(cwd, "topic", Some("main"), true, &cancellation)
            .await
            .expect("branch create operation");
        repository
            .git_manager_checkout_local_branch(cwd, "topic", &cancellation)
            .await
            .expect("local checkout operation");
        repository
            .git_manager_checkout_remote_branch(cwd, "topic", "origin/topic", &cancellation)
            .await
            .expect("remote checkout operation");
        repository
            .git_manager_rename_branch(cwd, "topic", "renamed", &cancellation)
            .await
            .expect("branch rename operation");
        repository
            .git_manager_delete_branch(cwd, "renamed", false, &cancellation)
            .await
            .expect("branch delete operation");
        repository
            .git_manager_delete_remote_branch(cwd, "origin", "renamed", &cancellation)
            .await
            .expect("remote branch delete operation");
        repository
            .git_manager_fetch(cwd, "origin", &cancellation)
            .await
            .expect("fetch operation");
        repository
            .git_manager_pull(cwd, "origin", &cancellation)
            .await
            .expect("pull operation");
        for (set_upstream, force_with_lease) in [(false, false), (true, false), (false, true)] {
            repository
                .git_manager_push(
                    cwd,
                    "origin",
                    "topic",
                    Some("topic"),
                    set_upstream,
                    force_with_lease,
                    false,
                    &cancellation,
                )
                .await
                .expect("push operation");
        }
        repository
            .git_manager_stash_push(
                cwd,
                "saved work",
                &["tracked.txt".to_owned()],
                false,
                &cancellation,
            )
            .await
            .expect("stash push operation");
        repository
            .git_manager_stash_apply(cwd, "stash@{0}", &cancellation)
            .await
            .expect("stash apply operation");
        repository
            .git_manager_stash_pop(cwd, "stash@{0}", &cancellation)
            .await
            .expect("stash pop operation");
        repository
            .git_manager_stash_drop(cwd, "stash@{0}", &cancellation)
            .await
            .expect("stash drop operation");
        repository
            .git_manager_merge(cwd, "topic", false, false, &cancellation)
            .await
            .expect("merge operation");
        repository
            .git_manager_merge(cwd, "topic", false, true, &cancellation)
            .await
            .expect("squash merge operation");
        repository
            .git_manager_squash_merge_commit(cwd, false, &cancellation)
            .await
            .expect("squash merge commit operation");
        repository
            .git_manager_commit_tree(cwd, SHA, [SHA, PARENT_SHA], "Merge", false, &cancellation)
            .await
            .expect("merge commit-tree operation");
        repository
            .git_manager_publish_merge(cwd, SHA, "release", "refs/heads/topic", &cancellation)
            .await
            .expect("merge publish operation");
        repository
            .git_manager_rebase(cwd, "main", "topic", &cancellation)
            .await
            .expect("rebase operation");
        repository
            .git_manager_interactive_rebase(
                cwd,
                &format!("pick {SHA} local commit\n"),
                Some("Squashed commit\n"),
                Some(PARENT_SHA),
                &cancellation,
            )
            .await
            .expect("squash operation");
        repository
            .git_manager_interactive_rebase(
                cwd,
                &format!("pick {SHA} local commit\n"),
                None,
                Some(PARENT_SHA),
                &cancellation,
            )
            .await
            .expect("reorder operation");
        repository
            .git_manager_cherry_pick(cwd, &[SHA.to_owned()], &cancellation)
            .await
            .expect("cherry-pick operation");
        repository
            .git_manager_revert(cwd, SHA, false, &cancellation)
            .await
            .expect("revert operation");
        for mode in ["hard", "soft", "mixed"] {
            repository
                .git_manager_reset(cwd, SHA, mode, &cancellation)
                .await
                .expect("reset operation");
        }
        for operation in ["merge", "rebase", "cherry-pick", "revert"] {
            repository
                .git_manager_continue(cwd, operation, false, &cancellation)
                .await
                .expect("continue operation");
            repository
                .git_manager_abort(cwd, operation, &cancellation)
                .await
                .expect("abort operation");
        }
        repository
            .git_manager_resolve_conflict(
                cwd,
                "tracked.txt",
                ConflictSide::Ours,
                false,
                &cancellation,
            )
            .await
            .expect("resolve conflict operation");
        tags::create_tag(&repository, cwd, "release", SHA, &cancellation)
            .await
            .expect("tag create operation");
        tags::delete_tag(&repository, cwd, "release", &cancellation)
            .await
            .expect("tag delete operation");
        tags::push_tag(&repository, cwd, "origin", "release", &cancellation)
            .await
            .expect("tag push operation");

        let operation_requests = runner.requests();
        assert!(!operation_requests.is_empty());
        for request in &operation_requests {
            assert_non_interactive_git(request);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn constructing_git_manager_services_starts_no_timer_or_process() {
        let runner = Arc::new(RecordingGitRunner::default());
        let _repository = GitRepository::with_runner_for_test(runner.clone());
        let _provider = PullRequestService::with_provider_commands(
            "must-not-run-gh",
            "must-not-run-glab",
            "must-not-run-az",
        );

        // These pre-existing periodic workers are deliberately outside this feature's scope:
        // summary.rs has 30-second subscriber-scoped provider enrichment, and fetch_owner.rs owns
        // automatic git fetch. Neither worker belongs to the Git Manager surface tested here.
        tokio::time::advance(Duration::from_secs(60 * 60)).await;

        assert!(runner.requests().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_checks_handler_is_the_only_non_git_process_surface() {
        use crate::source_control::{ProviderKind, checks::ProviderChecksResult};

        let sandbox = TestSandbox::new("git-manager-explicit-checks");
        let provider_cli = sandbox.executable_script(
            "gh",
            "printf '%s\\n' '{\"statusCheckRollup\":[{\"__typename\":\"CheckRun\",\"name\":\"build\",\"status\":\"COMPLETED\",\"conclusion\":\"SUCCESS\",\"detailsUrl\":null,\"workflowName\":\"CI\"}]}'",
            "",
        );
        let service = PullRequestService::with_provider_commands(
            provider_cli.to_string_lossy(),
            "must-not-run-glab",
            "must-not-run-az",
        );

        let result = service
            .read_checks(
                ProviderKind::Github,
                sandbox.root(),
                42,
                &CancellationToken::new(),
            )
            .await
            .expect("explicit checks handler may invoke the configured provider CLI");

        assert!(matches!(result, ProviderChecksResult::Available(checks) if checks.len() == 1));
    }
}
