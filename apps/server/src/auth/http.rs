use std::time::{Duration, UNIX_EPOCH};

use axum::{
    Form, Json, Router,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use uuid::Uuid;

use super::{
    model::{
        ACCESS_TOKEN_TYPE, BOOTSTRAP_TOKEN_TYPE, BrowserSessionRequest, BrowserSessionResult,
        CancelPairingOfferRequest, ClientMetadata, CreatePairingOfferRequest, CreatePairingRequest,
        PairingOfferResult, RevokeClientRequest, RevokePairingRequest, SCOPE_ACCESS_READ,
        SCOPE_ACCESS_WRITE, TOKEN_GRANT_TYPE, TokenExchangeRequest, WebSocketTicketResult,
    },
    pairing_code::{
        REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload, RemotePairingReach,
        encode_pairing_code,
    },
    pairing_offer::validate_pairing_offer_input,
    service::{
        AuthError, AuthService, PairingOfferIssuance, PairingOfferReplay, PairingOfferReservation,
        default_standard_scopes, format_iso, now_ms, parse_scopes,
    },
};
use crate::http::AppState;

const CREDENTIAL_HEADERS: [(&str, &str); 2] =
    [("cache-control", "no-store"), ("pragma", "no-cache")];
const MAX_PAIRING_OFFER_IDEMPOTENCY_KEY_BYTES: usize = 128;

pub(crate) fn add_routes(router: Router<AppState>) -> Router<AppState> {
    router
        .route("/api/auth/session", get(session))
        .route("/api/auth/browser-session", post(browser_session))
        .route("/oauth/token", post(token))
        .route("/api/auth/websocket-ticket", post(websocket_ticket))
        .route("/api/auth/pairing-token", post(pairing_token))
        .route("/api/auth/pairing-offer", post(create_pairing_offer))
        .route("/api/auth/pairing-offer/cancel", post(cancel_pairing_offer))
        .route("/api/auth/share-state", get(share_state))
        .route("/api/auth/pairing-links", get(pairing_links))
        .route("/api/auth/pairing-links/revoke", post(revoke_pairing_link))
        .route("/api/auth/clients", get(clients))
        .route("/api/auth/clients/revoke", post(revoke_client))
        .route(
            "/api/auth/clients/revoke-others",
            post(revoke_other_clients),
        )
}

async fn session(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    let auth = &state.auth;
    let descriptor = auth.descriptor();
    match authenticate_request(auth, &headers, &uri).await {
        Ok(principal) => Json(json!({
            "authenticated": true,
            "auth": descriptor,
            "scopes": principal.scopes,
            "sessionMethod": principal.method,
            "expiresAt": format_iso(principal.expires_at_ms),
        }))
        .into_response(),
        Err(AuthError::MissingCredential | AuthError::InvalidCredential) => Json(json!({
            "authenticated": false,
            "auth": descriptor,
        }))
        .into_response(),
        Err(error) => HttpAuthError::new(error, "internal_error").into_response(),
    }
}

async fn browser_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<BrowserSessionRequest>,
) -> Response {
    let response = match state
        .auth
        .create_browser_session(
            payload.credential.trim(),
            client_metadata(&headers, None),
            super::SessionTransport::Plain,
        )
        .await
    {
        Ok(issued) => {
            let expires = UNIX_EPOCH
                + Duration::from_millis(
                    u64::try_from(issued.principal.expires_at_ms).unwrap_or_default(),
                );
            let cookie = format!(
                "{}={}; Path=/; Expires={}; HttpOnly; SameSite=Lax",
                state.auth.cookie_name(),
                issued.token,
                httpdate::fmt_http_date(expires)
            );
            let result = BrowserSessionResult {
                authenticated: true,
                scopes: issued.principal.scopes,
                session_method: issued.principal.method,
                expires_at: format_iso(issued.principal.expires_at_ms),
            };
            let mut response = Json(result).into_response();
            if let Ok(value) = header::HeaderValue::from_str(&cookie) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        Err(error) => HttpAuthError::new(error, "browser_session_issuance_failed").into_response(),
    };
    credential_response(response)
}

async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Form(payload): Form<TokenExchangeRequest>,
) -> Response {
    let uses_dpop = headers.contains_key("dpop");
    let response = token_inner(&state.auth, &headers, &uri, payload)
        .await
        .unwrap_or_else(|error| {
            HttpAuthError::new(error, "access_token_issuance_failed").into_response()
        });
    let response = credential_response(response);
    if uses_dpop {
        dpop_challenge_on_unauthorized(response)
    } else {
        response
    }
}

