use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use axum::http::{Method, StatusCode, header};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::http_routes::{
    HttpRouteError, JsonOperation, JsonRouteResponse, McpHttpResponse, RouteContext,
};
use crate::preview::{OpenRequestError, PreviewManager};

const RELAY_LINK_PROOF_TYP: &str = "bibcode-env-link+jwt";
const RELAY_MINT_REQUEST_TYP: &str = "bibcode-cloud-mint+jwt";
const RELAY_HEALTH_REQUEST_TYP: &str = "bibcode-cloud-health+jwt";
const RELAY_MINT_RESPONSE_TYP: &str = "bibcode-env-mint+jwt";
const RELAY_HEALTH_RESPONSE_TYP: &str = "bibcode-env-health+jwt";
const PROOF_MAX_LIFETIME_SECONDS: i64 = 5 * 60;
const PROOF_CLOCK_SKEW_SECONDS: i64 = 60;
const MCP_IDLE_SECONDS: i64 = 30 * 60;
const MCP_LIFETIME_SECONDS: i64 = 8 * 60 * 60;

type StringFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

#[derive(Clone)]
pub struct ConnectMcpConfig {
    pub environment_id: String,
    pub descriptor: Value,
    pub mcp_endpoint: String,
    /// This server's `POST /api/preview/open-url` URL, as a process on this host reaches it.
    pub open_url_endpoint: String,
    pub now_epoch_seconds: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub max_mcp_credentials: usize,
    pub max_mcp_sessions: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedCloudProof {
    pub issuer: String,
    pub subject: String,
    pub audience: String,
    pub jwt_id: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub environment_id: String,
    pub nonce: String,
    pub scope: Vec<String>,
    pub client_proof_key_thumbprint: Option<String>,
    pub confirmation_thumbprint: Option<String>,
}

type SignFn = Arc<dyn Fn(String, Value) -> StringFuture<String> + Send + Sync>;
type VerifyFn = Arc<
    dyn Fn(String, String, String, String, String, i64) -> StringFuture<DecodedCloudProof>
        + Send
        + Sync,
>;
type KeyPairFn = Arc<dyn Fn() -> StringFuture<(String, String)> + Send + Sync>;

#[derive(Clone)]
pub struct JwtCodec {
    sign: SignFn,
    verify: VerifyFn,
    key_pair: KeyPairFn,
}

impl JwtCodec {
    pub fn new<Sign, SignFut, Verify, VerifyFut, Keys, KeysFut>(
        sign: Sign,
        verify: Verify,
        key_pair: Keys,
    ) -> Self
    where
        Sign: Fn(String, Value) -> SignFut + Send + Sync + 'static,
        SignFut: Future<Output = Result<String, String>> + Send + 'static,
        Verify:
            Fn(String, String, String, String, String, i64) -> VerifyFut + Send + Sync + 'static,
        VerifyFut: Future<Output = Result<DecodedCloudProof, String>> + Send + 'static,
        Keys: Fn() -> KeysFut + Send + Sync + 'static,
        KeysFut: Future<Output = Result<(String, String), String>> + Send + 'static,
    {
        Self {
            sign: Arc::new(move |typ, payload| Box::pin(sign(typ, payload))),
            verify: Arc::new(move |key, typ, token, issuer, audience, now| {
                Box::pin(verify(key, typ, token, issuer, audience, now))
            }),
            key_pair: Arc::new(move || Box::pin(key_pair())),
        }
    }
}

type EndpointFn = Arc<dyn Fn(Value) -> StringFuture<Value> + Send + Sync>;

#[derive(Clone)]
pub struct EndpointRuntime(EndpointFn);

impl EndpointRuntime {
    pub fn new<Apply, Fut>(apply: Apply) -> Self
    where
        Apply: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        Self(Arc::new(move |config| Box::pin(apply(config))))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingCredential {
    pub credential: String,
    pub expires_at: String,
}

type PairingFn = Arc<dyn Fn(String) -> StringFuture<PairingCredential> + Send + Sync>;

#[derive(Clone)]
pub struct PairingIssuer(PairingFn);

impl PairingIssuer {
    pub fn new<Issue, Fut>(issue: Issue) -> Self
    where
        Issue: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<PairingCredential, String>> + Send + 'static,
    {
        Self(Arc::new(move |thumbprint| Box::pin(issue(thumbprint))))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewScope {
    pub environment_id: String,
    pub thread_id: String,
    pub provider_session_id: String,
    pub provider_instance_id: String,
}

type PreviewFn = Arc<
    dyn Fn(PreviewScope, String, Value, Option<String>, CancellationToken) -> StringFuture<Value>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct PreviewInvoker(PreviewFn);

impl PreviewInvoker {
    pub fn new<Invoke, Fut>(invoke: Invoke) -> Self
    where
        Invoke: Fn(PreviewScope, String, Value, Option<String>, CancellationToken) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        Self(Arc::new(
            move |scope, operation, input, tab_id, cancellation| {
                Box::pin(invoke(scope, operation, input, tab_id, cancellation))
            },
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpIssuedCredential {
    pub environment_id: String,
    pub thread_id: String,
    pub provider_session_id: String,
    pub provider_instance_id: String,
    pub endpoint: String,
    pub authorization_header: String,
    pub expires_at: i64,
}

#[derive(Debug)]
pub struct ConnectMcpError {
    status: StatusCode,
    body: Value,
    headers: BTreeMap<String, String>,
}

impl ConnectMcpError {
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub const fn body(&self) -> &Value {
        &self.body
    }

    #[must_use]
    pub fn into_http(self) -> HttpRouteError {
        let mut error = HttpRouteError::new(self.status, self.body);
        for (name, value) in self.headers {
            error = error.with_header(name, value);
        }
        error
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "EnvironmentHttpBadRequestError",
            message,
        )
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "EnvironmentHttpUnauthorizedError",
            message,
        )
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "EnvironmentHttpConflictError",
            message,
        )
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EnvironmentHttpInternalServerError",
            message,
        )
    }

    fn mcp(status: StatusCode, message: impl Into<String>) -> Self {
        Self::new(status, "McpHttpTransportError", message)
    }

    fn new(status: StatusCode, tag: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: json!({ "_tag": tag, "message": message.into() }),
            headers: BTreeMap::new(),
        }
    }

