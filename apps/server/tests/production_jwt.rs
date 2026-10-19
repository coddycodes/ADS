#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use axum::http::{HeaderMap, Uri};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{
    Signer as _, SigningKey, VerifyingKey,
    pkcs8::{DecodePrivateKey, DecodePublicKey},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use bibcode_server::production::connect_mcp::{
    ConnectMcpConfig, ConnectMcpService, EndpointRuntime, PairingCredential, PairingIssuer,
    PreviewInvoker,
};
use bibcode_server::production::http_routes::{JsonOperation, RouteContext};
use bibcode_server::production::jwt::PersistentJwtCodec;

const NOW: i64 = 1_800_000_000;
const TYP: &str = "bibcode-cloud-mint+jwt";
const ISSUER: &str = "https://relay.example";
const AUDIENCE: &str = "bibcode-env:environment-1";

fn proof_claims() -> Value {
    json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": "cloud-user-1",
        "jti": "proof-id-1",
        "iat": NOW,
        "exp": NOW + 300,
        "environmentId": "environment-1",
        "nonce": "nonce-1",
        "scope": ["environment:connect"],
        "clientProofKeyThumbprint": "proof-key-thumbprint",
        "cnf": { "jkt": "proof-key-thumbprint" }
    })
}

fn public_jwk(public_pem: &str) -> String {
    let key = VerifyingKey::from_public_key_pem(public_pem).expect("valid public PEM");
    json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "x": URL_SAFE_NO_PAD.encode(key.as_bytes()),
        "alg": "EdDSA",
        "use": "sig"
    })
    .to_string()
}

fn token_header(token: &str) -> Value {
    let encoded = token.split('.').next().expect("JWT header");
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).expect("base64url header"))
        .expect("JSON header")
}
fn encoded_json(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("JSON"))
}

fn replace_segment(token: &str, index: usize, replacement: &str) -> String {
    let mut segments = token.split('.').map(str::to_owned).collect::<Vec<_>>();
    segments[index] = replacement.to_owned();
    segments.join(".")
}
fn sign_compact_segments(private_pem: &str, header: &str, claims: &str) -> String {
    let signing_key = SigningKey::from_pkcs8_pem(private_pem).expect("private key");
    let signing_input = format!("{header}.{claims}");
    let signature = signing_key.sign(signing_input.as_bytes());
    format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

async fn open_error(path: &std::path::Path) -> String {
    PersistentJwtCodec::open(path)
        .await
        .err()
        .expect("opening invalid key material must fail")
        .to_string()
}
fn route_context() -> RouteContext {
    RouteContext {
        headers: HeaderMap::new(),
        uri: Uri::from_static("http://127.0.0.1:43123/connect"),
        cancellation: CancellationToken::new(),
    }
}

#[tokio::test]
async fn persists_one_restrictive_atomic_ed25519_identity_across_restarts() {
    let temp = TempDir::new().expect("temporary directory");
    let path = temp.path().join("secrets/environment-signing-key.json");

    let first = PersistentJwtCodec::open(&path).await.expect("first open");
    let first_pair = first.key_pair().await.expect("first key pair");
    let second = PersistentJwtCodec::open(&path).await.expect("second open");
    let second_pair = second.key_pair().await.expect("second key pair");

    assert_eq!(first_pair, second_pair);
    assert!(first_pair.0.contains("BEGIN PRIVATE KEY"));
    assert!(first_pair.1.contains("BEGIN PUBLIC KEY"));
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap())
            .expect("key directory")
            .count(),
        1,
        "atomic persistence must not leave temporary artifacts"
    );

    #[cfg(unix)]
    {
        let directory_mode = std::fs::metadata(path.parent().unwrap())
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }
}

#[tokio::test]
async fn signs_typed_eddsa_jwts_and_produces_connect_mcp_callbacks() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");

    let token = codec.sign(TYP, proof_claims()).await.expect("signed JWT");
    assert_eq!(token_header(&token), json!({ "alg": "EdDSA", "typ": TYP }));
    let _callbacks: bibcode_server::production::connect_mcp::JwtCodec = codec.jwt_codec();
}