async fn token_inner(
    auth: &AuthService,
    headers: &HeaderMap,
    uri: &Uri,
    payload: TokenExchangeRequest,
) -> Result<Response, AuthError> {
    if payload.grant_type != TOKEN_GRANT_TYPE
        || payload.subject_token_type != BOOTSTRAP_TOKEN_TYPE
        || payload.requested_token_type != ACCESS_TOKEN_TYPE
        || payload.subject_token.trim().is_empty()
    {
        return Err(AuthError::InvalidCredential);
    }
    let requested_scopes = payload.scope.as_deref().map(parse_scopes).transpose()?;
    if payload
        .client_device_type
        .as_deref()
        .is_some_and(|value| !is_device_type(value))
        || payload
            .client_label
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        || payload
            .client_os
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
    {
        return Err(AuthError::InvalidScope);
    }
    let presented = ClientMetadata {
        label: non_empty(payload.client_label),
        device_type: payload
            .client_device_type
            .filter(|value| is_device_type(value))
            .unwrap_or_else(|| infer_device_type(headers).to_owned()),
        os: non_empty(payload.client_os).or_else(|| infer_os(headers).map(str::to_owned)),
        ..client_metadata(headers, None)
    };
    let proof_key_thumbprint = match headers.get("dpop") {
        Some(proof) => {
            let proof = proof.to_str().map_err(|_| AuthError::InvalidCredential)?;
            Some(
                auth.verify_dpop(proof, "POST", &request_url(headers, uri)?, None, None)
                    .await?,
            )
        }
        None => None,
    };
    let issued = auth
        .exchange_bootstrap(
            payload.subject_token.trim(),
            requested_scopes,
            presented,
            proof_key_thumbprint,
            super::SessionTransport::Plain,
        )
        .await?;
    let expires_in = ((issued.principal.expires_at_ms - now_ms()) / 1_000).max(0);
    Ok(Json(super::model::AccessTokenResult {
        access_token: issued.token,
        issued_token_type: ACCESS_TOKEN_TYPE,
        token_type: if issued.principal.proof_key_thumbprint.is_some() {
            "DPoP"
        } else {
            "Bearer"
        },
        expires_in,
        scope: issued.principal.scopes.join(" "),
    })
    .into_response())
}

async fn websocket_ticket(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    let response = match authenticate_request(&state.auth, &headers, &uri).await {
        Ok(principal) => match state.auth.issue_websocket_ticket(&principal) {
            Ok((ticket, expires_at)) => Json(WebSocketTicketResult {
                ticket,
                expires_at: format_iso(expires_at),
            })
            .into_response(),
            Err(error) => {
                HttpAuthError::new(error, "websocket_ticket_issuance_failed").into_response()
            }
        },
        Err(error) => {
            let response =
                HttpAuthError::new(error, "websocket_ticket_issuance_failed").into_response();
            if request_uses_dpop(&headers) {
                dpop_challenge_on_unauthorized(response)
            } else {
                response
            }
        }
    };
    credential_response(response)
}

async fn pairing_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<CreatePairingRequest>,
) -> Response {
    let result = async {
        let principal = authenticate_request(&state.auth, &headers, &uri).await?;
        require_scope(&principal, SCOPE_ACCESS_WRITE)?;
        if payload
            .label
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(AuthError::InvalidScope);
        }
        let scopes = payload.scopes.unwrap_or_else(default_standard_scopes);
        if scopes.is_empty()
            || scopes
                .iter()
                .any(|scope| !super::model::ALL_SCOPES.contains(&scope.as_str()))
            || scopes
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != scopes.len()
        {
            return Err(AuthError::InvalidScope);
        }
        for scope in &scopes {
            if !principal.has_scope(scope) {
                return Err(AuthError::ScopeRequired(scope.clone()));
            }
        }
        state
            .auth
            .issue_pairing(scopes, non_empty(payload.label))
            .await
    }
    .await;
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => auth_error_for_request(error, &headers, "pairing_credential_issuance_failed"),
    }
}

