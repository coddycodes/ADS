use std::path::PathBuf;

use coddy_studio_server::auth_pairing_code::{
    PairingCodeError, REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload, RemotePairingReach,
    browser_pair_url, decode_pairing_code, encode_pairing_code, pairing_deep_link,
};

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/remote-pairing")
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_directory().join(name)).expect("read fixture")
}

#[test]
fn canonical_payload_fixture_round_trips_through_the_rust_mirror() {
    let fixture = read_fixture("payload.json");
    let payload: RemotePairingCodePayload =
        serde_json::from_str(fixture.trim()).expect("decode payload");
    assert_eq!(payload.v, REMOTE_PAIRING_CODE_VERSION);
    assert_eq!(payload.endpoint, "http://192.168.1.20:3773");
    assert_eq!(payload.reach, RemotePairingReach::AnotherDevice);
    assert_eq!(
        serde_json::to_value(&payload).expect("encode payload"),
        serde_json::from_str::<serde_json::Value>(fixture.trim()).expect("decode fixture value")
    );
}

#[test]
fn code_fixture_matches_the_rust_encoder() {
    let payload: RemotePairingCodePayload =
        serde_json::from_str(read_fixture("payload.json").trim()).expect("decode payload");
    assert_eq!(
        encode_pairing_code(&payload).expect("encode"),
        read_fixture("code.txt").trim()
    );
    let decoded = decode_pairing_code(read_fixture("code.txt").trim()).expect("decode");
    assert_eq!(decoded, payload);
}

#[test]
fn unsupported_version_is_classified_distinctly() {
    let code = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        URL_SAFE_NO_PAD.encode(read_fixture("unsupported-version.json").trim())
    };
    assert!(matches!(
        decode_pairing_code(&code),
        Err(PairingCodeError::UnsupportedVersion { v: 2 })
    ));
}

#[test]
fn deep_link_and_browser_url_shapes_are_stable() {
    assert_eq!(pairing_deep_link("abc"), "coddy-studio://pair?code=abc");
    assert_eq!(
        browser_pair_url("http://192.168.1.20:3773", "abc").expect("browser url"),
        "http://192.168.1.20:3773/pair?code=abc"
    );
}
