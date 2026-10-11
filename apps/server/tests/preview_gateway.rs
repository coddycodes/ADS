//! Preview gateway listener: bootstrap, authentication, origin enforcement, header
//! rewriting, WebSocket tunnelling, limits, revocation, and shutdown against an in-process
//! upstream.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message as AxumMessage, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use bibcode_server::{
    preview::gateway::{
        GatewaySessions,
        capability::CapabilityIssuer,
        proxy::{
            GatewayContext, GatewayLimits, GatewayTarget, PrincipalCheck, RunningTarget,
            start_target,
        },
    },
    signed_token::now_millis,
};
use futures_util::{SinkExt, StreamExt, future::BoxFuture};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, Semaphore},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{self, Message, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;

const SESSION_COOKIE: &str = "bibcode_session_3773";
const EXPIRED_COPY: &str = "This preview link expired. Go back to BiBCode and open it again.";
const THREAD: &str = "thread-1";

/// Principal fake: the session's expiry, and whether checks hang (a stalled repository).
struct FakePrincipal(Mutex<Option<i64>>, AtomicBool);

impl PrincipalCheck for FakePrincipal {
    fn session_expiry<'a>(&'a self, _session_id: &'a str) -> BoxFuture<'a, Option<i64>> {
        if self.1.load(Ordering::SeqCst) {
            return Box::pin(std::future::pending());
        }
        let expiry = *self.0.lock().unwrap();
        Box::pin(async move { expiry })
    }
}

#[derive(Clone, Default)]
struct Upstream {
    hits: Arc<AtomicUsize>,
    release: Arc<Notify>,
}

async fn echo(
    State(upstream): State<Upstream>,
    headers: HeaderMap,
) -> Json<BTreeMap<String, String>> {
    upstream.hits.fetch_add(1, Ordering::SeqCst);
    Json(
        headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect(),
    )
}

async fn slow(State(upstream): State<Upstream>) -> &'static str {
    upstream.hits.fetch_add(1, Ordering::SeqCst);
    upstream.release.notified().await;
    "done"
}

async fn body(text: String) -> String {
    text
}

async fn redirect(headers: HeaderMap) -> Response {
    let host = headers[header::HOST].to_str().unwrap();
    let mut response = (
        StatusCode::FOUND,
        [(header::LOCATION, format!("http://{host}/next"))],
    )
        .into_response();
    for cookie in [
        "app=1; Domain=localhost; Path=/",
        "bibcode_session_3773=stolen; Path=/",
        "bibcode-gw-1=stolen; Path=/",
    ] {
        response
            .headers_mut()
            .append(header::SET_COOKIE, cookie.parse().unwrap());
    }
    response
}

/// Mirrors the brainstorm companion: refuses unless `Origin` is exactly `http://` + `Host`.
async fn ws_echo(
    State(upstream): State<Upstream>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    upstream.hits.fetch_add(1, Ordering::SeqCst);
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if host.is_none() || origin != host.map(|host| format!("http://{host}")).as_deref() {
        return StatusCode::FORBIDDEN.into_response();
    }
    upgrade.on_upgrade(|mut socket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            if let AxumMessage::Text(text) = message
                && socket.send(AxumMessage::Text(text)).await.is_err()
            {
                break;
            }
        }
    })
}

async fn start_upstream() -> (SocketAddr, Upstream) {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/echo", any(echo))
        .route("/slow", get(slow))
        .route("/redirect", get(redirect))
        .route("/body", any(body))
        .route("/ws", get(ws_echo))
        .with_state(upstream.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, upstream)
}

struct Gateway {
    running: RunningTarget,
    issuer: Arc<CapabilityIssuer>,
    principal: Arc<FakePrincipal>,
    upstream_port: u16,
    base: String,
    http: reqwest::Client,
}