async fn create_pairing_offer(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<CreatePairingOfferRequest>,
) -> Response {
    let principal =
        match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_WRITE).await {
            Ok(principal) => principal,
            Err(error) => {
                return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
            }
        };
    let idempotency_key = match headers.get("idempotency-key") {
        Some(value) => {
            let Ok(value) = value.to_str() else {
                return invalid_pairing_offer_response("idempotency key must be valid text");
            };
            let key = value.trim();
            if !is_valid_pairing_offer_idempotency_key(key) {
                return invalid_pairing_offer_response("idempotency key is invalid");
            }
            Some(key.to_owned())
        }
        None => None,
    };
    let input_fingerprint = format!(
        "{}\n{}\n{}\n{:?}\n{:?}",
        payload.name.trim(),
        payload.endpoint.trim(),
        payload.reach,
        payload.label,
        payload.scopes,
    );
    let _offer_guard = if idempotency_key.is_some() {
        Some(state.auth.lock_pairing_offer_issuance().await)
    } else {
        None
    };
    if let Some(key) = &idempotency_key {
        match state
            .auth
            .replay_pairing_offer(&principal.session_id, key, &input_fingerprint)
            .await
        {
            Ok(PairingOfferReplay::Original(original)) => return Json(original).into_response(),
            Ok(PairingOfferReplay::Cancelled) => {
                return invalid_pairing_offer_response("idempotency key was cancelled");
            }
            Ok(PairingOfferReplay::Conflict) => {
                return invalid_pairing_offer_response(
                    "idempotency key was already used with a different input",
                );
            }
            Ok(PairingOfferReplay::Fresh) => {}
            Err(error) => {
                return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
            }
        }
    }

    let validated = match validate_pairing_offer_input(
        &payload.name,
        &payload.endpoint,
        &payload.reach,
        payload.label.as_deref(),
    ) {
        Ok(validated) => validated,
        Err(error) => return invalid_pairing_offer_response(&error.to_string()),
    };
    let name = validated.name.clone();
    let label = validated.label.clone();
    let off_host = validated.off_host;
    let scopes = payload.scopes.unwrap_or_else(default_standard_scopes);
    if scopes.is_empty()
        || scopes
            .iter()
            .any(|scope| !super::model::ALL_SCOPES.contains(&scope.as_str()))
        || scopes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != scopes.len()
        || !scopes
            .iter()
            .all(|scope| principal.scopes.iter().any(|granted| granted == scope))
    {
        return invalid_pairing_offer_response("requested scope exceeds the caller's grant");
    }
    let Some(storage_instance_id) = state.config.storage_instance_id else {
        return auth_error_for_request(
            AuthError::Internal("storage identity is unavailable".to_owned()),
            &headers,
            "pairing_offer_issuance_failed",
        );
    };
    let issued = if let Some(key) = &idempotency_key {
        match state
            .auth
            .issue_share_pairing_offer(
                scopes,
                Some(label),
                payload.reach.clone(),
                off_host,
                PairingOfferReservation::new(
                    principal.session_id.clone(),
                    key.clone(),
                    input_fingerprint.clone(),
                ),
            )
            .await
        {
            Ok(PairingOfferIssuance::Reserved(issued)) => issued,
            Ok(PairingOfferIssuance::Replay(PairingOfferReplay::Original(original))) => {
                return Json(original).into_response();
            }
            Ok(PairingOfferIssuance::Replay(PairingOfferReplay::Cancelled)) => {
                return invalid_pairing_offer_response("idempotency key was cancelled");
            }
            Ok(PairingOfferIssuance::Replay(PairingOfferReplay::Conflict)) => {
                return invalid_pairing_offer_response(
                    "idempotency key was already used with a different input",
                );
            }
            Ok(PairingOfferIssuance::Replay(PairingOfferReplay::Fresh)) => {
                return auth_error_for_request(
                    AuthError::Internal("pairing offer reservation disappeared".to_owned()),
                    &headers,
                    "pairing_offer_issuance_failed",
                );
            }
            Err(error) => {
                return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
            }
        }
    } else {
        match state
            .auth
            .issue_share_pairing(scopes, Some(label), payload.reach.clone(), off_host)
            .await
        {
            Ok(issued) => issued,
            Err(error) => {
                return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
            }
        }
    };
    let code_payload = RemotePairingCodePayload {
        v: REMOTE_PAIRING_CODE_VERSION,
        endpoint: validated.endpoint.clone(),
        name: name.clone(),
        token: issued.credential.clone(),
        host_key: state.auth.host_identity().public_key_base64url(),
        reach: match payload.reach.as_str() {
            "this-computer" => RemotePairingReach::ThisComputer,
            "another-device" => RemotePairingReach::AnotherDevice,
            _ => RemotePairingReach::Custom,
        },
        storage_instance_id: storage_instance_id.to_string(),
    };
    let code = match encode_pairing_code(&code_payload) {
        Ok(code) => code,
        Err(error) => {
            if let Some(key) = &idempotency_key {
                let _ = state
                    .auth
                    .cancel_pairing_offer(&principal.session_id, key.clone())
                    .await;
            } else {
                let _ = state.auth.revoke_pairing(&issued.id).await;
            }
            return auth_error_for_request(
                AuthError::Internal(format!("pairing code encoding failed: {error}")),
                &headers,
                "pairing_offer_issuance_failed",
            );
        }
    };
    let result = PairingOfferResult {
        id: issued.id,
        code,
        reach: payload.reach,
        endpoint: code_payload.endpoint,
        name,
        expires_at: issued.expires_at,
    };
    if let Some(key) = idempotency_key
        && let Err(error) = state
            .auth
            .record_pairing_offer(
                &principal.session_id,
                key.clone(),
                input_fingerprint,
                result.clone(),
            )
            .await
    {
        let _ = state
            .auth
            .cancel_pairing_offer(&principal.session_id, key.clone())
            .await;
        return auth_error_for_request(error, &headers, "pairing_offer_issuance_failed");
    }
    Json(result).into_response()
}