    fn invalid_mcp_credential() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: json!({
                "error": "invalid_mcp_credential",
                "message": "A valid provider-scoped MCP bearer credential is required."
            }),
            headers: BTreeMap::from([(
                header::WWW_AUTHENTICATE.as_str().to_owned(),
                "Bearer".to_owned(),
            )]),
        }
    }

    fn invalid_open_url_credential() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: json!({
                "error": "invalid_open_url_credential",
                "message": "A valid thread-scoped open-url bearer credential is required."
            }),
            headers: BTreeMap::from([(
                header::WWW_AUTHENTICATE.as_str().to_owned(),
                "Bearer".to_owned(),
            )]),
        }
    }

    fn too_many_open_requests(message: String) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: json!({ "error": "too_many_open_requests", "message": message }),
            headers: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct ConnectMcpService {
    database_path: Arc<PathBuf>,
    config: Arc<ConnectMcpConfig>,
    jwt: JwtCodec,
    endpoint: EndpointRuntime,
    pairing: PairingIssuer,
    preview: PreviewInvoker,
    mcp: Arc<Mutex<McpState>>,
    /// Serializes changes to the stored link and its managed endpoint (link, unlink, and the
    /// startup restore), so a restore never reapplies a config a newer change replaced.
    link_mutation: Arc<Mutex<()>>,
}

#[derive(Clone)]
struct McpCredentialRecord {
    scope: PreviewScope,
    expires_at: i64,
    last_used_at: i64,
}

struct McpSessionRecord {
    credential_hash: String,
    last_used_at: i64,
}

/// A thread-scoped token that only authorizes `POST /api/preview/open-url`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenUrlCredential {
    pub token: String,
    pub expires_at: i64,
}

struct OpenUrlRecord {
    thread_id: String,
    expires_at: i64,
}

#[derive(Default)]
struct McpState {
    credentials: HashMap<String, McpCredentialRecord>,
    sessions: HashMap<String, McpSessionRecord>,
    /// Open-url credentials by token hash. Kept apart from `credentials` so neither kind of
    /// token can authenticate the other's route. A thread may hold several (one per provider
    /// session or terminal), so issuing one never revokes another.
    open_url: HashMap<String, OpenUrlRecord>,
}

impl ConnectMcpService {
    pub async fn open(
        database_path: impl AsRef<Path>,
        config: ConnectMcpConfig,
        jwt: JwtCodec,
        endpoint: EndpointRuntime,
        pairing: PairingIssuer,
        preview: PreviewInvoker,
    ) -> Result<Self, ConnectMcpError> {
        let database_path = database_path.as_ref().to_path_buf();
        let initialize_path = database_path.clone();
        tokio::task::spawn_blocking(move || initialize_database(&initialize_path))
            .await
            .map_err(|error| ConnectMcpError::internal(error.to_string()))?
            .map_err(ConnectMcpError::internal)?;
        Ok(Self {
            database_path: Arc::new(database_path),
            config: Arc::new(config),
            jwt,
            endpoint,
            pairing,
            preview,
            mcp: Arc::new(Mutex::new(McpState::default())),
            link_mutation: Arc::default(),
        })
    }

    pub async fn json(
        &self,
        operation: JsonOperation,
        payload: Option<Value>,
        context: RouteContext,
    ) -> Result<JsonRouteResponse, ConnectMcpError> {
        if context.cancellation.is_cancelled() {
            return Err(ConnectMcpError::bad_request("Request was cancelled."));
        }
        match operation {
            JsonOperation::ConnectLinkProof => {
                self.link_proof(required_payload(payload)?, context).await
            }
            JsonOperation::ConnectRelayConfig => {
                self.relay_config(required_payload(payload)?).await
            }
            JsonOperation::ConnectLinkState => self.link_state().await,
            JsonOperation::ConnectUnlink => self.unlink().await,
            JsonOperation::ConnectHealth => self.health(required_payload(payload)?).await,
            JsonOperation::ConnectMintCredential => {
                self.mint_credential(required_payload(payload)?).await
            }
            _ => Err(ConnectMcpError::bad_request(
                "Unsupported Connect operation.",
            )),
        }
    }

    pub async fn json_http(
        &self,
        operation: JsonOperation,
        payload: Option<Value>,
        context: RouteContext,
    ) -> Result<JsonRouteResponse, HttpRouteError> {
        self.json(operation, payload, context)
            .await
            .map_err(ConnectMcpError::into_http)
    }

    pub async fn issue_mcp_credential(
        &self,
        thread_id: impl Into<String>,
        provider_instance_id: impl Into<String>,
    ) -> Result<McpIssuedCredential, ConnectMcpError> {
        let now = (self.config.now_epoch_seconds)();
        let mut token_bytes = [0_u8; 32];
        getrandom::fill(&mut token_bytes)
            .map_err(|error| ConnectMcpError::internal(error.to_string()))?;
        let token = URL_SAFE_NO_PAD.encode(token_bytes);
        let token_hash = hash_token(&token);
        let thread_id = thread_id.into();
        let provider_instance_id = provider_instance_id.into();
        let provider_session_id = Uuid::new_v4().to_string();
        let expires_at = now.saturating_add(MCP_LIFETIME_SECONDS);
        let scope = PreviewScope {
            environment_id: self.config.environment_id.clone(),
            thread_id: thread_id.clone(),
            provider_session_id: provider_session_id.clone(),
            provider_instance_id: provider_instance_id.clone(),
        };
        let mut state = self.mcp.lock().await;
        prune_mcp(&mut state, now);
        state
            .credentials
            .retain(|_, record| record.scope.thread_id != thread_id);
        if state.credentials.len() >= self.config.max_mcp_credentials.max(1) {
            return Err(ConnectMcpError::internal(
                "Provider MCP credential capacity exceeded.",
            ));
        }
        state.credentials.insert(
            token_hash,
            McpCredentialRecord {
                scope,
                expires_at,
                last_used_at: now,
            },
        );
        Ok(McpIssuedCredential {
            environment_id: self.config.environment_id.clone(),
            thread_id,
            provider_session_id,
            provider_instance_id,
            endpoint: self.config.mcp_endpoint.clone(),
            authorization_header: format!("Bearer {token}"),
            expires_at,
        })
    }