impl Gateway {
    async fn start(upstream: SocketAddr, per_target: usize, check_interval: Duration) -> Self {
        let issuer = Arc::new(CapabilityIssuer::new(vec![9; 32]));
        let principal = Arc::new(FakePrincipal(
            Mutex::new(Some(i64::try_from(now_millis()).unwrap() + 3_600_000)),
            AtomicBool::new(false),
        ));
        let ctx = GatewayContext {
            issuer: issuer.clone(),
            sessions: Arc::new(GatewaySessions::new()),
            principal: principal.clone(),
            limits: GatewayLimits {
                per_target,
                global: Arc::new(Semaphore::new(256)),
            },
            session_cookie_name: SESSION_COOKIE.to_owned(),
            principal_check_interval: check_interval,
        };
        let target = GatewayTarget {
            thread_id: THREAD.to_owned(),
            upstream,
            fallback: None,
            upstream_port: upstream.port(),
            environment_label: "devbox".to_owned(),
        };
        Self::start_target(target, ctx, issuer, principal).await
    }

    async fn start_target(
        target: GatewayTarget,
        ctx: GatewayContext,
        issuer: Arc<CapabilityIssuer>,
        principal: Arc<FakePrincipal>,
    ) -> Self {
        let upstream_port = target.upstream_port;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let running = start_target(
            listener,
            target,
            ctx,
            CancellationToken::new(),
            CancellationToken::new(),
        )
        .expect("gateway listens");
        let base = format!("http://127.0.0.1:{}", running.gateway_port);
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self {
            running,
            issuer,
            principal,
            upstream_port,
            base,
            http,
        }
    }

    async fn default_for(upstream: SocketAddr) -> Self {
        Self::start(upstream, 64, Duration::from_secs(30)).await
    }

    fn capability_for(&self, upstream_port: u16, thread_id: &str) -> String {
        self.issuer
            .issue(
                self.running.gateway_port,
                upstream_port,
                thread_id,
                "session-1",
                now_millis(),
            )
            .0
    }

    fn capability(&self) -> String {
        self.capability_for(self.upstream_port, THREAD)
    }

    async fn bootstrap_with(&self, cap: &str) -> reqwest::Response {
        self.http
            .get(format!(
                "{}/__bibcode/bootstrap?cap={cap}&to=%2F%3Fkey%3Dab",
                self.base
            ))
            .send()
            .await
            .unwrap()
    }

    /// `name=value` of a fresh gateway session cookie.
    async fn session_cookie(&self) -> String {
        let response = self.bootstrap_with(&self.capability()).await;
        assert_eq!(response.status(), 200);
        let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        set_cookie.split(';').next().unwrap().to_owned()
    }

    fn origin(&self) -> String {
        self.base.clone()
    }

    async fn websocket(
        &self,
        cookie: &str,
        origin: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        tungstenite::Error,
    > {
        let mut request = format!("ws://127.0.0.1:{}/ws", self.running.gateway_port)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        connect_async(request).await.map(|(socket, _)| socket)
    }
}

fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn bootstrap_sets_port_named_cookie_and_redirects_via_page() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;

    let response = gateway.bootstrap_with(&gateway.capability()).await;

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let set_cookie = response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        set_cookie.starts_with(&format!("bibcode-gw-{}=", gateway.running.gateway_port)),
        "{set_cookie}"
    );
    // Lax, so an OAuth provider's top-level redirect back to the app carries it.
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(
            set_cookie.contains(attribute),
            "{set_cookie} lacks {attribute}"
        );
    }
    assert!(!set_cookie.contains("SameSite=Strict"), "{set_cookie}");
    let page = response.text().await.unwrap();
    assert!(page.contains("location.replace(\"/?key=ab\")"), "{page}");
}