async fn cancel_pairing_offer(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<CancelPairingOfferRequest>,
) -> Response {
    let principal = match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_WRITE)
        .await
    {
        Ok(principal) => principal,
        Err(error) => {
            return auth_error_for_request(error, &headers, "pairing_offer_cancellation_failed");
        }
    };
    let key = payload.idempotency_key.trim();
    if !is_valid_pairing_offer_idempotency_key(key) {
        return invalid_pairing_offer_response("idempotency key is invalid");
    }
    let _offer_guard = state.auth.lock_pairing_offer_issuance().await;
    match state
        .auth
        .cancel_pairing_offer(&principal.session_id, key.to_owned())
        .await
    {
        Ok(cancelled) => Json(json!({ "cancelled": cancelled })).into_response(),
        Err(error) => auth_error_for_request(error, &headers, "pairing_offer_cancellation_failed"),
    }
}

fn invalid_pairing_offer_response(detail: &str) -> Response {
    let trace_id = Uuid::new_v4().to_string();
    tracing::debug!(target: "coddy_studio_server::auth", %trace_id, "invalid pairing offer: {detail}");
    error_response(
        StatusCode::BAD_REQUEST,
        "EnvironmentRequestInvalidError",
        json!({
            "code": "invalid_request",
            "reason": "invalid_pairing_offer",
            "traceId": trace_id,
        }),
    )
}

fn is_valid_pairing_offer_idempotency_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_PAIRING_OFFER_IDEMPOTENCY_KEY_BYTES
}

async fn pairing_links(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_READ).await {
        Ok(_) => Json(state.auth.list_pairings().await).into_response(),
        Err(error) => auth_error_for_request(error, &headers, "pairing_links_load_failed"),
    }
}

async fn share_state(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_READ).await {
        Ok(_) => Json(state.auth.share_exposure_state().await).into_response(),
        Err(error) => auth_error_for_request(error, &headers, "share_state_load_failed"),
    }
}

async fn revoke_pairing_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<RevokePairingRequest>,
) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_WRITE).await {
        Ok(_) => match state.auth.revoke_pairing(&payload.id).await {
            Ok(revoked) => Json(json!({ "revoked": revoked })).into_response(),
            Err(error) => auth_error_for_request(error, &headers, "pairing_link_revoke_failed"),
        },
        Err(error) => auth_error_for_request(error, &headers, "pairing_link_revoke_failed"),
    }
}

