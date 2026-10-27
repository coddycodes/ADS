use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use coddy_studio_server::{
    ROUTE_INVENTORY, RpcRegistry, ServerConfig, ServerHandle, ServerRuntime,
};
use futures_util::{SinkExt, StreamExt};
use p256::ecdsa::{Signature, SigningKey, signature::hazmat::PrehashSigner};
use reqwest::{Client, Response, StatusCode, header};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const DESKTOP_BOOTSTRAP: &str = "desktop-bootstrap-fixture";
const TOKEN_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
const BOOTSTRAP_TOKEN_TYPE: &str = "urn:coddy-studio:params:oauth:token-type:environment-bootstrap";

#[test]
fn language_neutral_auth_fixtures_match_the_rust_http_inventory() {
    let fixture_directory = auth_fixture_directory();
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_directory.join("manifest.json"))
            .expect("auth manifest fixture"),
    )
    .expect("valid auth manifest");
    let mut fixture_routes = manifest["routes"]
        .as_array()
        .expect("fixture routes")
        .iter()
        .map(|route| {
            (
                route["method"].as_str().expect("fixture method").to_owned(),
                route["path"].as_str().expect("fixture path").to_owned(),
            )
        })
        .collect::<Vec<_>>();
    fixture_routes.sort();
    let mut rust_routes = ROUTE_INVENTORY
        .iter()
        .filter(|route| route.path.starts_with("/api/auth/") || route.path == "/oauth/token")
        .map(|route| (route.method.to_owned(), route.path.to_owned()))
        .collect::<Vec<_>>();
    rust_routes.sort();

    assert_eq!(rust_routes, fixture_routes);
    assert_eq!(
        manifest["scopes"]["all"],
        json!([
            "orchestration:read",
            "orchestration:operate",
            "terminal:operate",
            "review:write",
            "access:read",
            "access:write",
            "relay:read",
            "relay:write"
        ])
    );
    for fixture in manifest["fixtures"].as_array().expect("auth fixture list") {
        let path = fixture_directory.join(fixture.as_str().expect("auth fixture path"));
        assert!(path.is_file(), "missing auth fixture: {}", path.display());
    }
}

#[tokio::test]
async fn desktop_bootstrap_creates_cookie_and_bearer_sessions() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();

    let unauthenticated = get_json(
        client
            .get(http_url(&handle, "/api/auth/session"))
            .send()
            .await
            .expect("session request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(unauthenticated["authenticated"], false);
    assert_eq!(unauthenticated["auth"]["policy"], "desktop-managed-local");
    assert_eq!(
        unauthenticated["auth"]["bootstrapMethods"],
        json!(["desktop-bootstrap"])
    );
    assert_eq!(
        unauthenticated["auth"]["sessionCookieName"],
        "coddy_studio_session_0"
    );

    let browser_response = client
        .post(http_url(&handle, "/api/auth/browser-session"))
        .json(&json!({ "credential": DESKTOP_BOOTSTRAP }))
        .send()
        .await
        .expect("browser bootstrap request");
    assert_credential_headers(&browser_response);
    let cookie = browser_response
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie")
        .to_str()
        .expect("ASCII session cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    assert!(cookie.starts_with("coddy_studio_session_0="));
    let browser_session = get_json(browser_response, StatusCode::OK).await;
    assert_eq!(browser_session["authenticated"], true);
    assert_eq!(browser_session["sessionMethod"], "browser-session-cookie");

    let authenticated = get_json(
        client
            .get(http_url(&handle, "/api/auth/session"))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .expect("cookie session request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(authenticated["authenticated"], true);

    let access = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    assert_eq!(access["token_type"], "Bearer");
    assert_eq!(access["issued_token_type"], ACCESS_TOKEN_TYPE);
    assert!(access["expires_in"].as_u64().is_some_and(|ttl| ttl > 0));
    assert!(
        access["scope"]
            .as_str()
            .is_some_and(|scope| scope.contains("access:write"))
    );

    shutdown(handle).await;
}

#[tokio::test]
async fn web_mode_exposes_a_one_time_administrative_startup_pairing_url() {
    let temp = TempDir::new().expect("temporary base directory");
    let config = ServerConfig::new(temp.path()).with_bind("127.0.0.1", 0);
    let handle = ServerRuntime::start_with_registry(config, RpcRegistry::empty())
        .await
        .expect("web server starts");
    let startup = handle
        .startup_access()
        .expect("web mode startup pairing access");
    assert_eq!(
        startup.connection_string,
        format!("http://{}", handle.local_addr())
    );
    assert!(
        startup
            .pairing_url
            .starts_with(&format!("http://{}/pair#token=", handle.local_addr()))
    );
    assert!(startup.pairing_url.ends_with(&startup.credential));

    let client = Client::new();
    let session = exchange_token(&client, &handle, &startup.credential, None).await;
    assert!(
        session["scope"]
            .as_str()
            .is_some_and(|scopes| scopes.contains("access:write"))
    );
    let replay = client
        .post(http_url(&handle, "/oauth/token"))
        .form(&token_form(&startup.credential, None))
        .send()
        .await
        .expect("startup pairing replay request");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    shutdown(handle).await;
}

#[tokio::test]
async fn one_time_pairing_credentials_are_atomic_and_scope_constrained() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);

    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({
                "label": "Read-only client",
                "scopes": ["orchestration:read"]
            }))
            .send()
            .await
            .expect("pairing token request"),
        StatusCode::OK,
    )
    .await;
    let credential = pairing["credential"].as_str().expect("pairing credential");
    assert_eq!(credential.len(), 12);
    assert!(
        credential
            .bytes()
            .all(|byte| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains(&byte))
    );

    let paired = exchange_token(&client, &handle, credential, None).await;
    assert_eq!(paired["scope"], "orchestration:read");

    let replay = client
        .post(http_url(&handle, "/oauth/token"))
        .form(&token_form(credential, None))
        .send()
        .await
        .expect("replayed exchange request");
    assert_credential_headers(&replay);
    let replay_error = get_json(replay, StatusCode::UNAUTHORIZED).await;
    assert_eq!(replay_error["_tag"], "EnvironmentAuthInvalidError");
    assert_eq!(replay_error["code"], "auth_invalid");
    assert_eq!(replay_error["reason"], "invalid_credential");
    assert!(
        replay_error["traceId"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );

    let overbroad_pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(access_token(&paired))
            .json(&json!({ "scopes": ["access:read"] }))
            .send()
            .await
            .expect("overbroad pairing request"),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(overbroad_pairing["_tag"], "EnvironmentScopeRequiredError");
    assert_eq!(overbroad_pairing["requiredScope"], "access:write");

    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_links_and_client_sessions_can_be_listed_and_revoked() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);

    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Revocable client" }))
            .send()
            .await
            .expect("pairing token request"),
        StatusCode::OK,
    )
    .await;
    let pairing_id = pairing["id"].as_str().expect("pairing id");
    let pairing_credential = pairing["credential"].as_str().expect("pairing credential");

    let links = get_json(
        client
            .get(http_url(&handle, "/api/auth/pairing-links"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("list pairing links"),
        StatusCode::OK,
    )
    .await;
    assert!(
        links
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == pairing_id))
    );

    let revoked = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-links/revoke"))
            .bearer_auth(administrator_token)
            .json(&json!({ "id": pairing_id }))
            .send()
            .await
            .expect("revoke pairing link"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["revoked"], true);

    let unavailable = client
        .post(http_url(&handle, "/oauth/token"))
        .form(&token_form(pairing_credential, None))
        .send()
        .await
        .expect("revoked pairing exchange");
    assert_eq!(unavailable.status(), StatusCode::UNAUTHORIZED);

    let second_pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Paired client" }))
            .send()
            .await
            .expect("second pairing token request"),
        StatusCode::OK,
    )
    .await;
    let paired = exchange_token(
        &client,
        &handle,
        second_pairing["credential"]
            .as_str()
            .expect("second credential"),
        None,
    )
    .await;
    let paired_token = access_token(&paired);
    let clients = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("list clients"),
        StatusCode::OK,
    )
    .await;
    let paired_session_id = clients
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["client"]["label"] == "Paired client")
        })
        .and_then(|item| item["sessionId"].as_str())
        .expect("paired session id");

    let current_session_id = clients
        .as_array()
        .and_then(|items| items.iter().find(|item| item["current"] == true))
        .and_then(|item| item["sessionId"].as_str())
        .expect("current administrator session");
    let self_revoke = get_json(
        client
            .post(http_url(&handle, "/api/auth/clients/revoke"))
            .bearer_auth(administrator_token)
            .json(&json!({ "sessionId": current_session_id }))
            .send()
            .await
            .expect("self revoke request"),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(self_revoke["reason"], "current_session_revoke_not_allowed");

    let revoke = get_json(
        client
            .post(http_url(&handle, "/api/auth/clients/revoke"))
            .bearer_auth(administrator_token)
            .json(&json!({ "sessionId": paired_session_id }))
            .send()
            .await
            .expect("revoke client request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoke["revoked"], true);

    let revoked_state = get_json(
        client
            .get(http_url(&handle, "/api/auth/session"))
            .bearer_auth(paired_token)
            .send()
            .await
            .expect("revoked session state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked_state["authenticated"], false);

    let third_pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Revoke other client" }))
            .send()
            .await
            .expect("third pairing token request"),
        StatusCode::OK,
    )
    .await;
    let _third = exchange_token(
        &client,
        &handle,
        third_pairing["credential"]
            .as_str()
            .expect("third credential"),
        None,
    )
    .await;
    let revoked_others = get_json(
        client
            .post(http_url(&handle, "/api/auth/clients/revoke-others"))
            .bearer_auth(administrator_token)
            .json(&json!({}))
            .send()
            .await
            .expect("revoke other clients request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked_others["revokedCount"], 1);

    shutdown(handle).await;
}

#[tokio::test]
async fn share_state_reports_grant_derived_exposure() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    let initial = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(initial["desiredExposure"], "loopback");
    assert_eq!(initial["offHostGrantCount"], 0);
    assert_eq!(initial["legacyGrantCount"], 0);

    let _offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(token)
            .json(&json!({ "label": "Tablet" }))
            .send()
            .await
            .expect("pairing token"),
        StatusCode::OK,
    )
    .await;
    let after_legacy = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(after_legacy["desiredExposure"], "loopback");
    assert_eq!(after_legacy["legacyGrantCount"], 1);

    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_offer_persists_reach_onto_the_minted_grant() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({
                "name": "AI-SERVER",
                "endpoint": "http://192.168.1.20:3773",
                "reach": "another-device",
            }))
            .send()
            .await
            .expect("pairing offer"),
        StatusCode::OK,
    )
    .await;
    let offer_id = offer["id"].as_str().expect("offer id").to_owned();

    let links = get_json(
        client
            .get(http_url(&handle, "/api/auth/pairing-links"))
            .bearer_auth(token)
            .send()
            .await
            .expect("pairing links"),
        StatusCode::OK,
    )
    .await;
    let minted = links
        .as_array()
        .expect("links array")
        .iter()
        .find(|link| link["id"] == offer_id.as_str())
        .expect("minted link");
    assert_eq!(minted["reach"], "another-device");

    let state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["desiredExposure"], "wide");
    assert_eq!(state["offHostGrantCount"], 1);
    assert_eq!(state["legacyGrantCount"], 0);

    shutdown(handle).await;
}

