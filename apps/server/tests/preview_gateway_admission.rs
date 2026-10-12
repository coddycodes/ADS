//! Preview gateway admission and target registry: which URLs `preview.gatewayOpen` admits,
//! how upstreams are reached, listener reuse, and when targets close.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;
#[path = "support/websocket_frames.rs"]
mod websocket_frames;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{Router, routing::get};
use bibcode_server::{
    ServerConfig, ServerHandle, ServerRuntime,
    preview::{
        PreviewManager,
        gateway::{
            proxy::PrincipalCheck,
            registry::{
                GatewayError, GatewayOpenResult, MAX_TARGETS, PreviewGateway,
                SHUTDOWN_DRAIN_TIMEOUT, gateway_context,
            },
            relay::RelayHosts,
        },
    },
    signed_token::now_millis,
};
use futures_util::{SinkExt, future::BoxFuture};
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use websocket_frames::next_frame_past_heartbeat;

const THREAD: &str = "thread-1";
const SESSION: &str = "session-1";
const DESKTOP_BOOTSTRAP: &str = "preview-gateway-admission-bootstrap";
const TOKEN_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:bibcode:params:oauth:token-type:environment-bootstrap";

/// Every principal is active for another hour.
struct LivePrincipal;

impl PrincipalCheck for LivePrincipal {
    fn session_expiry<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Option<i64>> {
        Box::pin(async { Some(far_expiry()) })
    }
}

fn far_expiry() -> i64 {
    i64::try_from(now_millis()).unwrap() + 3_600_000
}

fn gateway(preview: &PreviewManager) -> PreviewGateway {
    PreviewGateway::new(
        "127.0.0.1",
        "devbox",
        gateway_context(
            Arc::new(LivePrincipal),
            vec![9; 32],
            "bibcode_session".into(),
        ),
        preview.clone(),
        RelayHosts::default(),
    )
}

async fn upstream_on(addr: &str) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let app = Router::new().route("/", get(|| async { "hello from upstream" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Ok(local)
}

async fn open(gateway: &PreviewGateway, url: &str) -> Result<GatewayOpenResult, GatewayError> {
    gateway.open(THREAD, url, SESSION, far_expiry(), None).await
}

fn http() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Bootstraps `capability` on `gateway_port` and returns the gateway cookie.
async fn bootstrap(gateway_port: u16, capability: &str) -> Result<String, StatusCode> {
    let response = http()
        .get(format!(
            "http://127.0.0.1:{gateway_port}/__bibcode/bootstrap?cap={capability}&to=%2F"
        ))
        .send()
        .await
        .unwrap();
    if response.status() != StatusCode::OK {
        return Err(response.status());
    }
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    Ok(cookie.split(';').next().unwrap().to_owned())
}

async fn listening(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

async fn wait_until_closed(port: u16) {
    timeout(Duration::from_secs(5), async {
        while listening(port).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("gateway port {port} kept listening"));
}

#[tokio::test]
async fn rejects_non_loopback_and_non_http_targets() {
    let preview = PreviewManager::new();
    let gateway = gateway(&preview);
    for url in [
        "http://example.com:5173/",
        "http://192.168.1.5:5173/",
        "http://localhost.example.com:5173/",
        "http://0.0.0.0:5173/",
        "ftp://localhost:5173/",
        "ws://localhost:5173/",
        "file:///tmp/index.html",
        "not a url",
    ] {
        assert!(
            matches!(open(&gateway, url).await, Err(GatewayError::NotAdmitted)),
            "{url} must not be admitted"
        );
    }
}

#[tokio::test]
async fn rejects_https_targets() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let gateway = gateway(&PreviewManager::new());
    let url = format!("https://localhost:{}/", upstream.port());
    assert!(matches!(
        open(&gateway, &url).await,
        Err(GatewayError::HttpsUnsupported)
    ));
}

#[tokio::test]
async fn no_upstream_when_nothing_listens() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let gateway = gateway(&PreviewManager::new());
    assert!(matches!(
        open(&gateway, &format!("http://localhost:{port}/")).await,
        Err(GatewayError::NoUpstream)
    ));
}

#[tokio::test]
async fn reaches_an_ipv6_only_upstream() {
    let upstream = match upstream_on("[::1]:0").await {
        Ok(upstream) => upstream,
        Err(error) => {
            println!("skipping: IPv6 loopback is unavailable ({error})");
            return;
        }
    };
    let gateway = gateway(&PreviewManager::new());
    let opened = open(&gateway, &format!("http://localhost:{}/", upstream.port()))
        .await
        .expect("an IPv6-only upstream is admitted");
    let cookie = bootstrap(opened.gateway_port, &opened.capability)
        .await
        .expect("capability bootstraps");
    let body = http()
        .get(format!("http://127.0.0.1:{}/", opened.gateway_port))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "hello from upstream");
}

#[tokio::test]
async fn a_literal_loopback_upstream_is_probed_at_that_address() {
    // Only 127.0.0.1 listens on this port: 127.0.0.2 must not be served by it.
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let gateway = gateway(&PreviewManager::new());
    // Not even by reusing the running `localhost` target on that port.
    open(&gateway, &format!("http://localhost:{}/", upstream.port()))
        .await
        .unwrap();
    assert!(matches!(
        open(&gateway, &format!("http://127.0.0.2:{}/", upstream.port())).await,
        Err(GatewayError::NoUpstream)
    ));
    // Linux routes all of 127.0.0.0/8 to loopback; other platforms only 127.0.0.1.
    if cfg!(target_os = "linux") {
        let second = upstream_on("127.0.0.2:0").await.unwrap();
        let opened = open(&gateway, &format!("http://127.0.0.2:{}/", second.port()))
            .await
            .expect("an upstream on 127.0.0.2 is admitted");
        let cookie = bootstrap(opened.gateway_port, &opened.capability)
            .await
            .unwrap();
        let body = http()
            .get(format!("http://127.0.0.1:{}/", opened.gateway_port))
            .header(header::COOKIE, cookie)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "hello from upstream");
    }
}

