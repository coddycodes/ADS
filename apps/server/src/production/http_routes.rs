use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{FromRef, Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub const MAX_JSON_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_DIAGNOSTIC_BODY_BYTES: usize = 512 * 1024;
pub const MAX_MCP_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_OPEN_URL_BODY_BYTES: usize = 16 * 1024;

/// The plain H.264 media socket (spec §4.2); it authenticates with a ticket, never a cookie.
pub const DEVICE_MEDIA_PATH: &str = "/api/device-media";
/// The same socket inside a Noise_NK channel, for profiles that pin the host key.
pub const DEVICE_MEDIA_E2EE_PATH: &str = "/api/device-media-e2ee";

const NO_STORE: &str = "no-store";
const OPERATE_SCOPE: &str = "orchestration:operate";

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The scopes of the one credential an authorized request presented. A route that learns
/// from its body that it needs more checks them here instead of authenticating again: a DPoP
/// proof is single-use, so a second check of the same request is refused as a replay.
#[derive(Clone, Debug, Default)]
pub struct Authorized {
    pub scopes: Vec<String>,
}

impl Authorized {
    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|granted| granted == scope)
    }
}

pub type AuthorizeHandler = Arc<
    dyn Fn(
            HeaderMap,
            Method,
            Uri,
            Option<&'static str>,
            CancellationToken,
        ) -> BoxFuture<Result<Authorized, Response>>
        + Send
        + Sync,
>;
pub type JsonHandler = Arc<
    dyn Fn(
            JsonOperation,
            Option<Value>,
            RouteContext,
        ) -> BoxFuture<Result<JsonRouteResponse, HttpRouteError>>
        + Send
        + Sync,
>;
pub type AssetHandler = Arc<
    dyn Fn(String, String, RouteContext) -> BoxFuture<Result<AssetHttpResponse, HttpRouteError>>
        + Send
        + Sync,
>;
/// Takes the frontend log snapshot and whether the bundle may include crash dumps.
pub type DiagnosticLogsHandler = Arc<
    dyn Fn(
            String,
            bool,
            RouteContext,
        ) -> BoxFuture<Result<DiagnosticLogsHttpResponse, HttpRouteError>>
        + Send
        + Sync,
>;
pub type McpHandler = Arc<
    dyn Fn(Method, Vec<u8>, RouteContext) -> BoxFuture<Result<McpHttpResponse, HttpRouteError>>
        + Send
        + Sync,
>;
/// `(body, context)` for `POST /api/preview/open-url`. The handler authenticates the request
/// itself: the route accepts only an open-url bearer token, never a session.
pub type OpenUrlHandler = Arc<
    dyn Fn(Vec<u8>, RouteContext) -> BoxFuture<Result<JsonRouteResponse, HttpRouteError>>
        + Send
        + Sync,
>;
pub type TransferDownloadHandler = Arc<
    dyn Fn(String, RouteContext) -> BoxFuture<Result<TransferDownloadHttpOutcome, HttpRouteError>>
        + Send
        + Sync,
>;
/// `(token, requested_name, overwrite, body, context)`. The upload's file name lives in the
/// token, so `requested_name` is only the `?name=` a client still sent: the handler refuses one
/// that disagrees with the token rather than honouring it.
pub type TransferUploadHandler = Arc<
    dyn Fn(
            String,
            Option<String>,
            bool,
            Body,
            RouteContext,
        ) -> BoxFuture<Result<TransferUploadHttpOutcome, HttpRouteError>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct HttpRoutesState {
    pub authorize: AuthorizeHandler,
    pub json: JsonHandler,
    pub diagnostic_logs: DiagnosticLogsHandler,
    pub assets: AssetHandler,
    pub mcp: McpHandler,
    pub transfer_download: TransferDownloadHandler,
    pub transfer_upload: TransferUploadHandler,
    /// Answers `503` until the production runtime replaces it.
    pub open_url: OpenUrlHandler,
    /// The media socket's devices and sessions; `None` (answering `503`) without the
    /// production runtime.
    pub(crate) device_media: Option<crate::device::media_socket::MediaSocketState>,
}

