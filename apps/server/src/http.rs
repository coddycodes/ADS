use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        ConnectInfo, FromRef, Request, State, WebSocketUpgrade, connect_info::Connected,
        rejection::ExtensionRejection,
    },
    http::{
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
        header::{
            CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HOST, IF_NONE_MATCH, LOCATION,
        },
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::IncomingStream,
};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::fs::File;
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use tower_http::cors::{AllowCredentials, AllowOrigin, Any, CorsLayer};

use crate::{
    auth,
    config::{ServerConfig, ServerMode},
    maintenance::{
        DESKTOP_MAINTENANCE_TOKEN_HEADER, MAINTENANCE_UPDATE_CANCEL_PATH,
        MAINTENANCE_UPDATE_COMMIT_PATH, MAINTENANCE_UPDATE_PREPARE_PATH,
        MAINTENANCE_UPDATE_STATUS_PATH, MaintenanceError, RpcAdmissionGate, UpdateMaintenance,
        http_mutability,
    },
    preview::gateway::registry::ClientReach,
    production::http_routes::{self, HttpRoutesState},
    remote_update::RemoteUpdateSupport,
    rpc::{
        CHUNKED_RPC_SUBPROTOCOL, E2eePreauthAdmission, MAX_E2EE_CIPHERTEXT_BYTES, RpcRegistry,
        RpcSessionContext, run_session,
    },
};

pub const ENVIRONMENT_DESCRIPTOR_PATH: &str = "/.well-known/bibcode/environment";
pub(crate) const WS_E2EE_PATH: &str = "/ws-e2ee";
pub(crate) const REMOTE_PROTOCOL_VERSION: u32 = 1;
pub(crate) const MIN_COMPATIBLE_REMOTE_PROTOCOL: u32 = 1;
pub const DESKTOP_SHUTDOWN_PATH: &str = "/.well-known/bibcode/desktop/shutdown";
pub const DESKTOP_SHUTDOWN_TOKEN_HEADER: &str = "x-bibcode-desktop-bootstrap-token";

const CONTENT_SECURITY_POLICY_VALUE: &str = "default-src 'self'; connect-src 'self' http: https: ws: wss:; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";
const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const HTML_CACHE_CONTROL: &str = "no-cache";
const MAX_PLAIN_WEBSOCKET_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PLAIN_WEBSOCKET_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteMethod {
    Delete,
    Get,
    Post,
}

impl RouteMethod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSpec {
    pub method: &'static str,
    pub path: &'static str,
}

const fn route(method: RouteMethod, path: &'static str) -> RouteSpec {
    RouteSpec {
        method: method.as_str(),
        path,
    }
}

pub const ROUTE_INVENTORY: &[RouteSpec] = &[
    route(RouteMethod::Get, ENVIRONMENT_DESCRIPTOR_PATH),
    route(RouteMethod::Get, "/api/auth/session"),
    route(RouteMethod::Post, "/api/auth/browser-session"),
    route(RouteMethod::Post, "/oauth/token"),
    route(RouteMethod::Post, "/api/auth/websocket-ticket"),
    route(RouteMethod::Post, "/api/auth/pairing-token"),
    route(RouteMethod::Post, "/api/auth/pairing-offer"),
    route(RouteMethod::Post, "/api/auth/pairing-offer/cancel"),
    route(RouteMethod::Get, "/api/auth/share-state"),
    route(RouteMethod::Get, "/api/auth/pairing-links"),
    route(RouteMethod::Post, "/api/auth/pairing-links/revoke"),
    route(RouteMethod::Get, "/api/auth/clients"),
    route(RouteMethod::Post, "/api/auth/clients/revoke"),
    route(RouteMethod::Post, "/api/auth/clients/revoke-others"),
    route(RouteMethod::Get, "/api/orchestration/snapshot"),
    route(RouteMethod::Post, "/api/orchestration/dispatch"),
    route(RouteMethod::Post, "/api/connect/link-proof"),
    route(RouteMethod::Post, "/api/connect/relay-config"),
    route(RouteMethod::Get, "/api/connect/link-state"),
    route(RouteMethod::Post, "/api/connect/unlink"),
    route(RouteMethod::Post, "/api/bibcode-connect/health"),
    route(RouteMethod::Post, "/api/connect/mint-credential"),
    route(RouteMethod::Post, "/api/bibcode-connect/mint-credential"),
    route(RouteMethod::Get, "/ws"),
    route(RouteMethod::Get, WS_E2EE_PATH),
    route(RouteMethod::Post, "/api/diagnostics/logs.zip"),
    route(RouteMethod::Get, "/api/assets/*"),
    route(RouteMethod::Post, "/api/preview/open-url"),
    route(RouteMethod::Get, "/api/transfers/*"),
    route(RouteMethod::Post, "/api/transfers/*"),
    route(RouteMethod::Post, DESKTOP_SHUTDOWN_PATH),
    route(RouteMethod::Post, MAINTENANCE_UPDATE_PREPARE_PATH),
    route(RouteMethod::Post, MAINTENANCE_UPDATE_COMMIT_PATH),
    route(RouteMethod::Post, MAINTENANCE_UPDATE_CANCEL_PATH),
    route(RouteMethod::Get, MAINTENANCE_UPDATE_STATUS_PATH),
    route(RouteMethod::Post, "/mcp"),
    route(RouteMethod::Delete, "/mcp"),
    route(RouteMethod::Get, "*"),
];

