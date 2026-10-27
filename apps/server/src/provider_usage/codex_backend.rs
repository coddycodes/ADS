use std::{
    collections::HashMap,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    CodexRateLimitResetOutcome, ProviderUsageCommandError, ProviderUsageWindow,
    RateLimitResetCredits, parse_reset,
};

const CODEX_BACKEND_BASE: &str = "https://chatgpt.com/backend-api/wham";

#[derive(Clone, Debug)]
pub(super) struct CodexBackendEndpoints {
    usage: String,
    reset_credits: String,
    consume_reset_credit: String,
}

impl CodexBackendEndpoints {
    pub(super) fn production() -> Self {
        Self::for_base(CODEX_BACKEND_BASE)
    }

    fn for_base(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            usage: format!("{base}/usage"),
            reset_credits: format!("{base}/rate-limit-reset-credits"),
            consume_reset_credit: format!("{base}/rate-limit-reset-credits/consume"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CodexBackendUsage {
    pub(super) session: Option<ProviderUsageWindow>,
    pub(super) weekly: Option<ProviderUsageWindow>,
    pub(super) plan_type: String,
    pub(super) rate_limit_reset_credits: Option<RateLimitResetCredits>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CodexBackendFetch {
    Success(CodexBackendUsage),
    Fallback(CodexBackendFallback),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CodexBackendFallbackKind {
    Credentials,
    Authorization,
    Unavailable,
    Incompatible,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CodexBackendFallback {
    kind: CodexBackendFallbackKind,
    message: &'static str,
}

impl CodexBackendFallback {
    fn new(kind: CodexBackendFallbackKind, message: &'static str) -> Self {
        Self { kind, message }
    }
}

impl fmt::Display for CodexBackendFallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

#[derive(Deserialize)]
struct CodexAuthFile {
    tokens: Option<CodexAuthTokens>,
}

#[derive(Deserialize)]
struct CodexAuthTokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

struct CodexBackendAuth {
    access_token: String,
    account_id: Option<String>,
}

type AuthReadFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send>>;
type AuthReader = Arc<dyn Fn(PathBuf) -> AuthReadFuture + Send + Sync>;

#[derive(Clone)]
enum AuthReadOutcome {
    Bytes(Arc<Vec<u8>>),
    Failed,
}

#[derive(Clone)]
struct SharedAuthRead {
    id: u64,
    receiver: tokio::sync::watch::Receiver<Option<AuthReadOutcome>>,
}

static PRODUCTION_AUTH_READER: OnceLock<AuthReader> = OnceLock::new();

struct AuthReadRegistry {
    reads: Mutex<HashMap<PathBuf, SharedAuthRead>>,
    next_id: AtomicU64,
}

impl Default for AuthReadRegistry {
    fn default() -> Self {
        Self {
            reads: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

struct AuthReadOwner {
    registry: Arc<AuthReadRegistry>,
    path: PathBuf,
    id: u64,
    sender: Option<tokio::sync::watch::Sender<Option<AuthReadOutcome>>>,
}

impl AuthReadOwner {
    fn publish(mut self, outcome: AuthReadOutcome) {
        self.remove_registration();
        if let Some(sender) = self.sender.take() {
            sender.send_replace(Some(outcome));
        }
    }

    fn remove_registration(&mut self) {
        let mut reads = self
            .registry
            .reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if reads
            .get(&self.path)
            .is_some_and(|current| current.id == self.id)
        {
            reads.remove(&self.path);
        }
    }
}

impl Drop for AuthReadOwner {
    fn drop(&mut self) {
        self.remove_registration();
    }
}

static AUTH_READ_REGISTRY: OnceLock<Arc<AuthReadRegistry>> = OnceLock::new();

#[derive(Deserialize)]
struct BackendUsageResponse {
    plan_type: Option<String>,
    rate_limit: Option<BackendRateLimit>,
    rate_limit_reset_credits: Option<BackendRateLimitResetCredits>,
}

#[derive(Deserialize)]
struct BackendRateLimit {
    primary_window: Option<BackendRateLimitWindow>,
    secondary_window: Option<BackendRateLimitWindow>,
}

#[derive(Deserialize)]
struct BackendRateLimitWindow {
    used_percent: Option<f64>,
    limit_window_seconds: Option<f64>,
    reset_at: Option<Value>,
}

#[derive(Deserialize)]
struct BackendRateLimitResetCredits {
    available_count: Option<Value>,
    total_earned_count: Option<Value>,
    credits: Option<Vec<BackendResetCredit>>,
}

#[derive(Deserialize)]
struct BackendResetCredit {
    status: Option<String>,
    expires_at: Option<Value>,
}

#[derive(Serialize)]
struct BackendConsumeRateLimitResetCreditRequest<'a> {
    redeem_request_id: &'a str,
}

#[derive(Deserialize)]
struct BackendConsumeRateLimitResetCreditResponse {
    code: Option<String>,
}

pub(super) async fn consume_codex_rate_limit_reset_credit(
    codex_home: &Path,
    endpoints: &CodexBackendEndpoints,
    timeout: Duration,
    request_id: &str,
) -> Result<CodexRateLimitResetOutcome, ProviderUsageCommandError> {
    match tokio::time::timeout(
        timeout,
        consume_codex_rate_limit_reset_credit_operation(codex_home, endpoints, timeout, request_id),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ProviderUsageCommandError::new(
            "Codex reset request timed out.",
        )),
    }
}

async fn consume_codex_rate_limit_reset_credit_operation(
    codex_home: &Path,
    endpoints: &CodexBackendEndpoints,
    timeout: Duration,
    request_id: &str,
) -> Result<CodexRateLimitResetOutcome, ProviderUsageCommandError> {
    let auth = read_auth(codex_home).await.ok_or_else(|| {
        ProviderUsageCommandError::new("Codex backend credentials are unavailable.")
    })?;
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| ProviderUsageCommandError::new("Codex reset client is unavailable."))?;
    let response = authenticated_post(&client, &endpoints.consume_reset_credit, &auth)
        .json(&BackendConsumeRateLimitResetCreditRequest {
            redeem_request_id: request_id,
        })
        .send()
        .await
        .map_err(|_| ProviderUsageCommandError::new("Codex reset request failed."))?;
    if !response.status().is_success() {
        return Err(ProviderUsageCommandError::new(format!(
            "Codex reset request failed with HTTP {}.",
            response.status().as_u16()
        )));
    }
    let payload = response
        .json::<BackendConsumeRateLimitResetCreditResponse>()
        .await
        .map_err(|_| ProviderUsageCommandError::new("Codex reset response was incompatible."))?;
    match payload.code.as_deref() {
        Some("reset") => Ok(CodexRateLimitResetOutcome::Reset),
        Some("nothing_to_reset") => Ok(CodexRateLimitResetOutcome::NothingToReset),
        Some("no_credit") => Ok(CodexRateLimitResetOutcome::NoCredit),
        Some("already_redeemed") => Ok(CodexRateLimitResetOutcome::AlreadyRedeemed),
        _ => Err(ProviderUsageCommandError::new(
            "Codex reset response had an unknown outcome.",
        )),
    }
}

pub(super) async fn fetch_codex_backend_usage(
    codex_home: &Path,
    endpoints: &CodexBackendEndpoints,
    timeout: Duration,
) -> CodexBackendFetch {
    fetch_codex_backend_usage_with_auth_reader(
        codex_home,
        endpoints,
        timeout,
        production_auth_reader(),
    )
    .await
}

async fn fetch_codex_backend_usage_with_auth_reader(
    codex_home: &Path,
    endpoints: &CodexBackendEndpoints,
    timeout: Duration,
    auth_reader: AuthReader,
) -> CodexBackendFetch {
    match tokio::time::timeout(
        timeout,
        fetch_codex_backend_usage_operation(codex_home, endpoints, timeout, auth_reader),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => fallback(
            CodexBackendFallbackKind::Unavailable,
            "Codex backend usage request timed out.",
        ),
    }
}

async fn fetch_codex_backend_usage_operation(
    codex_home: &Path,
    endpoints: &CodexBackendEndpoints,
    timeout: Duration,
    auth_reader: AuthReader,
) -> CodexBackendFetch {
    let auth = match read_auth_with_reader(codex_home, auth_reader).await {
        Some(auth) => auth,
        None => {
            return fallback(
                CodexBackendFallbackKind::Credentials,
                "Codex backend credentials are unavailable.",
            );
        }
    };
    let client = match Client::builder().timeout(timeout).build() {
        Ok(client) => client,
        Err(_) => {
            return fallback(
                CodexBackendFallbackKind::Unavailable,
                "Codex backend client is unavailable.",
            );
        }
    };
    let response = match authenticated_get(&client, &endpoints.usage, &auth)
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => {
            return fallback(
                CodexBackendFallbackKind::Unavailable,
                "Codex backend usage request failed.",
            );
        }
    };
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return fallback(
            CodexBackendFallbackKind::Authorization,
            "Codex backend authorization was rejected.",
        );
    }
    if !response.status().is_success() {
        return fallback(
            CodexBackendFallbackKind::Unavailable,
            "Codex backend usage request was unsuccessful.",
        );
    }
    let payload = match response.json::<BackendUsageResponse>().await {
        Ok(payload) => payload,
        Err(_) => {
            return fallback(
                CodexBackendFallbackKind::Incompatible,
                "Codex backend usage response was incompatible.",
            );
        }
    };
    let plan_type = match payload.plan_type.map(|plan| plan.trim().to_owned()) {
        Some(plan) if !plan.is_empty() => plan,
        _ => {
            return fallback(
                CodexBackendFallbackKind::Incompatible,
                "Codex backend usage response was missing required fields.",
            );
        }
    };
    let rate_limit = payload.rate_limit;
    let session = rate_limit
        .as_ref()
        .and_then(|limits| map_window(limits.primary_window.as_ref(), 300));
    let weekly = rate_limit
        .as_ref()
        .and_then(|limits| map_window(limits.secondary_window.as_ref(), 10_080));
    let mut credits = payload
        .rate_limit_reset_credits
        .as_ref()
        .and_then(map_reset_credits);
    if credits.as_ref().is_none_or(|credits| {
        credits.total_earned_count.is_none() || credits.next_expires_at.is_none()
    }) && let Some(supplemental) = fetch_reset_credits(&client, endpoints, &auth).await
    {
        credits = Some(merge_reset_credits(credits, supplemental));
    }

    CodexBackendFetch::Success(CodexBackendUsage {
        session,
        weekly,
        plan_type,
        rate_limit_reset_credits: credits,
    })
}

fn fallback(kind: CodexBackendFallbackKind, message: &'static str) -> CodexBackendFetch {
    CodexBackendFetch::Fallback(CodexBackendFallback::new(kind, message))
}

async fn read_auth(codex_home: &Path) -> Option<CodexBackendAuth> {
    read_auth_with_reader(codex_home, production_auth_reader()).await
}

fn production_auth_reader() -> AuthReader {
    PRODUCTION_AUTH_READER
        .get_or_init(|| {
            Arc::new(|path| {
                Box::pin(async move {
                    #[cfg(feature = "hermetic-test-guard")]
                    if !crate::hermetic_guard::credential_path_allowed("codex auth.json", &path) {
                        return Err(());
                    }
                    tokio::fs::read(path).await.map_err(|_| ())
                })
            })
        })
        .clone()
}

async fn read_auth_with_reader(
    codex_home: &Path,
    auth_reader: AuthReader,
) -> Option<CodexBackendAuth> {
    read_auth_with_reader_in_registry(
        codex_home,
        auth_reader,
        AUTH_READ_REGISTRY
            .get_or_init(|| Arc::new(AuthReadRegistry::default()))
            .clone(),
    )
    .await
}

async fn read_auth_with_reader_in_registry(
    codex_home: &Path,
    auth_reader: AuthReader,
    registry: Arc<AuthReadRegistry>,
) -> Option<CodexBackendAuth> {
    let mut read =
        shared_auth_read_with_registry(codex_home.join("auth.json"), auth_reader, registry);
    let bytes = loop {
        match read.receiver.borrow().clone() {
            Some(AuthReadOutcome::Bytes(bytes)) => break bytes,
            Some(AuthReadOutcome::Failed) => return None,
            None => {}
        }
        if read.receiver.changed().await.is_err() {
            return None;
        }
    };
    let auth: CodexAuthFile = serde_json::from_slice(bytes.as_slice()).ok()?;
    let tokens = auth.tokens?;
    let access_token = non_empty(tokens.access_token?)?;
    let account_id = tokens.account_id.and_then(non_empty);
    Some(CodexBackendAuth {
        access_token,
        account_id,
    })
}

fn shared_auth_read_with_registry(
    path: PathBuf,
    auth_reader: AuthReader,
    registry: Arc<AuthReadRegistry>,
) -> SharedAuthRead {
    let mut reads = registry
        .reads
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = reads.get(&path) {
        return existing.clone();
    }

    let id = registry.next_id.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = tokio::sync::watch::channel(None);
    let shared = SharedAuthRead { id, receiver };
    reads.insert(path.clone(), shared.clone());
    drop(reads);

    let owner = AuthReadOwner {
        registry,
        path: path.clone(),
        id,
        sender: Some(sender),
    };

    tokio::spawn(async move {
        let outcome = match auth_reader(path.clone()).await {
            Ok(bytes) => AuthReadOutcome::Bytes(Arc::new(bytes)),
            Err(()) => AuthReadOutcome::Failed,
        };
        owner.publish(outcome);
    });

    shared
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn authenticated_get(
    client: &Client,
    url: &str,
    auth: &CodexBackendAuth,
) -> reqwest::RequestBuilder {
    authenticated_request(client.get(url), auth)
}

fn authenticated_post(
    client: &Client,
    url: &str,
    auth: &CodexBackendAuth,
) -> reqwest::RequestBuilder {
    authenticated_request(client.post(url), auth)
}

fn authenticated_request(
    request: reqwest::RequestBuilder,
    auth: &CodexBackendAuth,
) -> reqwest::RequestBuilder {
    let request = request
        .bearer_auth(&auth.access_token)
        .header(reqwest::header::USER_AGENT, "codex-cli")
        .header("OpenAI-Beta", "codex-1")
        .header("originator", "Codex Desktop");
    match &auth.account_id {
        Some(account_id) => request.header("ChatGPT-Account-Id", account_id),
        None => request,
    }
}

fn map_window(
    raw: Option<&BackendRateLimitWindow>,
    fallback_window_minutes: u32,
) -> Option<ProviderUsageWindow> {
    let raw = raw?;
    let used_percent = raw.used_percent.filter(|value| value.is_finite())?;
    let window_minutes = raw
        .limit_window_seconds
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| (seconds / 60.0).ceil().clamp(1.0, f64::from(u32::MAX)) as u32)
        .unwrap_or(fallback_window_minutes);
    Some(ProviderUsageWindow {
        used_percent: used_percent.clamp(0.0, 100.0).round() as u32,
        window_minutes,
        resets_at: raw.reset_at.as_ref().and_then(parse_reset),
        reset_description: None,
    })
}

fn map_reset_credits(raw: &BackendRateLimitResetCredits) -> Option<RateLimitResetCredits> {
    let available_from_aggregate = raw.available_count.as_ref().and_then(map_count);
    let available_from_entries = raw.credits.as_ref().map(|credits| {
        credits
            .iter()
            .filter(|credit| is_available(credit.status.as_deref()))
            .count()
            .min(u32::MAX as usize) as u32
    });
    let available_count = available_from_aggregate.or(available_from_entries)?;
    let next_expires_at = raw
        .credits
        .iter()
        .flatten()
        .filter(|credit| is_available(credit.status.as_deref()))
        .filter_map(|credit| credit.expires_at.as_ref().and_then(parse_reset))
        .min();
    Some(RateLimitResetCredits {
        available_count,
        total_earned_count: raw.total_earned_count.as_ref().and_then(map_count),
        next_expires_at,
    })
}

fn map_count(value: &Value) -> Option<u32> {
    let value = value.as_f64().filter(|value| value.is_finite())?;
    Some(value.floor().clamp(0.0, f64::from(u32::MAX)) as u32)
}

fn is_available(status: Option<&str>) -> bool {
    status.is_some_and(|status| status.trim().eq_ignore_ascii_case("available"))
}

async fn fetch_reset_credits(
    client: &Client,
    endpoints: &CodexBackendEndpoints,
    auth: &CodexBackendAuth,
) -> Option<RateLimitResetCredits> {
    let response = authenticated_get(client, &endpoints.reset_credits, auth)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let payload = response.json::<BackendRateLimitResetCredits>().await.ok()?;
    map_reset_credits(&payload)
}

fn merge_reset_credits(
    primary: Option<RateLimitResetCredits>,
    supplemental: RateLimitResetCredits,
) -> RateLimitResetCredits {
    match primary {
        Some(primary) => RateLimitResetCredits {
            available_count: primary.available_count,
            total_earned_count: primary
                .total_earned_count
                .or(supplemental.total_earned_count),
            next_expires_at: primary.next_expires_at.or(supplemental.next_expires_at),
        },
        None => supplemental,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "hermetic-test-guard")]
    #[tokio::test]
    async fn hermetic_guard_refuses_outside_codex_auth_before_reading() {
        use crate::test_support::TestSandbox;
        const CASE: &str = "guard-codex-auth";
        const TEST: &str = "provider_usage::codex_backend::tests::hermetic_guard_refuses_outside_codex_auth_before_reading";
        let sandbox = TestSandbox::new("guard-codex-auth");
        let raw = r#"{"tokens":{"access_token":"fixture-token","account_id":"fixture-account"}}"#;
        if TestSandbox::is_isolated_case(CASE, TEST) {
            let outside =
                PathBuf::from(std::env::var_os("CODDY_STUDIO_GUARD_CREDENTIAL_HOME").unwrap());
            assert!(read_auth(&outside).await.is_none());
            std::fs::write(sandbox.path("auth.json"), raw).unwrap();
            assert_eq!(
                read_auth(sandbox.root()).await.unwrap().access_token,
                "fixture-token"
            );
            return;
        }
        let allowed_root = sandbox.path("allowed");
        std::fs::create_dir(&allowed_root).unwrap();
        std::fs::write(sandbox.path("auth.json"), raw).unwrap();
        let output = sandbox.run_isolated_case(
            CASE,
            TEST,
            &[
                ("TMPDIR", allowed_root.as_os_str()),
                ("TMP", allowed_root.as_os_str()),
                ("TEMP", allowed_root.as_os_str()),
                (
                    "CODDY_STUDIO_GUARD_CREDENTIAL_HOME",
                    sandbox.root().as_os_str(),
                ),
                (
                    "CODDY_STUDIO_HERMETIC_GUARD",
                    std::ffi::OsStr::new("report"),
                ),
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("hermetic-test-guard: refused codex auth.json")
        );
    }

    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        body::Bytes,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Json},
        routing::{get, post},
    };
    use serde_json::json;

    use super::*;

    async fn spawn_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback server");
        let address = listener.local_addr().expect("loopback address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve loopback");
        });
        (format!("http://{address}/backend-api/wham"), server)
    }

    fn write_auth(directory: &std::path::Path, account_id: Option<&str>) {
        std::fs::write(
            directory.join("auth.json"),
            json!({
                "tokens": {
                    "access_token": "private-access-token",
                    "account_id": account_id
                }
            })
            .to_string(),
        )
        .expect("write auth fixture");
    }

    #[tokio::test]
    async fn consume_codex_rate_limit_reset_posts_exact_json_and_maps_reset() {
        let observed_body = Arc::new(Mutex::new(None::<Bytes>));
        let app = Router::new().route(
            "/backend-api/wham/rate-limit-reset-credits/consume",
            post({
                let observed_body = observed_body.clone();
                move |body: Bytes| {
                    let observed_body = observed_body.clone();
                    async move {
                        *observed_body.lock().expect("body lock") = Some(body);
                        Json(json!({"code": "reset"}))
                    }
                }
            }),
        );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), Some("private-account-id"));

        let outcome = consume_codex_rate_limit_reset_credit(
            temporary.path(),
            &CodexBackendEndpoints::for_base(&base),
            Duration::from_secs(1),
            "request-123",
        )
        .await
        .expect("known reset outcome");
        server.abort();

        assert_eq!(outcome, CodexRateLimitResetOutcome::Reset);
        assert_eq!(
            observed_body.lock().expect("body lock").as_deref(),
            Some(br#"{"redeem_request_id":"request-123"}"#.as_slice())
        );
    }

    #[tokio::test]
    async fn consume_codex_rate_limit_reset_maps_every_known_outcome() {
        let cases = [
            (
                "nothing_to_reset",
                CodexRateLimitResetOutcome::NothingToReset,
            ),
            ("no_credit", CodexRateLimitResetOutcome::NoCredit),
            (
                "already_redeemed",
                CodexRateLimitResetOutcome::AlreadyRedeemed,
            ),
        ];

        for (code, expected) in cases {
            let app = Router::new().route(
                "/backend-api/wham/rate-limit-reset-credits/consume",
                post(move || async move { Json(json!({"code": code})) }),
            );
            let (base, server) = spawn_server(app).await;
            let temporary = tempfile::tempdir().expect("temporary Codex home");
            write_auth(temporary.path(), None);

            let outcome = consume_codex_rate_limit_reset_credit(
                temporary.path(),
                &CodexBackendEndpoints::for_base(&base),
                Duration::from_secs(1),
                "request-123",
            )
            .await
            .expect("known reset outcome");
            server.abort();

            assert_eq!(outcome, expected, "{code}");
        }
    }

    #[tokio::test]
    async fn consume_codex_rate_limit_reset_rejects_unknown_and_http_failures_safely() {
        let cases = [
            (StatusCode::OK, r#"{"code":"future_outcome"}"#),
            (StatusCode::SERVICE_UNAVAILABLE, "private-upstream-detail"),
        ];

        for (status, body) in cases {
            let app = Router::new().route(
                "/backend-api/wham/rate-limit-reset-credits/consume",
                post(move || async move { (status, body).into_response() }),
            );
            let (base, server) = spawn_server(app).await;
            let temporary = tempfile::tempdir().expect("temporary Codex home");
            write_auth(temporary.path(), Some("private-account-id"));

            let error = consume_codex_rate_limit_reset_credit(
                temporary.path(),
                &CodexBackendEndpoints::for_base(&base),
                Duration::from_secs(1),
                "request-123",
            )
            .await
            .expect_err("unknown or unsuccessful response");
            server.abort();

            let formatted = format!("{error:?}");
            assert!(!formatted.contains("private-access-token"));
            assert!(!formatted.contains("private-account-id"));
            assert!(!formatted.contains("private-upstream-detail"));
        }
    }

    #[tokio::test]
    async fn authenticated_usage_maps_windows_plan_and_inline_credits() {
        let observed_headers = Arc::new(Mutex::new(None::<HeaderMap>));
        let headers = observed_headers.clone();
        let app = Router::new().route(
            "/backend-api/wham/usage",
            get(move |request_headers: HeaderMap| {
                let headers = headers.clone();
                async move {
                    *headers.lock().expect("headers lock") = Some(request_headers);
                    Json(json!({
                        "plan_type": "  Plus  ",
                        "rate_limit": {
                            "primary_window": {
                                "used_percent": 12.4,
                                "limit_window_seconds": 3_600,
                                "reset_at": 1_800_000_000
                            },
                            "secondary_window": {
                                "used_percent": 34.6,
                                "limit_window_seconds": 604_800,
                                "reset_at": 1_800_100_000
                            }
                        },
                        "rate_limit_reset_credits": {
                            "available_count": 2,
                            "total_earned_count": 5,
                            "credits": [{
                                "status": "available",
                                "expires_at": "2027-01-15T12:00:00Z"
                            }]
                        }
                    }))
                }
            }),
        );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), Some("private-account-id"));

        let result = fetch_codex_backend_usage(
            temporary.path(),
            &CodexBackendEndpoints::for_base(&base),
            Duration::from_secs(1),
        )
        .await;
        server.abort();

        let usage = match result {
            CodexBackendFetch::Success(usage) => usage,
            CodexBackendFetch::Fallback(reason) => panic!("unexpected fallback: {reason}"),
        };
        assert_eq!(usage.plan_type, "Plus");
        let session = usage.session.expect("primary window");
        assert_eq!(session.used_percent, 12);
        assert_eq!(session.window_minutes, 60);
        assert_eq!(
            session.resets_at.expect("primary reset").unix_timestamp(),
            1_800_000_000
        );
        let weekly = usage.weekly.expect("secondary window");
        assert_eq!(weekly.used_percent, 35);
        assert_eq!(weekly.window_minutes, 10_080);
        assert_eq!(
            weekly.resets_at.expect("secondary reset").unix_timestamp(),
            1_800_100_000
        );
        let credits = usage.rate_limit_reset_credits.expect("reset credits");
        assert_eq!(credits.available_count, 2);
        assert_eq!(credits.total_earned_count, Some(5));
        assert_eq!(
            credits
                .next_expires_at
                .expect("available credit expiry")
                .unix_timestamp(),
            1_800_014_400
        );

        let headers = observed_headers
            .lock()
            .expect("headers lock")
            .take()
            .expect("observed headers");
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer private-access-token")
        );
        assert_eq!(
            headers
                .get("chatgpt-account-id")
                .and_then(|value| value.to_str().ok()),
            Some("private-account-id")
        );
        assert_eq!(
            headers
                .get("user-agent")
                .and_then(|value| value.to_str().ok()),
            Some("codex-cli")
        );
        assert_eq!(
            headers
                .get("openai-beta")
                .and_then(|value| value.to_str().ok()),
            Some("codex-1")
        );
        assert_eq!(
            headers
                .get("originator")
                .and_then(|value| value.to_str().ok()),
            Some("Codex Desktop")
        );
    }

    #[tokio::test]
    async fn usage_without_credit_fields_succeeds_when_supplemental_endpoint_is_missing() {
        let app = Router::new().route(
            "/backend-api/wham/usage",
            get(|| async {
                Json(json!({
                    "plan_type": "Plus",
                    "rate_limit": {
                        "primary_window": {
                            "used_percent": 22,
                            "limit_window_seconds": 18_000,
                            "reset_at": 1_800_000_000
                        }
                    }
                }))
            }),
        );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), None);

        let result = fetch_codex_backend_usage(
            temporary.path(),
            &CodexBackendEndpoints::for_base(&base),
            Duration::from_secs(1),
        )
        .await;
        server.abort();

        let usage = match result {
            CodexBackendFetch::Success(usage) => usage,
            CodexBackendFetch::Fallback(reason) => panic!("unexpected fallback: {reason}"),
        };
        assert_eq!(usage.plan_type, "Plus");
        assert_eq!(usage.session.expect("primary window").used_percent, 22);
        assert!(usage.weekly.is_none());
        assert!(usage.rate_limit_reset_credits.is_none());
    }

    #[tokio::test]
    async fn missing_account_header_and_expiry_trigger_supplemental_credit_merge() {
        let usage_headers = Arc::new(Mutex::new(None::<HeaderMap>));
        let credit_headers = Arc::new(Mutex::new(None::<HeaderMap>));
        let app = Router::new()
            .route(
                "/backend-api/wham/usage",
                get({
                    let usage_headers = usage_headers.clone();
                    move |headers: HeaderMap| {
                        let usage_headers = usage_headers.clone();
                        async move {
                            *usage_headers.lock().expect("headers lock") = Some(headers);
                            Json(json!({
                                "plan_type": "pro",
                                "rate_limit": {
                                    "primary_window": {"used_percent": 7}
                                },
                                "rate_limit_reset_credits": {
                                    "available_count": 3,
                                    "total_earned_count": 9
                                }
                            }))
                        }
                    }
                }),
            )
            .route(
                "/backend-api/wham/rate-limit-reset-credits",
                get({
                    let credit_headers = credit_headers.clone();
                    move |headers: HeaderMap| {
                        let credit_headers = credit_headers.clone();
                        async move {
                            *credit_headers.lock().expect("headers lock") = Some(headers);
                            Json(json!({
                                "available_count": 4,
                                "total_earned_count": 10,
                                "credits": [{
                                    "status": "AVAILABLE",
                                    "expires_at": "2027-02-01T00:00:00Z"
                                }]
                            }))
                        }
                    }
                }),
            );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), None);

        let result = fetch_codex_backend_usage(
            temporary.path(),
            &CodexBackendEndpoints::for_base(&base),
            Duration::from_secs(1),
        )
        .await;
        server.abort();

        let usage = match result {
            CodexBackendFetch::Success(usage) => usage,
            CodexBackendFetch::Fallback(reason) => panic!("unexpected fallback: {reason}"),
        };
        let credits = usage.rate_limit_reset_credits.expect("merged credits");
        assert_eq!(credits.available_count, 3);
        assert_eq!(credits.total_earned_count, Some(9));
        assert_eq!(
            credits
                .next_expires_at
                .expect("supplemental expiry")
                .unix_timestamp(),
            1_801_440_000
        );
        for headers in [&usage_headers, &credit_headers] {
            let headers = headers.lock().expect("headers lock");
            let headers = headers.as_ref().expect("observed request");
            assert!(headers.get("chatgpt-account-id").is_none());
            assert_eq!(
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer private-access-token")
            );
        }
    }

    #[tokio::test]
    async fn supplemental_credits_fill_missing_total_without_replacing_inline_values() {
        let supplemental_headers = Arc::new(Mutex::new(None::<HeaderMap>));
        let app = Router::new()
            .route(
                "/backend-api/wham/usage",
                get(|| async {
                    Json(json!({
                        "plan_type": "plus",
                        "rate_limit": {
                            "primary_window": {"used_percent": 8}
                        },
                        "rate_limit_reset_credits": {
                            "available_count": 3,
                            "credits": [{
                                "status": "available",
                                "expires_at": "2027-01-15T12:00:00Z"
                            }]
                        }
                    }))
                }),
            )
            .route(
                "/backend-api/wham/rate-limit-reset-credits",
                get({
                    let supplemental_headers = supplemental_headers.clone();
                    move |headers: HeaderMap| {
                        let supplemental_headers = supplemental_headers.clone();
                        async move {
                            *supplemental_headers.lock().expect("headers lock") = Some(headers);
                            Json(json!({
                                "available_count": 4,
                                "total_earned_count": 10,
                                "credits": [{
                                    "status": "available",
                                    "expires_at": "2027-02-01T00:00:00Z"
                                }]
                            }))
                        }
                    }
                }),
            );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), Some("private-account-id"));

        let result = fetch_codex_backend_usage(
            temporary.path(),
            &CodexBackendEndpoints::for_base(&base),
            Duration::from_secs(1),
        )
        .await;
        server.abort();

        let usage = match result {
            CodexBackendFetch::Success(usage) => usage,
            CodexBackendFetch::Fallback(reason) => panic!("unexpected fallback: {reason}"),
        };
        let credits = usage.rate_limit_reset_credits.expect("merged credits");
        assert_eq!(credits.available_count, 3);
        assert_eq!(credits.total_earned_count, Some(10));
        assert_eq!(
            credits
                .next_expires_at
                .expect("preserved inline expiry")
                .unix_timestamp(),
            1_800_014_400
        );
        assert!(
            supplemental_headers.lock().expect("headers lock").is_some(),
            "supplemental credits endpoint was not called"
        );
    }

    async fn fallback_for_response(status: StatusCode, body: &'static str) -> CodexBackendFallback {
        let app = Router::new().route(
            "/backend-api/wham/usage",
            get(move || async move { (status, body).into_response() }),
        );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), Some("private-account-id"));
        let result = fetch_codex_backend_usage(
            temporary.path(),
            &CodexBackendEndpoints::for_base(&base),
            Duration::from_secs(1),
        )
        .await;
        server.abort();
        match result {
            CodexBackendFetch::Success(_) => panic!("expected backend fallback"),
            CodexBackendFetch::Fallback(reason) => reason,
        }
    }

    #[tokio::test]
    async fn authorization_malformed_json_and_missing_plan_are_safe_fallbacks() {
        let cases = [
            (
                StatusCode::UNAUTHORIZED,
                "{}",
                CodexBackendFallbackKind::Authorization,
            ),
            (
                StatusCode::OK,
                "{malformed",
                CodexBackendFallbackKind::Incompatible,
            ),
            (
                StatusCode::OK,
                r#"{"rate_limit":{"primary_window":{"used_percent":1}}}"#,
                CodexBackendFallbackKind::Incompatible,
            ),
        ];

        for (status, body, expected_kind) in cases {
            let fallback = fallback_for_response(status, body).await;
            assert_eq!(fallback.kind, expected_kind);
            for formatted in [fallback.to_string(), format!("{fallback:?}")] {
                assert!(!formatted.contains("private-access-token"));
                assert!(!formatted.contains("private-account-id"));
            }
        }
    }

    #[tokio::test]
    async fn stalled_usage_request_is_bounded_and_falls_back() {
        let app = Router::new().route(
            "/backend-api/wham/usage",
            get(|| async {
                std::future::pending::<()>().await;
                Json(json!({}))
            }),
        );
        let (base, server) = spawn_server(app).await;
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        write_auth(temporary.path(), None);

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            fetch_codex_backend_usage(
                temporary.path(),
                &CodexBackendEndpoints::for_base(&base),
                Duration::from_millis(20),
            ),
        )
        .await
        .expect("adapter timeout bound");
        server.abort();

        let fallback = match result {
            CodexBackendFetch::Success(_) => panic!("expected timeout fallback"),
            CodexBackendFetch::Fallback(reason) => reason,
        };
        assert_eq!(fallback.kind, CodexBackendFallbackKind::Unavailable);
    }

    #[tokio::test]
    async fn stalled_credential_reads_share_one_operation_inside_the_backend_deadline() {
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let reader: AuthReader = Arc::new({
            let calls = calls.clone();
            let release = release.clone();
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).expect("signal credential read");
                let release = release.clone();
                Box::pin(async move {
                    release.notified().await;
                    Ok(Vec::new())
                })
            }
        });
        let endpoints = CodexBackendEndpoints::for_base("http://127.0.0.1:9");
        let timeout = Duration::from_millis(20);

        let mut first = tokio::spawn({
            let home = temporary.path().to_owned();
            let endpoints = endpoints.clone();
            let reader = reader.clone();
            async move {
                fetch_codex_backend_usage_with_auth_reader(&home, &endpoints, timeout, reader).await
            }
        });
        started_rx
            .recv()
            .await
            .expect("first credential read started");
        let mut second = tokio::spawn({
            let home = temporary.path().to_owned();
            let endpoints = endpoints.clone();
            async move {
                fetch_codex_backend_usage_with_auth_reader(&home, &endpoints, timeout, reader).await
            }
        });

        let (first_result, second_result) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(250), &mut first),
            tokio::time::timeout(Duration::from_millis(250), &mut second),
        );
        release.notify_waiters();
        if first_result.is_err() {
            first.abort();
        }
        if second_result.is_err() {
            second.abort();
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for result in [first_result, second_result] {
            let result = result.expect("backend deadline").expect("fetch task");
            let fallback = match result {
                CodexBackendFetch::Success(_) => panic!("expected credential timeout fallback"),
                CodexBackendFetch::Fallback(fallback) => fallback,
            };
            assert_eq!(fallback.kind, CodexBackendFallbackKind::Unavailable);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn auth_read_generation_slot_is_reusable_after_owner_abort() {
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        let auth_path = temporary.path().join("auth.json");
        let registry = Arc::new(AuthReadRegistry::default());
        let started = Arc::new(tokio::sync::Semaphore::new(0));
        let stalled_reader: AuthReader = Arc::new({
            let started = started.clone();
            move |_| {
                let started = started.clone();
                Box::pin(async move {
                    started.add_permits(1);
                    std::future::pending::<Result<Vec<u8>, ()>>().await
                })
            }
        });
        let (drop_runtime, drop_runtime_receiver) = tokio::sync::oneshot::channel();
        let owner = std::thread::spawn({
            let auth_path = auth_path.clone();
            let registry = registry.clone();
            move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("auth-read owner runtime");
                runtime.block_on(async move {
                    let _read = shared_auth_read_with_registry(auth_path, stalled_reader, registry);
                    drop_runtime_receiver
                        .await
                        .expect("runtime drop release event");
                });
            }
        });
        started
            .acquire()
            .await
            .expect("auth read start event")
            .forget();

        drop_runtime.send(()).expect("drop auth-read owner runtime");
        owner.join().expect("auth-read owner thread");

        let replacement_calls = Arc::new(AtomicUsize::new(0));
        let replacement_reader: AuthReader = Arc::new({
            let replacement_calls = replacement_calls.clone();
            move |_| {
                replacement_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(br#"{"tokens":{"access_token":"replacement-token"}}"#.to_vec())
                })
            }
        });
        let replacement =
            read_auth_with_reader_in_registry(temporary.path(), replacement_reader, registry)
                .await
                .expect("replacement auth read");

        assert_eq!(replacement.access_token, "replacement-token");
        assert_eq!(replacement_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn auth_read_generation_slot_is_reusable_after_reader_panic() {
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        let registry = Arc::new(AuthReadRegistry::default());
        let panicked_reader: AuthReader = Arc::new(|_| {
            Box::pin(async {
                panic!("injected Codex auth reader panic");
            })
        });
        assert!(
            read_auth_with_reader_in_registry(temporary.path(), panicked_reader, registry.clone(),)
                .await
                .is_none()
        );

        let replacement_calls = Arc::new(AtomicUsize::new(0));
        let replacement_reader: AuthReader = Arc::new({
            let replacement_calls = replacement_calls.clone();
            move |_| {
                replacement_calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Ok(br#"{"tokens":{"access_token":"replacement-token"}}"#.to_vec())
                })
            }
        });
        let replacement =
            read_auth_with_reader_in_registry(temporary.path(), replacement_reader, registry)
                .await
                .expect("replacement auth read after panic");

        assert_eq!(replacement.access_token, "replacement-token");
        assert_eq!(replacement_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reset_redemption_deadline_includes_a_shared_stalled_credential_read() {
        let temporary = tempfile::tempdir().expect("temporary Codex home");
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let reader: AuthReader = Arc::new({
            let calls = calls.clone();
            let release = release.clone();
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                let release = release.clone();
                Box::pin(async move {
                    release.notified().await;
                    Ok(Vec::new())
                })
            }
        });
        let endpoints = CodexBackendEndpoints::for_base("http://127.0.0.1:9");
        let timeout = Duration::from_millis(20);

        let usage = fetch_codex_backend_usage_with_auth_reader(
            temporary.path(),
            &endpoints,
            timeout,
            reader,
        )
        .await;
        assert!(matches!(usage, CodexBackendFetch::Fallback(_)));
        let reset = tokio::time::timeout(
            Duration::from_millis(250),
            consume_codex_rate_limit_reset_credit(
                temporary.path(),
                &endpoints,
                timeout,
                "request-123",
            ),
        )
        .await;
        release.notify_waiters();

        let error = reset
            .expect("reset adapter deadline")
            .expect_err("stalled credential reset");
        assert_eq!(error.message, "Codex reset request timed out.");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
