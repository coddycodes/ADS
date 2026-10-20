#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::Duration,
};

use base64::Engine;
use bibcode_server::{
    CauseItem, RequestId, RpcExit, RpcRegistry, RpcRequest, ServerConfig, ServerHandle,
    ServerMessage, ServerRuntime,
    git::{GitRepository, MAX_DIFF_BUFFER_SIZE, NativeFileTrash, StatusBroadcaster},
    persistence::{Database, ProjectionProject, ProjectionThread, Repositories, run_migrations},
    production::git_manager_rpc::{
        ConfiguredGitManagerRpcServices, GitManagerRpcServices, register_git_manager_rpc,
    },
    worktree_catalog::{WorkspaceAvailabilityRegistry, WorktreeCatalogService},
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const SOCKET_RESPONSE_BOUND: Duration = Duration::from_secs(60);

type TestSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Fixture {
    _root: TempDir,
    repository_path: PathBuf,
    remote_path: PathBuf,
    services: ConfiguredGitManagerRpcServices,
    broadcaster: StatusBroadcaster,
    repositories: Repositories,
}

#[test]
fn phase_13_history_rewrite_sources_do_not_log_repository_operands() {
    let operations = include_str!("../src/git/manager/operations.rs");
    let operation_section = operations
        .split_once("pub async fn run_branch_or_sync_operation")
        .expect("operation section starts")
        .1
        .split_once("fn validate_merge_source")
        .expect("operation section ends")
        .0;
    let repository = include_str!("../src/git/repository.rs");
    let repository_section = repository
        .split_once("pub(crate) async fn git_manager_rebase")
        .expect("rewrite repository section starts")
        .1
        .split_once("pub async fn switch_ref")
        .expect("rewrite repository section ends")
        .0;
    for source in [
        include_str!("../src/git/manager/rewrite.rs"),
        include_str!("../src/git/manager/conflicts.rs"),
        operation_section,
        repository_section,
    ] {
        assert!(!source.contains("tracing::"));
    }
}

impl Fixture {
    async fn new() -> Self {
        Self::with_fetch_interval(Duration::from_millis(50)).await
    }

    async fn with_fetch_interval(fetch_interval: Duration) -> Self {
        Self::build(fetch_interval, false)
            .await
            .expect("default Git fixture")
    }

    async fn new_reftable(fetch_interval: Duration) -> Option<Self> {
        Self::build(fetch_interval, true).await
    }

    async fn build(fetch_interval: Duration, reftable: bool) -> Option<Self> {
        let root = TempDir::new().expect("temporary Git Manager fixture");
        let repository_path = root.path().join("main");
        let remote_path = root.path().join("remote.git");
        fs::create_dir(&repository_path).expect("main checkout directory");
        git(root.path(), &["init", "-q", "--bare", path(&remote_path)]);
        if reftable {
            if !git_output(
                &repository_path,
                &["init", "-q", "-b", "main", "--ref-format=reftable"],
            )
            .status
            .success()
            {
                eprintln!(
                    "reftable coverage skipped: installed Git does not support reftable initialization"
                );
                return None;
            }
        } else {
            git(&repository_path, &["init", "-q", "-b", "main"]);
        }
        git(&repository_path, &["config", "core.autocrlf", "false"]);
        configure_identity(&repository_path);
        fs::write(repository_path.join("tracked.txt"), "base\n").expect("base file");
        git(&repository_path, &["add", "tracked.txt"]);
        git(&repository_path, &["commit", "-q", "-m", "base"]);
        git(
            &repository_path,
            &["remote", "add", "origin", path(&remote_path)],
        );
        git(&repository_path, &["push", "-q", "-u", "origin", "main"]);

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let repositories = Repositories::new(database);
        repositories
            .upsert_project(ProjectionProject {
                project_id: "project-1".to_owned(),
                title: "Git Manager Operations".to_owned(),
                workspace_root: repository_path.to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([]),
                worktree_discovery: json!({}),
                worktree_repository_key: None,
                created_at: "2026-09-01T00:00:00Z".to_owned(),
                updated_at: "2026-09-01T00:00:00Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("project projection");
        let repository = Arc::new(GitRepository::default());
        let (remote_refresh_interval, _) = tokio::sync::watch::channel(fetch_interval);
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            repository.clone(),
            Duration::from_secs(3_600),
            remote_refresh_interval,
            8,
        );
        let availability = WorkspaceAvailabilityRegistry::new();
        let catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            repository.clone(),
            availability.clone(),
        );
        let services = GitManagerRpcServices::with_dependencies(
            repository,
            broadcaster.clone(),
            catalog,
            repositories.clone(),
            availability,
            Arc::new(NativeFileTrash::default()),
        );
        Some(Self {
            _root: root,
            repository_path,
            remote_path,
            services,
            broadcaster,
            repositories,
        })
    }

    /// Operations resolve a linked worktree to its project through a thread that owns it.
    async fn own_worktree(&self, worktree: &Path) {
        self.repositories
            .upsert_thread(ProjectionThread {
                thread_id: "thread-linked".to_owned(),
                project_id: "project-1".to_owned(),
                title: "Linked worktree".to_owned(),
                kind: "default".to_owned(),
                model_selection: json!({"instanceId": "codex", "model": "gpt-5.4"}),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                branch: Some("linked".to_owned()),
                worktree_path: Some(worktree.to_string_lossy().into_owned()),
                latest_turn_id: None,
                created_at: "2026-09-01T00:00:01Z".to_owned(),
                updated_at: "2026-09-01T00:00:01Z".to_owned(),
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
            .expect("thread owning the linked worktree");
    }

    fn operation(&self, id: &str, payload: Value) -> mpsc::Receiver<Result<Vec<Value>, Value>> {
        self.services.operation_stream(
            RpcRequest {
                id: RequestId::try_from(id).expect("request id"),
                tag: "gitManager.runOperation".to_owned(),
                payload,
                headers: Vec::new(),
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            CancellationToken::new(),
        )
    }

    async fn read(&self, id: &str, tag: &str, payload: Value) -> Result<Value, Value> {
        self.services
            .read_unary(rpc_request(id, tag, payload), CancellationToken::new())
            .await
    }

    async fn mutate(&self, id: &str, tag: &str, payload: Value) -> Result<Value, Value> {
        self.services
            .mutation_unary(rpc_request(id, tag, payload), CancellationToken::new())
            .await
    }
}

#[tokio::test]
async fn commit_image_diff_round_trips_both_binary_blobs_byte_for_byte() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let before = b"\x89PNG\r\n\x1a\n\0\xff\xfe\x80before".to_vec();
    let after = b"\x89PNG\r\n\x1a\n\0\xff\xfe\x80after".to_vec();
    fs::write(cwd.join("image.png"), &before).expect("before image");
    git(&cwd, &["add", "image.png"]);
    git(&cwd, &["commit", "-q", "-m", "image before"]);
    fs::write(cwd.join("image.png"), &after).expect("after image");
    git(&cwd, &["commit", "-qam", "image after"]);
    let sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let diff = fixture
        .read(
            "90",
            "gitManager.getDiff",
            json!({
                "cwd": cwd,
                "source": { "_tag": "commit", "sha": sha, "path": "image.png" }
            }),
        )
        .await
        .expect("image diff");

    assert_eq!(diff["_tag"], "image");
    assert_eq!(diff["before"]["mimeType"], "image/png");
    assert_eq!(diff["after"]["mimeType"], "image/png");
    let decode = |value: &Value| {
        base64::engine::general_purpose::STANDARD
            .decode(value.as_str().expect("base64 image side"))
            .expect("valid base64 image side")
    };
    assert_eq!(decode(&diff["before"]["contentBase64"]), before);
    assert_eq!(decode(&diff["after"]["contentBase64"]), after);
}

#[tokio::test]
async fn added_image_diff_has_one_side_and_an_oversized_blob_is_unrenderable() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let added = b"\x89PNG\r\n\x1a\nadded".to_vec();
    fs::write(cwd.join("added.png"), &added).expect("added image");
    git(&cwd, &["add", "added.png"]);
    git(&cwd, &["commit", "-q", "-m", "add image"]);
    let sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let diff = fixture
        .read(
            "91",
            "gitManager.getDiff",
            json!({
                "cwd": cwd,
                "source": { "_tag": "commit", "sha": sha, "path": "added.png" }
            }),
        )
        .await
        .expect("added image diff");
    assert_eq!(diff["_tag"], "image");
    assert!(diff["before"]["contentBase64"].is_null());
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(
                diff["after"]["contentBase64"]
                    .as_str()
                    .expect("after image")
            )
            .expect("base64 after image"),
        added
    );

    fs::write(
        cwd.join("oversized.png"),
        vec![0xa5; MAX_DIFF_BUFFER_SIZE + 1],
    )
    .expect("oversized image");
    git(&cwd, &["add", "oversized.png"]);
    git(&cwd, &["commit", "-q", "-m", "oversized image"]);
    let oversized_sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    let oversized = fixture
        .read(
            "92",
            "gitManager.getDiff",
            json!({
                "cwd": cwd,
                "source": {
                    "_tag": "commit",
                    "sha": oversized_sha,
                    "path": "oversized.png"
                }
            }),
        )
        .await
        .expect("oversized image result");
    assert_eq!(oversized["_tag"], "unrenderable");
    assert_eq!(oversized["byteLength"], MAX_DIFF_BUFFER_SIZE + 1);
}

#[tokio::test]
async fn tag_create_push_and_delete_share_the_streaming_mutation_fence() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let created = collect_events(fixture.operation(
        "93",
        json!({
            "_tag": "tag-create",
            "cwd": cwd,
            "projectId": "project-1",
            "name": "release/v1",
            "sha": sha
        }),
    ))
    .await;
    assert_eq!(event_kinds(&created).last(), Some(&"finished"));
    let refs = fixture
        .read("94", "gitManager.getRefs", json!({ "cwd": cwd }))
        .await
        .expect("refs after tag create");
    assert!(
        refs["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag["name"] == "release/v1"))
    );

    let pushed = collect_events(fixture.operation(
        "95",
        json!({
            "_tag": "tag-push",
            "cwd": cwd,
            "projectId": "project-1",
            "name": "release/v1",
            "remote": "origin"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&pushed).last(), Some(&"finished"));
    assert!(
        git_output(
            &cwd,
            &["ls-remote", "--exit-code", "origin", "refs/tags/release/v1"]
        )
        .status
        .success()
    );

    let deleted = collect_events(fixture.operation(
        "96",
        json!({
            "_tag": "tag-delete",
            "cwd": cwd,
            "projectId": "project-1",
            "name": "release/v1"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&deleted).last(), Some(&"finished"));
    assert!(
        !git_output(&cwd, &["show-ref", "--verify", "refs/tags/release/v1"])
            .status
            .success()
    );
    assert!(
        git_output(
            &cwd,
            &["ls-remote", "--exit-code", "origin", "refs/tags/release/v1"]
        )
        .status
        .success(),
        "local deletion must not delete the remote tag"
    );

    let missing = collect_events(fixture.operation(
        "97",
        json!({
            "_tag": "tag-delete",
            "cwd": cwd,
            "projectId": "project-1",
            "name": "release/v1"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&missing), ["started", "failed"]);
    assert_eq!(missing[1]["code"], "tag-not-found");
}

/// A GitHub CLI stub that resolves pull request 42 for `pr list` and answers the
/// rollup read with `checks_stdout` (or fails it with `checks_exit`), recording
/// every argv line into `calls`.
#[cfg(unix)]
struct GitHubProviderStub {
    calls: PathBuf,
    services: ConfiguredGitManagerRpcServices,
}

#[cfg(unix)]
impl GitHubProviderStub {
    fn install(fixture: &Fixture, checks_stdout: &str, checks_exit: u8) -> Self {
        use bibcode_server::source_control::PullRequestService;

        git(
            &fixture.repository_path,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/example/repository.git",
            ],
        );
        let calls = fixture._root.path().join("provider-calls");
        let command = fixture._root.path().join("provider-gh");
        executable_fixture::write_executable(
            &command,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1:$2\" in\n  pr:list) printf '%s\\n' '[{{\"number\":42,\"title\":\"Explicit PR\",\"url\":\"https://github.test/42\",\"baseRefName\":\"main\",\"headRefName\":\"main\",\"state\":\"OPEN\"}}]' ;;\n  pr:view) printf '%s\\n' '{checks_stdout}'; exit {checks_exit} ;;\n  *) exit 64 ;;\nesac\n",
                calls.to_string_lossy()
            ),
        );
        let services = fixture.services.clone().with_pull_request_service(
            PullRequestService::with_provider_commands(
                command.to_string_lossy(),
                "unused-glab",
                "unused-az",
            ),
        );
        Self { calls, services }
    }

    async fn list_pull_requests(&self, cwd: &Path) -> Result<Value, Value> {
        self.services
            .read_unary(
                rpc_request("98", "gitManager.listPullRequests", json!({ "cwd": cwd })),
                CancellationToken::new(),
            )
            .await
    }

    fn calls(&self) -> String {
        fs::read_to_string(&self.calls).expect("provider calls")
    }
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_pull_request_read_invokes_the_provider_once_for_prs_and_once_for_checks() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let stub = GitHubProviderStub::install(
        &fixture,
        r#"{"statusCheckRollup":[{"__typename":"CheckRun","completedAt":"2026-09-02T13:09:09Z","conclusion":"SUCCESS","detailsUrl":"https://github.test/check/1","name":"build","startedAt":"2026-09-02T12:52:36Z","status":"COMPLETED","workflowName":"CI"}]}"#,
        0,
    );

    let result = stub
        .list_pull_requests(&cwd)
        .await
        .expect("explicit provider read");

    assert_eq!(result["status"], "available");
    assert_eq!(result["pullRequests"][0]["number"], 42);
    assert_eq!(
        result["checks"],
        json!([{
            "name": "build",
            "state": "SUCCESS",
            "link": "https://github.test/check/1",
            "workflow": "CI",
        }])
    );
    let calls = stub.calls();
    assert_eq!(calls.lines().count(), 2);
    assert!(calls.contains("pr list --head main --state open --limit 1 --json"));
    assert!(calls.contains("pr view 42 --json statusCheckRollup"));
    assert!(!calls.contains("pr checks"));
    assert!(!calls.contains("--watch"));
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_pull_request_read_renders_an_open_pull_request_that_has_no_checks() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let stub = GitHubProviderStub::install(&fixture, r#"{"statusCheckRollup":[]}"#, 0);

    let result = stub
        .list_pull_requests(&cwd)
        .await
        .expect("a pull request without checks is not a provider failure");

    assert_eq!(result["status"], "available");
    assert_eq!(result["pullRequests"][0]["number"], 42);
    assert_eq!(result["checks"], json!([]));
    assert_eq!(stub.calls().lines().count(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_pull_request_read_keeps_pending_checks_available() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let stub = GitHubProviderStub::install(
        &fixture,
        r#"{"statusCheckRollup":[{"__typename":"CheckRun","conclusion":null,"detailsUrl":"https://github.test/check/2","name":"build","startedAt":"2026-09-02T12:52:36Z","status":"IN_PROGRESS","workflowName":"CI"},{"__typename":"StatusContext","context":"ci/external","state":"PENDING","targetUrl":"https://external.test/1"}]}"#,
        0,
    );

    let result = stub
        .list_pull_requests(&cwd)
        .await
        .expect("pending checks remain readable");

    assert_eq!(result["status"], "available");
    assert_eq!(
        result["checks"],
        json!([
            {
                "name": "build",
                "state": "IN_PROGRESS",
                "link": "https://github.test/check/2",
                "workflow": "CI",
            },
            {
                "name": "ci/external",
                "state": "PENDING",
                "link": "https://external.test/1",
                "workflow": null,
            },
        ])
    );
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_pull_request_read_reports_a_genuine_check_command_failure() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let stub = GitHubProviderStub::install(&fixture, "HTTP 401: Bad credentials", 1);

    let error = stub
        .list_pull_requests(&cwd)
        .await
        .expect_err("a failing provider command is surfaced, not rendered as no checks");

    assert_eq!(error["code"], "provider-command-failed");
    assert_eq!(
        error["message"],
        "The source-control provider could not load pull-request checks."
    );
    assert_eq!(stub.calls().lines().count(), 2);
}

#[tokio::test]
async fn partial_stage_unstage_and_discard_mutate_only_their_intended_store() {
    let fixture = Fixture::new().await;
    let path = fixture.repository_path.join("tracked.txt");
    fs::write(&path, "base\none\ntwo\nthree\nfour\n").expect("four-line working change");

    let unstaged = fixture
        .read(
            "50",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
            }),
        )
        .await
        .expect("unstaged diff");
    fixture
        .mutate(
            "51",
            "gitManager.stagePartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1, 3],
                "baseGeneration": unstaged["generation"]
            }),
        )
        .await
        .expect("partial stage");

    assert_eq!(
        index_content(&fixture.repository_path),
        "base\none\nthree\n"
    );
    assert_eq!(
        fs::read(&path).expect("working bytes after stage"),
        b"base\none\ntwo\nthree\nfour\n"
    );

    let staged = fixture
        .read(
            "52",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": true }
            }),
        )
        .await
        .expect("staged diff");
    let before_unstage = fs::read(&path).expect("working bytes before unstage");
    fixture
        .mutate(
            "53",
            "gitManager.unstagePartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1],
                "baseGeneration": staged["generation"]
            }),
        )
        .await
        .expect("partial unstage");

    assert_eq!(index_content(&fixture.repository_path), "base\nthree\n");
    assert_eq!(
        fs::read(&path).expect("working bytes after unstage"),
        before_unstage,
        "unstaging must leave the working tree byte-for-byte unchanged"
    );

    let before_discard_index = index_content(&fixture.repository_path);
    let unstaged = fixture
        .read(
            "54",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
            }),
        )
        .await
        .expect("remaining unstaged diff");
    fixture
        .mutate(
            "55",
            "gitManager.discardPartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [2],
                "baseGeneration": unstaged["generation"]
            }),
        )
        .await
        .expect("partial discard");

    assert_eq!(
        fs::read_to_string(&path).expect("working content after discard"),
        "base\none\nthree\nfour\n"
    );
    assert_eq!(
        index_content(&fixture.repository_path),
        before_discard_index,
        "discarding must leave the index unchanged"
    );
}