#[tokio::test]
async fn persistent_callbacks_sign_link_proofs_and_verify_health_requests_end_to_end() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");
    let (_, public_pem) = codec.key_pair().await.expect("key pair");

    let endpoint = EndpointRuntime::new(|config| async move {
        Ok(if config.is_null() {
            json!({ "status": "disabled" })
        } else {
            json!({ "status": "running" })
        })
    });
    let pairing = PairingIssuer::new(|_| async {
        Ok(PairingCredential {
            credential: "unused".to_owned(),
            expires_at: "2027-01-15T08:00:00Z".to_owned(),
        })
    });
    let preview = PreviewInvoker::new(|_, _, _, _, cancellation| async move {
        if cancellation.is_cancelled() {
            Err("cancelled".to_owned())
        } else {
            Ok(json!({}))
        }
    });
    let service = ConnectMcpService::open(
        temp.path().join("connect.sqlite3"),
        ConnectMcpConfig {
            environment_id: "environment-1".to_owned(),
            descriptor: json!({
                "environmentId": "environment-1",
                "label": "Local",
                "platform": "win32",
                "architecture": "x64"
            }),
            mcp_endpoint: "http://127.0.0.1:43123/mcp".to_owned(),
            open_url_endpoint: "http://127.0.0.1:43123/api/preview/open-url".to_owned(),
            now_epoch_seconds: Arc::new(|| NOW),
            max_mcp_credentials: 4,
            max_mcp_sessions: 4,
        },
        codec.jwt_codec(),
        endpoint,
        pairing,
        preview,
        None,
    )
    .await
    .expect("connect service");

    let link_proof = service
        .json(
            JsonOperation::ConnectLinkProof,
            Some(json!({
                "challenge": "challenge-1",
                "relayIssuer": ISSUER,
                "endpoint": {
                    "providerKind": "cloudflare_tunnel",
                    "url": "https://managed.example"
                },
                "origin": {
                    "localHttpHost": "127.0.0.1",
                    "localHttpPort": 43123
                }
            })),
            route_context(),
        )
        .await
        .expect("link proof");
    assert_eq!(
        link_proof.body.as_str().expect("proof").split('.').count(),
        3
    );

    service
        .json(
            JsonOperation::ConnectRelayConfig,
            Some(json!({
                "relayUrl": ISSUER,
                "relayIssuer": ISSUER,
                "cloudUserId": "cloud-user-1",
                "environmentCredential": "environment-secret",
                "cloudMintPublicKey": public_pem,
                "endpointRuntime": {
                    "providerKind": "cloudflare_tunnel",
                    "connectorToken": "fixture"
                }
            })),
            route_context(),
        )
        .await
        .expect("relay config");

    let mut health_claims = proof_claims();
    health_claims["scope"] = json!(["environment:status"]);
    let health_token = codec
        .sign("bibcode-cloud-health+jwt", health_claims)
        .await
        .expect("health proof");
    let health = service
        .json(
            JsonOperation::ConnectHealth,
            Some(json!({ "proof": health_token })),
            route_context(),
        )
        .await
        .expect("health response");
    assert_eq!(health.body["status"], "online");
    assert_eq!(
        health.body["proof"]
            .as_str()
            .expect("response proof")
            .split('.')
            .count(),
        3
    );
}
#[tokio::test]
async fn verifies_spki_pem_and_ed25519_public_jwk_into_typed_cloud_proof() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");
    let (_, public_pem) = codec.key_pair().await.expect("key pair");
    let token = codec.sign(TYP, proof_claims()).await.expect("signed JWT");

    for public_key in [public_pem.clone(), public_jwk(&public_pem)] {
        let proof = codec
            .verify(public_key, TYP, token.clone(), ISSUER, AUDIENCE, NOW)
            .await
            .expect("verified proof");
        assert_eq!(proof.issuer, ISSUER);
        assert_eq!(proof.subject, "cloud-user-1");
        assert_eq!(proof.jwt_id, "proof-id-1");
        assert_eq!(proof.scope, ["environment:connect"]);
        assert_eq!(
            proof.client_proof_key_thumbprint.as_deref(),
            Some("proof-key-thumbprint")
        );
        assert_eq!(
            proof.confirmation_thumbprint,
            proof.client_proof_key_thumbprint
        );
    }
}