/// Both ends of an accepted connection. `local` is the address the client reached, which on a
/// wildcard bind tells which interface, and so which network, it came in on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionAddrs {
    pub(crate) peer: SocketAddr,
    pub(crate) local: Option<SocketAddr>,
}

impl Connected<IncomingStream<'_, tokio::net::TcpListener>> for ConnectionAddrs {
    fn connect_info(stream: IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self {
            peer: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}

type ConnectionInfo = Result<ConnectInfo<ConnectionAddrs>, ExtensionRejection>;

/// How a WebSocket client reached this server, for `preview.gatewayOpen`.
fn client_reach(connection: ConnectionInfo, headers: &HeaderMap) -> Option<ClientReach> {
    let ConnectInfo(addrs) = connection.ok()?;
    Some(ClientReach {
        local_ip: addrs.local?.ip(),
        host: headers
            .get(HOST)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    })
}

const X_FORWARDED_HOST: &str = "x-forwarded-host";
const X_FORWARDED_PROTO: &str = "x-forwarded-proto";

/// `X-Forwarded-Host` and `X-Forwarded-Proto` are trusted only from a loopback peer (a
/// reverse proxy on this machine), like `X-Forwarded-For`. From such a peer the forwarded
/// host becomes `Host`, so the Origin rule and the preview gateway see the address the
/// browser used (nginx sends `Host: $proxy_host` by default). From any other peer, or an
/// unknown one, both headers are dropped.
async fn trust_forwarded_headers(
    connection: ConnectionInfo,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = connection.ok().map(|ConnectInfo(addrs)| addrs.peer.ip());
    apply_forwarded_headers(request.headers_mut(), peer);
    next.run(request).await
}

fn apply_forwarded_headers(headers: &mut HeaderMap, peer: Option<IpAddr>) {
    if !peer.is_some_and(|peer| peer.to_canonical().is_loopback()) {
        headers.remove(X_FORWARDED_HOST);
        headers.remove(X_FORWARDED_PROTO);
        return;
    }
    // A proxy chain appends; the first entry is the host the browser asked for.
    let forwarded = headers
        .get(X_FORWARDED_HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .and_then(|host| HeaderValue::from_str(host).ok());
    if let Some(host) = forwarded {
        headers.insert(HOST, host);
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub config: Arc<ServerConfig>,
    pub shutdown: CancellationToken,
    pub rpc_registry: RpcRegistry,
    pub e2ee_preauth_admission: E2eePreauthAdmission,
    pub auth: auth::AuthService,
    pub http_routes: HttpRoutesState,
    pub admission_gate: RpcAdmissionGate,
    pub update_maintenance: Option<Arc<UpdateMaintenance>>,
}

impl FromRef<AppState> for HttpRoutesState {
    fn from_ref(state: &AppState) -> Self {
        state.http_routes.clone()
    }
}

pub(crate) fn build_router(state: AppState) -> Router {
    let cors = cors_layer(&state.config);
    let router = http_routes::add_routes(auth::add_routes(Router::<AppState>::new()));
    router
        .route(ENVIRONMENT_DESCRIPTOR_PATH, get(environment_descriptor))
        .route(DESKTOP_SHUTDOWN_PATH, post(desktop_shutdown))
        .route(MAINTENANCE_UPDATE_PREPARE_PATH, post(update_prepare))
        .route(MAINTENANCE_UPDATE_COMMIT_PATH, post(update_commit))
        .route(MAINTENANCE_UPDATE_CANCEL_PATH, post(update_cancel))
        .route(MAINTENANCE_UPDATE_STATUS_PATH, get(update_status))
        .route("/ws", get(websocket))
        .route(WS_E2EE_PATH, get(websocket_e2ee))
        .fallback(static_or_dev)
        .layer(middleware::from_fn_with_state(
            state.admission_gate.clone(),
            request_admission,
        ))
        .layer(middleware::from_fn(trust_forwarded_headers))
        .layer(cors)
        .with_state(state)
}

async fn request_admission(
    State(gate): State<RpcAdmissionGate>,
    request: Request,
    next: Next,
) -> Response {
    let mutability = http_mutability(request.method().as_str(), request.uri().path());
    let operation = format!("HTTP {} {}", request.method(), request.uri().path());
    let Ok(_permit) = gate.admit_named(mutability, operation) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(CACHE_CONTROL, "no-store")],
            Json(json!({
                "_tag": "UpdateMaintenanceActiveError",
                "message": "Persistent mutations are temporarily closed while project data is protected.",
            })),
        )
            .into_response();
    };
    next.run(request).await
}

