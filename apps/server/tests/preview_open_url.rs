use std::{path::Path, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header},
    response::IntoResponse,
};
use coddy_studio_server::{
    RpcExit, RpcRegistry, ServerConfig, ServerMessage, ServerRuntime, mcp,
    preview::{
        MAX_PENDING_OPEN_REQUESTS, PreviewManager,
        gateway::{
            proxy::PrincipalCheck,
            registry::{PreviewGateway, gateway_context},
            relay::RelayHosts,
        },
    },
    production::{
        connect_mcp::{
            ConnectMcpConfig, ConnectMcpService, DecodedCloudProof, EndpointRuntime, JwtCodec,
            PairingIssuer, PreviewInvoker,
        },
        http_routes::{HttpRouteError, HttpRoutesState, RouteContext, add_routes},
        workspace_preview::{WorkspacePreviewRpcServices, register_workspace_preview_rpc},
    },
    workspace::{WorkspaceRpc, WorkspaceService},
};
use futures_util::{SinkExt, future::BoxFuture};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

async fn setup_service(temp: &TempDir) -> ConnectMcpService {
    let jwt = JwtCodec::new(
        |_typ, _payload| async move { Err("unused".to_owned()) },
        |_key, _typ, _token, _issuer, _audience, _now| async move {
            Err::<DecodedCloudProof, _>("unused".to_owned())
        },
        || async { Err("unused".to_owned()) },
    );
    let endpoint = EndpointRuntime::new(|_config| async move { Ok(json!({"status":"disabled"})) });
    let pairing = PairingIssuer::new(|_thumbprint| async move { Err("unused".to_owned()) });
    let preview = PreviewInvoker::new(|_scope, _operation, _input, _tab, _cancellation| async {
        Ok(json!({}))
    });
    ConnectMcpService::open(
        temp.path().join("connect.sqlite3"),
        ConnectMcpConfig {
            environment_id: "env-1".into(),
            descriptor: json!({"environmentId":"env-1"}),
            mcp_endpoint: "http://127.0.0.1:43123/mcp".into(),
            open_url_endpoint: "http://127.0.0.1:43123/api/preview/open-url".into(),
            now_epoch_seconds: Arc::new(|| 1_700_000_000),
            max_mcp_credentials: 4,
            max_mcp_sessions: 4,
        },
        jwt,
        endpoint,
        pairing,
        preview,
        None,
    )
    .await
    .expect("connect service")
}

fn bearer(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    headers
}

fn context(headers: HeaderMap) -> RouteContext {
    RouteContext {
        headers,
        uri: Uri::from_static("http://127.0.0.1:43123/mcp"),
        cancellation: CancellationToken::new(),
    }
}

fn initialize_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}
    }))
    .unwrap()
}

fn unavailable() -> HttpRouteError {
    HttpRouteError::new(StatusCode::SERVICE_UNAVAILABLE, json!({}))
}

/// The real open-url route over a real credential store and preview manager; every other
/// route is stubbed out.
fn router(connect: Arc<ConnectMcpService>, preview: PreviewManager) -> Router {
    let mut state = HttpRoutesState::new(
        Arc::new(|_headers, _method, _uri, _scope, _cancellation| {
            Box::pin(async { Err(StatusCode::UNAUTHORIZED.into_response()) })
        }),
        Arc::new(|_operation, _payload, _context| Box::pin(async { Err(unavailable()) })),
        Arc::new(|_log, _include_crash_dumps, _context| Box::pin(async { Err(unavailable()) })),
        Arc::new(|_token, _path, _context| Box::pin(async { Err(unavailable()) })),
        Arc::new(|_method, _body, _context| Box::pin(async { Err(unavailable()) })),
        Arc::new(|_token, _context| Box::pin(async { Err(unavailable()) })),
        Arc::new(|_token, _name, _overwrite, _body, _context| {
            Box::pin(async { Err(unavailable()) })
        }),
    );
    state.open_url = Arc::new(move |body, context| {
        let connect = Arc::clone(&connect);
        let preview = preview.clone();
        Box::pin(async move { connect.open_url_http(&preview, body, context).await })
    });
    add_routes(Router::new()).with_state(state)
}