#[tokio::test]
async fn bootstrap_rejects_invalid_target_without_consuming_the_capability() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cap = gateway.capability();

    let response = gateway
        .http
        .get(format!(
            "{}/__bibcode/bootstrap?cap={cap}&to=https%3A%2F%2Fevil.test%2F",
            gateway.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(response.headers().get(header::SET_COOKIE).is_none());

    assert_eq!(gateway.bootstrap_with(&cap).await.status(), 200);
}

#[tokio::test]
async fn request_without_cookie_is_401_with_copy() {
    let (upstream, hits) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;

    let response = gateway
        .http
        .get(format!("{}/echo", gateway.base))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 401);
    assert!(response.text().await.unwrap().contains(EXPIRED_COPY));
    assert_eq!(hits.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replayed_capability_is_401() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cap = gateway.capability();

    assert_eq!(gateway.bootstrap_with(&cap).await.status(), 200);
    let replay = gateway.bootstrap_with(&cap).await;
    assert_eq!(replay.status(), 401);
    assert!(replay.headers().get(header::SET_COOKIE).is_none());
    assert!(replay.text().await.unwrap().contains(EXPIRED_COPY));

    // Capabilities minted for another thread or upstream port are refused too.
    let other_thread = gateway.capability_for(gateway.upstream_port, "thread-2");
    assert_eq!(gateway.bootstrap_with(&other_thread).await.status(), 401);
    let other_port = gateway.capability_for(gateway.upstream_port.wrapping_add(1), THREAD);
    assert_eq!(gateway.bootstrap_with(&other_port).await.status(), 401);

    // A principal that is no longer active cannot bootstrap.
    *gateway.principal.0.lock().unwrap() = None;
    assert_eq!(
        gateway.bootstrap_with(&gateway.capability()).await.status(),
        401
    );
}

#[tokio::test]
async fn rejects_invalid_host_header() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let mut stream = TcpStream::connect(("127.0.0.1", gateway.running.gateway_port))
        .await
        .unwrap();
    stream
        .write_all(b"GET /echo HTTP/1.1\r\nHost: user@evil.test\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[tokio::test]
async fn forwards_get_with_host_rewrite_and_stripped_credentials() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cookie = gateway.session_cookie().await;

    let response = gateway
        .http
        .get(format!("{}/echo", gateway.base))
        .header(
            header::COOKIE,
            format!("{cookie}; {SESSION_COOKIE}=secret; app=1; bibcode-gw-9=other; theme=dark"),
        )
        .header(header::AUTHORIZATION, "Bearer secret")
        .header("dpop", "proof")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let seen: BTreeMap<String, String> = response.json().await.unwrap();
    assert_eq!(
        seen.get("host").map(String::as_str),
        Some(format!("localhost:{}", gateway.upstream_port).as_str())
    );
    assert_eq!(
        seen.get("cookie").map(String::as_str),
        Some("app=1; theme=dark")
    );
    assert!(!seen.contains_key("authorization"));
    assert!(!seen.contains_key("dpop"));
}

#[tokio::test]
async fn refuses_cross_origin_websocket_and_post_before_forwarding() {
    let (upstream, hits) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cookie = gateway.session_cookie().await;

    let foreign = gateway
        .http
        .post(format!("{}/echo", gateway.base))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, "http://127.0.0.1:1")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    let missing = gateway
        .http
        .post(format!("{}/echo", gateway.base))
        .header(header::COOKIE, &cookie)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 403);
    match gateway.websocket(&cookie, "http://127.0.0.1:1").await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 403),
        other => panic!(
            "cross-origin websocket was not refused: {:?}",
            other.is_ok()
        ),
    }
    assert_eq!(hits.hits.load(Ordering::SeqCst), 0);

    // Same-origin POST is forwarded with its validated Origin matched to the upstream Host.
    let same = gateway
        .http
        .post(format!("{}/echo", gateway.base))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, gateway.origin())
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(same.status(), 200);
    assert_eq!(hits.hits.load(Ordering::SeqCst), 1);
    let seen: BTreeMap<String, String> = same.json().await.unwrap();
    assert_eq!(
        seen.get("origin").map(String::as_str),
        Some(format!("http://localhost:{}", gateway.upstream_port).as_str())
    );

    // A cross-origin GET is safe, so it is forwarded with its Origin untouched.
    let cross_get = gateway
        .http
        .get(format!("{}/echo", gateway.base))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, "http://127.0.0.1:1")
        .send()
        .await
        .unwrap();
    let seen: BTreeMap<String, String> = cross_get.json().await.unwrap();
    assert_eq!(
        seen.get("origin").map(String::as_str),
        Some("http://127.0.0.1:1")
    );
}