fn cors_layer(config: &ServerConfig) -> CorsLayer {
    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        // Never allow `x-forwarded-host` (or `-proto`) here. A loopback peer's forwarded host
        // becomes `Host` (`apply_forwarded_headers`), and the cookie Origin rule compares
        // against that `Host`. A page could otherwise make the browser on this machine send a
        // forwarded host equal to its own origin and pass the rule with the user's cookie.
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderName::from_static("b3"),
            axum::http::HeaderName::from_static("traceparent"),
            axum::http::HeaderName::from_static("dpop"),
            axum::http::HeaderName::from_static("idempotency-key"),
            axum::http::HeaderName::from_static(DESKTOP_MAINTENANCE_TOKEN_HEADER),
        ])
        .max_age(std::time::Duration::from_secs(600));
    let Some(dev_url) = &config.dev_url else {
        return layer.allow_origin(Any);
    };
    // The desktop app's origins get no credentials: it sends a bearer token, and cookie
    // requests from them are refused.
    let mut origins = Vec::new();
    if let Ok(origin) = dev_url.origin().ascii_serialization().parse() {
        origins.push(origin);
    }
    // Normal mode already allows every origin for header-authenticated clients.
    // Preserve that access in dev mode; only this allowlist gets credentialed CORS.
    layer
        .allow_origin(AllowOrigin::mirror_request())
        .allow_credentials(AllowCredentials::predicate(move |origin, _| {
            origins.contains(origin)
        }))
}

async fn websocket(
    State(state): State<AppState>,
    connection: ConnectionInfo,
    headers: HeaderMap,
    uri: Uri,
    upgrade: WebSocketUpgrade,
) -> Response {
    let session_shutdown = state.shutdown.child_token();
    let reach = client_reach(connection, &headers);
    if state.config.unsafe_no_auth {
        return upgrade
            .protocols([CHUNKED_RPC_SUBPROTOCOL])
            .max_frame_size(MAX_PLAIN_WEBSOCKET_FRAME_BYTES)
            .max_message_size(MAX_PLAIN_WEBSOCKET_MESSAGE_BYTES)
            .on_upgrade(move |socket| {
                run_session(
                    socket,
                    state.rpc_registry,
                    RpcSessionContext::unauthenticated().with_reach(reach),
                    session_shutdown,
                )
            })
            .into_response();
    }
    match auth::authenticate_websocket(&state.auth, &headers, &uri).await {
        Ok(principal) => {
            let auth = state.auth.clone();
            let session_id = principal.session_id.clone();
            let expires_at_ms = principal.expires_at_ms;
            let rpc_context =
                RpcSessionContext::authenticated(principal, auth.clone()).with_reach(reach);
            upgrade
                .protocols([CHUNKED_RPC_SUBPROTOCOL])
                .max_frame_size(MAX_PLAIN_WEBSOCKET_FRAME_BYTES)
                .max_message_size(MAX_PLAIN_WEBSOCKET_MESSAGE_BYTES)
                .on_upgrade(move |socket| async move {
                    let Ok(connection_guard) = auth
                        .mark_connected_guard(&session_id, session_shutdown.clone())
                        .await
                    else {
                        session_shutdown.cancel();
                        drop(socket);
                        return;
                    };
                    let expiration_guard =
                        spawn_session_expiration_guard(expires_at_ms, session_shutdown.clone());
                    run_session(
                        socket,
                        state.rpc_registry,
                        rpc_context,
                        session_shutdown.clone(),
                    )
                    .await;
                    session_shutdown.cancel();
                    let _ = expiration_guard.await;
                    connection_guard.close().await;
                })
                .into_response()
        }
        Err(error) => auth::auth_error_response(error),
    }
}

