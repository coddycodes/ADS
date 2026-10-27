use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::Duration,
};

use coddy_studio_server::production::git_manager_rpc::{
    GitManagerRpcServices, register_git_manager_rpc,
};
use coddy_studio_server::{
    RpcRegistry, ServerConfig, ServerHandle, ServerRuntime,
    git::{
        GitRepository,
        manager::{graph::page, refs::build_refs_snapshot},
    },
};
use futures_util::SinkExt;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const DESKTOP_BOOTSTRAP: &str = "git-manager-read-bootstrap";
const TOKEN_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:coddy-studio:params:oauth:token-type:environment-bootstrap";

#[tokio::test]
async fn unchanged_refs_reads_keep_the_same_generation() {
    let fixture = TempDir::new().expect("temporary Git fixture root");
    initialize_repository(fixture.path());
    let repository = GitRepository::default();
    let cancellation = CancellationToken::new();

    let first = build_refs_snapshot(&repository, fixture.path(), &cancellation)
        .await
        .expect("first refs snapshot");
    let second = build_refs_snapshot(&repository, fixture.path(), &cancellation)
        .await
        .expect("second refs snapshot");

    assert_eq!(second.generation, first.generation);
}

#[tokio::test]
async fn unchanged_pinned_page_reads_keep_the_same_generation() {
    let fixture = TempDir::new().expect("temporary Git fixture root");
    initialize_repository(fixture.path());
    let repository = GitRepository::default();
    let cancellation = CancellationToken::new();

    let first = page(&repository, fixture.path(), None, 0, 1, &cancellation)
        .await
        .expect("first commit page");
    let second = page(
        &repository,
        fixture.path(),
        Some(&first.pinned_tips),
        1,
        1,
        &cancellation,
    )
    .await
    .expect("second commit page");

    assert_eq!(second.generation, first.generation);
}

#[tokio::test]
async fn refs_and_page_reads_agree_on_generation() {
    let fixture = TempDir::new().expect("temporary Git fixture root");
    initialize_repository(fixture.path());
    let repository = GitRepository::default();
    let cancellation = CancellationToken::new();

    let refs = build_refs_snapshot(&repository, fixture.path(), &cancellation)
        .await
        .expect("refs snapshot");
    let commits = page(&repository, fixture.path(), None, 0, 1, &cancellation)
        .await
        .expect("commit page");

    assert_eq!(commits.generation, refs.generation);
}

#[tokio::test]
async fn repository_mutation_advances_generation() {
    let fixture = TempDir::new().expect("temporary Git fixture root");
    initialize_repository(fixture.path());
    let repository = GitRepository::default();
    let cancellation = CancellationToken::new();

    let before = build_refs_snapshot(&repository, fixture.path(), &cancellation)
        .await
        .expect("refs snapshot before mutation");
    git(
        fixture.path(),
        &["commit", "-q", "--allow-empty", "-m", "generation mutation"],
    );
    let after = build_refs_snapshot(&repository, fixture.path(), &cancellation)
        .await
        .expect("refs snapshot after mutation");

    assert!(after.generation > before.generation);
}

#[tokio::test]
async fn repository_generations_advance_independently() {
    let first_fixture = TempDir::new().expect("first temporary Git fixture root");
    let second_fixture = TempDir::new().expect("second temporary Git fixture root");
    initialize_repository(first_fixture.path());
    initialize_repository(second_fixture.path());
    let repository = GitRepository::default();
    let cancellation = CancellationToken::new();

    let first_before = build_refs_snapshot(&repository, first_fixture.path(), &cancellation)
        .await
        .expect("first repository snapshot before mutation");
    let second_before = build_refs_snapshot(&repository, second_fixture.path(), &cancellation)
        .await
        .expect("second repository snapshot before mutation");
    git(
        first_fixture.path(),
        &["commit", "-q", "--allow-empty", "-m", "generation mutation"],
    );
    let first_after = build_refs_snapshot(&repository, first_fixture.path(), &cancellation)
        .await
        .expect("first repository snapshot after mutation");
    let second_after = build_refs_snapshot(&repository, second_fixture.path(), &cancellation)
        .await
        .expect("second repository snapshot after other repository mutation");

    assert!(first_after.generation > first_before.generation);
    assert_eq!(second_after.generation, second_before.generation);
}