#[tokio::test]
async fn browser_pairing_session_keeps_off_host_exposure_until_revoked() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);

    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(administrator_token)
            .json(&json!({
                "name": "Tablet browser",
                "endpoint": "http://192.168.1.20:3773",
                "reach": "another-device",
            }))
            .send()
            .await
            .expect("pairing offer"),
        StatusCode::OK,
    )
    .await;
    let payload = coddy_studio_server::auth_pairing_code::decode_pairing_code(
        offer["code"].as_str().expect("pairing code"),
    )
    .expect("pairing code decodes");

    let browser_session = get_json(
        client
            .post(http_url(&handle, "/api/auth/browser-session"))
            .json(&json!({ "credential": payload.token }))
            .send()
            .await
            .expect("browser pairing exchange"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(browser_session["sessionMethod"], "browser-session-cookie");

    let state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("share state after browser pairing"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["desiredExposure"], "wide");
    assert_eq!(state["offHostGrantCount"], 1);

    let clients = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("list clients"),
        StatusCode::OK,
    )
    .await;
    let browser_session_id = clients
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["method"] == "browser-session-cookie")
        })
        .and_then(|item| item["sessionId"].as_str())
        .expect("browser session id");
    let revoked = get_json(
        client
            .post(http_url(&handle, "/api/auth/clients/revoke"))
            .bearer_auth(administrator_token)
            .json(&json!({ "sessionId": browser_session_id }))
            .send()
            .await
            .expect("revoke browser session"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["revoked"], true);

    let state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("share state after revocation"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["desiredExposure"], "loopback");
    assert_eq!(state["offHostGrantCount"], 0);

    shutdown(handle).await;
}

#[tokio::test]
async fn loopback_custom_offers_never_flip_desired_exposure_wide() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({
                "name": "AI-SERVER",
                "endpoint": "http://127.0.0.1:9022",
                "reach": "custom",
            }))
            .send()
            .await
            .expect("custom loopback offer"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(offer["reach"], "custom");

    let state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["desiredExposure"], "loopback");
    assert_eq!(state["offHostGrantCount"], 0);
    assert_eq!(state["legacyGrantCount"], 0);

    shutdown(handle).await;
}

#[tokio::test]
async fn websocket_requires_a_short_lived_ticket_or_request_credential() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);

    let ticket_response = client
        .post(http_url(&handle, "/api/auth/websocket-ticket"))
        .bearer_auth(administrator_token)
        .send()
        .await
        .expect("WebSocket ticket request");
    assert_credential_headers(&ticket_response);
    let ticket = get_json(ticket_response, StatusCode::OK).await;
    let ticket = ticket["ticket"].as_str().expect("WebSocket ticket");

    let (mut socket, _) =
        connect_async(format!("ws://{}/ws?wsTicket={ticket}", handle.local_addr()))
            .await
            .expect("ticket-authenticated WebSocket");
    socket
        .send(tungstenite::Message::Text(
            json!({ "_tag": "Ping" }).to_string().into(),
        ))
        .await
        .expect("send protocol ping");
    let pong = timeout(
        Duration::from_secs(2),
        next_frame_past_heartbeat(&mut socket),
    )
    .await
    .expect("Pong timeout")
    .expect("WebSocket open")
    .expect("valid WebSocket frame");
    assert_eq!(pong.into_text().expect("text Pong"), r#"{"_tag":"Pong"}"#);
    socket.close(None).await.expect("close WebSocket");

    for query in [
        format!("wsTicket={administrator_token}"),
        format!("token={administrator_token}"),
    ] {
        let error = connect_async(format!("ws://{}/ws?{query}", handle.local_addr()))
            .await
            .expect_err("raw session query token must be rejected");
        assert!(matches!(
            error,
            tungstenite::Error::Http(response)
                if response.status() == StatusCode::UNAUTHORIZED
        ));
    }

    shutdown(handle).await;
}

#[tokio::test]
async fn revoking_a_client_terminates_its_live_websocket() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);

    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
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
    let paired_token = access_token(&paired);
    let paired_ticket = websocket_ticket(&client, &handle, paired_token).await;
    let (mut paired_socket, _) = connect_async(format!(
        "ws://{}/ws?wsTicket={paired_ticket}",
        handle.local_addr()
    ))
    .await
    .expect("paired WebSocket");

    let clients_list = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("clients"),
        StatusCode::OK,
    )
    .await;
    let target = clients_list
        .as_array()
        .expect("clients array")
        .iter()
        .find(|session| session["current"] == false && session["connected"] == true)
        .expect("paired connected session")["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();

    let revoke = client
        .post(http_url(&handle, "/api/auth/clients/revoke"))
        .bearer_auth(administrator_token)
        .json(&json!({ "sessionId": target }))
        .send()
        .await
        .expect("revoke");
    assert_eq!(revoke.status(), StatusCode::OK);
    let closed = timeout(Duration::from_secs(5), async {
        loop {
            match paired_socket.next().await {
                None => break true,
                Some(Ok(tungstenite::Message::Close(_))) => break true,
                Some(Ok(_)) => continue,
                Some(Err(_)) => break true,
            }
        }
    })
    .await
    .expect("revoked socket must close within 5s");
    assert!(closed);

    shutdown(handle).await;
}

