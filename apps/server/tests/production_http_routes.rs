use std::{collections::BTreeMap, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use coddy_studio_server::production::http_routes::{
    AssetHttpResponse, Authorized, DiagnosticLogsHttpResponse, HttpRouteError, HttpRoutesState,
    JsonOperation, JsonRouteResponse, MAX_DIAGNOSTIC_BODY_BYTES, MAX_JSON_BODY_BYTES,
    McpHttpResponse, RouteContext, add_routes,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};
use tower::ServiceExt;

#[derive(Clone)]
struct TestState(HttpRoutesState);

impl axum::extract::FromRef<TestState> for HttpRoutesState {
    fn from_ref(state: &TestState) -> Self {
        state.0.clone()
    }
}

#[tokio::test]
async fn routes_apply_exact_scopes_and_preserve_json_wire_shapes() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = state_with_json_recorder(Arc::clone(&calls));
    let app = add_routes(Router::new()).with_state(TestState(state));
    let cases = [
        (
            "GET",
            "/api/orchestration/snapshot",
            JsonOperation::OrchestrationSnapshot,
            Some("orchestration:read"),
            None,
        ),
        (
            "POST",
            "/api/orchestration/dispatch",
            JsonOperation::OrchestrationDispatch,
            Some("orchestration:operate"),
            Some(json!({"_tag":"project.create","commandId":"c1"})),
        ),
        (
            "POST",
            "/api/connect/link-proof",
            JsonOperation::ConnectLinkProof,
            Some("relay:write"),
            Some(json!({"cloudOrigin":"https://cloud.example"})),
        ),
        (
            "POST",
            "/api/connect/relay-config",
            JsonOperation::ConnectRelayConfig,
            Some("relay:write"),
            Some(json!({"relayUrl":"https://relay.example"})),
        ),
        (
            "GET",
            "/api/connect/link-state",
            JsonOperation::ConnectLinkState,
            Some("relay:read"),
            None,
        ),
        (
            "POST",
            "/api/connect/unlink",
            JsonOperation::ConnectUnlink,
            Some("relay:write"),
            None,
        ),
        (
            "POST",
            "/api/coddy-connect/health",
            JsonOperation::ConnectHealth,
            None,
            Some(json!({"environmentId":"env-1"})),
        ),
        (
            "POST",
            "/api/connect/mint-credential",
            JsonOperation::ConnectMintCredential,
            None,
            Some(json!({"environmentId":"env-1"})),
        ),
        (
            "POST",
            "/api/coddy-connect/mint-credential",
            JsonOperation::ConnectMintCredential,
            None,
            Some(json!({"environmentId":"env-1"})),
        ),
    ];

    for (method, uri, operation, scope, payload) in &cases {
        let mut request = Request::builder().method(*method).uri(*uri);
        if scope.is_some() {
            request = request.header(header::AUTHORIZATION, "Bearer test-token");
        }
        let body = payload
            .as_ref()
            .map_or_else(Body::empty, |value| Body::from(value.to_string()));
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method} {uri}");
        let value: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(
            value,
            json!({"operation": operation.as_str(), "wire": "unchanged"})
        );
    }

    let calls = calls.lock().await;
    assert_eq!(calls.len(), cases.len());
    for (
        (operation, scope, payload),
        (_, _, expected_operation, expected_scope, expected_payload),
    ) in calls.iter().zip(cases)
    {
        assert_eq!(*operation, Some(expected_operation));
        assert_eq!(scope.as_deref(), expected_scope);
        assert_eq!(payload, &expected_payload);
    }

    drop(calls);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(concat!("/api/observability", "/v1/traces"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn json_routes_reject_oversized_and_malformed_bodies_before_service_dispatch() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = state_with_json_recorder(Arc::clone(&calls));
    let app = add_routes(Router::new()).with_state(TestState(state));

    let oversized = app
        .clone()
        .oneshot(
            Request::post("/api/orchestration/dispatch")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::from(vec![b'x'; MAX_JSON_BODY_BYTES + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let malformed = app
        .oneshot(
            Request::post("/api/orchestration/dispatch")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert!(
        calls
            .lock()
            .await
            .iter()
            .all(|(operation, _, _)| operation.is_none())
    );
}

#[tokio::test]
async fn crash_dumps_in_the_diagnostic_bundle_need_the_operate_scope() {
    let dispatched = Arc::new(Mutex::new(Vec::new()));
    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    // A client that may read, but not operate.
    state.authorize = Arc::new(|_headers, _method, _uri, _scope, _cancellation| {
        Box::pin(async move {
            Ok(Authorized {
                scopes: vec!["orchestration:read".to_owned()],
            })
        })
    });
    let handler_calls = Arc::clone(&dispatched);
    state.diagnostic_logs = Arc::new(move |_frontend_log, include_crash_dumps, _context| {
        let calls = Arc::clone(&handler_calls);
        Box::pin(async move {
            calls.lock().await.push(include_crash_dumps);
            Ok(DiagnosticLogsHttpResponse {
                filename: "coddy-studio-diagnostics-20260715T123456Z.zip".to_owned(),
                bytes: b"PK-test-archive".to_vec(),
            })
        })
    });
    let app = add_routes(Router::new()).with_state(TestState(state));
    let post = |body: &'static str| {
        Request::post("/api/diagnostics/logs.zip")
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    };

    let redacted = app
        .clone()
        .oneshot(post(r#"{"frontendLog":"x"}"#))
        .await
        .unwrap();
    let with_dumps = app
        .oneshot(post(r#"{"frontendLog":"x","includeCrashDumps":true}"#))
        .await
        .unwrap();

    assert_eq!(redacted.status(), StatusCode::OK);
    assert_eq!(with_dumps.status(), StatusCode::FORBIDDEN);
    assert_eq!(dispatched.lock().await.as_slice(), [false]);
}

#[tokio::test]
async fn diagnostic_logs_route_is_bounded_authorized_and_returns_zip_headers() {
    let authorization_calls = Arc::new(Mutex::new(Vec::new()));
    let dispatched_logs = Arc::new(Mutex::new(Vec::new()));
    let mut state = state_with_json_recorder(Arc::clone(&authorization_calls));
    let handler_calls = Arc::clone(&dispatched_logs);
    state.diagnostic_logs = Arc::new(move |frontend_log, include_crash_dumps, context| {
        let calls = Arc::clone(&handler_calls);
        Box::pin(async move {
            assert!(!context.cancellation.is_cancelled());
            calls.lock().await.push((frontend_log, include_crash_dumps));
            Ok(DiagnosticLogsHttpResponse {
                filename: "coddy-studio-diagnostics-20260715T123456Z.zip".to_owned(),
                bytes: b"PK-test-archive".to_vec(),
            })
        })
    });
    let app = add_routes(Router::new()).with_state(TestState(state));

    let response = app
        .clone()
        .oneshot(
            Request::post("/api/diagnostics/logs.zip")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"frontendLog":"client warning"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"coddy-studio-diagnostics-20260715T123456Z.zip\""
    );
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        b"PK-test-archive"
    );
    assert_eq!(
        dispatched_logs.lock().await.as_slice(),
        [("client warning".to_owned(), false)]
    );
    assert!(
        authorization_calls
            .lock()
            .await
            .iter()
            .any(|(_, scope, _)| { scope.as_deref() == Some("orchestration:read") })
    );

    let calls_before = authorization_calls.lock().await.len();
    let with_dumps = app
        .clone()
        .oneshot(
            Request::post("/api/diagnostics/logs.zip")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"frontendLog":"with dumps","includeCrashDumps":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(with_dumps.status(), StatusCode::OK);
    // One credential check per request: a DPoP proof is single-use, so a second check of the
    // same headers would be refused as a replay.
    assert_eq!(
        authorization_calls.lock().await.len() - calls_before,
        1,
        "the request was authorized exactly once"
    );
    assert_eq!(
        dispatched_logs.lock().await.last(),
        Some(&("with dumps".to_owned(), true))
    );

    let not_a_boolean = app
        .clone()
        .oneshot(
            Request::post("/api/diagnostics/logs.zip")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"frontendLog":"x","includeCrashDumps":"yes"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(not_a_boolean.status(), StatusCode::BAD_REQUEST);

    let malformed = app
        .clone()
        .oneshot(
            Request::post("/api/diagnostics/logs.zip")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    let oversized = app
        .oneshot(
            Request::post("/api/diagnostics/logs.zip")
                .header(header::AUTHORIZATION, "Bearer test-token")
                .body(Body::from(vec![b'x'; MAX_DIAGNOSTIC_BODY_BYTES + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(dispatched_logs.lock().await.len(), 2);
}

#[tokio::test]
async fn script_capable_assets_are_sandboxed() {
    for (path, content_type, expected) in [
        (
            "page.html",
            "text/html; charset=utf-8",
            Some("sandbox allow-scripts allow-forms allow-popups"),
        ),
        (
            "icon.svg",
            "image/svg+xml",
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        ),
        (
            "upper.html",
            "TEXT/HTML",
            Some("sandbox allow-scripts allow-forms allow-popups"),
        ),
        (
            "page.xhtml",
            "application/xhtml+xml",
            Some("sandbox allow-scripts allow-forms allow-popups"),
        ),
        (
            "a.xml",
            "text/xml",
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        ),
        (
            "b.xml",
            "application/xml",
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        ),
        (
            "feed.rss",
            "application/rss+xml",
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        ),
        (
            "feed.atom",
            "application/atom+xml; charset=utf-8",
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        ),
        (
            "math.mml",
            "application/mathml+xml",
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
        ),
        ("doc.pdf", "application/pdf", None),
    ] {
        let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
        let content_type = content_type.to_owned();
        state.assets = Arc::new(move |_token, _path, _context| {
            let content_type = content_type.clone();
            Box::pin(async move {
                Ok(AssetHttpResponse {
                    content_type,
                    bytes: b"x".to_vec(),
                    cache_control: "private, max-age=3600".to_owned(),
                })
            })
        });
        let app = add_routes(Router::new()).with_state(TestState(state));
        let response = app
            .oneshot(
                Request::get(format!("/api/assets/t/{path}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get("content-security-policy")
                .map(|v| v.to_str().unwrap()),
            expected,
            "{path}"
        );
    }
}

#[tokio::test]
async fn assets_and_mcp_use_native_handlers_with_protocol_headers() {
    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.assets = Arc::new(|token, path, _context| {
        Box::pin(async move {
            assert_eq!(token, "signed-token");
            assert_eq!(path, "nested/icon.svg");
            Ok(AssetHttpResponse {
                content_type: "image/svg+xml".to_owned(),
                bytes: b"<svg/>".to_vec(),
                cache_control: "private, max-age=3600".to_owned(),
            })
        })
    });
    state.mcp = Arc::new(|method, body, _context| {
        Box::pin(async move {
            assert_eq!(body.as_slice(), b"{}");
            Ok(McpHttpResponse {
                status: if method == Method::POST { 200 } else { 204 },
                headers: BTreeMap::from([("mcp-session-id".to_owned(), "session-1".to_owned())]),
                body: Vec::new(),
            })
        })
    });
    let app = add_routes(Router::new()).with_state(TestState(state));

    let asset = app
        .clone()
        .oneshot(
            Request::get("/api/assets/signed-token/nested/icon.svg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(asset.headers()[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(
        asset.headers()[header::CACHE_CONTROL],
        "private, max-age=3600"
    );
    assert_eq!(asset.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        asset.headers()[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; style-src 'unsafe-inline'; sandbox"
    );
    assert_eq!(
        to_bytes(asset.into_body(), 1024).await.unwrap().as_ref(),
        b"<svg/>"
    );

    let post = app
        .clone()
        .oneshot(Request::post("/mcp").body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::ACCEPTED);
    assert_eq!(post.headers()["mcp-session-id"], "session-1");

    let delete = app
        .oneshot(Request::delete("/mcp").body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn dropping_a_request_cancels_the_native_operation() {
    let (started_tx, started_rx) = oneshot::channel();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let cancelled_tx = Arc::new(Mutex::new(Some(cancelled_tx)));
    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.json = Arc::new(move |_operation, _payload, context| {
        let started_tx = Arc::clone(&started_tx);
        let cancelled_tx = Arc::clone(&cancelled_tx);
        Box::pin(async move {
            if let Some(sender) = started_tx.lock().await.take() {
                let _ = sender.send(());
            }
            tokio::spawn(async move {
                context.cancellation.cancelled().await;
                if let Some(sender) = cancelled_tx.lock().await.take() {
                    let _ = sender.send(());
                }
            });
            std::future::pending::<Result<JsonRouteResponse, HttpRouteError>>().await
        })
    });
    let app = add_routes(Router::new()).with_state(TestState(state));
    let request = Request::get("/api/orchestration/snapshot")
        .header(header::AUTHORIZATION, "Bearer test-token")
        .body(Body::empty())
        .unwrap();
    let task = tokio::spawn(app.oneshot(request));
    tokio::time::timeout(Duration::from_secs(1), started_rx)
        .await
        .expect("handler start")
        .expect("start notification");
    task.abort();
    tokio::time::timeout(Duration::from_secs(1), cancelled_rx)
        .await
        .expect("handler cancellation")
        .expect("cancellation notification");
}

type JsonRecorderCall = (Option<JsonOperation>, Option<String>, Option<Value>);

fn state_with_json_recorder(calls: Arc<Mutex<Vec<JsonRecorderCall>>>) -> HttpRoutesState {
    let authorization_calls = Arc::clone(&calls);
    let json_calls = Arc::clone(&calls);
    HttpRoutesState::new(
        Arc::new(move |_headers, _method, _uri, scope, _cancellation| {
            let calls = Arc::clone(&authorization_calls);
            Box::pin(async move {
                calls
                    .lock()
                    .await
                    .push((None, scope.map(str::to_owned), None));
                Ok(Authorized {
                    scopes: vec![
                        "orchestration:read".to_owned(),
                        "orchestration:operate".to_owned(),
                    ],
                })
            })
        }),
        Arc::new(move |operation, payload, context: RouteContext| {
            let calls = Arc::clone(&json_calls);
            Box::pin(async move {
                let mut calls = calls.lock().await;
                if let Some((probe, scope, _)) = calls.last() {
                    if probe.is_none() {
                        let scope = scope.clone();
                        calls.pop();
                        calls.push((Some(operation), scope, payload));
                    } else {
                        calls.push((Some(operation), None, payload));
                    }
                } else {
                    calls.push((Some(operation), None, payload));
                }
                assert!(!context.cancellation.is_cancelled());
                Ok(JsonRouteResponse::ok(json!({
                    "operation": operation.as_str(),
                    "wire": "unchanged"
                })))
            })
        }),
        Arc::new(|_frontend_log, _include_crash_dumps, _context| {
            Box::pin(async {
                Err(HttpRouteError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"message": "Unavailable"}),
                ))
            })
        }),
        Arc::new(|_token, _path, _context| {
            Box::pin(async {
                Err(HttpRouteError::new(
                    StatusCode::NOT_FOUND,
                    json!({"message": "Not Found"}),
                ))
            })
        }),
        Arc::new(|_method, _body, _context| {
            Box::pin(async {
                Err(HttpRouteError::new(
                    StatusCode::UNAUTHORIZED,
                    json!({
                        "error": "invalid_mcp_credential",
                        "message": "A valid provider-scoped MCP bearer credential is required."
                    }),
                )
                .with_header("www-authenticate", "Bearer"))
            })
        }),
        Arc::new(|_token, _context| {
            Box::pin(async {
                Err(HttpRouteError::new(
                    StatusCode::NOT_FOUND,
                    json!({"message": "Not Found"}),
                ))
            })
        }),
        Arc::new(|_token, _name, _overwrite, _body, _context| {
            Box::pin(async {
                Err(HttpRouteError::new(
                    StatusCode::NOT_FOUND,
                    json!({"message": "Not Found"}),
                ))
            })
        }),
    )
}

#[tokio::test]
async fn transfer_routes_stream_downloads_and_accept_uploads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let access = coddy_studio_server::transfer::TransferAccess::new(b"secret".to_vec());
    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_download =
        coddy_studio_server::production::transfer_routes::download_handler(access.clone());
    state.transfer_upload = coddy_studio_server::production::transfer_routes::upload_handler(
        access.clone(),
        Arc::new(|_root| Box::pin(async {})),
    );
    let app = add_routes(Router::new()).with_state(TestState(state));
    std::fs::create_dir_all(root.join("dir")).unwrap();
    std::fs::write(root.join("dir/a.txt"), "hello").unwrap();

    let file_url = access
        .issue_download(&root, "dir/a.txt")
        .unwrap()
        .relative_url;
    let response = app
        .clone()
        .oneshot(Request::get(&file_url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"a.txt\""
    );
    // A file's length is known before the first byte, so the client can show real progress.
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "5");
    assert_eq!(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
        "hello"
    );

    let folder_url = access.issue_download(&root, "dir").unwrap().relative_url;
    let response = app
        .clone()
        .oneshot(Request::get(&folder_url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/zip");
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"dir.zip\""
    );
    // A zip is produced as it streams, so it carries no length and stays chunked.
    assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"PK"));

    // The token names the file: no `?name=` is sent, and the write lands on the token's name.
    let upload_url = access
        .issue_upload(&root, "dir", "b.txt")
        .unwrap()
        .relative_url;
    let response = app
        .clone()
        .oneshot(Request::post(&upload_url).body(Body::from("new")).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(body_json(response).await["relativePath"], "dir/b.txt");
    assert_eq!(std::fs::read(root.join("dir/b.txt")).unwrap(), b"new");

    let response = app
        .clone()
        .oneshot(
            Request::post(&upload_url)
                .body(Body::from("again"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(response).await["_tag"],
        "TransferEntryExistsError"
    );
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("{upload_url}?overwrite=1"))
                .body(Body::from("again"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(std::fs::read(root.join("dir/b.txt")).unwrap(), b"again");

    // A `?name=` that repeats the token's own name is harmless and accepted.
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("{upload_url}?name=b.txt&overwrite=1"))
                .body(Body::from("echoed"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(std::fs::read(root.join("dir/b.txt")).unwrap(), b"echoed");

    // A `?name=` that disagrees with the token cannot redirect it.
    for name in ["../x", "c.txt"] {
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("{upload_url}?name={name}&overwrite=1"))
                    .body(Body::from("x"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
    }
    assert!(!root.join("dir/c.txt").exists());
    assert_eq!(std::fs::read(root.join("dir/b.txt")).unwrap(), b"echoed");
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/transfers/not.a.token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .oneshot(
            Request::post(format!("{file_url}?name=z"))
                .body(Body::from("x"))
                .unwrap(),
        )
        .await
        .unwrap();
    // A download token cannot upload.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

#[tokio::test]
async fn transfer_routes_distinguish_archive_and_upload_size_failures() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let access = coddy_studio_server::transfer::TransferAccess::new(b"secret".to_vec());
    std::fs::create_dir_all(root.join("dir")).unwrap();
    std::fs::write(root.join("dir/a.txt"), "hello").unwrap();
    std::fs::write(root.join("dir/b.txt"), "hello").unwrap();
    let folder_url = access.issue_download(&root, "dir").unwrap().relative_url;

    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_download =
        coddy_studio_server::production::transfer_routes::download_handler_with_limits(
            access.clone(),
            1,
            u64::MAX,
        );
    let entries_app = add_routes(Router::new()).with_state(TestState(state));
    let response = entries_app
        .oneshot(Request::get(&folder_url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "TransferArchiveTooLargeError");
    assert_eq!(body["unit"], "entries");
    assert_eq!(body["limit"], 1);

    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_download =
        coddy_studio_server::production::transfer_routes::download_handler_with_limits(
            access.clone(),
            usize::MAX,
            4,
        );
    let bytes_app = add_routes(Router::new()).with_state(TestState(state));
    let response = bytes_app
        .oneshot(Request::get(&folder_url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "TransferArchiveTooLargeError");
    assert_eq!(body["unit"], "bytes");
    assert_eq!(body["limit"], 4);

    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_upload = coddy_studio_server::production::transfer_routes::upload_handler(
        access.clone(),
        Arc::new(|_root| Box::pin(async {})),
    );
    let upload_app = add_routes(Router::new()).with_state(TestState(state));
    let upload_url = access
        .issue_upload_with_limit(&root, "dir", "big.txt", 2)
        .unwrap()
        .relative_url;
    let response = upload_app
        .oneshot(
            Request::post(&upload_url)
                .body(Body::from("too long"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(response).await;
    assert_eq!(body["_tag"], "TransferTooLargeError");
    assert_eq!(body["limit"], 2);
    assert!(!root.join("dir/big.txt").exists());
}

#[tokio::test]
async fn a_minted_download_url_redeems_against_the_transfer_route() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/app.ts"), "export const answer = 42;").unwrap();

    let access = coddy_studio_server::transfer::TransferAccess::new(b"secret".to_vec());
    let rpc = coddy_studio_server::workspace::WorkspaceRpc::with_dependencies(
        coddy_studio_server::workspace::WorkspaceService::default(),
        coddy_studio_server::workspace::WorkspaceRpcDependencies {
            transfer_access: Some(access.clone()),
            ..coddy_studio_server::workspace::WorkspaceRpcDependencies::default()
        },
    );
    let minted = rpc
        .handle(
            "projects.createDownloadUrl",
            json!({ "cwd": root.to_string_lossy(), "relativePath": "src/app.ts" }),
        )
        .await
        .expect("minted download URL");
    assert_eq!(minted["kind"], "file");

    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_download =
        coddy_studio_server::production::transfer_routes::download_handler(access);
    let app = add_routes(Router::new()).with_state(TestState(state));

    let response = app
        .oneshot(
            Request::get(minted["relativeUrl"].as_str().expect("relativeUrl"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"app.ts\""
    );
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX).await.unwrap(),
        "export const answer = 42;"
    );
}

#[tokio::test]
async fn a_minted_upload_url_writes_only_the_name_the_mint_bound() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();

    let access = coddy_studio_server::transfer::TransferAccess::new(b"secret".to_vec());
    let rpc = coddy_studio_server::workspace::WorkspaceRpc::with_dependencies(
        coddy_studio_server::workspace::WorkspaceService::default(),
        coddy_studio_server::workspace::WorkspaceRpcDependencies {
            transfer_access: Some(access.clone()),
            ..coddy_studio_server::workspace::WorkspaceRpcDependencies::default()
        },
    );
    let minted = rpc
        .handle(
            "projects.createUploadUrl",
            json!({
                "cwd": root.to_string_lossy(),
                "relativeDirectory": "src",
                "fileName": "notes.md",
            }),
        )
        .await
        .expect("minted upload URL");
    let upload_url = minted["relativeUrl"].as_str().expect("relativeUrl");

    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_upload = coddy_studio_server::production::transfer_routes::upload_handler(
        access,
        Arc::new(|_root| Box::pin(async {})),
    );
    let app = add_routes(Router::new()).with_state(TestState(state));

    // No `?name=`: the route takes the name from the token the RPC minted.
    let response = app
        .clone()
        .oneshot(Request::post(upload_url).body(Body::from("hi")).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(body_json(response).await["relativePath"], "src/notes.md");
    assert_eq!(std::fs::read(root.join("src/notes.md")).unwrap(), b"hi");

    // The same token cannot be pointed at a second file in that folder.
    let response = app
        .oneshot(
            Request::post(format!("{upload_url}?name=other.md&overwrite=1"))
                .body(Body::from("nope"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!root.join("src/other.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_download_names_a_non_ascii_file_without_breaking_the_response() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let access = coddy_studio_server::transfer::TransferAccess::new(b"secret".to_vec());
    let mut state = state_with_json_recorder(Arc::new(Mutex::new(Vec::new())));
    state.transfer_download =
        coddy_studio_server::production::transfer_routes::download_handler(access.clone());
    let app = add_routes(Router::new()).with_state(TestState(state));

    // A raw non-ASCII or control byte in `filename` is not a legal header value, so the ASCII
    // slot is transliterated and the real name rides in `filename*`.
    for (name, expected) in [
        (
            "é.txt",
            "attachment; filename=\"_.txt\"; filename*=UTF-8''%C3%A9.txt",
        ),
        (
            "a\nb.txt",
            "attachment; filename=\"a_b.txt\"; filename*=UTF-8''a%0Ab.txt",
        ),
        (
            // Nothing survives transliteration, so the ASCII slot falls back to a plain word.
            "\u{4f60}\u{597d}",
            "attachment; filename=\"download\"; filename*=UTF-8''%E4%BD%A0%E5%A5%BD",
        ),
    ] {
        std::fs::write(root.join(name), "hello").unwrap();
        let url = access.issue_download(&root, name).unwrap().relative_url;
        let response = app
            .clone()
            .oneshot(Request::get(&url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{name:?}");
        assert_eq!(
            response.headers()["content-disposition"],
            expected,
            "{name:?}"
        );
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "hello",
            "{name:?}"
        );
    }
}
