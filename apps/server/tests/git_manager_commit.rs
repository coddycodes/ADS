use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use coddy_studio_server::{
    RpcRegistry, ServerConfig, ServerHandle, ServerRuntime,
    git::{FileTrash, FileTrashFuture, GitRepository, StatusBroadcaster, TrashUnavailable},
    persistence::{Database, ProjectionProject, Repositories, run_migrations},
    production::{
        git_manager_rpc::{GitManagerRpcServices, register_git_manager_rpc},
        git_vcs::{GitVcsRpcServices, register_git_vcs_rpc},
    },
    worktree_catalog::{WorkspaceAvailabilityRegistry, WorktreeCatalogService},
};
use futures_util::SinkExt;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio::{
    net::TcpStream,
    sync::{Notify, Semaphore},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const DESKTOP_BOOTSTRAP: &str = "git-manager-commit-bootstrap";
const TOKEN_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:coddy-studio:params:oauth:token-type:environment-bootstrap";

struct TestTrash {
    destination: PathBuf,
    next_id: AtomicUsize,
    block_next: AtomicBool,
    started: Notify,
    release: Semaphore,
}

impl TestTrash {
    fn new(destination: PathBuf) -> Self {
        Self {
            destination,
            next_id: AtomicUsize::new(1),
            block_next: AtomicBool::new(false),
            started: Notify::new(),
            release: Semaphore::new(0),
        }
    }

    fn block_next(&self) {
        self.block_next.store(true, Ordering::Release);
    }

    async fn wait_until_started(&self) {
        self.started.notified().await;
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

impl FileTrash for TestTrash {
    fn trash<'a>(
        &'a self,
        path: PathBuf,
        cancellation: &'a CancellationToken,
    ) -> FileTrashFuture<'a> {
        Box::pin(async move {
            if self.block_next.swap(false, Ordering::AcqRel) {
                self.started.notify_one();
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(TrashUnavailable),
                    permit = self.release.acquire() => {
                        permit.map_err(|_| TrashUnavailable)?.forget();
                    }
                }
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let file_name = path.file_name().ok_or(TrashUnavailable)?;
            let destination = self
                .destination
                .join(format!("{id}-{}", file_name.to_string_lossy()));
            tokio::fs::rename(path, destination)
                .await
                .map_err(|_| TrashUnavailable)
        })
    }
}

#[tokio::test]
async fn commit_amend_undo_discard_scope_and_concurrency_follow_the_wire_contract() {
    let state = TempDir::new().expect("temporary server state");
    let fixture = TempDir::new().expect("temporary Git fixture root");
    let trash_directory = TempDir::new().expect("temporary trash root");
    let repository_path = fixture.path().join("main");
    fs::create_dir(&repository_path).expect("main checkout directory");
    initialize_repository(&repository_path);

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
            title: "Git Manager Commit".to_owned(),
            workspace_root: repository_path.to_string_lossy().into_owned(),
            default_model_selection: None,
            scripts: json!([]),
            worktree_discovery: json!({}),
            worktree_repository_key: None,
            created_at: "2026-08-31T00:00:00Z".to_owned(),
            updated_at: "2026-08-31T00:00:00Z".to_owned(),
            deleted_at: None,
            repository_identity: None,
        })
        .await
        .expect("project projection");
    let repository = Arc::new(GitRepository::default());
    let broadcaster = StatusBroadcaster::new(repository.clone(), Duration::from_secs(3_600), 8);
    let availability = WorkspaceAvailabilityRegistry::new();
    let catalog = WorktreeCatalogService::new_with_availability_registry(
        Arc::new(repositories.clone()),
        repository.clone(),
        availability.clone(),
    );
    let trash = Arc::new(TestTrash::new(trash_directory.path().to_path_buf()));
    let services = GitManagerRpcServices::with_dependencies(
        repository.clone(),
        broadcaster,
        catalog.clone(),
        repositories,
        availability,
        trash.clone(),
    );
    let mut registry = RpcRegistry::empty();
    register_git_manager_rpc(&mut registry, services);
    register_git_vcs_rpc(
        &mut registry,
        GitVcsRpcServices::with_repository(repository, Arc::default()),
    );
    let config = ServerConfig::new(state.path())
        .with_bind("127.0.0.1", 0)
        .with_desktop(DESKTOP_BOOTSTRAP)
        .expect("desktop server configuration");
    let handle = ServerRuntime::start_with_registry(config, registry)
        .await
        .expect("Git Manager server starts");
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP).await;
    let mut socket = authenticated_socket(&client, &handle, &administrator).await;
    let cwd = repository_path.to_string_lossy().into_owned();

    fs::write(repository_path.join("tracked.txt"), "first commit\n").expect("first change");
    request(
        &mut socket,
        "1",
        "vcs.stageFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt"] }),
    )
    .await;
    success(&mut socket, "1").await;
    request(
        &mut socket,
        "2",
        "gitManager.commit",
        json!({
            "cwd": cwd,
            "summary": "Commit summary",
            "description": "Description with --anything and newlines\nsecond line",
            "amend": false,
            "noVerify": true,
            "signoff": true,
            "allowEmpty": false,
            "coAuthors": [{ "name": "Ann Author", "email": "ann@example.test" }]
        }),
    )
    .await;
    let committed = success(&mut socket, "2").await;
    assert_eq!(committed["empty"], false);
    assert_eq!(
        committed["sha"],
        git_stdout(&repository_path, &["rev-parse", "HEAD"]).trim()
    );
    let first_message = git_stdout(&repository_path, &["log", "-1", "--format=%B"]);
    assert!(first_message.contains("Description with --anything and newlines\nsecond line"));
    assert!(first_message.contains("Co-Authored-By: Ann Author <ann@example.test>"));
    assert!(first_message.contains("Signed-off-by: Git Manager Test <git-manager@example.test>"));

    let before_amend = committed["sha"].as_str().expect("commit sha").to_owned();
    let before_count = git_stdout(&repository_path, &["rev-list", "--count", "HEAD"]);
    fs::write(repository_path.join("tracked.txt"), "amended\n").expect("amend change");
    request(
        &mut socket,
        "3",
        "vcs.stageFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt"] }),
    )
    .await;
    success(&mut socket, "3").await;
    request(
        &mut socket,
        "4",
        "gitManager.commit",
        json!({
            "cwd": cwd,
            "summary": "Amended summary",
            "description": "Amended description",
            "amend": true,
            "noVerify": false,
            "signoff": false,
            "allowEmpty": false,
            "coAuthors": [{ "name": "Bob Builder", "email": "bob@example.test" }]
        }),
    )
    .await;
    let amended = success(&mut socket, "4").await;
    assert_ne!(amended["sha"], before_amend);
    assert_eq!(
        git_stdout(&repository_path, &["rev-list", "--count", "HEAD"]),
        before_count
    );
    let amended_message = git_stdout(&repository_path, &["log", "-1", "--format=%B"]);

    request(
        &mut socket,
        "5",
        "gitManager.undoCommit",
        json!({ "cwd": cwd }),
    )
    .await;
    let undone = success(&mut socket, "5").await;
    assert_eq!(undone["summary"], "Amended summary");
    assert_eq!(undone["description"], "Amended description");
    assert_eq!(undone["coAuthors"][0]["name"], "Bob Builder");
    assert_eq!(
        fs::read_to_string(repository_path.join("tracked.txt")).expect("working file"),
        "amended\n"
    );

    request(
        &mut socket,
        "6",
        "vcs.stageFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt"] }),
    )
    .await;
    success(&mut socket, "6").await;
    request(
        &mut socket,
        "7",
        "gitManager.commit",
        json!({
            "cwd": cwd,
            "summary": undone["summary"],
            "description": undone["description"],
            "amend": false,
            "noVerify": false,
            "signoff": false,
            "allowEmpty": false,
            "coAuthors": undone["coAuthors"]
        }),
    )
    .await;
    success(&mut socket, "7").await;
    assert_eq!(
        git_stdout(&repository_path, &["log", "-1", "--format=%B"]),
        amended_message
    );

    request(
        &mut socket,
        "8",
        "gitManager.undoCommit",
        json!({ "cwd": cwd }),
    )
    .await;
    let undone_again = success(&mut socket, "8").await;
    assert_eq!(undone_again, undone);
    request(
        &mut socket,
        "9",
        "vcs.stageFiles",
        json!({ "cwd": cwd, "filePaths": ["tracked.txt"] }),
    )
    .await;
    success(&mut socket, "9").await;
    request(
        &mut socket,
        "10",
        "gitManager.commit",
        json!({
            "cwd": cwd,
            "summary": undone_again["summary"],
            "description": undone_again["description"],
            "amend": false,
            "noVerify": false,
            "signoff": false,
            "allowEmpty": false,
            "coAuthors": undone_again["coAuthors"]
        }),
    )
    .await;
    success(&mut socket, "10").await;
    assert_eq!(
        git_stdout(&repository_path, &["log", "-1", "--format=%B"]),
        amended_message
    );

    request(
        &mut socket,
        "11",
        "gitManager.undoCommit",
        json!({ "cwd": cwd }),
    )
    .await;
    assert_eq!(success(&mut socket, "11").await, undone);

    request(
        &mut socket,
        "12",
        "gitManager.discard",
        json!({ "cwd": cwd, "paths": ["tracked.txt"], "permitPermanent": false }),
    )
    .await;
    let discarded = success(&mut socket, "12").await;
    assert_eq!(discarded["trashed"], json!(["tracked.txt"]));
    assert_eq!(
        fs::read_to_string(repository_path.join("tracked.txt")).expect("restored file"),
        "base\n"
    );

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
    let mut read_socket = authenticated_socket(&client, &handle, &restricted).await;
    request(
        &mut read_socket,
        "13",
        "gitManager.commit",
        json!({
            "cwd": cwd,
            "summary": "Denied",
            "description": "",
            "amend": false,
            "noVerify": false,
            "signoff": false,
            "allowEmpty": false,
            "coAuthors": []
        }),
    )
    .await;
    let denied = failure(&mut read_socket, "13").await;
    assert_eq!(denied["_tag"], "EnvironmentAuthorizationError");
    assert_eq!(denied["requiredScope"], "orchestration:operate");

    fs::write(repository_path.join("tracked.txt"), "blocking discard\n")
        .expect("blocking discard change");
    trash.block_next();
    request(
        &mut socket,
        "14",
        "gitManager.discard",
        json!({ "cwd": cwd, "paths": ["tracked.txt"], "permitPermanent": false }),
    )
    .await;
    trash.wait_until_started().await;
    let mut second_socket = authenticated_socket(&client, &handle, &administrator).await;
    request(
        &mut second_socket,
        "15",
        "gitManager.commit",
        json!({
            "cwd": cwd,
            "summary": "Concurrent",
            "description": "",
            "amend": false,
            "noVerify": false,
            "signoff": false,
            "allowEmpty": false,
            "coAuthors": []
        }),
    )
    .await;
    let concurrent = failure(&mut second_socket, "15").await;
    assert_eq!(concurrent["_tag"], "GitManagerOperationError");
    assert_eq!(concurrent["code"], "operation-in-flight");
    assert_eq!(concurrent["blocked"]["code"], "operation-in-flight");
    trash.release();
    success(&mut socket, "14").await;

    second_socket
        .close(None)
        .await
        .expect("close second socket");
    read_socket.close(None).await.expect("close read socket");
    socket.close(None).await.expect("close WebSocket");
    handle.shutdown();
    timeout(Duration::from_secs(5), handle.join())
        .await
        .expect("server shutdown timeout")
        .expect("server joins");
}

fn initialize_repository(repository: &Path) {
    git(repository, &["init", "-q", "-b", "main"]);
    git(repository, &["config", "core.autocrlf", "false"]);
    git(repository, &["config", "user.name", "Git Manager Test"]);
    git(
        repository,
        &["config", "user.email", "git-manager@example.test"],
    );
    fs::write(repository.join("tracked.txt"), "base\n").expect("tracked fixture file");
    git(repository, &["add", "tracked.txt"]);
    git(repository, &["commit", "-q", "-m", "base"]);
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

async fn authenticated_socket(
    client: &Client,
    handle: &ServerHandle,
    token: &Value,
) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
    let ticket = response_json(
        client
            .post(http_url(handle, "/api/auth/websocket-ticket"))
            .bearer_auth(access_token(token))
            .send()
            .await
            .expect("WebSocket ticket request"),
        StatusCode::OK,
    )
    .await["ticket"]
        .as_str()
        .expect("WebSocket ticket")
        .to_owned();
    connect_async(format!("ws://{}/ws?wsTicket={ticket}", handle.local_addr()))
        .await
        .expect("WebSocket connects")
        .0
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