#[tokio::test]
async fn remote_revocation_closes_an_acked_live_stream_before_later_events() {
    let temp = TempDir::new().expect("temporary base directory");
    let first = start_desktop_server(&temp).await;
    let second = start_desktop_server(&temp).await;
    let client = Client::new();
    let first_administrator = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let first_token = access_token(&first_administrator);
    let first_clients = get_json(
        client
            .get(http_url(&first, "/api/auth/clients"))
            .bearer_auth(first_token)
            .send()
            .await
            .expect("first client list"),
        StatusCode::OK,
    )
    .await;
    let first_session_id = first_clients
        .as_array()
        .expect("first clients")
        .iter()
        .find(|session| session["current"] == true)
        .and_then(|session| session["sessionId"].as_str())
        .expect("first administrator session")
        .to_owned();
    // The desktop bootstrap represents the host's single WebView and each
    // exchange supersedes earlier desktop sessions on the shared root, so the
    // second process authenticates through a one-time pairing the first
    // administrator issues; both servers share the persisted pairing state.
    let second_pairing = get_json(
        client
            .post(http_url(&first, "/api/auth/pairing-token"))
            .bearer_auth(first_token)
            .json(&json!({
                "label": "Second administrator",
                "scopes": [
                    "orchestration:read",
                    "orchestration:operate",
                    "terminal:operate",
                    "review:write",
                    "relay:read",
                    "access:read",
                    "access:write",
                    "relay:write"
                ]
            }))
            .send()
            .await
            .expect("second administrator pairing request"),
        StatusCode::OK,
    )
    .await;
    let second_credential = second_pairing["credential"]
        .as_str()
        .expect("second administrator pairing credential")
        .to_owned();
    let second_administrator = exchange_token(&client, &second, &second_credential, None).await;
    let second_token = access_token(&second_administrator);

    let ticket = websocket_ticket(&client, &first, first_token).await;
    let (mut socket, _) =
        connect_async(format!("ws://{}/ws?wsTicket={ticket}", first.local_addr()))
            .await
            .expect("first live WebSocket");
    send_ws_json(
        &mut socket,
        json!({
            "_tag": "Request",
            "id": "201",
            "tag": "subscribeAuthAccess",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    let snapshot = next_ws_json(&mut socket).await;
    assert_eq!(snapshot["_tag"], "Chunk");
    assert_eq!(snapshot["requestId"], "201");
    send_ws_json(&mut socket, json!({ "_tag": "Ack", "requestId": "201" })).await;

    let revoked = get_json(
        client
            .post(http_url(&second, "/api/auth/clients/revoke"))
            .bearer_auth(second_token)
            .json(&json!({ "sessionId": first_session_id }))
            .send()
            .await
            .expect("remote client revocation"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["revoked"], true);
    let later_pairing = get_json(
        client
            .post(http_url(&second, "/api/auth/pairing-token"))
            .bearer_auth(second_token)
            .json(&json!({ "label": "Must not reach revoked stream" }))
            .send()
            .await
            .expect("later access mutation"),
        StatusCode::OK,
    )
    .await;
    let later_pairing_id = later_pairing["id"].as_str().expect("later pairing id");

    timeout(Duration::from_secs(2), async {
        loop {
            match socket.next().await {
                None | Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) => break,
                Some(Ok(tungstenite::Message::Text(text))) => {
                    let message: Value =
                        serde_json::from_str(&text).expect("valid live-stream frame");
                    if message["_tag"] == "Chunk" {
                        let later_event_reached_stream =
                            message["values"].as_array().is_some_and(|values| {
                                values.iter().any(|value| {
                                    value["type"] == "pairingLinkUpserted"
                                        && value["payload"]["id"] == later_pairing_id
                                })
                            });
                        assert!(
                            !later_event_reached_stream,
                            "later access event reached the revoked stream: {message}"
                        );
                        if socket
                            .send(tungstenite::Message::Text(
                                json!({ "_tag": "Ack", "requestId": "201" })
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("remote revocation must close the ACKed stream");

    shutdown(second).await;
    shutdown(first).await;
}

#[tokio::test]
async fn plain_websocket_rejects_a_single_frame_larger_than_16_mib() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let ticket = websocket_ticket(&client, &handle, access_token(&administrator)).await;
    let (mut socket, _) =
        connect_async(format!("ws://{}/ws?wsTicket={ticket}", handle.local_addr()))
            .await
            .expect("authenticated WebSocket");

    let send_result = socket
        .send(tungstenite::Message::Binary(
            vec![b' '; 16 * 1024 * 1024 + 1].into(),
        ))
        .await;
    let terminal = if send_result.is_err() {
        true
    } else {
        let outcome = timeout(
            Duration::from_secs(3),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        .expect("oversized plain frame reaches a terminal outcome");
        matches!(
            outcome,
            None | Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_))
        )
    };
    assert!(terminal, "oversized plain frame must terminate the socket");

    shutdown(handle).await;
}

#[tokio::test]
async fn plain_websocket_connected_state_tracks_the_completed_upgrade_lifecycle() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);
    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Lifecycle client" }))
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
    let paired_token = access_token(&paired);
    let paired_ticket = websocket_ticket(&client, &handle, paired_token).await;
    let (mut socket, _) = connect_async(format!(
        "ws://{}/ws?wsTicket={paired_ticket}",
        handle.local_addr()
    ))
    .await
    .expect("paired WebSocket");

    let clients = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("clients"),
        StatusCode::OK,
    )
    .await;
    let session_id = clients
        .as_array()
        .expect("clients array")
        .iter()
        .find(|session| session["current"] == false && session["connected"] == true)
        .and_then(|session| session["sessionId"].as_str())
        .expect("connected paired session")
        .to_owned();

    socket.close(None).await.expect("close paired WebSocket");
    timeout(Duration::from_secs(5), async {
        loop {
            let clients = get_json(
                client
                    .get(http_url(&handle, "/api/auth/clients"))
                    .bearer_auth(administrator_token)
                    .send()
                    .await
                    .expect("clients after close"),
                StatusCode::OK,
            )
            .await;
            let disconnected = clients
                .as_array()
                .expect("clients array")
                .iter()
                .any(|session| session["sessionId"] == session_id && session["connected"] == false);
            if disconnected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("closed WebSocket must be marked disconnected");

    shutdown(handle).await;
}

#[tokio::test]
async fn failed_websocket_upgrade_does_not_mark_the_session_connected() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);
    let pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Failed upgrade client" }))
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
    let paired_ticket = websocket_ticket(&client, &handle, access_token(&paired)).await;

    let mut stream = TcpStream::connect(handle.local_addr())
        .await
        .expect("raw TCP connection");
    stream
        .write_all(
            format!(
                "GET /ws?wsTicket={paired_ticket} HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 12\r\n\r\n",
                handle.local_addr()
            )
            .as_bytes(),
        )
        .await
        .expect("write invalid WebSocket upgrade");
    let mut response = vec![0_u8; 1_024];
    let bytes_read = timeout(Duration::from_secs(2), stream.read(&mut response))
        .await
        .expect("failed-upgrade response timeout")
        .expect("read failed-upgrade response");
    response.truncate(bytes_read);
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.starts_with("HTTP/1.1 400") || response.starts_with("HTTP/1.1 426"),
        "invalid WebSocket version must reject the upgrade: {response}"
    );

    let clients = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("clients after failed upgrade"),
        StatusCode::OK,
    )
    .await;
    assert!(
        clients
            .as_array()
            .expect("clients array")
            .iter()
            .filter(|session| session["current"] == false)
            .all(|session| session["connected"] == false),
        "a rejected HTTP upgrade must not create a live WebSocket connection"
    );

    shutdown(handle).await;
}

#[tokio::test]
async fn websocket_authorizes_rpc_scopes_and_streams_auth_access_changes() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);
    let administrator_ticket = websocket_ticket(&client, &handle, administrator_token).await;
    let (mut administrator_socket, _) = connect_async(format!(
        "ws://{}/ws?wsTicket={administrator_ticket}",
        handle.local_addr()
    ))
    .await
    .expect("administrator WebSocket");

    send_ws_json(
        &mut administrator_socket,
        json!({
            "_tag": "Request",
            "id": "101",
            "tag": "subscribeAuthAccess",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    let snapshot = next_ws_json(&mut administrator_socket).await;
    assert_eq!(snapshot["_tag"], "Chunk");
    assert_eq!(snapshot["requestId"], "101");
    assert_eq!(snapshot["values"][0]["type"], "snapshot");
    assert_eq!(snapshot["values"][0]["version"], 1);
    assert!(
        snapshot["values"][0]["payload"]["clientSessions"]
            .as_array()
            .is_some_and(|sessions| sessions
                .iter()
                .any(|session| { session["current"] == true && session["connected"] == true }))
    );
    send_ws_json(
        &mut administrator_socket,
        json!({ "_tag": "Ack", "requestId": "101" }),
    )
    .await;

    let live_pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "label": "Live stream fixture" }))
            .send()
            .await
            .expect("live pairing request"),
        StatusCode::OK,
    )
    .await;
    let upsert = next_ws_json(&mut administrator_socket).await;
    assert_eq!(upsert["_tag"], "Chunk");
    assert_eq!(upsert["values"][0]["type"], "pairingLinkUpserted");
    assert_eq!(upsert["values"][0]["payload"]["id"], live_pairing["id"]);
    administrator_socket
        .close(None)
        .await
        .expect("close administrator WebSocket");

    let restricted_pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "scopes": ["orchestration:read"] }))
            .send()
            .await
            .expect("restricted pairing request"),
        StatusCode::OK,
    )
    .await;
    let restricted = exchange_token(
        &client,
        &handle,
        restricted_pairing["credential"]
            .as_str()
            .expect("restricted credential"),
        None,
    )
    .await;
    let restricted_ticket = websocket_ticket(&client, &handle, access_token(&restricted)).await;
    let (mut restricted_socket, _) = connect_async(format!(
        "ws://{}/ws?wsTicket={restricted_ticket}",
        handle.local_addr()
    ))
    .await
    .expect("restricted WebSocket");
    for (id, tag) in [
        ("110", "activity.getSnapshot"),
        ("111", "activity.listRoster"),
        ("112", "activity.listDetail"),
    ] {
        send_ws_json(
            &mut restricted_socket,
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": {},
                "headers": []
            }),
        )
        .await;
        let authorized = next_ws_json(&mut restricted_socket).await;
        assert_eq!(authorized["_tag"], "Exit");
        assert_eq!(authorized["requestId"], id);
        assert_eq!(authorized["exit"]["_tag"], "Success");
        assert_eq!(authorized["exit"]["value"]["tag"], tag);
    }
    for (id, tag) in [
        ("114", "activity.cancelSubtree"),
        ("115", "activity.retrySubtreeCancellation"),
        // Scan reads CLI transcripts outside Coddy Studio's data on the server host.
        ("116", "agentSessions.scan"),
        ("117", "agentSessions.import"),
    ] {
        send_ws_json(
            &mut restricted_socket,
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": {},
                "headers": []
            }),
        )
        .await;
        let denied = next_ws_json(&mut restricted_socket).await;
        assert_eq!(denied["_tag"], "Exit");
        assert_eq!(denied["exit"]["_tag"], "Failure");
        assert_eq!(
            denied["exit"]["cause"][0]["error"]["_tag"],
            "EnvironmentAuthorizationError"
        );
        assert_eq!(
            denied["exit"]["cause"][0]["error"]["requiredScope"],
            "orchestration:operate"
        );
    }
    send_ws_json(
        &mut restricted_socket,
        json!({
            "_tag": "Request",
            "id": "113",
            "tag": "subscribeActivity",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    let activity_chunk = next_ws_json(&mut restricted_socket).await;
    assert_eq!(activity_chunk["_tag"], "Chunk");
    assert_eq!(activity_chunk["requestId"], "113");
    assert_eq!(activity_chunk["values"][0]["tag"], "subscribeActivity");
    send_ws_json(
        &mut restricted_socket,
        json!({ "_tag": "Ack", "requestId": "113" }),
    )
    .await;
    let activity_exit = next_ws_json(&mut restricted_socket).await;
    assert_eq!(activity_exit["_tag"], "Exit");
    assert_eq!(activity_exit["exit"]["_tag"], "Success");

    send_ws_json(
        &mut restricted_socket,
        json!({
            "_tag": "Request",
            "id": "102",
            "tag": "subscribeAuthAccess",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    let denied = next_ws_json(&mut restricted_socket).await;
    assert_eq!(denied["_tag"], "Exit");
    assert_eq!(denied["exit"]["_tag"], "Failure");
    assert_eq!(
        denied["exit"]["cause"][0]["error"]["_tag"],
        "EnvironmentAuthorizationError"
    );
    assert_eq!(
        denied["exit"]["cause"][0]["error"]["requiredScope"],
        "access:read"
    );

    send_ws_json(
        &mut restricted_socket,
        json!({
            "_tag": "Request",
            "id": "103",
            "tag": "server.consumeCodexRateLimitReset",
            "payload": { "requestId": "request-123" },
            "headers": []
        }),
    )
    .await;
    let reset_denied = next_ws_json(&mut restricted_socket).await;
    assert_eq!(reset_denied["_tag"], "Exit");
    assert_eq!(reset_denied["exit"]["_tag"], "Failure");
    assert_eq!(
        reset_denied["exit"]["cause"][0]["error"]["_tag"],
        "EnvironmentAuthorizationError"
    );
    assert_eq!(
        reset_denied["exit"]["cause"][0]["error"]["requiredScope"],
        "orchestration:operate"
    );

    let operate_pairing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(administrator_token)
            .json(&json!({ "scopes": ["orchestration:operate"] }))
            .send()
            .await
            .expect("operate pairing request"),
        StatusCode::OK,
    )
    .await;
    let operate = exchange_token(
        &client,
        &handle,
        operate_pairing["credential"]
            .as_str()
            .expect("operate credential"),
        None,
    )
    .await;
    let operate_ticket = websocket_ticket(&client, &handle, access_token(&operate)).await;
    let (mut operate_socket, _) = connect_async(format!(
        "ws://{}/ws?wsTicket={operate_ticket}",
        handle.local_addr()
    ))
    .await
    .expect("operate WebSocket");
    for (id, tag) in [
        ("105", "activity.getSnapshot"),
        ("106", "activity.listRoster"),
        ("107", "activity.listDetail"),
    ] {
        send_ws_json(
            &mut operate_socket,
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": {},
                "headers": []
            }),
        )
        .await;
        let denied = next_ws_json(&mut operate_socket).await;
        assert_eq!(denied["_tag"], "Exit");
        assert_eq!(denied["exit"]["_tag"], "Failure");
        assert_eq!(
            denied["exit"]["cause"][0]["error"]["requiredScope"],
            "orchestration:read"
        );
    }
    send_ws_json(
        &mut operate_socket,
        json!({
            "_tag": "Request",
            "id": "108",
            "tag": "subscribeActivity",
            "payload": {},
            "headers": []
        }),
    )
    .await;
    let stream_denied = next_ws_json(&mut operate_socket).await;
    assert_eq!(stream_denied["_tag"], "Exit");
    assert_eq!(stream_denied["exit"]["_tag"], "Failure");
    assert_eq!(
        stream_denied["exit"]["cause"][0]["error"]["requiredScope"],
        "orchestration:read"
    );
    for (id, tag) in [
        ("109", "activity.cancelSubtree"),
        ("1090", "activity.retrySubtreeCancellation"),
    ] {
        send_ws_json(
            &mut operate_socket,
            json!({
                "_tag": "Request",
                "id": id,
                "tag": tag,
                "payload": {},
                "headers": []
            }),
        )
        .await;
        let authorized = next_ws_json(&mut operate_socket).await;
        assert_eq!(authorized["_tag"], "Exit");
        assert_eq!(authorized["requestId"], id);
        assert_eq!(authorized["exit"]["_tag"], "Success");
        assert_eq!(authorized["exit"]["value"]["tag"], tag);
    }
    send_ws_json(
        &mut operate_socket,
        json!({
            "_tag": "Request",
            "id": "104",
            "tag": "server.consumeCodexRateLimitReset",
            "payload": { "requestId": "  " },
            "headers": []
        }),
    )
    .await;
    let reset_authorized = next_ws_json(&mut operate_socket).await;
    assert_eq!(reset_authorized["_tag"], "Exit");
    assert_eq!(reset_authorized["exit"]["_tag"], "Failure");
    assert_eq!(
        reset_authorized["exit"]["cause"][0]["error"]["_tag"],
        "ServerProviderUsageResetError"
    );
    operate_socket
        .close(None)
        .await
        .expect("close operate WebSocket");

    let clients = get_json(
        client
            .get(http_url(&handle, "/api/auth/clients"))
            .bearer_auth(administrator_token)
            .send()
            .await
            .expect("client list request"),
        StatusCode::OK,
    )
    .await;
    let restricted_session_id = clients
        .as_array()
        .expect("client list")
        .iter()
        .find(|session| session["scopes"] == json!(["orchestration:read"]))
        .and_then(|session| session["sessionId"].as_str())
        .expect("restricted session id");
    let revoked = get_json(
        client
            .post(http_url(&handle, "/api/auth/clients/revoke"))
            .bearer_auth(administrator_token)
            .json(&json!({ "sessionId": restricted_session_id }))
            .send()
            .await
            .expect("revoke restricted session"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["revoked"], true);
    timeout(Duration::from_secs(2), async {
        loop {
            match restricted_socket.next().await {
                None | Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .expect("revoked restricted WebSocket closes");
    shutdown(handle).await;
}

#[tokio::test]
async fn invalid_scope_and_missing_credentials_use_stable_error_shapes() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();

    let missing = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .json(&json!({}))
            .send()
            .await
            .expect("unauthenticated pairing request"),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(missing["_tag"], "EnvironmentAuthInvalidError");
    assert_eq!(missing["reason"], "missing_credential");

    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let invalid_scope = get_json(
        client
            .post(http_url(&handle, "/oauth/token"))
            .form(&token_form(
                DESKTOP_BOOTSTRAP,
                Some("orchestration:read unknown:scope"),
            ))
            .send()
            .await
            .expect("invalid scope request"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(invalid_scope["_tag"], "EnvironmentRequestInvalidError");
    assert_eq!(invalid_scope["reason"], "invalid_scope");

    let empty_scope = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(access_token(&administrator))
            .json(&json!({ "scopes": [] }))
            .send()
            .await
            .expect("empty delegated scope request"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(empty_scope["reason"], "invalid_scope");

    let unknown_delegated_scope = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(access_token(&administrator))
            .json(&json!({ "scopes": ["unknown:scope"] }))
            .send()
            .await
            .expect("unknown delegated scope request"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(unknown_delegated_scope["reason"], "invalid_scope");

    let invalid_device = get_json(
        client
            .post(http_url(&handle, "/oauth/token"))
            .form(
                &token_form(DESKTOP_BOOTSTRAP, None)
                    .into_iter()
                    .chain([("client_device_type", "game-console")])
                    .collect::<Vec<_>>(),
            )
            .send()
            .await
            .expect("invalid client device request"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(invalid_device["_tag"], "EnvironmentRequestInvalidError");

    let empty_label = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-token"))
            .bearer_auth(access_token(&administrator))
            .json(&json!({ "label": "   " }))
            .send()
            .await
            .expect("empty pairing label request"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(empty_label["_tag"], "EnvironmentRequestInvalidError");

    let malformed_token = get_json(
        client
            .post(http_url(&handle, "/api/auth/websocket-ticket"))
            .bearer_auth("malformed.session.token")
            .send()
            .await
            .expect("malformed token request"),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    assert_eq!(malformed_token["_tag"], "EnvironmentAuthInvalidError");
    assert_eq!(malformed_token["reason"], "invalid_credential");

    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_offer_mints_the_spec_payload_with_pinned_host_identity() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({
                "name": "AI-SERVER",
                "endpoint": "http://192.168.1.20:3773",
                "reach": "another-device",
            }))
            .send()
            .await
            .expect("pairing offer"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(offer["name"], "AI-SERVER");
    assert_eq!(offer["reach"], "another-device");
    assert_eq!(offer["endpoint"], "http://192.168.1.20:3773");
    assert!(offer["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(offer["expiresAt"].as_str().is_some_and(|at| !at.is_empty()));

    let code = offer["code"].as_str().expect("offer code");
    let payload =
        coddy_studio_server::auth_pairing_code::decode_pairing_code(code).expect("decodes");
    assert_eq!(payload.v, 1);
    assert_eq!(payload.name, "AI-SERVER");
    assert_eq!(payload.endpoint, "http://192.168.1.20:3773");
    assert_eq!(
        payload.reach,
        coddy_studio_server::auth_pairing_code::RemotePairingReach::AnotherDevice
    );
    assert!(!payload.token.is_empty());
    assert_eq!(payload.host_key.len(), 43);
    uuid::Uuid::parse_str(&payload.storage_instance_id).expect("storage id is a UUID");

    let links = get_json(
        client
            .get(http_url(&handle, "/api/auth/pairing-links"))
            .bearer_auth(token)
            .send()
            .await
            .expect("pairing links"),
        StatusCode::OK,
    )
    .await;
    assert!(
        links
            .as_array()
            .expect("link list")
            .iter()
            .any(|link| link["id"] == offer["id"])
    );
    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_offer_rejects_invalid_endpoint_and_reach_combinations() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);

    for (endpoint, reach) in [
        ("ftp://192.168.1.20:3773", "custom"),
        ("http://0.0.0.0:3773", "custom"),
        ("http://127.0.0.1:0", "custom"),
        ("http://127.0.0.1:3773", "another-device"),
        ("http://192.168.1.20:3773", "this-computer"),
    ] {
        let response = client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .json(&json!({ "name": "X", "endpoint": endpoint, "reach": reach }))
            .send()
            .await
            .expect("invalid offer");
        let body = get_json(response, StatusCode::BAD_REQUEST).await;
        assert_eq!(body["code"], "invalid_request", "{endpoint} {reach}");
        assert_eq!(
            body["reason"], "invalid_pairing_offer",
            "{endpoint} {reach}"
        );
    }
    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_offer_replays_are_idempotent_per_key() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);
    let body = json!({
        "name": "AI-SERVER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    });
    let mint = |body: Value| {
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .header("idempotency-key", "retry-key-1")
            .json(&body)
            .send()
    };
    let (first_response, second_response) = tokio::join!(mint(body.clone()), mint(body.clone()));
    let first = get_json(first_response.expect("first mint"), StatusCode::OK).await;
    let second = get_json(second_response.expect("replay"), StatusCode::OK).await;
    assert_eq!(first, second);

    let links = get_json(
        client
            .get(http_url(&handle, "/api/auth/pairing-links"))
            .bearer_auth(token)
            .send()
            .await
            .expect("pairing links"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(links.as_array().expect("link list").len(), 1);

    let conflicting = mint(json!({
        "name": "OTHER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    }))
    .await
    .expect("conflicting mint");
    let conflict_body = get_json(conflicting, StatusCode::BAD_REQUEST).await;
    assert_eq!(conflict_body["reason"], "invalid_pairing_offer");
    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_offer_authority_is_shared_across_simultaneously_live_servers() {
    let temp = TempDir::new().expect("temporary base directory");
    let first = start_desktop_server(&temp).await;
    let second = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&administrator);
    let body = json!({
        "name": "AI-SERVER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    });

    let created = get_json(
        client
            .post(http_url(&first, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .header("idempotency-key", "shared-authority-key")
            .json(&body)
            .send()
            .await
            .expect("create offer on first server"),
        StatusCode::OK,
    )
    .await;
    let replayed = get_json(
        client
            .post(http_url(&second, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .header("idempotency-key", "shared-authority-key")
            .json(&body)
            .send()
            .await
            .expect("replay offer on second server"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(replayed, created);

    let conflicting = client
        .post(http_url(&second, "/api/auth/pairing-offer"))
        .bearer_auth(token)
        .header("idempotency-key", "shared-authority-key")
        .json(&json!({
            "name": "OTHER",
            "endpoint": "http://192.168.1.20:3773",
            "reach": "another-device",
        }))
        .send()
        .await
        .expect("conflicting replay on second server");
    let conflict_body = get_json(conflicting, StatusCode::BAD_REQUEST).await;
    assert_eq!(conflict_body["reason"], "invalid_pairing_offer");

    let cancelled = get_json(
        client
            .post(http_url(&second, "/api/auth/pairing-offer/cancel"))
            .bearer_auth(token)
            .json(&json!({ "idempotencyKey": "shared-authority-key" }))
            .send()
            .await
            .expect("cancel offer on second server"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cancelled["cancelled"], true);

    let replay_after_cancellation = client
        .post(http_url(&first, "/api/auth/pairing-offer"))
        .bearer_auth(token)
        .header("idempotency-key", "shared-authority-key")
        .json(&body)
        .send()
        .await
        .expect("replay cancelled offer on first server");
    let replay_body = get_json(replay_after_cancellation, StatusCode::BAD_REQUEST).await;
    assert_eq!(replay_body["reason"], "invalid_pairing_offer");

    shutdown(second).await;
    shutdown(first).await;
}

#[tokio::test]
async fn concurrent_pairing_offer_retries_across_live_servers_return_one_result() {
    let temp = TempDir::new().expect("temporary base directory");
    let first = start_desktop_server(&temp).await;
    let second = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&administrator);
    let body = json!({
        "name": "AI-SERVER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    });

    for index in 0..16 {
        let key = format!("concurrent-shared-key-{index}");
        let mint = |handle: &ServerHandle| {
            client
                .post(http_url(handle, "/api/auth/pairing-offer"))
                .bearer_auth(token)
                .header("idempotency-key", &key)
                .json(&body)
                .send()
        };
        let (first_response, second_response) = tokio::join!(mint(&first), mint(&second));
        let first_response = first_response.expect("first concurrent mint");
        let second_response = second_response.expect("second concurrent mint");
        let statuses = [first_response.status(), second_response.status()];
        assert!(statuses.contains(&StatusCode::OK));
        assert!(
            statuses
                .iter()
                .all(|status| { matches!(*status, StatusCode::OK | StatusCode::BAD_REQUEST) })
        );
        if statuses == [StatusCode::OK, StatusCode::OK] {
            let first_result = get_json(first_response, StatusCode::OK).await;
            let second_result = get_json(second_response, StatusCode::OK).await;
            assert_eq!(first_result, second_result);
        } else if first_response.status() == StatusCode::OK {
            assert!(get_json(first_response, StatusCode::OK).await["id"].is_string());
            assert_eq!(
                get_json(second_response, StatusCode::BAD_REQUEST).await["reason"],
                "invalid_pairing_offer"
            );
        } else {
            assert_eq!(
                get_json(first_response, StatusCode::BAD_REQUEST).await["reason"],
                "invalid_pairing_offer"
            );
            assert!(get_json(second_response, StatusCode::OK).await["id"].is_string());
        }
    }

    shutdown(second).await;
    shutdown(first).await;
}

#[tokio::test]
async fn pairing_offer_cancellation_revokes_committed_grants_and_tombstones_the_key() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let token_response = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let token = access_token(&token_response);
    let body = json!({
        "name": "AI-SERVER",
        "endpoint": "http://192.168.1.20:3773",
        "reach": "another-device",
    });

    let offer = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer"))
            .bearer_auth(token)
            .header("idempotency-key", "lost-response-key")
            .json(&body)
            .send()
            .await
            .expect("pairing offer"),
        StatusCode::OK,
    )
    .await;
    assert!(offer["id"].is_string());

    let cancelled = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer/cancel"))
            .bearer_auth(token)
            .json(&json!({ "idempotencyKey": "lost-response-key" }))
            .send()
            .await
            .expect("cancel pairing offer"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cancelled["cancelled"], true);

    let share_state = get_json(
        client
            .get(http_url(&handle, "/api/auth/share-state"))
            .bearer_auth(token)
            .send()
            .await
            .expect("share state"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(share_state["desiredExposure"], "loopback");
    assert_eq!(share_state["offHostGrantCount"], 0);

    let replay = client
        .post(http_url(&handle, "/api/auth/pairing-offer"))
        .bearer_auth(token)
        .header("idempotency-key", "lost-response-key")
        .json(&body)
        .send()
        .await
        .expect("cancelled replay");
    let replay_body = get_json(replay, StatusCode::BAD_REQUEST).await;
    assert_eq!(replay_body["reason"], "invalid_pairing_offer");

    let pre_cancelled = get_json(
        client
            .post(http_url(&handle, "/api/auth/pairing-offer/cancel"))
            .bearer_auth(token)
            .json(&json!({ "idempotencyKey": "cancel-before-create" }))
            .send()
            .await
            .expect("pre-cancel pairing offer"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pre_cancelled["cancelled"], false);
    let delayed_create = client
        .post(http_url(&handle, "/api/auth/pairing-offer"))
        .bearer_auth(token)
        .header("idempotency-key", "cancel-before-create")
        .json(&body)
        .send()
        .await
        .expect("delayed create");
    let delayed_body = get_json(delayed_create, StatusCode::BAD_REQUEST).await;
    assert_eq!(delayed_body["reason"], "invalid_pairing_offer");

    shutdown(handle).await;
}

#[tokio::test]
async fn pairing_offer_requires_the_access_write_scope() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let limited_response = exchange_token(
        &client,
        &handle,
        DESKTOP_BOOTSTRAP,
        Some("orchestration:read orchestration:operate terminal:operate review:write relay:read"),
    )
    .await;
    let limited = access_token(&limited_response);

    let response = client
        .post(http_url(&handle, "/api/auth/pairing-offer"))
        .bearer_auth(limited)
        .json(&json!({
            "name": "X",
            "endpoint": "http://192.168.1.20:3773",
            "reach": "another-device",
        }))
        .send()
        .await
        .expect("scope-limited offer");
    let body = get_json(response, StatusCode::FORBIDDEN).await;
    assert_eq!(body["_tag"], "EnvironmentScopeRequiredError");
    assert_eq!(body["requiredScope"], "access:write");
    shutdown(handle).await;
}

#[tokio::test]
async fn dpop_tokens_validate_proof_binding_time_and_replay() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let signing_key = SigningKey::from_bytes((&[7_u8; 32]).into()).expect("fixture signing key");
    let token_url = http_url(&handle, "/oauth/token");
    let issued_at = unix_seconds();
    let proof = dpop_proof(
        &signing_key,
        "POST",
        &token_url,
        "token-proof-1",
        issued_at,
        None,
    );
    let response = client
        .post(&token_url)
        .header("dpop", &proof)
        .form(&token_form(DESKTOP_BOOTSTRAP, None))
        .send()
        .await
        .expect("DPoP token exchange");
    assert_credential_headers(&response);
    let issued = get_json(response, StatusCode::OK).await;
    assert_eq!(issued["token_type"], "DPoP");
    let access_token = access_token(&issued);

    let ticket_url = http_url(&handle, "/api/auth/websocket-ticket");
    let request_proof = dpop_proof(
        &signing_key,
        "POST",
        &ticket_url,
        "request-proof-1",
        unix_seconds(),
        Some(access_token),
    );
    let ticket_response = client
        .post(&ticket_url)
        .header(header::AUTHORIZATION, format!("DPoP {access_token}"))
        .header("dpop", &request_proof)
        .send()
        .await
        .expect("proof-bound ticket request");
    assert_eq!(ticket_response.status(), StatusCode::OK);

    let replay = client
        .post(&ticket_url)
        .header(header::AUTHORIZATION, format!("DPoP {access_token}"))
        .header("dpop", &request_proof)
        .send()
        .await
        .expect("replayed DPoP request");
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        replay.headers().get(header::WWW_AUTHENTICATE),
        Some(&header::HeaderValue::from_static("DPoP"))
    );

    let bearer_misuse = client
        .post(&ticket_url)
        .bearer_auth(access_token)
        .send()
        .await
        .expect("Bearer misuse request");
    assert_eq!(bearer_misuse.status(), StatusCode::UNAUTHORIZED);

    let wrong_method = dpop_proof(
        &signing_key,
        "GET",
        &ticket_url,
        "wrong-method-proof",
        unix_seconds(),
        Some(access_token),
    );
    let wrong_method_response = client
        .post(&ticket_url)
        .header(header::AUTHORIZATION, format!("DPoP {access_token}"))
        .header("dpop", wrong_method)
        .send()
        .await
        .expect("wrong-method DPoP request");
    assert_eq!(wrong_method_response.status(), StatusCode::UNAUTHORIZED);

    let wrong_hash = dpop_proof(
        &signing_key,
        "POST",
        &ticket_url,
        "wrong-hash-proof",
        unix_seconds(),
        Some("a-different-access-token"),
    );
    let wrong_hash_response = client
        .post(&ticket_url)
        .header(header::AUTHORIZATION, format!("DPoP {access_token}"))
        .header("dpop", wrong_hash)
        .send()
        .await
        .expect("wrong-hash DPoP request");
    assert_eq!(wrong_hash_response.status(), StatusCode::UNAUTHORIZED);

    let future = dpop_proof(
        &signing_key,
        "POST",
        &token_url,
        "future-proof",
        unix_seconds() + 60,
        None,
    );
    let future_response = client
        .post(&token_url)
        .header("dpop", future)
        .form(&token_form(DESKTOP_BOOTSTRAP, None))
        .send()
        .await
        .expect("future DPoP request");
    assert_eq!(future_response.status(), StatusCode::UNAUTHORIZED);

    let stale = dpop_proof(
        &signing_key,
        "POST",
        &token_url,
        "stale-proof",
        unix_seconds() - 301,
        None,
    );
    let stale_response = client
        .post(&token_url)
        .header("dpop", stale)
        .form(&token_form(DESKTOP_BOOTSTRAP, None))
        .send()
        .await
        .expect("stale DPoP request");
    assert_eq!(stale_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        stale_response.headers().get(header::WWW_AUTHENTICATE),
        Some(&header::HeaderValue::from_static("DPoP"))
    );

    // A second desktop bootstrap exchange supersedes the first session, so the
    // reverse-proxied exchange runs only after every proof check bound to the
    // first access token has completed.
    let proxied_proof = dpop_proof(
        &signing_key,
        "POST",
        &token_url.replacen("http://", "https://", 1),
        "proxied-token-proof",
        unix_seconds(),
        None,
    );
    let proxied = client
        .post(&token_url)
        .header("x-forwarded-proto", "https")
        .header("dpop", proxied_proof)
        .form(&token_form(DESKTOP_BOOTSTRAP, None))
        .send()
        .await
        .expect("reverse-proxied DPoP token exchange");
    assert_eq!(proxied.status(), StatusCode::OK);

    shutdown(handle).await;
}

#[tokio::test]
async fn dpop_replay_state_survives_a_server_restart() {
    let temp = TempDir::new().expect("temporary base directory");
    let first_config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_desktop(DESKTOP_BOOTSTRAP)
        .expect("desktop config");
    let first = ServerRuntime::start_with_registry(first_config, RpcRegistry::empty())
        .await
        .expect("first server starts");
    let port = first.local_addr().port();
    let token_url = http_url(&first, "/oauth/token");
    let signing_key = SigningKey::from_bytes((&[11_u8; 32]).into()).expect("fixture signing key");
    let proof = dpop_proof(
        &signing_key,
        "POST",
        &token_url,
        "restart-replay-proof",
        unix_seconds(),
        None,
    );
    let client = Client::new();
    let accepted = client
        .post(&token_url)
        .header("dpop", &proof)
        .form(&token_form(DESKTOP_BOOTSTRAP, None))
        .send()
        .await
        .expect("first DPoP request");
    assert_eq!(accepted.status(), StatusCode::OK);
    shutdown(first).await;

    let second_config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", port)
        .with_desktop(DESKTOP_BOOTSTRAP)
        .expect("desktop config");
    let second = ServerRuntime::start_with_registry(second_config, RpcRegistry::empty())
        .await
        .expect("second server starts");
    let replayed = client
        .post(http_url(&second, "/oauth/token"))
        .header("dpop", proof)
        .form(&token_form(DESKTOP_BOOTSTRAP, None))
        .send()
        .await
        .expect("replayed DPoP request after restart");
    assert_eq!(replayed.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        replayed.headers().get(header::WWW_AUTHENTICATE),
        Some(&header::HeaderValue::from_static("DPoP"))
    );
    shutdown(second).await;
}

#[tokio::test]
async fn sessions_pairings_consumption_and_revocation_survive_restarts() {
    let temp = TempDir::new().expect("temporary base directory");
    let client = Client::new();
    let first = start_desktop_server(&temp).await;
    let administrator = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator).to_owned();
    let pairing = get_json(
        client
            .post(http_url(&first, "/api/auth/pairing-token"))
            .bearer_auth(&administrator_token)
            .json(&json!({ "label": "Restarted client" }))
            .send()
            .await
            .expect("persistent pairing request"),
        StatusCode::OK,
    )
    .await;
    let pairing_id = pairing["id"].as_str().expect("pairing id").to_owned();
    let pairing_credential = pairing["credential"]
        .as_str()
        .expect("pairing credential")
        .to_owned();
    shutdown(first).await;

    let second = start_desktop_server(&temp).await;
    let restored_administrator = get_json(
        client
            .get(http_url(&second, "/api/auth/session"))
            .bearer_auth(&administrator_token)
            .send()
            .await
            .expect("restored administrator request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(restored_administrator["authenticated"], true);
    let restored_pairings = get_json(
        client
            .get(http_url(&second, "/api/auth/pairing-links"))
            .bearer_auth(&administrator_token)
            .send()
            .await
            .expect("restored pairing list"),
        StatusCode::OK,
    )
    .await;
    assert!(
        restored_pairings
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == pairing_id))
    );

    let paired = exchange_token(&client, &second, &pairing_credential, None).await;
    let paired_token = access_token(&paired).to_owned();
    let clients = get_json(
        client
            .get(http_url(&second, "/api/auth/clients"))
            .bearer_auth(&administrator_token)
            .send()
            .await
            .expect("client list after restored pairing exchange"),
        StatusCode::OK,
    )
    .await;
    let paired_session_id = clients
        .as_array()
        .expect("client list")
        .iter()
        .find(|session| session["client"]["label"] == "Restarted client")
        .and_then(|session| session["sessionId"].as_str())
        .expect("paired session id");
    let revoked = get_json(
        client
            .post(http_url(&second, "/api/auth/clients/revoke"))
            .bearer_auth(&administrator_token)
            .json(&json!({ "sessionId": paired_session_id }))
            .send()
            .await
            .expect("persistent client revocation"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["revoked"], true);
    shutdown(second).await;

    let third = start_desktop_server(&temp).await;
    let revoked_session = get_json(
        client
            .get(http_url(&third, "/api/auth/session"))
            .bearer_auth(&paired_token)
            .send()
            .await
            .expect("revoked session after second restart"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked_session["authenticated"], false);
    let consumed_pairing = client
        .post(http_url(&third, "/oauth/token"))
        .form(&token_form(&pairing_credential, None))
        .send()
        .await
        .expect("consumed pairing after second restart");
    assert_eq!(consumed_pairing.status(), StatusCode::UNAUTHORIZED);
    shutdown(third).await;
}

#[tokio::test]
async fn session_revocation_is_immediate_across_live_server_processes() {
    let temp = TempDir::new().expect("temporary base directory");
    let client = Client::new();
    let first = start_desktop_server(&temp).await;
    let administrator = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator).to_owned();
    let pairing = get_json(
        client
            .post(http_url(&first, "/api/auth/pairing-token"))
            .bearer_auth(&administrator_token)
            .json(&json!({ "label": "Cross-process client" }))
            .send()
            .await
            .expect("cross-process pairing request"),
        StatusCode::OK,
    )
    .await;
    let paired = exchange_token(
        &client,
        &first,
        pairing["credential"].as_str().expect("pairing credential"),
        None,
    )
    .await;
    let paired_token = access_token(&paired).to_owned();

    let second = start_desktop_server(&temp).await;
    let accepted_by_second = get_json(
        client
            .get(http_url(&second, "/api/auth/session"))
            .bearer_auth(&paired_token)
            .send()
            .await
            .expect("second server session request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(accepted_by_second["authenticated"], true);

    let clients = get_json(
        client
            .get(http_url(&first, "/api/auth/clients"))
            .bearer_auth(&administrator_token)
            .send()
            .await
            .expect("first server client list"),
        StatusCode::OK,
    )
    .await;
    let paired_session_id = clients
        .as_array()
        .expect("client list")
        .iter()
        .find(|session| session["client"]["label"] == "Cross-process client")
        .and_then(|session| session["sessionId"].as_str())
        .expect("paired session id");
    let revoked = get_json(
        client
            .post(http_url(&first, "/api/auth/clients/revoke"))
            .bearer_auth(&administrator_token)
            .json(&json!({ "sessionId": paired_session_id }))
            .send()
            .await
            .expect("first server revocation"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(revoked["revoked"], true);

    let rejected_by_second = get_json(
        client
            .get(http_url(&second, "/api/auth/session"))
            .bearer_auth(&paired_token)
            .send()
            .await
            .expect("second server revoked-session request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rejected_by_second["authenticated"], false);

    shutdown(second).await;
    shutdown(first).await;
}

#[tokio::test]
async fn simultaneous_live_server_starts_share_the_existing_store() {
    let temp = TempDir::new().expect("temporary base directory");
    let initialized = start_desktop_server(&temp).await;
    shutdown(initialized).await;

    let (first, second) = tokio::join!(start_desktop_server(&temp), start_desktop_server(&temp));
    let client = Client::new();
    let token = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let accepted_by_second = get_json(
        client
            .get(http_url(&second, "/api/auth/session"))
            .bearer_auth(access_token(&token))
            .send()
            .await
            .expect("simultaneous second-server session request"),
        StatusCode::OK,
    )
    .await;

    assert_eq!(accepted_by_second["authenticated"], true);
    shutdown(second).await;
    shutdown(first).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_starts_while_live_store_is_continuously_committed_and_checkpointed() {
    let temp = TempDir::new().expect("temporary base directory");
    let first = start_desktop_server(&temp).await;
    let client = Client::new();
    let administrator = exchange_token(&client, &first, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator).to_owned();
    let database_path = temp.path().join("userdata/state.sqlite");
    let setup = Connection::open(&database_path).expect("live-store churn setup");
    setup
        .execute_batch(
            "CREATE TABLE validation_startup_churn (
               singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
               revision INTEGER NOT NULL,
               padding BLOB NOT NULL
             );
             INSERT INTO validation_startup_churn (singleton, revision, padding)
               VALUES (1, 0, zeroblob(1048576));",
        )
        .expect("multi-batch live-store churn table");
    drop(setup);
    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = Arc::clone(&stop);
    let startup_started = Arc::new(AtomicBool::new(false));
    let writer_startup_started = Arc::clone(&startup_started);
    let startup_finished = Arc::new(AtomicBool::new(false));
    let writer_startup_finished = Arc::clone(&startup_finished);
    let checkpoints_during_startup = Arc::new(AtomicUsize::new(0));
    let writer_checkpoints_during_startup = Arc::clone(&checkpoints_during_startup);
    let (checkpoint_ready, checkpoint_observed) = std::sync::mpsc::sync_channel(1);

    let writer = std::thread::spawn(move || {
        let checkpoint = Connection::open(database_path).expect("checkpoint connection");
        checkpoint
            .busy_timeout(Duration::from_secs(1))
            .expect("checkpoint busy timeout");
        let mut checkpoint_ready = Some(checkpoint_ready);
        let mut successful_checkpoints = 0_usize;
        let mut ordinal = 0_i64;
        while !writer_stop.load(Ordering::Acquire) || ordinal < 2 {
            let cycle_started_before_startup_finished =
                !writer_startup_finished.load(Ordering::Acquire);
            checkpoint
                .execute(
                    "UPDATE validation_startup_churn SET revision = ?1 WHERE singleton = 1",
                    [ordinal],
                )
                .expect("live-store commit");
            let checkpoint_result = checkpoint
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("live-store checkpoint");
            if checkpoint_result == 0 {
                successful_checkpoints += 1;
                // Compare interval boundaries instead of sampling one in-progress flag here.
                // A checkpoint may begin before startup and finish after a fast startup has
                // completed; that cycle still overlaps startup and must not be missed.
                if cycle_started_before_startup_finished
                    && writer_startup_started.load(Ordering::Acquire)
                {
                    writer_checkpoints_during_startup.fetch_add(1, Ordering::AcqRel);
                }
                if let Some(ready) = checkpoint_ready.take() {
                    ready.send(()).expect("signal first checkpoint");
                }
            }
            ordinal += 1;
            std::thread::yield_now();
        }
        successful_checkpoints
    });
    checkpoint_observed
        .recv_timeout(Duration::from_secs(5))
        .expect("initial checkpoint signal");

    startup_started.store(true, Ordering::Release);
    let second = start_desktop_server(&temp).await;
    startup_finished.store(true, Ordering::Release);
    stop.store(true, Ordering::Release);
    let successful_checkpoints = writer.join().expect("live-store writer thread");
    assert!(
        successful_checkpoints >= 2,
        "writer must checkpoint before and during concurrent startup"
    );
    assert!(
        checkpoints_during_startup.load(Ordering::Acquire) > 0,
        "at least one commit/checkpoint cycle overlaps second-server startup"
    );
    let accepted_by_second = get_json(
        client
            .get(http_url(&second, "/api/auth/session"))
            .bearer_auth(&administrator_token)
            .send()
            .await
            .expect("second server session request"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(accepted_by_second["authenticated"], true);

    shutdown(second).await;
    shutdown(first).await;
}

#[tokio::test]
async fn auth_routes_include_browser_cors_and_preflight_headers() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let origin = "https://client.example.test";

    let session = client
        .get(http_url(&handle, "/api/auth/session"))
        .header(header::ORIGIN, origin)
        .send()
        .await
        .expect("CORS session request");
    assert_eq!(
        session.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&header::HeaderValue::from_static("*"))
    );

    let preflight = client
        .request(
            reqwest::Method::OPTIONS,
            http_url(&handle, "/api/auth/pairing-offer"),
        )
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization, content-type, dpop, idempotency-key",
        )
        .send()
        .await
        .expect("CORS preflight request");
    assert_eq!(preflight.status(), StatusCode::OK);
    assert_eq!(
        preflight.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&header::HeaderValue::from_static("*"))
    );
    let allowed_headers = preflight
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|value| value.to_str().ok())
        .expect("allowed headers");
    for expected in ["authorization", "content-type", "dpop", "idempotency-key"] {
        assert!(allowed_headers.contains(expected));
    }

    shutdown(handle).await;
}

#[tokio::test]
async fn cookie_authenticated_post_requires_same_origin() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let cookie = browser_session_cookie(&client, &handle).await;
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let administrator_token = access_token(&administrator);
    let same_origin = format!("http://{}", handle.local_addr());
    let dispatch = |origin: Option<&str>| {
        let request = client
            .post(http_url(&handle, "/api/orchestration/dispatch"))
            .header(header::COOKIE, &cookie)
            .json(&json!({}));
        match origin {
            Some(origin) => request.header(header::ORIGIN, origin),
            None => request,
        }
    };

    let missing = dispatch(None)
        .send()
        .await
        .expect("dispatch without Origin");
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);
    let missing = missing.json::<Value>().await.expect("forbidden body");
    assert_eq!(missing["_tag"], "EnvironmentOperationForbiddenError");
    assert_eq!(missing["reason"], "origin_not_allowed");
    let foreign = dispatch(Some("http://evil.localhost:9999"))
        .send()
        .await
        .expect("dispatch with foreign Origin");
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    let same = dispatch(Some(&same_origin))
        .send()
        .await
        .expect("dispatch with same Origin");
    assert_ne!(same.status(), StatusCode::FORBIDDEN);
    let bearer = client
        .post(http_url(&handle, "/api/orchestration/dispatch"))
        .bearer_auth(administrator_token)
        .json(&json!({}))
        .send()
        .await
        .expect("bearer dispatch without Origin");
    assert_ne!(bearer.status(), StatusCode::FORBIDDEN);

    // Auth routes resolve their method through the same gate.
    let ticket = client
        .post(http_url(&handle, "/api/auth/websocket-ticket"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .expect("cookie ticket without Origin");
    assert_eq!(ticket.status(), StatusCode::FORBIDDEN);
    let ticket = client
        .post(http_url(&handle, "/api/auth/websocket-ticket"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, &same_origin)
        .send()
        .await
        .expect("cookie ticket with same Origin");
    assert_eq!(ticket.status(), StatusCode::OK);

    // Cookie reads keep working without Origin.
    for path in ["/api/auth/session", "/api/auth/share-state"] {
        let read = client
            .get(http_url(&handle, path))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .expect("cookie read without Origin");
        assert_eq!(read.status(), StatusCode::OK, "{path}");
    }

    shutdown(handle).await;
}

#[tokio::test]
async fn cookie_authenticated_websocket_upgrade_requires_same_origin() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let cookie = browser_session_cookie(&client, &handle).await;
    let administrator = exchange_token(&client, &handle, DESKTOP_BOOTSTRAP, None).await;
    let ticket = websocket_ticket(&client, &handle, access_token(&administrator)).await;
    let same_origin = format!("http://{}", handle.local_addr());
    let upgrade = |query: &str, origin: Option<&str>| {
        let mut request = format!("ws://{}/ws{query}", handle.local_addr())
            .into_client_request()
            .expect("WebSocket request");
        let headers = request.headers_mut();
        headers.insert(header::COOKIE, cookie.parse().expect("cookie header"));
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().expect("origin header"));
        }
        connect_async(request)
    };

    for origin in [None, Some("http://other:1")] {
        let error = upgrade("", origin)
            .await
            .expect_err("cross-origin cookie upgrade must be refused");
        assert!(
            matches!(
                &error,
                tungstenite::Error::Http(response) if response.status() == StatusCode::FORBIDDEN
            ),
            "{origin:?}: {error:?}"
        );
    }
    let (mut socket, _) = upgrade("", Some(&same_origin))
        .await
        .expect("same-origin cookie upgrade");
    socket.close(None).await.expect("close same-origin socket");
    let (mut socket, _) = upgrade(&format!("?wsTicket={ticket}"), None)
        .await
        .expect("ticket upgrade without Origin");
    socket.close(None).await.expect("close ticket socket");

    shutdown(handle).await;
}

#[tokio::test]
async fn desktop_app_origins_do_not_pass_cookie_checks() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let cookie = browser_session_cookie(&client, &handle).await;

    // The desktop app authenticates with a bearer token, never the cookie.
    for origin in [
        "coddy-studio://app",
        "coddy-studio-dev://app",
        "coddy-studio-other://app",
    ] {
        let response = client
            .post(http_url(&handle, "/api/orchestration/dispatch"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, origin)
            .json(&json!({}))
            .send()
            .await
            .expect("app-origin dispatch");
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin}");
    }

    shutdown(handle).await;
}

#[tokio::test]
async fn a_loopback_proxy_forwarded_host_is_the_cookie_origin() {
    let temp = TempDir::new().expect("temporary base directory");
    let handle = start_desktop_server(&temp).await;
    let client = Client::new();
    let cookie = browser_session_cookie(&client, &handle).await;
    let dispatch = |origin: String| {
        client
            .post(http_url(&handle, "/api/orchestration/dispatch"))
            .header(header::COOKIE, &cookie)
            // nginx's default `proxy_set_header Host $proxy_host`.
            .header("x-forwarded-host", "coddy-studio.example.test")
            .header("x-forwarded-proto", "https")
            .header(header::ORIGIN, origin)
            .json(&json!({}))
            .send()
    };

    // The test client is a loopback peer, like a reverse proxy on this machine.
    let proxied = dispatch("https://coddy-studio.example.test".to_owned())
        .await
        .expect("proxied dispatch");
    assert_ne!(proxied.status(), StatusCode::FORBIDDEN);
    let socket_origin = dispatch(format!("http://{}", handle.local_addr()))
        .await
        .expect("socket-origin dispatch");
    assert_eq!(
        socket_origin.status(),
        StatusCode::FORBIDDEN,
        "the forwarded host replaces the proxy's own Host"
    );

    shutdown(handle).await;
}

async fn browser_session_cookie(client: &Client, handle: &ServerHandle) -> String {
    client
        .post(http_url(handle, "/api/auth/browser-session"))
        .json(&json!({ "credential": DESKTOP_BOOTSTRAP }))
        .send()
        .await
        .expect("browser session request")
        .headers()
        .get(header::SET_COOKIE)
        .expect("session cookie")
        .to_str()
        .expect("ASCII session cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned()
}

async fn start_desktop_server(temp: &TempDir) -> ServerHandle {
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_desktop(DESKTOP_BOOTSTRAP)
        .expect("valid desktop configuration");
    let mut registry = RpcRegistry::empty();
    registry.register_unary("server.getConfig", |_request, _cancellation| async {
        Ok(json!({}))
    });
    registry.register_unary(
        "server.consumeCodexRateLimitReset",
        |_request, _cancellation| async {
            Err(json!({
                "_tag": "ServerProviderUsageResetError",
                "message": "Codex reset request ID is required.",
            }))
        },
    );
    for tag in [
        "activity.getSnapshot",
        "activity.listRoster",
        "activity.listDetail",
        "activity.cancelSubtree",
        "activity.retrySubtreeCancellation",
        "agentSessions.scan",
        "agentSessions.import",
    ] {
        let response_tag = tag.to_owned();
        registry.register_unary(tag, move |_request, _cancellation| {
            let response_tag = response_tag.clone();
            async move { Ok(json!({ "tag": response_tag })) }
        });
    }
    registry.register_stream("subscribeActivity", |_request, _cancellation| {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender
            .try_send(Ok(vec![json!({ "tag": "subscribeActivity" })]))
            .expect("activity fixture chunk");
        receiver
    });
    ServerRuntime::start_with_registry(config, registry)
        .await
        .expect("server starts")
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

async fn send_ws_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, value: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(tungstenite::Message::Text(value.to_string().into()))
        .await
        .expect("send WebSocket JSON");
}

async fn next_ws_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = timeout(Duration::from_secs(2), next_frame_past_heartbeat(socket))
        .await
        .expect("WebSocket response timeout")
        .expect("WebSocket remains open")
        .expect("valid WebSocket frame");
    serde_json::from_str(frame.to_text().expect("text WebSocket frame"))
        .expect("valid WebSocket JSON")
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

async fn shutdown(handle: ServerHandle) {
    handle.shutdown();
    timeout(Duration::from_secs(2), handle.join())
        .await
        .expect("server shutdown timeout")
        .expect("server joins");
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_secs()
        .try_into()
        .expect("fixture timestamp fits i64")
}

fn dpop_proof(
    signing_key: &SigningKey,
    method: &str,
    url: &str,
    jti: &str,
    issued_at: i64,
    access_token: Option<&str>,
) -> String {
    let point = signing_key.verifying_key().to_sec1_point(false);
    let header = json!({
        "typ": "dpop+jwt",
        "alg": "ES256",
        "jwk": {
            "kty": "EC",
            "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(point.x().expect("P-256 x coordinate")),
            "y": URL_SAFE_NO_PAD.encode(point.y().expect("P-256 y coordinate")),
        }
    });
    let mut payload = json!({
        "htm": method,
        "htu": normalize_dpop_url(url),
        "jti": jti,
        "iat": issued_at,
    });
    if let Some(access_token) = access_token {
        payload["ath"] = json!(URL_SAFE_NO_PAD.encode(Sha256::digest(access_token.as_bytes())));
    }
    let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("DPoP header JSON"));
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("DPoP payload JSON"));
    let signing_input = format!("{header}.{payload}");
    let digest = Sha256::digest(signing_input.as_bytes());
    let signature: Signature = signing_key
        .sign_prehash(&digest)
        .expect("sign DPoP fixture");
    format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn normalize_dpop_url(url: &str) -> String {
    let mut url = url::Url::parse(url).expect("fixture URL");
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn auth_fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts/fixtures/auth-http")
}
