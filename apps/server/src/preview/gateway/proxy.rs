//! Per-target preview gateway listener: a loopback/LAN HTTP/1.1 listener that bootstraps a
//! gateway session from a single-use capability, then reverse-proxies authenticated
//! requests and WebSocket upgrades to one upstream port. Bodies and tunnels stream through
//! without buffering; every upstream exchange holds a per-target and a global permit for
//! its whole lifetime.

use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::future::BoxFuture;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode, Uri, Version,
    body::{Body, Bytes, Incoming},
    header::{
        ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CACHE_CONTROL, CONNECTION, CONTENT_DISPOSITION,
        CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, HOST, HeaderMap, HeaderName,
        HeaderValue, LOCATION, ORIGIN, RETRY_AFTER, SET_COOKIE, TRANSFER_ENCODING, UPGRADE, VARY,
    },
    http::uri::Authority,
    server::conn::http1 as server_http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use percent_encoding::percent_decode_str;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tokio_util::sync::CancellationToken;

use super::{
    GatewaySession, GatewaySessions,
    capability::CapabilityIssuer,
    frame,
    rewrite::{
        HOP_BY_HOP, bootstrap_page, bootstrap_target, client_origin, filter_set_cookie,
        gateway_cookie_name, gateway_origin_allowed, gateway_session_from_cookie, rewrite_location,
        status_page, strip_request_cookies,
    },
};
use crate::{auth::AuthService, signed_token::now_millis};

#[path = "proxy_ws.rs"]
mod ws;

pub const PER_TARGET_CONNECTIONS: usize = 64;
pub const GLOBAL_CONNECTIONS: usize = 256;
pub const PRINCIPAL_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PRINCIPAL_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_PATH: &str = "/__bibcode/bootstrap";
const EXPIRED_COPY: &str = "This preview link expired. Go back to BiBCode and open it again.";

/// Whether a BiBCode session is still active; `Some(expires_at_ms)` while it is.
pub trait PrincipalCheck: Send + Sync {
    fn session_expiry<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Option<i64>>;
}

impl PrincipalCheck for AuthService {
    fn session_expiry<'a>(&'a self, session_id: &'a str) -> BoxFuture<'a, Option<i64>> {
        Box::pin(self.active_session_expiry(session_id))
    }
}

/// Upstream connection limits. `global` is shared by every target of one server.
#[derive(Clone)]
pub struct GatewayLimits {
    pub per_target: usize,
    pub global: Arc<Semaphore>,
}

#[derive(Clone)]
pub struct GatewayContext {
    pub issuer: Arc<CapabilityIssuer>,
    pub sessions: Arc<GatewaySessions>,
    pub principal: Arc<dyn PrincipalCheck>,
    pub limits: GatewayLimits,
    /// The BiBCode session cookie name, never forwarded upstream or accepted back.
    pub session_cookie_name: String,
    /// How often an active principal is re-checked; [`PRINCIPAL_CHECK_INTERVAL`] in production.
    pub principal_check_interval: Duration,
}

#[derive(Clone, Debug)]
pub struct GatewayTarget {
    pub thread_id: String,
    /// Where the gateway connects: the upstream itself or a local forward to it.
    pub upstream: SocketAddr,
    /// Tried once when `upstream` refuses: the other loopback family of a `localhost` target.
    pub fallback: Option<SocketAddr>,
    /// The port the previewed app listens on in its environment; used for `Host` and redirects.
    pub upstream_port: u16,
    pub environment_label: String,
}

pub struct RunningTarget {
    pub gateway_port: u16,
    pub shutdown: CancellationToken,
    pub last_activity_ms: Arc<AtomicU64>,
    /// Open client connections plus in-flight upstream exchanges and WebSocket tunnels;
    /// zero means idle since `last_activity_ms`.
    pub active: Arc<AtomicUsize>,
}