#[tokio::test]
async fn proxies_a_websocket_whose_upstream_enforces_origin_equals_host() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cookie = gateway.session_cookie().await;

    let mut socket = gateway
        .websocket(&cookie, &gateway.origin())
        .await
        .expect("websocket through gateway");
    socket.send(Message::text("hello")).await.unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("echo arrives")
        .unwrap()
        .unwrap();
    assert_eq!(echoed, Message::text("hello"));
    assert!(gateway.running.active.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn rewrites_set_cookie_domain_and_location() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cookie = gateway.session_cookie().await;

    let response = gateway
        .http
        .get(format!("{}/redirect", gateway.base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 302);
    assert_eq!(
        response.headers()[header::LOCATION],
        format!("{}/next", gateway.base).as_str()
    );
    let cookies: Vec<&str> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(cookies, ["app=1; Path=/"]);
}

#[tokio::test]
async fn a_refused_upstream_is_retried_once_on_the_other_loopback_family() {
    // The dev server restarted on IPv6 after the target was probed on IPv4.
    let listener = match TcpListener::bind("[::1]:0").await {
        Ok(listener) => listener,
        Err(error) => {
            println!("skipping: IPv6 loopback is unavailable ({error})");
            return;
        }
    };
    let ipv6 = listener.local_addr().unwrap();
    let app = Router::new().route("/", get(|| async { "from ipv6" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let refused: SocketAddr = format!("127.0.0.1:{}", ipv6.port()).parse().unwrap();
    // Bound but never listening: connections are refused, and no parallel test can take the port.
    let reserved = tokio::net::TcpSocket::new_v4().unwrap();
    if let Err(error) = reserved.bind(refused) {
        println!("skipping: {refused} is taken ({error})");
        return;
    }
    let issuer = Arc::new(CapabilityIssuer::new(vec![9; 32]));
    let principal = Arc::new(FakePrincipal(
        Mutex::new(Some(i64::try_from(now_millis()).unwrap() + 3_600_000)),
        AtomicBool::new(false),
    ));
    let target = GatewayTarget {
        thread_id: THREAD.to_owned(),
        upstream: refused,
        fallback: Some(ipv6),
        upstream_port: ipv6.port(),
        environment_label: "devbox".to_owned(),
    };
    let ctx = GatewayContext {
        issuer: issuer.clone(),
        sessions: Arc::new(GatewaySessions::new()),
        principal: principal.clone(),
        limits: GatewayLimits {
            per_target: 64,
            global: Arc::new(Semaphore::new(256)),
        },
        session_cookie_name: SESSION_COOKIE.to_owned(),
        principal_check_interval: Duration::from_secs(30),
    };
    let gateway = Gateway::start_target(target, ctx, issuer, principal).await;
    let cookie = gateway.session_cookie().await;
    let response = gateway
        .http
        .get(format!("{}/", gateway.base))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "from ipv6");
}

#[tokio::test]
async fn unreachable_upstream_is_502_with_copy() {
    let port = closed_port();
    let gateway = Gateway::default_for(SocketAddr::from(([127, 0, 0, 1], port))).await;
    let cookie = gateway.session_cookie().await;

    let response = gateway
        .http
        .get(format!("{}/", gateway.base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 502);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains(&format!("Nothing is listening on port {port} on devbox."))
    );
}

#[tokio::test]
async fn limits_return_503() {
    let (upstream, state) = start_upstream().await;
    let gateway = Gateway::start(upstream, 1, Duration::from_secs(30)).await;
    let cookie = gateway.session_cookie().await;

    let held = tokio::spawn({
        let request = gateway
            .http
            .get(format!("{}/slow", gateway.base))
            .header(header::COOKIE, &cookie);
        async move { request.send().await.unwrap().text().await.unwrap() }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.hits.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("slow request reaches upstream");

    let refused = gateway
        .http
        .get(format!("{}/echo", gateway.base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 503);
    assert_eq!(refused.headers()[header::RETRY_AFTER], "1");

    state.release.notify_one();
    assert_eq!(held.await.unwrap(), "done");
    // The permit drops when the upstream exchange finishes, just after the client reads it.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let after = gateway
                .http
                .get(format!("{}/echo", gateway.base))
                .header(header::COOKIE, &cookie)
                .send()
                .await
                .unwrap();
            if after.status() == 200 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the held permit is released");
}

#[tokio::test]
async fn closes_live_connections_when_the_principal_is_revoked() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::start(upstream, 64, Duration::from_millis(100)).await;
    let cookie = gateway.session_cookie().await;
    let mut socket = gateway
        .websocket(&cookie, &gateway.origin())
        .await
        .expect("websocket through gateway");
    socket.send(Message::text("ping")).await.unwrap();
    assert_eq!(socket.next().await.unwrap().unwrap(), Message::text("ping"));

    *gateway.principal.0.lock().unwrap() = None;

    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(Message::Text(_))) = socket.next().await {}
    })
    .await;
    assert!(closed.is_ok(), "socket stayed open after revocation");
    let response = gateway
        .http
        .get(format!("{}/echo", gateway.base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_stalled_principal_check_fails_closed() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::start(upstream, 64, Duration::from_millis(100)).await;
    let cookie = gateway.session_cookie().await;
    let mut socket = gateway
        .websocket(&cookie, &gateway.origin())
        .await
        .expect("websocket through gateway");

    gateway.principal.1.store(true, Ordering::SeqCst);

    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
    assert!(closed.is_ok(), "socket stayed open while the check hung");
    let bootstrap = tokio::time::timeout(
        Duration::from_secs(2),
        gateway.bootstrap_with(&gateway.capability()),
    )
    .await
    .expect("bootstrap answers while the check hangs");
    assert_eq!(bootstrap.status(), 401);
}

#[tokio::test]
async fn revocation_window_counts_from_the_last_confirmation() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::start(upstream, 64, Duration::from_millis(1_000)).await;
    // Bootstrap confirms the principal; the socket below is admitted on that cached check.
    let cookie = gateway.session_cookie().await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let mut socket = gateway
        .websocket(&cookie, &gateway.origin())
        .await
        .expect("websocket through gateway");

    *gateway.principal.0.lock().unwrap() = None;

    let closed = tokio::time::timeout(Duration::from_millis(800), async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
    assert!(
        closed.is_ok(),
        "socket outlived the confirmation's check window"
    );
}

#[tokio::test]
async fn forwards_a_chunked_get_body() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cookie = gateway.session_cookie().await;
    let port = gateway.running.gateway_port;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream
        .write_all(
            format!(
                "GET /body HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\n\
                 Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("hello"), "{response}");
}

#[tokio::test]
async fn shutdown_cancels_listener() {
    let (upstream, _) = start_upstream().await;
    let gateway = Gateway::default_for(upstream).await;
    let cookie = gateway.session_cookie().await;
    let mut socket = gateway
        .websocket(&cookie, &gateway.origin())
        .await
        .expect("websocket through gateway");

    gateway.running.shutdown.cancel();

    let port = gateway.running.gateway_port;
    tokio::time::timeout(Duration::from_secs(2), async {
        while TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("listener closes");
    let socket_closed = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
    assert!(socket_closed.is_ok(), "live websocket outlived shutdown");
    tokio::time::timeout(Duration::from_secs(2), async {
        while gateway.running.active.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connections are released");
}