#[tokio::test]
async fn read_scoped_git_manager_rpcs_preserve_worktree_history_and_diff_state() {
    let state = TempDir::new().expect("temporary server state");
    let fixture = TempDir::new().expect("temporary Git fixture root");
    let repository = fixture.path().join("main");
    let linked_worktree = fixture.path().join("linked");
    fs::create_dir(&repository).expect("main checkout directory");
    initialize_repository(&repository);
    let expected_second_page_sha = git_stdout(&repository, &["rev-parse", "HEAD^"])
        .trim()
        .to_owned();
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked_worktree.to_str().expect("UTF-8 worktree path"),
        ],
    );
    fs::write(repository.join("tracked.txt"), "working tree change\n")
        .expect("modified working-tree file");

    let mut registry = RpcRegistry::empty();
    register_git_manager_rpc(&mut registry, GitManagerRpcServices);
    let config = ServerConfig::new(state.path())
        .with_bind("127.0.0.1", 0)
        .with_desktop(DESKTOP_BOOTSTRAP)
        .expect("valid desktop server configuration");
    let handle = ServerRuntime::start_with_registry(config, registry)
        .await
        .expect("Git Manager server starts");
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP).await;
    let pairing = response_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(access_token(&administrator))
            .json(&json!({
                "label": "Git Manager read-only test",
                "scopes": ["orchestration:read"]
            }))
            .send()
            .await
            .expect("read-only pairing request"),
        StatusCode::OK,
    )
    .await;
    let restricted = exchange_token(
        &client,
        &handle,
        pairing["credential"].as_str().expect("pairing credential"),
    )
    .await;
    assert_eq!(restricted["scope"], "orchestration:read");
    let ticket = response_json(
        client
            .post(http_url(&handle, "/api/auth/websocket-ticket"))
            .bearer_auth(access_token(&restricted))
            .send()
            .await
            .expect("WebSocket ticket request"),
        StatusCode::OK,
    )
    .await["ticket"]
        .as_str()
        .expect("WebSocket ticket")
        .to_owned();
    let (mut socket, _) =
        connect_async(format!("ws://{}/ws?wsTicket={ticket}", handle.local_addr()))
            .await
            .expect("read-only WebSocket connects");
    let cwd = repository.to_string_lossy().into_owned();

    request(
        &mut socket,
        "1",
        "gitManager.getRefs",
        json!({ "cwd": cwd }),
    )
    .await;
    let refs = success(&mut socket, "1").await;
    let feature = refs["localBranches"]
        .as_array()
        .expect("local branch array")
        .iter()
        .find(|branch| branch["name"] == "feature")
        .expect("feature branch");
    // The snapshot keeps Git's own display spelling, which on macOS resolves the
    // temporary root's `/var` alias to `/private/var`; identity is compared
    // through the server's canonical key rather than the fixture's spelling.
    let reported_worktree_path = feature["worktreePath"]
        .as_str()
        .expect("occupied branch reports its worktree path");
    assert_eq!(
        coddy_studio_server::git::canonical_worktree_path_key(std::path::Path::new(
            reported_worktree_path
        ))
        .await
        .expect("reported worktree key"),
        coddy_studio_server::git::canonical_worktree_path_key(&linked_worktree)
            .await
            .expect("fixture worktree key")
    );
    assert!(
        git_stdout(&repository, &["worktree", "list", "--porcelain"])
            .lines()
            .any(|line| line == format!("worktree {reported_worktree_path}")),
        "the reported spelling must be Git's own worktree spelling"
    );

    request(
        &mut socket,
        "2",
        "gitManager.getCommits",
        json!({ "cwd": cwd, "offset": 0, "limit": 1 }),
    )
    .await;
    let first_page = success(&mut socket, "2").await;
    let pinned_tips = first_page["pinnedTips"].clone();
    assert_eq!(first_page["commits"].as_array().map(Vec::len), Some(1));
    assert_eq!(first_page["exhausted"], false);

    git(
        &repository,
        &["commit", "-q", "--allow-empty", "-m", "concurrent commit"],
    );
    request(
        &mut socket,
        "3",
        "gitManager.getCommits",
        json!({
            "cwd": cwd,
            "pinnedTips": pinned_tips,
            "offset": 1,
            "limit": 1
        }),
    )
    .await;
    let second_page = success(&mut socket, "3").await;
    assert_eq!(second_page["commits"][0]["sha"], expected_second_page_sha);

    request(
        &mut socket,
        "4",
        "gitManager.getDiff",
        json!({
            "cwd": cwd,
            "source": { "_tag": "working-tree", "path": "tracked.txt", "staged": false }
        }),
    )
    .await;
    let working_diff = success(&mut socket, "4").await;
    assert_eq!(working_diff["_tag"], "patch");
    assert!(
        working_diff["patch"]
            .as_str()
            .is_some_and(|patch| patch.contains("working tree change"))
    );

    request(
        &mut socket,
        "5",
        "gitManager.getDiff",
        json!({
            "cwd": cwd,
            "source": {
                "_tag": "commit",
                "sha": expected_second_page_sha,
                "path": "tracked.txt"
            }
        }),
    )
    .await;
    assert_eq!(success(&mut socket, "5").await["_tag"], "patch");

    request(&mut socket, "6", "gitManager.commit", json!({ "cwd": cwd })).await;
    let denied = failure(&mut socket, "6").await;
    assert_eq!(denied["_tag"], "EnvironmentAuthorizationError");
    assert_eq!(denied["requiredScope"], "orchestration:operate");

    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("server shutdown timeout")
        .expect("server joins");
}