async fn clients(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_READ).await {
        Ok(principal) => Json(state.auth.list_clients(&principal.session_id).await).into_response(),
        Err(error) => auth_error_for_request(error, &headers, "client_sessions_load_failed"),
    }
}

async fn revoke_client(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Json(payload): Json<RevokeClientRequest>,
) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_WRITE).await {
        Ok(principal) => match state
            .auth
            .revoke_client(&principal.session_id, &payload.session_id)
            .await
        {
            Ok(revoked) => Json(json!({ "revoked": revoked })).into_response(),
            Err(error) => auth_error_for_request(error, &headers, "client_session_revoke_failed"),
        },
        Err(error) => auth_error_for_request(error, &headers, "client_session_revoke_failed"),
    }
}

async fn revoke_other_clients(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    match authenticated_with_scope(&state.auth, &headers, &uri, SCOPE_ACCESS_WRITE).await {
        Ok(principal) => match state.auth.revoke_other_clients(&principal.session_id).await {
            Ok(revoked_count) => Json(json!({ "revokedCount": revoked_count })).into_response(),
            Err(error) => auth_error_for_request(error, &headers, "client_session_revoke_failed"),
        },
        Err(error) => auth_error_for_request(error, &headers, "client_session_revoke_failed"),
    }
}

pub(crate) async fn authenticate_websocket(
    auth: &AuthService,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<super::model::Principal, AuthError> {
    if let Some(ticket) = query_value(uri, "wsTicket").filter(|value| !value.trim().is_empty()) {
        return auth.verify_websocket_ticket(&ticket).await;
    }
    authenticate_request(auth, headers, uri).await
}

pub(crate) fn auth_error_response(error: AuthError) -> Response {
    HttpAuthError::new(error, "internal_error").into_response()
}

async fn authenticated_with_scope(
    auth: &AuthService,
    headers: &HeaderMap,
    uri: &Uri,
    scope: &str,
) -> Result<super::model::Principal, AuthError> {
    let principal = authenticate_request(auth, headers, uri).await?;
    require_scope(&principal, scope)?;
    Ok(principal)
}

async fn authenticate_request(
    auth: &AuthService,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<super::model::Principal, AuthError> {
    authenticate_request_for_method(auth, headers, request_method(uri), uri).await
}

pub(crate) async fn authorize_http_request(
    auth: &AuthService,
    headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    scope: Option<&str>,
) -> Result<super::model::Principal, AuthError> {
    let principal = authenticate_request_for_method(auth, headers, method.as_str(), uri).await?;
    if let Some(scope) = scope {
        require_scope(&principal, scope)?;
    }
    Ok(principal)
}

async fn authenticate_request_for_method(
    auth: &AuthService,
    headers: &HeaderMap,
    method: &str,
    uri: &Uri,
) -> Result<super::model::Principal, AuthError> {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| cookie_value(cookies, auth.cookie_name()));
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let bearer = authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let dpop = authorization
        .and_then(|value| value.strip_prefix("DPoP "))
        .map(str::trim);
    let token = cookie.or(bearer).or(dpop).filter(|value| !value.is_empty());
    let token = token.ok_or(AuthError::MissingCredential)?;
    // The cookie wins the chain above, so a present cookie is the credential.
    // Browsers attach it cross-site; only same-origin pages may mutate with it.
    let mutates = !matches!(method, "GET" | "HEAD" | "OPTIONS")
        || headers
            .get(header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if cookie.is_some()
        && mutates
        && !cookie_request_origin_allowed(headers, auth.trusted_cookie_origins())
    {
        return Err(AuthError::Forbidden);
    }
    let principal = auth
        .authenticate_token(token, super::SessionTransport::Plain)
        .await?;
    if let Some(expected_thumbprint) = principal.proof_key_thumbprint.as_deref() {
        let dpop_token = dpop.filter(|candidate| *candidate == token);
        let dpop_token = dpop_token.ok_or(AuthError::InvalidCredential)?;
        let proof = headers
            .get("dpop")
            .and_then(|value| value.to_str().ok())
            .ok_or(AuthError::InvalidCredential)?;
        auth.verify_dpop(
            proof,
            method,
            &request_url(headers, uri)?,
            Some(expected_thumbprint),
            Some(dpop_token),
        )
        .await?;
    } else if dpop.is_some() {
        return Err(AuthError::InvalidCredential);
    }
    Ok(principal)
}

/// True when `Origin` is a trusted app origin or the request's own
/// `http(s)://<Host>` origin.
pub(crate) fn cookie_request_origin_allowed(headers: &HeaderMap, trusted: &[String]) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if trusted
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(origin))
    {
        return true;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.path() == "/"
        && url
            .host_str()
            .map(|origin_host| {
                let port = url
                    .port()
                    .map(|port| format!(":{port}"))
                    .unwrap_or_default();
                format!("{origin_host}{port}")
            })
            .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

fn require_scope(principal: &super::model::Principal, scope: &str) -> Result<(), AuthError> {
    if principal.has_scope(scope) {
        Ok(())
    } else {
        Err(AuthError::ScopeRequired(scope.to_owned()))
    }
}

fn credential_response(mut response: Response) -> Response {
    for (name, value) in CREDENTIAL_HEADERS {
        response.headers_mut().insert(
            header::HeaderName::from_static(name),
            header::HeaderValue::from_static(value),
        );
    }
    response
}

fn dpop_challenge_on_unauthorized(mut response: Response) -> Response {
    if response.status() == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("DPoP"),
        );
    }
    response
}