#[tokio::test]
async fn opens_past_the_global_target_cap_are_unavailable() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let gateway = gateway(&PreviewManager::new());
    let url = format!("http://localhost:{}/", upstream.port());
    for thread in 0..MAX_TARGETS {
        gateway
            .open(&format!("t{thread}"), &url, SESSION, far_expiry(), None)
            .await
            .unwrap();
    }
    assert!(matches!(
        gateway
            .open("one-more", &url, SESSION, far_expiry(), None)
            .await,
        Err(GatewayError::Unavailable(_))
    ));
    // A running target still mints capabilities at the cap.
    gateway
        .open("t0", &url, SESSION, far_expiry(), None)
        .await
        .expect("reopen at the cap");
    gateway.shutdown().await;
}

#[tokio::test]
async fn shutdown_lets_an_in_flight_request_finish() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/slow",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(400)).await;
            "slow done"
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let gateway = gateway(&PreviewManager::new());
    let opened = open(&gateway, &format!("http://localhost:{}/", upstream.port()))
        .await
        .unwrap();
    let cookie = bootstrap(opened.gateway_port, &opened.capability)
        .await
        .unwrap();
    let request = tokio::spawn(
        http()
            .get(format!("http://127.0.0.1:{}/slow", opened.gateway_port))
            .header(header::COOKIE, cookie)
            .send(),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = std::time::Instant::now();
    gateway.shutdown().await;

    let response = request
        .await
        .unwrap()
        .expect("the in-flight request completes");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "slow done");
    assert!(
        started.elapsed() < SHUTDOWN_DRAIN_TIMEOUT,
        "shutdown ended with the request, not at the deadline"
    );
    assert!(!listening(opened.gateway_port).await);
}