#[tokio::test]
async fn stale_partial_requests_fail_closed_for_stage_unstage_and_discard() {
    let fixture = Fixture::new().await;
    let path = fixture.repository_path.join("tracked.txt");

    fs::write(&path, "base\none\ntwo\n").expect("unstaged additions");
    let stage_diff = fixture
        .read(
            "60",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
            }),
        )
        .await
        .expect("stage source diff");
    fs::write(&path, "base\none\ntwo\nthree\n").expect("rewrite after stage selection");
    let stale_stage = fixture
        .mutate(
            "61",
            "gitManager.stagePartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1],
                "baseGeneration": stage_diff["generation"]
            }),
        )
        .await
        .expect_err("stale stage selection");
    assert_eq!(stale_stage["code"], "stale-selection");
    assert_eq!(index_content(&fixture.repository_path), "base\n");

    git(&fixture.repository_path, &["add", "tracked.txt"]);
    let unstage_diff = fixture
        .read(
            "62",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": true }
            }),
        )
        .await
        .expect("unstage source diff");
    git(&fixture.repository_path, &["reset", "--", "tracked.txt"]);
    let before_unstage = fs::read(&path).expect("working bytes before stale unstage");
    let stale_unstage = fixture
        .mutate(
            "63",
            "gitManager.unstagePartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1],
                "baseGeneration": unstage_diff["generation"]
            }),
        )
        .await
        .expect_err("stale unstage selection");
    assert_eq!(stale_unstage["code"], "stale-selection");
    assert_eq!(index_content(&fixture.repository_path), "base\n");
    assert_eq!(
        fs::read(&path).expect("bytes after stale unstage"),
        before_unstage
    );

    let discard_diff = fixture
        .read(
            "64",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
            }),
        )
        .await
        .expect("discard source diff");
    fs::write(&path, "base\none\ntwo\nthree\nfour\n").expect("rewrite after discard selection");
    let before_discard = fs::read(&path).expect("bytes before stale discard");
    let stale_discard = fixture
        .mutate(
            "65",
            "gitManager.discardPartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1],
                "baseGeneration": discard_diff["generation"]
            }),
        )
        .await
        .expect_err("stale discard selection");
    assert_eq!(stale_discard["code"], "stale-selection");
    assert_eq!(
        fs::read(&path).expect("bytes after stale discard"),
        before_discard
    );
    assert_eq!(index_content(&fixture.repository_path), "base\n");
}

#[tokio::test]
async fn untracked_partial_selection_round_trips_through_intent_to_add() {
    for prefix_config in ["diff.mnemonicPrefix", "diff.noprefix"] {
        let fixture = Fixture::new().await;
        git(
            &fixture.repository_path,
            &["config", "diff.mnemonicPrefix", "false"],
        );
        git(
            &fixture.repository_path,
            &["config", "diff.noprefix", "false"],
        );
        git(&fixture.repository_path, &["config", prefix_config, "true"]);
        let path = fixture.repository_path.join("new.txt");
        fs::write(&path, "one\ntwo\nthree\nfour\n").expect("untracked content");

        let unstaged = fixture
            .read(
                "70",
                "gitManager.getDiff",
                json!({
                    "cwd": fixture.repository_path,
                    "source": { "_tag": "working-tree", "path": "new.txt", "staged": false }
                }),
            )
            .await
            .expect("untracked diff");
        fixture
            .mutate(
                "71",
                "gitManager.stagePartial",
                json!({
                    "cwd": fixture.repository_path,
                    "projectId": "project-1",
                    "path": "new.txt",
                    "selectedLines": [0, 2],
                    "baseGeneration": unstaged["generation"]
                }),
            )
            .await
            .expect("partial stage of untracked file");
        assert_eq!(
            index_file_content(&fixture.repository_path, "new.txt"),
            "one\nthree\n"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("untracked working content"),
            "one\ntwo\nthree\nfour\n"
        );

        let staged = fixture
            .read(
                "72",
                "gitManager.getDiff",
                json!({
                    "cwd": fixture.repository_path,
                    "source": { "_tag": "working-tree", "path": "new.txt", "staged": true }
                }),
            )
            .await
            .expect("staged new-file diff");
        fixture
            .mutate(
                "73",
                "gitManager.unstagePartial",
                json!({
                    "cwd": fixture.repository_path,
                    "projectId": "project-1",
                    "path": "new.txt",
                    "selectedLines": [0],
                    "baseGeneration": staged["generation"]
                }),
            )
            .await
            .expect("partial unstage of new file");
        assert_eq!(
            index_file_content(&fixture.repository_path, "new.txt"),
            "three\n"
        );

        let index_before_discard = index_file_content(&fixture.repository_path, "new.txt");
        let unstaged = fixture
            .read(
                "74",
                "gitManager.getDiff",
                json!({
                    "cwd": fixture.repository_path,
                    "source": { "_tag": "working-tree", "path": "new.txt", "staged": false }
                }),
            )
            .await
            .expect("remaining new-file diff");
        fixture
            .mutate(
                "75",
                "gitManager.discardPartial",
                json!({
                    "cwd": fixture.repository_path,
                    "projectId": "project-1",
                    "path": "new.txt",
                    "selectedLines": [1],
                    "baseGeneration": unstaged["generation"]
                }),
            )
            .await
            .expect("partial discard of new file");
        assert_eq!(
            fs::read_to_string(&path).expect("working content after discard"),
            "one\nthree\nfour\n"
        );
        assert_eq!(
            index_file_content(&fixture.repository_path, "new.txt"),
            index_before_discard
        );
    }
}

#[tokio::test]
async fn partial_operations_preserve_no_trailing_newline_markers() {
    let fixture = Fixture::new().await;
    let path = fixture.repository_path.join("tracked.txt");
    fs::write(&path, b"").expect("empty tracked file");
    git(&fixture.repository_path, &["add", "tracked.txt"]);
    git(
        &fixture.repository_path,
        &["commit", "-q", "-m", "empty tracked file"],
    );
    fs::write(&path, b"one\ntwo").expect("working content without trailing newline");

    let unstaged = fixture
        .read(
            "80",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
            }),
        )
        .await
        .expect("no-newline unstaged diff");
    fixture
        .mutate(
            "81",
            "gitManager.stagePartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1],
                "baseGeneration": unstaged["generation"]
            }),
        )
        .await
        .expect("stage no-newline line");
    assert_eq!(index_content(&fixture.repository_path).as_bytes(), b"two");
    assert_eq!(
        fs::read(&path).expect("working bytes after stage"),
        b"one\ntwo"
    );

    let staged = fixture
        .read(
            "82",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": true }
            }),
        )
        .await
        .expect("no-newline staged diff");
    let before_unstage = fs::read(&path).expect("working bytes before no-newline unstage");
    fixture
        .mutate(
            "83",
            "gitManager.unstagePartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [0],
                "baseGeneration": staged["generation"]
            }),
        )
        .await
        .expect("unstage no-newline line");
    assert_eq!(index_content(&fixture.repository_path), "");
    assert_eq!(
        fs::read(&path).expect("working bytes after unstage"),
        before_unstage
    );

    let unstaged = fixture
        .read(
            "84",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
            }),
        )
        .await
        .expect("no-newline discard diff");
    fixture
        .mutate(
            "85",
            "gitManager.discardPartial",
            json!({
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "path": "tracked.txt",
                "selectedLines": [1],
                "baseGeneration": unstaged["generation"]
            }),
        )
        .await
        .expect("discard no-newline line");
    assert_eq!(
        fs::read(&path).expect("working bytes after discard"),
        b"one\n"
    );
    assert_eq!(index_content(&fixture.repository_path), "");
}

#[tokio::test]
async fn stash_listing_and_diff_use_the_full_native_list_and_stable_sha() {
    let fixture = Fixture::new().await;
    let linked = fixture._root.path().join("stash-linked");
    git(
        &fixture.repository_path,
        &["worktree", "add", "-q", "-b", "stash-linked", path(&linked)],
    );
    fs::write(linked.join("tracked.txt"), "first stash\n").expect("first stash content");
    git(
        &linked,
        &["stash", "push", "-q", "-m", "agent-created stash"],
    );
    let first_sha = String::from_utf8(
        git_output(&fixture.repository_path, &["rev-parse", "refs/stash"]).stdout,
    )
    .expect("UTF-8 stash sha")
    .trim()
    .to_owned();
    fs::write(linked.join("tracked.txt"), "second stash\n").expect("second stash content");
    git(
        &linked,
        &["stash", "push", "-q", "-m", "user-created stash"],
    );

    let stashes = fixture
        .read(
            "30",
            "gitManager.getStashes",
            json!({ "cwd": fixture.repository_path }),
        )
        .await
        .expect("stash list");

    let stashes = stashes.as_array().expect("stash array");
    assert_eq!(stashes.len(), 2);
    assert_eq!(stashes[0]["message"], "On stash-linked: user-created stash");
    assert!(stashes.iter().any(|stash| stash["sha"] == first_sha));
    assert!(stashes.iter().all(|stash| stash["files"].is_array()));

    let diff = fixture
        .read(
            "31",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "stash", "sha": first_sha, "path": "tracked.txt" }
            }),
        )
        .await
        .expect("stable-SHA stash diff");
    assert_eq!(diff["_tag"], "patch");
    assert!(
        diff["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("first stash"))
    );

    git(&fixture.repository_path, &["stash", "drop", "stash@{1}"]);
    let missing = fixture
        .read(
            "32",
            "gitManager.getDiff",
            json!({
                "cwd": fixture.repository_path,
                "source": { "_tag": "stash", "sha": first_sha, "path": "tracked.txt" }
            }),
        )
        .await
        .expect_err("dropped stash fails structurally");
    assert_eq!(missing["code"], "stash-not-found");
}

#[tokio::test]
async fn merge_preview_reports_clean_ahead_and_behind_state() {
    let fixture = Fixture::new().await;
    git(&fixture.repository_path, &["switch", "-q", "-c", "feature"]);
    fs::write(fixture.repository_path.join("feature.txt"), "feature\n").expect("feature file");
    git(&fixture.repository_path, &["add", "feature.txt"]);
    git(&fixture.repository_path, &["commit", "-q", "-m", "feature"]);
    git(&fixture.repository_path, &["switch", "-q", "main"]);

    let preview = fixture
        .read(
            "33",
            "gitManager.previewMerge",
            json!({ "cwd": fixture.repository_path, "source": "feature" }),
        )
        .await
        .expect("merge preview");

    assert_eq!(preview["_tag"], "clean");
    assert_eq!(preview["source"], "feature");
    assert_eq!(preview["current"], "main");
    assert_eq!(preview["ahead"], 1);
    assert_eq!(preview["behind"], 0);
}

#[tokio::test]
async fn merge_preview_compares_against_a_named_target_branch() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["branch", "target"]);
    git(&cwd, &["switch", "-q", "-c", "feature"]);
    fs::write(cwd.join("feature.txt"), "feature\n").expect("feature file");
    git(&cwd, &["add", "feature.txt"]);
    git(&cwd, &["commit", "-q", "-m", "feature"]);
    git(&cwd, &["switch", "-q", "main"]);
    // A tag named like the target must not shadow the branch.
    git(&cwd, &["tag", "target", "feature"]);

    let preview = fixture
        .read(
            "34",
            "gitManager.previewMerge",
            json!({ "cwd": cwd, "source": "refs/heads/feature", "target": "target" }),
        )
        .await
        .expect("merge preview");

    assert_eq!(preview["_tag"], "clean");
    assert_eq!(preview["current"], "target");
    assert_eq!(preview["ahead"], 1);
}