/// Serves `target` on `listener`. `draining` stops accepting and closes idle connections and
/// WebSocket tunnels while in-flight requests finish; `shutdown` cuts everything.
pub fn start_target(
    listener: TcpListener,
    target: GatewayTarget,
    ctx: GatewayContext,
    shutdown: CancellationToken,
    draining: CancellationToken,
) -> std::io::Result<RunningTarget> {
    let gateway_port = listener.local_addr()?.port();
    let state = Arc::new(Listener {
        per_target: Arc::new(Semaphore::new(ctx.limits.per_target)),
        target,
        ctx,
        gateway_port,
        shutdown: shutdown.clone(),
        draining,
        last_activity_ms: Arc::new(AtomicU64::new(now_millis())),
        active: Arc::new(AtomicUsize::new(0)),
        principal_checked_ms: Mutex::new(HashMap::new()),
    });
    let running = RunningTarget {
        gateway_port,
        shutdown,
        last_activity_ms: state.last_activity_ms.clone(),
        active: state.active.clone(),
    };
    tokio::spawn(accept_loop(listener, state));
    Ok(running)
}

type ProxyBody = BoxBody<Bytes, hyper::Error>;

struct Listener {
    target: GatewayTarget,
    ctx: GatewayContext,
    gateway_port: u16,
    per_target: Arc<Semaphore>,
    shutdown: CancellationToken,
    draining: CancellationToken,
    last_activity_ms: Arc<AtomicU64>,
    active: Arc<AtomicUsize>,
    /// Principal session id -> when it was last confirmed active.
    principal_checked_ms: Mutex<HashMap<String, u64>>,
}

/// Counts one open connection or tunnel in `active` and stamps activity on both ends.
struct Activity {
    active: Arc<AtomicUsize>,
    last_activity_ms: Arc<AtomicU64>,
}