#[tokio::test]
async fn navigating_away_keeps_the_old_target_until_the_tab_closes() {
    let first = upstream_on("127.0.0.1:0").await.unwrap();
    let second = upstream_on("127.0.0.1:0").await.unwrap();
    let preview = PreviewManager::new();
    let gateway = gateway(&preview);
    let first_url = format!("http://localhost:{}/", first.port());
    let tab = preview.open(THREAD, Some(&first_url)).await.unwrap();
    let target = open(&gateway, &first_url).await.unwrap();

    preview
        .navigate(
            THREAD,
            &tab.tab_id,
            &format!("http://localhost:{}/", second.port()),
            None,
        )
        .await
        .unwrap();
    // The desktop reports no native load failures, so Back to the old gateway origin must
    // still find its listener.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        listening(target.gateway_port).await,
        "navigating away keeps the old target"
    );

    // Closing another tab of the thread, or a lagged re-check, keeps it for that tab's Back.
    let other = preview.open(THREAD, None).await.unwrap();
    preview.close(THREAD, Some(&other.tab_id)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        listening(target.gateway_port).await,
        "a live tab that once showed the port keeps it"
    );

    preview.close(THREAD, Some(&tab.tab_id)).await.unwrap();
    wait_until_closed(target.gateway_port).await;
}

#[tokio::test]
async fn second_open_reuses_listener_with_fresh_capability() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let gateway = gateway(&PreviewManager::new());
    let url = format!("http://127.0.0.1:{}/app?x=1", upstream.port());
    let first = open(&gateway, &url).await.unwrap();
    let second = open(&gateway, &url).await.unwrap();
    assert_eq!(first.gateway_port, second.gateway_port);
    assert_ne!(first.capability, second.capability);
    let now = now_millis();
    assert!(second.expires_at_ms > now && second.expires_at_ms <= now + 60_000);
    // Each capability bootstraps its own gateway session.
    let a = bootstrap(first.gateway_port, &first.capability)
        .await
        .unwrap();
    let b = bootstrap(second.gateway_port, &second.capability)
        .await
        .unwrap();
    assert_ne!(a, b);

    // Another thread on the same port gets its own listener.
    let other = gateway
        .open("thread-2", &url, SESSION, far_expiry(), None)
        .await
        .unwrap();
    assert_ne!(other.gateway_port, first.gateway_port);
}

#[tokio::test]
async fn closing_last_tab_closes_target() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let unrelated = upstream_on("127.0.0.1:0").await.unwrap();
    let preview = PreviewManager::new();
    let gateway = gateway(&preview);
    let url = format!("http://localhost:{}/", upstream.port());
    let first_tab = preview.open(THREAD, Some(&url)).await.unwrap();
    let second_tab = preview.open(THREAD, Some(&url)).await.unwrap();
    let unrelated_tab = preview
        .open(
            THREAD,
            Some(&format!("http://localhost:{}/", unrelated.port())),
        )
        .await
        .unwrap();
    let target = open(&gateway, &url).await.unwrap();
    let unrelated_target = open(&gateway, &format!("http://localhost:{}/", unrelated.port()))
        .await
        .unwrap();

    preview
        .close(THREAD, Some(&first_tab.tab_id))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        listening(target.gateway_port).await,
        "a tab on the port is still open"
    );

    preview
        .close(THREAD, Some(&second_tab.tab_id))
        .await
        .unwrap();
    wait_until_closed(target.gateway_port).await;
    assert!(
        listening(unrelated_target.gateway_port).await,
        "closing a tab only re-checks that tab's port"
    );

    // Reopening after close starts a fresh target, now with no tab on its port.
    let reopened = open(&gateway, &url).await.unwrap();
    assert!(listening(reopened.gateway_port).await);

    preview
        .close(THREAD, Some(&unrelated_tab.tab_id))
        .await
        .unwrap();
    wait_until_closed(unrelated_target.gateway_port).await;
    // A target no tab points at (a browser-mode tab) is left to the idle sweep.
    assert!(listening(reopened.gateway_port).await);
}