    pub async fn issue_open_url_credential(
        &self,
        thread_id: &str,
    ) -> Result<OpenUrlCredential, ConnectMcpError> {
        let now = (self.config.now_epoch_seconds)();
        let mut token_bytes = [0_u8; 32];
        getrandom::fill(&mut token_bytes)
            .map_err(|error| ConnectMcpError::internal(error.to_string()))?;
        let token = URL_SAFE_NO_PAD.encode(token_bytes);
        let expires_at = now.saturating_add(MCP_LIFETIME_SECONDS);
        let mut state = self.mcp.lock().await;
        state.open_url.retain(|_, record| record.expires_at >= now);
        if state.open_url.len() >= self.config.max_mcp_credentials.max(1) {
            // ponytail: evicts the soonest-expiring token (a linear scan) rather than refusing,
            // so a busy server never fails a terminal launch; its holder's open-url stops working.
            if let Some(oldest) = state
                .open_url
                .iter()
                .min_by_key(|(_, record)| record.expires_at)
                .map(|(hash, _)| hash.clone())
            {
                state.open_url.remove(&oldest);
            }
        }
        state.open_url.insert(
            hash_token(&token),
            OpenUrlRecord {
                thread_id: thread_id.to_owned(),
                expires_at,
            },
        );
        Ok(OpenUrlCredential { token, expires_at })
    }

    #[must_use]
    pub fn open_url_endpoint(&self) -> &str {
        &self.config.open_url_endpoint
    }

    /// Returns the thread an open-url token was issued for, if it is live.
    pub async fn verify_open_url_credential(&self, token: &str) -> Option<String> {
        let now = (self.config.now_epoch_seconds)();
        let mut state = self.mcp.lock().await;
        state.open_url.retain(|_, record| record.expires_at >= now);
        state
            .open_url
            .get(&hash_token(token))
            .map(|record| record.thread_id.clone())
    }