fn initialize_repository(repository: &Path) {
    git(repository, &["init", "-q", "-b", "main"]);
    git(repository, &["config", "user.name", "Git Manager Test"]);
    git(
        repository,
        &["config", "user.email", "git-manager@example.test"],
    );
    for (index, contents) in ["first\n", "second\n", "third\n"].into_iter().enumerate() {
        fs::write(repository.join("tracked.txt"), contents).expect("tracked fixture file");
        git(repository, &["add", "tracked.txt"]);
        git(
            repository,
            &["commit", "-q", "-m", &format!("commit {}", index + 1)],
        );
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git fixture starts")
}

fn git(cwd: &Path, args: &[&str]) {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = git_output(cwd, args);
    assert!(
        output.status.success(),
        "git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}

fn http_url(handle: &ServerHandle, path: &str) -> String {
    format!("http://{}{}", handle.local_addr(), path)
}

async fn exchange_token(client: &Client, handle: &ServerHandle, credential: &str) -> Value {
    response_json(
        client
            .post(http_url(handle, "/oauth/token"))
            .form(&[
                ("grant_type", TOKEN_GRANT_TYPE),
                ("subject_token", credential),
                ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
                ("requested_token_type", ACCESS_TOKEN_TYPE),
            ])
            .send()
            .await
            .expect("token exchange request"),
        StatusCode::OK,
    )
    .await
}

fn access_token(response: &Value) -> &str {
    response["access_token"].as_str().expect("access token")
}

async fn response_json(response: reqwest::Response, expected_status: StatusCode) -> Value {
    let status = response.status();
    let body = response.text().await.expect("HTTP response body");
    assert_eq!(status, expected_status, "unexpected HTTP response: {body}");
    serde_json::from_str(&body).expect("JSON response")
}

async fn request<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    id: &str,
    tag: &str,
    payload: Value,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": payload,
                "headers": []
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send RPC request");
}

async fn success<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, id: &str) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = next_message(socket).await;
    assert_eq!(message["_tag"], "Exit");
    assert_eq!(message["requestId"], id);
    assert_eq!(message["exit"]["_tag"], "Success", "{message}");
    message["exit"]["value"].clone()
}

async fn failure<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, id: &str) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = next_message(socket).await;
    assert_eq!(message["_tag"], "Exit");
    assert_eq!(message["requestId"], id);
    assert_eq!(message["exit"]["_tag"], "Failure", "{message}");
    message["exit"]["cause"][0]["error"].clone()
}

async fn next_message<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(10), next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    let Message::Text(text) = frame else {
        panic!("expected text frame, got {frame:?}");
    };
    serde_json::from_str(&text).expect("valid RPC response")
}