impl HttpRoutesState {
    #[must_use]
    pub fn new(
        authorize: AuthorizeHandler,
        json: JsonHandler,
        diagnostic_logs: DiagnosticLogsHandler,
        assets: AssetHandler,
        mcp: McpHandler,
        transfer_download: TransferDownloadHandler,
        transfer_upload: TransferUploadHandler,
    ) -> Self {
        Self {
            authorize,
            json,
            diagnostic_logs,
            assets,
            mcp,
            transfer_download,
            transfer_upload,
            device_media: None,
            open_url: Arc::new(|_body, _context| {
                Box::pin(async {
                    Err(HttpRouteError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({
                            "_tag": "NativeRuntimeUnavailableError",
                            "message": "The native production runtime is unavailable."
                        }),
                    ))
                })
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonOperation {
    OrchestrationSnapshot,
    OrchestrationDispatch,
    ConnectLinkProof,
    ConnectRelayConfig,
    ConnectLinkState,
    ConnectUnlink,
    ConnectHealth,
    ConnectMintCredential,
}

impl JsonOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OrchestrationSnapshot => "orchestration.snapshot",
            Self::OrchestrationDispatch => "orchestration.dispatch",
            Self::ConnectLinkProof => "connect.linkProof",
            Self::ConnectRelayConfig => "connect.relayConfig",
            Self::ConnectLinkState => "connect.linkState",
            Self::ConnectUnlink => "connect.unlink",
            Self::ConnectHealth => "connect.health",
            Self::ConnectMintCredential => "connect.mintCredential",
        }
    }
}

#[derive(Clone)]
pub struct RouteContext {
    pub headers: HeaderMap,
    pub uri: Uri,
    pub cancellation: CancellationToken,
}

pub struct JsonRouteResponse {
    pub status: StatusCode,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

impl JsonRouteResponse {
    #[must_use]
    pub fn ok(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            headers: BTreeMap::new(),
            body,
        }
    }
}

pub struct AssetHttpResponse {
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub cache_control: String,
}

pub struct DiagnosticLogsHttpResponse {
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// A download response whose body is streamed: a file's bytes, or a folder's zip archive.
///
/// `content_length` is `Some` only when the whole length is known before the first byte goes
/// out -- a file on disk. A zip is produced as it streams, so it stays chunked with no length.
pub struct TransferDownloadHttpResponse {
    pub file_name: String,
    pub content_type: &'static str,
    pub content_length: Option<u64>,
    pub body: Body,
}

/// Which archive budget a folder download blew through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferArchiveLimitUnit {
    Entries,
    Bytes,
}

impl TransferArchiveLimitUnit {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Entries => "entries",
            Self::Bytes => "bytes",
        }
    }
}

/// The outcomes a download route reports back to the client.
///
/// An oversized folder is an outcome rather than an error so that the route, not the handler,
/// stays responsible for HTTP framing -- the same split the upload outcome already uses.
pub enum TransferDownloadHttpOutcome {
    Stream(TransferDownloadHttpResponse),
    ArchiveTooLarge {
        limit: u64,
        unit: TransferArchiveLimitUnit,
    },
}

/// The three outcomes an upload route reports back to the client.
pub enum TransferUploadHttpOutcome {
    Created { relative_path: String },
    Exists,
    TooLarge { limit: u64 },
}

pub struct McpHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct HttpRouteError {
    status: StatusCode,
    body: Value,
    headers: BTreeMap<String, String>,
}