    /// `POST /api/preview/open-url`: bearer open-url token only, body `{ "url": … }`. Answers
    /// `202 { requestId, delivered }`; `delivered` is false when no client could see the
    /// request, so the caller should show the URL itself. A thread with too many unclaimed
    /// requests gets `429`.
    pub async fn open_url(
        &self,
        preview: &PreviewManager,
        body: Vec<u8>,
        context: RouteContext,
    ) -> Result<JsonRouteResponse, ConnectMcpError> {
        let token =
            bearer_token(&context).map_err(|_| ConnectMcpError::invalid_open_url_credential())?;
        let thread_id = self
            .verify_open_url_credential(token)
            .await
            .ok_or_else(ConnectMcpError::invalid_open_url_credential)?;
        let url = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|payload| payload.get("url")?.as_str().map(str::to_owned))
            .ok_or_else(|| ConnectMcpError::bad_request("A string \"url\" is required."))?;
        let receipt =
            preview
                .request_open(&thread_id, &url)
                .await
                .map_err(|error| match error {
                    OpenRequestError::InvalidUrl(error) => {
                        ConnectMcpError::bad_request(error.to_string())
                    }
                    OpenRequestError::TooManyPending => {
                        ConnectMcpError::too_many_open_requests(error.to_string())
                    }
                })?;
        Ok(JsonRouteResponse {
            status: StatusCode::ACCEPTED,
            headers: BTreeMap::new(),
            body: json!({ "requestId": receipt.request_id, "delivered": receipt.delivered }),
        })
    }

    pub async fn open_url_http(
        &self,
        preview: &PreviewManager,
        body: Vec<u8>,
        context: RouteContext,
    ) -> Result<JsonRouteResponse, HttpRouteError> {
        self.open_url(preview, body, context)
            .await
            .map_err(ConnectMcpError::into_http)
    }

    pub async fn revoke_mcp_provider_session(&self, provider_session_id: &str) {
        let mut state = self.mcp.lock().await;
        let hashes = state
            .credentials
            .iter()
            .filter(|(_, record)| record.scope.provider_session_id == provider_session_id)
            .map(|(hash, _)| hash.clone())
            .collect::<Vec<_>>();
        state.credentials.retain(|hash, _| !hashes.contains(hash));
        state
            .sessions
            .retain(|_, session| !hashes.contains(&session.credential_hash));
    }

    pub async fn revoke_all_mcp_credentials(&self) {
        let mut state = self.mcp.lock().await;
        state.credentials.clear();
        state.sessions.clear();
    }

    pub async fn mcp(
        &self,
        method: Method,
        body: Vec<u8>,
        context: RouteContext,
    ) -> Result<McpHttpResponse, ConnectMcpError> {
        match method {
            Method::POST => self.mcp_post(body, context).await,
            Method::DELETE => self.mcp_delete(context).await,
            _ => Err(ConnectMcpError::mcp(
                StatusCode::METHOD_NOT_ALLOWED,
                "Unsupported MCP transport method.",
            )),
        }
    }

    pub async fn mcp_http(
        &self,
        method: Method,
        body: Vec<u8>,
        context: RouteContext,
    ) -> Result<McpHttpResponse, HttpRouteError> {
        self.mcp(method, body, context)
            .await
            .map_err(ConnectMcpError::into_http)
    }

    async fn link_proof(
        &self,
        payload: Value,
        context: RouteContext,
    ) -> Result<JsonRouteResponse, ConnectMcpError> {
        let object = object(&payload)?;
        let challenge = string_field(object, "challenge")?;
        let relay_issuer = normalize_issuer(string_field(object, "relayIssuer")?);
        let endpoint = object.get("endpoint").cloned().ok_or_else(|| {
            ConnectMcpError::bad_request("Managed endpoint configuration is required.")
        })?;
        if endpoint.get("providerKind").and_then(Value::as_str) != Some("cloudflare_tunnel")
            || !allowed_origin(object.get("origin"), &context)
        {
            return Err(ConnectMcpError::bad_request(
                "Invalid managed endpoint origin.",
            ));
        }
        let now = (self.config.now_epoch_seconds)();
        let (_, public_key) = (self.jwt.key_pair)()
            .await
            .map_err(|_| ConnectMcpError::internal("Could not generate environment link proof."))?;
        let signed_payload = json!({
            "iss": format!("bibcode-env:{}", self.config.environment_id),
            "aud": relay_issuer,
            "sub": self.config.environment_id,
            "jti": Uuid::new_v4().to_string(),
            "iat": now,
            "exp": now.saturating_add(PROOF_MAX_LIFETIME_SECONDS),
            "challenge": challenge,
            "descriptor": self.config.descriptor,
            "environmentId": self.config.environment_id,
            "environmentPublicKey": public_key.trim(),
            "endpoint": endpoint,
            "origin": object.get("origin").cloned().unwrap_or(Value::Null),
            "scopes": ["managed_tunnels"]
        });
        let proof = (self.jwt.sign)(RELAY_LINK_PROOF_TYP.into(), signed_payload)
            .await
            .map_err(|_| ConnectMcpError::internal("Could not generate environment link proof."))?;
        credential_response(json!(proof))
    }

    async fn relay_config(&self, payload: Value) -> Result<JsonRouteResponse, ConnectMcpError> {
        let _link_mutation = self.link_mutation.lock().await;
        let object = object(&payload)?;
        let relay_url = string_field(object, "relayUrl")?;
        validate_secure_url(relay_url, "Relay URL")?;
        let relay_issuer = optional_string_field(object, "relayIssuer")?.unwrap_or(relay_url);
        validate_secure_url(relay_issuer, "Relay issuer")?;
        let cloud_user_id = nonempty_field(object, "cloudUserId", "Cloud user id is required.")?;
        let environment_credential = nonempty_field(
            object,
            "environmentCredential",
            "Relay environment credential is required.",
        )?;
        let cloud_mint_public_key = nonempty_field(
            object,
            "cloudMintPublicKey",
            "Cloud mint public key must be a valid Ed25519 public key.",
        )?;
        if !cloud_mint_public_key.contains("BEGIN PUBLIC KEY") {
            return Err(ConnectMcpError::bad_request(
                "Cloud mint public key must be a valid Ed25519 public key.",
            ));
        }
        let existing = self.read_secret("cloud-linked-user-id").await?;
        // A background re-link names the link it set out to refresh; if another client
        // unlinked or re-linked meanwhile (even to the same account), it must not undo that.
        if let Some(required) = optional_string_field(object, "requireLinkGeneration")?
            && self.link_generation().await?.as_deref() != Some(required)
        {
            return Err(ConnectMcpError::conflict(
                "This environment's BiBCode Connect link changed. Link it again from Settings.",
            ));
        }
        if let Some(existing) = existing
            && existing != cloud_user_id
        {
            return Err(ConnectMcpError::conflict(
                "This environment is already linked to a different cloud account. Unlink it before switching accounts.",
            ));
        }
        let endpoint_config = object
            .get("endpointRuntime")
            .cloned()
            .unwrap_or(Value::Null);
        let endpoint_status = (self.endpoint.0)(endpoint_config.clone())
            .await
            .map_err(|_| {
                ConnectMcpError::internal("Managed endpoint runtime could not be started.")
            })?;
        let status = endpoint_status
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        if !matches!(status, "disabled" | "running") {
            return Err(ConnectMcpError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "EnvironmentCloudEndpointUnavailableError",
                "Managed endpoint runtime could not be started.",
            ));
        }
        self.replace_link_config(LinkConfig {
            relay_url: relay_url.into(),
            relay_issuer: relay_issuer.into(),
            cloud_user_id: cloud_user_id.into(),
            environment_credential: environment_credential.into(),
            cloud_mint_public_key: cloud_mint_public_key.into(),
            endpoint_runtime: endpoint_config,
            // The relay just pointed the tunnel's ingress at this server's current port.
            origin_port: self.current_origin_port(),
        })
        .await?;
        Ok(JsonRouteResponse::ok(json!({
            "ok": true,
            "endpointRuntimeStatus": endpoint_status
        })))
    }

    async fn link_state(&self) -> Result<JsonRouteResponse, ConnectMcpError> {
        let values = self.read_link_state().await?;
        let endpoint = self.stored_endpoint_runtime().await?;
        let preview_hostnames = endpoint
            .as_ref()
            .and_then(|config| config.get("previewHostnames"))
            .and_then(Value::as_array)
            .is_some_and(|names| !names.is_empty());
        let stale = endpoint.is_some() && self.endpoint_origin_moved().await?;
        let generation = self.link_generation().await?;
        Ok(JsonRouteResponse::ok(json!({
            "linked": values.0.is_some(),
            "cloudUserId": values.0,
            "relayUrl": values.1,
            "relayIssuer": values.2,
            "managedEndpoint": endpoint.is_some(),
            "previewHostnames": preview_hostnames,
            "managedEndpointStale": stale,
            "linkGeneration": generation
        })))
    }

    /// Restarts the managed tunnel from the stored link after a server restart. An unlinked
    /// server, or a link without a managed endpoint, has nothing to restore.
    pub async fn restore_managed_endpoint(&self) -> Result<Option<Value>, ConnectMcpError> {
        let _link_mutation = self.link_mutation.lock().await;
        if self.read_secret("cloud-linked-user-id").await?.is_none() {
            return Ok(None);
        }
        let Some(config) = self.stored_endpoint_runtime().await? else {
            return Ok(None);
        };
        // The tunnel's ingress still points at the port of the last link; a connector for it
        // would report running while reaching nothing. The primary client re-links instead.
        if self.endpoint_origin_moved().await? {
            return Ok(Some(json!({ "status": "stale-origin" })));
        }
        (self.endpoint.0)(config).await.map(Some).map_err(|_| {
            ConnectMcpError::internal("Managed endpoint runtime could not be started.")
        })
    }

    /// Identifies the stored link: every link rotates the relay environment credential, so a
    /// domain-separated digest of it changes with each link without revealing it.
    async fn link_generation(&self) -> Result<Option<String>, ConnectMcpError> {
        Ok(self
            .read_secret("relay-environment-credential")
            .await?
            .map(|credential| {
                let digest = Sha256::digest(format!("bibcode-link-generation:{credential}"));
                URL_SAFE_NO_PAD.encode(&digest[..16])
            }))
    }

    /// This server's own listening port (its MCP endpoint is on the same listener).
    fn current_origin_port(&self) -> Option<u16> {
        self.config
            .mcp_endpoint
            .parse::<axum::http::Uri>()
            .ok()?
            .port_u16()
    }

    /// The stored link's tunnel was provisioned for another port than this server now
    /// listens on (the desktop picks a free port each launch). Links stored before the port
    /// was recorded count as current.
    async fn endpoint_origin_moved(&self) -> Result<bool, ConnectMcpError> {
        let stored = self
            .read_secret("cloud-endpoint-origin-port")
            .await?
            .and_then(|port| port.parse::<u16>().ok());
        Ok(stored.is_some_and(|stored| Some(stored) != self.current_origin_port()))
    }

    /// The stored, non-null managed endpoint runtime config.
    async fn stored_endpoint_runtime(&self) -> Result<Option<Value>, ConnectMcpError> {
        Ok(self
            .read_secret("cloud-endpoint-runtime-config")
            .await?
            .and_then(|stored| serde_json::from_str::<Value>(&stored).ok())
            .filter(|config| !config.is_null()))
    }

    async fn unlink(&self) -> Result<JsonRouteResponse, ConnectMcpError> {
        let _link_mutation = self.link_mutation.lock().await;
        let endpoint_status = (self.endpoint.0)(Value::Null).await.map_err(|_| {
            ConnectMcpError::internal("Could not disable managed endpoint runtime.")
        })?;
        self.clear_link_config().await?;
        Ok(JsonRouteResponse::ok(json!({
            "ok": true,
            "endpointRuntimeStatus": endpoint_status
        })))
    }

    async fn health(&self, payload: Value) -> Result<JsonRouteResponse, ConnectMcpError> {
        let proof_token = string_field(object(&payload)?, "proof")?;
        let linked = self.required_link_material().await?;
        let now = (self.config.now_epoch_seconds)();
        let proof = (self.jwt.verify)(
            linked.cloud_mint_public_key,
            RELAY_HEALTH_REQUEST_TYP.into(),
            proof_token.into(),
            linked.relay_issuer.clone(),
            format!("bibcode-env:{}", self.config.environment_id),
            now,
        )
        .await
        .map_err(|_| ConnectMcpError::unauthorized("Invalid cloud health request."))?;
        validate_cloud_proof(
            &proof,
            &self.config.environment_id,
            &linked.cloud_user_id,
            "environment:status",
            now,
            false,
        )?;
        self.consume_replay("health", &proof.jwt_id, &proof.nonce, now)
            .await
            .map_err(|error| match error.status {
                StatusCode::CONFLICT => {
                    ConnectMcpError::conflict("Cloud health request was already consumed.")
                }
                _ => error,
            })?;
        let checked_at = iso_timestamp(now)?;
        let response_payload = json!({
            "iss": format!("bibcode-env:{}", self.config.environment_id),
            "aud": linked.relay_issuer,
            "sub": self.config.environment_id,
            "jti": Uuid::new_v4().to_string(),
            "iat": now,
            "exp": now.saturating_add(PROOF_MAX_LIFETIME_SECONDS),
            "environmentId": self.config.environment_id,
            "requestNonce": proof.nonce,
            "status":"online",
            "descriptor": self.config.descriptor,
            "checkedAt": checked_at
        });
        let response_proof = (self.jwt.sign)(RELAY_HEALTH_RESPONSE_TYP.into(), response_payload)
            .await
            .map_err(|_| ConnectMcpError::internal("Could not answer cloud health request."))?;
        credential_response(json!({
            "environmentId": self.config.environment_id,
            "status":"online",
            "descriptor": self.config.descriptor,
            "checkedAt": checked_at,
            "proof": response_proof
        }))
    }

    async fn mint_credential(&self, payload: Value) -> Result<JsonRouteResponse, ConnectMcpError> {
        let proof_token = string_field(object(&payload)?, "proof")?;
        let linked = self.required_link_material().await?;
        let now = (self.config.now_epoch_seconds)();
        let proof = (self.jwt.verify)(
            linked.cloud_mint_public_key,
            RELAY_MINT_REQUEST_TYP.into(),
            proof_token.into(),
            linked.relay_issuer.clone(),
            format!("bibcode-env:{}", self.config.environment_id),
            now,
        )
        .await
        .map_err(|_| ConnectMcpError::unauthorized("Invalid cloud mint request."))?;
        validate_cloud_proof(
            &proof,
            &self.config.environment_id,
            &linked.cloud_user_id,
            "environment:connect",
            now,
            true,
        )?;
        self.consume_replay("mint", &proof.jwt_id, &proof.nonce, now)
            .await
            .map_err(|error| match error.status {
                StatusCode::CONFLICT => {
                    ConnectMcpError::conflict("Cloud mint request was already consumed.")
                }
                _ => error,
            })?;
        let thumbprint = proof
            .client_proof_key_thumbprint
            .clone()
            .ok_or_else(|| ConnectMcpError::unauthorized("Invalid cloud mint request."))?;
        let issued = (self.pairing.0)(thumbprint.clone()).await.map_err(|_| {
            ConnectMcpError::internal("Could not issue cloud connection credential.")
        })?;
        let expires_at_seconds = parse_iso_timestamp(&issued.expires_at)?;
        let response_payload = json!({
            "iss": format!("bibcode-env:{}", self.config.environment_id),
            "aud": linked.relay_issuer,
            "sub": self.config.environment_id,
            "jti": Uuid::new_v4().to_string(),
            "iat": now,
            "exp": expires_at_seconds,
            "environmentId": self.config.environment_id,
            "clientProofKeyThumbprint": thumbprint,
            "requestNonce": proof.nonce,
            "credential": issued.credential
        });
        let response_proof = (self.jwt.sign)(RELAY_MINT_RESPONSE_TYP.into(), response_payload)
            .await
            .map_err(|_| {
                ConnectMcpError::internal("Could not issue cloud connection credential.")
            })?;
        credential_response(json!({
            "credential": issued.credential,
            "expiresAt": issued.expires_at,
            "proof": response_proof
        }))
    }

    async fn mcp_post(
        &self,
        body: Vec<u8>,
        context: RouteContext,
    ) -> Result<McpHttpResponse, ConnectMcpError> {
        let token = bearer_token(&context)?;
        let token_hash = hash_token(token);
        let now = (self.config.now_epoch_seconds)();
        let request: Value = serde_json::from_slice(&body)
            .map_err(|_| ConnectMcpError::mcp(StatusCode::BAD_REQUEST, "Invalid JSON-RPC body."))?;
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ConnectMcpError::mcp(StatusCode::BAD_REQUEST, "JSON-RPC method is required.")
            })?;
        let request_id = request.get("id").cloned();
        let mut state = self.mcp.lock().await;
        prune_mcp(&mut state, now);
        let credential = state
            .credentials
            .get_mut(&token_hash)
            .ok_or_else(ConnectMcpError::invalid_mcp_credential)?;
        credential.last_used_at = now;
        let scope = credential.scope.clone();
        let supplied_session = context
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok());
        let session_id = if method == "initialize" {
            if state.sessions.len() >= self.config.max_mcp_sessions.max(1) {
                return Err(ConnectMcpError::mcp(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "MCP session capacity exceeded.",
                ));
            }
            let session_id = Uuid::new_v4().to_string();
            state.sessions.insert(
                session_id.clone(),
                McpSessionRecord {
                    credential_hash: token_hash.clone(),
                    last_used_at: now,
                },
            );
            session_id
        } else {
            let supplied = supplied_session.ok_or_else(|| {
                ConnectMcpError::mcp(StatusCode::BAD_REQUEST, "MCP session id is required.")
            })?;
            let session = state.sessions.get_mut(supplied).ok_or_else(|| {
                ConnectMcpError::mcp(StatusCode::NOT_FOUND, "MCP session was not found.")
            })?;
            if session.credential_hash != token_hash {
                return Err(ConnectMcpError::invalid_mcp_credential());
            }
            session.last_used_at = now;
            supplied.to_owned()
        };
        drop(state);

        if context.cancellation.is_cancelled() && method != "tools/call" {
            return Err(ConnectMcpError::mcp(
                StatusCode::REQUEST_TIMEOUT,
                "MCP request was cancelled.",
            ));
        }
        let result = match method {
            "initialize" => json!({
                "protocolVersion": request.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"BiBCode","version":env!("CARGO_PKG_VERSION")}
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tool_descriptors()}),
            "tools/call" => {
                self.call_tool(request.get("params"), scope, context.cancellation)
                    .await
            }
            "notifications/initialized" => {
                return Ok(McpHttpResponse {
                    status: 202,
                    headers: BTreeMap::new(),
                    body: Vec::new(),
                });
            }
            _ => return json_rpc_error(request_id, -32601, "Method not found", Some(session_id)),
        };
        json_rpc_result(request_id, result, Some(session_id))
    }

    async fn mcp_delete(&self, context: RouteContext) -> Result<McpHttpResponse, ConnectMcpError> {
        let token_hash = hash_token(bearer_token(&context)?);
        let now = (self.config.now_epoch_seconds)();
        let session_id = context
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ConnectMcpError::mcp(StatusCode::BAD_REQUEST, "MCP session id is required.")
            })?;
        let mut state = self.mcp.lock().await;
        prune_mcp(&mut state, now);
        let Some(session) = state.sessions.get(session_id) else {
            return Err(ConnectMcpError::mcp(
                StatusCode::NOT_FOUND,
                "MCP session was not found.",
            ));
        };
        if session.credential_hash != token_hash || !state.credentials.contains_key(&token_hash) {
            return Err(ConnectMcpError::invalid_mcp_credential());
        }
        state.sessions.remove(session_id);
        Ok(McpHttpResponse {
            status: 204,
            headers: BTreeMap::new(),
            body: Vec::new(),
        })
    }

    async fn call_tool(
        &self,
        params: Option<&Value>,
        scope: PreviewScope,
        cancellation: CancellationToken,
    ) -> Value {
        let name = params
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(operation) = name.strip_prefix("preview_") else {
            return tool_failure("McpUnknownToolError", "Unknown preview tool.");
        };
        if !TOOL_OPERATIONS.contains(&operation) {
            return tool_failure("McpUnknownToolError", "Unknown preview tool.");
        }
        let mut arguments = params
            .and_then(|value| value.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let tab_id = arguments
            .get("tabId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(object) = arguments.as_object_mut() {
            object.remove("tabId");
        }
        match (self.preview.0)(
            scope,
            operation_wire_name(operation).into(),
            arguments,
            tab_id,
            cancellation,
        )
        .await
        {
            Ok(value) if operation == "snapshot" => snapshot_tool_result(value),
            Ok(value) => json!({
                "isError":false,
                "structuredContent":value,
                "content":[{"type":"text","text":value.to_string()}]
            }),
            Err(message) => tool_failure("PreviewAutomationExecutionError", &message),
        }
    }

    async fn read_secret(&self, name: &'static str) -> Result<Option<String>, ConnectMcpError> {
        let path = Arc::clone(&self.database_path);
        tokio::task::spawn_blocking(move || {
            let connection = open_database(path.as_ref())?;
            connection
                .query_row(
                    "SELECT value FROM connect_native_secrets WHERE name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?
        .map_err(|_| ConnectMcpError::internal("Could not read environment relay configuration."))
    }

    async fn replace_link_config(&self, config: LinkConfig) -> Result<(), ConnectMcpError> {
        let path = Arc::clone(&self.database_path);
        tokio::task::spawn_blocking(move || write_link_config(path.as_ref(), config))
            .await
            .map_err(|error| ConnectMcpError::internal(error.to_string()))?
            .map_err(|_| {
                ConnectMcpError::internal("Could not persist environment relay configuration.")
            })
    }

    async fn clear_link_config(&self) -> Result<(), ConnectMcpError> {
        let path = Arc::clone(&self.database_path);
        tokio::task::spawn_blocking(move || {
            let mut connection = open_database(path.as_ref())?;
            let transaction = connection.transaction().map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "DELETE FROM connect_native_secrets WHERE name IN ('cloud-linked-user-id','relay-url','relay-issuer','relay-environment-credential','cloud-mint-public-key','cloud-endpoint-runtime-config','cloud-endpoint-origin-port')",
                    [],
                )
                .map_err(|error| error.to_string())?;
            transaction.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?
        .map_err(|_| ConnectMcpError::internal("Could not remove environment relay configuration."))
    }

    async fn read_link_state(
        &self,
    ) -> Result<(Option<String>, Option<String>, Option<String>), ConnectMcpError> {
        let path = Arc::clone(&self.database_path);
        tokio::task::spawn_blocking(move || {
            let connection = open_database(path.as_ref())?;
            let mut result = HashMap::new();
            let mut statement = connection
                .prepare("SELECT name, value FROM connect_native_secrets WHERE name IN ('cloud-linked-user-id','relay-url','relay-issuer')")
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|error| error.to_string())?;
            for row in rows {
                let (name, value) = row.map_err(|error| error.to_string())?;
                result.insert(name, value);
            }
            Ok::<_, String>((
                result.remove("cloud-linked-user-id"),
                result.remove("relay-url"),
                result.remove("relay-issuer"),
            ))
        })
        .await
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?
        .map_err(|_| ConnectMcpError::internal("Could not read environment relay configuration."))
    }

    async fn required_link_material(&self) -> Result<LinkMaterial, ConnectMcpError> {
        let path = Arc::clone(&self.database_path);
        tokio::task::spawn_blocking(move || {
            let connection = open_database(path.as_ref())?;
            let read = |name: &str| {
                connection
                    .query_row(
                        "SELECT value FROM connect_native_secrets WHERE name = ?1",
                        [name],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| error.to_string())
            };
            let cloud_user_id = read("cloud-linked-user-id")?
                .ok_or_else(|| "linked cloud account is missing".to_owned())?;
            let relay_issuer = read("relay-issuer")?
                .or(read("relay-url")?)
                .ok_or_else(|| "relay issuer is missing".to_owned())?;
            let cloud_mint_public_key = read("cloud-mint-public-key")?
                .ok_or_else(|| "cloud mint public key is missing".to_owned())?;
            Ok::<_, String>(LinkMaterial {
                cloud_user_id,
                relay_issuer: normalize_issuer(&relay_issuer),
                cloud_mint_public_key,
            })
        })
        .await
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?
        .map_err(|_| ConnectMcpError::internal("Could not read environment relay configuration."))
    }

    async fn consume_replay(
        &self,
        kind: &'static str,
        jwt_id: &str,
        nonce: &str,
        now: i64,
    ) -> Result<(), ConnectMcpError> {
        let path = Arc::clone(&self.database_path);
        let jwt_id = jwt_id.to_owned();
        let nonce = nonce.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = open_database(path.as_ref())?;
            let transaction = connection.transaction().map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "DELETE FROM connect_native_replay WHERE consumed_at < ?1",
                    [now.saturating_sub(PROOF_MAX_LIFETIME_SECONDS + PROOF_CLOCK_SKEW_SECONDS)],
                )
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "DELETE FROM connect_native_replay WHERE rowid IN (
                       SELECT rowid FROM connect_native_replay
                       ORDER BY consumed_at DESC, rowid DESC LIMIT -1 OFFSET 4094
                     )",
                    [],
                )
                .map_err(|error| error.to_string())?;
            let first = transaction.execute(
                "INSERT OR IGNORE INTO connect_native_replay(kind, value, consumed_at) VALUES (?1, ?2, ?3)",
                params![format!("{kind}:jti"), jwt_id, now],
            ).map_err(|error| error.to_string())?;
            let second = transaction.execute(
                "INSERT OR IGNORE INTO connect_native_replay(kind, value, consumed_at) VALUES (?1, ?2, ?3)",
                params![format!("{kind}:nonce"), nonce, now],
            ).map_err(|error| error.to_string())?;
            if first != 1 || second != 1 {
                return Err("replay".to_owned());
            }
            transaction.commit().map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?
        .map_err(|error| {
            if error == "replay" {
                ConnectMcpError::conflict("Cloud request was already consumed.")
            } else {
                ConnectMcpError::internal("Could not persist cloud request replay state.")
            }
        })
    }
}