impl Drop for Activity {
    fn drop(&mut self) {
        self.last_activity_ms.store(now_millis(), Ordering::Relaxed);
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Permits for one upstream exchange; held until its connection or tunnel ends.
struct Lease {
    _target: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
    _activity: Activity,
}

impl Listener {
    fn touch(&self) {
        self.last_activity_ms.store(now_millis(), Ordering::Relaxed);
    }

    fn activity(&self) -> Activity {
        self.touch();
        self.active.fetch_add(1, Ordering::Relaxed);
        Activity {
            active: self.active.clone(),
            last_activity_ms: self.last_activity_ms.clone(),
        }
    }

    fn lease(&self) -> Option<Lease> {
        Some(Lease {
            _target: self.per_target.clone().try_acquire_owned().ok()?,
            _global: self.ctx.limits.global.clone().try_acquire_owned().ok()?,
            _activity: self.activity(),
        })
    }

    fn checked(&self) -> MutexGuard<'_, HashMap<String, u64>> {
        self.principal_checked_ms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn check_interval(&self) -> Duration {
        self.ctx
            .principal_check_interval
            .max(Duration::from_millis(1))
    }

    /// Whether the principal is still active. Unless `force`, a confirmation younger than
    /// the check interval is reused. An inactive principal loses every gateway session.
    async fn principal_active(&self, session_id: &str, force: bool) -> bool {
        let now = now_millis();
        let interval = u64::try_from(self.check_interval().as_millis()).unwrap_or(u64::MAX);
        if !force
            && self
                .checked()
                .get(session_id)
                .is_some_and(|at| now.saturating_sub(*at) < interval)
        {
            return true;
        }
        let active = self.principal_expiry(session_id).await.is_some();
        let mut checked = self.checked();
        if active {
            checked.retain(|_, at| now.saturating_sub(*at) < interval);
            checked.insert(session_id.to_owned(), now);
        } else {
            checked.remove(session_id);
            drop(checked);
            self.ctx.sessions.remove_for_principal(session_id);
        }
        active
    }

    /// The principal's unexpired expiry. A check that fails or misses its deadline counts as
    /// inactive, so a stalled auth repository fails closed instead of hanging requests.
    async fn principal_expiry(&self, session_id: &str) -> Option<u64> {
        let deadline = self.check_interval().min(PRINCIPAL_CHECK_TIMEOUT);
        let check = self.ctx.principal.session_expiry(session_id);
        let expiry = tokio::time::timeout(deadline, check).await.ok().flatten()?;
        u64::try_from(expiry)
            .ok()
            .filter(|expiry| *expiry > now_millis())
    }

    /// Resolves when the listener shuts down or the principal stops being active. Rechecks
    /// are due one interval after the principal's last confirmation, so a connection admitted
    /// on a cached confirmation does not extend the revocation window.
    async fn until_revoked(&self, session_id: &str) {
        let interval = self.check_interval();
        let confirmed_ago = self.checked().get(session_id).map_or(interval, |at| {
            Duration::from_millis(now_millis().saturating_sub(*at))
        });
        let first = tokio::time::Instant::now() + interval.saturating_sub(confirmed_ago);
        let watch = async {
            let mut ticks = tokio::time::interval_at(first, interval);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                if !self.principal_active(session_id, true).await {
                    return;
                }
            }
        };
        tokio::select! {
            () = self.shutdown.cancelled() => {}
            () = watch => {}
        }
    }

    fn session_for(&self, headers: &HeaderMap) -> Option<GatewaySession> {
        let id = gateway_session_from_cookie(&joined_cookies(headers), self.gateway_port)?;
        let session = self.ctx.sessions.get(&id, now_millis())?;
        (session.gateway_port == self.gateway_port
            && session.upstream_port == self.target.upstream_port
            && session.thread_id == self.target.thread_id)
            .then_some(session)
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<Listener>) {
    loop {
        let accepted = tokio::select! {
            () = state.shutdown.cancelled() => break,
            () = state.draining.cancelled() => break,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                tokio::spawn(serve_connection(stream, state.clone()));
            }
            Err(error) => {
                // Usually descriptor exhaustion; back off instead of spinning.
                tracing::debug!(%error, port = state.gateway_port, "preview gateway accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    drop(listener);
    // Draining requests still need their sessions; they end once the drain wait or its
    // deadline does (or at once, when only this target closes).
    state.shutdown.cancelled().await;
    state.ctx.sessions.remove_for_port(state.gateway_port);
}

async fn serve_connection(stream: TcpStream, state: Arc<Listener>) {
    let _activity = state.activity();
    let service = service_fn({
        let state = state.clone();
        move |request| {
            let state = state.clone();
            async move { Ok::<_, Infallible>(handle(state, request).await) }
        }
    });
    let connection = server_http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades();
    let mut connection = std::pin::pin!(connection);
    tokio::select! {
        _ = connection.as_mut() => return,
        () = state.shutdown.cancelled() => return,
        () = state.draining.cancelled() => {}
    }
    // Draining: an idle connection closes now, an in-flight request finishes first.
    connection.as_mut().graceful_shutdown();
    tokio::select! {
        _ = connection => {}
        () = state.shutdown.cancelled() => {}
    }
}

/// Responses to a preview a browser-mode BiBCode UI opened, the gateway's own pages
/// included, may be framed by that UI only (see `frame::apply_frame_policy`). Other
/// sessions (desktop views, system-browser tabs) keep the upstream's framing rules,
/// and only the preview's own pages may frame them.
async fn handle(state: Arc<Listener>, request: Request<Incoming>) -> Response<ProxyBody> {
    let ui_origin = framing_ui_origin(&state, &request);
    let mut response = handle_request(state, request).await;
    let gateway_page = response.extensions().get::<GatewayPage>().is_some();
    let headers = response.headers_mut();
    match ui_origin {
        Some(ui_origin) => frame::apply_frame_policy(headers, &ui_origin),
        // A gateway page (expired link, nothing listening) holds no upstream content or
        // authority, so a frame whose session is gone can still explain what happened.
        None if gateway_page => {}
        None => frame::apply_self_only_framing(headers),
    }
    // The framing rules depend on the gateway session.
    headers.append(VARY, HeaderValue::from_static("cookie"));
    response
}

/// Marks a response the gateway wrote itself (its own pages), as opposed to one
/// forwarded from the upstream.
#[derive(Clone, Copy)]
struct GatewayPage;

/// The BiBCode UI allowed to frame this request's preview: a bootstrap's validated
/// `ui=`, or else the browser's current session's, so opening the same target in a
/// tab (which replaces the shared cookie) keeps an open frame working.
fn framing_ui_origin(state: &Listener, request: &Request<Incoming>) -> Option<String> {
    let current = state
        .session_for(request.headers())
        .and_then(|session| session.ui_origin);
    if request.uri().path() != BOOTSTRAP_PATH {
        return current;
    }
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|host| valid_host(host))
        .unwrap_or_default();
    bootstrap_ui_origin(request.uri().query().unwrap_or_default(), host).or(current)
}

/// The validated BiBCode UI origin a bootstrap link carries in `ui=`.
fn bootstrap_ui_origin(query: &str, host: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "ui")
            .then(|| percent_decode_str(value).decode_utf8().ok())
            .flatten()
            .and_then(|ui| frame::validated_ui_origin(&ui, host))
    })
}

async fn handle_request(state: Arc<Listener>, request: Request<Incoming>) -> Response<ProxyBody> {
    state.touch();
    let Some(host) = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|host| valid_host(host))
        .map(str::to_owned)
    else {
        return bad_request();
    };
    if request.uri().path() == BOOTSTRAP_PATH {
        let ui_origin = framing_ui_origin(&state, &request);
        return bootstrap(&state, request.uri().query().unwrap_or_default(), ui_origin).await;
    }
    if request.uri().path() == frame::FRAME_SCRIPT_PATH {
        return frame_script();
    }
    let Some(session) = state.session_for(request.headers()) else {
        return expired();
    };
    if !state
        .principal_active(&session.principal_session_id, false)
        .await
    {
        return expired();
    }
    let client_origin = client_origin("http", &host);
    let websocket = ws::is_websocket(request.headers());
    let safe_method = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok());
    if (websocket || !safe_method) && !gateway_origin_allowed(origin, &client_origin) {
        return page(
            StatusCode::FORBIDDEN,
            "Request blocked",
            "This request didn't come from the preview itself, so the gateway blocked it.",
        );
    }
    let Some(lease) = state.lease() else {
        let mut response = page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Preview is busy",
            "This preview has too many open connections. Try again in a moment.",
        );
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };
    forward(
        state,
        request,
        session.principal_session_id,
        &client_origin,
        websocket,
        lease,
    )
    .await
}