fn auth_error_for_request(
    error: AuthError,
    headers: &HeaderMap,
    internal_reason: &'static str,
) -> Response {
    let response = HttpAuthError::new(error, internal_reason).into_response();
    if request_uses_dpop(headers) {
        dpop_challenge_on_unauthorized(response)
    } else {
        response
    }
}

fn request_uses_dpop(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("DPoP "))
}

struct HttpAuthError {
    error: AuthError,
    internal_reason: &'static str,
}

impl HttpAuthError {
    fn new(error: AuthError, internal_reason: &'static str) -> Self {
        Self {
            error,
            internal_reason,
        }
    }
}

impl IntoResponse for HttpAuthError {
    fn into_response(self) -> Response {
        let trace_id = Uuid::new_v4().to_string();
        match self.error {
            AuthError::MissingCredential => error_response(
                StatusCode::UNAUTHORIZED,
                "EnvironmentAuthInvalidError",
                json!({ "code": "auth_invalid", "reason": "missing_credential", "traceId": trace_id }),
            ),
            AuthError::InvalidCredential => error_response(
                StatusCode::UNAUTHORIZED,
                "EnvironmentAuthInvalidError",
                json!({ "code": "auth_invalid", "reason": "invalid_credential", "traceId": trace_id }),
            ),
            AuthError::InvalidScope => error_response(
                StatusCode::BAD_REQUEST,
                "EnvironmentRequestInvalidError",
                json!({ "code": "invalid_request", "reason": "invalid_scope", "traceId": trace_id }),
            ),
            AuthError::ScopeNotGranted => error_response(
                StatusCode::BAD_REQUEST,
                "EnvironmentRequestInvalidError",
                json!({ "code": "invalid_request", "reason": "scope_not_granted", "traceId": trace_id }),
            ),
            AuthError::ScopeRequired(required_scope) => error_response(
                StatusCode::FORBIDDEN,
                "EnvironmentScopeRequiredError",
                json!({ "code": "insufficient_scope", "requiredScope": required_scope, "traceId": trace_id }),
            ),
            AuthError::CurrentSessionRevokeNotAllowed => error_response(
                StatusCode::FORBIDDEN,
                "EnvironmentOperationForbiddenError",
                json!({ "code": "operation_forbidden", "reason": "current_session_revoke_not_allowed", "traceId": trace_id }),
            ),
            AuthError::Forbidden => error_response(
                StatusCode::FORBIDDEN,
                "EnvironmentOperationForbiddenError",
                json!({ "code": "operation_forbidden", "reason": "origin_not_allowed", "traceId": trace_id }),
            ),
            AuthError::Internal(diagnostic) => {
                tracing::error!(%trace_id, %diagnostic, "environment authentication failed");
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "EnvironmentInternalError",
                    json!({ "code": "internal_error", "reason": self.internal_reason, "traceId": trace_id }),
                )
            }
        }
    }
}

