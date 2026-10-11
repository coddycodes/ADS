use std::{
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use bibcode_server::{
    ConfigError, DESKTOP_SHUTDOWN_PATH, DESKTOP_SHUTDOWN_TOKEN_HEADER, ROUTE_INVENTORY,
    RpcRegistry, ServerConfig, ServerError, ServerMode, ServerRuntime, logging,
};
use futures_util::SinkExt;
use reqwest::{Client, StatusCode, redirect::Policy};
use serde_json::Value;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use url::Url;
use uuid::Uuid;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;
#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;
use hermetic_providers::BUILTIN_PROVIDER_DRIVERS;

const SAME_LOG_PATH_CHILD_ENV: &str = "BIBCODE_TEST_SAME_LOG_PATH_CHILD_ROOT";
const SAME_LOG_PATH_TEST: &str = "public_and_runtime_initializers_share_one_physical_log_writer";

fn test_config(temp: &TempDir) -> ServerConfig {
    let config = ServerConfig::new(temp.path()).with_bind("127.0.0.1", 0);
    // Preserve pre-written fixture overrides while pinning unspecified drivers
    // and disabling update checks before runtime startup.
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    config
}

async fn assert_log_contains(path: &Path, marker: &str) {
    let contents = tokio::fs::read_to_string(path)
        .await
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert!(
        contents.contains(marker),
        "{} does not contain {marker:?}: {contents}",
        path.display()
    );
}

fn endpoint(address: SocketAddr, path: &str) -> String {
    format!("http://{address}{path}")
}

fn proxy_free_client() -> Client {
    Client::builder()
        .no_proxy()
        .build()
        .expect("proxy-free HTTP client")
}

async fn assert_json_wire(response: reqwest::Response, status: StatusCode, body: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.text().await.expect("JSON response body"), body);
}

async fn assert_missing_credential_wire(response: reqwest::Response) {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["content-type"], "application/json");
    let mut body = response.json::<Value>().await.expect("authentication JSON");
    let trace_id = body
        .as_object_mut()
        .expect("authentication error object")
        .remove("traceId")
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("authentication trace id");
    Uuid::parse_str(&trace_id).expect("UUID authentication trace id");
    assert_eq!(
        body,
        serde_json::json!({
            "_tag": "EnvironmentAuthInvalidError",
            "code": "auth_invalid",
            "reason": "missing_credential"
        })
    );
}

async fn exchange_startup_credential(
    client: &Client,
    address: SocketAddr,
    credential: &str,
) -> String {
    let response = client
        .post(endpoint(address, "/oauth/token"))
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:token-exchange",
            ),
            ("subject_token", credential),
            (
                "subject_token_type",
                "urn:bibcode:params:oauth:token-type:environment-bootstrap",
            ),
            (
                "requested_token_type",
                "urn:ietf:params:oauth:token-type:access_token",
            ),
        ])
        .send()
        .await
        .expect("startup credential exchange");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("token exchange JSON")["access_token"]
        .as_str()
        .expect("bearer access token")
        .to_owned()
}

fn write_disabled_provider_settings(temp: &TempDir) -> PathBuf {
    let settings_path = temp.path().join("userdata").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().expect("settings parent"))
        .expect("create settings directory");
    std::fs::write(
        &settings_path,
        serde_json::to_vec(&serde_json::json!({
            "enableProviderUpdateChecks": false,
            "providers": {
                "codex": { "enabled": false },
                "claudeAgent": { "enabled": false },
                "cursor": { "enabled": false },
                "grok": { "enabled": false },
                "opencode": { "enabled": false }
            }
        }))
        .expect("encode provider settings"),
    )
    .expect("write provider settings");
    settings_path
}