async fn websocket_e2ee(
    State(state): State<AppState>,
    connection: ConnectionInfo,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let session_shutdown = state.shutdown.child_token();
    let socket_peer_ip = connection
        .as_ref()
        .ok()
        .map(|ConnectInfo(addrs)| addrs.peer.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let reach = client_reach(connection, &headers);
    let peer_ip = crate::rpc::effective_preauth_peer(
        socket_peer_ip,
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
    );
    let preauth_admission = state.e2ee_preauth_admission;
    upgrade
        .max_frame_size(MAX_E2EE_CIPHERTEXT_BYTES)
        .max_message_size(MAX_E2EE_CIPHERTEXT_BYTES)
        .on_upgrade(move |socket| {
            crate::rpc::run_e2ee_session(
                socket,
                peer_ip,
                reach,
                preauth_admission,
                state.auth,
                state.rpc_registry,
                state.config,
                session_shutdown,
            )
        })
        .into_response()
}

pub(crate) fn spawn_session_expiration_guard(
    expires_at_ms: i64,
    session_shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let remaining_ms = expires_at_ms.saturating_sub(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                .unwrap_or(i64::MAX),
        );
        tokio::select! {
            () = session_shutdown.cancelled() => {}
            () = tokio::time::sleep(std::time::Duration::from_millis(
                u64::try_from(remaining_ms.max(0)).unwrap_or_default(),
            )) => session_shutdown.cancel(),
        }
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentDescriptor {
    environment_id: String,
    label: String,
    platform: PlatformDescriptor,
    server_version: String,
    storage_instance_id: String,
    boot_id: Option<String>,
    remote_update_support: RemoteUpdateSupport,
    remote_protocol_version: u32,
    min_compatible_remote_protocol: u32,
    capabilities: EnvironmentCapabilities,
}

#[derive(Serialize)]
struct PlatformDescriptor {
    os: &'static str,
    arch: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentCapabilities {
    repository_identity: bool,
    remote_update_control: bool,
    remote_update_progress: bool,
    terminal_ordered_input: bool,
    terminal_size_ownership: bool,
}

async fn environment_descriptor(State(state): State<AppState>) -> Json<EnvironmentDescriptor> {
    let config = state.config;
    Json(EnvironmentDescriptor {
        environment_id: config.environment_id.clone(),
        label: config.environment_label.clone(),
        platform: PlatformDescriptor {
            os: platform_os(),
            arch: platform_arch(),
        },
        server_version: config.server_version.clone(),
        storage_instance_id: config
            .storage_instance_id
            .expect("a running server has a prepared persistent store")
            .to_string(),
        boot_id: config.boot_id.map(|id| id.to_string()),
        remote_update_support: config.remote_update_support,
        remote_protocol_version: REMOTE_PROTOCOL_VERSION,
        min_compatible_remote_protocol: MIN_COMPATIBLE_REMOTE_PROTOCOL,
        capabilities: EnvironmentCapabilities {
            repository_identity: true,
            remote_update_control: true,
            remote_update_progress: true,
            terminal_ordered_input: true,
            terminal_size_ownership: true,
        },
    })
}

async fn desktop_shutdown(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.config.mode != ServerMode::Desktop || state.config.desktop_bootstrap_token.is_none() {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    let supplied_token = headers
        .get(DESKTOP_SHUTDOWN_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if !token_matches(
        state.config.desktop_bootstrap_token.as_deref(),
        supplied_token,
    ) {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    state.shutdown.cancel();
    (
        StatusCode::ACCEPTED,
        [(CACHE_CONTROL, "no-store")],
        Json(json!({ "shuttingDown": true })),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOperationInput {
    operation_id: String,
}

fn authorized_update_maintenance(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<UpdateMaintenance>, StatusCode> {
    let Some(maintenance) = state.update_maintenance.clone() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let supplied_token = headers
        .get(DESKTOP_MAINTENANCE_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if !token_matches(
        state.config.desktop_bootstrap_token.as_deref(),
        supplied_token,
    ) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(maintenance)
}

async fn update_prepare(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let maintenance = match authorized_update_maintenance(&state, &headers) {
        Ok(maintenance) => maintenance,
        Err(status) => {
            return (status, status.canonical_reason().unwrap_or("Error")).into_response();
        }
    };
    match maintenance.prepare().await {
        Ok(result) => (StatusCode::OK, [(CACHE_CONTROL, "no-store")], Json(result)).into_response(),
        Err(error) => maintenance_error_response(error),
    }
}

async fn update_commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<UpdateOperationInput>,
) -> Response {
    let maintenance = match authorized_update_maintenance(&state, &headers) {
        Ok(maintenance) => maintenance,
        Err(status) => {
            return (status, status.canonical_reason().unwrap_or("Error")).into_response();
        }
    };
    let operation_id = match uuid::Uuid::parse_str(&input.operation_id) {
        Ok(operation_id) => operation_id,
        Err(_) => return maintenance_error_response(MaintenanceError::OperationMismatch),
    };
    match maintenance.commit(operation_id).await {
        Ok(()) => {
            maintenance.shutdown_after_response();
            (
                StatusCode::OK,
                [(CACHE_CONTROL, "no-store")],
                Json(json!({"committed":true})),
            )
                .into_response()
        }
        Err(error) => maintenance_error_response(error),
    }
}

async fn update_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<UpdateOperationInput>,
) -> Response {
    let maintenance = match authorized_update_maintenance(&state, &headers) {
        Ok(maintenance) => maintenance,
        Err(status) => {
            return (status, status.canonical_reason().unwrap_or("Error")).into_response();
        }
    };
    let operation_id = match uuid::Uuid::parse_str(&input.operation_id) {
        Ok(operation_id) => operation_id,
        Err(_) => return maintenance_error_response(MaintenanceError::OperationMismatch),
    };
    match maintenance.cancel(operation_id).await {
        Ok(()) => {
            maintenance.shutdown_after_response();
            (
                StatusCode::OK,
                [(CACHE_CONTROL, "no-store")],
                Json(json!({"cancelled":true})),
            )
                .into_response()
        }
        Err(error) => maintenance_error_response(error),
    }
}

async fn update_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let maintenance = match authorized_update_maintenance(&state, &headers) {
        Ok(maintenance) => maintenance,
        Err(status) => {
            return (status, status.canonical_reason().unwrap_or("Error")).into_response();
        }
    };
    (
        StatusCode::OK,
        [(CACHE_CONTROL, "no-store")],
        Json(maintenance.status().await),
    )
        .into_response()
}

fn maintenance_error_response(error: MaintenanceError) -> Response {
    let status = match error {
        MaintenanceError::OperationMismatch | MaintenanceError::NoPreparedOperation => {
            StatusCode::CONFLICT
        }
        MaintenanceError::AdmissionClosed
        | MaintenanceError::DrainTimeout { .. }
        | MaintenanceError::Preparation(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    (
        status,
        [(CACHE_CONTROL, "no-store")],
        Json(json!({
            "_tag":"UpdateMaintenanceError",
            "message":error.to_string(),
        })),
    )
        .into_response()
}

fn token_matches(expected: Option<&str>, supplied: Option<&str>) -> bool {
    let (Some(expected), Some(supplied)) = (expected, supplied) else {
        return false;
    };
    expected.len() == supplied.len() && bool::from(expected.as_bytes().ct_eq(supplied.as_bytes()))
}

async fn static_or_dev(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }

    if let Some(dev_url) = &state.config.dev_url
        && request_is_loopback(&headers)
    {
        let mut redirect = dev_url.clone();
        redirect.set_path(uri.path());
        redirect.set_query(uri.query());
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(LOCATION, redirect.as_str())
            .body(Body::empty())
            .unwrap_or_else(|_| internal_server_error());
    }

    let Some(static_dir) = &state.config.static_dir else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "No static directory configured and no dev URL set.",
        )
            .into_response();
    };
    serve_static(static_dir, uri.path(), &method, &headers).await
}

async fn serve_static(
    static_dir: &Path,
    request_path: &str,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    let relative = match safe_relative_path(request_path) {
        Ok(path) => path,
        Err(()) => return (StatusCode::BAD_REQUEST, "Invalid static file path").into_response(),
    };
    let root = match tokio::fs::canonicalize(static_dir).await {
        Ok(path) => path,
        Err(_) => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
    };

    let mut candidate = root.join(relative);
    if candidate.extension().is_none() {
        candidate.push("index.html");
    }
    let candidate = match canonical_file_within(&root, &candidate).await {
        Some(path) => path,
        None => match canonical_file_within(&root, &root.join("index.html")).await {
            Some(path) => path,
            None => return (StatusCode::NOT_FOUND, "Not Found").into_response(),
        },
    };
    let content_hashed = candidate
        .strip_prefix(&root)
        .is_ok_and(is_content_hashed_asset);
    stream_file(candidate, content_hashed, method, headers).await
}

fn safe_relative_path(request_path: &str) -> Result<PathBuf, ()> {
    let decoded = percent_decode_str(request_path)
        .decode_utf8()
        .map_err(|_| ())?;
    let normalized = decoded.replace('\\', "/");
    let relative = normalized.trim_start_matches('/');
    if relative.contains('\0') || relative.starts_with("..") {
        return Err(());
    }

    let path = if relative.is_empty() {
        Path::new("index.html")
    } else {
        Path::new(relative)
    };
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(());
    }
    Ok(path.to_path_buf())
}

async fn canonical_file_within(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let canonical = tokio::fs::canonicalize(candidate).await.ok()?;
    if !canonical.starts_with(root) {
        return None;
    }
    let metadata = tokio::fs::metadata(&canonical).await.ok()?;
    metadata.is_file().then_some(canonical)
}

/// Vite emits build outputs into `assetsDir` (`assets/`) as `[name]-[hash].[ext]`
/// with an 8-character base64url hash; `apps/web/public/` files are copied to the
/// static root unhashed. The input is the resolved file's path relative to that root.
fn is_content_hashed_asset(relative: &Path) -> bool {
    let mut components = relative.components();
    let (Some(Component::Normal(directory)), Some(Component::Normal(filename)), None) =
        (components.next(), components.next(), components.next())
    else {
        return false;
    };
    if directory != "assets" {
        return false;
    }
    let filename = Path::new(filename);
    if filename
        .extension()
        .is_none_or(|extension| extension.is_empty())
    {
        return false;
    }
    let Some(stem) = filename.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let stem = stem.as_bytes();
    let Some(hash_start) = stem.len().checked_sub(8) else {
        return false;
    };
    hash_start > 0
        && stem[hash_start - 1] == b'-'
        && stem[hash_start..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn if_none_match_matches(header: &str, etag: &str) -> bool {
    let header = header.trim();
    let etag = etag.strip_prefix("W/").unwrap_or(etag);
    header == "*"
        || header.split(',').any(|entry| {
            let entry = entry.trim();
            entry.strip_prefix("W/").unwrap_or(entry) == etag
        })
}

/// The UI's policy. Browser mode frames preview gateways on the host the page was
/// loaded from, one port per target, so `frame-src` allows that host on any port.
/// An IPv6 host gets none: the gateway lets no IPv6 page frame it.
fn content_security_policy(headers: &HeaderMap) -> String {
    let gateways = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<hyper::http::uri::Authority>().ok())
        .map(|authority| authority.host().to_owned())
        // Only name and IPv4 characters reach the policy (never `;`, `,`, or `[`).
        .filter(|host| {
            host.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
        })
        .map(|host| format!(" http://{host}:*"))
        .unwrap_or_default();
    format!("{CONTENT_SECURITY_POLICY_VALUE}; frame-src 'self'{gateways}")
}

async fn stream_file(
    path: PathBuf,
    content_hashed: bool,
    method: &Method,
    headers: &HeaderMap,
) -> Response {
    let file = match File::open(&path).await {
        Ok(file) => file,
        Err(_) => return internal_server_error(),
    };
    let metadata = match file.metadata().await {
        Ok(metadata) => metadata,
        Err(_) => return internal_server_error(),
    };
    let content_type = mime_guess::from_path(&path).first_or_octet_stream();
    let (cache_control, etag) = if content_type.type_() == mime_guess::mime::TEXT
        && content_type.subtype() == mime_guess::mime::HTML
    {
        (HTML_CACHE_CONTROL, None)
    } else if content_hashed {
        (IMMUTABLE_CACHE_CONTROL, None)
    } else {
        let etag = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|modified| {
                format!(
                    "W/\"{:x}-{:x}-{:x}\"",
                    metadata.len(),
                    modified.as_secs(),
                    modified.subsec_nanos()
                )
            });
        ("no-cache", etag)
    };
    let not_modified = etag.as_deref().is_some_and(|etag| {
        headers.get_all(IF_NONE_MATCH).iter().any(|value| {
            value
                .to_str()
                .is_ok_and(|value| if_none_match_matches(value, etag))
        })
    });

    let mut response = Response::builder()
        .status(if not_modified {
            StatusCode::NOT_MODIFIED
        } else {
            StatusCode::OK
        })
        .header(CONTENT_TYPE, content_type.as_ref())
        .header(CONTENT_LENGTH, metadata.len())
        .header(CACHE_CONTROL, cache_control)
        .header("x-content-type-options", "nosniff")
        .header("content-security-policy", content_security_policy(headers));
    if let Some(etag) = etag {
        response = response.header(ETAG, etag);
    }
    let body = if not_modified || method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(ReaderStream::new(file))
    };
    response
        .body(body)
        .unwrap_or_else(|_| internal_server_error())
}

fn request_is_loopback(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    if host == "localhost" || host.starts_with("localhost:") {
        return true;
    }
    let without_port = host
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(address, _)| address))
        .or_else(|| host.split_once(':').map(|(address, _)| address))
        .unwrap_or(&host);
    without_port.parse::<IpAddr>().is_ok_and(|address| {
        address == IpAddr::V4(Ipv4Addr::LOCALHOST) || address == IpAddr::V6(Ipv6Addr::LOCALHOST)
    })
}

fn platform_os() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "darwin",
        "linux" => "linux",
        _ => "unknown",
    }
}