#[tokio::test]
async fn merge_preview_rejects_a_missing_target_branch() {
    let fixture = Fixture::new().await;

    let failure = fixture
        .read(
            "36",
            "gitManager.previewMerge",
            json!({ "cwd": fixture.repository_path, "source": "main", "target": "gone" }),
        )
        .await
        .expect_err("a target branch that does not exist fails structurally");

    assert_eq!(failure["code"], "local-branch-not-found");
}

#[tokio::test]
async fn merge_preview_target_is_a_literal_branch_name_not_a_revision_expression() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    fs::write(cwd.join("tracked.txt"), "second\n").expect("second commit content");
    git(&cwd, &["commit", "-qam", "second"]);

    for (index, target) in ["main~1", "main^", "main@{1}"].into_iter().enumerate() {
        let failure = fixture
            .read(
                &format!("4{index}"),
                "gitManager.previewMerge",
                json!({ "cwd": cwd, "source": "main", "target": target }),
            )
            .await
            .expect_err("a revision expression is not a local branch");

        assert_eq!(failure["code"], "local-branch-not-found", "{target}");
    }
}

#[tokio::test]
async fn refs_detect_an_external_merge_inside_a_linked_worktree() {
    let fixture = Fixture::new().await;
    let linked = fixture._root.path().join("linked");
    git(&fixture.repository_path, &["switch", "-q", "-c", "feature"]);
    fs::write(fixture.repository_path.join("tracked.txt"), "feature\n").expect("feature content");
    git(&fixture.repository_path, &["commit", "-qam", "feature"]);
    git(&fixture.repository_path, &["switch", "-q", "main"]);
    git(&fixture.repository_path, &["branch", "linked"]);
    git(
        &fixture.repository_path,
        &["worktree", "add", "-q", path(&linked), "linked"],
    );
    configure_identity(&linked);
    fs::write(linked.join("tracked.txt"), "linked\n").expect("linked content");
    git(&linked, &["commit", "-qam", "linked"]);
    assert!(!git_output(&linked, &["merge", "feature"]).status.success());

    let refs = fixture
        .read("35", "gitManager.getRefs", json!({ "cwd": linked }))
        .await
        .expect("refs snapshot");

    assert_eq!(refs["inProgressOperation"]["kind"], "merge");
}

#[tokio::test]
async fn git_manager_signal_generation_bumps_after_an_external_commit() {
    let fixture = Fixture::new().await;
    let linked = fixture._root.path().join("signal-linked");
    git(
        &fixture.repository_path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "signal-linked",
            path(&linked),
        ],
    );
    configure_identity(&linked);
    let cancellation = CancellationToken::new();
    let mut stream = fixture.services.git_manager_signal_stream(
        rpc_request("34", "subscribeGitManagerSignal", json!({ "cwd": linked })),
        cancellation.clone(),
    );
    let initial = next_event(&mut stream).await;
    let first = next_generation_after(&mut stream, initial["generation"].as_u64().unwrap()).await;

    fs::write(linked.join("external.txt"), "external\n").expect("external file");
    git(&linked, &["add", "external.txt"]);
    git(&linked, &["commit", "-q", "-m", "external"]);
    let second = next_generation_after(&mut stream, first).await;

    assert!(second > first);
    cancellation.cancel();
}

#[tokio::test]
async fn git_manager_signal_external_changes_use_the_default_fetch_interval() {
    let fixture = Fixture::with_fetch_interval(Duration::from_secs(180)).await;
    let cwd = &fixture.repository_path;
    let cancellation = CancellationToken::new();
    let mut stream = fixture.services.git_manager_signal_stream(
        rpc_request("95", "subscribeGitManagerSignal", json!({ "cwd": cwd })),
        cancellation.clone(),
    );
    let initial = next_event(&mut stream).await;
    let mut generation = initial["generation"].as_u64().expect("initial generation");
    if generation == 0 {
        generation = next_generation_after(&mut stream, generation).await;
    }
    struct ExternalChange {
        name: &'static str,
        apply: fn(&Fixture),
    }
    for change in [
        ExternalChange {
            name: "commit",
            apply: |fixture| {
                fs::write(
                    fixture.repository_path.join("tracked.txt"),
                    "external commit\n",
                )
                .expect("commit content");
                git(&fixture.repository_path, &["commit", "-qam", "external"]);
            },
        },
        ExternalChange {
            name: "stash",
            apply: |fixture| {
                fs::write(
                    fixture.repository_path.join("tracked.txt"),
                    "external stash\n",
                )
                .expect("stash content");
                git(
                    &fixture.repository_path,
                    &["stash", "push", "-q", "-m", "external"],
                );
            },
        },
        ExternalChange {
            name: "tag",
            apply: |fixture| git(&fixture.repository_path, &["tag", "external-tag"]),
        },
        ExternalChange {
            name: "update-ref",
            apply: |fixture| {
                git(
                    &fixture.repository_path,
                    &["update-ref", "refs/heads/external-branch", "HEAD"],
                )
            },
        },
        ExternalChange {
            name: "fetch",
            apply: |fixture| {
                publish_remote_change(&fixture.remote_path, fixture._root.path());
                git(&fixture.repository_path, &["fetch", "-q", "origin"]);
            },
        },
    ] {
        (change.apply)(&fixture);
        generation = timeout(
            Duration::from_secs(5),
            next_generation_after(&mut stream, generation),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "external {} waited for the automatic fetch tick",
                change.name
            )
        });
    }
    cancellation.cancel();
}

async fn initialized_signal(
    fixture: &Fixture,
    cwd: &Path,
) -> (
    mpsc::Receiver<Result<Vec<Value>, Value>>,
    CancellationToken,
    u64,
) {
    let cancellation = CancellationToken::new();
    let mut stream = fixture.services.git_manager_signal_stream(
        rpc_request("96", "subscribeGitManagerSignal", json!({ "cwd": cwd })),
        cancellation.clone(),
    );
    let mut generation = next_event(&mut stream).await["generation"]
        .as_u64()
        .expect("generation");
    if generation == 0 {
        generation = next_generation_after(&mut stream, generation).await;
    }
    (stream, cancellation, generation)
}

#[tokio::test]
async fn signal_tracks_a_repository_whose_head_is_a_clone_placeholder() {
    let fixture = Fixture::with_fetch_interval(Duration::from_secs(180)).await;
    let cwd = &fixture.repository_path;
    // An interrupted clone leaves HEAD on Git's placeholder, which names no valid ref.
    fs::write(cwd.join(".git/HEAD"), "ref: refs/heads/.invalid\n").expect("placeholder HEAD");
    let (mut stream, cancellation, generation) =
        timeout(Duration::from_secs(5), initialized_signal(&fixture, cwd))
            .await
            .expect("a placeholder HEAD still yields a signature");
    // Repairing HEAD is an ordinary ref change.
    fs::write(cwd.join(".git/HEAD"), "ref: refs/heads/main\n").expect("repaired HEAD");
    timeout(
        Duration::from_secs(3),
        next_generation_after(&mut stream, generation),
    )
    .await
    .expect("repairing a placeholder HEAD bumps the signal");
    cancellation.cancel();
}

#[tokio::test]
async fn main_checkout_stash_push_pop_and_drop_refresh_after_idle() {
    for existing_stash in [false, true] {
        let fixture = Fixture::with_fetch_interval(Duration::from_secs(180)).await;
        let cwd = &fixture.repository_path;
        if existing_stash {
            fs::write(cwd.join("tracked.txt"), "older stash\n").expect("older stash content");
            git(cwd, &["stash", "push", "-q", "-m", "older"]);
        }
        let (mut stream, cancellation, generation) = initialized_signal(&fixture, cwd).await;
        let mut generation = wait_until_signal_idle(&mut stream, generation).await;
        for (operation, expected_count) in [("push", 1), ("pop", 0), ("push", 1), ("drop", 0)] {
            if operation == "push" {
                fs::write(cwd.join("tracked.txt"), "external stash change\n")
                    .expect("stash content");
            }
            if operation == "drop" && existing_stash {
                git(cwd, &["stash", "drop", "-q", "stash@{1}"]);
            } else {
                git(cwd, &["stash", operation, "-q"]);
            }
            generation = timeout(
                Duration::from_secs(3),
                next_generation_after(&mut stream, generation),
            )
            .await
            .unwrap_or_else(|_| {
                panic!("main checkout stash {operation} did not refresh after idle")
            });
            let stashes = fixture
                .read("97", "gitManager.getStashes", json!({ "cwd": cwd }))
                .await
                .expect("refreshed stash list");
            assert_eq!(
                stashes.as_array().expect("stash entries").len(),
                expected_count + usize::from(existing_stash)
            );
            generation = wait_until_signal_idle(&mut stream, generation).await;
        }
        cancellation.cancel();
    }
}

#[tokio::test]
async fn git_manager_signal_detects_non_top_stash_drop_and_pop_in_both_ref_stores() {
    for ref_format in ["files", "reftable"] {
        for operation in ["drop", "pop"] {
            let fixture = if ref_format == "reftable" {
                let Some(fixture) = Fixture::new_reftable(Duration::from_secs(180)).await else {
                    continue;
                };
                fixture
            } else {
                Fixture::with_fetch_interval(Duration::from_secs(180)).await
            };
            let cwd = &fixture.repository_path;
            for index in 0..3 {
                fs::write(cwd.join("tracked.txt"), format!("stash {index}\n"))
                    .expect("stash content");
                git(
                    cwd,
                    &["stash", "push", "-q", "-m", &format!("stash {index}")],
                );
            }
            let (mut stream, cancellation, generation) = initialized_signal(&fixture, cwd).await;
            let tip = git_stdout(cwd, &["rev-parse", "refs/stash"]);
            git(cwd, &["stash", operation, "-q", "stash@{1}"]);
            assert_eq!(
                git_stdout(cwd, &["rev-parse", "refs/stash"]),
                tip,
                "non-top removal preserves the ref tip"
            );
            timeout(
                Duration::from_secs(5),
                next_generation_after(&mut stream, generation),
            )
            .await
            .unwrap_or_else(|_| {
                panic!("{ref_format} stash {operation} did not invalidate the signal")
            });
            cancellation.cancel();
        }
    }
}

#[tokio::test]
async fn git_manager_signal_observes_a_sibling_worktree_head_switch() {
    assert_sibling_worktree_head_signal(
        Fixture::with_fetch_interval(Duration::from_secs(180)).await,
    )
    .await;
}

#[tokio::test]
async fn git_manager_signal_observes_a_reftable_sibling_worktree_head_switch() {
    let Some(fixture) = Fixture::new_reftable(Duration::from_secs(180)).await else {
        return;
    };
    assert_sibling_worktree_head_signal(fixture).await;
}

async fn assert_sibling_worktree_head_signal(fixture: Fixture) {
    let observed = fixture._root.path().join("observed-worktree");
    let sibling = fixture._root.path().join("sibling-worktree");
    git(
        &fixture.repository_path,
        &["worktree", "add", "-q", "-b", "observed", path(&observed)],
    );
    git(
        &fixture.repository_path,
        &["worktree", "add", "-q", "-b", "sibling", path(&sibling)],
    );
    git(&fixture.repository_path, &["branch", "alternate", "HEAD"]);
    let (mut stream, cancellation, generation) = initialized_signal(&fixture, &observed).await;
    git(&sibling, &["switch", "-q", "alternate"]);
    timeout(
        Duration::from_secs(5),
        next_generation_after(&mut stream, generation),
    )
    .await
    .expect("sibling HEAD change must refresh worktree occupancy");
    cancellation.cancel();
}

#[tokio::test]
async fn git_manager_signal_refreshes_during_a_continuous_working_tree_writer() {
    let fixture = Fixture::with_fetch_interval(Duration::from_secs(180)).await;
    let (mut stream, cancellation, generation) =
        initialized_signal(&fixture, &fixture.repository_path).await;
    let writer_stop = CancellationToken::new();
    let writer_cancellation = writer_stop.clone();
    let file = fixture.repository_path.join("build-output.txt");
    let (started, ready) = tokio::sync::oneshot::channel();
    let writer = tokio::spawn(async move {
        tokio::fs::write(&file, b"starting")
            .await
            .expect("writer starts");
        let _ = started.send(());
        let mut interval = tokio::time::interval(Duration::from_millis(25));
        let mut sequence = 0;
        loop {
            tokio::select! {
                biased;
                () = writer_cancellation.cancelled() => break,
                _ = interval.tick() => {
                    sequence += 1;
                    tokio::fs::write(&file, sequence.to_string()).await.expect("continuous write");
                }
            }
        }
    });
    ready.await.expect("writer ready");
    git(&fixture.repository_path, &["tag", "during-build"]);
    let observed = timeout(
        Duration::from_secs(2),
        next_generation_after(&mut stream, generation),
    )
    .await;
    writer_stop.cancel();
    writer.await.expect("writer joins");
    cancellation.cancel();
    observed.expect("continuous working-tree writes must not defer ref refresh until they stop");
}

#[tokio::test]
async fn retained_history_decorations_match_git_log_before_and_after_external_ref_changes() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    git(
        cwd,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(cwd, &["tag", "v1"]);
    git(cwd, &["branch", "feature"]);
    let old_tip = git_stdout(cwd, &["rev-parse", "HEAD"]);
    let initial = fixture
        .read(
            "98",
            "gitManager.getCommits",
            json!({ "cwd": cwd, "limit": 100 }),
        )
        .await
        .expect("history page");
    let expected = git_stdout(
        cwd,
        &["log", "--decorate=short", "-1", "--format=%D", &old_tip],
    );
    assert_eq!(
        initial["commits"][0]["decorations"],
        json!(expected.split(", ").collect::<Vec<_>>())
    );
    assert!(expected.contains("origin/HEAD"));
    fs::write(cwd.join("new-commit.txt"), "external\n").expect("new file");
    git(cwd, &["add", "new-commit.txt"]);
    git(cwd, &["commit", "-q", "-m", "external"]);
    git(cwd, &["branch", "-m", "feature", "renamed"]);
    git(cwd, &["tag", "-d", "v1"]);
    git(cwd, &["tag", "v2", &old_tip]);
    let retained = fixture
        .read(
            "99",
            "gitManager.getCommits",
            json!({ "cwd": cwd, "pinnedTips": [old_tip], "offset": 0, "limit": 100 }),
        )
        .await
        .expect("retained pinned page");
    let expected = git_stdout(
        cwd,
        &["log", "--decorate=short", "-1", "--format=%D", &old_tip],
    );
    assert_eq!(retained["commits"][0]["sha"], old_tip);
    assert_eq!(
        retained["commits"][0]["decorations"],
        json!(expected.split(", ").collect::<Vec<_>>())
    );
    assert!(!expected.contains("HEAD ->"));
    assert!(expected.contains("origin/HEAD"));
}