async fn fetch_server_config(client: &Client, address: SocketAddr, access_token: &str) -> Value {
    let ticket_response = client
        .post(endpoint(address, "/api/auth/websocket-ticket"))
        .bearer_auth(access_token)
        .send()
        .await
        .expect("WebSocket ticket response");
    assert_eq!(ticket_response.status(), StatusCode::OK);
    let ticket_body = ticket_response
        .json::<Value>()
        .await
        .expect("WebSocket ticket JSON");
    let ticket = ticket_body["ticket"].as_str().expect("WebSocket ticket");
    let (mut socket, _) = connect_async(format!("ws://{address}/ws?wsTicket={ticket}"))
        .await
        .expect("configuration WebSocket");
    socket
        .send(Message::Text(
            serde_json::json!({
                "_tag": "Request",
                "id": "1",
                "tag": "server.getConfig",
                "payload": {},
                "headers": []
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send server configuration request");
    let frame = timeout(
        Duration::from_secs(2),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("server configuration timeout")
    .expect("configuration WebSocket remains open")
    .expect("server configuration frame");
    let wire: Value = serde_json::from_str(frame.to_text().expect("configuration text frame"))
        .expect("server configuration JSON");
    assert_eq!(wire["_tag"], "Exit");
    assert_eq!(wire["requestId"], "1");
    assert_eq!(wire["exit"]["_tag"], "Success");
    socket
        .close(None)
        .await
        .expect("close configuration WebSocket");
    wire["exit"]["value"].clone()
}

async fn send_rpc_request(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    request_id: &str,
    tag: &str,
) {
    socket
        .send(Message::Text(
            serde_json::json!({
                "_tag": "Request",
                "id": request_id,
                "tag": tag,
                "payload": {},
                "headers": []
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap_or_else(|error| panic!("send {tag} request: {error}"));
}

async fn next_rpc_wire(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    context: &str,
) -> Value {
    let frame = timeout(Duration::from_secs(2), next_frame_past_heartbeat(socket))
        .await
        .unwrap_or_else(|_| panic!("{context} timeout"))
        .unwrap_or_else(|| panic!("{context} WebSocket remains open"))
        .unwrap_or_else(|error| panic!("{context} frame: {error}"));
    serde_json::from_str(
        frame
            .to_text()
            .unwrap_or_else(|error| panic!("{context} text: {error}")),
    )
    .unwrap_or_else(|error| panic!("{context} JSON: {error}"))
}

async fn acknowledge_rpc_chunk(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    request_id: &str,
) {
    socket
        .send(Message::Text(
            serde_json::json!({ "_tag": "Ack", "requestId": request_id })
                .to_string()
                .into(),
        ))
        .await
        .expect("acknowledge RPC chunk");
}

async fn next_rpc_wire_for_request(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    request_id: &str,
    context: &str,
) -> Value {
    loop {
        let wire = next_rpc_wire(socket, context).await;
        if wire["requestId"] == request_id {
            return wire;
        }

        assert_eq!(
            wire["_tag"], "Chunk",
            "{context} received an unexpected frame for another request"
        );
        let other_request_id = wire["requestId"]
            .as_str()
            .unwrap_or_else(|| panic!("{context} unrelated chunk request ID"));
        acknowledge_rpc_chunk(socket, other_request_id).await;
    }
}

#[tokio::test]
async fn binds_an_ephemeral_port_and_serves_the_environment_descriptor() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start(test_config(&temp))
        .await
        .expect("server starts");

    assert_ne!(handle.local_addr().port(), 0);
    let response = reqwest::get(endpoint(
        handle.local_addr(),
        "/.well-known/bibcode/environment",
    ))
    .await
    .expect("environment response");
    assert_eq!(response.status(), StatusCode::OK);
    let descriptor: Value = response.json().await.expect("environment JSON");
    assert_eq!(descriptor["environmentId"], "local");
    assert_eq!(descriptor["capabilities"]["repositoryIdentity"], true);
    assert_eq!(descriptor["remoteProtocolVersion"], 1);
    assert_eq!(descriptor["minCompatibleRemoteProtocol"], 1);
    assert!(descriptor["capabilities"].get("worktreeCatalog").is_none());
    assert!(
        descriptor["capabilities"]
            .get("worktreeCatalogRefreshReason")
            .is_none()
    );

    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("shutdown timeout")
        .expect("server joins");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_runtimes_retain_owned_log_sinks_without_retargeting() {
    let left_root = TempDir::new().expect("left runtime root");
    let right_root = TempDir::new().expect("right runtime root");
    let left_log = left_root.path().join("userdata/logs/server.log");
    let right_log = right_root.path().join("userdata/logs/server.log");
    let (left, right) = tokio::join!(
        ServerRuntime::start_with_registry(test_config(&left_root), RpcRegistry::empty()),
        ServerRuntime::start_with_registry(test_config(&right_root), RpcRegistry::empty()),
    );
    let left = left.expect("left runtime");
    let right = right.expect("right runtime");

    let marker_a = format!("runtime-log-sink-a-{}", Uuid::new_v4());
    tracing::warn!(target: "runtime_log_sink_registry_test", marker = %marker_a);
    assert_log_contains(&left_log, &marker_a).await;
    assert_log_contains(&right_log, &marker_a).await;

    left.shutdown();
    left.join().await.expect("left runtime joins");
    let left_before = tokio::fs::read(&left_log)
        .await
        .expect("snapshot joined left runtime log");

    let marker_b = format!("runtime-log-sink-b-{}", Uuid::new_v4());
    tracing::warn!(target: "runtime_log_sink_registry_test", marker = %marker_b);
    assert_log_contains(&right_log, &marker_b).await;
    assert_eq!(
        tokio::fs::read(&left_log)
            .await
            .expect("read unchanged left runtime log"),
        left_before,
        "a joined runtime sink must not receive later process events"
    );
    left_root.close().expect("remove joined left runtime root");

    right.shutdown();
    right.join().await.expect("right runtime joins");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_and_runtime_initializers_share_one_physical_log_writer() {
    if let Some(root) = std::env::var_os(SAME_LOG_PATH_CHILD_ENV) {
        let root = PathBuf::from(root);
        let log_path = root.join("userdata/logs/server.log");
        assert_eq!(
            logging::initialize(&log_path).expect("public logging initialization"),
            logging::Init::Installed
        );
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(&root).with_bind("127.0.0.1", 0),
            RpcRegistry::empty(),
        )
        .await
        .expect("runtime with the public log path starts");

        let marker = format!("same-public-runtime-log-sink-{}", Uuid::new_v4());
        tracing::warn!(target: "runtime_log_sink_registry_test", marker = %marker);
        let contents = tokio::fs::read_to_string(&log_path)
            .await
            .expect("shared public and runtime log");
        assert_eq!(
            contents.matches(&marker).count(),
            1,
            "one process record must be written exactly once through one physical writer"
        );

        handle.shutdown();
        handle.join().await.expect("same-path runtime joins");
        let retained_marker = format!("retained-public-log-sink-{}", Uuid::new_v4());
        tracing::warn!(target: "runtime_log_sink_registry_test", marker = %retained_marker);
        let contents = tokio::fs::read_to_string(&log_path)
            .await
            .expect("process-owned log after runtime join");
        assert_eq!(
            contents.matches(&retained_marker).count(),
            1,
            "dropping the runtime lease must retain exactly one process-owned writer"
        );
        println!("BIBCODE_TEST_SAME_LOG_PATH_CHILD_DONE");
        return;
    }

    let root = TempDir::new().expect("same-path subprocess root");
    let output = timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                SAME_LOG_PATH_TEST,
                "--nocapture",
                "--test-threads=8",
            ])
            .env(SAME_LOG_PATH_CHILD_ENV, root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .expect("same-path child timeout")
    .expect("run same-path child");
    assert!(
        output.status.success(),
        "same-path child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("BIBCODE_TEST_SAME_LOG_PATH_CHILD_DONE"),
        "same-path child did not confirm completion"
    );
}

#[tokio::test]
async fn authenticated_rpc_and_lifecycle_descriptors_match_the_well_known_storage_identity() {
    let temp = TempDir::new().expect("temporary base directory");
    write_disabled_provider_settings(&temp);
    let handle = ServerRuntime::start(test_config(&temp))
        .await
        .expect("production server starts");
    let client = proxy_free_client();
    let well_known = client
        .get(endpoint(
            handle.local_addr(),
            "/.well-known/bibcode/environment",
        ))
        .send()
        .await
        .expect("well-known descriptor response")
        .json::<Value>()
        .await
        .expect("well-known descriptor JSON");
    let storage_instance_id = well_known["storageInstanceId"]
        .as_str()
        .expect("well-known storage instance ID")
        .to_owned();
    Uuid::parse_str(&storage_instance_id).expect("well-known storage instance UUID");

    let credential = handle
        .startup_access()
        .expect("authenticated web startup access")
        .credential
        .clone();
    let access_token = exchange_startup_credential(&client, handle.local_addr(), &credential).await;
    let ticket_response = client
        .post(endpoint(handle.local_addr(), "/api/auth/websocket-ticket"))
        .bearer_auth(&access_token)
        .send()
        .await
        .expect("WebSocket ticket response");
    assert_eq!(ticket_response.status(), StatusCode::OK);
    let ticket_body = ticket_response
        .json::<Value>()
        .await
        .expect("WebSocket ticket JSON");
    let ticket = ticket_body["ticket"].as_str().expect("WebSocket ticket");
    let (mut socket, _) =
        connect_async(format!("ws://{}/ws?wsTicket={ticket}", handle.local_addr()))
            .await
            .expect("authenticated RPC WebSocket");

    send_rpc_request(&mut socket, "10", "server.getConfig").await;
    let get_config = next_rpc_wire(&mut socket, "server.getConfig").await;
    assert_eq!(get_config["_tag"], "Exit");
    assert_eq!(get_config["requestId"], "10");
    assert_eq!(get_config["exit"]["_tag"], "Success");
    assert_eq!(
        get_config["exit"]["value"]["environment"]["storageInstanceId"],
        storage_instance_id
    );

    send_rpc_request(&mut socket, "11", "subscribeServerConfig").await;
    let config_snapshot = next_rpc_wire(&mut socket, "server config snapshot").await;
    assert_eq!(config_snapshot["_tag"], "Chunk");
    assert_eq!(config_snapshot["requestId"], "11");
    assert_eq!(config_snapshot["values"][0]["type"], "snapshot");
    assert_eq!(
        config_snapshot["values"][0]["config"]["environment"]["storageInstanceId"],
        storage_instance_id
    );
    acknowledge_rpc_chunk(&mut socket, "11").await;

    send_rpc_request(&mut socket, "12", "subscribeServerLifecycle").await;
    for expected_type in ["welcome", "ready"] {
        let lifecycle = next_rpc_wire_for_request(&mut socket, "12", expected_type).await;
        assert_eq!(lifecycle["_tag"], "Chunk");
        assert_eq!(lifecycle["requestId"], "12");
        assert_eq!(lifecycle["values"][0]["type"], expected_type);
        assert_eq!(
            lifecycle["values"][0]["payload"]["environment"]["storageInstanceId"],
            storage_instance_id
        );
        acknowledge_rpc_chunk(&mut socket, "12").await;
    }

    socket.close(None).await.expect("close RPC WebSocket");
    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("shutdown timeout")
        .expect("server joins");
}

#[tokio::test]
async fn activity_access_tokens_are_rejected_by_a_different_environment() {
    let first_temp = TempDir::new().expect("first environment directory");
    let second_temp = TempDir::new().expect("second environment directory");
    let first = ServerRuntime::start(test_config(&first_temp))
        .await
        .expect("first server starts");
    let second = ServerRuntime::start(test_config(&second_temp))
        .await
        .expect("second server starts");
    let client = proxy_free_client();
    let first_credential = first
        .startup_access()
        .expect("first startup access")
        .credential
        .clone();
    let first_token =
        exchange_startup_credential(&client, first.local_addr(), &first_credential).await;

    let response = client
        .post(endpoint(second.local_addr(), "/api/auth/websocket-ticket"))
        .bearer_auth(first_token)
        .send()
        .await
        .expect("cross-environment ticket request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    first.shutdown();
    second.shutdown();
    timeout(Duration::from_secs(2), first.join())
        .await
        .expect("first shutdown timeout")
        .expect("first server joins");
    timeout(Duration::from_secs(2), second.join())
        .await
        .expect("second shutdown timeout")
        .expect("second server joins");
}

#[tokio::test]
async fn validates_the_desktop_shutdown_token_and_joins_gracefully() {
    let temp = TempDir::new().expect("temporary base directory");
    let config = test_config(&temp)
        .with_desktop("desktop-secret")
        .expect("valid desktop token");
    let handle = ServerRuntime::start(config).await.expect("server starts");
    let url = endpoint(handle.local_addr(), DESKTOP_SHUTDOWN_PATH);
    let client = Client::new();

    let forbidden = client
        .post(&url)
        .header(DESKTOP_SHUTDOWN_TOKEN_HEADER, "wrong")
        .send()
        .await
        .expect("forbidden response");
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let accepted = client
        .post(&url)
        .header(DESKTOP_SHUTDOWN_TOKEN_HEADER, "desktop-secret")
        .send()
        .await
        .expect("accepted response");
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(accepted.headers()["cache-control"], "no-store");

    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("shutdown timeout")
        .expect("server joins");
}

#[test]
fn rejects_empty_programmatic_desktop_tokens() {
    let temp = TempDir::new().expect("temporary base directory");
    let error = test_config(&temp)
        .with_desktop("  ")
        .expect_err("empty desktop token must fail");
    assert!(matches!(error, ConfigError::EmptyDesktopBootstrapToken));
}

#[tokio::test]
async fn hides_the_desktop_shutdown_route_outside_desktop_mode() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start(test_config(&temp))
        .await
        .expect("server starts");
    let response = Client::new()
        .post(endpoint(handle.local_addr(), DESKTOP_SHUTDOWN_PATH))
        .send()
        .await
        .expect("shutdown response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn native_mcp_routes_are_live_and_enforce_authentication() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start(test_config(&temp))
        .await
        .expect("server starts");
    let client = Client::new();

    for request in [
        client.post(endpoint(handle.local_addr(), "/mcp")),
        client.delete(endpoint(handle.local_addr(), "/mcp")),
    ] {
        let response = request.send().await.expect("MCP response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }

    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn custom_registry_uses_exact_fallback_http_responses() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(test_config(&temp), RpcRegistry::empty())
        .await
        .expect("custom-registry server starts");
    let client = proxy_free_client();
    let startup = handle
        .startup_access()
        .expect("authenticated web startup access");
    assert!(handle.local_addr().ip().is_loopback());
    assert_eq!(
        startup.connection_string,
        format!("http://{}", handle.local_addr())
    );
    assert!(!startup.credential.is_empty());
    let pairing_url = Url::parse(&startup.pairing_url).expect("valid pairing URL");
    assert_eq!(pairing_url.scheme(), "http");
    assert_eq!(pairing_url.host_str(), Some("127.0.0.1"));
    assert_eq!(pairing_url.port(), Some(handle.local_addr().port()));
    assert_eq!(pairing_url.path(), "/pair");
    assert_eq!(pairing_url.query(), None);
    let expected_fragment = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("token", &startup.credential)
        .finish();
    assert_eq!(pairing_url.fragment(), Some(expected_fragment.as_str()));
    let decoded_fragment = url::form_urlencoded::parse(expected_fragment.as_bytes())
        .into_owned()
        .collect::<Vec<_>>();
    assert_eq!(
        decoded_fragment,
        vec![("token".to_owned(), startup.credential.clone())]
    );
    assert_missing_credential_wire(
        client
            .get(endpoint(handle.local_addr(), "/api/orchestration/snapshot"))
            .send()
            .await
            .expect("unauthenticated fallback JSON response"),
    )
    .await;
    let access_token =
        exchange_startup_credential(&client, handle.local_addr(), startup.credential.as_str())
            .await;

    assert_json_wire(
        client
            .get(endpoint(
                handle.local_addr(),
                "/api/orchestration/snapshot",
            ))
            .bearer_auth(&access_token)
            .send()
            .await
            .expect("fallback JSON response"),
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"_tag":"NativeRuntimeUnavailableError","message":"The native production runtime is unavailable."}"#,
    )
    .await;

    assert_json_wire(
        client
            .get(endpoint(
                handle.local_addr(),
                "/api/assets/missing-token/missing.txt",
            ))
            .send()
            .await
            .expect("fallback asset response"),
        StatusCode::NOT_FOUND,
        r#"{"_tag":"AssetNotFoundError"}"#,
    )
    .await;

    for request in [
        client.post(endpoint(handle.local_addr(), "/mcp")),
        client.delete(endpoint(handle.local_addr(), "/mcp")),
    ] {
        assert_json_wire(
            request.send().await.expect("fallback MCP response"),
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"_tag":"McpUnavailableError"}"#,
        )
        .await;
    }

    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("shutdown timeout")
        .expect("custom-registry server joins");
}

#[tokio::test]
async fn unspecified_bind_uses_localhost_startup_access_and_waits_for_shutdown() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(
        ServerConfig::new(temp.path()).with_bind("0.0.0.0", 0),
        RpcRegistry::empty(),
    )
    .await
    .expect("unspecified-address server starts");
    assert!(handle.local_addr().ip().is_unspecified());
    let startup = handle
        .startup_access()
        .expect("authenticated web startup access");
    assert!(!startup.credential.is_empty());
    assert_eq!(
        startup.connection_string,
        format!("http://localhost:{}", handle.local_addr().port())
    );
    let pairing_url = Url::parse(&startup.pairing_url).expect("valid pairing URL");
    assert_eq!(pairing_url.host_str(), Some("localhost"));
    assert_eq!(pairing_url.port(), Some(handle.local_addr().port()));
    assert_eq!(pairing_url.path(), "/pair");
    assert_eq!(pairing_url.query(), None);
    let fragment = pairing_url.fragment().expect("pairing URL fragment");
    assert_eq!(
        url::form_urlencoded::parse(fragment.as_bytes())
            .into_owned()
            .collect::<Vec<_>>(),
        vec![("token".to_owned(), startup.credential.clone())]
    );

    handle.shutdown();
    timeout(Duration::from_secs(2), handle.wait_for_shutdown())
        .await
        .expect("shutdown notification timeout");
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("shutdown join timeout")
        .expect("unspecified-address server joins");
}

#[tokio::test]
async fn existing_file_as_base_directory_returns_typed_creation_error() {
    let temp = TempDir::new().expect("temporary parent directory");
    let base_file = temp.path().join("base-file");
    std::fs::write(&base_file, "not a directory").expect("base path fixture");

    let error = match ServerRuntime::start_with_registry(
        ServerConfig::new(&base_file).with_bind("127.0.0.1", 0),
        RpcRegistry::empty(),
    )
    .await
    {
        Ok(handle) => {
            drop(handle);
            panic!("file base path must fail startup");
        }
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "failed to create the server base directory"
    );
    match error {
        ServerError::CreateBaseDirectory(source) => {
            assert_eq!(source.kind(), ErrorKind::AlreadyExists);
            assert!(!source.to_string().trim().is_empty());
        }
        other => panic!("expected CreateBaseDirectory, got {other:?}"),
    }
}

#[tokio::test]
async fn occupied_listener_address_returns_typed_bind_error() {
    let occupied = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("occupy ephemeral listener");
    let occupied_addr = occupied.local_addr().expect("occupied listener address");
    let temp = TempDir::new().expect("temporary base directory");

    let error = match ServerRuntime::start_with_registry(
        ServerConfig::new(temp.path()).with_bind("127.0.0.1", occupied_addr.port()),
        RpcRegistry::empty(),
    )
    .await
    {
        Ok(handle) => {
            drop(handle);
            panic!("occupied listener must fail startup");
        }
        Err(error) => error,
    };
    let message = error.to_string();
    match error {
        ServerError::Bind { address, source } => {
            assert_eq!(address, occupied_addr.to_string());
            assert_eq!(source.kind(), ErrorKind::AddrInUse);
            assert!(!source.to_string().trim().is_empty());
            assert!(message.contains(&source.to_string()), "{message}");
        }
        other => panic!("expected Bind, got {other:?}"),
    }
}

#[tokio::test]
async fn file_at_state_directory_returns_typed_state_files_error() {
    let temp = TempDir::new().expect("temporary base directory");
    let state_directory = temp.path().join("userdata");
    std::fs::write(&state_directory, "not a directory").expect("state path fixture");

    // This custom-registry test must reach startup with an invalid state path;
    // the normal fixture cannot write settings into that deliberate file.
    let error = match ServerRuntime::start_with_registry(
        ServerConfig::new(temp.path()).with_bind("127.0.0.1", 0),
        RpcRegistry::empty(),
    )
    .await
    {
        Ok(handle) => {
            drop(handle);
            panic!("file state path must fail startup");
        }
        Err(error) => error,
    };
    match error {
        ServerError::StateFiles(message) => {
            assert!(message.contains("failed to create state directory"));
            assert!(message.contains("userdata"));
        }
        other => panic!("expected StateFiles, got {other:?}"),
    }
}

#[tokio::test]
async fn directory_at_database_path_returns_typed_persistence_error() {
    let temp = TempDir::new().expect("temporary base directory");
    let database_path = temp.path().join("userdata").join("state.sqlite");
    std::fs::create_dir_all(&database_path).expect("database path fixture");

    let error =
        match ServerRuntime::start_with_registry(test_config(&temp), RpcRegistry::empty()).await {
            Ok(handle) => {
                drop(handle);
                panic!("directory database path must fail startup");
            }
            Err(error) => error,
        };
    match error {
        ServerError::PersistenceInitialize(message) => {
            assert!(database_path.is_dir(), "deliberate database fixture");
            let canonical_database_path = database_path
                .canonicalize()
                .expect("canonical database fixture");
            assert!(
                message.contains(&canonical_database_path.display().to_string()),
                "typed error identifies the classified database path: {message}"
            );
            assert!(
                message.contains("cannot be inspected without side effects"),
                "directory fixture is rejected before private snapshot validation: {message}"
            );
        }
        other => panic!("expected PersistenceInitialize, got {other:?}"),
    }
}

#[tokio::test]
async fn production_runtime_adapters_serve_snapshot_and_asset_errors() {
    let temp = TempDir::new().expect("temporary base directory");
    let settings_path = write_disabled_provider_settings(&temp);
    assert_eq!(
        settings_path,
        temp.path().join("userdata").join("settings.json")
    );
    let handle = ServerRuntime::start(test_config(&temp))
        .await
        .expect("production server starts");
    let client = proxy_free_client();
    let credential = handle
        .startup_access()
        .expect("authenticated web startup access")
        .credential
        .clone();
    let access_token = exchange_startup_credential(&client, handle.local_addr(), &credential).await;
    let config = fetch_server_config(&client, handle.local_addr(), &access_token).await;
    let providers = config["providers"].as_array().expect("provider snapshots");
    assert_eq!(providers.len(), BUILTIN_PROVIDER_DRIVERS.len());
    for &driver in BUILTIN_PROVIDER_DRIVERS {
        assert_eq!(config["settings"]["providers"][driver]["enabled"], false);
        let provider = providers
            .iter()
            .find(|provider| provider["instanceId"] == driver)
            .unwrap_or_else(|| panic!("missing disabled provider snapshot for {driver}"));
        assert_eq!(provider["driver"], driver);
        assert_eq!(provider["enabled"], false);
        assert_eq!(provider["installed"], false);
        assert_eq!(provider["status"], "disabled");
    }

    let snapshot_response = client
        .get(endpoint(handle.local_addr(), "/api/orchestration/snapshot"))
        .bearer_auth(&access_token)
        .send()
        .await
        .expect("production snapshot response");
    assert_eq!(snapshot_response.status(), StatusCode::OK);
    assert_eq!(snapshot_response.headers()["cache-control"], "no-store");
    let snapshot: Value = snapshot_response.json().await.expect("snapshot JSON");
    assert_eq!(snapshot["snapshotSequence"], serde_json::json!(0));
    let updated_at = snapshot["updatedAt"]
        .as_str()
        .expect("public snapshot timestamp");
    time::OffsetDateTime::parse(updated_at, &time::format_description::well_known::Rfc3339)
        .expect("public snapshot timestamp is RFC 3339");
    for collection in ["projects", "threads"] {
        assert_eq!(snapshot[collection], serde_json::json!([]), "{collection}");
    }
    assert_eq!(
        snapshot,
        serde_json::json!({
            "snapshotSequence": 0,
            "projects": [],
            "threads": [],
            "updatedAt": updated_at,
        }),
        "empty snapshot follows the complete public read-model contract"
    );
    for internal_collection in [
        "messages",
        "activities",
        "sessions",
        "approvals",
        "proposed_plans",
        "turns",
        "checkpoints",
        "states",
        "receipts",
        "diffs",
    ] {
        assert!(
            snapshot.get(internal_collection).is_none(),
            "persistence collection must not escape the public read model: {internal_collection}"
        );
    }

    assert_json_wire(
        client
            .get(endpoint(
                handle.local_addr(),
                "/api/assets/missing-token/missing.txt",
            ))
            .send()
            .await
            .expect("production asset response"),
        StatusCode::NOT_FOUND,
        r#"{"_tag":"AssetNotFoundError","message":"Asset was not found or its access token expired."}"#,
    )
    .await;

    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("production shutdown timeout")
        .expect("production server joins");
}

#[tokio::test]
async fn dropping_server_handle_aborts_the_task_and_releases_the_listener() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = ServerRuntime::start_with_registry(
        test_config(&temp).with_unsafe_no_auth(),
        RpcRegistry::empty(),
    )
    .await
    .expect("custom-registry server starts");
    assert!(handle.startup_access().is_none());
    let address = handle.local_addr();

    drop(handle);

    let replacement = timeout(Duration::from_secs(2), async {
        loop {
            match TcpListener::bind(address).await {
                Ok(listener) => break listener,
                Err(error) if error.kind() == ErrorKind::AddrInUse => {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("unexpected listener rebind error: {error}"),
            }
        }
    })
    .await
    .expect("dropped server releases listener before timeout");
    assert_eq!(
        replacement.local_addr().expect("replacement address"),
        address
    );
}

#[tokio::test]
async fn streams_static_assets_with_security_and_cache_headers() {
    let temp = TempDir::new().expect("temporary base directory");
    let static_dir = temp.path().join("static");
    std::fs::create_dir_all(static_dir.join("docs")).expect("static directories");
    std::fs::create_dir_all(static_dir.join("assets")).expect("build asset directory");
    std::fs::write(static_dir.join("index.html"), "<main>spa</main>").expect("SPA index");
    std::fs::write(static_dir.join("app.js"), "console.log('ok')").expect("asset");
    std::fs::write(
        static_dir.join("assets/app-AbCd1234.js"),
        "console.log('hashed')",
    )
    .expect("hashed asset");
    std::fs::write(static_dir.join("docs/index.html"), "<main>docs</main>")
        .expect("extensionless index");

    let config = test_config(&temp).with_static_dir(&static_dir);
    let handle = ServerRuntime::start(config).await.expect("server starts");
    let client = Client::new();

    let asset = client
        .get(endpoint(handle.local_addr(), "/app.js"))
        .send()
        .await
        .expect("asset response");
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(asset.headers()["x-content-type-options"], "nosniff");
    assert_eq!(asset.headers()["cache-control"], "no-cache");
    let etag = asset.headers()["etag"].clone();
    assert!(etag.to_str().expect("ETag").starts_with("W/\""));
    assert!(asset.headers().contains_key("content-security-policy"));
    let csp = asset.headers()["content-security-policy"]
        .to_str()
        .expect("CSP header")
        .to_owned();
    for directive in [
        "object-src 'none'",
        "base-uri 'self'",
        "frame-ancestors 'none'",
        // Browser mode frames this host's preview gateways, each on its own port.
        "frame-src 'self' http://127.0.0.1:*",
    ] {
        assert!(csp.contains(directive), "missing {directive} in {csp}");
    }
    assert_eq!(asset.text().await.expect("asset body"), "console.log('ok')");

    let hashed = client
        .get(endpoint(handle.local_addr(), "/assets/app-AbCd1234.js"))
        .send()
        .await
        .expect("hashed asset response");
    assert_eq!(hashed.status(), StatusCode::OK);
    assert_eq!(
        hashed.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert!(!hashed.headers().contains_key("etag"));
    assert_eq!(
        hashed.text().await.expect("hashed body"),
        "console.log('hashed')"
    );

    let conditional = client
        .get(endpoint(handle.local_addr(), "/app.js"))
        .header("if-none-match", etag.clone())
        .send()
        .await
        .expect("conditional asset response");
    assert_eq!(conditional.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(conditional.headers()["etag"], etag);
    assert_eq!(conditional.headers()["cache-control"], "no-cache");
    assert_eq!(conditional.headers()["x-content-type-options"], "nosniff");
    assert_eq!(conditional.headers()["content-security-policy"], csp);
    assert!(
        conditional
            .bytes()
            .await
            .expect("conditional body")
            .is_empty()
    );

    let head = client
        .head(endpoint(handle.local_addr(), "/app.js"))
        .send()
        .await
        .expect("HEAD asset response");
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()["cache-control"], "no-cache");
    assert_eq!(head.headers()["etag"], etag);
    assert_eq!(head.headers()["content-length"], "17");
    assert_eq!(head.headers()["x-content-type-options"], "nosniff");
    assert_eq!(head.headers()["content-security-policy"], csp);
    assert!(head.bytes().await.expect("HEAD body").is_empty());

    std::fs::write(static_dir.join("app.js"), "console.log('updated')").expect("updated asset");
    let changed = client
        .get(endpoint(handle.local_addr(), "/app.js"))
        .header("if-none-match", etag.clone())
        .send()
        .await
        .expect("changed asset response");
    assert_eq!(changed.status(), StatusCode::OK);
    assert_eq!(changed.headers()["cache-control"], "no-cache");
    assert_ne!(changed.headers()["etag"], etag);
    assert_eq!(
        changed.text().await.expect("changed body"),
        "console.log('updated')"
    );

    let post = client
        .post(endpoint(handle.local_addr(), "/app.js"))
        .send()
        .await
        .expect("POST asset response");
    assert_eq!(post.status(), StatusCode::NOT_FOUND);

    let docs = client
        .get(endpoint(handle.local_addr(), "/docs"))
        .send()
        .await
        .expect("docs response");
    assert_eq!(docs.text().await.expect("docs body"), "<main>docs</main>");

    let index = client
        .get(endpoint(handle.local_addr(), "/"))
        .send()
        .await
        .expect("index response");
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(index.headers()["cache-control"], "no-cache");
    assert!(!index.headers().contains_key("etag"));
    assert_eq!(index.text().await.expect("index body"), "<main>spa</main>");

    let fallback = client
        .get(endpoint(handle.local_addr(), "/missing/route"))
        .send()
        .await
        .expect("fallback response");
    assert_eq!(fallback.headers()["cache-control"], "no-cache");
    assert!(!fallback.headers().contains_key("etag"));
    assert_eq!(
        fallback.text().await.expect("fallback body"),
        "<main>spa</main>"
    );

    let missing_asset = client
        .get(endpoint(handle.local_addr(), "/assets/missing-AbCd1234.js"))
        .header("if-none-match", "*")
        .send()
        .await
        .expect("missing asset fallback response");
    assert_eq!(missing_asset.status(), StatusCode::OK);
    assert_eq!(missing_asset.headers()["cache-control"], "no-cache");
    assert!(!missing_asset.headers().contains_key("etag"));
    assert_eq!(
        missing_asset.text().await.expect("missing asset body"),
        "<main>spa</main>"
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../app.js", static_dir.join("assets/alias-AbCd1234.js"))
            .expect("unhashed asset symlink");
        std::os::unix::fs::symlink("assets/app-AbCd1234.js", static_dir.join("alias.js"))
            .expect("hashed asset symlink");
        for (path, cache_control, has_etag, body) in [
            (
                "/assets/alias-AbCd1234.js",
                "no-cache",
                true,
                "console.log('updated')",
            ),
            (
                "/alias.js",
                "public, max-age=31536000, immutable",
                false,
                "console.log('hashed')",
            ),
        ] {
            let response = client
                .get(endpoint(handle.local_addr(), path))
                .send()
                .await
                .expect("symlink response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(response.headers()["cache-control"], cache_control, "{path}");
            assert_eq!(response.headers().contains_key("etag"), has_etag, "{path}");
            assert_eq!(response.text().await.expect("symlink body"), body, "{path}");
        }
    }

    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn rejects_percent_encoded_static_path_traversal() {
    let temp = TempDir::new().expect("temporary base directory");
    let static_dir = temp.path().join("static");
    std::fs::create_dir_all(&static_dir).expect("static directory");
    std::fs::write(static_dir.join("index.html"), "index").expect("index");
    let handle = ServerRuntime::start(test_config(&temp).with_static_dir(&static_dir))
        .await
        .expect("server starts");

    let response = raw_get(handle.local_addr(), "/%2e%2e/secret").await;
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");

    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn preserves_path_and_query_when_redirecting_loopback_dev_requests() {
    let temp = TempDir::new().expect("temporary base directory");
    let config = test_config(&temp)
        .with_dev_url(Url::parse("http://127.0.0.1:5173/base").expect("valid dev URL"));
    hermetic_providers::ensure_hermetic_settings(&config.state_dir());
    let handle = ServerRuntime::start(config).await.expect("server starts");
    let client = Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("HTTP client");

    let response = client
        .get(endpoint(handle.local_addr(), "/projects/one?tab=files"))
        .send()
        .await
        .expect("redirect response");
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers()["location"],
        "http://127.0.0.1:5173/projects/one?tab=files"
    );

    let head = client
        .head(endpoint(handle.local_addr(), "/projects/one?tab=files"))
        .send()
        .await
        .expect("HEAD redirect response");
    assert_eq!(head.status(), StatusCode::FOUND);
    assert_eq!(
        head.headers()["location"],
        "http://127.0.0.1:5173/projects/one?tab=files"
    );
    assert!(head.bytes().await.expect("HEAD redirect body").is_empty());

    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[test]
fn route_inventory_covers_every_current_http_method_and_path() {
    let actual = ROUTE_INVENTORY
        .iter()
        .map(|route| (route.method, route.path))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected_routes());
}

fn expected_routes() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GET", "/.well-known/bibcode/environment"),
        ("GET", "/api/auth/session"),
        ("POST", "/api/auth/browser-session"),
        ("POST", "/oauth/token"),
        ("POST", "/api/auth/websocket-ticket"),
        ("POST", "/api/auth/pairing-token"),
        ("POST", "/api/auth/pairing-offer"),
        ("POST", "/api/auth/pairing-offer/cancel"),
        ("GET", "/api/auth/share-state"),
        ("GET", "/api/auth/pairing-links"),
        ("POST", "/api/auth/pairing-links/revoke"),
        ("GET", "/api/auth/clients"),
        ("POST", "/api/auth/clients/revoke"),
        ("POST", "/api/auth/clients/revoke-others"),
        ("GET", "/api/orchestration/snapshot"),
        ("POST", "/api/orchestration/dispatch"),
        ("POST", "/api/connect/link-proof"),
        ("POST", "/api/connect/relay-config"),
        ("GET", "/api/connect/link-state"),
        ("POST", "/api/connect/unlink"),
        ("POST", "/api/bibcode-connect/health"),
        ("POST", "/api/connect/mint-credential"),
        ("POST", "/api/bibcode-connect/mint-credential"),
        ("GET", "/ws"),
        ("GET", "/ws-e2ee"),
        ("POST", "/api/diagnostics/logs.zip"),
        ("GET", "/api/assets/*"),
        ("POST", "/api/preview/open-url"),
        ("GET", "/api/transfers/*"),
        ("POST", "/api/transfers/*"),
        ("POST", "/.well-known/bibcode/desktop/shutdown"),
        ("POST", "/api/maintenance/update/prepare"),
        ("POST", "/api/maintenance/update/commit"),
        ("POST", "/api/maintenance/update/cancel"),
        ("GET", "/api/maintenance/update/status"),
        ("POST", "/mcp"),
        ("DELETE", "/mcp"),
        ("GET", "*"),
    ]
}

async fn raw_get(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("raw TCP connection");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read response");
    String::from_utf8(response).expect("UTF-8 HTTP response")
}

#[test]
fn server_mode_remains_a_typed_configuration_value() {
    assert_eq!(ServerMode::Web.to_string(), "web");
}
