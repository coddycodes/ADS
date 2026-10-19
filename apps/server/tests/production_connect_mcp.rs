use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use bibcode_server::production::connect_mcp::{
    ConnectMcpConfig, ConnectMcpService, DecodedCloudProof, EndpointRuntime, JwtCodec,
    PairingCredential, PairingIssuer, PreviewInvoker, PreviewScope,
};
use bibcode_server::production::http_routes::{JsonOperation, RouteContext};

fn context(headers: HeaderMap) -> RouteContext {
    RouteContext {
        headers,
        uri: Uri::from_static("http://127.0.0.1:43123/mcp"),
        cancellation: CancellationToken::new(),
    }
}

async fn setup_service(
    temp: &TempDir,
) -> (
    ConnectMcpService,
    Arc<Mutex<Vec<(PreviewScope, String, Value)>>>,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let preview_calls = Arc::clone(&calls);
    let jwt = JwtCodec::new(
        |typ, payload| async move { Ok(format!("signed:{typ}:{}", payload["jti"])) },
        |_key, typ, token, _issuer, _audience, _now| async move {
            let scope = if typ.contains("health") {
                "environment:status"
            } else {
                "environment:connect"
            };
            Ok(DecodedCloudProof {
                issuer: "https://relay.example".into(),
                subject: "cloud-user".into(),
                audience: "bibcode-env:env-1".into(),
                jwt_id: token.clone(),
                issued_at: 1_700_000_000,
                expires_at: 1_700_000_120,
                environment_id: "env-1".into(),
                nonce: format!("nonce-{token}"),
                scope: vec![scope.into()],
                client_proof_key_thumbprint: (scope == "environment:connect")
                    .then(|| "thumbprint".into()),
                confirmation_thumbprint: (scope == "environment:connect")
                    .then(|| "thumbprint".into()),
            })
        },
        || async {
            Ok((
                "private-key".into(),
                "-----BEGIN PUBLIC KEY-----\npublic-key\n-----END PUBLIC KEY-----".into(),
            ))
        },
    );
    let endpoint = EndpointRuntime::new(|config| async move {
        Ok(if config.is_null() {
            json!({"status":"disabled"})
        } else {
            json!({"status":"running"})
        })
    });
    let pairing = PairingIssuer::new(|thumbprint| async move {
        assert_eq!(thumbprint, "thumbprint");
        Ok(PairingCredential {
            credential: "pairing-secret".into(),
            expires_at: "2026-07-10T12:02:00Z".into(),
        })
    });
    let preview = PreviewInvoker::new(move |scope, operation, input, _tab, cancellation| {
        let preview_calls = Arc::clone(&preview_calls);
        async move {
            if cancellation.is_cancelled() {
                return Err("cancelled".into());
            }
            preview_calls.lock().await.push((scope, operation, input));
            Ok(json!({"url":"https://example.test","title":"Example"}))
        }
    });
    let config = ConnectMcpConfig {
        environment_id: "env-1".into(),
        descriptor: json!({
            "environmentId":"env-1",
            "label":"Local",
            "platform":"win32",
            "architecture":"x64"
        }),
        mcp_endpoint: "http://127.0.0.1:43123/mcp".into(),
        open_url_endpoint: "http://127.0.0.1:43123/api/preview/open-url".into(),
        now_epoch_seconds: Arc::new(|| 1_700_000_000),
        max_mcp_credentials: 4,
        max_mcp_sessions: 4,
    };
    let service = ConnectMcpService::open(
        temp.path().join("connect.sqlite3"),
        config,
        jwt,
        endpoint,
        pairing,
        preview,
        None,
    )
    .await
    .expect("connect service");
    (service, calls)
}

#[derive(Clone, Copy)]
struct DependencyFailures {
    key_pair: bool,
    sign: bool,
    verify: bool,
    endpoint: bool,
    endpoint_status: &'static str,
    pairing: bool,
}