#[tokio::test]
async fn git_manager_signal_observes_reftable_ref_updates() {
    let Some(fixture) = Fixture::new_reftable(Duration::from_secs(180)).await else {
        return;
    };
    let (mut stream, cancellation, generation) =
        initialized_signal(&fixture, &fixture.repository_path).await;
    git(&fixture.repository_path, &["tag", "external-reftable"]);
    timeout(
        Duration::from_secs(5),
        next_generation_after(&mut stream, generation),
    )
    .await
    .expect("reftable tag must refresh");
    cancellation.cancel();
}

#[tokio::test]
async fn fetch_stream_orders_started_output_and_finished_events() {
    let fixture = Fixture::new().await;
    publish_remote_change(&fixture.remote_path, fixture._root.path());

    let events = collect_events(fixture.operation(
        "1",
        json!({
            "_tag": "fetch",
            "cwd": fixture.repository_path,
            "projectId": "project-1",
            "remote": "origin"
        }),
    ))
    .await;

    let kinds = events
        .iter()
        .map(|event| event["_tag"].as_str().expect("event tag"))
        .collect::<Vec<_>>();
    assert_eq!(kinds.first(), Some(&"started"));
    assert!(kinds[1..kinds.len() - 1].contains(&"output"), "{events:?}");
    assert_eq!(kinds.last(), Some(&"finished"));
    assert!(events.iter().all(|event| event["operation"] == "fetch"));
}

#[tokio::test]
async fn stale_missing_worktree_delete_emits_a_structured_blocked_failure_without_deleting() {
    let fixture = Fixture::new().await;
    let topic_path = fixture._root.path().join("topic-worktree");
    git(&fixture.repository_path, &["branch", "topic"]);
    git(
        &fixture.repository_path,
        &["worktree", "add", "-q", path(&topic_path), "topic"],
    );
    fs::remove_dir_all(&topic_path).expect("remove only the temporary worktree directory");

    let events = collect_events(fixture.operation(
        "2",
        json!({
            "_tag": "branch-delete",
            "cwd": fixture.repository_path,
            "projectId": "project-1",
            "name": "topic",
            "force": true,
            "deleteRemote": false
        }),
    ))
    .await;

    assert_eq!(event_kinds(&events), ["started", "failed"]);
    let failed = &events[1];
    assert_eq!(failed["code"], "worktree-checked-out");
    assert_eq!(failed["blocked"]["operation"], "delete-branch");
    assert_eq!(failed["blocked"]["code"], "worktree-checked-out");
    assert!(
        failed["message"]
            .as_str()
            .is_some_and(|message| message.contains("remove or prune the worktree first"))
    );
    assert!(
        !failed["message"]
            .as_str()
            .unwrap()
            .contains("cannot delete branch")
    );
    assert!(
        git_output(
            &fixture.repository_path,
            &["show-ref", "--verify", "refs/heads/topic"]
        )
        .status
        .success()
    );
}

#[tokio::test]
async fn remote_branch_checkout_keeps_local_and_remote_namespaces_distinct() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    let remote_tip = git_stdout(cwd, &["rev-parse", "HEAD"]);
    git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    git(
        cwd,
        &["commit", "-q", "--allow-empty", "-m", "local commit"],
    );
    git(cwd, &["branch", "origin/develop"]);
    let local_name = "refs/remotes/origin/develop";
    let shadow_worktree = fixture._root.path().join("shadow");
    git(
        cwd,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            local_name,
            path(&shadow_worktree),
        ],
    );
    let events = collect_events(fixture.operation(
        "9",
        json!({
            "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
            "name": "refs/remotes/origin/develop", "strategy": null
        }),
    ))
    .await;
    // Git rejects this ambiguous ref, but the unrelated local worktree must not block it.
    assert_eq!(events.last().unwrap()["_tag"], "failed", "{events:?}");
    assert!(events.last().unwrap()["blocked"].is_null(), "{events:?}");
    assert_eq!(git_stdout(cwd, &["branch", "--show-current"]), "main");
    git(cwd, &["worktree", "remove", path(&shadow_worktree)]);
    git(cwd, &["branch", "-D", local_name]);

    let events = collect_events(fixture.operation(
        "10",
        json!({
            "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
            "name": "refs/remotes/origin/develop", "strategy": null
        }),
    ))
    .await;

    assert_eq!(events.last().unwrap()["_tag"], "finished", "{events:?}");
    assert_eq!(
        git_stdout(cwd, &["branch", "--show-current"]).trim(),
        "develop"
    );
    assert_eq!(git_stdout(cwd, &["rev-parse", "HEAD"]), remote_tip);
    assert_eq!(
        git_stdout(cwd, &["rev-parse", "--symbolic-full-name", "@{upstream}"]).trim(),
        "refs/remotes/origin/develop"
    );
    let refs = fixture
        .read("11", "gitManager.getRefs", json!({ "cwd": cwd }))
        .await
        .expect("refreshed refs");
    assert_eq!(refs["headRef"], "develop");
    assert!(
        refs["localBranches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| branch["name"] == "develop" && branch["upstream"] == "origin/develop")
    );

    git(cwd, &["branch", local_name, "main"]);
    let events = collect_events(fixture.operation(
        "12",
        json!({
            "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
            "name": format!("refs/heads/{local_name}"), "strategy": null
        }),
    ))
    .await;
    assert_eq!(events.last().unwrap()["_tag"], "finished", "{events:?}");
    assert_eq!(
        git_stdout(cwd, &["branch", "--show-current"]).trim(),
        local_name
    );
    assert_eq!(
        git_stdout(cwd, &["rev-parse", "HEAD"]),
        git_stdout(cwd, &["rev-parse", "refs/heads/main"])
    );
}

#[tokio::test]
async fn remote_branch_checkout_preserves_existing_local_branches_and_reports_missing_refs() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    git(cwd, &["branch", "develop"]);
    let local_tip = git_stdout(cwd, &["rev-parse", "refs/heads/develop"]);

    for (name, code, message) in [
        (
            "refs/remotes/origin/develop",
            "branch-exists",
            "Select it in the local branches",
        ),
        (
            "refs/remotes/origin/missing",
            "remote-branch-not-found",
            "Fetch and choose",
        ),
        (
            "refs/heads/missing",
            "local-branch-not-found",
            "choose a branch again",
        ),
    ] {
        let events = collect_events(fixture.operation(
            "10",
            json!({
                "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
                "name": name, "strategy": null
            }),
        ))
        .await;
        let failed = events.last().unwrap();
        assert_eq!(failed["_tag"], "failed", "{events:?}");
        assert_eq!(failed["code"], code, "{events:?}");
        assert!(failed["message"].as_str().unwrap().contains(message));
        assert_eq!(
            git_stdout(cwd, &["branch", "--show-current"]).trim(),
            "main"
        );
        assert_eq!(
            git_stdout(cwd, &["rev-parse", "refs/heads/develop"]),
            local_tip
        );
    }
    assert!(
        !git_output(cwd, &["config", "--get", "branch.develop.remote"])
            .status
            .success()
    );
}

#[tokio::test]
async fn remote_branch_checkout_requires_an_explicit_choice_to_bring_local_changes() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    fs::write(cwd.join("tracked.txt"), "my unfinished work\n").unwrap();

    for (strategy, terminal) in [(Value::Null, "failed"), (json!("bring"), "finished")] {
        let events = collect_events(fixture.operation(
            "10",
            json!({
                "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
                "name": "refs/remotes/origin/develop", "strategy": strategy
            }),
        ))
        .await;
        assert_eq!(events.last().unwrap()["_tag"], terminal, "{events:?}");
        assert_eq!(
            fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
            "my unfinished work\n"
        );
        assert_eq!(
            git_stdout(cwd, &["branch", "--show-current"]).trim(),
            if terminal == "failed" {
                "main"
            } else {
                "develop"
            }
        );
    }
}

/// The two requests the Git Manager's **Leave my changes** sends, in order.
async fn leave_my_changes(fixture: &Fixture, target: &str) -> (Vec<Value>, Option<Vec<Value>>) {
    let cwd = &fixture.repository_path;
    let stashed = collect_events(fixture.operation(
        "50",
        json!({
            "_tag": "stash-push", "cwd": cwd, "projectId": "project-1",
            "message": "WIP before switching to origin/develop", "paths": [],
            "includeUntracked": true
        }),
    ))
    .await;
    if stashed.last().unwrap()["_tag"] != "finished" {
        return (stashed, None);
    }
    let switched = collect_events(fixture.operation(
        "51",
        json!({
            "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
            "name": target, "strategy": null
        }),
    ))
    .await;
    (stashed, Some(switched))
}

async fn stash_files(fixture: &Fixture) -> Vec<Vec<String>> {
    let stashes = fixture
        .read(
            "52",
            "gitManager.getStashes",
            json!({ "cwd": fixture.repository_path }),
        )
        .await
        .expect("stash list");
    stashes
        .as_array()
        .expect("stash array")
        .iter()
        .map(|stash| {
            stash["files"]
                .as_array()
                .expect("stash files")
                .iter()
                .map(|file| file["path"].as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn leave_my_changes_stashes_untracked_files_then_switches_to_a_remote_only_branch() {
    for with_tracked_edit in [false, true] {
        let fixture = Fixture::new().await;
        let cwd = &fixture.repository_path;
        fs::write(cwd.join(".gitignore"), "*.log\n").unwrap();
        git(cwd, &["add", ".gitignore"]);
        git(cwd, &["commit", "-q", "-m", "ignore logs"]);
        git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
        assert!(
            !git_output(cwd, &["show-ref", "--verify", "refs/heads/develop"])
                .status
                .success()
        );
        fs::create_dir(cwd.join("docs")).unwrap();
        fs::write(cwd.join("docs/notes.md"), "my notes\n").unwrap();
        fs::write(cwd.join("build.log"), "ignored output\n").unwrap();
        if with_tracked_edit {
            fs::write(cwd.join("tracked.txt"), "my unfinished work\n").unwrap();
        }

        let (stashed, switched) = leave_my_changes(&fixture, "refs/remotes/origin/develop").await;

        assert_eq!(stashed.last().unwrap()["_tag"], "finished", "{stashed:?}");
        let switched = switched.expect("checkout follows a successful stash");
        assert_eq!(switched.last().unwrap()["_tag"], "finished", "{switched:?}");
        assert_eq!(git_stdout(cwd, &["branch", "--show-current"]), "develop");
        assert!(!cwd.join("docs/notes.md").exists());
        assert_eq!(
            fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
            "base\n"
        );
        // Ignored files are not the user's pending work and stay where they are.
        assert_eq!(
            fs::read_to_string(cwd.join("build.log")).unwrap(),
            "ignored output\n"
        );
        let mut expected = vec!["docs/notes.md".to_owned()];
        if with_tracked_edit {
            expected.push("tracked.txt".to_owned());
        }
        let mut listed = stash_files(&fixture).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        listed[0].sort();
        assert_eq!(listed[0], expected);

        let applied = collect_events(fixture.operation(
            "53",
            json!({
                "_tag": "stash-apply", "cwd": cwd, "projectId": "project-1", "index": 0
            }),
        ))
        .await;
        assert_eq!(applied.last().unwrap()["_tag"], "finished", "{applied:?}");
        assert_eq!(
            fs::read_to_string(cwd.join("docs/notes.md")).unwrap(),
            "my notes\n"
        );
        if with_tracked_edit {
            assert_eq!(
                fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
                "my unfinished work\n"
            );
        }
    }
}

#[tokio::test]
async fn leave_my_changes_keeps_an_untracked_file_that_collides_with_the_target_in_the_stash() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    git(cwd, &["switch", "-q", "-c", "remote-source"]);
    fs::create_dir(cwd.join("docs")).unwrap();
    fs::write(cwd.join("docs/notes.md"), "develop notes\n").unwrap();
    git(cwd, &["add", "docs/notes.md"]);
    git(cwd, &["commit", "-q", "-m", "track notes"]);
    git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    git(cwd, &["switch", "-q", "main"]);
    git(cwd, &["branch", "-q", "-D", "remote-source"]);
    fs::create_dir_all(cwd.join("docs")).unwrap();
    fs::write(cwd.join("docs/notes.md"), "my notes\n").unwrap();

    let (stashed, switched) = leave_my_changes(&fixture, "refs/remotes/origin/develop").await;

    assert_eq!(stashed.last().unwrap()["_tag"], "finished", "{stashed:?}");
    let switched = switched.expect("checkout follows a successful stash");
    assert_eq!(switched.last().unwrap()["_tag"], "finished", "{switched:?}");
    assert_eq!(git_stdout(cwd, &["branch", "--show-current"]), "develop");
    assert_eq!(
        fs::read_to_string(cwd.join("docs/notes.md")).unwrap(),
        "develop notes\n"
    );

    let popped = collect_events(fixture.operation(
        "54",
        json!({
            "_tag": "stash-pop", "cwd": cwd, "projectId": "project-1", "index": 0
        }),
    ))
    .await;
    let failed = popped.last().unwrap();
    assert_eq!(failed["_tag"], "failed", "{popped:?}");
    assert_eq!(failed["code"], "stash-untracked-collision", "{popped:?}");
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .contains("The stash was kept"),
        "{popped:?}"
    );
    // Git refuses to overwrite the tracked file and keeps the stash entry intact.
    assert_eq!(
        fs::read_to_string(cwd.join("docs/notes.md")).unwrap(),
        "develop notes\n"
    );
    assert_eq!(stash_files(&fixture).await, vec![vec!["docs/notes.md"]]);
    assert_eq!(
        git_stdout(cwd, &["show", "stash@{0}^3:docs/notes.md"]),
        "my notes"
    );
}

#[tokio::test]
async fn leave_my_changes_fails_before_switching_when_git_cannot_stash_the_dirty_state() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    let submodule = fixture._root.path().join("submodule-source");
    fs::create_dir(&submodule).unwrap();
    git(&submodule, &["init", "-q", "-b", "main"]);
    configure_identity(&submodule);
    fs::write(submodule.join("module.txt"), "module\n").unwrap();
    git(&submodule, &["add", "module.txt"]);
    git(&submodule, &["commit", "-q", "-m", "module"]);
    git(
        cwd,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            path(&submodule),
            "module",
        ],
    );
    git(cwd, &["commit", "-q", "-m", "add submodule"]);
    git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    // Untracked content inside a submodule makes the checkout dirty, but no
    // stash can hold it.
    fs::write(cwd.join("module/inside.txt"), "inside\n").unwrap();

    let (stashed, switched) = leave_my_changes(&fixture, "refs/remotes/origin/develop").await;

    let failed = stashed.last().unwrap();
    assert_eq!(failed["_tag"], "failed", "{stashed:?}");
    assert_eq!(failed["code"], "nothing-to-stash", "{stashed:?}");
    assert!(switched.is_none());
    assert_eq!(git_stdout(cwd, &["branch", "--show-current"]), "main");
    assert_eq!(
        fs::read_to_string(cwd.join("module/inside.txt")).unwrap(),
        "inside\n"
    );
    assert!(stash_files(&fixture).await.is_empty());
}

