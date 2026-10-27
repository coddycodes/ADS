//! Single-use, short-lived capability tokens that bootstrap a gateway session. A token is
//! bound to the gateway listener, upstream port, thread, and Coddy Studio session it was minted
//! for; redeeming it consumes it.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::signed_token::{sign, verify};

pub const GATEWAY_TOKEN_PURPOSE: &str = "preview-gateway";
pub const CAPABILITY_TTL_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCapabilityClaims {
    pub gateway_port: u16,
    pub upstream_port: u16,
    pub thread_id: String,
    pub session_id: String,
    pub expires_at: u64,
    pub jti: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CapabilityError {
    #[error("invalid")]
    Invalid,
    #[error("expired")]
    Expired,
    #[error("replayed")]
    Replayed,
    #[error("wrong target")]
    WrongTarget,
}

pub struct CapabilityIssuer {
    secret: Vec<u8>,
    /// Redeemed `jti` -> the token's `expires_at`, kept only while the token could still
    /// pass the expiry check.
    redeemed: Mutex<HashMap<String, u64>>,
}

impl CapabilityIssuer {
    pub fn new(secret: Vec<u8>) -> Self {
        Self {
            secret,
            redeemed: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the signed token and its expiry in epoch milliseconds.
    pub fn issue(
        &self,
        gateway_port: u16,
        upstream_port: u16,
        thread_id: &str,
        session_id: &str,
        now_ms: u64,
    ) -> (String, u64) {
        let expires_at = now_ms.saturating_add(CAPABILITY_TTL_MS);
        let claims = GatewayCapabilityClaims {
            gateway_port,
            upstream_port,
            thread_id: thread_id.to_owned(),
            session_id: session_id.to_owned(),
            expires_at,
            jti: super::random_base64url(16),
        };
        let token = sign(&self.secret, GATEWAY_TOKEN_PURPOSE, &claims)
            .expect("capability claims always serialize");
        (token, expires_at)
    }

    /// Verifies signature, expiry, gateway port, and single use; prunes expired jtis.
    pub fn redeem(
        &self,
        token: &str,
        gateway_port: u16,
        now_ms: u64,
    ) -> Result<GatewayCapabilityClaims, CapabilityError> {
        let claims: GatewayCapabilityClaims =
            verify(&self.secret, GATEWAY_TOKEN_PURPOSE, token).ok_or(CapabilityError::Invalid)?;
        if now_ms >= claims.expires_at {
            return Err(CapabilityError::Expired);
        }
        if claims.gateway_port != gateway_port {
            return Err(CapabilityError::WrongTarget);
        }
        let mut redeemed = self.redeemed.lock().unwrap_or_else(PoisonError::into_inner);
        redeemed.retain(|_, expires_at| *expires_at > now_ms);
        if redeemed.contains_key(&claims.jti) {
            return Err(CapabilityError::Replayed);
        }
        redeemed.insert(claims.jti.clone(), claims.expires_at);
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_capability_redeems_once() {
        let issuer = CapabilityIssuer::new(vec![7; 32]);
        let (token, exp) = issuer.issue(40001, 5173, "t1", "s1", 1_000);
        assert_eq!(exp, 61_000);
        let claims = issuer.redeem(&token, 40001, 2_000).unwrap();
        assert_eq!(
            (
                claims.upstream_port,
                claims.thread_id.as_str(),
                claims.session_id.as_str()
            ),
            (5173, "t1", "s1")
        );
        assert_eq!(
            issuer.redeem(&token, 40001, 2_001),
            Err(CapabilityError::Replayed)
        );
    }

    #[test]
    fn capability_rejects_expired_wrong_port_and_tampered() {
        let issuer = CapabilityIssuer::new(vec![7; 32]);
        let (token, _) = issuer.issue(40001, 5173, "t1", "s1", 1_000);
        assert_eq!(
            issuer.redeem(&token, 40001, 61_001),
            Err(CapabilityError::Expired)
        );
        let (token, _) = issuer.issue(40001, 5173, "t1", "s1", 1_000);
        assert_eq!(
            issuer.redeem(&token, 40002, 2_000),
            Err(CapabilityError::WrongTarget)
        );
        assert_eq!(
            issuer.redeem("abc.def", 40001, 2_000),
            Err(CapabilityError::Invalid)
        );
        let other = CapabilityIssuer::new(vec![8; 32]);
        let (foreign, _) = other.issue(40001, 5173, "t1", "s1", 1_000);
        assert_eq!(
            issuer.redeem(&foreign, 40001, 2_000),
            Err(CapabilityError::Invalid)
        );
    }

    #[test]
    fn wrong_port_attempt_does_not_consume_the_capability() {
        let issuer = CapabilityIssuer::new(vec![7; 32]);
        let (token, _) = issuer.issue(40001, 5173, "t1", "s1", 1_000);
        assert_eq!(
            issuer.redeem(&token, 40002, 2_000),
            Err(CapabilityError::WrongTarget)
        );
        assert!(issuer.redeem(&token, 40001, 2_001).is_ok());
    }

    #[test]
    fn replay_after_pruning_is_expired_never_accepted() {
        let issuer = CapabilityIssuer::new(vec![7; 32]);
        let (first, _) = issuer.issue(40001, 5173, "t1", "s1", 1_000);
        issuer.redeem(&first, 40001, 2_000).unwrap();
        // Redeeming a later token prunes the first token's jti (expired at 61_000).
        let (second, _) = issuer.issue(40001, 5173, "t1", "s1", 70_000);
        issuer.redeem(&second, 40001, 70_001).unwrap();
        assert_eq!(
            issuer.redeem(&first, 40001, 70_002),
            Err(CapabilityError::Expired)
        );
        assert_eq!(
            issuer.redeem(&second, 40001, 70_003),
            Err(CapabilityError::Replayed)
        );
    }

    #[test]
    fn token_signed_for_another_purpose_is_invalid() {
        let issuer = CapabilityIssuer::new(vec![7; 32]);
        let (token, _) = issuer.issue(40001, 5173, "t1", "s1", 1_000);
        let claims: GatewayCapabilityClaims =
            crate::signed_token::verify(&[7; 32], GATEWAY_TOKEN_PURPOSE, &token).unwrap();
        let foreign = crate::signed_token::sign(&[7; 32], "asset", &claims).unwrap();
        assert_eq!(
            issuer.redeem(&foreign, 40001, 2_000),
            Err(CapabilityError::Invalid)
        );
    }
}