impl Default for DependencyFailures {
    fn default() -> Self {
        Self {
            key_pair: false,
            sign: false,
            verify: false,
            endpoint: false,
            endpoint_status: "running",
            pairing: false,
        }
    }
}

async fn setup_service_with_failures(
    temp: &TempDir,
    name: &str,
    failures: DependencyFailures,
) -> ConnectMcpService {
    let jwt = JwtCodec::new(
        move |typ, payload| async move {
            if failures.sign {
                Err("injected signing failure".to_owned())
            } else {
                Ok(format!("signed:{typ}:{}", payload["jti"]))
            }
        },
        move |_key, typ, token, _issuer, _audience, _now| async move {
            if failures.verify {
                return Err("injected verification failure".to_owned());
            }
            let is_health = typ.contains("health");
            let scope = if is_health {
                "environment:status"
            } else {
                "environment:connect"
            };
            Ok(DecodedCloudProof {
                issuer: "https://relay.example".into(),
                subject: "cloud-user".into(),
                audience: "bibcode-env:env-1".into(),
                jwt_id: token.clone(),
                issued_at: 1_700_000_000,
                expires_at: 1_700_000_120,
                environment_id: "env-1".into(),
                nonce: format!("nonce-{token}"),
                scope: vec![scope.into()],
                client_proof_key_thumbprint: (!is_health).then(|| "thumbprint".into()),
                confirmation_thumbprint: (!is_health).then(|| "thumbprint".into()),
            })
        },
        move || async move {
            if failures.key_pair {
                Err("injected key pair failure".to_owned())
            } else {
                Ok((
                    "private-key".into(),
                    "-----BEGIN PUBLIC KEY-----\npublic-key\n-----END PUBLIC KEY-----".into(),
                ))
            }
        },
    );
    let endpoint = EndpointRuntime::new(move |_config| async move {
        if failures.endpoint {
            Err("injected endpoint failure".to_owned())
        } else {
            Ok(json!({"status":failures.endpoint_status}))
        }
    });
    let pairing = PairingIssuer::new(move |_thumbprint| async move {
        if failures.pairing {
            Err("injected pairing failure".to_owned())
        } else {
            Ok(PairingCredential {
                credential: "pairing-secret".into(),
                expires_at: "2026-07-10T12:02:00Z".into(),
            })
        }
    });
    let preview = PreviewInvoker::new(|_scope, _operation, _input, _tab, _cancellation| async {
        Ok(Value::Null)
    });
    ConnectMcpService::open(
        temp.path().join(name),
        ConnectMcpConfig {
            environment_id: "env-1".into(),
            descriptor: json!({"environmentId":"env-1"}),
            mcp_endpoint: "http://127.0.0.1:43123/mcp".into(),
            open_url_endpoint: "http://127.0.0.1:43123/api/preview/open-url".into(),
            now_epoch_seconds: Arc::new(|| 1_700_000_000),
            max_mcp_credentials: 4,
            max_mcp_sessions: 4,
        },
        jwt,
        endpoint,
        pairing,
        preview,
        None,
    )
    .await
    .expect("connect service")
}

async fn link_service(service: &ConnectMcpService) {
    service
        .json(
            JsonOperation::ConnectRelayConfig,
            Some(json!({
                "relayUrl":"https://relay.example",
                "relayIssuer":"https://relay.example",
                "cloudUserId":"cloud-user",
                "environmentCredential":"environment-secret",
                "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\ncloud-key\n-----END PUBLIC KEY-----",
                "endpointRuntime":null,
            })),
            context(HeaderMap::new()),
        )
        .await
        .expect("link service");
}

