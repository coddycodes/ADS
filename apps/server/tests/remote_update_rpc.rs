use std::{sync::Arc, time::Duration};

use coddy_studio_server::remote_update::{
    HostUpdaterFuture, HostUpdaterStatus, RemoteUpdateDelegate, RemoteUpdateInstallMode,
    RemoteUpdateRequester, RemoteUpdateState, RemoteUpdateSupport, RemoteUpdateSupportReason,
};
use coddy_studio_server::{
    RemoteUpdateInstallKind, RpcExit, ServerConfig, ServerHandle, ServerMessage, ServerRuntime,
};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Response, StatusCode, header};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const TOKEN_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:coddy-studio:params:oauth:token-type:environment-bootstrap";

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

fn disable_provider_processes(root: &std::path::Path) {
    hermetic_providers::write_hermetic_settings(
        &root.join("userdata"),
        json!({
            "providers": {
                "codex": {"enabled": false},
                "claudeAgent": {"enabled": false},
                "cursor": {"enabled": false},
                "grok": {"enabled": false},
                "opencode": {"enabled": false}
            }
        }),
    );
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[test]
fn the_status_contract_shape_round_trips_through_the_rust_snapshot() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../packages/contracts/fixtures/rpc-wire/contract-shapes/updater__status-success.json",
    );
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("contract-shape fixture"))
            .expect("fixture JSON");
    let wire = fixture["exit"]["value"].clone();
    let snapshot: coddy_studio_server::RemoteUpdateSnapshot =
        serde_json::from_value(wire.clone()).expect("the Rust mirror decodes the TS shape");
    assert_eq!(snapshot.download_percent, Some(42));
    assert_eq!(
        serde_json::to_value(&snapshot).expect("snapshot re-encodes"),
        wire,
        "the Rust mirror must stay byte-identical to the TypeScript contract"
    );
}

async fn call_unary(socket: &mut WsStream, id: &str, method: &str) -> ServerMessage {
    call_unary_with(socket, id, method, json!({})).await
}

async fn call_unary_with(
    socket: &mut WsStream,
    id: &str,
    method: &str,
    payload: Value,
) -> ServerMessage {
    let request = json!({
        "_tag": "Request",
        "id": id,
        "tag": method,
        "payload": payload,
        "headers": []
    });
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .expect("request sends");
    loop {
        let message = socket
            .next()
            .await
            .expect("socket yields")
            .expect("frame decodes");
        if let Message::Text(text) = message {
            let decoded: ServerMessage =
                serde_json::from_str(&text).expect("server message decodes");
            if matches!(decoded, ServerMessage::Exit { .. }) {
                return decoded;
            }
        }
    }
}

async fn wait_for_active_work(socket: &mut WsStream, next_id: &mut u64, expected: &Value) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let id = next_id.to_string();
            *next_id += 1;
            let ServerMessage::Exit {
                exit: RpcExit::Success { value: Some(value) },
                ..
            } = call_unary(socket, &id, "updater.activeWork").await
            else {
                panic!("updater.activeWork must succeed for every client");
            };
            if value == *expected {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("active work reaches the expected counts within five seconds")
}