fn error_response(status: StatusCode, tag: &str, fields: serde_json::Value) -> Response {
    let mut object = fields.as_object().cloned().unwrap_or_default();
    object.insert("_tag".to_owned(), json!(tag));
    (status, Json(object)).into_response()
}

fn cookie_value<'a>(cookies: &'a str, name: &str) -> Option<&'a str> {
    cookies.split(';').find_map(|cookie| {
        let (candidate, value) = cookie.trim().split_once('=')?;
        (candidate == name).then_some(value)
    })
}

fn query_value(uri: &Uri, key: &str) -> Option<String> {
    url::form_urlencoded::parse(uri.query()?.as_bytes())
        .find_map(|(candidate, value)| (candidate == key).then(|| value.into_owned()))
}

fn request_url(headers: &HeaderMap, uri: &Uri) -> Result<String, AuthError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("localhost");
    let protocol = if headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        == Some("https")
    {
        "https"
    } else {
        "http"
    };
    Ok(format!("{protocol}://{host}{uri}"))
}

fn request_method(uri: &Uri) -> &'static str {
    match uri.path() {
        "/api/auth/session"
        | "/api/auth/share-state"
        | "/api/auth/pairing-links"
        | "/api/auth/clients"
        | "/ws" => "GET",
        _ => "POST",
    }
}

fn client_metadata(headers: &HeaderMap, label: Option<String>) -> ClientMetadata {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    ClientMetadata {
        label,
        user_agent: user_agent.clone(),
        device_type: infer_device_type(headers).to_owned(),
        os: infer_os(headers).map(str::to_owned),
        browser: infer_browser(user_agent.as_deref()).map(str::to_owned),
        ip_address: None,
    }
}

fn infer_device_type(headers: &HeaderMap) -> &'static str {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ["bot", "crawler", "spider", "slurp", "curl", "wget"]
        .iter()
        .any(|needle| user_agent.contains(needle))
    {
        "bot"
    } else if user_agent.contains("ipad") || user_agent.contains("tablet") {
        "tablet"
    } else if user_agent.contains("iphone") || user_agent.contains("mobile") {
        "mobile"
    } else {
        "desktop"
    }
}

fn infer_os(headers: &HeaderMap) -> Option<&'static str> {
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())?
        .to_ascii_lowercase();
    if user_agent.contains("iphone") || user_agent.contains("ipad") {
        Some("iOS")
    } else if user_agent.contains("android") {
        Some("Android")
    } else if user_agent.contains("mac os x") || user_agent.contains("macintosh") {
        Some("macOS")
    } else if user_agent.contains("windows nt") {
        Some("Windows")
    } else if user_agent.contains("linux") {
        Some("Linux")
    } else {
        None
    }
}

fn infer_browser(user_agent: Option<&str>) -> Option<&'static str> {
    let user_agent = user_agent?.to_ascii_lowercase();
    if user_agent.contains("edg/") {
        Some("Edge")
    } else if user_agent.contains("opr/") {
        Some("Opera")
    } else if user_agent.contains("firefox/") {
        Some("Firefox")
    } else if user_agent.contains("chrome/") || user_agent.contains("crios/") {
        Some("Chrome")
    } else if user_agent.contains("safari/") {
        Some("Safari")
    } else {
        None
    }
}