#[tokio::test]
async fn remote_branch_checkout_bring_preserves_overwrite_and_operation_guards() {
    let fixture = Fixture::new().await;
    let cwd = &fixture.repository_path;
    git(cwd, &["switch", "-q", "-c", "remote-source"]);
    fs::write(cwd.join("tracked.txt"), "remote work\n").unwrap();
    git(cwd, &["commit", "-qam", "remote change"]);
    git(cwd, &["push", "-q", "origin", "HEAD:refs/heads/develop"]);
    git(cwd, &["switch", "-q", "main"]);
    let occupied = fixture._root.path().join("occupied");
    git(
        cwd,
        &["worktree", "add", "-q", "-b", "occupied", path(&occupied)],
    );
    fs::write(cwd.join("tracked.txt"), "my unfinished work\n").unwrap();

    for (name, code) in [
        ("refs/remotes/origin/develop", "local-changes-overwritten"),
        ("refs/heads/occupied", "worktree-checked-out"),
    ] {
        let events = collect_events(fixture.operation(
            "10",
            json!({
                "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
                "name": name, "strategy": "bring"
            }),
        ))
        .await;
        assert_eq!(events.last().unwrap()["_tag"], "failed", "{events:?}");
        assert_eq!(events.last().unwrap()["code"], code, "{events:?}");
        assert_eq!(git_stdout(cwd, &["branch", "--show-current"]), "main");
        assert_eq!(
            fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
            "my unfinished work\n"
        );
    }
    assert!(
        !git_output(cwd, &["show-ref", "--verify", "refs/heads/develop"])
            .status
            .success()
    );

    git(cwd, &["commit", "-qam", "local change"]);
    assert!(
        !git_output(cwd, &["merge", "remote-source"])
            .status
            .success()
    );
    assert!(merge_in_progress(cwd));
    let conflicted_content = fs::read_to_string(cwd.join("tracked.txt")).unwrap();
    let events = collect_events(fixture.operation(
        "11",
        json!({
            "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
            "name": "refs/remotes/origin/develop", "strategy": "bring"
        }),
    ))
    .await;
    assert_eq!(events.last().unwrap()["_tag"], "failed", "{events:?}");
    assert_eq!(events.last().unwrap()["code"], "merge-in-progress");
    assert!(merge_in_progress(cwd));
    assert_eq!(git_stdout(cwd, &["branch", "--show-current"]), "main");
    assert_eq!(
        fs::read_to_string(cwd.join("tracked.txt")).unwrap(),
        conflicted_content
    );
}

#[tokio::test]
async fn branch_and_sync_operations_execute_through_the_streaming_adapter() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();

    for (id, payload) in [
        (
            "10",
            json!({
                "_tag": "branch-create", "cwd": cwd, "projectId": "project-1",
                "name": "topic", "startPoint": "main", "checkout": false
            }),
        ),
        (
            "11",
            json!({
                "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
                "name": "topic", "strategy": "bring"
            }),
        ),
        (
            "12",
            json!({
                "_tag": "branch-rename", "cwd": cwd, "projectId": "project-1",
                "name": "topic", "newName": "renamed"
            }),
        ),
        (
            "13",
            json!({
                "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
                "name": "main", "strategy": null
            }),
        ),
        (
            "14",
            json!({
                "_tag": "branch-delete", "cwd": cwd, "projectId": "project-1",
                "name": "renamed", "force": false, "deleteRemote": false
            }),
        ),
        (
            "15",
            json!({
                "_tag": "branch-create", "cwd": cwd, "projectId": "project-1",
                "name": "publishme", "startPoint": "main", "checkout": true
            }),
        ),
        (
            "16",
            json!({
                "_tag": "publish-branch", "cwd": cwd, "projectId": "project-1",
                "remote": "origin", "localBranch": "publishme", "remoteBranch": "published"
            }),
        ),
        (
            "17",
            json!({
                "_tag": "push", "cwd": cwd, "projectId": "project-1",
                "remote": "origin", "localBranch": "publishme", "remoteBranch": "published"
            }),
        ),
        (
            "18",
            json!({
                "_tag": "force-push", "cwd": cwd, "projectId": "project-1",
                "remote": "origin", "localBranch": "publishme", "remoteBranch": "published"
            }),
        ),
        (
            "19",
            json!({
                "_tag": "pull", "cwd": cwd, "projectId": "project-1", "remote": "origin"
            }),
        ),
        (
            "20",
            json!({
                "_tag": "branch-checkout", "cwd": cwd, "projectId": "project-1",
                "name": "main", "strategy": null
            }),
        ),
        (
            "21",
            json!({
                "_tag": "branch-delete", "cwd": cwd, "projectId": "project-1",
                "name": "publishme", "force": false, "deleteRemote": true
            }),
        ),
    ] {
        let events = collect_events(fixture.operation(id, payload)).await;
        assert_eq!(
            events.first().and_then(|event| event["_tag"].as_str()),
            Some("started")
        );
        assert_eq!(
            events.last().and_then(|event| event["_tag"].as_str()),
            Some("finished"),
            "operation {id} failed: {events:?}"
        );
    }

    assert!(
        !git_output(&cwd, &["show-ref", "--verify", "refs/heads/renamed"])
            .status
            .success()
    );
    assert!(
        !git_output(&cwd, &["show-ref", "--verify", "refs/heads/publishme"])
            .status
            .success()
    );
    assert!(
        !git_output(
            &cwd,
            &["ls-remote", "--exit-code", "origin", "refs/heads/published"]
        )
        .status
        .success()
    );
}

#[tokio::test]
async fn stash_and_merge_operations_execute_through_the_existing_mutation_path() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    fs::write(cwd.join("tracked.txt"), "saved change\n").expect("stash content");
    fs::write(cwd.join("untracked.txt"), "untracked\n").expect("untracked content");

    let pushed = collect_events(fixture.operation(
        "40",
        json!({
            "_tag": "stash-push", "cwd": cwd, "projectId": "project-1",
            "message": "save visible work", "paths": ["untracked.txt"]
        }),
    ))
    .await;
    assert_eq!(
        pushed.last().and_then(|event| event["_tag"].as_str()),
        Some("finished")
    );
    assert!(!cwd.join("untracked.txt").exists());

    let applied = collect_events(fixture.operation(
        "41",
        json!({
            "_tag": "stash-apply", "cwd": cwd, "projectId": "project-1", "index": 0
        }),
    ))
    .await;
    assert_eq!(
        applied.last().and_then(|event| event["_tag"].as_str()),
        Some("finished")
    );
    assert_eq!(
        fs::read_to_string(cwd.join("tracked.txt")).expect("applied tracked content"),
        "saved change\n"
    );
    git(&cwd, &["reset", "--hard", "-q", "HEAD"]);
    let _ = fs::remove_file(cwd.join("untracked.txt"));
    let dropped = collect_events(fixture.operation(
        "42",
        json!({
            "_tag": "stash-drop", "cwd": cwd, "projectId": "project-1", "index": 0
        }),
    ))
    .await;
    assert_eq!(
        dropped.last().and_then(|event| event["_tag"].as_str()),
        Some("finished")
    );

    git(&cwd, &["switch", "-q", "-c", "feature"]);
    fs::write(cwd.join("feature.txt"), "feature\n").expect("feature file");
    git(&cwd, &["add", "feature.txt"]);
    git(&cwd, &["commit", "-q", "-m", "feature"]);
    git(&cwd, &["switch", "-q", "main"]);
    let main_tip = git_stdout(&cwd, &["rev-parse", "main"]);
    let feature_tip = git_stdout(&cwd, &["rev-parse", "feature"]);
    let merged = collect_events(fixture.operation(
        "43",
        json!({
            "_tag": "merge", "cwd": cwd, "projectId": "project-1",
            "source": "feature", "noVerify": false
        }),
    ))
    .await;
    assert_eq!(
        merged.last().and_then(|event| event["_tag"].as_str()),
        Some("finished")
    );
    assert!(cwd.join("feature.txt").exists());
    assert_eq!(
        head_parents(&cwd),
        [main_tip.clone(), feature_tip],
        "a merge commit is recorded even when a fast-forward was possible"
    );
    assert!(!merge_in_progress(&cwd));

    let main_before_squash = git_stdout(&cwd, &["rev-parse", "main"]);
    git(&cwd, &["switch", "-q", "-c", "squash-source"]);
    fs::write(cwd.join("squash.txt"), "squash\n").expect("squash file");
    git(&cwd, &["add", "squash.txt"]);
    git(&cwd, &["commit", "-q", "-m", "squash source"]);
    git(&cwd, &["switch", "-q", "main"]);
    let squashed = collect_events(fixture.operation(
        "44",
        json!({
            "_tag": "squash-merge", "cwd": cwd, "projectId": "project-1",
            "source": "squash-source", "noVerify": false
        }),
    ))
    .await;
    assert_eq!(
        squashed.last().and_then(|event| event["_tag"].as_str()),
        Some("finished")
    );
    assert!(cwd.join("squash.txt").exists());
    assert_ne!(git_stdout(&cwd, &["rev-parse", "main"]), main_before_squash);
    assert_eq!(
        head_parents(&cwd),
        [main_before_squash],
        "a squash merge records one ordinary commit, not a merge commit"
    );

    fs::write(cwd.join("dirty.txt"), "dirty\n").expect("dirty file");
    let blocked = collect_events(fixture.operation(
        "45",
        json!({
            "_tag": "merge", "cwd": cwd, "projectId": "project-1",
            "source": "feature", "noVerify": false
        }),
    ))
    .await;
    assert_eq!(event_kinds(&blocked), ["started", "failed"]);
    assert_eq!(blocked[1]["code"], "dirty-working-tree");
}

#[tokio::test]
async fn merge_commit_mode_overrides_branch_merge_options_that_would_skip_the_commit() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();

    let configurations = [
        ("branch.main.mergeOptions", "--no-commit"),
        ("branch.main.mergeOptions", "--squash"),
        ("merge.ff", "only"),
    ];
    for (index, (key, value)) in configurations.into_iter().enumerate() {
        let source = format!("source-{index}");
        git(&cwd, &["config", key, value]);
        git(&cwd, &["switch", "-q", "-c", &source]);
        fs::write(cwd.join(format!("{source}.txt")), "change\n").expect("source file");
        git(&cwd, &["add", "."]);
        git(&cwd, &["commit", "-q", "-m", &source]);
        git(&cwd, &["switch", "-q", "main"]);
        let main_tip = git_stdout(&cwd, &["rev-parse", "main"]);
        let source_tip = git_stdout(&cwd, &["rev-parse", &source]);

        let events = collect_events(fixture.operation(
            &(470 + index).to_string(),
            json!({
                "_tag": "merge", "cwd": cwd, "projectId": "project-1",
                "source": source, "noVerify": false
            }),
        ))
        .await;

        assert_eq!(
            events.last().and_then(|event| event["_tag"].as_str()),
            Some("finished"),
            "merge with {key}={value}: {events:?}"
        );
        assert!(
            !merge_in_progress(&cwd),
            "no merge is left in progress with {key}={value}"
        );
        assert_eq!(
            head_parents(&cwd),
            [main_tip, source_tip],
            "a merge commit is recorded despite {key}={value}"
        );
    }
}

#[tokio::test]
async fn squash_merge_overrides_merge_configuration_that_rejects_a_squash() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();

    let configurations = [
        ("merge.ff", "false"),
        ("merge.ff", "only"),
        ("branch.main.mergeOptions", "--commit"),
    ];
    for (index, (key, value)) in configurations.into_iter().enumerate() {
        let source = format!("squash-source-{index}");
        git(&cwd, &["config", key, value]);
        git(&cwd, &["switch", "-q", "-c", &source]);
        fs::write(cwd.join(format!("{source}.txt")), "change\n").expect("source file");
        git(&cwd, &["add", "."]);
        git(&cwd, &["commit", "-q", "-m", &source]);
        git(&cwd, &["switch", "-q", "main"]);
        fs::write(cwd.join(format!("main-{index}.txt")), "diverged\n").expect("main file");
        git(&cwd, &["add", "."]);
        git(
            &cwd,
            &["commit", "-q", "-m", &format!("main diverges {index}")],
        );
        let main_tip = git_stdout(&cwd, &["rev-parse", "main"]);

        let events = collect_events(fixture.operation(
            &(480 + index).to_string(),
            json!({
                "_tag": "squash-merge", "cwd": cwd, "projectId": "project-1",
                "source": source, "noVerify": false
            }),
        ))
        .await;

        assert_eq!(
            events.last().and_then(|event| event["_tag"].as_str()),
            Some("finished"),
            "squash merge with {key}={value}: {events:?}"
        );
        assert!(cwd.join(format!("{source}.txt")).exists());
        assert_ne!(git_stdout(&cwd, &["rev-parse", "main"]), main_tip);
        assert_eq!(
            head_parents(&cwd),
            [main_tip],
            "a squash merge records one ordinary commit despite {key}={value}"
        );
        assert!(!merge_in_progress(&cwd));
    }
}

#[tokio::test]
async fn squash_merge_bypasses_commit_hooks_only_when_no_verify_is_requested() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let hooks = fixture._root.path().join("hooks");
    fs::create_dir(&hooks).expect("hooks directory");
    let hook = hooks.join("pre-commit");
    executable_fixture::write_executable(
        &hook,
        "#!/bin/sh\necho 'pre-commit hook blocked the commit' >&2\nexit 1\n",
    );
    git(
        &cwd,
        &[
            "config",
            "core.hooksPath",
            &hooks.to_string_lossy().replace('\\', "/"),
        ],
    );

    for (index, (no_verify, expected)) in [(true, "finished"), (false, "failed")]
        .into_iter()
        .enumerate()
    {
        let source = format!("hooked-source-{index}");
        git(&cwd, &["switch", "-q", "-c", &source]);
        fs::write(cwd.join(format!("{source}.txt")), "change\n").expect("source file");
        git(&cwd, &["add", "."]);
        git(&cwd, &["commit", "-q", "--no-verify", "-m", &source]);
        git(&cwd, &["switch", "-q", "main"]);
        let main_tip = git_stdout(&cwd, &["rev-parse", "main"]);

        let events = collect_events(fixture.operation(
            &(490 + index).to_string(),
            json!({
                "_tag": "squash-merge", "cwd": cwd, "projectId": "project-1",
                "source": source, "noVerify": no_verify
            }),
        ))
        .await;

        assert_eq!(
            events.last().and_then(|event| event["_tag"].as_str()),
            Some(expected),
            "squash merge with noVerify={no_verify}: {events:?}"
        );
        let committed = git_stdout(&cwd, &["rev-parse", "main"]) != main_tip;
        assert_eq!(
            committed, no_verify,
            "the squash commit is created only when the hook is bypassed"
        );
    }
}