struct LinkConfig {
    relay_url: String,
    relay_issuer: String,
    cloud_user_id: String,
    environment_credential: String,
    cloud_mint_public_key: String,
    endpoint_runtime: Value,
    /// The local port the tunnel's ingress was provisioned for.
    origin_port: Option<u16>,
}

struct LinkMaterial {
    cloud_user_id: String,
    relay_issuer: String,
    cloud_mint_public_key: String,
}

fn initialize_database(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let connection = open_database(path)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS connect_native_secrets (
               name TEXT PRIMARY KEY NOT NULL,
               value TEXT NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS connect_native_replay (
               kind TEXT NOT NULL,
               value TEXT NOT NULL,
               consumed_at INTEGER NOT NULL,
               PRIMARY KEY(kind, value)
             ) STRICT;",
        )
        .map_err(|error| error.to_string())
}

fn open_database(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path).map_err(|error| error.to_string())?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn write_link_config(path: &Path, config: LinkConfig) -> Result<(), String> {
    let mut connection = open_database(path)?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let values = [
        ("relay-url", config.relay_url),
        ("relay-issuer", config.relay_issuer),
        ("cloud-linked-user-id", config.cloud_user_id),
        (
            "relay-environment-credential",
            config.environment_credential,
        ),
        ("cloud-mint-public-key", config.cloud_mint_public_key),
        (
            "cloud-endpoint-runtime-config",
            config.endpoint_runtime.to_string(),
        ),
        (
            "cloud-endpoint-origin-port",
            config
                .origin_port
                .map(|port| port.to_string())
                .unwrap_or_default(),
        ),
    ];
    for (name, value) in values {
        transaction
            .execute(
                "INSERT INTO connect_native_secrets(name, value) VALUES (?1, ?2)
                 ON CONFLICT(name) DO UPDATE SET value = excluded.value",
                params![name, value],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn validate_cloud_proof(
    proof: &DecodedCloudProof,
    environment_id: &str,
    cloud_user_id: &str,
    expected_scope: &str,
    now: i64,
    require_confirmation: bool,
) -> Result<(), ConnectMcpError> {
    let confirmation_matches = !require_confirmation
        || proof.client_proof_key_thumbprint.is_some()
            && proof.client_proof_key_thumbprint == proof.confirmation_thumbprint;
    if proof.environment_id != environment_id
        || proof.subject != cloud_user_id
        || proof.scope.as_slice() != [expected_scope]
        || proof.expires_at <= proof.issued_at
        || proof.expires_at.saturating_sub(proof.issued_at) > PROOF_MAX_LIFETIME_SECONDS
        || proof.issued_at > now.saturating_add(PROOF_CLOCK_SKEW_SECONDS)
        || proof.expires_at < now.saturating_sub(PROOF_CLOCK_SKEW_SECONDS)
        || !confirmation_matches
    {
        return Err(ConnectMcpError::unauthorized(if require_confirmation {
            "Invalid cloud mint request."
        } else {
            "Invalid cloud health request."
        }));
    }
    Ok(())
}

fn required_payload(payload: Option<Value>) -> Result<Value, ConnectMcpError> {
    payload.ok_or_else(|| ConnectMcpError::bad_request("Request body is required."))
}

fn object(value: &Value) -> Result<&Map<String, Value>, ConnectMcpError> {
    value
        .as_object()
        .ok_or_else(|| ConnectMcpError::bad_request("Request body must be a JSON object."))
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<&'a str, ConnectMcpError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| ConnectMcpError::bad_request(format!("{name} must be a string.")))
}

fn optional_string_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a str>, ConnectMcpError> {
    match object.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ConnectMcpError::bad_request(format!("{name} must be a string."))),
    }
}

fn nonempty_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    message: &'static str,
) -> Result<&'a str, ConnectMcpError> {
    string_field(object, name).and_then(|value| {
        if value.trim().is_empty() {
            Err(ConnectMcpError::bad_request(message))
        } else {
            Ok(value)
        }
    })
}

