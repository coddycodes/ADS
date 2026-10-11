//! Preview gateway core: capability tokens, gateway sessions, and the pure header,
//! cookie, and `Location` rewriting the proxy applies in both directions.

pub mod capability;
pub mod frame;
pub mod proxy;
pub mod registry;
pub mod rewrite;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use capability::GatewayCapabilityClaims;

/// Authority the gateway cookie carries: one previewed upstream port reached by one
/// BiBCode principal for one thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewaySession {
    pub gateway_port: u16,
    pub upstream_port: u16,
    pub thread_id: String,
    pub principal_session_id: String,
    pub principal_expires_at_ms: u64,
    /// The browser-mode BiBCode UI that frames this preview; only it may frame
    /// the gateway's responses for this session (see `frame::apply_frame_policy`).
    pub ui_origin: Option<String>,
}

/// In-memory gateway sessions keyed by the random id stored in the gateway cookie.
#[derive(Default)]
pub struct GatewaySessions {
    sessions: Mutex<HashMap<String, GatewaySession>>,
}

impl GatewaySessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores a session for redeemed capability `claims` and returns its cookie value.
    pub fn create(
        &self,
        claims: &GatewayCapabilityClaims,
        principal_expires_at_ms: u64,
        ui_origin: Option<String>,
    ) -> String {
        let id = random_base64url(32);
        let session = GatewaySession {
            gateway_port: claims.gateway_port,
            upstream_port: claims.upstream_port,
            thread_id: claims.thread_id.clone(),
            principal_session_id: claims.session_id.clone(),
            principal_expires_at_ms,
            ui_origin,
        };
        self.lock().insert(id.clone(), session);
        id
    }

    /// The live session for `id`. A session whose principal has expired
    /// (`principal_expires_at_ms <= now_ms`) is evicted and treated as absent.
    pub fn get(&self, id: &str, now_ms: u64) -> Option<GatewaySession> {
        let mut sessions = self.lock();
        let session = sessions.get(id)?;
        if session.principal_expires_at_ms <= now_ms {
            sessions.remove(id);
            return None;
        }
        Some(session.clone())
    }

    /// Drops every gateway session minted for BiBCode session `session_id`.
    pub fn remove_for_principal(&self, session_id: &str) {
        self.lock()
            .retain(|_, session| session.principal_session_id != session_id);
    }

    /// Drops every gateway session served by gateway listener `gateway_port`.
    pub fn remove_for_port(&self, gateway_port: u16) {
        self.lock()
            .retain(|_, session| session.gateway_port != gateway_port);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, GatewaySession>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `len` random bytes, base64url without padding. Failure of the OS RNG is unrecoverable
/// for a security token, so it panics rather than minting a predictable value.
fn random_base64url(len: usize) -> String {
    let mut bytes = vec![0_u8; len];
    getrandom::fill(&mut bytes).expect("OS random source is unavailable");
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::capability::GatewayCapabilityClaims;
    use super::*;

    fn claims(gateway_port: u16, session_id: &str) -> GatewayCapabilityClaims {
        GatewayCapabilityClaims {
            gateway_port,
            upstream_port: 5173,
            thread_id: "t1".into(),
            session_id: session_id.into(),
            expires_at: 0,
            jti: "j".into(),
        }
    }

    #[test]
    fn sessions_create_get_and_remove() {
        let sessions = GatewaySessions::new();
        let a = sessions.create(&claims(40001, "s1"), 9_000, None);
        let b = sessions.create(&claims(40001, "s2"), 9_000, None);
        let c = sessions.create(&claims(40002, "s1"), 9_000, None);
        assert_ne!(a, b);
        assert_eq!(a.len(), 43, "32 random bytes, base64url without padding");
        assert_eq!(
            sessions.get(&a, 1_000),
            Some(GatewaySession {
                gateway_port: 40001,
                upstream_port: 5173,
                thread_id: "t1".into(),
                principal_session_id: "s1".into(),
                principal_expires_at_ms: 9_000,
                ui_origin: None,
            })
        );
        assert_eq!(sessions.get("missing", 1_000), None);

        sessions.remove_for_principal("s1");
        assert_eq!(
            (sessions.get(&a, 1_000), sessions.get(&c, 1_000)),
            (None, None)
        );
        assert!(sessions.get(&b, 1_000).is_some());

        let d = sessions.create(&claims(40002, "s3"), 9_000, None);
        sessions.remove_for_port(40001);
        assert_eq!(sessions.get(&b, 1_000), None);
        assert!(sessions.get(&d, 1_000).is_some());
    }

    #[test]
    fn expired_principal_session_is_ignored_and_evicted() {
        let sessions = GatewaySessions::new();
        let id = sessions.create(&claims(40001, "s1"), 9_000, None);
        assert!(sessions.get(&id, 8_999).is_some());
        assert_eq!(sessions.get(&id, 9_000), None);
        // Evicted: even a clock that moved backwards does not resurrect it.
        assert_eq!(sessions.get(&id, 1_000), None);
    }
}