#[tokio::test]
async fn remote_tags_read_lists_tags_on_the_remote_and_reports_an_unavailable_remote() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["tag", "v1.0.0"]);
    git(&cwd, &["push", "-q", "origin", "v1.0.0"]);
    git(&cwd, &["tag", "local-only"]);

    let tags = fixture
        .read(
            "60",
            "gitManager.getRemoteTags",
            json!({ "cwd": cwd, "remote": "origin" }),
        )
        .await
        .expect("remote tags");
    assert_eq!(tags["remote"], "origin");
    assert_eq!(tags["status"], "available");
    assert_eq!(tags["reason"], Value::Null);
    let names = tags["tags"]
        .as_array()
        .expect("tag list")
        .iter()
        .map(|tag| tag["name"].as_str().expect("tag name").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, ["v1.0.0"]);
    assert_eq!(
        tags["tags"][0]["targetSha"],
        json!(git_stdout(&cwd, &["rev-parse", "v1.0.0"]))
    );

    git(
        &cwd,
        &[
            "remote",
            "add",
            "elsewhere",
            "/nonexistent/bibcode-missing-remote.git",
        ],
    );
    let unavailable = fixture
        .read(
            "61",
            "gitManager.getRemoteTags",
            json!({ "cwd": cwd, "remote": "elsewhere" }),
        )
        .await
        .expect("an unavailable remote is a result, not an RPC failure");
    assert_eq!(unavailable["status"], "unavailable");
    assert!(
        unavailable["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "{unavailable}"
    );
    assert_eq!(unavailable["tags"], json!([]));
}

#[tokio::test]
async fn interrupting_a_remote_tags_read_stops_git_and_its_transport_helpers() {
    let mut read = StalledRemoteTagsRead::start("71").await;

    send_json(
        &mut read.socket,
        json!({ "_tag": "Interrupt", "requestId": "71" }),
    )
    .await;
    let interrupted = next_server_message(&mut read.socket).await;
    assert!(
        matches!(
            &interrupted,
            ServerMessage::Exit { request_id, exit: RpcExit::Failure { cause } }
                if request_id.as_str() == "71"
                    && cause == &vec![CauseItem::Interrupt { fiber_id: None }]
        ),
        "the read must end as interrupted, got {interrupted:?}"
    );
    wait_for_connection_close(&mut read.helper_connection, "interrupting the read").await;

    read.shutdown().await;
}

#[tokio::test]
async fn closing_the_socket_during_a_remote_tags_read_stops_git_and_its_transport_helpers() {
    let mut read = StalledRemoteTagsRead::start("72").await;

    read.socket.close(None).await.expect("close the WebSocket");
    wait_for_connection_close(&mut read.helper_connection, "closing the socket").await;

    read.shutdown().await;
}

#[tokio::test]
async fn push_with_tags_publishes_local_tags_atomically() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["push", "-q", "-u", "origin", "main"]);
    git(&cwd, &["tag", "v1"]);

    let pushed = collect_events(fixture.operation(
        "62",
        json!({
            "_tag": "push", "cwd": cwd, "projectId": "project-1",
            "remote": "origin", "localBranch": "main", "remoteBranch": "main", "pushTags": true
        }),
    ))
    .await;
    assert_eq!(
        pushed.last().and_then(|event| event["_tag"].as_str()),
        Some("finished"),
        "{pushed:?}"
    );
    assert!(git_stdout(&cwd, &["ls-remote", "--tags", "origin"]).contains("refs/tags/v1"));

    // A tag that already exists on the remote at another commit must reject the
    // whole push: the branch ref on the remote may not move.
    let base = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    git(&fixture.remote_path, &["update-ref", "refs/tags/v2", &base]);
    fs::write(cwd.join("next.txt"), "next\n").expect("next file");
    git(&cwd, &["add", "next.txt"]);
    git(&cwd, &["commit", "-q", "-m", "next"]);
    git(&cwd, &["tag", "v2"]);
    let rejected = collect_events(fixture.operation(
        "63",
        json!({
            "_tag": "push", "cwd": cwd, "projectId": "project-1",
            "remote": "origin", "localBranch": "main", "remoteBranch": "main", "pushTags": true
        }),
    ))
    .await;
    assert_eq!(
        rejected.last().and_then(|event| event["_tag"].as_str()),
        Some("failed"),
        "{rejected:?}"
    );
    assert_eq!(
        git_stdout(&fixture.remote_path, &["rev-parse", "refs/heads/main"]),
        base,
        "an atomic push with a rejected tag leaves the remote branch untouched"
    );

    let without_tags = collect_events(fixture.operation(
        "64",
        json!({
            "_tag": "push", "cwd": cwd, "projectId": "project-1",
            "remote": "origin", "localBranch": "main", "remoteBranch": "main"
        }),
    ))
    .await;
    assert_eq!(
        without_tags.last().and_then(|event| event["_tag"].as_str()),
        Some("finished"),
        "{without_tags:?}"
    );
    assert_eq!(
        git_stdout(&fixture.remote_path, &["rev-parse", "refs/heads/main"]),
        git_stdout(&cwd, &["rev-parse", "HEAD"])
    );
}

#[tokio::test]
async fn tag_create_can_push_the_new_tag_to_a_remote() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let created = collect_events(fixture.operation(
        "65",
        json!({
            "_tag": "tag-create", "cwd": cwd, "projectId": "project-1",
            "name": "v9", "sha": sha, "pushRemote": "origin"
        }),
    ))
    .await;
    assert_eq!(
        created.last().and_then(|event| event["_tag"].as_str()),
        Some("finished"),
        "{created:?}"
    );
    assert!(git_stdout(&cwd, &["ls-remote", "--tags", "origin"]).contains("refs/tags/v9"));

    git(
        &cwd,
        &[
            "remote",
            "add",
            "elsewhere",
            "/nonexistent/bibcode-missing-remote.git",
        ],
    );
    let failed = collect_events(fixture.operation(
        "66",
        json!({
            "_tag": "tag-create", "cwd": cwd, "projectId": "project-1",
            "name": "v10", "sha": sha, "pushRemote": "elsewhere"
        }),
    ))
    .await;
    let last = failed.last().expect("tag create event");
    assert_eq!(last["_tag"], "failed", "{failed:?}");
    assert!(
        last["message"]
            .as_str()
            .is_some_and(|message| message.contains("locally")),
        "the failure says the tag still exists locally: {last}"
    );
    assert_eq!(git_stdout(&cwd, &["tag", "-l", "v10"]), "v10");
}

#[tokio::test]
async fn conflicting_merge_reports_conflicts_and_leaves_the_operation_in_progress() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["switch", "-q", "-c", "feature"]);
    fs::write(cwd.join("tracked.txt"), "feature\n").expect("feature content");
    git(&cwd, &["commit", "-qam", "feature"]);
    git(&cwd, &["switch", "-q", "main"]);
    fs::write(cwd.join("tracked.txt"), "main\n").expect("main content");
    git(&cwd, &["commit", "-qam", "main"]);

    let events = collect_events(fixture.operation(
        "46",
        json!({
            "_tag": "merge", "cwd": cwd, "projectId": "project-1",
            "source": "feature", "noVerify": false
        }),
    ))
    .await;

    assert_eq!(event_kinds(&events).first(), Some(&"started"));
    assert_eq!(event_kinds(&events).last(), Some(&"failed"));
    assert_eq!(events.last().expect("failed event")["code"], "conflicts");
    let merge_head =
        String::from_utf8(git_output(&cwd, &["rev-parse", "--git-path", "MERGE_HEAD"]).stdout)
            .expect("UTF-8 merge path");
    assert!(cwd.join(merge_head.trim()).exists() || Path::new(merge_head.trim()).exists());
}

#[tokio::test]
async fn branch_operations_validate_after_taking_the_worktree_mutation_guard() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["switch", "-q", "-c", "feature"]);
    fs::write(cwd.join("feature.txt"), "feature\n").expect("feature file");
    git(&cwd, &["add", "feature.txt"]);
    git(&cwd, &["commit", "-q", "-m", "feature"]);
    git(&cwd, &["switch", "-q", "main"]);
    let main_tip = git_stdout(&cwd, &["rev-parse", "main"]);

    let guard = fixture.broadcaster.begin_mutation(&cwd).await;
    let receiver = fixture.operation(
        "490",
        json!({ "_tag": "merge", "cwd": cwd, "projectId": "project-1",
                "source": "feature", "noVerify": false }),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    // A vcs.* writer holding the guard dirties the tree before the merge may validate.
    fs::write(cwd.join("tracked.txt"), "dirty while waiting\n").expect("dirty tracked file");
    guard.finish().await;

    let events = collect_events(receiver).await;
    let last = events.last().expect("terminal event");
    assert_eq!(last["_tag"], "failed", "{events:?}");
    assert_eq!(last["code"], "dirty-working-tree", "{events:?}");
    assert_eq!(git_stdout(&cwd, &["rev-parse", "main"]), main_tip);
}

/// main checkout on `main`; `release` and `feature` branch from it with one commit each.
fn merge_into_fixture(cwd: &Path, feature_file: &str, feature_text: &str, release_text: &str) {
    git(cwd, &["switch", "-q", "-c", "release"]);
    fs::write(cwd.join("tracked.txt"), release_text).expect("release change");
    git(cwd, &["commit", "-qam", "release"]);
    git(cwd, &["switch", "-q", "-c", "feature", "main"]);
    fs::write(cwd.join(feature_file), feature_text).expect("feature change");
    git(cwd, &["add", "."]);
    git(cwd, &["commit", "-q", "-m", "feature"]);
    git(cwd, &["switch", "-q", "main"]);
}

fn merge_into(
    fixture: &Fixture,
    id: &str,
    cwd: &Path,
    source: &str,
    target: &str,
) -> mpsc::Receiver<Result<Vec<Value>, Value>> {
    fixture.operation(
        id,
        json!({ "_tag": "merge-into", "cwd": cwd, "projectId": "project-1",
                "source": source, "target": target }),
    )
}

fn merge_head_exists(cwd: &Path) -> bool {
    git_output(cwd, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
        .status
        .success()
}

#[tokio::test]
async fn merge_into_from_a_linked_worktree_leaves_that_checkout_untouched() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    let linked = fixture._root.path().join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            path(&linked),
            "main",
        ],
    );
    configure_identity(&linked);
    fixture.own_worktree(&linked).await;
    fs::write(linked.join("scratch.txt"), "uncommitted\n").expect("dirty linked checkout");
    let before = (
        git_stdout(&linked, &["rev-parse", "HEAD"]),
        git_stdout(&linked, &["ls-files", "-s"]),
        git_stdout(&linked, &["status", "--porcelain"]),
    );
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);
    let feature_tip = git_stdout(&main, &["rev-parse", "feature"]);

    let events = collect_events(merge_into(
        &fixture,
        "501",
        &linked,
        "refs/heads/feature",
        "release",
    ))
    .await;

    assert_eq!(
        events.last().and_then(|event| event["_tag"].as_str()),
        Some("finished"),
        "{events:?}"
    );
    let parents = git_stdout(&main, &["rev-list", "--parents", "-n", "1", "release"]);
    let parents: Vec<_> = parents
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect();
    assert_eq!(parents, [release_tip, feature_tip]);
    assert_eq!(
        git_stdout(&main, &["log", "-1", "--format=%s", "release"]),
        "Merge branch 'feature' into release"
    );
    let after = (
        git_stdout(&linked, &["rev-parse", "HEAD"]),
        git_stdout(&linked, &["ls-files", "-s"]),
        git_stdout(&linked, &["status", "--porcelain"]),
    );
    assert_eq!(before, after);
}

#[tokio::test]
async fn conflicting_merge_into_is_refused_before_any_write() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "tracked.txt", "feature\n", "release\n");
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "502",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    let last = events.last().expect("terminal event");
    assert_eq!(last["_tag"], "failed");
    assert_eq!(last["code"], "conflicts");
    assert!(
        last["message"]
            .as_str()
            .unwrap_or("")
            .contains("Check out release")
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
    assert!(!merge_head_exists(&main));
}

#[tokio::test]
async fn merge_into_blocks_a_target_checked_out_in_another_worktree() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    let holder = fixture._root.path().join("holder");
    git(&main, &["worktree", "add", "-q", path(&holder), "release"]);
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "503",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    let last = events.last().expect("terminal event");
    assert_eq!(last["code"], "worktree-checked-out", "{events:?}");
    assert!(!last["blocked"].is_null());
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
}

#[tokio::test]
async fn merge_into_maps_gits_refusal_for_a_target_mid_rebase_elsewhere() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(&main, &["switch", "-q", "-c", "other", "main"]);
    fs::write(main.join("tracked.txt"), "other\n").expect("other change");
    git(&main, &["commit", "-qam", "other"]);
    git(&main, &["switch", "-q", "main"]);
    let holder = fixture._root.path().join("holder");
    git(&main, &["worktree", "add", "-q", path(&holder), "release"]);
    configure_identity(&holder);
    // The rebase stops on a conflict, leaving `release` detached but in use.
    assert!(!git_output(&holder, &["rebase", "other"]).status.success());
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "504",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    let last = events.last().expect("terminal event");
    assert_eq!(last["code"], "worktree-checked-out", "{events:?}");
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
}

#[tokio::test]
async fn merge_into_fails_without_moving_the_target_when_signing_fails() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(&main, &["config", "commit.gpgSign", "true"]);
    git(&main, &["config", "gpg.program", "false"]);
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "505",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    assert_eq!(
        events.last().expect("terminal event")["_tag"],
        "failed",
        "{events:?}"
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
}

#[tokio::test]
async fn merge_into_fails_when_the_signing_setting_is_unreadable() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    // commit-tree ignores the setting, so an unreadable value must not read as "unsigned".
    git(&main, &["config", "commit.gpgSign", "maybe"]);
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "516",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    let last = events.last().expect("terminal event");
    assert_eq!(last["_tag"], "failed", "{events:?}");
    assert!(
        last["message"]
            .as_str()
            .unwrap_or("")
            .contains("bad boolean"),
        "{last}"
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
}

/// `fetch .` checks occupancy against the alias but writes through it, which would move a
/// checked-out branch under its worktree without touching that worktree's index or files.
#[tokio::test]
async fn merge_into_refuses_a_symbolic_target_that_aliases_a_checked_out_branch() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(
        &main,
        &["symbolic-ref", "refs/heads/trunk", "refs/heads/main"],
    );
    let main_tip = git_stdout(&main, &["rev-parse", "main"]);

    let events = collect_events(merge_into(
        &fixture,
        "517",
        &main,
        "refs/heads/feature",
        "trunk",
    ))
    .await;

    let last = events.last().expect("terminal event");
    assert_eq!(last["_tag"], "failed", "{events:?}");
    assert_eq!(last["code"], "invalid-request", "{events:?}");
    assert!(
        last["message"].as_str().unwrap_or("").contains("main"),
        "{last}"
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "main"]), main_tip);
    assert_eq!(git_stdout(&main, &["status", "--porcelain"]), "");
}