fn validate_secure_url(value: &str, label: &str) -> Result<(), ConnectMcpError> {
    let parsed = url::Url::parse(value).map_err(|_| {
        ConnectMcpError::bad_request(format!("{label} must be a secure absolute HTTPS URL."))
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
    {
        return Err(ConnectMcpError::bad_request(format!(
            "{label} must be a secure absolute HTTPS URL."
        )));
    }
    Ok(())
}

fn normalize_issuer(value: &str) -> String {
    value.trim().trim_end_matches('/').to_owned()
}

fn allowed_origin(origin: Option<&Value>, context: &RouteContext) -> bool {
    let Some(origin) = origin.and_then(Value::as_object) else {
        return false;
    };
    let host = origin
        .get("localHttpHost")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    let port = origin.get("localHttpPort").and_then(Value::as_u64);
    let request_host = context
        .uri
        .host()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    let request_port = context.uri.port_u16().map(u64::from).unwrap_or(80);
    matches!(host.as_str(), "127.0.0.1" | "::1" | "localhost")
        && matches!(request_host.as_str(), "127.0.0.1" | "::1" | "localhost")
        && port == Some(request_port)
        && !context.headers.contains_key("x-forwarded-host")
        && !context.headers.contains_key("x-forwarded-proto")
}

fn credential_response(body: Value) -> Result<JsonRouteResponse, ConnectMcpError> {
    Ok(JsonRouteResponse {
        status: StatusCode::OK,
        headers: BTreeMap::from([
            ("cache-control".into(), "no-store".into()),
            ("pragma".into(), "no-cache".into()),
        ]),
        body,
    })
}

fn iso_timestamp(epoch_seconds: i64) -> Result<String, ConnectMcpError> {
    time::OffsetDateTime::from_unix_timestamp(epoch_seconds)
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| ConnectMcpError::internal(error.to_string()))
}

fn parse_iso_timestamp(value: &str) -> Result<i64, ConnectMcpError> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(|timestamp| timestamp.unix_timestamp())
        .map_err(|_| ConnectMcpError::internal("Could not issue cloud connection credential."))
}

