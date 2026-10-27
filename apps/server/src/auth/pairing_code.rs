use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const REMOTE_PAIRING_CODE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemotePairingReach {
    AnotherDevice,
    ThisComputer,
    Custom,
}

/// Rust mirror of `packages/contracts/src/remotePairing.ts`.
/// Field order is the canonical serialization order pinned by the parity fixture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingCodePayload {
    pub v: u32,
    pub endpoint: String,
    pub name: String,
    pub token: String,
    pub host_key: String,
    pub reach: RemotePairingReach,
    pub storage_instance_id: String,
}

#[derive(Debug, Error)]
pub enum PairingCodeError {
    #[error("pairing code is not valid base64url")]
    Encoding(#[from] base64::DecodeError),
    #[error("pairing code payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported pairing code version {v}")]
    UnsupportedVersion { v: u64 },
    #[error("pairing endpoint is not a valid HTTP URL: {0}")]
    Endpoint(#[from] url::ParseError),
}

pub fn encode_pairing_code(payload: &RemotePairingCodePayload) -> Result<String, PairingCodeError> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload)?))
}

pub fn decode_pairing_code(code: &str) -> Result<RemotePairingCodePayload, PairingCodeError> {
    let bytes = URL_SAFE_NO_PAD.decode(code.trim())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let version = value.get("v").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(REMOTE_PAIRING_CODE_VERSION)) {
        return Err(PairingCodeError::UnsupportedVersion {
            v: version.unwrap_or(0),
        });
    }
    Ok(serde_json::from_value(value)?)
}

#[must_use]
pub fn pairing_deep_link(code: &str) -> String {
    format!("coddy-studio://pair?code={code}")
}

pub fn browser_pair_url(endpoint: &str, code: &str) -> Result<String, PairingCodeError> {
    let mut url = url::Url::parse(endpoint)?;
    url.set_path("/pair");
    url.set_query(Some(&format!("code={code}")));
    Ok(url.to_string())
}