/// The publish is a self-fetch; a configured bundle URI must not be downloaded and imported.
#[tokio::test]
async fn merge_into_publish_does_not_import_a_configured_fetch_bundle() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    let bundle = fixture._root.path().join("configured.bundle");
    git(&main, &["bundle", "create", path(&bundle), "--all"]);
    git(
        &main,
        &[
            "config",
            "fetch.bundleURI",
            &format!("file://{}", path(&bundle)),
        ],
    );

    let events = collect_events(merge_into(
        &fixture,
        "518",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    assert_eq!(
        events.last().expect("terminal event")["_tag"],
        "finished",
        "{events:?}"
    );
    assert_eq!(
        git_stdout(&main, &["for-each-ref", "refs/bundles"]),
        "",
        "publishing must write only the target branch"
    );
}

#[tokio::test]
async fn merge_into_updates_the_branch_not_a_same_named_tag() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(&main, &["tag", "release", "feature"]);
    let release_tip = git_stdout(&main, &["rev-parse", "refs/heads/release"]);

    let events = collect_events(merge_into(
        &fixture,
        "506",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    let last = events.last().expect("terminal event");
    assert_eq!(last["_tag"], "finished", "{events:?}");
    assert_ne!(last["message"], "Already up to date.");
    assert_ne!(
        git_stdout(&main, &["rev-parse", "refs/heads/release"]),
        release_tip
    );
}

#[tokio::test]
async fn merge_into_reports_already_up_to_date_without_writing() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(&main, &["branch", "behind", "release~1"]);
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "507",
        &main,
        "refs/heads/behind",
        "release",
    ))
    .await;

    assert_eq!(
        events.last().expect("terminal")["message"],
        "Already up to date."
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
}

#[tokio::test]
async fn merge_into_rejects_the_current_and_a_deleted_target() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");

    let current = collect_events(merge_into(
        &fixture,
        "508",
        &main,
        "refs/heads/feature",
        "main",
    ))
    .await;
    assert_eq!(
        current.last().expect("terminal")["code"],
        "target-is-current"
    );

    let missing = collect_events(merge_into(
        &fixture,
        "509",
        &main,
        "refs/heads/feature",
        "gone",
    ))
    .await;
    assert_eq!(
        missing.last().expect("terminal")["code"],
        "local-branch-not-found"
    );
    assert!(
        !git_output(&main, &["rev-parse", "-q", "--verify", "refs/heads/gone"])
            .status
            .success()
    );
}

/// `release~1` is not a branch, but `rev-parse refs/heads/release~1` evaluates it as
/// release's parent; the target must be refused as a literal name, writing nothing.
#[tokio::test]
async fn merge_into_target_is_a_literal_branch_name_not_a_revision_expression() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    let refs_before = git_stdout(
        &main,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    );

    for (index, target) in ["release~1", "release^", "release@{1}", "release^{commit}"]
        .into_iter()
        .enumerate()
    {
        let events = collect_events(merge_into(
            &fixture,
            &format!("52{index}"),
            &main,
            "refs/heads/feature",
            target,
        ))
        .await;

        let last = events.last().expect("terminal event");
        assert_eq!(last["_tag"], "failed", "{target}: {events:?}");
        assert_eq!(last["code"], "local-branch-not-found", "{target}");
    }

    assert_eq!(
        git_stdout(
            &main,
            &["for-each-ref", "--format=%(refname) %(objectname)"]
        ),
        refs_before
    );
}

#[tokio::test]
async fn merge_into_records_a_remote_tracking_source() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(&main, &["push", "-q", "origin", "feature"]);
    git(&main, &["fetch", "-q", "origin"]);

    let events = collect_events(merge_into(
        &fixture,
        "510",
        &main,
        "refs/remotes/origin/feature",
        "release",
    ))
    .await;

    assert_eq!(
        events.last().expect("terminal")["_tag"],
        "finished",
        "{events:?}"
    );
    assert_eq!(
        git_stdout(&main, &["log", "-1", "--format=%s", "release"]),
        "Merge remote-tracking branch 'origin/feature' into release"
    );
}

/// The Source Control path (spec criterion 3): fetch, merge a remote-tracking ref into the
/// current branch, resolve the conflict, and finish through `continue` with two parents.
#[tokio::test]
async fn remote_tracking_merge_conflict_finishes_through_continue_with_two_parents() {
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "tracked.txt", "feature\n", "release\n");
    git(&main, &["push", "-q", "origin", "feature"]);
    fs::write(main.join("tracked.txt"), "main\n").expect("main change");
    git(&main, &["commit", "-qam", "main"]);
    let fetched = collect_events(fixture.operation(
        "512",
        json!({ "_tag": "fetch", "cwd": main, "projectId": "project-1", "remote": "origin" }),
    ))
    .await;
    assert_eq!(
        fetched.last().expect("terminal")["_tag"],
        "finished",
        "{fetched:?}"
    );
    let main_tip = git_stdout(&main, &["rev-parse", "main"]);
    let source_tip = git_stdout(&main, &["rev-parse", "refs/remotes/origin/feature"]);

    let merged = collect_events(fixture.operation(
        "513",
        json!({ "_tag": "merge", "cwd": main, "projectId": "project-1",
                "source": "refs/remotes/origin/feature", "noVerify": false }),
    ))
    .await;
    assert_eq!(
        merged.last().expect("terminal")["code"],
        "conflicts",
        "{merged:?}"
    );
    assert!(merge_in_progress(&main));

    let resolved = collect_events(fixture.operation(
        "514",
        json!({ "_tag": "resolve-conflict", "cwd": main, "projectId": "project-1",
                "path": "tracked.txt", "side": "theirs" }),
    ))
    .await;
    assert_eq!(
        resolved.last().expect("terminal")["_tag"],
        "finished",
        "{resolved:?}"
    );
    let finished = collect_events(fixture.operation(
        "515",
        json!({ "_tag": "continue", "cwd": main, "projectId": "project-1", "operation": "merge" }),
    ))
    .await;
    assert_eq!(
        finished.last().expect("terminal")["_tag"],
        "finished",
        "{finished:?}"
    );
    assert_eq!(head_parents(&main), [main_tip, source_tip]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_rejecting_reference_transaction_hook_leaves_the_target_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    let hook = main.join(".git/hooks/reference-transaction");
    fs::write(
        &hook,
        "#!/bin/sh\n[ \"$1\" = prepared ] && exit 1\nexit 0\n",
    )
    .expect("hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("hook mode");
    let release_tip = git_stdout(&main, &["rev-parse", "release"]);

    let events = collect_events(merge_into(
        &fixture,
        "511",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    let last = events.last().expect("terminal");
    assert_eq!(last["_tag"], "failed", "{events:?}");
    assert_eq!(last["code"], "unknown", "{events:?}");
    assert!(
        last["message"]
            .as_str()
            .unwrap_or("")
            .contains("reference-transaction hook"),
        "{last}"
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), release_tip);
}

/// A competing writer moves the target after the in-lock re-read and before the publish
/// updates the ref. The `preparing` hook state runs before Git locks the ref, so the
/// hook can rewind the target with a real nested transaction (once, guarded by a marker).
#[cfg(unix)]
#[tokio::test]
async fn merge_into_reports_non_fast_forward_when_the_target_moves_before_the_publish() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await;
    let main = fixture.repository_path.clone();
    merge_into_fixture(&main, "feature.txt", "feature\n", "release\n");
    git(&main, &["branch", "rival", "main"]);
    git(&main, &["switch", "-q", "rival"]);
    fs::write(main.join("rival.txt"), "rival\n").expect("rival change");
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "rival"]);
    git(&main, &["switch", "-q", "main"]);
    let rival_tip = git_stdout(&main, &["rev-parse", "rival"]);
    let git_dir = main.join(".git");
    let hook = git_dir.join("hooks/reference-transaction");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nif [ \"$1\" = preparing ] && [ ! -e '{dir}/moved' ]; then\n  touch '{dir}/moved'\n  git update-ref refs/heads/release {rival_tip}\nfi\nexit 0\n",
            dir = git_dir.display()
        ),
    )
    .expect("hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("hook mode");

    let events = collect_events(merge_into(
        &fixture,
        "519",
        &main,
        "refs/heads/feature",
        "release",
    ))
    .await;

    if !git_dir.join("moved").exists() {
        eprintln!("non-fast-forward publish coverage skipped: Git has no `preparing` hook state");
        return;
    }
    let last = events.last().expect("terminal event");
    assert_eq!(last["_tag"], "failed", "{events:?}");
    assert_eq!(last["code"], "non-fast-forward", "{events:?}");
    assert_eq!(
        last["message"],
        "release changed while merging. Review the preview and try again."
    );
    assert_eq!(git_stdout(&main, &["rev-parse", "release"]), rival_tip);
}