impl HttpRouteError {
    #[must_use]
    pub fn new(status: StatusCode, body: Value) -> Self {
        Self {
            status,
            body,
            headers: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }

    fn into_response(self) -> Response {
        json_response(self.status, self.headers, self.body)
    }
}

pub fn add_routes<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    HttpRoutesState: FromRef<S>,
{
    router
        .route("/api/orchestration/snapshot", get(orchestration_snapshot))
        .route("/api/orchestration/dispatch", post(orchestration_dispatch))
        .route("/api/connect/link-proof", post(connect_link_proof))
        .route("/api/connect/relay-config", post(connect_relay_config))
        .route("/api/connect/link-state", get(connect_link_state))
        .route("/api/connect/unlink", post(connect_unlink))
        .route("/api/bibcode-connect/health", post(connect_health))
        .route(
            "/api/connect/mint-credential",
            post(connect_mint_credential),
        )
        .route(
            "/api/bibcode-connect/mint-credential",
            post(connect_mint_credential),
        )
        .route("/api/diagnostics/logs.zip", post(diagnostic_logs))
        .route("/api/assets/{token}/{*path}", get(asset))
        .route(
            "/api/transfers/{token}",
            get(transfer_download).post(transfer_upload),
        )
        .route("/api/preview/open-url", post(preview_open_url))
        .route(DEVICE_MEDIA_PATH, get(device_media))
        .route(DEVICE_MEDIA_E2EE_PATH, get(device_media_e2ee))
        .route("/mcp", post(mcp_post).delete(mcp_delete))
}

async fn device_media(
    State(state): State<HttpRoutesState>,
    connection: crate::http::ConnectionInfo,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    device_media_upgrade(
        state,
        connection,
        &headers,
        upgrade,
        crate::device::media_socket::upgrade_plain,
    )
}

async fn device_media_e2ee(
    State(state): State<HttpRoutesState>,
    connection: crate::http::ConnectionInfo,
    headers: HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    device_media_upgrade(
        state,
        connection,
        &headers,
        upgrade,
        crate::device::media_socket::upgrade_e2ee,
    )
}

fn device_media_upgrade(
    state: HttpRoutesState,
    connection: crate::http::ConnectionInfo,
    headers: &HeaderMap,
    upgrade: axum::extract::ws::WebSocketUpgrade,
    upgrade_route: fn(
        crate::device::media_socket::MediaSocketState,
        std::net::IpAddr,
        axum::extract::ws::WebSocketUpgrade,
    ) -> Response,
) -> Response {
    // The same peer `/ws-e2ee` admits: a loopback forwarder's last `X-Forwarded-For` hop.
    let socket_peer = connection.map_or(
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        |axum::extract::ConnectInfo(addrs)| addrs.peer.ip(),
    );
    let peer_ip = crate::rpc::effective_preauth_peer(
        socket_peer,
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
    );
    match state.device_media {
        Some(media) => upgrade_route(media, peer_ip, upgrade),
        None => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            BTreeMap::new(),
            json!({
                "_tag": "NativeRuntimeUnavailableError",
                "message": "The native production runtime is unavailable."
            }),
        ),
    }
}

async fn orchestration_snapshot(
    State(state): State<HttpRoutesState>,
    request: Request,
) -> Response {
    json_request(
        state,
        request,
        JsonOperation::OrchestrationSnapshot,
        Some("orchestration:read"),
        BodyKind::None,
    )
    .await
}

async fn orchestration_dispatch(
    State(state): State<HttpRoutesState>,
    request: Request,
) -> Response {
    json_request(
        state,
        request,
        JsonOperation::OrchestrationDispatch,
        Some("orchestration:operate"),
        BodyKind::Json(MAX_JSON_BODY_BYTES),
    )
    .await
}

async fn connect_link_proof(State(state): State<HttpRoutesState>, request: Request) -> Response {
    json_request(
        state,
        request,
        JsonOperation::ConnectLinkProof,
        Some("relay:write"),
        BodyKind::Json(MAX_JSON_BODY_BYTES),
    )
    .await
}

async fn connect_relay_config(State(state): State<HttpRoutesState>, request: Request) -> Response {
    json_request(
        state,
        request,
        JsonOperation::ConnectRelayConfig,
        Some("relay:write"),
        BodyKind::Json(MAX_JSON_BODY_BYTES),
    )
    .await
}

async fn connect_link_state(State(state): State<HttpRoutesState>, request: Request) -> Response {
    json_request(
        state,
        request,
        JsonOperation::ConnectLinkState,
        Some("relay:read"),
        BodyKind::None,
    )
    .await
}