async fn bootstrap(
    state: &Listener,
    query: &str,
    ui_origin: Option<String>,
) -> Response<ProxyBody> {
    let (mut cap, mut to) = (None, None);
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decoded = percent_decode_str(value)
            .decode_utf8()
            .ok()
            .map(|value| value.into_owned());
        match name {
            "cap" => cap = decoded,
            "to" => to = decoded,
            _ => {}
        }
    }
    // Validate the target first so a malformed link does not burn its capability.
    let Some(to) = to.as_deref().and_then(bootstrap_target) else {
        return bad_request();
    };
    let Some(claims) = cap.and_then(|cap| {
        state
            .ctx
            .issuer
            .redeem(&cap, state.gateway_port, now_millis())
            .ok()
    }) else {
        return expired();
    };
    if claims.upstream_port != state.target.upstream_port
        || claims.thread_id != state.target.thread_id
    {
        return expired();
    }
    let Some(expiry) = state.principal_expiry(&claims.session_id).await else {
        return expired();
    };
    let id = state
        .ctx
        .sessions
        .create(&claims, expiry, ui_origin.clone());
    state
        .checked()
        .insert(claims.session_id.clone(), now_millis());
    let mut response = html(StatusCode::OK, bootstrap_page(&to, ui_origin.as_deref()));
    // Lax: an OAuth provider's top-level GET redirect back to the app carries
    // it; cross-site subrequests do not, and the Origin rule refuses cross-site
    // writes and WebSocket upgrades.
    let cookie = format!(
        "{}={id}; HttpOnly; SameSite=Lax; Path=/",
        gateway_cookie_name(state.gateway_port)
    );
    insert(response.headers_mut(), SET_COOKIE, &cookie);
    response.headers_mut().insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    response
}