#[tokio::test]
async fn cherry_pick_conflict_lifecycle_detects_resolves_continues_and_aborts() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["switch", "-q", "-c", "conflict-source"]);
    fs::write(cwd.join("tracked.txt"), "theirs first\n").expect("source content");
    git(&cwd, &["commit", "-qam", "source conflict"]);
    let source_sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    git(&cwd, &["switch", "-q", "main"]);
    fs::write(cwd.join("tracked.txt"), "ours first\n").expect("main content");
    git(&cwd, &["commit", "-qam", "main conflict"]);

    let started = collect_events(fixture.operation(
        "60",
        json!({
            "_tag": "cherry-pick", "cwd": cwd, "projectId": "project-1",
            "shas": [source_sha]
        }),
    ))
    .await;
    assert_eq!(event_kinds(&started).first(), Some(&"started"));
    assert_eq!(event_kinds(&started).last(), Some(&"failed"));
    assert_eq!(
        started.last().expect("conflict failure")["code"],
        "conflicts-encountered"
    );

    let conflicts = GitRepository::default()
        .git_manager_conflict_states(&cwd, &CancellationToken::new())
        .await
        .expect("conflict state");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].path, "tracked.txt");
    assert_eq!(conflicts[0].marker_count, 3);

    let theirs = collect_events(fixture.operation(
        "61",
        json!({
            "_tag": "resolve-conflict", "cwd": cwd, "projectId": "project-1",
            "path": "tracked.txt", "side": "theirs"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&theirs).last(), Some(&"finished"));
    assert_eq!(
        fs::read_to_string(cwd.join("tracked.txt")).expect("theirs resolution"),
        "theirs first\n"
    );

    let continued = collect_events(fixture.operation(
        "62",
        json!({
            "_tag": "continue", "cwd": cwd, "projectId": "project-1",
            "operation": "cherry-pick"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&continued).last(), Some(&"finished"));
    let continued_tip = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    git(&cwd, &["switch", "-q", "-c", "abort-source"]);
    fs::write(cwd.join("tracked.txt"), "theirs second\n").expect("second source content");
    git(&cwd, &["commit", "-qam", "second source conflict"]);
    let abort_source_sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    git(&cwd, &["switch", "-q", "main"]);
    assert_eq!(git_stdout(&cwd, &["rev-parse", "HEAD"]), continued_tip);
    fs::write(cwd.join("tracked.txt"), "ours second\n").expect("second main content");
    git(&cwd, &["commit", "-qam", "second main conflict"]);
    let pre_abort_tip = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let second = collect_events(fixture.operation(
        "63",
        json!({
            "_tag": "cherry-pick", "cwd": cwd, "projectId": "project-1",
            "shas": [abort_source_sha]
        }),
    ))
    .await;
    assert_eq!(event_kinds(&second).last(), Some(&"failed"));

    let ours = collect_events(fixture.operation(
        "64",
        json!({
            "_tag": "resolve-conflict", "cwd": cwd, "projectId": "project-1",
            "path": "tracked.txt", "side": "ours"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&ours).last(), Some(&"finished"));
    assert_eq!(
        fs::read_to_string(cwd.join("tracked.txt")).expect("ours resolution"),
        "ours second\n"
    );

    let aborted = collect_events(fixture.operation(
        "65",
        json!({
            "_tag": "abort", "cwd": cwd, "projectId": "project-1",
            "operation": "cherry-pick"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&aborted).last(), Some(&"finished"));
    assert_eq!(git_stdout(&cwd, &["rev-parse", "HEAD"]), pre_abort_tip);
    assert_eq!(
        fs::read_to_string(cwd.join("tracked.txt")).expect("abort restoration"),
        "ours second\n"
    );
    assert!(
        GitRepository::default()
            .git_manager_conflict_states(&cwd, &CancellationToken::new())
            .await
            .expect("clean conflict state")
            .is_empty()
    );
}

#[tokio::test]
async fn squash_can_rewrite_through_the_initial_commit_with_root() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    let initial = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    fs::write(cwd.join("second.txt"), "second\n").expect("second file");
    git(&cwd, &["add", "second.txt"]);
    git(&cwd, &["commit", "-q", "-m", "second"]);
    let second = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    fs::write(cwd.join("third.txt"), "third\n").expect("third file");
    git(&cwd, &["add", "third.txt"]);
    git(&cwd, &["commit", "-q", "-m", "third"]);
    let third = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let events = collect_events(fixture.operation(
        "66",
        json!({
            "_tag": "squash", "cwd": cwd, "projectId": "project-1",
            "shas": [third, second, initial], "message": "combined root history"
        }),
    ))
    .await;

    assert_eq!(event_kinds(&events).last(), Some(&"finished"));
    assert_eq!(git_stdout(&cwd, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        git_stdout(&cwd, &["show", "-s", "--format=%s", "HEAD"]),
        "combined root history"
    );
    assert!(cwd.join("tracked.txt").exists());
    assert!(cwd.join("second.txt").exists());
    assert!(cwd.join("third.txt").exists());
}

#[tokio::test]
async fn reorder_replays_the_touched_range_in_the_requested_order() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    fs::write(cwd.join("a.txt"), "a\n").expect("a file");
    git(&cwd, &["add", "a.txt"]);
    git(&cwd, &["commit", "-q", "-m", "commit A"]);
    let commit_a = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    fs::write(cwd.join("b.txt"), "b\n").expect("b file");
    git(&cwd, &["add", "b.txt"]);
    git(&cwd, &["commit", "-q", "-m", "commit B"]);
    let commit_b = git_stdout(&cwd, &["rev-parse", "HEAD"]);

    let events = collect_events(fixture.operation(
        "67",
        json!({
            "_tag": "reorder", "cwd": cwd, "projectId": "project-1",
            "shas": [commit_a], "insertBeforeSha": commit_b
        }),
    ))
    .await;

    assert_eq!(event_kinds(&events).last(), Some(&"finished"));
    assert_eq!(
        git_stdout(&cwd, &["log", "-3", "--format=%s"]),
        "commit A\ncommit B\nbase"
    );
}

#[tokio::test]
async fn rebase_revert_and_all_reset_modes_execute_through_the_stream() {
    let fixture = Fixture::new().await;
    let cwd = fixture.repository_path.clone();
    git(&cwd, &["switch", "-q", "-c", "topic"]);
    fs::write(cwd.join("topic.txt"), "topic\n").expect("topic file");
    git(&cwd, &["add", "topic.txt"]);
    git(&cwd, &["commit", "-q", "-m", "topic"]);
    git(&cwd, &["switch", "-q", "main"]);
    fs::write(cwd.join("main.txt"), "main\n").expect("main file");
    git(&cwd, &["add", "main.txt"]);
    git(&cwd, &["commit", "-q", "-m", "main"]);

    let rebased = collect_events(fixture.operation(
        "68",
        json!({
            "_tag": "rebase", "cwd": cwd, "projectId": "project-1",
            "base": "main", "target": "topic"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&rebased).last(), Some(&"finished"));
    assert_eq!(git_stdout(&cwd, &["branch", "--show-current"]), "topic");
    git(&cwd, &["merge-base", "--is-ancestor", "main", "topic"]);
    git(&cwd, &["switch", "-q", "main"]);

    fs::write(cwd.join("revert-me.txt"), "revert\n").expect("revert file");
    git(&cwd, &["add", "revert-me.txt"]);
    git(&cwd, &["commit", "-q", "-m", "revert target"]);
    let revert_sha = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    let reverted = collect_events(fixture.operation(
        "69",
        json!({
            "_tag": "revert", "cwd": cwd, "projectId": "project-1", "sha": revert_sha
        }),
    ))
    .await;
    assert_eq!(event_kinds(&reverted).last(), Some(&"finished"));
    assert!(!cwd.join("revert-me.txt").exists());

    let hard_target = git_stdout(&cwd, &["rev-parse", "HEAD"]);
    fs::write(cwd.join("hard.txt"), "hard\n").expect("hard file");
    git(&cwd, &["add", "hard.txt"]);
    git(&cwd, &["commit", "-q", "-m", "hard reset target"]);
    let hard = collect_events(fixture.operation(
        "70",
        json!({
            "_tag": "reset", "cwd": cwd, "projectId": "project-1",
            "sha": hard_target, "mode": "hard"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&hard).last(), Some(&"finished"));
    assert!(!cwd.join("hard.txt").exists());

    fs::write(cwd.join("soft.txt"), "soft\n").expect("soft file");
    git(&cwd, &["add", "soft.txt"]);
    git(&cwd, &["commit", "-q", "-m", "soft reset target"]);
    let soft_parent = git_stdout(&cwd, &["rev-parse", "HEAD^"]);
    let soft = collect_events(fixture.operation(
        "71",
        json!({
            "_tag": "reset", "cwd": cwd, "projectId": "project-1",
            "sha": soft_parent, "mode": "soft"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&soft).last(), Some(&"finished"));
    assert_eq!(
        git_stdout(&cwd, &["diff", "--cached", "--name-only"]),
        "soft.txt"
    );
    git(&cwd, &["reset", "--hard", "-q", "HEAD"]);

    fs::write(cwd.join("mixed.txt"), "mixed\n").expect("mixed file");
    git(&cwd, &["add", "mixed.txt"]);
    git(&cwd, &["commit", "-q", "-m", "mixed reset target"]);
    let mixed_parent = git_stdout(&cwd, &["rev-parse", "HEAD^"]);
    let mixed = collect_events(fixture.operation(
        "72",
        json!({
            "_tag": "reset", "cwd": cwd, "projectId": "project-1",
            "sha": mixed_parent, "mode": "mixed"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&mixed).last(), Some(&"finished"));
    assert!(cwd.join("mixed.txt").exists());
    assert!(git_stdout(&cwd, &["diff", "--cached", "--name-only"]).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_operation_is_rejected_and_cancellation_terminates_the_child() {
    use std::process::Stdio;
    use tokio::time::sleep;

    let fixture = Fixture::new().await;
    let marker = fixture._root.path().join("slow-fetch.pid");
    let helper = fixture._root.path().join("slow-remote.sh");
    executable_fixture::write_executable(
        &helper,
        format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 60\n", path(&marker)),
    );
    git(
        &fixture.repository_path,
        &["config", "protocol.ext.allow", "always"],
    );
    git(
        &fixture.repository_path,
        &[
            "remote",
            "add",
            "slow",
            &format!("ext::sh {}", path(&helper)),
        ],
    );

    let first_cancellation = CancellationToken::new();
    let mut first = fixture.services.operation_stream(
        operation_request(
            "3",
            json!({
                "_tag": "fetch",
                "cwd": fixture.repository_path,
                "projectId": "project-1",
                "remote": "slow"
            }),
        ),
        first_cancellation.clone(),
    );
    let started = next_event(&mut first).await;
    assert_eq!(started["_tag"], "started");
    wait_for_file(&marker).await;

    let second = collect_events(fixture.operation(
        "4",
        json!({
            "_tag": "fetch",
            "cwd": fixture.repository_path,
            "projectId": "project-1",
            "remote": "origin"
        }),
    ))
    .await;
    assert_eq!(event_kinds(&second), ["started", "failed"]);
    assert_eq!(second[1]["code"], "operation-in-flight");
    assert_eq!(second[1]["blocked"]["code"], "operation-in-flight");

    let pid = fs::read_to_string(&marker)
        .expect("slow helper pid")
        .trim()
        .to_owned();
    first_cancellation.cancel();
    let remaining = collect_events(first).await;
    assert_eq!(event_kinds(&remaining), ["failed"]);
    assert_eq!(remaining[0]["code"], "cancelled");
    let stopped = timeout(Duration::from_secs(5), async {
        loop {
            if !Command::new("kill")
                .args(["-0", &pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("inspect slow helper")
                .success()
            {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(stopped.is_ok(), "cancelled Git child remained alive");
}

#[tokio::test]
async fn refs_render_an_unborn_or_placeholder_head_as_a_repository_without_commits() {
    let fixture = Fixture::new().await;
    let unborn = fixture._root.path().join("unborn");
    fs::create_dir(&unborn).expect("unborn repository directory");
    git(&unborn, &["init", "-q", "-b", "main"]);
    // `git clone` writes this placeholder before the remote HEAD is known; an
    // interrupted clone leaves it behind with no refs.
    let placeholder = fixture._root.path().join("placeholder");
    fs::create_dir(&placeholder).expect("placeholder repository directory");
    git(&placeholder, &["init", "-q", "-b", "main"]);
    fs::write(placeholder.join(".git/HEAD"), "ref: refs/heads/.invalid\n")
        .expect("placeholder HEAD");
    git(
        &placeholder,
        &["config", "remote.origin.url", path(&fixture.remote_path)],
    );

    let unborn_refs = fixture
        .read("901", "gitManager.getRefs", json!({ "cwd": unborn }))
        .await
        .expect("an unborn branch has no commits yet");
    assert_eq!(unborn_refs["headRef"], "main");
    assert_eq!(unborn_refs["detachedSha"], Value::Null);
    assert_eq!(unborn_refs["localBranches"], json!([]));
    assert_eq!(unborn_refs["worktrees"][0]["isDetached"], false);

    let placeholder_refs = fixture
        .read("902", "gitManager.getRefs", json!({ "cwd": placeholder }))
        .await
        .expect("a placeholder HEAD has no commits yet");
    assert_eq!(placeholder_refs["headRef"], Value::Null);
    assert_eq!(placeholder_refs["detachedSha"], Value::Null);
    assert_eq!(placeholder_refs["localBranches"], json!([]));
    assert_eq!(placeholder_refs["remoteBranches"], json!([]));
    assert_eq!(placeholder_refs["remotes"], json!(["origin"]));
    assert_eq!(placeholder_refs["worktrees"][0]["branch"], Value::Null);
    assert_eq!(placeholder_refs["worktrees"][0]["isDetached"], false);
}

#[cfg(unix)]
fn operation_request(id: &str, payload: Value) -> RpcRequest {
    rpc_request(id, "gitManager.runOperation", payload)
}

fn rpc_request(id: &str, tag: &str, payload: Value) -> RpcRequest {
    RpcRequest {
        id: RequestId::try_from(id).expect("request id"),
        tag: tag.to_owned(),
        payload,
        headers: Vec::new(),
        trace_id: None,
        span_id: None,
        sampled: None,
    }
}

/// A `gitManager.getRemoteTags` read, sent over a real WebSocket, against a
/// remote that accepts Git's HTTP connection and never answers. The read runs
/// inline in its RPC handler, so an Interrupt or a closed socket makes the
/// session drop the handler's future: the process runner never sees a
/// cancelled token, and that drop alone must stop Git and its helpers.
struct StalledRemoteTagsRead {
    _state: TempDir,
    _repository: TempDir,
    handle: ServerHandle,
    socket: TestSocket,
    /// Git's HTTP transport helper holds this connection while it runs.
    helper_connection: tokio::net::TcpStream,
}

impl StalledRemoteTagsRead {
    async fn start(request_id: &str) -> Self {
        let state = TempDir::new().expect("temporary server state");
        let repository = TempDir::new().expect("temporary repository");
        git(repository.path(), &["init", "-q", "-b", "main"]);
        // An explicit empty proxy keeps Git on the loopback remote even when the host sets one.
        git(repository.path(), &["config", "http.proxy", ""]);
        let remote = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stalled remote listener");
        let url = format!(
            "http://{}/stalled.git",
            remote.local_addr().expect("stalled remote address")
        );
        git(repository.path(), &["remote", "add", "stalled", &url]);

        let mut registry = RpcRegistry::empty();
        register_git_manager_rpc(&mut registry, GitManagerRpcServices);
        let config = ServerConfig::new(state.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        let handle = ServerRuntime::start_with_registry(config, registry)
            .await
            .expect("Git Manager server starts");
        let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
            .await
            .expect("WebSocket connects");

        send_json(
            &mut socket,
            json!({
                "_tag": "Request",
                "id": request_id,
                "tag": "gitManager.getRemoteTags",
                "payload": { "cwd": repository.path(), "remote": "stalled" },
                "headers": []
            }),
        )
        .await;
        let (mut helper_connection, _) = timeout(SOCKET_RESPONSE_BOUND, remote.accept())
            .await
            .expect("Git connects to the stalled remote")
            .expect("accept Git's connection");
        let request = read_http_request_head(&mut helper_connection).await;
        assert!(
            request.starts_with("GET /stalled.git/info/refs?service=git-upload-pack "),
            "unexpected request from Git: {request}"
        );
        Self {
            _state: state,
            _repository: repository,
            handle,
            socket,
            helper_connection,
        }
    }

    async fn shutdown(self) {
        let Self {
            handle, mut socket, ..
        } = self;
        let _ = socket.close(None).await;
        handle.shutdown();
        let _ = handle.join().await;
    }
}

async fn read_http_request_head(connection: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;

    let mut head = Vec::new();
    timeout(SOCKET_RESPONSE_BOUND, async {
        let mut buffer = [0_u8; 1024];
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = connection
                .read(&mut buffer)
                .await
                .expect("read Git's request");
            assert_ne!(
                read, 0,
                "Git closed its connection before sending a request"
            );
            head.extend_from_slice(&buffer[..read]);
        }
    })
    .await
    .expect("Git sends its request");
    String::from_utf8_lossy(&head).into_owned()
}

async fn wait_for_connection_close(connection: &mut tokio::net::TcpStream, action: &str) {
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
    .unwrap_or_else(|_| panic!("{action} left Git's transport helper connected to the remote"));
}

async fn send_json(socket: &mut TestSocket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .expect("send WebSocket message");
}

async fn next_server_message(socket: &mut TestSocket) -> ServerMessage {
    let frame = timeout(SOCKET_RESPONSE_BOUND, next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected a text WebSocket message, got {frame:?}");
    };
    serde_json::from_str(&text).expect("valid server RPC message")
}

/// Waits until the signal has been quiet for longer than the ref debounce and
/// returns the last generation seen. The quiet window is part of the "after
/// idle" scenario: the next external change must still bump the signal. It
/// also drains a late bump from the previous step, so the next assertion can
/// only be satisfied by the next operation.
async fn wait_until_signal_idle(
    stream: &mut mpsc::Receiver<Result<Vec<Value>, Value>>,
    mut generation: u64,
) -> u64 {
    while let Ok(event) = timeout(Duration::from_millis(300), next_event(stream)).await {
        generation = event["generation"].as_u64().expect("signal generation");
    }
    generation
}

async fn next_generation_after(
    receiver: &mut mpsc::Receiver<Result<Vec<Value>, Value>>,
    generation: u64,
) -> u64 {
    loop {
        let event = next_event(receiver).await;
        let observed = event["generation"].as_u64().expect("signal generation");
        if observed > generation {
            return observed;
        }
    }
}

async fn collect_events(mut receiver: mpsc::Receiver<Result<Vec<Value>, Value>>) -> Vec<Value> {
    let mut events = Vec::new();
    while let Some(chunk) = timeout(Duration::from_secs(15), receiver.recv())
        .await
        .expect("operation stream timeout")
    {
        events.extend(chunk.expect("operation stream chunk"));
    }
    events
}

async fn next_event(receiver: &mut mpsc::Receiver<Result<Vec<Value>, Value>>) -> Value {
    let chunk = timeout(Duration::from_secs(15), receiver.recv())
        .await
        .expect("operation event timeout")
        .expect("operation stream remains open")
        .expect("operation event chunk");
    let [event] = chunk.as_slice() else {
        panic!("expected one operation event, got {chunk:?}");
    };
    event.clone()
}

fn event_kinds(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["_tag"].as_str().expect("event tag"))
        .collect()
}

#[cfg(unix)]
async fn wait_for_file(path: &Path) {
    use tokio::time::{Instant, sleep};

    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for helper start"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

fn publish_remote_change(remote: &Path, root: &Path) {
    let publisher = root.join("publisher");
    git(
        root,
        &["clone", "-q", "-b", "main", path(remote), path(&publisher)],
    );
    configure_identity(&publisher);
    fs::write(publisher.join("remote.txt"), "remote change\n").expect("remote file");
    git(&publisher, &["add", "remote.txt"]);
    git(&publisher, &["commit", "-q", "-m", "remote change"]);
    git(&publisher, &["push", "-q", "origin", "main"]);
}

fn configure_identity(repository: &Path) {
    git(repository, &["config", "user.name", "Git Manager Test"]);
    git(
        repository,
        &["config", "user.email", "git-manager@example.test"],
    );
}

fn index_content(repository: &Path) -> String {
    index_file_content(repository, "tracked.txt")
}

fn index_file_content(repository: &Path, path: &str) -> String {
    let selector = format!(":{path}");
    let output = git_output(repository, &["show", &selector]);
    assert!(output.status.success(), "index blob is readable");
    String::from_utf8(output.stdout).expect("UTF-8 index content")
}

fn git_output(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git fixture starts")
}

fn git(cwd: &Path, args: &[&str]) {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn head_parents(cwd: &Path) -> Vec<String> {
    git_stdout(cwd, &["rev-list", "--parents", "-n", "1", "HEAD"])
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

fn merge_in_progress(cwd: &Path) -> bool {
    let merge_head = git_stdout(cwd, &["rev-parse", "--git-path", "MERGE_HEAD"]);
    cwd.join(&merge_head).exists() || Path::new(&merge_head).exists()
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn path(path: &Path) -> &str {
    path.to_str().expect("UTF-8 temporary path")
}