fn platform_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => "other",
    }
}

fn internal_server_error() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}

#[cfg(test)]
mod tests {
    use tower::ServiceExt;

    #[test]
    fn the_ui_may_frame_only_its_own_hosts_preview_gateways() {
        let csp = |host: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(HOST, HeaderValue::from_str(host).unwrap());
            super::content_security_policy(&headers)
        };
        assert!(csp("box.lan:3773").ends_with("; frame-src 'self' http://box.lan:*"));
        // The gateway frames no IPv6 host, and a Host that isn't an authority adds nothing.
        for host in [
            "[::1]:3773",
            "box.lan:3773; script-src *",
            "a b",
            "box.lan;x:3773",
        ] {
            assert!(csp(host).ends_with("; frame-src 'self'"), "{host}");
        }
        assert!(super::content_security_policy(&HeaderMap::new()).ends_with("; frame-src 'self'"));
    }

    use super::*;

    #[test]
    fn forwarded_host_is_trusted_only_from_a_loopback_peer() {
        let forwarded = || {
            let mut headers = HeaderMap::new();
            headers.insert(HOST, HeaderValue::from_static("127.0.0.1:3773"));
            headers.insert(
                X_FORWARDED_HOST,
                HeaderValue::from_static("bibcode.example.test, inner.proxy"),
            );
            headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
            headers
        };
        for loopback in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            let mut headers = forwarded();
            apply_forwarded_headers(&mut headers, Some(loopback.parse().unwrap()));
            assert_eq!(headers[HOST], "bibcode.example.test", "{loopback}");
            assert_eq!(headers[X_FORWARDED_PROTO], "https", "{loopback}");
        }
        for untrusted in [Some("192.168.1.9".parse().unwrap()), None] {
            let mut headers = forwarded();
            apply_forwarded_headers(&mut headers, untrusted);
            assert_eq!(headers[HOST], "127.0.0.1:3773", "{untrusted:?}");
            assert!(!headers.contains_key(X_FORWARDED_HOST), "{untrusted:?}");
            assert!(!headers.contains_key(X_FORWARDED_PROTO), "{untrusted:?}");
        }
    }

    #[test]
    fn content_hashed_assets_follow_the_vite_output_convention() {
        for (path, expected) in [
            ("assets/index-C0420DUf.js", true),
            ("assets/actionscript-3--17pq3dv.js", true),
            ("assets/react-DV3x_TFi.js", true),
            ("assets/style-AbCd12_-.css", true),
            ("theme-bootstrap.js", false),
            ("favicon.ico", false),
            ("assets/logo.png", false),
            ("nested/assets/index-C0420DUf.js", false),
            ("assets/nested/index-C0420DUf.js", false),
            ("assets/index-C0420DU.js", false),
            ("assets/index-C0420DUff.js", false),
            ("assets/index-C0420DUf", false),
            ("assets/index-C0420DUf.", false),
            ("assets/index-C0420D+f.js", false),
            ("assets/index-C0420Déf.js", false),
            ("/assets/index-C0420DUf.js", false),
            ("index-C0420DUf.js", false),
        ] {
            assert_eq!(is_content_hashed_asset(Path::new(path)), expected, "{path}");
        }
    }

    #[test]
    fn if_none_match_uses_weak_comparison() {
        for (header, etag, expected) in [
            ("\"abc\"", "\"abc\"", true),
            ("W/\"abc\"", "\"abc\"", true),
            ("\"abc\"", "W/\"abc\"", true),
            ("W/\"abc\"", "W/\"abc\"", true),
            (" \"other\", W/\"abc\", \"last\" ", "W/\"abc\"", true),
            ("*", "W/\"abc\"", true),
            (" \t* \t", "W/\"abc\"", true),
            ("\"other\"", "W/\"abc\"", false),
            ("\"ABC\"", "W/\"abc\"", false),
            ("", "W/\"abc\"", false),
            (" \t", "W/\"abc\"", false),
            ("abc", "W/\"abc\"", false),
            ("\"other,*,value\"", "W/\"abc\"", false),
        ] {
            assert_eq!(if_none_match_matches(header, etag), expected, "{header:?}");
        }
    }

    async fn cors_response(
        config: &ServerConfig,
        method: Method,
        origin: Option<&str>,
    ) -> Response {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::OK }))
            .layer(cors_layer(config));
        let mut request = Request::builder().uri("/").method(method.clone());
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        if method == Method::OPTIONS {
            request = request
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "authorization");
        }
        app.oneshot(request.body(Body::empty()).expect("CORS request"))
            .await
            .expect("CORS response")
    }

    #[tokio::test]
    async fn cors_normal_mode_preserves_response_headers() {
        let config = ServerConfig::new("unused-cors-test-root");
        for (method, expected) in [
            (Method::GET, vec![("access-control-allow-origin", "*")]),
            (
                Method::OPTIONS,
                vec![
                    (
                        "access-control-allow-headers",
                        "authorization,content-type,b3,traceparent,dpop,idempotency-key,x-bibcode-desktop-bootstrap-token",
                    ),
                    ("access-control-allow-methods", "GET,POST,DELETE,OPTIONS"),
                    ("access-control-allow-origin", "*"),
                    ("access-control-max-age", "600"),
                ],
            ),
        ] {
            let response =
                cors_response(&config, method.clone(), Some("https://client.example.test")).await;
            assert_eq!(response.status(), StatusCode::OK);
            let mut actual = response
                .headers()
                .iter()
                .filter(|(name, _)| {
                    name.as_str().starts_with("access-control-") || name.as_str() == "vary"
                })
                .map(|(name, value)| (name.as_str(), value.to_str().expect("CORS header value")))
                .collect::<Vec<_>>();
            actual.sort_unstable();
            assert_eq!(actual, expected, "{method}");
        }
    }

    async fn assert_dev_cors_origin(method: Method, origin: &str, credentials: bool) {
        let config = ServerConfig::new("unused-cors-test-root")
            .with_dev_url("http://localhost:5733".parse().expect("dev URL"));
        let response = cors_response(&config, method.clone(), Some(origin)).await;
        let headers = response.headers();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            headers.get("access-control-allow-origin"),
            Some(&origin.parse::<axum::http::HeaderValue>().expect("origin")),
            "{method} from {origin}"
        );
        assert_eq!(
            headers
                .get("access-control-allow-credentials")
                .map(|value| value.to_str().expect("credentials header")),
            credentials.then_some("true"),
            "{method} from {origin}"
        );
        assert!(
            headers.get_all("vary").iter().any(|value| {
                value
                    .to_str()
                    .expect("Vary header")
                    .split(',')
                    .any(|name| name.trim().eq_ignore_ascii_case("origin"))
            }),
            "{method} from {origin} must vary by origin"
        );
    }

    #[tokio::test]
    async fn cors_dev_mode_reflects_other_origins_without_credentials_on_get() {
        for origin in [
            "https://random.example.test",
            "http://127.0.0.1:65000",
            "http://localhost:5734",
            "bibcode://other",
            // The desktop app sends a bearer token, and cookies from its origins are refused.
            "bibcode://app",
            "bibcode-dev://app",
            "null",
        ] {
            assert_dev_cors_origin(Method::GET, origin, false).await;
        }
    }

    #[tokio::test]
    async fn cors_dev_mode_reflects_other_origins_without_credentials_on_preflight() {
        for origin in [
            "https://random.example.test",
            "http://127.0.0.1:65000",
            "http://localhost:5734",
            "bibcode://other",
            // The desktop app sends a bearer token, and cookies from its origins are refused.
            "bibcode://app",
            "bibcode-dev://app",
            "null",
        ] {
            assert_dev_cors_origin(Method::OPTIONS, origin, false).await;
        }
    }

    #[tokio::test]
    async fn cors_dev_mode_preserves_credentials_for_the_dev_origin() {
        for method in [Method::GET, Method::OPTIONS] {
            assert_dev_cors_origin(method, "http://localhost:5733", true).await;
        }
    }

    #[tokio::test]
    async fn cors_dev_mode_omits_allow_origin_without_an_origin_header() {
        let config = ServerConfig::new("unused-cors-test-root")
            .with_dev_url("http://localhost:5733".parse().expect("dev URL"));
        for method in [Method::GET, Method::OPTIONS] {
            let response = cors_response(&config, method, None).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                !response
                    .headers()
                    .contains_key("access-control-allow-origin")
            );
        }
    }

    #[test]
    fn route_helpers_preserve_runtime_methods_and_internal_error_status() {
        assert_eq!(RouteMethod::Delete.as_str(), "DELETE");
        assert_eq!(RouteMethod::Get.as_str(), "GET");
        assert_eq!(RouteMethod::Post.as_str(), "POST");
        assert_eq!(
            route(RouteMethod::Delete, "/runtime"),
            RouteSpec {
                method: "DELETE",
                path: "/runtime",
            }
        );
        assert_eq!(
            internal_server_error().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