async fn forward(
    state: Arc<Listener>,
    mut request: Request<Incoming>,
    principal: String,
    client_origin: &str,
    websocket: bool,
    lease: Lease,
) -> Response<ProxyBody> {
    let downstream_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    let (mut parts, body) = request.into_parts();
    parts.uri = parts
        .uri
        .path_and_query()
        .cloned()
        .map_or_else(|| Uri::from_static("/"), Uri::from);
    parts.version = Version::HTTP_11;
    upstream_request_headers(
        &mut parts.headers,
        state.target.upstream_port,
        &state.ctx.session_cookie_name,
        client_origin,
        websocket,
    );
    if !body.is_end_stream() && body.size_hint().exact().is_none() {
        // Hyper only chunks unknown-length bodies for methods that usually carry one, so a
        // streamed GET body would otherwise be sent as empty.
        parts
            .headers
            .insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
    }
    let upstream_request = Request::from_parts(parts, body);

    let Some(stream) = connect_upstream(&state.target).await else {
        return unreachable(&state);
    };
    let _ = stream.set_nodelay(true);
    let Ok((mut sender, connection)) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    else {
        return unreachable(&state);
    };
    let lease = Arc::new(lease);
    tokio::spawn({
        let (state, principal, lease) = (state.clone(), principal.clone(), lease.clone());
        async move {
            let _lease = lease;
            tokio::select! {
                _ = connection.with_upgrades() => {}
                () = state.until_revoked(&principal) => {}
            }
        }
    });
    let Ok(response) = sender.send_request(upstream_request).await else {
        return unreachable(&state);
    };
    drop(sender);

    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let Some(downstream_upgrade) = downstream_upgrade else {
            return unreachable(&state);
        };
        return ws::switch_protocols(
            state,
            principal,
            lease,
            downstream_upgrade,
            response,
            client_origin,
        );
    }

    let (mut parts, body) = response.into_parts();
    rewrite_response_headers(&mut parts.headers, &state, client_origin);
    if injectable_html(parts.status, &parts.headers) {
        parts.headers.remove(CONTENT_LENGTH);
        return Response::from_parts(parts, frame::inject_frame_script(body).boxed());
    }
    Response::from_parts(parts, body.boxed())
}

/// A successful, uncompressed HTML page the navigation reporter can join.
fn injectable_html(status: StatusCode, headers: &HeaderMap) -> bool {
    let html = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
        });
    let identity = headers
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| value.trim().eq_ignore_ascii_case("identity"));
    // A download reaches the user exactly as the app sent it.
    let attachment = headers
        .get(CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("attachment"))
        });
    // `no-transform` forbids intermediaries from changing the body.
    let no_transform = headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"));
    status == StatusCode::OK && html && identity && !attachment && !no_transform
}

fn frame_script() -> Response<ProxyBody> {
    let mut response = Response::new(
        Full::new(Bytes::from_static(frame::FRAME_SCRIPT.as_bytes()))
            .map_err(|never| match never {})
            .boxed(),
    );
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Connects to the target's upstream; a refused `localhost` upstream is retried once on the
/// other loopback family, where a restarted dev server may now listen.
async fn connect_upstream(target: &GatewayTarget) -> Option<TcpStream> {
    let connect = |addr| tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, TcpStream::connect(addr));
    match connect(target.upstream).await {
        Ok(Ok(stream)) => Some(stream),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            connect(target.fallback?).await.ok()?.ok()
        }
        _ => None,
    }
}

fn upstream_request_headers(
    headers: &mut HeaderMap,
    upstream_port: u16,
    session_cookie_name: &str,
    client_origin: &str,
    websocket: bool,
) {
    let cookies = joined_cookies(headers);
    // Only an `Origin` the gateway already validated is rewritten; upstreams that compare
    // it with `Host` (WebSocket servers, server actions) then see a same-origin request.
    let validated_origin = headers
        .get(ORIGIN)
        .is_some_and(|origin| origin.as_bytes() == client_origin.as_bytes());
    strip_hop_by_hop(headers);
    for name in [COOKIE, AUTHORIZATION, HeaderName::from_static("dpop")] {
        headers.remove(name);
    }
    if let Some(kept) = strip_request_cookies(&cookies, session_cookie_name)
        && let Ok(kept) = HeaderValue::from_bytes(&kept)
    {
        headers.insert(COOKIE, kept);
    }
    // A page load's HTML must arrive uncompressed for the reporter to join it.
    // Plain-HTTP origins get no Fetch Metadata, so a request for HTML counts too.
    let navigation = match headers.get("sec-fetch-dest") {
        Some(dest) => dest.as_bytes() == b"document" || dest.as_bytes() == b"iframe",
        None => headers
            .get(ACCEPT)
            .and_then(|accept| accept.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html")),
    };
    if navigation {
        headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    }
    let authority = format!("localhost:{upstream_port}");
    insert(headers, HOST, &authority);
    if validated_origin {
        insert(headers, ORIGIN, &format!("http://{authority}"));
    }
    if websocket {
        headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
    }
}

fn rewrite_response_headers(headers: &mut HeaderMap, state: &Listener, client_origin: &str) {
    strip_hop_by_hop(headers);
    let cookies: Vec<HeaderValue> = headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| filter_set_cookie(value.as_bytes(), &state.ctx.session_cookie_name))
        .filter_map(|value| HeaderValue::from_bytes(&value).ok())
        .collect();
    headers.remove(SET_COOKIE);
    for cookie in cookies {
        headers.append(SET_COOKIE, cookie);
    }
    let location = headers
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| rewrite_location(value, state.target.upstream_port, client_origin));
    if let Some(location) = location {
        insert(headers, LOCATION, &location);
    }
}