async fn connect_unlink(State(state): State<HttpRoutesState>, request: Request) -> Response {
    json_request(
        state,
        request,
        JsonOperation::ConnectUnlink,
        Some("relay:write"),
        BodyKind::None,
    )
    .await
}

async fn connect_health(State(state): State<HttpRoutesState>, request: Request) -> Response {
    json_request(
        state,
        request,
        JsonOperation::ConnectHealth,
        None,
        BodyKind::Json(MAX_JSON_BODY_BYTES),
    )
    .await
}

async fn connect_mint_credential(
    State(state): State<HttpRoutesState>,
    request: Request,
) -> Response {
    json_request(
        state,
        request,
        JsonOperation::ConnectMintCredential,
        None,
        BodyKind::Json(MAX_JSON_BODY_BYTES),
    )
    .await
}

async fn diagnostic_logs(State(state): State<HttpRoutesState>, request: Request) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, body) = request.into_parts();
    let authorized = match (state.authorize)(
        parts.headers.clone(),
        parts.method.clone(),
        parts.uri.clone(),
        Some("orchestration:read"),
        cancellation.clone(),
    )
    .await
    {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let payload = match bounded_json(body, MAX_DIAGNOSTIC_BODY_BYTES).await {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let Some(frontend_log) = payload.get("frontendLog").and_then(Value::as_str) else {
        return bad_request("Request body must contain a frontendLog string.");
    };
    // Crash dumps hold fragments of memory, so they come only from an explicit `true`.
    let include_crash_dumps = match payload.get("includeCrashDumps") {
        None => false,
        Some(Value::Bool(include)) => *include,
        Some(_) => return bad_request("includeCrashDumps must be a boolean."),
    };
    // Dumps are raw process memory, unlike the redacted logs `orchestration:read` covers,
    // so asking for them takes the same scope as running commands, of the same credential.
    if include_crash_dumps && !authorized.has_scope(OPERATE_SCOPE) {
        return crate::auth::auth_error_response(crate::auth::AuthError::ScopeRequired(
            OPERATE_SCOPE.to_owned(),
        ));
    }
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.diagnostic_logs)(frontend_log.to_owned(), include_crash_dumps, context).await {
        Ok(response) => diagnostic_logs_response(response),
        Err(error) => error.into_response(),
    }
}

enum BodyKind {
    None,
    Json(usize),
}

async fn json_request(
    state: HttpRoutesState,
    request: Request,
    operation: JsonOperation,
    required_scope: Option<&'static str>,
    body_kind: BodyKind,
) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, body) = request.into_parts();
    if let Err(response) = (state.authorize)(
        parts.headers.clone(),
        parts.method.clone(),
        parts.uri.clone(),
        required_scope,
        cancellation.clone(),
    )
    .await
    {
        return response;
    }
    let payload = match body_kind {
        BodyKind::None => None,
        BodyKind::Json(limit) => match bounded_json(body, limit).await {
            Ok(payload) => Some(payload),
            Err(response) => return response,
        },
    };
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.json)(operation, payload, context).await {
        Ok(response) => json_response(response.status, response.headers, response.body),
        Err(error) => error.into_response(),
    }
}

/// Agent-written HTML/SVG is served from the BiBCode origin; an opaque-origin
/// sandbox keeps its scripts away from the session cookie and `/api`.
fn asset_content_security_policy(content_type: &str) -> Option<&'static str> {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "text/html" | "application/xhtml+xml" => {
            Some("sandbox allow-scripts allow-forms allow-popups")
        }
        // XML documents (any `+xml` type: RSS, Atom, MathML, ...) can run scripts
        // through XSLT or embedded XHTML, like SVG.
        "image/svg+xml" | "text/xml" | "application/xml" => {
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox")
        }
        essence if essence.ends_with("+xml") => {
            Some("default-src 'none'; style-src 'unsafe-inline'; sandbox")
        }
        _ => None,
    }
}