#[tokio::test]
async fn headless_server_answers_manual_update_surface() {
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let handle = ServerRuntime::start(config).await.expect("server starts");

    // Descriptor advertises the surface before any RPC (covers apps/server/src/http.rs).
    let descriptor: Value = reqwest::get(format!(
        "http://{}/.well-known/coddy-studio/environment",
        handle.local_addr()
    ))
    .await
    .expect("descriptor fetch")
    .json()
    .await
    .expect("descriptor JSON");
    assert_eq!(descriptor["capabilities"]["remoteUpdateControl"], true);
    assert_eq!(descriptor["capabilities"]["terminalOrderedInput"], true);
    assert_eq!(
        descriptor["remoteUpdateSupport"],
        json!({
            "installMode": "manual",
            "reason": "manual-update-required",
            "installKind": "unknown"
        })
    );

    assert_eq!(descriptor["capabilities"]["remoteUpdateProgress"], true);
    let boot_id = descriptor["bootId"]
        .as_str()
        .expect("descriptor carries a bootId");
    assert!(uuid::Uuid::parse_str(boot_id).is_ok());

    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    let ServerMessage::Exit {
        exit: RpcExit::Success {
            value: Some(status),
        },
        ..
    } = call_unary(&mut socket, "1", "updater.status").await
    else {
        panic!("updater.status must succeed");
    };
    assert_eq!(status["state"], "idle");
    assert_eq!(status["latestVersion"], Value::Null);
    assert_eq!(status["support"]["installMode"], "manual");
    assert_eq!(status["serverVersion"], env!("CARGO_PKG_VERSION"));

    let checked = call_unary(&mut socket, "2", "updater.check").await;
    assert!(matches!(
        checked,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(ref value) },
            ..
        } if value["state"] == "idle" && value["latestVersion"] == Value::Null
    ));

    let install = call_unary(&mut socket, "3", "updater.install").await;
    let ServerMessage::Exit { exit, .. } = install else {
        panic!("expected exit");
    };
    let failure = serde_json::to_value(&exit).expect("exit serializes");
    let failure_text = failure.to_string();
    assert!(
        failure_text.contains("RemoteUpdateInstallError")
            && failure_text.contains("remote_update_manual_required"),
        "manual install must fail with the typed error, got {failure_text}"
    );

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn boot_id_changes_on_every_start_while_storage_identity_stays() {
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let config = || {
        let mut config = ServerConfig::new(temp.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth();
        assert!(config.boot_id.is_none());
        config.boot_id = Some(uuid::Uuid::nil());
        config
    };
    let descriptor = |handle: &ServerHandle| {
        let url = format!(
            "http://{}/.well-known/coddy-studio/environment",
            handle.local_addr()
        );
        async move {
            reqwest::get(url)
                .await
                .expect("descriptor fetch")
                .json::<Value>()
                .await
                .expect("descriptor JSON")
        }
    };

    let first = ServerRuntime::start(config()).await.expect("first start");
    let first_descriptor = descriptor(&first).await;
    first.shutdown();
    first.join().await.expect("first joins");
    let second = ServerRuntime::start(config()).await.expect("second start");
    let second_descriptor = descriptor(&second).await;
    second.shutdown();
    second.join().await.expect("second joins");

    for descriptor in [&first_descriptor, &second_descriptor] {
        let boot_id = uuid::Uuid::parse_str(
            descriptor["bootId"]
                .as_str()
                .expect("descriptor carries a bootId"),
        )
        .expect("bootId is a UUID");
        assert_eq!(boot_id.get_version_num(), 4);
    }
    assert_ne!(first_descriptor["bootId"], second_descriptor["bootId"]);
    assert!(first_descriptor["storageInstanceId"].as_str().is_some());
    assert_eq!(
        first_descriptor["storageInstanceId"],
        second_descriptor["storageInstanceId"]
    );
}

#[derive(Default)]
struct FixtureHostUpdater {
    requests: std::sync::Mutex<Vec<RemoteUpdateRequester>>,
}

impl RemoteUpdateDelegate for FixtureHostUpdater {
    fn status(&self) -> HostUpdaterFuture {
        Box::pin(async {
            HostUpdaterStatus {
                latest_version: Some("9.9.9".to_owned()),
                state: RemoteUpdateState::UpdateAvailable,
                error: None,
                ..HostUpdaterStatus::default()
            }
        })
    }

    fn check(&self) -> HostUpdaterFuture {
        self.status()
    }

    fn request_install(&self, requester: RemoteUpdateRequester) -> HostUpdaterFuture {
        self.requests.lock().expect("requests").push(requester);
        Box::pin(async {
            HostUpdaterStatus {
                latest_version: Some("9.9.9".to_owned()),
                state: RemoteUpdateState::Installing,
                error: None,
                ..HostUpdaterStatus::default()
            }
        })
    }
}

#[tokio::test]
async fn desktop_integrated_server_routes_install_through_the_delegate() {
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
        .with_remote_update_support(RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
            install_kind: RemoteUpdateInstallKind::Unknown,
        });
    let handle = ServerRuntime::start_with_desktop_integration(
        config,
        std::sync::Arc::new(coddy_studio_server::diagnostics::UnavailableDesktopUiProcessObserver),
        Arc::new(FixtureHostUpdater::default()),
    )
    .await
    .expect("server starts");

    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    let checked = call_unary(&mut socket, "1", "updater.check").await;
    assert!(matches!(
        checked,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(ref value) },
            ..
        } if value["latestVersion"] == "9.9.9" && value["state"] == "update-available"
    ));

    let install = call_unary(&mut socket, "2", "updater.install").await;
    assert!(matches!(
        install,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(ref value) },
            ..
        } if value["state"] == "installing"
    ));

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