fn is_device_type(value: &str) -> bool {
    ["desktop", "mobile", "tablet", "bot", "unknown"].contains(&value)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::{MAX_PAIRING_OFFER_IDEMPOTENCY_KEY_BYTES, is_valid_pairing_offer_idempotency_key};
    use crate::auth::service::{is_loopback_host, is_unspecified_host};

    #[derive(Deserialize)]
    struct PairingEndpointFixture {
        endpoint: String,
        classification: String,
    }

    #[test]
    fn pairing_endpoint_loopback_semantics_match_shared_fixtures() {
        let fixtures: Vec<PairingEndpointFixture> = serde_json::from_str(include_str!(
            "../../../../packages/shared/fixtures/pairing-endpoint-classification.json"
        ))
        .expect("pairing endpoint fixtures");

        for fixture in fixtures {
            let Ok(endpoint) = url::Url::parse(&fixture.endpoint) else {
                assert_eq!(fixture.classification, "unconnectable");
                continue;
            };
            let host = endpoint.host_str().unwrap_or_default();
            let wildcard = host.is_empty() || is_unspecified_host(host);
            let unconnectable = wildcard || endpoint.port() == Some(0);
            assert_eq!(
                unconnectable,
                fixture.classification == "unconnectable",
                "{}",
                fixture.endpoint
            );
            if !unconnectable {
                assert_eq!(
                    is_loopback_host(host),
                    fixture.classification == "loopback",
                    "{}",
                    fixture.endpoint
                );
            }
        }
    }

    #[test]
    fn cookie_origin_matches_host_or_the_dev_origin() {
        use axum::http::{HeaderMap, HeaderValue, header};

        let config = crate::config::ServerConfig::new(std::env::temp_dir())
            .with_dev_url("http://localhost:5733/app".parse().expect("dev URL"));
        let auth = crate::auth::AuthService::new(&config, vec![7_u8; 32]);
        let trusted = auth.trusted_cookie_origins();
        let allowed = |host: Option<&str>, origin: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(host) = host {
                headers.insert(header::HOST, HeaderValue::from_str(host).expect("host"));
            }
            if let Some(origin) = origin {
                headers.insert(
                    header::ORIGIN,
                    HeaderValue::from_str(origin).expect("origin"),
                );
            }
            super::cookie_request_origin_allowed(&headers, trusted)
        };

        assert_eq!(trusted, ["http://localhost:5733"]);
        assert!(allowed(None, Some("http://localhost:5733")));
        // The desktop app never authenticates with the cookie, so its origins get no pass.
        for origin in ["coddy-studio://app", "coddy-studio-dev://app"] {
            assert!(!allowed(Some("127.0.0.1:3773"), Some(origin)), "{origin}");
        }
        assert!(allowed(
            Some("127.0.0.1:3773"),
            Some("http://127.0.0.1:3773")
        ));
        assert!(allowed(Some("[::1]:3773"), Some("http://[::1]:3773")));
        assert!(allowed(Some("Example.test"), Some("https://example.test")));
        assert!(!allowed(Some("127.0.0.1:3773"), None));
        assert!(!allowed(Some("127.0.0.1:3773"), Some("null")));
        let opaque_dev = crate::config::ServerConfig::new(std::env::temp_dir())
            .with_dev_url("file:///tmp/app".parse().expect("opaque dev URL"));
        let opaque_auth = crate::auth::AuthService::new(&opaque_dev, vec![7_u8; 32]);
        assert!(opaque_auth.trusted_cookie_origins().is_empty());
        assert!(!allowed(
            Some("127.0.0.1:3773"),
            Some("http://127.0.0.1:3774")
        ));
        assert!(!allowed(Some("[::1]:3773"), Some("http://[::2]:3773")));
        assert!(!allowed(
            Some("127.0.0.1:3773"),
            Some("ws://127.0.0.1:3773")
        ));
        assert!(!allowed(
            Some("127.0.0.1:3773"),
            Some("http://127.0.0.1:3773/x")
        ));
        assert!(!allowed(None, Some("http://127.0.0.1:3773")));
        assert!(!allowed(
            Some("localhost:5734"),
            Some("http://localhost:5734.evil")
        ));
    }

    #[test]
    fn pairing_offer_idempotency_keys_are_byte_bounded() {
        assert!(!is_valid_pairing_offer_idempotency_key(""));
        assert!(is_valid_pairing_offer_idempotency_key(
            &"x".repeat(MAX_PAIRING_OFFER_IDEMPOTENCY_KEY_BYTES)
        ));
        assert!(!is_valid_pairing_offer_idempotency_key(
            &"x".repeat(MAX_PAIRING_OFFER_IDEMPOTENCY_KEY_BYTES + 1)
        ));
        assert!(!is_valid_pairing_offer_idempotency_key(
            &"🦀".repeat(MAX_PAIRING_OFFER_IDEMPOTENCY_KEY_BYTES / 2)
        ));
    }
}