fn bearer_token(context: &RouteContext) -> Result<&str, ConnectMcpError> {
    context
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(ConnectMcpError::invalid_mcp_credential)
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn prune_mcp(state: &mut McpState, now: i64) {
    state.credentials.retain(|_, record| {
        record.expires_at >= now && now.saturating_sub(record.last_used_at) <= MCP_IDLE_SECONDS
    });
    let live_credentials = state.credentials.keys().cloned().collect::<Vec<_>>();
    state.sessions.retain(|_, session| {
        live_credentials.contains(&session.credential_hash)
            && now.saturating_sub(session.last_used_at) <= MCP_IDLE_SECONDS
    });
}

fn json_rpc_result(
    id: Option<Value>,
    result: Value,
    session_id: Option<String>,
) -> Result<McpHttpResponse, ConnectMcpError> {
    let id = id.ok_or_else(|| {
        ConnectMcpError::mcp(StatusCode::BAD_REQUEST, "JSON-RPC request id is required.")
    })?;
    let mut headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
    if let Some(session_id) = session_id {
        headers.insert("mcp-session-id".into(), session_id);
    }
    Ok(McpHttpResponse {
        status: 200,
        headers,
        body: serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result}))
            .map_err(|error| ConnectMcpError::internal(error.to_string()))?,
    })
}