#[tokio::test]
async fn install_passes_the_callers_client_metadata_and_logs_the_request_once() {
    const BOOTSTRAP: &str = "remote-update-bootstrap";
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_desktop(BOOTSTRAP)
        .expect("desktop config")
        .with_remote_update_support(RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
            install_kind: RemoteUpdateInstallKind::Unknown,
        });
    let log_path = coddy_studio_server::persistence::StatePaths::from_config(&config).server_log;
    let updater = Arc::new(FixtureHostUpdater::default());
    let handle = ServerRuntime::start_with_desktop_integration(
        config,
        Arc::new(coddy_studio_server::diagnostics::UnavailableDesktopUiProcessObserver),
        updater.clone(),
    )
    .await
    .expect("server starts");
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, BOOTSTRAP, None).await;
    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(access_token(&administrator))
            .json(&json!({ "label": "Tablet" }))
            .send()
            .await
            .expect("pairing"),
        StatusCode::OK,
    )
    .await;
    let paired = exchange_token(
        &client,
        &handle,
        pairing["credential"].as_str().expect("credential"),
        None,
    )
    .await;
    let ticket = websocket_ticket(&client, &handle, access_token(&paired)).await;
    let (mut socket, _) =
        connect_async(format!("ws://{}/ws?wsTicket={ticket}", handle.local_addr()))
            .await
            .expect("paired WebSocket");

    let install = call_unary(&mut socket, "1", "updater.install").await;
    assert!(matches!(
        install,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    ));
    let requests = updater.requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].label.as_deref(), Some("Tablet"));
    assert_eq!(
        requests[0].session_id_prefix.as_ref().map(String::len),
        Some(8)
    );

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
    let log = std::fs::read_to_string(log_path).expect("server log");
    // Tracing is one process-wide stream mirrored to every live runtime's server.log,
    // so count only lines that carry this test's requester, not the bare message.
    let prefix = requests[0]
        .session_id_prefix
        .clone()
        .expect("session prefix");
    let ours = log
        .lines()
        .filter(|line| line.contains("remote update install requested"))
        .filter(|line| line.contains("Tablet") && line.contains(&prefix))
        .count();
    assert_eq!(
        ours, 1,
        "exactly one requester line for this install: {log}"
    );
    assert!(
        !log.contains(access_token(&paired)),
        "no credentials in the log"
    );
}