async fn asset(
    State(state): State<HttpRoutesState>,
    Path((token, path)): Path<(String, String)>,
    request: Request,
) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, _) = request.into_parts();
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.assets)(token, path, context).await {
        Ok(asset) => {
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, &asset.content_type)
                .header(header::CACHE_CONTROL, asset.cache_control)
                .header("x-content-type-options", "nosniff");
            if let Some(policy) = asset_content_security_policy(&asset.content_type) {
                builder = builder.header(header::CONTENT_SECURITY_POLICY, policy);
            }
            builder
                .body(Body::from(asset.bytes))
                .unwrap_or_else(|_| internal_error())
        }
        Err(error) => error.into_response(),
    }
}

async fn transfer_download(
    State(state): State<HttpRoutesState>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, _) = request.into_parts();
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.transfer_download)(token, context).await {
        Ok(TransferDownloadHttpOutcome::ArchiveTooLarge { limit, unit }) => json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            BTreeMap::new(),
            json!({
                "_tag": "TransferArchiveTooLargeError",
                "limit": limit,
                "unit": unit.as_str(),
                "message": "Folder is too large to download as an archive."
            }),
        ),
        Ok(TransferDownloadHttpOutcome::Stream(download)) => {
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, download.content_type)
                .header(
                    header::CONTENT_DISPOSITION,
                    attachment_disposition(&download.file_name),
                )
                .header(header::CACHE_CONTROL, NO_STORE)
                .header("x-content-type-options", "nosniff");
            if let Some(length) = download.content_length {
                builder = builder.header(header::CONTENT_LENGTH, length);
            }
            builder
                .body(download.body)
                .unwrap_or_else(|_| internal_error())
        }
        Err(error) => error.into_response(),
    }
}

async fn transfer_upload(
    State(state): State<HttpRoutesState>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, body) = request.into_parts();
    let query: BTreeMap<String, String> = parts
        .uri
        .query()
        .map(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    // The token names the file; `?name=` survives only so a stale client that still sends it is
    // refused loudly instead of silently writing somewhere it did not intend.
    let requested_name = query.get("name").cloned();
    let overwrite = query
        .get("overwrite")
        .is_some_and(|value| value == "1" || value == "true");
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.transfer_upload)(token, requested_name, overwrite, body, context).await {
        Ok(TransferUploadHttpOutcome::Created { relative_path }) => json_response(
            StatusCode::CREATED,
            BTreeMap::new(),
            json!({ "relativePath": relative_path }),
        ),
        Ok(TransferUploadHttpOutcome::Exists) => json_response(
            StatusCode::CONFLICT,
            BTreeMap::new(),
            json!({
                "_tag": "TransferEntryExistsError",
                "message": "An entry with that name already exists."
            }),
        ),
        Ok(TransferUploadHttpOutcome::TooLarge { limit }) => json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            BTreeMap::new(),
            json!({
                "_tag": "TransferTooLargeError",
                "message": "Upload exceeds the configured limit.",
                "limit": limit
            }),
        ),
        Err(error) => error.into_response(),
    }
}

/// Bearer-only: deliberately skips `state.authorize`, which would also accept a session cookie.
async fn preview_open_url(State(state): State<HttpRoutesState>, request: Request) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_OPEN_URL_BODY_BYTES).await {
        Ok(body) => body.to_vec(),
        Err(_) => return payload_too_large(),
    };
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.open_url)(body, context).await {
        Ok(response) => json_response(response.status, response.headers, response.body),
        Err(error) => error.into_response(),
    }
}

async fn mcp_post(State(state): State<HttpRoutesState>, request: Request) -> Response {
    mcp_request(state, Method::POST, request).await
}

async fn mcp_delete(State(state): State<HttpRoutesState>, request: Request) -> Response {
    mcp_request(state, Method::DELETE, request).await
}

async fn mcp_request(state: HttpRoutesState, method: Method, request: Request) -> Response {
    let cancellation = CancellationToken::new();
    let _guard = CancellationGuard(cancellation.clone());
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_MCP_BODY_BYTES).await {
        Ok(body) => body.to_vec(),
        Err(_) => return payload_too_large(),
    };
    let context = RouteContext {
        headers: parts.headers,
        uri: parts.uri,
        cancellation,
    };
    match (state.mcp)(method, body, context).await {
        Ok(response) => mcp_response(response),
        Err(error) => error.into_response(),
    }
}