fn json_rpc_error(
    id: Option<Value>,
    code: i64,
    message: &str,
    session_id: Option<String>,
) -> Result<McpHttpResponse, ConnectMcpError> {
    let mut headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
    if let Some(session_id) = session_id {
        headers.insert("mcp-session-id".into(), session_id);
    }
    Ok(McpHttpResponse {
        status: 200,
        headers,
        body: serde_json::to_vec(&json!({
            "jsonrpc":"2.0",
            "id":id.unwrap_or(Value::Null),
            "error":{"code":code,"message":message}
        }))
        .map_err(|error| ConnectMcpError::internal(error.to_string()))?,
    })
}

fn tool_failure(tag: &str, message: &str) -> Value {
    json!({
        "isError":true,
        "structuredContent":{"error":{"_tag":tag,"message":message}},
        "content":[{"type":"text","text":message}]
    })
}

fn snapshot_tool_result(mut value: Value) -> Value {
    let screenshot = value
        .as_object_mut()
        .and_then(|object| object.remove("screenshot"));
    let Some(screenshot) = screenshot else {
        return tool_failure(
            "PreviewAutomationMalformedResponseError",
            "Preview snapshot response did not include a screenshot.",
        );
    };
    let data = screenshot
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mime_type = screenshot
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    let metadata = json!({
        "mimeType": mime_type,
        "width": screenshot.get("width").cloned().unwrap_or(Value::Null),
        "height": screenshot.get("height").cloned().unwrap_or(Value::Null)
    });
    if let Some(object) = value.as_object_mut() {
        object.insert("screenshot".into(), metadata);
    }
    json!({
        "isError":false,
        "structuredContent":value,
        "content":[
            {"type":"text","text":value.to_string()},
            {"type":"image","data":data,"mimeType":mime_type}
        ]
    })
}

const TOOL_OPERATIONS: &[&str] = &[
    "status",
    "open",
    "navigate",
    "resize",
    "snapshot",
    "click",
    "type",
    "press",
    "scroll",
    "evaluate",
    "wait_for",
    "recording_start",
    "recording_stop",
];

fn operation_wire_name(operation: &str) -> &str {
    match operation {
        "wait_for" => "waitFor",
        "recording_start" => "recordingStart",
        "recording_stop" => "recordingStop",
        other => other,
    }
}

fn tool_descriptors() -> Vec<Value> {
    TOOL_OPERATIONS
        .iter()
        .map(|operation| {
            let name = format!("preview_{operation}");
            let read_only = matches!(*operation, "status" | "snapshot" | "wait_for");
            json!({
                "name":name,
                "description":format!("Control the collaborative browser preview ({operation})."),
                "inputSchema":{"type":"object","additionalProperties":true},
                "annotations":{
                    "title":format!("Preview {operation}"),
                    "readOnlyHint":read_only,
                    "destructiveHint":!read_only,
                    "idempotentHint":read_only,
                    "openWorldHint":true
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