#[tokio::test]
async fn active_work_counts_work_from_every_client() {
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let handle = ServerRuntime::start(
        ServerConfig::new(temp.path())
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
    )
    .await
    .expect("server starts");
    let (mut first, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("first client");
    let (mut second, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("second client");

    let ServerMessage::Exit {
        exit: RpcExit::Success { value: Some(idle) },
        ..
    } = call_unary(&mut first, "1", "updater.activeWork").await
    else {
        panic!("updater.activeWork must succeed");
    };
    assert_eq!(
        idle,
        json!({ "runningTurns": 0, "liveTerminals": 0, "queuedMessages": 0 })
    );

    let ServerMessage::Exit {
        exit: RpcExit::Success {
            value: Some(from_second),
        },
        ..
    } = call_unary(&mut second, "2", "updater.activeWork").await
    else {
        panic!("updater.activeWork must succeed for every client");
    };
    assert_eq!(from_second, idle);

    let command = if cfg!(windows) {
        json!({
            "executable": "powershell.exe",
            "args": ["-NoProfile", "-NonInteractive", "-Command", "Start-Sleep -Seconds 60"]
        })
    } else {
        json!({"executable": "/bin/sh", "args": ["-c", "sleep 60"]})
    };
    let opened = call_unary_with(
        &mut first,
        "2",
        "terminal.open",
        json!({
            "threadId": "active-work",
            "terminalId": "term-1",
            "cwd": temp.path(),
            "command": command
        }),
    )
    .await;
    assert!(matches!(
        opened,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    ));

    let active = json!({ "runningTurns": 0, "liveTerminals": 1, "queuedMessages": 0 });
    let mut second_request_id = 3;
    assert_eq!(
        wait_for_active_work(&mut second, &mut second_request_id, &active).await,
        active
    );

    let closed = call_unary_with(
        &mut first,
        "3",
        "terminal.close",
        json!({"threadId": "active-work", "terminalId": "term-1"}),
    )
    .await;
    assert!(matches!(
        closed,
        ServerMessage::Exit {
            exit: RpcExit::Success { .. },
            ..
        }
    ));
    assert_eq!(
        wait_for_active_work(&mut second, &mut second_request_id, &idle).await,
        idle
    );

    first.close(None).await.expect("close first");
    second.close(None).await.expect("close second");
    handle.shutdown();
    handle.join().await.expect("server joins");
}

fn http_url(handle: &ServerHandle, path: &str) -> String {
    format!("http://{}{}", handle.local_addr(), path)
}

fn token_form<'a>(credential: &'a str, scope: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut form = vec![
        ("grant_type", TOKEN_GRANT_TYPE),
        ("subject_token", credential),
        ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
        ("requested_token_type", ACCESS_TOKEN_TYPE),
    ];
    if let Some(scope) = scope {
        form.push(("scope", scope));
    }
    form
}

async fn exchange_token(
    client: &Client,
    handle: &ServerHandle,
    credential: &str,
    scope: Option<&str>,
) -> Value {
    let response = client
        .post(http_url(handle, "/oauth/token"))
        .form(&token_form(credential, scope))
        .send()
        .await
        .expect("token exchange request");
    assert_credential_headers(&response);
    get_json(response, StatusCode::OK).await
}

async fn websocket_ticket(client: &Client, handle: &ServerHandle, token: &str) -> String {
    let response = client
        .post(http_url(handle, "/api/auth/websocket-ticket"))
        .bearer_auth(token)
        .send()
        .await
        .expect("WebSocket ticket request");
    get_json(response, StatusCode::OK).await["ticket"]
        .as_str()
        .expect("WebSocket ticket")
        .to_owned()
}

fn access_token(response: &Value) -> &str {
    response["access_token"].as_str().expect("access token")
}

fn assert_credential_headers(response: &Response) {
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&header::HeaderValue::from_static("no-store"))
    );
    assert_eq!(
        response.headers().get(header::PRAGMA),
        Some(&header::HeaderValue::from_static("no-cache"))
    );
}

async fn get_json(response: Response, expected_status: StatusCode) -> Value {
    let actual_status = response.status();
    let body = response.text().await.expect("HTTP response body");
    assert_eq!(
        actual_status, expected_status,
        "unexpected HTTP response body: {body}"
    );
    serde_json::from_str(&body).expect("JSON response")
}