#[tokio::test]
async fn a_lagged_follower_still_closes_targets_whose_last_tab_closed() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let tabless_upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let preview = PreviewManager::new();
    let gateway = gateway(&preview);
    let url = format!("http://localhost:{}/", upstream.port());
    let tab = preview.open(THREAD, Some(&url)).await.unwrap();
    let target = open(&gateway, &url).await.unwrap();
    let tabless = open(
        &gateway,
        &format!("http://localhost:{}/", tabless_upstream.port()),
    )
    .await
    .unwrap();
    let busy = preview.open("thread-2", None).await.unwrap();

    // Close the tab, then flood the event channel before the follower runs, so the
    // `closed` event is lost to lag.
    preview.close(THREAD, Some(&tab.tab_id)).await.unwrap();
    for _ in 0..200 {
        preview
            .resize(
                "thread-2",
                &busy.tab_id,
                bibcode_server::preview::PreviewViewportSetting::Fill,
            )
            .await
            .unwrap();
    }
    wait_until_closed(target.gateway_port).await;
    assert!(
        listening(tabless.gateway_port).await,
        "a target no tab ever pointed at survives the re-check"
    );
}

#[tokio::test]
async fn close_thread_and_shutdown_close_every_target() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let gateway = gateway(&PreviewManager::new());
    let url = format!("http://localhost:{}/", upstream.port());
    let first = open(&gateway, &url).await.unwrap();
    let other = gateway
        .open("thread-2", &url, SESSION, far_expiry(), None)
        .await
        .unwrap();
    gateway.close_thread(THREAD).await;
    wait_until_closed(first.gateway_port).await;
    assert!(listening(other.gateway_port).await);

    gateway.shutdown().await;
    wait_until_closed(other.gateway_port).await;
    assert!(matches!(
        open(&gateway, &url).await,
        Err(GatewayError::Unavailable(_))
    ));
}

// --- Full server: RPC scope, wiring, and thread deletion ---

struct Server {
    handle: ServerHandle,
    _root: tempfile::TempDir,
    client: Client,
    admin_token: String,
}

impl Server {
    async fn start() -> Self {
        Self::start_on("127.0.0.1").await
    }