#[allow(clippy::result_large_err)]
async fn bounded_json(body: Body, limit: usize) -> Result<Value, Response> {
    let body = to_bytes(body, limit)
        .await
        .map_err(|_| payload_too_large())?;
    serde_json::from_slice(&body).map_err(|_| {
        json_response(
            StatusCode::BAD_REQUEST,
            BTreeMap::new(),
            json!({
                "_tag": "EnvironmentHttpBadRequestError",
                "message": "Request body must be valid JSON."
            }),
        )
    })
}

fn json_response(status: StatusCode, headers: BTreeMap<String, String>, body: Value) -> Response {
    let mut response = (status, axum::Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    append_headers(&mut response, headers);
    response
}

fn mcp_response(response: McpHttpResponse) -> Response {
    let Ok(mut status) = StatusCode::from_u16(response.status) else {
        return internal_error();
    };
    if status == StatusCode::OK && response.body.is_empty() {
        status = StatusCode::ACCEPTED;
    }
    let mut output = Response::builder()
        .status(status)
        .header(header::CACHE_CONTROL, NO_STORE)
        .body(Body::from(response.body))
        .unwrap_or_else(|_| internal_error());
    append_headers(&mut output, response.headers);
    output
}

fn diagnostic_logs_response(response: DiagnosticLogsHttpResponse) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            attachment_disposition(&response.filename),
        )
        .header(header::CACHE_CONTROL, NO_STORE)
        .header("x-content-type-options", "nosniff")
        .body(Body::from(response.bytes))
        .unwrap_or_else(|_| internal_error())
}

/// The RFC 5987 `attr-char` set: everything outside it is percent-encoded in a `filename*`.
const RFC5987_ATTR_CHAR: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// `Content-Disposition` for a download, for any name a workspace can hold.
///
/// A header value carries bytes, not text: a raw `é` or a control byte in `filename` would make
/// `Response::builder` refuse the whole response, turning a downloadable file into an opaque 500.
/// So `filename` is an ASCII transliteration every client understands, and the real name rides
/// alongside in an RFC 5987 `filename*`, which every current browser prefers when present. A name
/// that is already plain ASCII carries no `filename*` at all.
fn attachment_disposition(file_name: &str) -> String {
    let fallback: String = file_name
        .chars()
        .map(|character| {
            if character.is_ascii()
                && !character.is_ascii_control()
                && !matches!(character, '"' | '\\')
            {
                character
            } else {
                '_'
            }
        })
        .collect();
    if fallback == file_name {
        return format!("attachment; filename=\"{fallback}\"");
    }
    // An all-substituted name still needs something readable in the ASCII slot.
    let fallback = if fallback.chars().all(|character| character == '_') {
        "download".to_owned()
    } else {
        fallback
    };
    let encoded = percent_encoding::utf8_percent_encode(file_name, RFC5987_ATTR_CHAR);
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

fn append_headers(response: &mut Response, headers: BTreeMap<String, String>) {
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            response.headers_mut().insert(name, value);
        }
    }
}

fn payload_too_large() -> Response {
    json_response(
        StatusCode::PAYLOAD_TOO_LARGE,
        BTreeMap::new(),
        json!({
            "_tag": "EnvironmentHttpBadRequestError",
            "message": "Request body exceeds the configured limit."
        }),
    )
}

fn bad_request(message: &str) -> Response {
    json_response(
        StatusCode::BAD_REQUEST,
        BTreeMap::new(),
        json!({
            "_tag": "EnvironmentHttpBadRequestError",
            "message": message,
        }),
    )
}

fn internal_error() -> Response {
    json_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        BTreeMap::new(),
        json!({
            "_tag": "EnvironmentHttpInternalServerError",
            "message": "Internal Server Error"
        }),
    )
}

struct CancellationGuard(CancellationToken);

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_error_helpers_preserve_bad_request_and_internal_statuses() {
        assert_eq!(bad_request("invalid").status(), StatusCode::BAD_REQUEST);
        assert_eq!(internal_error().status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(payload_too_large().status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