#[tokio::test]
async fn cloud_config_is_persistent_atomic_and_replay_safe() {
    let temp = TempDir::new().unwrap();
    let (service, _) = setup_service(&temp).await;

    let unlinked = service
        .json(
            JsonOperation::ConnectLinkState,
            None,
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(unlinked.body["linked"], false);

    let linked = service
        .json(
            JsonOperation::ConnectRelayConfig,
            Some(json!({
                "relayUrl":"https://relay.example/",
                "relayIssuer":"https://relay.example",
                "cloudUserId":"cloud-user",
                "environmentCredential":"environment-secret",
                "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\ncloud-key\n-----END PUBLIC KEY-----",
                "endpointRuntime":{"providerKind":"cloudflare_tunnel","connectorToken":"token"}
            })),
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(linked.body["ok"], true);

    let health = service
        .json(
            JsonOperation::ConnectHealth,
            Some(json!({"proof":"health-jti"})),
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(health.body["status"], "online");
    assert_eq!(
        health.headers.get("pragma").map(String::as_str),
        Some("no-cache")
    );

    let replay = service
        .json(
            JsonOperation::ConnectHealth,
            Some(json!({"proof":"health-jti"})),
            context(HeaderMap::new()),
        )
        .await;
    let replay = match replay {
        Ok(_) => panic!("replayed health proof was accepted"),
        Err(error) => error,
    };
    assert_eq!(replay.status(), StatusCode::CONFLICT);

    drop(service);
    let (reopened, _) = setup_service(&temp).await;
    let state = reopened
        .json(
            JsonOperation::ConnectLinkState,
            None,
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(state.body["cloudUserId"], "cloud-user");
    assert_eq!(state.body["relayUrl"], "https://relay.example/");
    assert_eq!(state.body["managedEndpoint"], true);
    // A restarted server brings the stored tunnel back.
    assert_eq!(
        reopened.restore_managed_endpoint().await.unwrap(),
        Some(json!({"status":"running"}))
    );
}

#[tokio::test]
async fn link_proof_mint_and_unlink_preserve_connect_wire_contracts() {
    let temp = TempDir::new().unwrap();
    let (service, _) = setup_service(&temp).await;
    service
        .json(
            JsonOperation::ConnectRelayConfig,
            Some(json!({
                "relayUrl":"https://relay.example",
                "cloudUserId":"cloud-user",
                "environmentCredential":"environment-secret",
                "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\ncloud-key\n-----END PUBLIC KEY-----",
                "endpointRuntime":null
            })),
            context(HeaderMap::new()),
        )
        .await
        .unwrap();

    let proof = service
        .json(
            JsonOperation::ConnectLinkProof,
            Some(json!({
                "challenge":"challenge-1",
                "relayIssuer":"https://relay.example/",
                "endpoint":{"providerKind":"cloudflare_tunnel","url":"https://managed.example"},
                "origin":{"localHttpHost":"127.0.0.1","localHttpPort":43123}
            })),
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert!(
        proof
            .body
            .as_str()
            .unwrap()
            .starts_with("signed:bibcode-env-link+jwt:")
    );

    let minted = service
        .json(
            JsonOperation::ConnectMintCredential,
            Some(json!({"proof":"mint-jti"})),
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(minted.body["credential"], "pairing-secret");
    assert!(
        minted.body["proof"]
            .as_str()
            .unwrap()
            .starts_with("signed:bibcode-env-mint+jwt:")
    );

    let unlinked = service
        .json(
            JsonOperation::ConnectUnlink,
            None,
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(unlinked.body["endpointRuntimeStatus"]["status"], "disabled");
    let state = service
        .json(
            JsonOperation::ConnectLinkState,
            None,
            context(HeaderMap::new()),
        )
        .await
        .unwrap();
    assert_eq!(state.body["linked"], false);
}

#[tokio::test]
async fn mcp_requires_provider_credential_and_terminates_bounded_sessions() {
    let temp = TempDir::new().unwrap();
    let (service, calls) = setup_service(&temp).await;
    let issued = service
        .issue_mcp_credential("thread-1", "codex-1")
        .await
        .unwrap();

    let unauthorized = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
                .unwrap(),
            context(HeaderMap::new()),
        )
        .await;
    let unauthorized = match unauthorized {
        Ok(_) => panic!("unauthenticated MCP request was accepted"),
        Err(error) => error,
    };
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&issued.authorization_header).unwrap(),
    );
    let initialized = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({
                "jsonrpc":"2.0","id":1,"method":"initialize",
                "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}
            }))
            .unwrap(),
            context(headers.clone()),
        )
        .await
        .unwrap();
    assert_eq!(initialized.status, 200);
    let session_id = initialized.headers["mcp-session-id"].clone();
    headers.insert(
        "mcp-session-id",
        HeaderValue::from_str(&session_id).unwrap(),
    );

    let listed = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
                .unwrap(),
            context(headers.clone()),
        )
        .await
        .unwrap();
    let listed: Value = serde_json::from_slice(&listed.body).unwrap();
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "preview_status")
    );
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "preview_snapshot")
    );

    let called = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"preview_status","arguments":{"tabId":"tab-1"}}
            }))
            .unwrap(),
            context(headers.clone()),
        )
        .await
        .unwrap();
    let called: Value = serde_json::from_slice(&called.body).unwrap();
    assert_eq!(called["result"]["isError"], false);
    assert_eq!(calls.lock().await.len(), 1);

    let deleted = service
        .mcp(Method::DELETE, Vec::new(), context(headers.clone()))
        .await
        .unwrap();
    assert_eq!(deleted.status, 204);
    let reused = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":4,"method":"ping","params":{}}))
                .unwrap(),
            context(headers),
        )
        .await;
    let reused = match reused {
        Ok(_) => panic!("terminated MCP session was reused"),
        Err(error) => error,
    };
    assert_eq!(reused.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cancellation_stops_preview_tool_invocation() {
    let temp = TempDir::new().unwrap();
    let (service, calls) = setup_service(&temp).await;
    let issued = service
        .issue_mcp_credential("thread", "provider")
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&issued.authorization_header).unwrap(),
    );
    let initialized = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
                .unwrap(),
            context(headers.clone()),
        )
        .await
        .unwrap();
    headers.insert(
        "mcp-session-id",
        HeaderValue::from_str(&initialized.headers["mcp-session-id"]).unwrap(),
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let response = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"tools/call",
                "params":{"name":"preview_status","arguments":{}}
            }))
            .unwrap(),
            RouteContext {
                headers,
                uri: Uri::from_static("http://127.0.0.1/mcp"),
                cancellation,
            },
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(body["result"]["isError"], true);
    assert_eq!(
        body["result"]["structuredContent"]["error"],
        json!({
            "_tag":"PreviewAutomationExecutionError",
            "message":"cancelled"
        })
    );
    assert!(calls.lock().await.is_empty());
}