async fn post_open_url(app: &Router, headers: HeaderMap, body: Value) -> (StatusCode, Value) {
    let mut request = Request::post("/api/preview/open-url")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.headers_mut().extend(headers);
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn open_url_route_accepts_bearer_token_and_emits_open_request() {
    let temp = TempDir::new().unwrap();
    let connect = Arc::new(setup_service(&temp).await);
    let preview = PreviewManager::new();
    let mut events = preview.subscribe_events();
    let app = router(Arc::clone(&connect), preview.clone());
    let credential = connect
        .issue_open_url_credential("thread-1")
        .await
        .expect("open-url credential");
    assert_eq!(credential.expires_at, 1_700_000_000 + 8 * 60 * 60);

    let (status, body) = post_open_url(
        &app,
        bearer(&credential.token),
        json!({"url": "http://localhost:5173/"}),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivered"], true);
    let request_id = body["requestId"].as_str().expect("request id").to_owned();
    let event = serde_json::to_value(events.recv().await.expect("event")).unwrap();
    assert_eq!(event["type"], "openRequested");
    assert_eq!(event["threadId"], "thread-1");
    assert_eq!(event["requestId"], request_id.as_str());
    assert_eq!(event["url"], "http://localhost:5173/");
    assert!(preview.claim_open_request(&request_id).await);
}

struct LivePrincipal;

impl PrincipalCheck for LivePrincipal {
    fn session_expiry<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Option<i64>> {
        Box::pin(async { Some(i64::MAX) })
    }
}

#[tokio::test]
async fn open_url_route_reports_whether_a_client_could_see_the_request() {
    let temp = TempDir::new().unwrap();
    let connect = Arc::new(setup_service(&temp).await);
    let preview = PreviewManager::new();
    // The production gateway follows preview events itself; it is not a client.
    let gateway = PreviewGateway::new(
        "127.0.0.1",
        "devbox",
        gateway_context(Arc::new(LivePrincipal), vec![1; 32], "session".into()),
        preview.clone(),
        RelayHosts::default(),
    );
    let app = router(Arc::clone(&connect), preview.clone());
    let credential = connect.issue_open_url_credential("thread-1").await.unwrap();
    let post = || {
        post_open_url(
            &app,
            bearer(&credential.token),
            json!({"url": "http://localhost:5173/"}),
        )
    };

    let (status, body) = post().await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivered"], false);
    assert!(body["requestId"].is_string());

    let _client = preview.subscribe_events();
    let (status, body) = post().await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivered"], true);
    gateway.shutdown().await;
}

#[tokio::test]
async fn open_url_route_answers_429_past_the_pending_request_cap() {
    let temp = TempDir::new().unwrap();
    let connect = Arc::new(setup_service(&temp).await);
    let app = router(Arc::clone(&connect), PreviewManager::new());
    let credential = connect.issue_open_url_credential("thread-1").await.unwrap();
    let body = json!({"url": "http://localhost:5173/"});
    for _ in 0..MAX_PENDING_OPEN_REQUESTS {
        let (status, _) = post_open_url(&app, bearer(&credential.token), body.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, response) = post_open_url(&app, bearer(&credential.token), body).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response["error"], "too_many_open_requests");
}

#[tokio::test]
async fn open_url_route_rejects_bad_tokens_and_cookies() {
    let temp = TempDir::new().unwrap();
    let connect = Arc::new(setup_service(&temp).await);
    let preview = PreviewManager::new();
    let app = router(Arc::clone(&connect), preview);
    let mcp = connect
        .issue_mcp_credential("thread-1", "codex-1")
        .await
        .unwrap();
    let mcp_token = mcp.authorization_header.strip_prefix("Bearer ").unwrap();
    let body = json!({"url": "http://localhost:5173/"});

    let mut cookie = HeaderMap::new();
    cookie.insert(
        header::COOKIE,
        HeaderValue::from_static("coddy_studio_session=session-token"),
    );
    for headers in [
        HeaderMap::new(),
        bearer("not-a-token"),
        bearer(mcp_token),
        cookie,
    ] {
        let (status, response) = post_open_url(&app, headers, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(response["error"], "invalid_open_url_credential");
    }
}

#[tokio::test]
async fn open_url_route_rejects_non_http_urls() {
    let temp = TempDir::new().unwrap();
    let connect = Arc::new(setup_service(&temp).await);
    let app = router(Arc::clone(&connect), PreviewManager::new());
    let credential = connect.issue_open_url_credential("thread-1").await.unwrap();

    for body in [
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "javascript:alert(1)"}),
        json!({"url": ""}),
        json!({}),
    ] {
        let (status, _) = post_open_url(&app, bearer(&credential.token), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn open_url_token_does_not_authorize_mcp() {
    let temp = TempDir::new().unwrap();
    let connect = setup_service(&temp).await;
    let credential = connect.issue_open_url_credential("thread-1").await.unwrap();

    let error = match connect
        .mcp(
            Method::POST,
            initialize_body(),
            context(bearer(&credential.token)),
        )
        .await
    {
        Ok(_) => panic!("open-url token authorized MCP"),
        Err(error) => error,
    };
    assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error.body()["error"], "invalid_mcp_credential");
}

#[tokio::test]
async fn mcp_credential_survives_open_url_issue() {
    let temp = TempDir::new().unwrap();
    let connect = setup_service(&temp).await;
    let mcp = connect
        .issue_mcp_credential("thread-1", "codex-1")
        .await
        .unwrap();
    let first = connect.issue_open_url_credential("thread-1").await.unwrap();
    let second = connect.issue_open_url_credential("thread-1").await.unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&mcp.authorization_header).unwrap(),
    );
    let initialized = connect
        .mcp(Method::POST, initialize_body(), context(headers))
        .await
        .expect("MCP token still valid");
    assert_eq!(initialized.status, 200);
    // A second terminal on the same thread must not revoke the first one's token.
    assert_eq!(
        connect.verify_open_url_credential(&first.token).await,
        Some("thread-1".to_owned())
    );
    assert_eq!(
        connect.verify_open_url_credential(&second.token).await,
        Some("thread-1".to_owned())
    );
}

#[tokio::test]
async fn claim_open_request_rpc_is_single_use() {
    let temp = TempDir::new().unwrap();
    let preview = PreviewManager::new();
    let mut registry = RpcRegistry::empty();
    register_workspace_preview_rpc(
        &mut registry,
        WorkspacePreviewRpcServices::new(
            WorkspaceRpc::new(WorkspaceService::default()),
            preview.clone(),
            mcp::preview_automation::PreviewAutomationBroker::new(),
        ),
    );
    let handle = ServerRuntime::start_with_registry(
        ServerConfig::new(temp.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
        registry,
    )
    .await
    .expect("server starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");
    let request_id = preview
        .request_open("thread-1", "http://localhost:5173")
        .await
        .unwrap()
        .request_id;

    for (id, expected) in [("1", true), ("2", false)] {
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": id,
                    "tag": "preview.claimOpenRequest",
                    "payload": {"requestId": request_id},
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let message = timeout(
            Duration::from_secs(2),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        .expect("response")
        .expect("open")
        .expect("frame");
        let Message::Text(text) = message else {
            panic!("expected text frame");
        };
        let message: ServerMessage = serde_json::from_str(&text).unwrap();
        assert!(
            matches!(
                &message,
                ServerMessage::Exit {
                    exit: RpcExit::Success { value: Some(value) },
                    ..
                } if *value == json!({"claimed": expected})
            ),
            "unexpected claim response: {message:?}"
        );
    }
}

/// Runs the real `coddy-studio open-url` the shim runs, with `env` as its only open-url settings.
async fn run_open_url_cli(url: &str, env: &[(&str, &str)]) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_coddy-studio"));
    command
        .args(["open-url", url])
        .env_remove("CODDY_STUDIO_OPEN_URL_ENDPOINT")
        .env_remove("CODDY_STUDIO_OPEN_URL_AUTH");
    for (key, value) in env {
        command.env(key, value);
    }
    timeout(Duration::from_secs(20), command.output())
        .await
        .expect("open-url exits")
        .expect("open-url runs")
}

#[tokio::test]
async fn open_url_cli_posts_to_the_route_and_emits_open_request() {
    let temp = TempDir::new().unwrap();
    let connect = Arc::new(setup_service(&temp).await);
    let preview = PreviewManager::new();
    let mut events = preview.subscribe_events();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/preview/open-url",
        listener.local_addr().unwrap()
    );
    let app = router(Arc::clone(&connect), preview.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let credential = connect.issue_open_url_credential("thread-1").await.unwrap();
    let url = "http://localhost:5173/?key=abc";

    let output = run_open_url_cli(
        url,
        &[
            ("CODDY_STUDIO_OPEN_URL_ENDPOINT", &endpoint),
            ("CODDY_STUDIO_OPEN_URL_AUTH", &credential.token),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let event = serde_json::to_value(events.recv().await.expect("event")).unwrap();
    assert_eq!(event["type"], "openRequested");
    assert_eq!(event["threadId"], "thread-1");
    assert_eq!(event["url"], url);

    // With no client subscribed nobody will open it, so the URL is printed for a human.
    drop(events);
    let output = run_open_url_cli(
        url,
        &[
            ("CODDY_STUDIO_OPEN_URL_ENDPOINT", &endpoint),
            ("CODDY_STUDIO_OPEN_URL_AUTH", &credential.token),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), format!("{url}\n"));
    assert!(output.stderr.is_empty(), "{output:?}");

    // A refused token still succeeds for the caller: the URL is printed for a human.
    let output = run_open_url_cli(
        url,
        &[
            ("CODDY_STUDIO_OPEN_URL_ENDPOINT", &endpoint),
            ("CODDY_STUDIO_OPEN_URL_AUTH", "not-a-token"),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), format!("{url}\n"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("401"));
    server.abort();
}

/// The Windows `coddy-studio-open-url.exe` alias is the server binary under another name; it takes
/// the URL as its only argument. Dispatch is by name, so it is exercised on every platform.
#[tokio::test]
async fn open_url_alias_posts_the_url_and_rejects_a_missing_one() {
    let temp = TempDir::new().unwrap();
    let alias = temp.path().join(format!(
        "coddy-studio-open-url{}",
        std::env::consts::EXE_SUFFIX
    ));
    executable_fixture::copy_executable(Path::new(env!("CARGO_BIN_EXE_coddy-studio")), &alias);
    let connect = Arc::new(setup_service(&temp).await);
    let preview = PreviewManager::new();
    let mut events = preview.subscribe_events();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/preview/open-url",
        listener.local_addr().unwrap()
    );
    let app = router(Arc::clone(&connect), preview.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let credential = connect.issue_open_url_credential("thread-1").await.unwrap();
    let url = "http://localhost:5173/?a=\"b\"&c";
    let run = |args: Vec<&str>| {
        let mut command = tokio::process::Command::new(&alias);
        command
            .args(args)
            .env("CODDY_STUDIO_OPEN_URL_ENDPOINT", &endpoint)
            .env("CODDY_STUDIO_OPEN_URL_AUTH", &credential.token);
        async move {
            timeout(Duration::from_secs(20), command.output())
                .await
                .expect("alias exits")
                .expect("alias runs")
        }
    };

    let output = run(vec![url]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let event = serde_json::to_value(events.recv().await.expect("event")).unwrap();
    assert_eq!(event["type"], "openRequested");
    // The raw quote arrived as data; the server stores the URL normalized.
    assert_eq!(event["url"], "http://localhost:5173/?a=%22b%22&c");

    let output = run(Vec::new()).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "coddy-studio open-url: expected an http(s) URL\n"
    );
    server.abort();
}

#[tokio::test]
async fn open_url_cli_without_a_session_prints_the_url_and_rejects_non_http() {
    let output = run_open_url_cli("https://example.com/a?b=$(c)", &[]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "https://example.com/a?b=$(c)\n"
    );

    let output = run_open_url_cli("file:///etc/passwd", &[]).await;
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "coddy-studio open-url: expected an http(s) URL\n"
    );
}