#[tokio::test]
async fn rejects_corrupt_persisted_keypairs_and_unusable_storage_paths() {
    let temp = TempDir::new().expect("temporary directory");
    let first_path = temp.path().join("first.json");
    let second_path = temp.path().join("second.json");
    PersistentJwtCodec::open(&first_path)
        .await
        .expect("first codec");
    PersistentJwtCodec::open(&second_path)
        .await
        .expect("second codec");
    let first: Value =
        serde_json::from_slice(&tokio::fs::read(&first_path).await.expect("first record"))
            .expect("first JSON");
    let second: Value =
        serde_json::from_slice(&tokio::fs::read(&second_path).await.expect("second record"))
            .expect("second JSON");

    let malformed = temp.path().join("malformed.json");
    tokio::fs::write(&malformed, b"{")
        .await
        .expect("malformed fixture");
    assert!(
        open_error(&malformed)
            .await
            .contains("failed to decode JWT keypair")
    );

    let unsupported = temp.path().join("unsupported.json");
    tokio::fs::write(
        &unsupported,
        serde_json::to_vec(&json!({
            "version": 2,
            "privateKey": first["privateKey"],
            "publicKey": first["publicKey"]
        }))
        .expect("unsupported JSON"),
    )
    .await
    .expect("unsupported fixture");
    assert!(
        open_error(&unsupported)
            .await
            .contains("unsupported keypair version 2")
    );

    let invalid_private = temp.path().join("invalid-private.json");
    tokio::fs::write(
        &invalid_private,
        serde_json::to_vec(&json!({
            "version": 1,
            "privateKey": "not a private key",
            "publicKey": first["publicKey"]
        }))
        .expect("private JSON"),
    )
    .await
    .expect("private fixture");
    assert!(
        open_error(&invalid_private)
            .await
            .contains("invalid PKCS#8 private key")
    );

    let invalid_public = temp.path().join("invalid-public.json");
    tokio::fs::write(
        &invalid_public,
        serde_json::to_vec(&json!({
            "version": 1,
            "privateKey": first["privateKey"],
            "publicKey": "not a public key"
        }))
        .expect("public JSON"),
    )
    .await
    .expect("public fixture");
    assert!(
        open_error(&invalid_public)
            .await
            .contains("invalid SPKI public key")
    );

    let mismatch = temp.path().join("mismatch.json");
    tokio::fs::write(
        &mismatch,
        serde_json::to_vec(&json!({
            "version": 1,
            "privateKey": first["privateKey"],
            "publicKey": second["publicKey"]
        }))
        .expect("mismatch JSON"),
    )
    .await
    .expect("mismatch fixture");
    assert!(
        open_error(&mismatch)
            .await
            .contains("private and public keys do not form a pair")
    );

    let parent_file = temp.path().join("parent-file");
    tokio::fs::write(&parent_file, b"occupied")
        .await
        .expect("parent file");
    assert!(
        open_error(&parent_file.join("key.json"))
            .await
            .contains("failed to restrict JWT keypair path")
    );

    let directory_path = temp.path().join("directory-key.json");
    tokio::fs::create_dir(&directory_path)
        .await
        .expect("directory fixture");
    assert!(
        open_error(&directory_path)
            .await
            .contains("failed to read JWT keypair")
    );
}