#[tokio::test]
async fn http_route_adapters_and_provider_revocation_are_ready_for_central_wiring() {
    let temp = TempDir::new().unwrap();
    let (service, _) = setup_service(&temp).await;
    assert!(
        service
            .json_http(
                JsonOperation::OrchestrationSnapshot,
                None,
                context(HeaderMap::new()),
            )
            .await
            .is_err()
    );
    assert!(
        service
            .mcp_http(Method::POST, Vec::new(), context(HeaderMap::new()))
            .await
            .is_err()
    );

    let first = service
        .issue_mcp_credential("thread-1", "provider-1")
        .await
        .unwrap();
    service
        .revoke_mcp_provider_session(&first.provider_session_id)
        .await;
    let mut revoked_headers = HeaderMap::new();
    revoked_headers.insert(
        "authorization",
        HeaderValue::from_str(&first.authorization_header).unwrap(),
    );
    let revoked = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).unwrap(),
            context(revoked_headers),
        )
        .await;
    let revoked = match revoked {
        Ok(_) => panic!("revoked MCP credential was accepted"),
        Err(error) => error,
    };
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    let second = service
        .issue_mcp_credential("thread-2", "provider-2")
        .await
        .unwrap();
    assert_ne!(first.authorization_header, second.authorization_header);
    service.revoke_all_mcp_credentials().await;
    let mut revoked_headers = HeaderMap::new();
    revoked_headers.insert(
        "authorization",
        HeaderValue::from_str(&second.authorization_header).unwrap(),
    );
    let revoked = service
        .mcp(
            Method::POST,
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":2,"method":"initialize"})).unwrap(),
            context(revoked_headers),
        )
        .await;
    let revoked = match revoked {
        Ok(_) => panic!("globally revoked MCP credential was accepted"),
        Err(error) => error,
    };
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn malformed_connect_and_mcp_requests_preserve_transport_errors() {
    let temp = TempDir::new().unwrap();
    let (service, _) = setup_service(&temp).await;

    for (operation, payload) in [
        (JsonOperation::ConnectLinkProof, None),
        (JsonOperation::ConnectRelayConfig, Some(json!([]))),
        (JsonOperation::ConnectHealth, Some(json!({}))),
        (
            JsonOperation::ConnectMintCredential,
            Some(json!({"proof":7})),
        ),
    ] {
        let error = service
            .json(operation, payload, context(HeaderMap::new()))
            .await
            .err()
            .expect("malformed connect payload should fail");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    for payload in [
        json!({
            "challenge":"challenge",
            "relayIssuer":"https://relay.example",
        }),
        json!({
            "challenge":"challenge",
            "relayIssuer":"https://relay.example",
            "endpoint":{"providerKind":"unsupported"},
            "origin":{"localHttpHost":"127.0.0.1","localHttpPort":43123},
        }),
        json!({
            "challenge":"challenge",
            "relayIssuer":"https://relay.example",
            "endpoint":{"providerKind":"cloudflare_tunnel"},
            "origin":{"localHttpHost":"example.test","localHttpPort":43123},
        }),
    ] {
        let error = service
            .json(
                JsonOperation::ConnectLinkProof,
                Some(payload),
                context(HeaderMap::new()),
            )
            .await
            .err()
            .expect("invalid link proof should fail");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    for payload in [
        json!({
            "relayUrl":"http://relay.example",
            "cloudUserId":"user",
            "environmentCredential":"credential",
            "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\nkey\n-----END PUBLIC KEY-----",
        }),
        json!({
            "relayUrl":"https://relay.example",
            "cloudUserId":"",
            "environmentCredential":"credential",
            "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\nkey\n-----END PUBLIC KEY-----",
        }),
        json!({
            "relayUrl":"https://relay.example",
            "cloudUserId":"user",
            "environmentCredential":"credential",
            "cloudMintPublicKey":"not-a-public-key",
        }),
    ] {
        let error = service
            .json(
                JsonOperation::ConnectRelayConfig,
                Some(payload),
                context(HeaderMap::new()),
            )
            .await
            .err()
            .expect("invalid relay config should fail");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    let issued = service
        .issue_mcp_credential("thread", "provider")
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&issued.authorization_header).unwrap(),
    );

    let unsupported = service
        .mcp(Method::GET, Vec::new(), context(headers.clone()))
        .await
        .err()
        .expect("unsupported method should fail");
    assert_eq!(unsupported.status(), StatusCode::METHOD_NOT_ALLOWED);

    for body in [
        b"not-json".to_vec(),
        serde_json::to_vec(&json!({"id":1})).unwrap(),
    ] {
        let error = service
            .mcp(Method::POST, body, context(headers.clone()))
            .await
            .err()
            .expect("invalid JSON-RPC request should fail");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    let missing_session = service
        .mcp(Method::DELETE, Vec::new(), context(headers))
        .await
        .err()
        .expect("delete without session should fail");
    assert_eq!(missing_session.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn connect_dependencies_map_failures_without_partial_success() {
    let temp = TempDir::new().unwrap();
    let link_payload = || {
        json!({
            "challenge":"challenge",
            "relayIssuer":"https://relay.example",
            "endpoint":{"providerKind":"cloudflare_tunnel"},
            "origin":{"localHttpHost":"127.0.0.1","localHttpPort":43123},
        })
    };

    for (name, failures) in [
        (
            "keypair.sqlite3",
            DependencyFailures {
                key_pair: true,
                ..DependencyFailures::default()
            },
        ),
        (
            "link-sign.sqlite3",
            DependencyFailures {
                sign: true,
                ..DependencyFailures::default()
            },
        ),
    ] {
        let service = setup_service_with_failures(&temp, name, failures).await;
        let error = service
            .json(
                JsonOperation::ConnectLinkProof,
                Some(link_payload()),
                context(HeaderMap::new()),
            )
            .await
            .err()
            .expect("link dependency failure");
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    let endpoint_failure = setup_service_with_failures(
        &temp,
        "endpoint.sqlite3",
        DependencyFailures {
            endpoint: true,
            ..DependencyFailures::default()
        },
    )
    .await;
    for (operation, payload) in [
        (
            JsonOperation::ConnectRelayConfig,
            Some(json!({
                "relayUrl":"https://relay.example",
                "cloudUserId":"cloud-user",
                "environmentCredential":"environment-secret",
                "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\ncloud-key\n-----END PUBLIC KEY-----",
                "endpointRuntime":{"providerKind":"cloudflare_tunnel"},
            })),
        ),
        (JsonOperation::ConnectUnlink, None),
    ] {
        let error = endpoint_failure
            .json(operation, payload, context(HeaderMap::new()))
            .await
            .err()
            .expect("endpoint dependency failure");
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    let unavailable = setup_service_with_failures(
        &temp,
        "unavailable.sqlite3",
        DependencyFailures {
            endpoint_status: "failed",
            ..DependencyFailures::default()
        },
    )
    .await;
    let unavailable_error = unavailable
        .json(
            JsonOperation::ConnectRelayConfig,
            Some(json!({
                "relayUrl":"https://relay.example",
                "cloudUserId":"cloud-user",
                "environmentCredential":"environment-secret",
                "cloudMintPublicKey":"-----BEGIN PUBLIC KEY-----\ncloud-key\n-----END PUBLIC KEY-----",
                "endpointRuntime":{"providerKind":"cloudflare_tunnel"},
            })),
            context(HeaderMap::new()),
        )
        .await
        .err()
        .expect("unavailable endpoint");
    assert_eq!(unavailable_error.status(), StatusCode::SERVICE_UNAVAILABLE);

    let verification = setup_service_with_failures(
        &temp,
        "verification.sqlite3",
        DependencyFailures {
            verify: true,
            ..DependencyFailures::default()
        },
    )
    .await;
    link_service(&verification).await;
    // A link without a managed endpoint has no tunnel to restore.
    assert_eq!(verification.restore_managed_endpoint().await.unwrap(), None);
    for operation in [
        JsonOperation::ConnectHealth,
        JsonOperation::ConnectMintCredential,
    ] {
        let error = verification
            .json(
                operation,
                Some(json!({"proof":"proof"})),
                context(HeaderMap::new()),
            )
            .await
            .err()
            .expect("verification dependency failure");
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    }

    let signing = setup_service_with_failures(
        &temp,
        "response-sign.sqlite3",
        DependencyFailures {
            sign: true,
            ..DependencyFailures::default()
        },
    )
    .await;
    link_service(&signing).await;
    for (operation, proof) in [
        (JsonOperation::ConnectHealth, "health-sign"),
        (JsonOperation::ConnectMintCredential, "mint-sign"),
    ] {
        let error = signing
            .json(
                operation,
                Some(json!({"proof":proof})),
                context(HeaderMap::new()),
            )
            .await
            .err()
            .expect("response signing dependency failure");
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    let pairing = setup_service_with_failures(
        &temp,
        "pairing.sqlite3",
        DependencyFailures {
            pairing: true,
            ..DependencyFailures::default()
        },
    )
    .await;
    link_service(&pairing).await;
    let pairing_error = pairing
        .json(
            JsonOperation::ConnectMintCredential,
            Some(json!({"proof":"mint-pairing"})),
            context(HeaderMap::new()),
        )
        .await
        .err()
        .expect("pairing dependency failure");
    assert_eq!(pairing_error.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