    async fn start_on(host: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let providers = hermetic_providers::BUILTIN_PROVIDER_DRIVERS
            .iter()
            .map(|driver| ((*driver).to_owned(), json!({"enabled": false})))
            .collect::<serde_json::Map<String, Value>>();
        hermetic_providers::write_hermetic_settings(
            &ServerConfig::new(root.path()).state_dir(),
            json!({"providers": providers}),
        );
        let config = ServerConfig::new(root.path())
            .with_bind(host, 0)
            .with_desktop(DESKTOP_BOOTSTRAP)
            .unwrap();
        let handle = ServerRuntime::start(config).await.expect("server starts");
        let client = http();
        let admin = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP).await;
        let admin_token = admin["access_token"].as_str().unwrap().to_owned();
        Self {
            handle,
            _root: root,
            client,
            admin_token,
        }
    }

    fn url(&self, path: &str) -> String {
        format!(
            "http://127.0.0.1:{}{}",
            self.handle.local_addr().port(),
            path
        )
    }

    async fn read_only_token(&self) -> String {
        let pairing = json_ok(
            self.client
                .post(self.url("/api/auth/pairing-token"))
                .bearer_auth(&self.admin_token)
                .json(&json!({"label": "read only", "scopes": ["orchestration:read"]}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        let restricted = exchange_token(
            &self.client,
            &self.handle,
            pairing["credential"].as_str().unwrap(),
        )
        .await;
        assert_eq!(restricted["scope"], "orchestration:read");
        restricted["access_token"].as_str().unwrap().to_owned()
    }

    async fn socket(&self, token: &str) -> Socket {
        let authority = format!("127.0.0.1:{}", self.handle.local_addr().port());
        self.socket_at(token, &authority, &[]).await
    }

    /// An RPC socket to `authority` (an address of this server) with extra upgrade headers.
    async fn socket_at(&self, token: &str, authority: &str, headers: &[(&str, &str)]) -> Socket {
        let ticket = json_ok(
            self.client
                .post(self.url("/api/auth/websocket-ticket"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap(),
        )
        .await["ticket"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut request = format!("ws://{authority}/ws?wsTicket={ticket}")
            .into_client_request()
            .unwrap();
        for (name, value) in headers {
            request.headers_mut().insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        connect_async(request).await.expect("WebSocket connects").0
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn exchange_token(client: &Client, handle: &ServerHandle, credential: &str) -> Value {
    json_ok(
        client
            .post(format!("http://{}/oauth/token", handle.local_addr()))
            .form(&[
                ("grant_type", TOKEN_GRANT_TYPE),
                ("subject_token", credential),
                ("subject_token_type", BOOTSTRAP_TOKEN_TYPE),
                ("requested_token_type", ACCESS_TOKEN_TYPE),
            ])
            .send()
            .await
            .unwrap(),
    )
    .await
}

async fn json_ok(response: reqwest::Response) -> Value {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// Sends one RPC request and returns its exit.
async fn call(socket: &mut Socket, id: &str, tag: &str, payload: Value) -> Value {
    socket
        .send(Message::Text(
            json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    loop {
        let frame = timeout(Duration::from_secs(10), next_frame_past_heartbeat(socket))
            .await
            .expect("RPC response")
            .expect("socket open")
            .expect("valid frame");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: Value = serde_json::from_str(&text).unwrap();
        if message["_tag"] == "Exit" && message["requestId"] == id {
            return message["exit"].clone();
        }
    }
}

fn success(exit: &Value) -> &Value {
    assert_eq!(exit["_tag"], "Success", "{exit}");
    &exit["value"]
}

fn failure(exit: &Value) -> &Value {
    assert_eq!(exit["_tag"], "Failure", "{exit}");
    &exit["cause"][0]["error"]
}

#[tokio::test]
async fn gateway_open_requires_operate_scope() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let server = Server::start().await;
    let url = format!("http://localhost:{}/", upstream.port());
    let payload = json!({"threadId": THREAD, "url": url});

    let mut read_only = server.socket(&server.read_only_token().await).await;
    let denied = call(&mut read_only, "1", "preview.gatewayOpen", payload.clone()).await;
    let denied = failure(&denied);
    assert_eq!(denied["_tag"], "EnvironmentAuthorizationError");
    assert_eq!(denied["requiredScope"], "orchestration:operate");

    let mut operator = server.socket(&server.admin_token).await;
    let opened = call(&mut operator, "1", "preview.gatewayOpen", payload).await;
    let opened = success(&opened);
    let gateway_port = u16::try_from(opened["gatewayPort"].as_u64().unwrap()).unwrap();
    // The capability is bound to the caller's live session, checked by the real auth service.
    let cookie = bootstrap(gateway_port, opened["capability"].as_str().unwrap())
        .await
        .expect("the caller's capability bootstraps");
    let body = http()
        .get(format!("http://127.0.0.1:{gateway_port}/"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "hello from upstream");

    let https = call(
        &mut operator,
        "2",
        "preview.gatewayOpen",
        json!({"threadId": THREAD, "url": format!("https://localhost:{}/", upstream.port())}),
    )
    .await;
    let https = failure(&https);
    assert_eq!(https["_tag"], "PreviewGatewayError");
    assert_eq!(https["reason"], "https-unsupported");

    server.handle.shutdown();
    wait_until_closed(gateway_port).await;
}

#[tokio::test]
async fn gateway_open_refuses_a_proxied_caller() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let server = Server::start().await;
    let payload =
        json!({"threadId": THREAD, "url": format!("http://localhost:{}/", upstream.port())});
    let authority = format!("127.0.0.1:{}", server.handle.local_addr().port());
    // A reverse proxy on this machine names the public host in `X-Forwarded-Host`; Tailscale
    // Serve and proxies that pass `Host` through send it directly.
    for header in [
        ("x-forwarded-host", "bibcode.example.test"),
        ("host", "bibcode.example.test"),
    ] {
        let mut socket = server
            .socket_at(&server.admin_token, &authority, &[header])
            .await;
        let exit = call(&mut socket, "1", "preview.gatewayOpen", payload.clone()).await;
        let error = failure(&exit);
        assert_eq!(error["_tag"], "PreviewGatewayError", "{header:?}");
        assert_eq!(error["reason"], "not-reachable", "{header:?}");
        assert_eq!(
            error["message"],
            "Previews aren't available through a proxied address."
        );
    }
    server.handle.shutdown();
}

/// On a wildcard bind the gateway listens only on the address the client reached, not on
/// every interface. Linux routes all of `127.0.0.0/8` to loopback, so `127.0.0.2` stands in
/// for a LAN address.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_gateway_binds_the_address_the_client_reached() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let server = Server::start_on("0.0.0.0").await;
    let port = server.handle.local_addr().port();
    let mut socket = server
        .socket_at(&server.admin_token, &format!("127.0.0.2:{port}"), &[])
        .await;
    let exit = call(
        &mut socket,
        "1",
        "preview.gatewayOpen",
        json!({"threadId": THREAD, "url": format!("http://localhost:{}/", upstream.port())}),
    )
    .await;
    let gateway_port = u16::try_from(success(&exit)["gatewayPort"].as_u64().unwrap()).unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.2", gateway_port))
            .await
            .is_ok()
    );
    assert!(
        !listening(gateway_port).await,
        "the listener is not on the server's wildcard bind"
    );
    server.handle.shutdown();
}

#[tokio::test]
async fn the_open_url_route_is_wired_in_a_real_server() {
    let server = Server::start().await;
    let response = server
        .client
        .post(server.url("/api/preview/open-url"))
        .bearer_auth("not-a-token")
        .json(&json!({"url": "http://localhost:5173/"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"], "invalid_open_url_credential");
    server.handle.shutdown();
}

#[tokio::test]
async fn thread_delete_closes_previews_and_targets() {
    let upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let tabless_upstream = upstream_on("127.0.0.1:0").await.unwrap();
    let server = Server::start().await;
    let workspace = server._root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut socket = server.socket(&server.admin_token).await;
    for (id, command) in [
        (
            "1",
            json!({
                "type": "project.create", "commandId": "create-project", "projectId": "project-1",
                "title": "Project", "workspaceRoot": workspace, "defaultModelSelection": null,
                "createdAt": "2026-10-08T00:00:00.000Z"
            }),
        ),
        (
            "2",
            json!({
                "type": "thread.create", "commandId": "create-thread", "threadId": THREAD,
                "projectId": "project-1", "title": "Thread",
                "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "runtimeMode": "full-access", "branch": null, "worktreePath": null,
                "createdAt": "2026-10-08T00:00:00.000Z"
            }),
        ),
    ] {
        let exit = call(&mut socket, id, "orchestration.dispatchCommand", command).await;
        success(&exit);
    }
    let url = format!("http://localhost:{}/", upstream.port());
    let exit = call(
        &mut socket,
        "3",
        "preview.open",
        json!({"threadId": THREAD, "url": url}),
    )
    .await;
    success(&exit);
    let exit = call(
        &mut socket,
        "4",
        "preview.gatewayOpen",
        json!({"threadId": THREAD, "url": url}),
    )
    .await;
    let gateway_port = u16::try_from(success(&exit)["gatewayPort"].as_u64().unwrap()).unwrap();
    // Only thread deletion closes a target that no preview tab points at.
    let exit = call(
        &mut socket,
        "7",
        "preview.gatewayOpen",
        json!({"threadId": THREAD, "url": format!("http://localhost:{}/", tabless_upstream.port())}),
    )
    .await;
    let tabless_port = u16::try_from(success(&exit)["gatewayPort"].as_u64().unwrap()).unwrap();
    assert!(listening(gateway_port).await && listening(tabless_port).await);

    let exit = call(
        &mut socket,
        "5",
        "orchestration.dispatchCommand",
        json!({"type": "thread.delete", "commandId": "delete-thread", "threadId": THREAD}),
    )
    .await;
    success(&exit);

    wait_until_closed(gateway_port).await;
    wait_until_closed(tabless_port).await;
    let exit = call(
        &mut socket,
        "6",
        "preview.list",
        json!({"threadId": THREAD}),
    )
    .await;
    assert_eq!(success(&exit)["sessions"], json!([]));
    server.handle.shutdown();
}