#[tokio::test]
async fn signing_rejects_invalid_types_and_registered_claims() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");

    for typ in [
        "", "bad
kind",
    ] {
        assert!(codec.sign(typ, proof_claims()).await.is_err());
    }
    assert!(codec.sign(&"x".repeat(129), proof_claims()).await.is_err());
    assert!(codec.sign(TYP, json!([])).await.is_err());

    for field in ["iss", "sub", "aud", "jti"] {
        let mut claims = proof_claims();
        claims[field] = json!("   ");
        assert!(codec.sign(TYP, claims).await.is_err(), "field {field}");
    }

    let mut reversed_time = proof_claims();
    reversed_time["exp"] = reversed_time["iat"].clone();
    assert!(codec.sign(TYP, reversed_time).await.is_err());
}

#[tokio::test]
async fn verification_rejects_malformed_tokens_signatures_and_public_keys() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");
    let (private_pem, public_pem) = codec.key_pair().await.expect("key pair");
    let valid = codec.sign(TYP, proof_claims()).await.expect("token");
    let header = valid.split('.').next().expect("header");
    let malformed_claims = sign_compact_segments(&private_pem, header, "%%%");
    assert!(
        codec
            .verify(
                public_pem.clone(),
                TYP,
                malformed_claims,
                ISSUER,
                AUDIENCE,
                NOW
            )
            .await
            .is_err()
    );

    for token in ["header", "header.claims", "a.b.c.d"] {
        assert!(
            codec
                .verify(
                    public_pem.clone(),
                    TYP,
                    token.to_owned(),
                    ISSUER,
                    AUDIENCE,
                    NOW
                )
                .await
                .is_err()
        );
    }

    let invalid_header_encoding = replace_segment(&valid, 0, "%%%");
    let invalid_header_json = replace_segment(&valid, 0, &URL_SAFE_NO_PAD.encode(b"not-json"));
    let wrong_header = replace_segment(
        &valid,
        0,
        &encoded_json(&json!({ "alg": "HS256", "typ": TYP })),
    );
    let invalid_signature_encoding = replace_segment(&valid, 2, "%%%");
    let invalid_signature_length = replace_segment(&valid, 2, "AA");
    for token in [
        invalid_header_encoding,
        invalid_header_json,
        wrong_header,
        invalid_signature_encoding,
        invalid_signature_length,
    ] {
        assert!(
            codec
                .verify(public_pem.clone(), TYP, token, ISSUER, AUDIENCE, NOW)
                .await
                .is_err()
        );
    }

    let malformed_jwk = "{".to_owned();
    let wrong_jwk = json!({
        "kty": "EC",
        "crv": "Ed25519",
        "x": URL_SAFE_NO_PAD.encode([0_u8; 32]),
        "alg": "EdDSA",
        "use": "sig"
    })
    .to_string();
    let invalid_x_encoding = json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "x": "%%%",
        "alg": "EdDSA",
        "use": "sig"
    })
    .to_string();
    let short_x = json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "x": URL_SAFE_NO_PAD.encode([1_u8; 3]),
        "alg": "EdDSA",
        "use": "sig"
    })
    .to_string();
    let invalid_point = json!({
        "kty": "OKP",
        "crv": "Ed25519",
        "x": URL_SAFE_NO_PAD.encode([255_u8; 32]),
        "alg": "EdDSA",
        "use": "sig"
    })
    .to_string();
    for key in [
        "not a PEM key".to_owned(),
        malformed_jwk,
        wrong_jwk,
        invalid_x_encoding,
        short_x,
        invalid_point,
    ] {
        assert!(
            codec
                .verify(key, TYP, valid.clone(), ISSUER, AUDIENCE, NOW)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn verification_enforces_cloud_claim_shape_and_time_boundaries() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");
    let (_, public_pem) = codec.key_pair().await.expect("key pair");

    let mut malformed_cases = Vec::new();

    let mut missing_environment = proof_claims();
    missing_environment
        .as_object_mut()
        .expect("claims object")
        .remove("environmentId");
    malformed_cases.push(missing_environment);

    let mut blank_environment = proof_claims();
    blank_environment["environmentId"] = json!(" ");
    malformed_cases.push(blank_environment);

    let mut blank_nonce = proof_claims();
    blank_nonce["nonce"] = json!(" ");
    malformed_cases.push(blank_nonce);

    let mut empty_scope = proof_claims();
    empty_scope["scope"] = json!([]);
    malformed_cases.push(empty_scope);

    let mut blank_scope = proof_claims();
    blank_scope["scope"] = json!([" "]);
    malformed_cases.push(blank_scope);

    let mut old = proof_claims();
    old["iat"] = json!(NOW - 361);
    old["exp"] = json!(NOW + 60);
    malformed_cases.push(old);

    let mut expired = proof_claims();
    expired["iat"] = json!(NOW - 120);
    expired["exp"] = json!(NOW - 61);
    malformed_cases.push(expired);

    for claims in malformed_cases {
        let token = codec
            .sign(TYP, claims)
            .await
            .expect("registered claims remain signable");
        assert!(
            codec
                .verify(public_pem.clone(), TYP, token, ISSUER, AUDIENCE, NOW)
                .await
                .is_err()
        );
    }

    let mut optional_claims = proof_claims();
    optional_claims
        .as_object_mut()
        .expect("claims object")
        .remove("clientProofKeyThumbprint");
    optional_claims
        .as_object_mut()
        .expect("claims object")
        .remove("cnf");
    let optional_token = codec
        .sign(TYP, optional_claims)
        .await
        .expect("optional claims");
    let escaped_pem = public_pem.replace('\n', "\\n");
    let proof = codec
        .verify(escaped_pem, TYP, optional_token, ISSUER, AUDIENCE, NOW)
        .await
        .expect("escaped PEM");
    assert_eq!(proof.client_proof_key_thumbprint, None);
    assert_eq!(proof.confirmation_thumbprint, None);
}
#[tokio::test]
async fn rejects_tampering_and_invalid_registered_claims() {
    let temp = TempDir::new().expect("temporary directory");
    let codec = PersistentJwtCodec::open(temp.path().join("keypair.json"))
        .await
        .expect("codec");
    let (_, public_pem) = codec.key_pair().await.expect("key pair");

    let valid = codec.sign(TYP, proof_claims()).await.expect("signed JWT");
    let mut tampered = valid.clone().into_bytes();
    let signature_start = valid.rfind('.').expect("signature separator") + 1;
    tampered[signature_start] = if tampered[signature_start] == b'A' {
        b'B'
    } else {
        b'A'
    };
    let tampered = String::from_utf8(tampered).expect("ASCII JWT");

    let invalid_cases = [
        (tampered, TYP, ISSUER, AUDIENCE, NOW),
        (valid.clone(), "wrong+jwt", ISSUER, AUDIENCE, NOW),
        (valid.clone(), TYP, "https://wrong.example", AUDIENCE, NOW),
        (valid.clone(), TYP, ISSUER, "wrong-audience", NOW),
        (valid.clone(), TYP, ISSUER, AUDIENCE, NOW + 361),
    ];
    for (token, typ, issuer, audience, now) in invalid_cases {
        assert!(
            codec
                .verify(public_pem.clone(), typ, token, issuer, audience, now)
                .await
                .is_err()
        );
    }

    let mut future = proof_claims();
    future["iat"] = json!(NOW + 61);
    future["exp"] = json!(NOW + 120);
    let future = codec.sign(TYP, future).await.expect("signed future JWT");
    assert!(
        codec
            .verify(public_pem.clone(), TYP, future, ISSUER, AUDIENCE, NOW)
            .await
            .is_err()
    );

    let mut missing_jti = proof_claims();
    missing_jti.as_object_mut().unwrap().remove("jti");
    assert!(codec.sign(TYP, missing_jti).await.is_err());
}