/// Removes the fixed hop-by-hop headers and any header the `Connection` header nominates.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let nominated: Vec<HeaderName> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|token| HeaderName::from_bytes(token.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
}

fn joined_cookies(headers: &HeaderMap) -> Vec<u8> {
    headers
        .get_all(COOKIE)
        .iter()
        .map(HeaderValue::as_bytes)
        .collect::<Vec<_>>()
        .join(&b"; "[..])
}

/// `host[:port]` with nothing a browser would not send: no userinfo, path, whitespace, or
/// control characters.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"@/\\?#".contains(&byte))
        && host.parse::<Authority>().is_ok()
        // `Authority` accepts any port text; a port, when present, must be numeric.
        && host[host.rfind(']').map_or(0, |end| end + 1)..]
            .split_once(':')
            .is_none_or(|(_, port)| port.parse::<u16>().is_ok())
}

/// Sets `name` to `value`, or removes it when `value` is not a valid header value.
fn insert(headers: &mut HeaderMap, name: HeaderName, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => {
            headers.insert(name, value);
        }
        Err(_) => {
            headers.remove(name);
        }
    }
}

fn empty() -> ProxyBody {
    Empty::new().map_err(|never| match never {}).boxed()
}

fn html(status: StatusCode, body: String) -> Response<ProxyBody> {
    let mut response = Response::new(
        Full::new(Bytes::from(body))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.extensions_mut().insert(GatewayPage);
    response
}

fn page(status: StatusCode, title: &str, body: &str) -> Response<ProxyBody> {
    html(status, status_page(title, body))
}

fn expired() -> Response<ProxyBody> {
    page(
        StatusCode::UNAUTHORIZED,
        "Preview link expired",
        EXPIRED_COPY,
    )
}

fn bad_request() -> Response<ProxyBody> {
    page(
        StatusCode::BAD_REQUEST,
        "Invalid preview request",
        "This preview address is malformed. Go back to BiBCode and open it again.",
    )
}

fn unreachable(state: &Listener) -> Response<ProxyBody> {
    page(
        StatusCode::BAD_GATEWAY,
        "Nothing is listening",
        &format!(
            "Nothing is listening on port {} on {}.",
            state.target.upstream_port, state.target.environment_label
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_validation_accepts_only_host_and_port() {
        for good in ["127.0.0.1:40001", "localhost", "[::1]:8080", "box.lan:3000"] {
            assert!(valid_host(good), "{good}");
        }
        for bad in [
            "",
            "a b",
            "user@host",
            "host/path",
            "host\t",
            "host:port",
            "host:",
            "a:1:2",
        ] {
            assert!(!valid_host(bad), "{bad:?}");
        }
    }

    #[test]
    fn connection_nominated_and_fixed_hop_by_hop_headers_are_stripped() {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive, x-secret"));
        headers.insert("x-secret", HeaderValue::from_static("1"));
        headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        headers.insert("x-kept", HeaderValue::from_static("1"));
        strip_hop_by_hop(&mut headers);
        assert_eq!(
            headers.keys().map(HeaderName::as_str).collect::<Vec<_>>(),
            ["x-kept"]
        );
    }
}
