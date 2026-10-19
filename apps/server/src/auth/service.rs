use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use subtle::ConstantTimeEq;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, OwnedMutexGuard, broadcast};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    HostIdentity,
    dpop::DpopVerifier,
    limits::{
        MAX_ACTIVE_PAIRING_OFFERS, MAX_ACTIVE_PAIRING_OFFERS_PER_PRINCIPAL, MAX_ACTIVE_PAIRINGS,
        MAX_ACTIVE_SESSIONS,
    },
    model::{
        ADMINISTRATIVE_SCOPES, ALL_SCOPES, AuthAccessChange, AuthAccessEvent, AuthDescriptor,
        ClientMetadata, ClientSessionView, PairingCredentialResult, PairingLinkView,
        PairingOfferResult, Principal, STANDARD_SCOPES, ShareExposureState,
    },
    secret_store::SecretStore,
    token::{SessionClaims, TokenError, TokenSigner, WebSocketClaims},
};
use crate::config::{ServerConfig, ServerMode};
use crate::persistence::{
    AuthAuthoritySnapshot as PersistedAuthAuthoritySnapshot,
    AuthPairingLink as PersistedPairingLink, AuthPairingOffer as PersistedPairingOffer,
    AuthSession as PersistedAuthSession, AuthSessionClient as PersistedAuthSessionClient,
    AuthSessionDeliveryState, NewAuthPairingOffer, NewAuthSession, PersistenceError, Repositories,
};

const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const DPOP_SESSION_TTL_MS: i64 = 60 * 60 * 1_000;
const WEBSOCKET_TICKET_TTL_MS: i64 = 5 * 60 * 1_000;
/// Pending pairing sessions and offer reservations older than this are crash
/// orphans. Younger rows may still belong to a live request or confirmation and
/// are left alone.
const PENDING_PAIRING_SWEEP_GRACE_MS: i64 = 2 * 60 * 1_000;
const PENDING_PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const PAIRING_TTL_MS: i64 = 5 * 60 * 1_000;
const CLOUD_PAIRING_TTL_MS: i64 = 2 * 60 * 1_000;
const DESKTOP_BOOTSTRAP_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
/// Subject of every session minted from the desktop bootstrap credential.
const DESKTOP_BOOTSTRAP_SUBJECT: &str = "desktop-bootstrap";
const PAIRING_ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const PAIRING_LENGTH: usize = 12;
const PAIRING_REJECTION_LIMIT: u8 =
    (u8::MAX as usize / PAIRING_ALPHABET.len() * PAIRING_ALPHABET.len()) as u8;
const ACCESS_EVENT_CAPACITY: usize = 64;
const AUTHORITY_CONVERGENCE_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const PAIRING_REACH_ANOTHER_DEVICE: &str = "another-device";
pub(crate) const PAIRING_REACH_THIS_COMPUTER: &str = "this-computer";
pub(crate) const PAIRING_REACH_CUSTOM: &str = "custom";
pub(crate) const PAIRING_REACH_VALUES: [&str; 3] = [
    PAIRING_REACH_ANOTHER_DEVICE,
    PAIRING_REACH_THIS_COMPUTER,
    PAIRING_REACH_CUSTOM,
];

fn is_valid_pairing_reach(value: &str) -> bool {
    PAIRING_REACH_VALUES.contains(&value)
}

#[derive(Clone, Debug)]
pub enum AuthError {
    MissingCredential,
    InvalidCredential,
    InvalidScope,
    ScopeNotGranted,
    ScopeRequired(String),
    CurrentSessionRevokeNotAllowed,
    /// A cookie-authenticated mutation or WebSocket upgrade from an untrusted `Origin`.
    Forbidden,
    Internal(String),
}

#[derive(Clone)]
pub struct AuthService {
    descriptor: AuthDescriptor,
    host_identity: HostIdentity,
    desktop_bootstrap: Option<DesktopBootstrap>,
    signer: TokenSigner,
    state: Arc<Mutex<AuthState>>,
    issuance: Arc<Mutex<()>>,
    pairing_offer_issuance: Arc<Mutex<()>>,
    repositories: Option<Repositories>,
    access_events: Arc<broadcast::Sender<AuthAccessEvent>>,
    access_revision: Arc<AtomicU64>,
    authority_watcher_running: Arc<AtomicBool>,
    /// Redeemed single-use websocket ticket ids, pruned by expiry.
    redeemed_websocket_tickets: Arc<std::sync::Mutex<HashMap<String, i64>>>,
    dpop: DpopVerifier,
    /// Non-request origins that may use the session cookie for mutations: the dev UI only.
    trusted_cookie_origins: Arc<[String]>,
}

pub(crate) struct AuthenticatedConnectionGuard {
    auth: AuthService,
    session_id: String,
    connection_id: Option<u64>,
}

impl AuthenticatedConnectionGuard {
    pub(crate) async fn close(mut self) {
        self.disconnect().await;
    }

    async fn disconnect(&mut self) {
        let Some(connection_id) = self.connection_id.take() else {
            return;
        };
        self.auth
            .mark_disconnected(&self.session_id, connection_id)
            .await;
    }
}

impl Drop for AuthenticatedConnectionGuard {
    fn drop(&mut self) {
        let Some(connection_id) = self.connection_id.take() else {
            return;
        };
        let auth = self.auth.clone();
        let session_id = self.session_id.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                %session_id,
                connection_id,
                "unable to schedule authenticated connection cleanup"
            );
            return;
        };
        runtime.spawn(async move {
            auth.mark_disconnected(&session_id, connection_id).await;
        });
    }
}

struct PendingSessionIssuanceGuard {
    auth: AuthService,
    session_id: String,
    armed: bool,
}

impl PendingSessionIssuanceGuard {
    fn new(auth: AuthService, session_id: String) -> Self {
        Self {
            auth,
            session_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingSessionIssuanceGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let auth = self.auth.clone();
        let session_id = self.session_id.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                session_id,
                "unable to schedule compensation for cancelled pending session issuance"
            );
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = auth.revoke_failed_pairing_session(&session_id).await {
                tracing::error!(
                    session_id,
                    ?error,
                    "failed to compensate cancelled pending session issuance"
                );
            }
        });
    }
}

#[derive(Clone)]
struct DesktopBootstrap {
    credential: String,
    expires_at_ms: i64,
}

#[derive(Default)]
struct AuthState {
    sessions: HashMap<String, SessionRecord>,
    pairings: HashMap<String, PairingRecord>,
    pairing_offer_idempotency: HashMap<(String, String), StoredPairingOffer>,
    live_connections: HashMap<String, HashMap<u64, CancellationToken>>,
    next_connection_id: u64,
    authority_generation: u64,
}

impl AuthState {
    fn bump_authority_generation(&mut self) {
        self.authority_generation = self.authority_generation.wrapping_add(1);
    }
}

#[derive(Clone)]
struct StoredPairingOffer {
    input_fingerprint: String,
    pairing_id: Option<String>,
    result: Option<PairingOfferResult>,
    expires_at_ms: i64,
}

pub(crate) struct PairingOfferReservation {
    principal_id: String,
    idempotency_key: String,
    input_fingerprint: String,
}

impl PairingOfferReservation {
    pub(crate) fn new(
        principal_id: String,
        idempotency_key: String,
        input_fingerprint: String,
    ) -> Self {
        Self {
            principal_id,
            idempotency_key,
            input_fingerprint,
        }
    }
}

fn ensure_pairing_offer_capacity(
    state: &AuthState,
    lookup_key: &(String, String),
) -> Result<(), AuthError> {
    if state.pairing_offer_idempotency.contains_key(lookup_key) {
        return Ok(());
    }
    let principal_entries = state
        .pairing_offer_idempotency
        .keys()
        .filter(|(principal_id, _)| principal_id == &lookup_key.0)
        .count();
    if principal_entries >= MAX_ACTIVE_PAIRING_OFFERS_PER_PRINCIPAL {
        return Err(AuthError::Internal(
            "pairing offer principal capacity exceeded".to_owned(),
        ));
    }
    if state.pairing_offer_idempotency.len() >= MAX_ACTIVE_PAIRING_OFFERS {
        return Err(AuthError::Internal(
            "pairing offer idempotency capacity exceeded".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct AuthGrantMetadata {
    /// Whether the session comes from the reusable desktop bootstrap credential.
    desktop_bootstrap: bool,
    proof_key_thumbprint: Option<String>,
    reach: Option<String>,
    off_host: Option<bool>,
}

struct AuthSessionIssuanceMetadata {
    grant: AuthGrantMetadata,
    delivery_state: AuthSessionDeliveryState,
}

pub(crate) enum PairingOfferReplay {
    Original(PairingOfferResult),
    Cancelled,
    Conflict,
    Fresh,
}

pub(crate) enum PairingOfferIssuance {
    Reserved(PairingCredentialResult),
    Replay(PairingOfferReplay),
}

enum PairingIssuance {
    Reserved(PairingCredentialResult),
    Existing(PersistedPairingOffer),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionTransport {
    Plain,
    E2ee,
}

impl SessionTransport {
    const fn claim(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::E2ee => "e2ee",
        }
    }
}

#[derive(Clone)]
struct SessionRecord {
    session_id: String,
    subject: String,
    scopes: Vec<String>,
    method: String,
    client: ClientMetadata,
    issued_at_ms: i64,
    expires_at_ms: i64,
    revoked_at_ms: Option<i64>,
    last_connected_at_ms: Option<i64>,
    connected_count: usize,
    proof_key_thumbprint: Option<String>,
    transport: SessionTransport,
    reach: Option<String>,
    off_host: Option<bool>,
    delivery_state: AuthSessionDeliveryState,
}

#[derive(Clone)]
struct PairingRecord {
    id: String,
    credential: String,
    scopes: Vec<String>,
    subject: String,
    label: Option<String>,
    proof_key_thumbprint: Option<String>,
    created_at_ms: i64,
    expires_at_ms: i64,
    consumed_at_ms: Option<i64>,
    revoked_at_ms: Option<i64>,
    reach: Option<String>,
    off_host: Option<bool>,
}

struct Grant {
    scopes: Vec<String>,
    subject: String,
    label: Option<String>,
    reach: Option<String>,
    off_host: Option<bool>,
    /// The reusable desktop bootstrap credential of this backend process. The
    /// host's own WebView exchanges it on every load and after every backend
    /// restart, so a new exchange supersedes the sessions it minted before.
    desktop_bootstrap: bool,
}

pub struct IssuedSession {
    pub token: String,
    pub principal: Principal,
}

impl AuthService {
    #[must_use]
    #[cfg(test)]
    pub fn new(config: &ServerConfig, signing_secret: Vec<u8>) -> Self {
        Self::build(
            config,
            signing_secret,
            HostIdentity::generate_ephemeral(),
            None,
            None,
        )
    }

    pub(crate) async fn new_with_persistence(
        config: &ServerConfig,
        signing_secret: Vec<u8>,
        secret_store: SecretStore,
        repositories: Repositories,
    ) -> Result<Self, AuthError> {
        let host_identity = HostIdentity::load_or_generate(&secret_store)
            .await
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        // Crash compensation for unconfirmed pairing credentials is age-gated:
        // no connection survives a restart, so a young pending session can
        // never be confirmed anyway, but sweeping only past the grace window
        // keeps one uniform rule with the periodic sweeper below. The startup
        // pass is best-effort — a transiently locked store must not fail
        // boot when the periodic sweeper converges within a minute anyway.
        let now = now_ms();
        if let Err(error) = repositories
            .revoke_pending_auth_sessions(
                format_iso(now),
                format_iso(now - PENDING_PAIRING_SWEEP_GRACE_MS),
            )
            .await
        {
            tracing::warn!(
                ?error,
                "startup pending-pairing sweep failed; the periodic sweeper retries"
            );
        }
        let service = Self::build(
            config,
            signing_secret,
            host_identity,
            Some(secret_store),
            Some(repositories),
        );
        service.hydrate_active_state().await?;
        service.ensure_authority_watcher();
        service.spawn_pending_pairing_sweeper();
        Ok(service)
    }

    /// Periodically revokes pending-pairing sessions older than the grace
    /// window. Startup-only sweeping would let a crash-orphaned unconfirmed
    /// credential keep the host's desired exposure wide until the next
    /// restart; the periodic sweep bounds that window to minutes, while the
    /// age gate keeps it from racing a live confirmation.
    fn spawn_pending_pairing_sweeper(&self) {
        let Some(repositories) = self.repositories.clone() else {
            return;
        };
        let liveness = Arc::downgrade(&self.state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PENDING_PAIRING_SWEEP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick fires immediately and duplicates the startup
            // sweep, which is harmless.
            loop {
                interval.tick().await;
                if liveness.upgrade().is_none() {
                    break;
                }
                let now = now_ms();
                match repositories
                    .revoke_pending_auth_sessions(
                        format_iso(now),
                        format_iso(now - PENDING_PAIRING_SWEEP_GRACE_MS),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(PersistenceError::WorkerUnavailable) => break,
                    Err(error) => {
                        tracing::warn!(?error, "pending-pairing sweep failed");
                    }
                }
            }
        });
    }

    fn build(
        config: &ServerConfig,
        signing_secret: Vec<u8>,
        host_identity: HostIdentity,
        secret_store: Option<SecretStore>,
        repositories: Option<Repositories>,
    ) -> Self {
        let remote_reachable = !is_loopback_host(&config.host);
        let policy = match (config.unsafe_no_auth, config.mode, remote_reachable) {
            (true, _, _) => "unsafe-no-auth",
            (false, ServerMode::Desktop, false) => "desktop-managed-local",
            (false, ServerMode::Web, false) => "loopback-browser",
            (false, _, true) => "remote-reachable",
        };
        let bootstrap_methods = match (config.unsafe_no_auth, config.mode, policy) {
            (true, _, _) => Vec::new(),
            (false, ServerMode::Desktop, "desktop-managed-local") => vec!["desktop-bootstrap"],
            (false, ServerMode::Desktop, _) => vec!["desktop-bootstrap", "one-time-token"],
            (false, _, _) => vec!["one-time-token"],
        };
        let session_cookie_name = if config.mode == ServerMode::Desktop {
            format!("bibcode_session_{}", config.port)
        } else {
            "bibcode_session".to_owned()
        };
        let desktop_bootstrap =
            config
                .desktop_bootstrap_token
                .as_ref()
                .map(|credential| DesktopBootstrap {
                    credential: credential.clone(),
                    expires_at_ms: now_ms().saturating_add(DESKTOP_BOOTSTRAP_TTL_MS),
                });
        let (access_events, _) = broadcast::channel(ACCESS_EVENT_CAPACITY);
        // Only a dev UI served from another origin uses the cookie cross-origin. The desktop
        // app (`bibcode://app`) never sends it: its requests carry a bearer token with
        // `credentials: "omit"`, and its WebSockets authenticate with a ticket.
        let trusted_cookie_origins = config
            .dev_url
            .as_ref()
            .map(|dev_url| dev_url.origin())
            // An opaque origin serializes as `null`, which sandboxed frames send.
            .filter(url::Origin::is_tuple)
            .map(|origin| origin.ascii_serialization())
            .into_iter()
            .collect();
        Self {
            descriptor: AuthDescriptor {
                policy,
                bootstrap_methods,
                session_methods: [
                    "browser-session-cookie",
                    "bearer-access-token",
                    "dpop-access-token",
                ],
                session_cookie_name,
            },
            host_identity,
            desktop_bootstrap,
            signer: TokenSigner::new(signing_secret),
            state: Arc::new(Mutex::new(AuthState::default())),
            issuance: Arc::new(Mutex::new(())),
            pairing_offer_issuance: Arc::new(Mutex::new(())),
            repositories,
            access_events: Arc::new(access_events),
            access_revision: Arc::new(AtomicU64::new(1)),
            authority_watcher_running: Arc::new(AtomicBool::new(false)),
            redeemed_websocket_tickets: Arc::new(std::sync::Mutex::new(HashMap::new())),
            dpop: DpopVerifier::new(secret_store),
            trusted_cookie_origins,
        }
    }

    async fn hydrate_active_state(&self) -> Result<(), AuthError> {
        let Some(repositories) = &self.repositories else {
            return Ok(());
        };
        let snapshot = repositories
            .load_auth_authority_snapshot(format_iso(now_ms()))
            .await
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        let pairings = snapshot
            .pairings
            .into_iter()
            .map(pairing_record_from_persisted)
            .collect::<Result<Vec<_>, _>>()?;
        let sessions = snapshot
            .sessions
            .into_iter()
            .map(session_record_from_persisted)
            .collect::<Result<Vec<_>, _>>()?;
        let pairing_offers = snapshot
            .offers
            .into_iter()
            .map(stored_pairing_offer_from_persisted)
            .collect::<Result<HashMap<_, _>, AuthError>>()?;
        let mut state = self.state.lock().await;
        state.pairings = pairings
            .into_iter()
            .map(|pairing| (pairing.id.clone(), pairing))
            .collect();
        state.sessions = sessions
            .into_iter()
            .map(|session| (session.session_id.clone(), session))
            .collect();
        state.pairing_offer_idempotency = pairing_offers;
        Ok(())
    }

    #[must_use]
    pub fn descriptor(&self) -> AuthDescriptor {
        self.descriptor.clone()
    }

    #[must_use]
    pub fn host_identity(&self) -> &HostIdentity {
        &self.host_identity
    }

    #[must_use]
    pub fn cookie_name(&self) -> &str {
        &self.descriptor.session_cookie_name
    }

    #[must_use]
    pub(crate) fn trusted_cookie_origins(&self) -> &[String] {
        &self.trusted_cookie_origins
    }

    #[must_use]
    pub(crate) fn subscribe_access(&self) -> broadcast::Receiver<AuthAccessEvent> {
        let receiver = self.access_events.subscribe();
        self.ensure_authority_watcher();
        receiver
    }

    pub(crate) async fn access_snapshot(
        &self,
        current_session_id: &str,
    ) -> (u64, Vec<PairingLinkView>, Vec<ClientSessionView>) {
        let now = now_ms();
        let state = self.state.lock().await;
        let mut pairings = state
            .pairings
            .values()
            .filter(|pairing| {
                pairing.consumed_at_ms.is_none()
                    && pairing.revoked_at_ms.is_none()
                    && pairing.expires_at_ms > now
            })
            .map(PairingRecord::view)
            .collect::<Vec<_>>();
        pairings.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        let mut sessions = state
            .sessions
            .values()
            .filter(|session| session.revoked_at_ms.is_none() && session.expires_at_ms > now)
            .map(|session| session.view(session.session_id == current_session_id))
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| !session.current);
        let revision = self.access_revision.load(Ordering::Acquire);
        (revision, pairings, sessions)
    }

    fn emit_access_change(&self, change: AuthAccessChange) {
        emit_access_change_on(&self.access_events, &self.access_revision, change);
    }

    fn ensure_authority_watcher(&self) {
        let Some(repositories) = self.repositories.clone() else {
            return;
        };
        if self
            .authority_watcher_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let state = Arc::downgrade(&self.state);
        let access_events = Arc::downgrade(&self.access_events);
        let access_revision = Arc::downgrade(&self.access_revision);
        let watcher_running = Arc::downgrade(&self.authority_watcher_running);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(AUTHORITY_CONVERGENCE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut observed_revision = None;
            let mut next_expiry_ms = None;
            'watcher: loop {
                interval.tick().await;
                let (
                    Some(state),
                    Some(access_events),
                    Some(access_revision),
                    Some(watcher_running),
                ) = (
                    state.upgrade(),
                    access_events.upgrade(),
                    access_revision.upgrade(),
                    watcher_running.upgrade(),
                )
                else {
                    break;
                };
                let now = now_ms();
                match repositories.auth_authority_revision().await {
                    Ok(revision)
                        if observed_revision != Some(revision)
                            || next_expiry_ms.is_some_and(|expires_at| expires_at <= now) =>
                    {
                        let generation = state.lock().await.authority_generation;
                        match repositories
                            .load_auth_authority_snapshot(format_iso(now))
                            .await
                        {
                            Ok(snapshot) => {
                                let snapshot_revision = snapshot.revision;
                                let snapshot_expiry = snapshot
                                    .next_expiry_at
                                    .as_deref()
                                    .map(parse_timestamp_ms)
                                    .transpose();
                                match (
                                    snapshot_expiry,
                                    reconcile_authority_snapshot(&state, snapshot, generation)
                                        .await,
                                ) {
                                    (Ok(snapshot_expiry), Ok(Some(changes))) => {
                                        observed_revision = Some(snapshot_revision);
                                        next_expiry_ms = snapshot_expiry;
                                        for change in changes {
                                            emit_access_change_on(
                                                &access_events,
                                                &access_revision,
                                                change,
                                            );
                                        }
                                    }
                                    (Err(error), _) | (_, Err(error)) => {
                                        tracing::error!(
                                            ?error,
                                            "failed to decode durable auth authority snapshot"
                                        );
                                    }
                                    (Ok(_), Ok(None)) => {}
                                }
                            }
                            Err(PersistenceError::WorkerUnavailable) => {
                                watcher_running.store(false, Ordering::Release);
                                break 'watcher;
                            }
                            Err(error) => {
                                tracing::error!(%error, "failed to reconcile durable auth authority");
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(PersistenceError::WorkerUnavailable) => {
                        watcher_running.store(false, Ordering::Release);
                        break;
                    }
                    Err(error) => {
                        tracing::error!(%error, "failed to read durable auth authority revision");
                    }
                }

                if authority_has_consumers(&state, &access_events).await {
                    continue;
                }
                watcher_running.store(false, Ordering::Release);
                if authority_has_consumers(&state, &access_events).await
                    && watcher_running
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                break;
            }
        });
    }

    pub(crate) async fn create_browser_session(
        &self,
        credential: &str,
        client: ClientMetadata,
        transport: SessionTransport,
    ) -> Result<IssuedSession, AuthError> {
        let grant = self.consume_grant(credential, None).await?;
        self.issue_session(
            grant.subject,
            grant.scopes,
            "browser-session-cookie",
            apply_grant_label(client, grant.label),
            transport,
            AuthSessionIssuanceMetadata {
                grant: AuthGrantMetadata {
                    desktop_bootstrap: grant.desktop_bootstrap,
                    reach: grant.reach,
                    off_host: grant.off_host,
                    ..AuthGrantMetadata::default()
                },
                delivery_state: AuthSessionDeliveryState::Active,
            },
        )
        .await
    }

    pub(crate) async fn exchange_bootstrap(
        &self,
        credential: &str,
        requested_scopes: Option<Vec<String>>,
        client: ClientMetadata,
        proof_key_thumbprint: Option<String>,
        transport: SessionTransport,
    ) -> Result<IssuedSession, AuthError> {
        self.exchange_bootstrap_with_delivery(
            credential,
            requested_scopes,
            client,
            proof_key_thumbprint,
            transport,
            AuthSessionDeliveryState::Active,
        )
        .await
    }

    /// Exchanges a pairing one-time token over the encrypted channel. The
    /// server — not the client — decides delivery from the grant it consumed:
    /// an off-host grant mints a pending session that must be confirmed before
    /// the credential authenticates anywhere, an on-host grant is delivered
    /// immediately. A client cannot opt out of the guard by omitting a wire
    /// flag. Returns the issued session and whether confirmation is required.
    pub(crate) async fn exchange_pairing_bootstrap(
        &self,
        credential: &str,
        client: ClientMetadata,
    ) -> Result<(IssuedSession, bool), AuthError> {
        let grant = self.consume_grant(credential, None).await?;
        let confirmation_required = matches!(grant.off_host, Some(true));
        let delivery_state = if confirmation_required {
            AuthSessionDeliveryState::PendingPairing
        } else {
            AuthSessionDeliveryState::Active
        };
        let issued = self
            .issue_session_from_grant(
                grant,
                None,
                client,
                None,
                SessionTransport::E2ee,
                delivery_state,
            )
            .await?;
        Ok((issued, confirmation_required))
    }

    async fn exchange_bootstrap_with_delivery(
        &self,
        credential: &str,
        requested_scopes: Option<Vec<String>>,
        client: ClientMetadata,
        proof_key_thumbprint: Option<String>,
        transport: SessionTransport,
        delivery_state: AuthSessionDeliveryState,
    ) -> Result<IssuedSession, AuthError> {
        let grant = self
            .consume_grant(credential, proof_key_thumbprint.as_deref())
            .await?;
        self.issue_session_from_grant(
            grant,
            requested_scopes,
            client,
            proof_key_thumbprint,
            transport,
            delivery_state,
        )
        .await
    }

    async fn issue_session_from_grant(
        &self,
        grant: Grant,
        requested_scopes: Option<Vec<String>>,
        client: ClientMetadata,
        proof_key_thumbprint: Option<String>,
        transport: SessionTransport,
        delivery_state: AuthSessionDeliveryState,
    ) -> Result<IssuedSession, AuthError> {
        let scopes = requested_scopes.unwrap_or_else(|| grant.scopes.clone());
        if !scopes
            .iter()
            .all(|scope| grant.scopes.iter().any(|granted| granted == scope))
        {
            return Err(AuthError::ScopeNotGranted);
        }
        let method = if proof_key_thumbprint.is_some() {
            "dpop-access-token"
        } else {
            "bearer-access-token"
        };
        self.issue_session(
            grant.subject,
            scopes,
            method,
            apply_grant_label(client, grant.label),
            transport,
            AuthSessionIssuanceMetadata {
                grant: AuthGrantMetadata {
                    desktop_bootstrap: grant.desktop_bootstrap,
                    proof_key_thumbprint,
                    reach: grant.reach,
                    off_host: grant.off_host,
                },
                delivery_state,
            },
        )
        .await
    }

    pub(crate) async fn authenticate_token(
        &self,
        token: &str,
        surface: SessionTransport,
    ) -> Result<Principal, AuthError> {
        let claims: SessionClaims = self
            .signer
            .verify(token)
            .map_err(map_token_error_to_credential)?;
        let observed_at = now_ms();
        if claims.v != 1
            || claims.kind != "session"
            || claims.exp <= observed_at
            || claims.scopes.iter().any(|scope| !is_scope(scope))
            || !matches!(claims.tr.as_str(), "plain" | "e2ee")
            || (claims.tr == "e2ee" && surface == SessionTransport::Plain)
        {
            return Err(AuthError::InvalidCredential);
        }
        self.refresh_session_from_repository(&claims.sid, observed_at)
            .await?;
        let state = self.state.lock().await;
        let record = state
            .sessions
            .get(&claims.sid)
            .ok_or(AuthError::InvalidCredential)?;
        if record.revoked_at_ms.is_some()
            || record.expires_at_ms <= observed_at
            || record.expires_at_ms != claims.exp
            || record.subject != claims.sub
            || record.method != claims.method
            || record.scopes != claims.scopes
            || (record.transport == SessionTransport::E2ee && claims.tr != "e2ee")
            || record.delivery_state != AuthSessionDeliveryState::Active
        {
            return Err(AuthError::InvalidCredential);
        }
        Ok(Principal {
            session_id: claims.sid,
            subject: claims.sub,
            method: claims.method,
            scopes: claims.scopes,
            proof_key_thumbprint: claims.jkt,
            expires_at_ms: claims.exp,
        })
    }

    pub async fn verify_dpop(
        &self,
        proof: &str,
        method: &str,
        url: &str,
        expected_thumbprint: Option<&str>,
        expected_access_token: Option<&str>,
    ) -> Result<String, AuthError> {
        self.dpop
            .verify(
                proof,
                method,
                url,
                now_ms() / 1_000,
                expected_thumbprint,
                expected_access_token,
            )
            .await
    }

    pub fn issue_websocket_ticket(
        &self,
        principal: &Principal,
    ) -> Result<(String, i64), AuthError> {
        let issued_at = now_ms();
        let expires_at = issued_at.saturating_add(WEBSOCKET_TICKET_TTL_MS);
        let claims = WebSocketClaims {
            v: 1,
            kind: "websocket".to_owned(),
            sid: principal.session_id.clone(),
            jti: Some(Uuid::new_v4().to_string()),
            iat: issued_at,
            exp: expires_at,
        };
        self.signer
            .issue(&claims)
            .map(|token| (token, expires_at))
            .map_err(|error| AuthError::Internal(error.to_string()))
    }

    // Tickets are only accepted by the plain `/ws` route. An E2EE session cannot
    // obtain one because the preceding plain HTTP authentication rejects its
    // transport-scoped bearer credential.
    pub async fn verify_websocket_ticket(&self, token: &str) -> Result<Principal, AuthError> {
        let claims: WebSocketClaims = self
            .signer
            .verify(token)
            .map_err(map_token_error_to_credential)?;
        let observed_at = now_ms();
        if claims.v != 1 || claims.kind != "websocket" || claims.exp <= observed_at {
            return Err(AuthError::InvalidCredential);
        }
        self.refresh_session_from_repository(&claims.sid, observed_at)
            .await?;
        let state = self.state.lock().await;
        let record = state
            .sessions
            .get(&claims.sid)
            .ok_or(AuthError::InvalidCredential)?;
        if record.revoked_at_ms.is_some() || record.expires_at_ms <= observed_at {
            return Err(AuthError::InvalidCredential);
        }
        // Defense in depth for the no-downgrade invariant: tickets are only
        // accepted by the plain `/ws` route, so only plain-transport sessions
        // may redeem them.
        if record.transport != SessionTransport::Plain {
            return Err(AuthError::InvalidCredential);
        }
        let principal = record.principal();
        drop(state);
        // Each ticket is redeemable exactly once; the client mints a fresh
        // ticket per connection attempt, so replays are always hostile.
        let Some(jti) = claims.jti else {
            return Err(AuthError::InvalidCredential);
        };
        let mut redeemed = self
            .redeemed_websocket_tickets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        redeemed.retain(|_, expires_at| *expires_at > observed_at);
        if redeemed.insert(jti, claims.exp).is_some() {
            return Err(AuthError::InvalidCredential);
        }
        drop(redeemed);
        Ok(principal)
    }

    pub(crate) async fn authorize_session(
        &self,
        session_id: &str,
        required_scope: &str,
    ) -> Result<(), AuthError> {
        let observed_at = now_ms();
        self.refresh_session_from_repository(session_id, observed_at)
            .await?;
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(session_id)
            .ok_or(AuthError::InvalidCredential)?;
        if session.revoked_at_ms.is_some() || session.expires_at_ms <= observed_at {
            return Err(AuthError::InvalidCredential);
        }
        if session.scopes.iter().any(|scope| scope == required_scope) {
            Ok(())
        } else {
            Err(AuthError::ScopeRequired(required_scope.to_owned()))
        }
    }

    /// Expiry of `session_id` while it exists, is not revoked, and has not expired.
    /// Repository failures count as inactive so callers fail closed.
    pub(crate) async fn active_session_expiry(&self, session_id: &str) -> Option<i64> {
        let observed_at = now_ms();
        self.refresh_session_from_repository(session_id, observed_at)
            .await
            .ok()?;
        let state = self.state.lock().await;
        let session = state.sessions.get(session_id)?;
        (session.revoked_at_ms.is_none() && session.expires_at_ms > observed_at)
            .then_some(session.expires_at_ms)
    }

    async fn refresh_session_from_repository(
        &self,
        session_id: &str,
        observed_at: i64,
    ) -> Result<(), AuthError> {
        let Some(repositories) = &self.repositories else {
            return Ok(());
        };
        let persisted = repositories
            .get_auth_session(session_id.to_owned())
            .await
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        let Some(persisted) = persisted else {
            self.remove_cached_session(session_id).await;
            return Err(AuthError::InvalidCredential);
        };
        if persisted.revoked_at.is_some()
            || parse_timestamp_ms(&persisted.expires_at)? <= observed_at
        {
            self.remove_cached_session(session_id).await;
            return Err(AuthError::InvalidCredential);
        }
        let mut refreshed = session_record_from_persisted(persisted)?;
        let mut state = self.state.lock().await;
        let change = if let Some(current) = state.sessions.get(session_id) {
            let previous_view = current.view(false);
            refreshed.connected_count = current.connected_count;
            refreshed.proof_key_thumbprint = current.proof_key_thumbprint.clone();
            refreshed.transport = current.transport;
            if current.last_connected_at_ms > refreshed.last_connected_at_ms {
                refreshed.last_connected_at_ms = current.last_connected_at_ms;
            }
            (refreshed.view(false) != previous_view)
                .then(|| AuthAccessChange::ClientUpserted(refreshed.view(false)))
        } else {
            Some(AuthAccessChange::ClientUpserted(refreshed.view(false)))
        };
        state.sessions.insert(session_id.to_owned(), refreshed);
        if change.is_some() {
            state.bump_authority_generation();
        }
        drop(state);
        if let Some(change) = change {
            self.emit_access_change(change);
        }
        self.ensure_authority_watcher();
        Ok(())
    }

    async fn remove_cached_session(&self, session_id: &str) {
        let mut state = self.state.lock().await;
        let removed = state.sessions.remove(session_id).is_some();
        cancel_live_connections(&mut state, session_id);
        if removed {
            state.bump_authority_generation();
        }
        drop(state);
        if removed {
            self.emit_access_change(AuthAccessChange::ClientRemoved {
                session_id: session_id.to_owned(),
            });
        }
    }

    pub async fn issue_pairing(
        &self,
        scopes: Vec<String>,
        label: Option<String>,
    ) -> Result<PairingCredentialResult, AuthError> {
        reserved_pairing(
            self.issue_pairing_for_subject(
                scopes,
                label,
                "one-time-token",
                PAIRING_TTL_MS,
                AuthGrantMetadata::default(),
                None,
            )
            .await?,
        )
    }

    pub async fn issue_pairing_with_proof(
        &self,
        scopes: Vec<String>,
        label: Option<String>,
        proof_key_thumbprint: String,
    ) -> Result<PairingCredentialResult, AuthError> {
        if proof_key_thumbprint.trim().is_empty() {
            return Err(AuthError::InvalidCredential);
        }
        reserved_pairing(
            self.issue_pairing_for_subject(
                scopes,
                label,
                "one-time-token",
                PAIRING_TTL_MS,
                AuthGrantMetadata {
                    desktop_bootstrap: false,
                    proof_key_thumbprint: Some(proof_key_thumbprint),
                    ..AuthGrantMetadata::default()
                },
                None,
            )
            .await?,
        )
    }

    pub async fn issue_cloud_pairing(
        &self,
        proof_key_thumbprint: String,
    ) -> Result<PairingCredentialResult, AuthError> {
        if proof_key_thumbprint.trim().is_empty() {
            return Err(AuthError::InvalidCredential);
        }
        reserved_pairing(
            self.issue_pairing_for_subject(
                owned_scopes(STANDARD_SCOPES),
                Some("BiBCode Connect connect".to_owned()),
                "cloud-connect",
                CLOUD_PAIRING_TTL_MS,
                AuthGrantMetadata {
                    desktop_bootstrap: false,
                    proof_key_thumbprint: Some(proof_key_thumbprint),
                    ..AuthGrantMetadata::default()
                },
                None,
            )
            .await?,
        )
    }

    pub(crate) async fn issue_startup_pairing(&self) -> Result<PairingCredentialResult, AuthError> {
        reserved_pairing(
            self.issue_pairing_for_subject(
                owned_scopes(ADMINISTRATIVE_SCOPES),
                None,
                "administrative-bootstrap",
                PAIRING_TTL_MS,
                AuthGrantMetadata::default(),
                None,
            )
            .await?,
        )
    }

    pub async fn issue_share_pairing(
        &self,
        scopes: Vec<String>,
        label: Option<String>,
        reach: String,
        off_host: bool,
    ) -> Result<PairingCredentialResult, AuthError> {
        if !is_valid_pairing_reach(&reach) {
            return Err(AuthError::InvalidCredential);
        }
        reserved_pairing(
            self.issue_pairing_for_subject(
                scopes,
                label,
                "one-time-token",
                PAIRING_TTL_MS,
                AuthGrantMetadata {
                    desktop_bootstrap: false,
                    reach: Some(reach),
                    off_host: Some(off_host),
                    ..AuthGrantMetadata::default()
                },
                None,
            )
            .await?,
        )
    }

    pub(crate) async fn issue_share_pairing_offer(
        &self,
        scopes: Vec<String>,
        label: Option<String>,
        reach: String,
        off_host: bool,
        reservation: PairingOfferReservation,
    ) -> Result<PairingOfferIssuance, AuthError> {
        if !is_valid_pairing_reach(&reach) {
            return Err(AuthError::InvalidCredential);
        }
        let replay_fingerprint = reservation.input_fingerprint.clone();
        match self
            .issue_pairing_for_subject(
                scopes,
                label,
                "one-time-token",
                PAIRING_TTL_MS,
                AuthGrantMetadata {
                    desktop_bootstrap: false,
                    reach: Some(reach),
                    off_host: Some(off_host),
                    ..AuthGrantMetadata::default()
                },
                Some(reservation),
            )
            .await?
        {
            PairingIssuance::Reserved(issued) => Ok(PairingOfferIssuance::Reserved(issued)),
            PairingIssuance::Existing(offer) => Ok(PairingOfferIssuance::Replay(
                pairing_offer_replay_from_persisted(&offer, &replay_fingerprint)?,
            )),
        }
    }

    async fn issue_pairing_for_subject(
        &self,
        scopes: Vec<String>,
        label: Option<String>,
        subject: &str,
        ttl_ms: i64,
        metadata: AuthGrantMetadata,
        offer_reservation: Option<PairingOfferReservation>,
    ) -> Result<PairingIssuance, AuthError> {
        let AuthGrantMetadata {
            desktop_bootstrap: _,
            proof_key_thumbprint,
            reach,
            off_host,
        } = metadata;
        let _issuance = self.issuance.lock().await;
        if scopes.is_empty()
            || scopes.iter().any(|scope| !is_scope(scope))
            || scopes.iter().collect::<HashSet<_>>().len() != scopes.len()
        {
            return Err(AuthError::InvalidScope);
        }
        let now = now_ms();
        let expires_at = now.saturating_add(ttl_ms);
        let durable_offer_reservation = self.repositories.is_some() && offer_reservation.is_some();
        let credential = {
            let mut state = self.state.lock().await;
            state.pairings.retain(|_, pairing| {
                pairing.consumed_at_ms.is_none()
                    && pairing.revoked_at_ms.is_none()
                    && pairing.expires_at_ms > now
            });
            state
                .pairing_offer_idempotency
                .retain(|_, stored| stored.expires_at_ms > now);
            if !durable_offer_reservation && state.pairings.len() >= MAX_ACTIVE_PAIRINGS {
                return Err(AuthError::Internal(
                    "active pairing capacity exceeded".to_owned(),
                ));
            }
            if !durable_offer_reservation && let Some(reservation) = &offer_reservation {
                let lookup_key = (
                    reservation.principal_id.clone(),
                    reservation.idempotency_key.clone(),
                );
                ensure_pairing_offer_capacity(&state, &lookup_key)?;
                if state.pairing_offer_idempotency.contains_key(&lookup_key) {
                    return Err(AuthError::Internal(
                        "pairing offer idempotency key is already reserved".to_owned(),
                    ));
                }
            }
            loop {
                let candidate = generate_pairing_credential()?;
                if !state
                    .pairings
                    .values()
                    .any(|pairing| pairing.credential == candidate)
                {
                    break candidate;
                }
            }
        };
        let id = Uuid::new_v4().to_string();
        let record = PairingRecord {
            id: id.clone(),
            credential: credential.clone(),
            scopes,
            subject: subject.to_owned(),
            label: label.clone(),
            proof_key_thumbprint,
            created_at_ms: now,
            expires_at_ms: expires_at,
            consumed_at_ms: None,
            revoked_at_ms: None,
            reach,
            off_host,
        };
        let view = record.view();
        if let Some(repositories) = &self.repositories {
            if let Some(reservation) = &offer_reservation {
                let reservation = repositories
                    .create_auth_pairing_link_with_offer(
                        persisted_pairing_link(&record),
                        NewAuthPairingOffer {
                            principal_id: reservation.principal_id.clone(),
                            idempotency_key: reservation.idempotency_key.clone(),
                            input_fingerprint: reservation.input_fingerprint.clone(),
                            expires_at: format_iso(expires_at),
                        },
                    )
                    .await
                    .map_err(|error| AuthError::Internal(error.to_string()))?;
                if !reservation.reserved {
                    let offer = reservation.offer;
                    let (lookup_key, stored) = stored_pairing_offer_from_persisted(offer.clone())?;
                    let mut state = self.state.lock().await;
                    state.pairing_offer_idempotency.insert(lookup_key, stored);
                    let access_change = if let Some(pairing) = reservation.pairing {
                        let pairing = pairing_record_from_persisted(pairing)?;
                        let change = (!state.pairings.contains_key(&pairing.id))
                            .then(|| AuthAccessChange::PairingLinkUpserted(pairing.view()));
                        state.pairings.insert(pairing.id.clone(), pairing);
                        change
                    } else if let Some(pairing_id) = &offer.pairing_id {
                        state.pairings.remove(pairing_id).map(|_| {
                            AuthAccessChange::PairingLinkRemoved {
                                id: pairing_id.clone(),
                            }
                        })
                    } else {
                        None
                    };
                    state.bump_authority_generation();
                    drop(state);
                    if let Some(change) = access_change {
                        self.emit_access_change(change);
                    }
                    self.ensure_authority_watcher();
                    return Ok(PairingIssuance::Existing(offer));
                }
            } else {
                repositories
                    .create_auth_pairing_link(persisted_pairing_link(&record))
                    .await
                    .map_err(|error| AuthError::Internal(error.to_string()))?;
            }
        }
        let mut state = self.state.lock().await;
        state.pairings.insert(id.clone(), record);
        if let Some(reservation) = offer_reservation {
            state.pairing_offer_idempotency.insert(
                (reservation.principal_id, reservation.idempotency_key),
                StoredPairingOffer {
                    input_fingerprint: reservation.input_fingerprint,
                    pairing_id: Some(id.clone()),
                    result: None,
                    expires_at_ms: expires_at,
                },
            );
        }
        state.bump_authority_generation();
        drop(state);
        self.emit_access_change(AuthAccessChange::PairingLinkUpserted(view));
        self.ensure_authority_watcher();
        Ok(PairingIssuance::Reserved(PairingCredentialResult {
            id,
            credential,
            label,
            expires_at: format_iso(expires_at),
        }))
    }

    pub async fn list_pairings(&self) -> Vec<PairingLinkView> {
        let now = now_ms();
        let mut state = self.state.lock().await;
        state.pairings.retain(|_, pairing| {
            pairing.consumed_at_ms.is_none()
                && pairing.revoked_at_ms.is_none()
                && pairing.expires_at_ms > now
        });
        let mut pairings = state
            .pairings
            .values()
            .filter(|pairing| {
                pairing.consumed_at_ms.is_none()
                    && pairing.revoked_at_ms.is_none()
                    && pairing.expires_at_ms > now
            })
            .map(PairingRecord::view)
            .collect::<Vec<_>>();
        pairings.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        pairings
    }

    pub(crate) async fn lock_pairing_offer_issuance(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.pairing_offer_issuance).lock_owned().await
    }

    pub(crate) async fn replay_pairing_offer(
        &self,
        principal_id: &str,
        key: &str,
        input_fingerprint: &str,
    ) -> Result<PairingOfferReplay, AuthError> {
        let observed_at = now_ms();
        let lookup_key = (principal_id.to_owned(), key.to_owned());
        let mut access_change = None;
        if let Some(repositories) = &self.repositories {
            let recovered_pairing_id = repositories
                .recover_pending_auth_pairing_offer(
                    principal_id.to_owned(),
                    key.to_owned(),
                    input_fingerprint.to_owned(),
                    format_iso(observed_at),
                    format_iso(observed_at.saturating_sub(PENDING_PAIRING_SWEEP_GRACE_MS)),
                )
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            if let Some(pairing_id) = recovered_pairing_id {
                let mut state = self.state.lock().await;
                state.pairing_offer_idempotency.remove(&lookup_key);
                let removed = state.pairings.remove(&pairing_id).is_some();
                state.bump_authority_generation();
                drop(state);
                if removed {
                    self.emit_access_change(AuthAccessChange::PairingLinkRemoved {
                        id: pairing_id,
                    });
                }
                self.ensure_authority_watcher();
                return Ok(PairingOfferReplay::Fresh);
            }
            let persisted = repositories
                .prune_and_get_active_auth_pairing_offer(
                    principal_id.to_owned(),
                    key.to_owned(),
                    format_iso(observed_at),
                )
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            let mut state = self.state.lock().await;
            match persisted {
                Some(authority) => {
                    let offer = authority.offer;
                    let (persisted_key, stored) =
                        stored_pairing_offer_from_persisted(offer.clone())?;
                    state
                        .pairing_offer_idempotency
                        .insert(persisted_key, stored);
                    if let Some(pairing) = authority.pairing {
                        let pairing = pairing_record_from_persisted(pairing)?;
                        if !state.pairings.contains_key(&pairing.id) {
                            access_change =
                                Some(AuthAccessChange::PairingLinkUpserted(pairing.view()));
                        }
                        state.pairings.insert(pairing.id.clone(), pairing);
                    } else if let Some(pairing_id) = offer.pairing_id
                        && state.pairings.remove(&pairing_id).is_some()
                    {
                        access_change =
                            Some(AuthAccessChange::PairingLinkRemoved { id: pairing_id });
                    }
                }
                None => {
                    state.pairing_offer_idempotency.remove(&lookup_key);
                }
            }
            state.bump_authority_generation();
        }
        if let Some(change) = access_change {
            self.emit_access_change(change);
        }
        let mut state = self.state.lock().await;
        state
            .pairing_offer_idempotency
            .retain(|_, stored| stored.expires_at_ms > observed_at);
        let stale_completed = state
            .pairing_offer_idempotency
            .get(&lookup_key)
            .is_some_and(|stored| {
                stored.input_fingerprint == input_fingerprint
                    && stored.result.is_some()
                    && stored
                        .pairing_id
                        .as_ref()
                        .is_some_and(|id| !state.pairings.contains_key(id))
            });
        if stale_completed {
            // The recorded offer was consumed or revoked since it was minted;
            // replaying its result would advertise a dead code as fresh.
            state.pairing_offer_idempotency.remove(&lookup_key);
            state.bump_authority_generation();
            return Ok(PairingOfferReplay::Fresh);
        }
        Ok(match state.pairing_offer_idempotency.get(&lookup_key) {
            Some(stored) if stored.result.is_none() && stored.pairing_id.is_none() => {
                PairingOfferReplay::Cancelled
            }
            Some(stored) if stored.input_fingerprint == input_fingerprint => stored
                .result
                .clone()
                .map(PairingOfferReplay::Original)
                .unwrap_or(PairingOfferReplay::Cancelled),
            Some(_) => PairingOfferReplay::Conflict,
            None => PairingOfferReplay::Fresh,
        })
    }

    pub(crate) async fn record_pairing_offer(
        &self,
        principal_id: &str,
        key: String,
        input_fingerprint: String,
        result: PairingOfferResult,
    ) -> Result<(), AuthError> {
        let expires_at_ms = parse_timestamp_ms(&result.expires_at)?;
        let observed_at = now_ms();
        if let Some(repositories) = &self.repositories {
            let persisted = serde_json::to_value(&result)
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            if !repositories
                .complete_auth_pairing_offer(principal_id.to_owned(), key.clone(), persisted)
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?
            {
                return Err(AuthError::Internal(
                    "pairing offer reservation is unavailable".to_owned(),
                ));
            }
        }
        let mut state = self.state.lock().await;
        state
            .pairing_offer_idempotency
            .retain(|_, stored| stored.expires_at_ms > observed_at);
        let lookup_key = (principal_id.to_owned(), key.clone());
        ensure_pairing_offer_capacity(&state, &lookup_key)?;
        if let Some(stored) = state.pairing_offer_idempotency.get_mut(&lookup_key) {
            stored.input_fingerprint = input_fingerprint;
            stored.pairing_id = Some(result.id.clone());
            stored.result = Some(result);
            stored.expires_at_ms = expires_at_ms;
        } else {
            state.pairing_offer_idempotency.insert(
                lookup_key,
                StoredPairingOffer {
                    input_fingerprint,
                    pairing_id: Some(result.id.clone()),
                    result: Some(result),
                    expires_at_ms,
                },
            );
        }
        state.bump_authority_generation();
        drop(state);
        self.ensure_authority_watcher();
        Ok(())
    }

    pub(crate) async fn cancel_pairing_offer(
        &self,
        principal_id: &str,
        key: String,
    ) -> Result<bool, AuthError> {
        let observed_at = now_ms();
        let lookup_key = (principal_id.to_owned(), key.clone());
        let offer_id = {
            let mut state = self.state.lock().await;
            state
                .pairing_offer_idempotency
                .retain(|_, stored| stored.expires_at_ms > observed_at);
            if self.repositories.is_none() {
                ensure_pairing_offer_capacity(&state, &lookup_key)?;
            }
            state
                .pairing_offer_idempotency
                .get(&lookup_key)
                .and_then(|stored| stored.pairing_id.clone())
        };

        let (cancelled, cancelled_pairing_id) = if let Some(repositories) = &self.repositories {
            let cancellation = repositories
                .cancel_auth_pairing_offer(
                    principal_id.to_owned(),
                    key.clone(),
                    format_iso(observed_at),
                    format_iso(observed_at.saturating_add(PAIRING_TTL_MS)),
                )
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            (cancellation.revoked, cancellation.pairing_id)
        } else {
            let cancelled = match &offer_id {
                Some(id) => self.revoke_pairing(id).await?,
                None => false,
            };
            (cancelled, offer_id)
        };
        let removed = {
            let mut state = self.state.lock().await;
            let removed = cancelled_pairing_id
                .as_ref()
                .and_then(|id| state.pairings.remove(id));
            state.pairing_offer_idempotency.insert(
                lookup_key,
                StoredPairingOffer {
                    input_fingerprint: String::new(),
                    pairing_id: None,
                    result: None,
                    expires_at_ms: observed_at.saturating_add(PAIRING_TTL_MS),
                },
            );
            state.bump_authority_generation();
            removed
        };
        if self.repositories.is_some()
            && cancelled
            && let Some(pairing) = removed
        {
            self.emit_access_change(AuthAccessChange::PairingLinkRemoved { id: pairing.id });
        }
        Ok(cancelled)
    }

    pub async fn share_exposure_state(&self) -> ShareExposureState {
        let now = now_ms();
        let state = self.state.lock().await;
        let link_grants = state
            .pairings
            .values()
            .filter(|pairing| {
                pairing.subject == "one-time-token"
                    && pairing.consumed_at_ms.is_none()
                    && pairing.revoked_at_ms.is_none()
                    && pairing.expires_at_ms > now
            })
            .map(|pairing| (pairing.reach.as_deref(), pairing.off_host));
        let session_grants = state
            .sessions
            .values()
            .filter(|session| {
                session.subject == "one-time-token"
                    && session.revoked_at_ms.is_none()
                    && session.expires_at_ms > now
            })
            .map(|session| (session.reach.as_deref(), session.off_host));
        let mut off_host_grant_count = 0usize;
        let mut native_exposure_grant_count = 0usize;
        let mut legacy_grant_count = 0usize;
        for (reach, off_host) in link_grants.chain(session_grants) {
            match off_host {
                Some(true) => {
                    off_host_grant_count += 1;
                    if reach == Some(PAIRING_REACH_ANOTHER_DEVICE) {
                        native_exposure_grant_count += 1;
                    }
                }
                Some(false) => {}
                None => legacy_grant_count += 1,
            }
        }
        ShareExposureState {
            desired_exposure: if native_exposure_grant_count > 0 {
                "wide"
            } else {
                "loopback"
            }
            .to_owned(),
            off_host_grant_count,
            legacy_grant_count,
        }
    }

    pub async fn revoke_pairing(&self, id: &str) -> Result<bool, AuthError> {
        let revoked_at = now_ms();
        if let Some(repositories) = &self.repositories {
            let revoked = repositories
                .revoke_auth_pairing_link(id.to_owned(), format_iso(revoked_at))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            if !revoked {
                return Ok(false);
            }
            let mut state = self.state.lock().await;
            state.pairings.remove(id);
            state.bump_authority_generation();
            drop(state);
            self.emit_access_change(AuthAccessChange::PairingLinkRemoved { id: id.to_owned() });
            return Ok(true);
        }
        let mut state = self.state.lock().await;
        let Some(pairing) = state.pairings.get_mut(id) else {
            return Ok(false);
        };
        if pairing.revoked_at_ms.is_some() {
            return Ok(false);
        }
        pairing.revoked_at_ms = Some(revoked_at);
        let id = pairing.id.clone();
        state.pairings.remove(&id);
        state.bump_authority_generation();
        drop(state);
        self.emit_access_change(AuthAccessChange::PairingLinkRemoved { id });
        Ok(true)
    }

    pub async fn list_clients(&self, current_session_id: &str) -> Vec<ClientSessionView> {
        let now = now_ms();
        let mut state = self.state.lock().await;
        state
            .sessions
            .retain(|_, session| session.revoked_at_ms.is_none() && session.expires_at_ms > now);
        let mut sessions = state
            .sessions
            .values()
            .filter(|session| session.revoked_at_ms.is_none() && session.expires_at_ms > now)
            .map(|session| session.view(session.session_id == current_session_id))
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| !session.current);
        sessions
    }

    pub async fn revoke_client(
        &self,
        current_session_id: &str,
        target_session_id: &str,
    ) -> Result<bool, AuthError> {
        if current_session_id == target_session_id {
            return Err(AuthError::CurrentSessionRevokeNotAllowed);
        }
        self.revoke_session(target_session_id).await
    }

    pub(crate) async fn revoke_failed_pairing_session(
        &self,
        session_id: &str,
    ) -> Result<bool, AuthError> {
        let revoked_at = now_ms();
        if let Some(repositories) = &self.repositories {
            let revoked = repositories
                .revoke_pending_auth_session(session_id.to_owned(), format_iso(revoked_at))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            if !revoked {
                return Ok(false);
            }
        }
        let mut state = self.state.lock().await;
        let pending = state.sessions.get(session_id).is_some_and(|session| {
            session.delivery_state == AuthSessionDeliveryState::PendingPairing
        });
        if !pending {
            return Ok(false);
        }
        state.sessions.remove(session_id);
        cancel_live_connections(&mut state, session_id);
        state.bump_authority_generation();
        drop(state);
        self.emit_access_change(AuthAccessChange::ClientRemoved {
            session_id: session_id.to_owned(),
        });
        Ok(true)
    }

    pub(crate) async fn confirm_pending_pairing_session(
        &self,
        session_id: &str,
    ) -> Result<bool, AuthError> {
        let now = now_ms();
        if let Some(repositories) = &self.repositories {
            let confirmed = repositories
                .confirm_pending_auth_session(session_id.to_owned(), format_iso(now))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            if !confirmed {
                return Ok(false);
            }
        }
        let mut state = self.state.lock().await;
        let Some(session) = state.sessions.get_mut(session_id) else {
            return Ok(false);
        };
        if session.revoked_at_ms.is_some() || session.expires_at_ms <= now {
            return Ok(false);
        }
        if session.delivery_state == AuthSessionDeliveryState::Active {
            return Ok(true);
        }
        session.delivery_state = AuthSessionDeliveryState::Active;
        state.bump_authority_generation();
        Ok(true)
    }

    /// Revokes every active session the desktop bootstrap credential minted
    /// before a new exchange. The host has one WebView, and each of its loads
    /// or backend restarts exchanges the same reusable credential; without this
    /// every launch would leave another "current host" row in the paired-client
    /// list. Runs under the issuance lock after the replacement session is
    /// durable, so concurrent exchanges serialize and a crash in between leaves
    /// sessions the next exchange supersedes. With persistence the database is
    /// the authority (the authority watcher converges memory to it), so the
    /// candidates come from the persisted active rows and revocation goes
    /// through the persisted path; the watcher cannot resurrect a superseded
    /// session. The WebView holds one session per session method (its bearer
    /// access token and its browser-session cookie), so only sessions issued
    /// with the same `method` are superseded. `replacement_session_id` is the
    /// session being issued and is never revoked.
    async fn supersede_desktop_bootstrap_sessions(
        &self,
        replacement_session_id: &str,
        method: &str,
    ) -> Result<usize, AuthError> {
        let now = now_ms();
        let mut superseded = if let Some(repositories) = &self.repositories {
            repositories
                .list_active_auth_sessions(format_iso(now))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?
                .into_iter()
                .filter(|session| {
                    session.subject == DESKTOP_BOOTSTRAP_SUBJECT && session.method == method
                })
                .map(|session| session.session_id)
                .collect::<Vec<_>>()
        } else {
            let state = self.state.lock().await;
            state
                .sessions
                .values()
                .filter(|session| {
                    session.subject == DESKTOP_BOOTSTRAP_SUBJECT
                        && session.method == method
                        && session.revoked_at_ms.is_none()
                        && session.expires_at_ms > now
                })
                .map(|session| session.session_id.clone())
                .collect::<Vec<_>>()
        };
        superseded.retain(|session_id| session_id != replacement_session_id);
        superseded.sort_unstable();
        superseded.dedup();
        let mut revoked = 0;
        for session_id in superseded {
            if self.revoke_session(&session_id).await? {
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    async fn revoke_session(&self, target_session_id: &str) -> Result<bool, AuthError> {
        let revoked_at = now_ms();
        if let Some(repositories) = &self.repositories {
            let revoked = repositories
                .revoke_auth_session(target_session_id.to_owned(), format_iso(revoked_at))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            if revoked {
                let mut state = self.state.lock().await;
                state.sessions.remove(target_session_id);
                cancel_live_connections(&mut state, target_session_id);
                state.bump_authority_generation();
                drop(state);
                self.emit_access_change(AuthAccessChange::ClientRemoved {
                    session_id: target_session_id.to_owned(),
                });
            }
            return Ok(revoked);
        }
        let mut state = self.state.lock().await;
        let Some(session) = state.sessions.get_mut(target_session_id) else {
            return Ok(false);
        };
        if session.revoked_at_ms.is_some() {
            return Ok(false);
        }
        session.revoked_at_ms = Some(revoked_at);
        let session_id = session.session_id.clone();
        state.sessions.remove(&session_id);
        cancel_live_connections(&mut state, &session_id);
        state.bump_authority_generation();
        drop(state);
        self.emit_access_change(AuthAccessChange::ClientRemoved { session_id });
        Ok(true)
    }

    pub async fn revoke_other_clients(&self, current_session_id: &str) -> Result<usize, AuthError> {
        let now = now_ms();
        if let Some(repositories) = &self.repositories {
            let removed_session_ids = repositories
                .revoke_other_auth_sessions(current_session_id.to_owned(), format_iso(now))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
            let mut state = self.state.lock().await;
            for session_id in &removed_session_ids {
                state.sessions.remove(session_id);
                cancel_live_connections(&mut state, session_id);
            }
            state.bump_authority_generation();
            drop(state);
            for session_id in &removed_session_ids {
                self.emit_access_change(AuthAccessChange::ClientRemoved {
                    session_id: session_id.clone(),
                });
            }
            return Ok(removed_session_ids.len());
        }
        let mut state = self.state.lock().await;
        let mut revoked = 0;
        let mut removed_session_ids = Vec::new();
        for session in state.sessions.values_mut() {
            if session.session_id != current_session_id && session.revoked_at_ms.is_none() {
                session.revoked_at_ms = Some(now);
                removed_session_ids.push(session.session_id.clone());
                revoked += 1;
            }
        }
        for session_id in &removed_session_ids {
            cancel_live_connections(&mut state, session_id);
        }
        state.bump_authority_generation();
        drop(state);
        for session_id in removed_session_ids {
            self.emit_access_change(AuthAccessChange::ClientRemoved { session_id });
        }
        Ok(revoked)
    }

    /// Test-only convenience over [`Self::mark_connected_guard`]; production
    /// connection lifecycles must hold the guard so disconnection cannot be
    /// skipped.
    #[cfg(test)]
    async fn mark_connected(
        &self,
        session_id: &str,
        shutdown: CancellationToken,
    ) -> Result<u64, AuthError> {
        let mut guard = self.mark_connected_guard(session_id, shutdown).await?;
        Ok(guard
            .connection_id
            .take()
            .expect("new authenticated connection guard is armed"))
    }

    /// Moves `session_id`'s expiry to `expires_at_ms`, as if it had been issued so; only the
    /// device media socket's tests use it.
    #[cfg(all(test, unix))]
    pub(crate) async fn set_session_expiry_for_test(&self, session_id: &str, expires_at_ms: i64) {
        if let Some(session) = self.state.lock().await.sessions.get_mut(session_id) {
            session.expires_at_ms = expires_at_ms;
        }
    }

    #[cfg(test)]
    pub(crate) async fn live_connection_count_for_test(&self, session_id: &str) -> usize {
        self.state
            .lock()
            .await
            .live_connections
            .get(session_id)
            .map_or(0, HashMap::len)
    }

    pub(crate) async fn mark_connected_guard(
        &self,
        session_id: &str,
        shutdown: CancellationToken,
    ) -> Result<AuthenticatedConnectionGuard, AuthError> {
        let mut state = self.state.lock().await;
        let observed_at = now_ms();
        let Some(session) = state.sessions.get_mut(session_id) else {
            drop(state);
            shutdown.cancel();
            return Err(AuthError::InvalidCredential);
        };
        if session.revoked_at_ms.is_some() || session.expires_at_ms <= observed_at {
            drop(state);
            shutdown.cancel();
            return Err(AuthError::InvalidCredential);
        }
        if session.connected_count == 0 {
            session.last_connected_at_ms = Some(observed_at);
        }
        session.connected_count = session.connected_count.saturating_add(1);
        let view = session.view(false);
        state.next_connection_id = state.next_connection_id.wrapping_add(1);
        let connection_id = state.next_connection_id;
        state
            .live_connections
            .entry(session_id.to_owned())
            .or_default()
            .insert(connection_id, shutdown);
        drop(state);
        let guard = AuthenticatedConnectionGuard {
            auth: self.clone(),
            session_id: session_id.to_owned(),
            connection_id: Some(connection_id),
        };
        if let Some(repositories) = &self.repositories
            && let Err(error) = repositories
                .set_auth_session_last_connected_at(
                    session_id.to_owned(),
                    view.last_connected_at
                        .clone()
                        .unwrap_or_else(|| format_iso(observed_at)),
                )
                .await
        {
            tracing::error!(%error, %session_id, "failed to persist session connection time");
        }
        self.emit_access_change(AuthAccessChange::ClientUpserted(view));
        Ok(guard)
    }

    async fn mark_disconnected(&self, session_id: &str, connection_id: u64) {
        let mut state = self.state.lock().await;
        let disconnected = if let Some(connections) = state.live_connections.get_mut(session_id) {
            let disconnected = connections.remove(&connection_id).is_some();
            if connections.is_empty() {
                state.live_connections.remove(session_id);
            }
            disconnected
        } else {
            false
        };
        if !disconnected {
            return;
        }
        let view = if let Some(session) = state.sessions.get_mut(session_id) {
            session.connected_count = session.connected_count.saturating_sub(1);
            Some(session.view(false))
        } else {
            None
        };
        drop(state);
        if let Some(view) = view {
            self.emit_access_change(AuthAccessChange::ClientUpserted(view));
        }
    }

    async fn issue_session(
        &self,
        subject: String,
        scopes: Vec<String>,
        method: &str,
        client: ClientMetadata,
        transport: SessionTransport,
        metadata: AuthSessionIssuanceMetadata,
    ) -> Result<IssuedSession, AuthError> {
        let AuthSessionIssuanceMetadata {
            grant:
                AuthGrantMetadata {
                    desktop_bootstrap,
                    proof_key_thumbprint,
                    reach,
                    off_host,
                },
            delivery_state,
        } = metadata;
        let _issuance = self.issuance.lock().await;
        let issued_at = now_ms();
        let ttl = if proof_key_thumbprint.is_some() {
            DPOP_SESSION_TTL_MS
        } else {
            SESSION_TTL_MS
        };
        let expires_at = issued_at.saturating_add(ttl);
        let session_id = Uuid::new_v4().to_string();
        let claims = SessionClaims {
            v: 1,
            kind: "session".to_owned(),
            sid: session_id.clone(),
            sub: subject.clone(),
            scopes: scopes.clone(),
            method: method.to_owned(),
            jkt: proof_key_thumbprint.clone(),
            tr: transport.claim().to_owned(),
            iat: issued_at,
            exp: expires_at,
        };
        let token = self
            .signer
            .issue(&claims)
            .map_err(|error| AuthError::Internal(error.to_string()))?;
        let record = SessionRecord {
            session_id: session_id.clone(),
            subject: subject.clone(),
            scopes: scopes.clone(),
            method: method.to_owned(),
            client,
            issued_at_ms: issued_at,
            expires_at_ms: expires_at,
            revoked_at_ms: None,
            last_connected_at_ms: None,
            connected_count: 0,
            proof_key_thumbprint: proof_key_thumbprint.clone(),
            transport,
            reach,
            off_host,
            delivery_state,
        };
        {
            let mut state = self.state.lock().await;
            state.sessions.retain(|_, session| {
                session.revoked_at_ms.is_none() && session.expires_at_ms > issued_at
            });
            // Sessions the desktop bootstrap is about to supersede do not hold
            // capacity against the session that replaces them.
            let occupied = if desktop_bootstrap {
                state
                    .sessions
                    .values()
                    .filter(|session| {
                        session.subject != DESKTOP_BOOTSTRAP_SUBJECT || session.method != method
                    })
                    .count()
            } else {
                state.sessions.len()
            };
            if occupied >= MAX_ACTIVE_SESSIONS {
                return Err(AuthError::Internal(
                    "active session capacity exceeded".to_owned(),
                ));
            }
        }
        let view = record.view(false);
        let mut pending_issuance_guard = (delivery_state
            == AuthSessionDeliveryState::PendingPairing)
            .then(|| PendingSessionIssuanceGuard::new(self.clone(), session_id.clone()));
        if let Some(repositories) = &self.repositories {
            repositories
                .create_auth_session(persisted_auth_session(&record))
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?;
        }
        if desktop_bootstrap {
            self.supersede_desktop_bootstrap_sessions(&session_id, method)
                .await?;
        }
        let mut state = self.state.lock().await;
        state.sessions.insert(session_id.clone(), record);
        state.bump_authority_generation();
        drop(state);
        self.emit_access_change(AuthAccessChange::ClientUpserted(view));
        self.ensure_authority_watcher();
        if let Some(guard) = &mut pending_issuance_guard {
            guard.disarm();
        }
        Ok(IssuedSession {
            token,
            principal: Principal {
                session_id,
                subject,
                method: method.to_owned(),
                scopes,
                proof_key_thumbprint,
                expires_at_ms: expires_at,
            },
        })
    }

    async fn consume_grant(
        &self,
        credential: &str,
        proof_key_thumbprint: Option<&str>,
    ) -> Result<Grant, AuthError> {
        let now = now_ms();
        if let Some(desktop) = &self.desktop_bootstrap
            && constant_time_text_equal(&desktop.credential, credential)
        {
            return if desktop.expires_at_ms > now {
                Ok(Grant {
                    scopes: owned_scopes(ADMINISTRATIVE_SCOPES),
                    subject: DESKTOP_BOOTSTRAP_SUBJECT.to_owned(),
                    label: None,
                    reach: None,
                    off_host: None,
                    desktop_bootstrap: true,
                })
            } else {
                Err(AuthError::InvalidCredential)
            };
        }

        if let Some(repositories) = &self.repositories {
            let consumed = repositories
                .consume_auth_pairing_link(
                    credential.to_owned(),
                    proof_key_thumbprint.map(str::to_owned),
                    format_iso(now),
                    format_iso(now),
                )
                .await
                .map_err(|error| AuthError::Internal(error.to_string()))?
                .ok_or(AuthError::InvalidCredential)?;
            let pairing = pairing_record_from_persisted(consumed)?;
            let mut state = self.state.lock().await;
            state.pairings.remove(&pairing.id);
            state.bump_authority_generation();
            drop(state);
            self.emit_access_change(AuthAccessChange::PairingLinkRemoved {
                id: pairing.id.clone(),
            });
            return Ok(Grant {
                scopes: pairing.scopes,
                subject: pairing.subject,
                label: pairing.label,
                reach: pairing.reach,
                off_host: pairing.off_host,
                desktop_bootstrap: false,
            });
        }

        let mut state = self.state.lock().await;
        let pairing = state
            .pairings
            .values_mut()
            .find(|pairing| pairing.credential == credential)
            .ok_or(AuthError::InvalidCredential)?;
        if pairing.revoked_at_ms.is_some()
            || pairing.consumed_at_ms.is_some()
            || pairing.expires_at_ms <= now
            || pairing
                .proof_key_thumbprint
                .as_deref()
                .is_some_and(|expected| Some(expected) != proof_key_thumbprint)
        {
            return Err(AuthError::InvalidCredential);
        }
        pairing.consumed_at_ms = Some(now);
        let pairing_id = pairing.id.clone();
        let grant = Grant {
            scopes: pairing.scopes.clone(),
            subject: pairing.subject.clone(),
            label: pairing.label.clone(),
            reach: pairing.reach.clone(),
            off_host: pairing.off_host,
            desktop_bootstrap: false,
        };
        drop(state);
        self.emit_access_change(AuthAccessChange::PairingLinkRemoved { id: pairing_id });
        Ok(grant)
    }
}

impl SessionRecord {
    fn principal(&self) -> Principal {
        Principal {
            session_id: self.session_id.clone(),
            subject: self.subject.clone(),
            method: self.method.clone(),
            scopes: self.scopes.clone(),
            proof_key_thumbprint: self.proof_key_thumbprint.clone(),
            expires_at_ms: self.expires_at_ms,
        }
    }

    fn view(&self, current: bool) -> ClientSessionView {
        ClientSessionView {
            session_id: self.session_id.clone(),
            subject: self.subject.clone(),
            scopes: self.scopes.clone(),
            method: self.method.clone(),
            client: self.client.clone(),
            issued_at: format_iso(self.issued_at_ms),
            expires_at: format_iso(self.expires_at_ms),
            last_connected_at: self.last_connected_at_ms.map(format_iso),
            connected: self.connected_count > 0,
            current,
            reach: self.reach.clone(),
        }
    }
}

impl PairingRecord {
    fn view(&self) -> PairingLinkView {
        PairingLinkView {
            id: self.id.clone(),
            credential: self.credential.clone(),
            scopes: self.scopes.clone(),
            subject: self.subject.clone(),
            label: self.label.clone(),
            created_at: format_iso(self.created_at_ms),
            expires_at: format_iso(self.expires_at_ms),
            reach: self.reach.clone(),
        }
    }
}

fn cancel_live_connections(state: &mut AuthState, session_id: &str) {
    if let Some(connections) = state.live_connections.remove(session_id) {
        for shutdown in connections.into_values() {
            shutdown.cancel();
        }
    }
}

fn emit_access_change_on(
    events: &broadcast::Sender<AuthAccessEvent>,
    revision: &AtomicU64,
    change: AuthAccessChange,
) {
    let revision = revision.fetch_add(1, Ordering::AcqRel) + 1;
    let _ = events.send(AuthAccessEvent { revision, change });
}

async fn authority_has_consumers(
    state: &Mutex<AuthState>,
    access_events: &broadcast::Sender<AuthAccessEvent>,
) -> bool {
    let state = state.lock().await;
    !state.pairings.is_empty()
        || !state.sessions.is_empty()
        || !state.live_connections.is_empty()
        || access_events.receiver_count() > 0
}

async fn reconcile_authority_snapshot(
    state: &Mutex<AuthState>,
    snapshot: PersistedAuthAuthoritySnapshot,
    expected_generation: u64,
) -> Result<Option<Vec<AuthAccessChange>>, AuthError> {
    let pairings = snapshot
        .pairings
        .into_iter()
        .map(pairing_record_from_persisted)
        .collect::<Result<Vec<_>, _>>()?;
    let sessions = snapshot
        .sessions
        .into_iter()
        .map(session_record_from_persisted)
        .collect::<Result<Vec<_>, _>>()?;
    let offers = snapshot
        .offers
        .into_iter()
        .map(stored_pairing_offer_from_persisted)
        .collect::<Result<HashMap<_, _>, _>>()?;
    let mut state = state.lock().await;
    if state.authority_generation != expected_generation {
        return Ok(None);
    }

    let mut changes = Vec::new();
    let mut next_pairings = HashMap::with_capacity(pairings.len());
    for pairing in pairings {
        if !state.pairings.contains_key(&pairing.id) {
            changes.push(AuthAccessChange::PairingLinkUpserted(pairing.view()));
        }
        next_pairings.insert(pairing.id.clone(), pairing);
    }
    for pairing_id in state.pairings.keys() {
        if !next_pairings.contains_key(pairing_id) {
            changes.push(AuthAccessChange::PairingLinkRemoved {
                id: pairing_id.clone(),
            });
        }
    }

    let mut next_sessions = HashMap::with_capacity(sessions.len());
    for mut session in sessions {
        if let Some(current) = state.sessions.get(&session.session_id) {
            let previous_view = current.view(false);
            session.connected_count = current.connected_count;
            session.proof_key_thumbprint = current.proof_key_thumbprint.clone();
            session.transport = current.transport;
            if current.last_connected_at_ms > session.last_connected_at_ms {
                session.last_connected_at_ms = current.last_connected_at_ms;
            }
            if session.view(false) != previous_view {
                changes.push(AuthAccessChange::ClientUpserted(session.view(false)));
            }
        } else {
            changes.push(AuthAccessChange::ClientUpserted(session.view(false)));
        }
        next_sessions.insert(session.session_id.clone(), session);
    }
    let removed_sessions = state
        .sessions
        .keys()
        .filter(|session_id| !next_sessions.contains_key(*session_id))
        .cloned()
        .collect::<Vec<_>>();
    for session_id in removed_sessions {
        cancel_live_connections(&mut state, &session_id);
        changes.push(AuthAccessChange::ClientRemoved { session_id });
    }

    state.pairings = next_pairings;
    state.sessions = next_sessions;
    state.pairing_offer_idempotency = offers;
    Ok(Some(changes))
}

fn persisted_pairing_link(record: &PairingRecord) -> PersistedPairingLink {
    PersistedPairingLink {
        id: record.id.clone(),
        credential: record.credential.clone(),
        method: "one-time-token".to_owned(),
        scopes: serde_json::json!(record.scopes),
        subject: record.subject.clone(),
        label: record.label.clone(),
        proof_key_thumbprint: record.proof_key_thumbprint.clone(),
        created_at: format_iso(record.created_at_ms),
        expires_at: format_iso(record.expires_at_ms),
        consumed_at: record.consumed_at_ms.map(format_iso),
        revoked_at: record.revoked_at_ms.map(format_iso),
        reach: record.reach.clone(),
        off_host: record.off_host,
    }
}

fn persisted_auth_session(record: &SessionRecord) -> NewAuthSession {
    NewAuthSession {
        session_id: record.session_id.clone(),
        subject: record.subject.clone(),
        scopes: serde_json::json!(record.scopes),
        method: record.method.clone(),
        client: PersistedAuthSessionClient {
            label: record.client.label.clone(),
            ip_address: record.client.ip_address.clone(),
            user_agent: record.client.user_agent.clone(),
            device_type: record.client.device_type.clone(),
            os: record.client.os.clone(),
            browser: record.client.browser.clone(),
        },
        issued_at: format_iso(record.issued_at_ms),
        expires_at: format_iso(record.expires_at_ms),
        reach: record.reach.clone(),
        off_host: record.off_host,
        delivery_state: record.delivery_state,
    }
}

fn pairing_record_from_persisted(row: PersistedPairingLink) -> Result<PairingRecord, AuthError> {
    let scopes = decode_persisted_scopes(row.scopes)?;
    Ok(PairingRecord {
        id: row.id,
        credential: row.credential,
        scopes,
        subject: row.subject,
        label: row.label,
        proof_key_thumbprint: row.proof_key_thumbprint,
        created_at_ms: parse_timestamp_ms(&row.created_at)?,
        expires_at_ms: parse_timestamp_ms(&row.expires_at)?,
        consumed_at_ms: row
            .consumed_at
            .as_deref()
            .map(parse_timestamp_ms)
            .transpose()?,
        revoked_at_ms: row
            .revoked_at
            .as_deref()
            .map(parse_timestamp_ms)
            .transpose()?,
        reach: row.reach,
        off_host: row.off_host,
    })
}

fn stored_pairing_offer_from_persisted(
    offer: PersistedPairingOffer,
) -> Result<((String, String), StoredPairingOffer), AuthError> {
    let expires_at_ms = parse_timestamp_ms(&offer.expires_at)?;
    let result = offer
        .result
        .map(serde_json::from_value::<PairingOfferResult>)
        .transpose()
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    Ok((
        (offer.principal_id, offer.idempotency_key),
        StoredPairingOffer {
            input_fingerprint: offer.input_fingerprint,
            pairing_id: offer.pairing_id,
            result,
            expires_at_ms,
        },
    ))
}

fn pairing_offer_replay_from_persisted(
    offer: &PersistedPairingOffer,
    input_fingerprint: &str,
) -> Result<PairingOfferReplay, AuthError> {
    if offer.cancelled_at.is_some() || offer.result.is_none() {
        return Ok(PairingOfferReplay::Cancelled);
    }
    if offer.input_fingerprint != input_fingerprint {
        return Ok(PairingOfferReplay::Conflict);
    }
    serde_json::from_value::<PairingOfferResult>(
        offer
            .result
            .clone()
            .expect("completed pairing offer checked above"),
    )
    .map(PairingOfferReplay::Original)
    .map_err(|error| AuthError::Internal(error.to_string()))
}

fn reserved_pairing(issuance: PairingIssuance) -> Result<PairingCredentialResult, AuthError> {
    match issuance {
        PairingIssuance::Reserved(issued) => Ok(issued),
        PairingIssuance::Existing(_) => Err(AuthError::Internal(
            "unexpected keyed pairing offer reservation".to_owned(),
        )),
    }
}

fn session_record_from_persisted(row: PersistedAuthSession) -> Result<SessionRecord, AuthError> {
    let scopes = decode_persisted_scopes(row.scopes)?;
    Ok(SessionRecord {
        session_id: row.session_id,
        subject: row.subject,
        scopes,
        method: row.method,
        client: ClientMetadata {
            label: row.client.label,
            ip_address: row.client.ip_address,
            user_agent: row.client.user_agent,
            device_type: row.client.device_type,
            os: row.client.os,
            browser: row.client.browser,
        },
        issued_at_ms: parse_timestamp_ms(&row.issued_at)?,
        expires_at_ms: parse_timestamp_ms(&row.expires_at)?,
        revoked_at_ms: row
            .revoked_at
            .as_deref()
            .map(parse_timestamp_ms)
            .transpose()?,
        last_connected_at_ms: row
            .last_connected_at
            .as_deref()
            .map(parse_timestamp_ms)
            .transpose()?,
        connected_count: 0,
        proof_key_thumbprint: None,
        transport: SessionTransport::Plain,
        reach: row.reach,
        off_host: row.off_host,
        delivery_state: row.delivery_state,
    })
}

fn decode_persisted_scopes(value: serde_json::Value) -> Result<Vec<String>, AuthError> {
    let scopes = serde_json::from_value::<Vec<String>>(value)
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    if scopes.is_empty()
        || scopes.iter().any(|scope| !is_scope(scope))
        || scopes.iter().collect::<HashSet<_>>().len() != scopes.len()
    {
        return Err(AuthError::Internal(
            "persisted authentication scopes are invalid".to_owned(),
        ));
    }
    Ok(scopes)
}

fn parse_timestamp_ms(value: &str) -> Result<i64, AuthError> {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000)
        .map_err(|error| AuthError::Internal(error.to_string()))
}

pub fn parse_scopes(value: &str) -> Result<Vec<String>, AuthError> {
    let scopes = value
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if scopes.is_empty()
        || scopes.iter().any(|scope| !is_scope(scope))
        || scopes.iter().collect::<HashSet<_>>().len() != scopes.len()
    {
        return Err(AuthError::InvalidScope);
    }
    Ok(scopes)
}

#[must_use]
pub fn owned_scopes(scopes: &[&str]) -> Vec<String> {
    scopes.iter().map(|scope| (*scope).to_owned()).collect()
}

#[must_use]
pub fn format_iso(epoch_ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(epoch_ms) * 1_000_000)
        .ok()
        .and_then(|date| date.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn is_scope(scope: &str) -> bool {
    ALL_SCOPES.contains(&scope)
}

pub(crate) fn is_loopback_host(host: &str) -> bool {
    let normalized = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    normalized == "localhost"
        || normalized
            .parse::<IpAddr>()
            .is_ok_and(|address| match address {
                IpAddr::V4(address) => address.is_loopback(),
                IpAddr::V6(address) => {
                    address
                        .to_ipv4_mapped()
                        .is_some_and(|mapped| mapped.is_loopback())
                        || address.is_loopback()
                }
            })
}

pub(crate) fn is_unspecified_host(host: &str) -> bool {
    host.trim()
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .is_ok_and(|address| match address {
            IpAddr::V4(address) => address.is_unspecified(),
            IpAddr::V6(address) => {
                address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_unspecified())
                    || address.is_unspecified()
            }
        })
}

fn apply_grant_label(mut client: ClientMetadata, label: Option<String>) -> ClientMetadata {
    if label.is_some() {
        client.label = label;
    }
    client
}

fn map_token_error_to_credential(_error: TokenError) -> AuthError {
    AuthError::InvalidCredential
}

fn constant_time_text_equal(left: &str, right: &str) -> bool {
    left.len() == right.len() && bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

fn generate_pairing_credential() -> Result<String, AuthError> {
    let mut credential = String::with_capacity(PAIRING_LENGTH);
    while credential.len() < PAIRING_LENGTH {
        let mut bytes = [0_u8; PAIRING_LENGTH];
        getrandom::fill(&mut bytes).map_err(|error| AuthError::Internal(error.to_string()))?;
        for byte in bytes {
            if byte >= PAIRING_REJECTION_LIMIT {
                continue;
            }
            credential.push(char::from(
                PAIRING_ALPHABET[usize::from(byte) % PAIRING_ALPHABET.len()],
            ));
            if credential.len() == PAIRING_LENGTH {
                break;
            }
        }
    }
    Ok(credential)
}

/// Issues a one-time administrative pairing link directly against a data
/// root's repositories, without a full [`AuthService`]. Used by the native CLI
/// (`bibcode pairing issue`) beside a running server: credential consumption
/// reads `auth_pairing_links` from the database, so the running server honors
/// links inserted here. Mirrors `issue_startup_pairing` (administrative
/// scopes, `administrative-bootstrap` subject, five-minute TTL).
pub(crate) async fn issue_administrative_pairing_link(
    repositories: &Repositories,
    label: Option<String>,
) -> Result<PairingCredentialResult, AuthError> {
    let now = now_ms();
    let active = repositories
        .list_active_auth_pairing_links(format_iso(now))
        .await
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    if active.len() >= MAX_ACTIVE_PAIRINGS {
        return Err(AuthError::Internal(
            "active pairing capacity exceeded".to_owned(),
        ));
    }
    let record = PairingRecord {
        id: Uuid::new_v4().to_string(),
        credential: generate_pairing_credential()?,
        scopes: owned_scopes(ADMINISTRATIVE_SCOPES),
        subject: "administrative-bootstrap".to_owned(),
        label: label.clone(),
        proof_key_thumbprint: None,
        created_at_ms: now,
        expires_at_ms: now.saturating_add(PAIRING_TTL_MS),
        consumed_at_ms: None,
        revoked_at_ms: None,
        reach: None,
        off_host: None,
    };
    repositories
        .create_auth_pairing_link(persisted_pairing_link(&record))
        .await
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    Ok(PairingCredentialResult {
        id: record.id,
        credential: record.credential,
        label,
        expires_at: format_iso(record.expires_at_ms),
    })
}

/// Issues a one-time share pairing grant directly against a data root's
/// repositories, without a full [`AuthService`]. Used by `bibcode pairing offer`
/// beside a running server: consumption reads `auth_pairing_links` from the
/// database, so the running server honors grants inserted here. Mirrors
/// `issue_share_pairing` (standard scopes, `one-time-token` subject, reach and
/// off-host recorded, five-minute TTL) rather than the administrative shape,
/// so the off-host confirmation guard and exposure accounting see it.
pub(crate) async fn issue_offline_share_pairing(
    repositories: &Repositories,
    label: Option<String>,
    reach: String,
    off_host: bool,
) -> Result<PairingCredentialResult, AuthError> {
    if !is_valid_pairing_reach(&reach) {
        return Err(AuthError::InvalidCredential);
    }
    let now = now_ms();
    let active = repositories
        .list_active_auth_pairing_links(format_iso(now))
        .await
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    if active.len() >= MAX_ACTIVE_PAIRINGS {
        return Err(AuthError::Internal(
            "active pairing capacity exceeded".to_owned(),
        ));
    }
    let record = PairingRecord {
        id: Uuid::new_v4().to_string(),
        credential: generate_pairing_credential()?,
        scopes: owned_scopes(STANDARD_SCOPES),
        subject: "one-time-token".to_owned(),
        label: label.clone(),
        proof_key_thumbprint: None,
        created_at_ms: now,
        expires_at_ms: now.saturating_add(PAIRING_TTL_MS),
        consumed_at_ms: None,
        revoked_at_ms: None,
        reach: Some(reach),
        off_host: Some(off_host),
    };
    repositories
        .create_auth_pairing_link(persisted_pairing_link(&record))
        .await
        .map_err(|error| AuthError::Internal(error.to_string()))?;
    Ok(PairingCredentialResult {
        id: record.id,
        credential: record.credential,
        label,
        expires_at: format_iso(record.expires_at_ms),
    })
}

#[must_use]
pub fn default_standard_scopes() -> Vec<String> {
    owned_scopes(STANDARD_SCOPES)
}

#[cfg(test)]
mod tests {
    use crate::rpc::{PairingConfirmationLatch, RpcSessionContext};

    use super::*;

    struct IndependentAuthRepositories {
        _database_directory: tempfile::TempDir,
        first: Repositories,
        second: Repositories,
    }

    impl IndependentAuthRepositories {
        async fn new() -> Self {
            let database_directory = tempfile::tempdir().expect("temporary SQLite directory");
            let database_path = database_directory.path().join("auth.sqlite3");
            let first_database = crate::persistence::Database::create_new(&database_path)
                .await
                .expect("first SQLite connection opens");
            first_database
                .call(|connection| {
                    crate::persistence::run_migrations(connection, None)?;
                    Ok(())
                })
                .await
                .expect("all migrations apply");
            let second_database = crate::persistence::Database::open_existing(&database_path)
                .await
                .expect("second independent SQLite connection opens");
            let first = Repositories::new(first_database);
            let second = Repositories::new(second_database);
            let first_observer = first
                .database()
                .enable_queue_backpressure_observation_for_integration_test()
                .expect("first repository owns a SQLite worker");
            let second_observer = second
                .database()
                .enable_queue_backpressure_observation_for_integration_test()
                .expect("second repository must own an independent SQLite worker");
            drop((first_observer, second_observer));
            Self {
                _database_directory: database_directory,
                first,
                second,
            }
        }

        async fn close(self) {
            self.first.database().clone().close().await;
            self.second.database().clone().close().await;
        }
    }

    #[tokio::test]
    async fn issues_a_persisted_administrative_pairing_a_running_service_exchanges_once() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database);

        let issued =
            issue_administrative_pairing_link(&repositories, Some("SSH bootstrap".to_owned()))
                .await
                .expect("pairing link issues");
        assert_eq!(issued.credential.len(), PAIRING_LENGTH);
        assert!(
            issued
                .credential
                .bytes()
                .all(|byte| PAIRING_ALPHABET.contains(&byte))
        );
        assert_eq!(issued.label.as_deref(), Some("SSH bootstrap"));

        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store opens");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let service = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            secret_store,
            repositories.clone(),
        )
        .await
        .expect("service hydrates over the same repositories");

        let session = service
            .exchange_bootstrap(
                &issued.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("credential exchanges once");
        assert_eq!(
            session.principal.scopes,
            owned_scopes(ADMINISTRATIVE_SCOPES)
        );
        assert_eq!(session.principal.subject, "administrative-bootstrap");
        assert!(matches!(
            service
                .exchange_bootstrap(
                    &issued.credential,
                    None,
                    ClientMetadata::default(),
                    None,
                    SessionTransport::Plain,
                )
                .await,
            Err(AuthError::InvalidCredential)
        ));
    }

    #[tokio::test]
    async fn authorization_context_rechecks_a_revoked_session() {
        let service = service();
        let issued = service
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let context = RpcSessionContext::authenticated(issued.principal.clone(), service.clone());

        assert!(context.is_currently_authorized("orchestration:read").await);
        assert!(
            service
                .revoke_client("administrator", &issued.principal.session_id)
                .await
                .expect("revoke session")
        );
        assert!(!context.is_currently_authorized("orchestration:read").await);
    }

    fn desktop_client() -> ClientMetadata {
        ClientMetadata {
            label: Some("BiBCode Tauri Desktop".to_owned()),
            device_type: "desktop".to_owned(),
            os: Some("macOS".to_owned()),
            ..ClientMetadata::default()
        }
    }

    async fn exchange_desktop_bootstrap(service: &AuthService) -> IssuedSession {
        service
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                desktop_client(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("desktop bootstrap exchanges")
    }

    /// Issues and exchanges a one-time pairing so a test owns an independent
    /// client session; the desktop bootstrap credential supersedes its own
    /// earlier sessions, so it cannot mint several coexisting ones.
    async fn paired_session(service: &AuthService, label: &str) -> IssuedSession {
        let pairing = service
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some(label.to_owned()))
            .await
            .expect("pairing issues");
        service
            .exchange_bootstrap(
                &pairing.credential,
                None,
                ClientMetadata {
                    label: Some(label.to_owned()),
                    device_type: "mobile".to_owned(),
                    ..ClientMetadata::default()
                },
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("pairing exchanges")
    }

    #[tokio::test]
    async fn desktop_bootstrap_exchange_supersedes_the_previous_desktop_session() {
        let service = service();
        let first = exchange_desktop_bootstrap(&service).await;
        let second_load = exchange_desktop_bootstrap(&service).await;
        let mut events = service.subscribe_access();
        let third_load = exchange_desktop_bootstrap(&service).await;

        let (_, _, clients) = service
            .access_snapshot(&third_load.principal.session_id)
            .await;
        assert_eq!(
            clients
                .iter()
                .map(|client| client.session_id.as_str())
                .collect::<Vec<_>>(),
            [third_load.principal.session_id.as_str()],
            "the host's WebView holds exactly one session"
        );
        assert!(clients[0].current);
        assert_eq!(
            clients[0].client.label.as_deref(),
            Some("BiBCode Tauri Desktop")
        );

        let removed = events.recv().await.expect("superseded session removal");
        assert!(matches!(
            removed.change,
            AuthAccessChange::ClientRemoved { ref session_id }
                if *session_id == second_load.principal.session_id
        ));
        let upserted = events.recv().await.expect("new session announcement");
        assert!(matches!(
            upserted.change,
            AuthAccessChange::ClientUpserted(ref view)
                if view.session_id == third_load.principal.session_id
        ));

        for stale in [&first, &second_load] {
            let context =
                RpcSessionContext::authenticated(stale.principal.clone(), service.clone());
            assert!(
                !context.is_currently_authorized("orchestration:read").await,
                "a superseded desktop session no longer authorizes"
            );
        }
        let current =
            RpcSessionContext::authenticated(third_load.principal.clone(), service.clone());
        assert!(current.is_currently_authorized("orchestration:read").await);
    }

    #[tokio::test]
    async fn superseding_a_desktop_session_closes_its_live_connections() {
        let service = service();
        let first = exchange_desktop_bootstrap(&service).await;
        let shutdown = CancellationToken::new();
        service
            .mark_connected(&first.principal.session_id, shutdown.clone())
            .await
            .expect("first session connects");
        assert!(!shutdown.is_cancelled());

        let second = exchange_desktop_bootstrap(&service).await;

        assert!(
            shutdown.is_cancelled(),
            "the superseded session's connection is closed"
        );
        let (_, _, clients) = service.access_snapshot(&second.principal.session_id).await;
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].session_id, second.principal.session_id);
        assert!(!clients[0].connected);
    }

    #[tokio::test]
    async fn desktop_bootstrap_keeps_one_bearer_and_one_browser_cookie_session_side_by_side() {
        let service = service();
        let bearer = exchange_desktop_bootstrap(&service).await;
        let cookie = service
            .create_browser_session(
                "desktop-test-seed",
                desktop_client(),
                SessionTransport::Plain,
            )
            .await
            .expect("browser session issues");
        let bearer_again = exchange_desktop_bootstrap(&service).await;
        let cookie_again = service
            .create_browser_session(
                "desktop-test-seed",
                desktop_client(),
                SessionTransport::Plain,
            )
            .await
            .expect("browser session re-issues");

        let (_, _, clients) = service
            .access_snapshot(&bearer_again.principal.session_id)
            .await;
        let mut remaining = clients
            .iter()
            .map(|client| (client.method.as_str(), client.session_id.as_str()))
            .collect::<Vec<_>>();
        remaining.sort_unstable();
        let mut expected = vec![
            (
                "bearer-access-token",
                bearer_again.principal.session_id.as_str(),
            ),
            (
                "browser-session-cookie",
                cookie_again.principal.session_id.as_str(),
            ),
        ];
        expected.sort_unstable();
        assert_eq!(
            remaining, expected,
            "each method keeps exactly its latest desktop session"
        );
        for stale in [&bearer, &cookie] {
            let context =
                RpcSessionContext::authenticated(stale.principal.clone(), service.clone());
            assert!(!context.is_currently_authorized("orchestration:read").await);
        }
    }

    #[tokio::test]
    async fn pairing_exchanges_never_supersede_each_other_or_the_desktop_session() {
        let service = service();
        let desktop = exchange_desktop_bootstrap(&service).await;
        let mut paired = Vec::new();
        for label in ["Phone", "Tablet"] {
            paired.push(paired_session(&service, label).await.principal.session_id);
        }

        let (_, _, clients) = service.access_snapshot(&desktop.principal.session_id).await;
        let mut session_ids = clients
            .iter()
            .map(|client| client.session_id.clone())
            .collect::<Vec<_>>();
        session_ids.sort();
        let mut expected = paired.clone();
        expected.push(desktop.principal.session_id.clone());
        expected.sort();
        assert_eq!(session_ids, expected);
    }

    #[tokio::test(start_paused = true)]
    async fn desktop_supersession_persists_and_survives_authority_convergence() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database);
        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store opens");
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        let service = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            secret_store,
            repositories.clone(),
        )
        .await
        .expect("persisted service starts");

        let first = exchange_desktop_bootstrap(&service).await;
        let _events = service.subscribe_access();
        let second = exchange_desktop_bootstrap(&service).await;

        let persisted_first = repositories
            .get_auth_session(first.principal.session_id.clone())
            .await
            .expect("persisted session reads")
            .expect("superseded session row remains for audit");
        assert!(
            persisted_first.revoked_at.is_some(),
            "supersession is durable, not memory-only"
        );
        let active = repositories
            .list_active_auth_sessions(format_iso(now_ms()))
            .await
            .expect("active sessions read");
        assert_eq!(
            active
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            [second.principal.session_id.as_str()]
        );

        // The authority watcher reconciles memory with the database every 250 ms;
        // the superseded session must not come back.
        for _ in 0..4 {
            tokio::time::advance(AUTHORITY_CONVERGENCE_INTERVAL).await;
            tokio::task::yield_now().await;
        }
        let (_, _, clients) = service.access_snapshot(&second.principal.session_id).await;
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].session_id, second.principal.session_id);
    }

    #[tokio::test]
    async fn revoking_other_clients_cancels_every_registered_connection() {
        let auth = service();
        let current = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("current session");
        let first = paired_session(&auth, "first other").await;
        let second = paired_session(&auth, "second other").await;

        let current_shutdown = CancellationToken::new();
        let first_shutdown_a = CancellationToken::new();
        let first_shutdown_b = CancellationToken::new();
        let second_shutdown = CancellationToken::new();
        let current_connection = auth
            .mark_connected(&current.principal.session_id, current_shutdown.clone())
            .await
            .expect("current connection registers");
        auth.mark_connected(&first.principal.session_id, first_shutdown_a.clone())
            .await
            .expect("first connection registers");
        auth.mark_connected(&first.principal.session_id, first_shutdown_b.clone())
            .await
            .expect("second connection registers");
        auth.mark_connected(&second.principal.session_id, second_shutdown.clone())
            .await
            .expect("third connection registers");

        assert_eq!(
            auth.revoke_other_clients(&current.principal.session_id)
                .await
                .expect("revoke other clients"),
            2
        );
        assert!(!current_shutdown.is_cancelled());
        assert!(first_shutdown_a.is_cancelled());
        assert!(first_shutdown_b.is_cancelled());
        assert!(second_shutdown.is_cancelled());

        auth.mark_disconnected(&current.principal.session_id, current_connection)
            .await;
    }

    #[tokio::test]
    async fn connection_registration_rejects_a_session_revoked_after_authentication() {
        let auth = service();
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        auth.authenticate_token(&issued.token, SessionTransport::Plain)
            .await
            .expect("authentication completes before the race pause");
        let session_id = issued.principal.session_id;
        let connection_shutdown = CancellationToken::new();
        let (paused_tx, paused_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let registering_auth = auth.clone();
        let registering_session_id = session_id.clone();
        let registering_shutdown = connection_shutdown.clone();
        let registration = tokio::spawn(async move {
            paused_tx.send(()).expect("signal deterministic race pause");
            resume_rx.await.expect("resume registration");
            registering_auth
                .mark_connected(&registering_session_id, registering_shutdown)
                .await
        });

        paused_rx.await.expect("registration reached race pause");
        let next_connection_id = auth.state.lock().await.next_connection_id;
        assert!(
            auth.revoke_client("other-session", &session_id)
                .await
                .expect("revoke authenticated session")
        );
        resume_tx.send(()).expect("release registration");
        assert!(matches!(
            registration.await.expect("registration task completes"),
            Err(AuthError::InvalidCredential)
        ));

        assert!(connection_shutdown.is_cancelled());
        let state = auth.state.lock().await;
        assert_eq!(state.next_connection_id, next_connection_id);
        assert!(!state.live_connections.contains_key(&session_id));
    }

    #[tokio::test]
    async fn connection_registration_rejects_an_expired_extant_session() {
        let auth = service();
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let session_id = issued.principal.session_id;
        {
            let mut state = auth.state.lock().await;
            state
                .sessions
                .get_mut(&session_id)
                .expect("cached session")
                .expires_at_ms = now_ms() - 1;
        }
        let connection_shutdown = CancellationToken::new();
        let next_connection_id = auth.state.lock().await.next_connection_id;

        assert!(matches!(
            auth.mark_connected(&session_id, connection_shutdown.clone())
                .await,
            Err(AuthError::InvalidCredential)
        ));

        assert!(connection_shutdown.is_cancelled());
        let state = auth.state.lock().await;
        assert_eq!(state.next_connection_id, next_connection_id);
        assert!(!state.live_connections.contains_key(&session_id));
        assert_eq!(
            state
                .sessions
                .get(&session_id)
                .expect("expired record remains available for the assertion")
                .connected_count,
            0
        );
    }

    #[tokio::test]
    async fn disconnect_bookkeeping_ignores_unadmitted_connection_ids() {
        let auth = service();
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let session_id = issued.principal.session_id;
        let connection_id = auth
            .mark_connected(&session_id, CancellationToken::new())
            .await
            .expect("connection registers");

        auth.mark_disconnected(&session_id, connection_id.wrapping_add(1))
            .await;

        assert_eq!(
            auth.state
                .lock()
                .await
                .sessions
                .get(&session_id)
                .expect("live session")
                .connected_count,
            1
        );
        auth.mark_disconnected(&session_id, connection_id).await;
    }

    #[tokio::test]
    async fn dropped_connection_guard_releases_connected_accounting_exactly_once() {
        let auth = service();
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let session_id = issued.principal.session_id;
        let guard = auth
            .mark_connected_guard(&session_id, CancellationToken::new())
            .await
            .expect("connection guard registers");
        assert_eq!(
            auth.state
                .lock()
                .await
                .sessions
                .get(&session_id)
                .expect("live session")
                .connected_count,
            1
        );

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if auth
                    .state
                    .lock()
                    .await
                    .sessions
                    .get(&session_id)
                    .expect("live session")
                    .connected_count
                    == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("guard drop schedules disconnect bookkeeping");
        auth.mark_disconnected(&session_id, u64::MAX).await;
        assert_eq!(
            auth.state
                .lock()
                .await
                .sessions
                .get(&session_id)
                .expect("live session")
                .connected_count,
            0
        );
    }

    #[tokio::test]
    async fn cancelled_guard_registration_releases_bookkeeping_while_persistence_is_queued() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database.clone());
        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store opens");
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        let auth =
            AuthService::new_with_persistence(&config, vec![7_u8; 32], secret_store, repositories)
                .await
                .expect("persistent auth service starts");
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let session_id = issued.principal.session_id;

        let (worker_entered_tx, worker_entered_rx) = tokio::sync::oneshot::channel();
        let (release_worker_tx, release_worker_rx) = std::sync::mpsc::channel();
        let blocking_database = database.clone();
        let blocker = tokio::spawn(async move {
            blocking_database
                .call(move |_connection| {
                    worker_entered_tx.send(()).expect("signal blocked worker");
                    release_worker_rx.recv().expect("worker release signal");
                    Ok(())
                })
                .await
        });
        worker_entered_rx.await.expect("database worker is blocked");

        let registering_auth = auth.clone();
        let registering_session_id = session_id.clone();
        let registration = tokio::spawn(async move {
            registering_auth
                .mark_connected_guard(&registering_session_id, CancellationToken::new())
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if auth
                    .state
                    .lock()
                    .await
                    .sessions
                    .get(&session_id)
                    .expect("live session")
                    .connected_count
                    == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("registration mutates in-memory bookkeeping before persistence");

        registration.abort();
        let registration_result = registration.await;
        assert!(matches!(registration_result, Err(error) if error.is_cancelled()));
        release_worker_tx.send(()).expect("release database worker");
        blocker
            .await
            .expect("database blocker joins")
            .expect("database blocker succeeds");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let state = auth.state.lock().await;
                let connected_count = state
                    .sessions
                    .get(&session_id)
                    .expect("live session")
                    .connected_count;
                let has_live_token = state.live_connections.contains_key(&session_id);
                drop(state);
                if connected_count == 0 && !has_live_token {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled registration releases bookkeeping");
        database.close().await;
    }

    #[tokio::test]
    async fn cancelled_pending_session_issuance_revokes_durable_commit_before_state_publication() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database.clone());
        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store opens");
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        let auth = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            secret_store,
            repositories.clone(),
        )
        .await
        .expect("persistent auth service starts");

        let initial_state_guard = auth.state.lock().await;
        let exchanging_auth = auth.clone();
        // The desktop seed consumes no shared state before issuance, which is
        // what lets this test hold the state lock across the exchange; force
        // the pending delivery the guard protects through the private seam.
        let exchange = tokio::spawn(async move {
            exchanging_auth
                .exchange_bootstrap_with_delivery(
                    "desktop-test-seed",
                    None,
                    ClientMetadata::default(),
                    None,
                    SessionTransport::E2ee,
                    AuthSessionDeliveryState::PendingPairing,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if auth.issuance.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session issuance waits for the initial state lock");

        drop(initial_state_guard);
        let publication_state_guard = auth.state.lock().await;
        let pending_session = repositories
            .list_active_auth_sessions(format_iso(now_ms()))
            .await
            .expect("durable sessions list")
            .into_iter()
            .find(|session| session.delivery_state == AuthSessionDeliveryState::PendingPairing)
            .expect("pending session commits before state publication");
        assert!(
            !publication_state_guard
                .sessions
                .contains_key(&pending_session.session_id)
        );

        exchange.abort();
        let exchange_result = exchange.await;
        assert!(matches!(exchange_result, Err(error) if error.is_cancelled()));
        drop(publication_state_guard);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let persisted = repositories
                    .get_auth_session(pending_session.session_id.clone())
                    .await
                    .expect("persisted session reads")
                    .expect("persisted pending session remains auditable");
                if persisted.revoked_at.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled pending issuance revokes its durable session");
        assert!(
            !auth
                .state
                .lock()
                .await
                .sessions
                .contains_key(&pending_session.session_id)
        );
        database.close().await;
    }

    #[tokio::test]
    async fn cancelled_confirmation_owns_activation_through_latch_publication() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database.clone());
        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store opens");
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        let auth = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            secret_store,
            repositories.clone(),
        )
        .await
        .expect("persistent auth service starts");
        let offer = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "another-device".to_owned(),
                true,
            )
            .await
            .expect("off-host share pairing");
        let (issued, confirmation_required) = auth
            .exchange_pairing_bootstrap(&offer.credential, ClientMetadata::default())
            .await
            .expect("pending session issues");
        assert!(confirmation_required);
        let session_id = issued.principal.session_id.clone();
        let latch = PairingConfirmationLatch::default();
        let context = RpcSessionContext::authenticated_pending_pairing(
            issued.principal,
            auth.clone(),
            latch.clone(),
        );

        let publication_state_guard = auth.state.lock().await;
        let confirmation = tokio::spawn(async move { context.confirm_current_pairing().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let persisted = repositories
                    .get_auth_session(session_id.clone())
                    .await
                    .expect("persisted session reads")
                    .expect("persisted session exists");
                if persisted.delivery_state == AuthSessionDeliveryState::Active {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable activation commits while state publication is blocked");
        assert_eq!(
            publication_state_guard
                .sessions
                .get(&session_id)
                .expect("cached pending session exists")
                .delivery_state,
            AuthSessionDeliveryState::PendingPairing
        );

        confirmation.abort();
        assert!(matches!(confirmation.await, Err(error) if error.is_cancelled()));
        drop(publication_state_guard);

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let active = auth
                    .state
                    .lock()
                    .await
                    .sessions
                    .get(&session_id)
                    .is_some_and(|session| {
                        session.delivery_state == AuthSessionDeliveryState::Active
                    });
                if active && latch.is_confirmed() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned activation publishes cached state and confirmation latch");
        assert!(
            !auth
                .revoke_failed_pairing_session(&session_id)
                .await
                .expect("delivery-guard compensation checks the session")
        );
        auth.authenticate_token(&issued.token, SessionTransport::E2ee)
            .await
            .expect("confirmed credential remains authoritative");
        database.close().await;
    }

    fn service() -> AuthService {
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        AuthService::new(&config, vec![7_u8; 32])
    }

    #[tokio::test]
    async fn active_session_expiry_requires_a_live_unrevoked_session() {
        let auth = service();
        let issue = || {
            auth.exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
        };
        let live = issue().await.expect("session should issue");
        let live_id = live.principal.session_id;
        let expected = auth.state.lock().await.sessions[&live_id].expires_at_ms;
        assert_eq!(auth.active_session_expiry(&live_id).await, Some(expected));
        assert_eq!(auth.active_session_expiry("missing").await, None);

        auth.state
            .lock()
            .await
            .sessions
            .get_mut(&live_id)
            .expect("session record")
            .expires_at_ms = now_ms() - 1;
        assert_eq!(auth.active_session_expiry(&live_id).await, None);

        let revoked = issue().await.expect("session should issue");
        auth.revoke_client("administrator", &revoked.principal.session_id)
            .await
            .expect("revoke session");
        assert_eq!(
            auth.active_session_expiry(&revoked.principal.session_id)
                .await,
            None
        );
    }

    #[tokio::test]
    async fn share_pairing_reach_is_recorded_and_inherited_by_sessions() {
        let auth = service();
        let issued = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                Some("Tablet".to_owned()),
                "another-device".to_owned(),
                true,
            )
            .await
            .expect("share pairing");
        let listed = auth.list_pairings().await;
        assert_eq!(listed[0].reach.as_deref(), Some("another-device"));

        let session = auth
            .exchange_bootstrap(
                &issued.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let clients = auth.list_clients(&session.principal.session_id).await;
        let paired = clients
            .iter()
            .find(|client| client.session_id == session.principal.session_id)
            .expect("paired client");
        assert_eq!(paired.reach.as_deref(), Some("another-device"));
    }

    #[tokio::test]
    async fn off_host_pairing_mints_pending_and_refuses_the_bearer_until_confirmed() {
        let auth = service();
        let offer = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "another-device".to_owned(),
                true,
            )
            .await
            .expect("off-host share pairing");
        let (issued, confirmation_required) = auth
            .exchange_pairing_bootstrap(&offer.credential, ClientMetadata::default())
            .await
            .expect("pending session issues");
        assert!(
            confirmation_required,
            "an off-host grant always requires confirmation; the client sends no flag"
        );
        assert!(matches!(
            auth.authenticate_token(&issued.token, SessionTransport::E2ee)
                .await,
            Err(AuthError::InvalidCredential)
        ));

        assert!(
            auth.confirm_pending_pairing_session(&issued.principal.session_id)
                .await
                .expect("confirmation commits")
        );
        auth.authenticate_token(&issued.token, SessionTransport::E2ee)
            .await
            .expect("confirmed credential authenticates");
    }

    #[tokio::test]
    async fn on_host_pairing_mints_an_active_credential_without_confirmation() {
        let auth = service();
        let offer = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "this-computer".to_owned(),
                false,
            )
            .await
            .expect("on-host share pairing");
        let (issued, confirmation_required) = auth
            .exchange_pairing_bootstrap(&offer.credential, ClientMetadata::default())
            .await
            .expect("active session issues");
        assert!(!confirmation_required);
        auth.authenticate_token(&issued.token, SessionTransport::E2ee)
            .await
            .expect("on-host credential is delivered immediately");
    }

    #[tokio::test]
    async fn pending_pairing_sweep_only_revokes_sessions_past_the_grace_window() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database.clone());
        let now = now_ms();
        let session_id = Uuid::new_v4().to_string();
        repositories
            .create_auth_session(NewAuthSession {
                session_id: session_id.clone(),
                subject: "pairing-sweep-test".to_owned(),
                scopes: serde_json::json!(owned_scopes(STANDARD_SCOPES)),
                method: "bearer-access-token".to_owned(),
                client: PersistedAuthSessionClient {
                    label: Some("Pairing sweep test".to_owned()),
                    ip_address: None,
                    user_agent: None,
                    device_type: "desktop".to_owned(),
                    os: None,
                    browser: None,
                },
                issued_at: format_iso(now),
                expires_at: format_iso(now + SESSION_TTL_MS),
                reach: Some(PAIRING_REACH_ANOTHER_DEVICE.to_owned()),
                off_host: Some(true),
                delivery_state: AuthSessionDeliveryState::PendingPairing,
            })
            .await
            .expect("pending session persists");

        // A sweep whose cutoff predates the mint leaves the fresh pending
        // session alone — a restart cannot revoke an in-flight pairing.
        let swept = repositories
            .revoke_pending_auth_sessions(
                format_iso(now),
                format_iso(now - PENDING_PAIRING_SWEEP_GRACE_MS),
            )
            .await
            .expect("age-gated sweep runs");
        assert!(swept.is_empty(), "fresh pending sessions survive the sweep");

        // Once the session is older than the grace window it is a crash
        // orphan and the sweep revokes it.
        let swept = repositories
            .revoke_pending_auth_sessions(format_iso(now), format_iso(now))
            .await
            .expect("expired sweep runs");
        assert_eq!(swept, vec![session_id]);
        database.close().await;
    }

    #[tokio::test]
    async fn confirm_pairing_policy_is_pending_capability_or_access_write() {
        use crate::auth::model::SCOPE_ACCESS_WRITE;
        // The declared scope governs third-party callers…
        assert_eq!(
            crate::auth::required_scope("auth.confirmPairing"),
            Some(SCOPE_ACCESS_WRITE)
        );
        // …which standard device grants do not carry, so a delivered
        // (non-pending) device session cannot invoke confirmation…
        let auth = service();
        let offer = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "this-computer".to_owned(),
                false,
            )
            .await
            .expect("on-host share pairing");
        let (issued, _) = auth
            .exchange_pairing_bootstrap(&offer.credential, ClientMetadata::default())
            .await
            .expect("active session issues");
        assert!(matches!(
            auth.authorize_session(&issued.principal.session_id, SCOPE_ACCESS_WRITE)
                .await,
            Err(AuthError::ScopeRequired(_))
        ));
        // …while a pending session confirms its own delivery through the
        // session capability gate regardless of scopes (see
        // pending_pairing_capability_is_limited_to_confirmation and the e2ee
        // scope-bypass suite).
    }

    #[tokio::test]
    async fn websocket_tickets_are_single_use_and_plain_transport_only() {
        let auth = service();
        let offer = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "this-computer".to_owned(),
                false,
            )
            .await
            .expect("plain share pairing");
        let plain = auth
            .exchange_bootstrap(
                &offer.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("plain session");
        let (ticket, _expires_at) = auth
            .issue_websocket_ticket(&plain.principal)
            .expect("ticket issues");
        auth.verify_websocket_ticket(&ticket)
            .await
            .expect("first redemption authenticates");
        assert!(
            matches!(
                auth.verify_websocket_ticket(&ticket).await,
                Err(AuthError::InvalidCredential)
            ),
            "a websocket ticket is redeemable exactly once"
        );

        let e2ee_offer = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "this-computer".to_owned(),
                false,
            )
            .await
            .expect("e2ee share pairing");
        let (e2ee_session, _) = auth
            .exchange_pairing_bootstrap(&e2ee_offer.credential, ClientMetadata::default())
            .await
            .expect("e2ee session");
        let (e2ee_ticket, _expires_at) = auth
            .issue_websocket_ticket(&e2ee_session.principal)
            .expect("ticket issues for the wrong transport");
        assert!(
            matches!(
                auth.verify_websocket_ticket(&e2ee_ticket).await,
                Err(AuthError::InvalidCredential)
            ),
            "tickets never authenticate a non-plain-transport session"
        );
    }

    #[tokio::test]
    async fn share_pairing_rejects_unknown_reach() {
        let auth = service();
        let error = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "everywhere".to_owned(),
                true,
            )
            .await
            .expect_err("invalid reach");
        assert!(matches!(error, AuthError::InvalidCredential));
    }

    #[tokio::test]
    async fn share_exposure_derives_wide_only_from_native_managed_off_host_grants() {
        let auth = service();
        let state = auth.share_exposure_state().await;
        assert_eq!(state.desired_exposure, "loopback");

        auth.issue_share_pairing(
            owned_scopes(STANDARD_SCOPES),
            None,
            "this-computer".to_owned(),
            false,
        )
        .await
        .expect("this-computer pairing");
        assert_eq!(
            auth.share_exposure_state().await.desired_exposure,
            "loopback"
        );

        auth.issue_share_pairing(
            owned_scopes(STANDARD_SCOPES),
            None,
            "custom".to_owned(),
            false,
        )
        .await
        .expect("loopback custom pairing");
        assert_eq!(
            auth.share_exposure_state().await.desired_exposure,
            "loopback"
        );

        let off_host = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "another-device".to_owned(),
                true,
            )
            .await
            .expect("off-host pairing");
        let state = auth.share_exposure_state().await;
        assert_eq!(state.desired_exposure, "wide");
        assert_eq!(state.off_host_grant_count, 1);

        let session = auth
            .exchange_bootstrap(
                &off_host.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("off-host session");
        assert_eq!(auth.share_exposure_state().await.desired_exposure, "wide");

        auth.revoke_client("administrator", &session.principal.session_id)
            .await
            .expect("revoke off-host session");
        assert_eq!(
            auth.share_exposure_state().await.desired_exposure,
            "loopback"
        );
    }

    #[tokio::test]
    async fn custom_off_host_grants_remain_externally_managed_after_exchange() {
        let auth = service();
        let custom = auth
            .issue_share_pairing(
                owned_scopes(STANDARD_SCOPES),
                None,
                "custom".to_owned(),
                true,
            )
            .await
            .expect("custom off-host pairing");

        let state = auth.share_exposure_state().await;
        assert_eq!(state.desired_exposure, "loopback");
        assert_eq!(state.off_host_grant_count, 1);

        auth.exchange_bootstrap(
            &custom.credential,
            None,
            ClientMetadata::default(),
            None,
            SessionTransport::Plain,
        )
        .await
        .expect("custom off-host session");

        let state = auth.share_exposure_state().await;
        assert_eq!(state.desired_exposure, "loopback");
        assert_eq!(state.off_host_grant_count, 1);
    }

    #[tokio::test]
    async fn legacy_null_reach_grants_count_separately_and_never_widen() {
        let auth = service();
        let legacy = auth
            .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
            .await
            .expect("legacy pairing");
        auth.exchange_bootstrap(
            &legacy.credential,
            None,
            ClientMetadata::default(),
            None,
            SessionTransport::Plain,
        )
        .await
        .expect("legacy session");

        let state = auth.share_exposure_state().await;
        assert_eq!(state.desired_exposure, "loopback");
        assert_eq!(state.legacy_grant_count, 1);
    }

    #[tokio::test]
    async fn e2ee_minted_tokens_are_rejected_on_plain_surfaces() {
        let auth = service();
        let pairing = auth
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("device".to_owned()))
            .await
            .expect("pairing issues");
        let issued = auth
            .exchange_bootstrap(
                &pairing.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::E2ee,
            )
            .await
            .expect("e2ee session issues");

        assert!(
            auth.authenticate_token(&issued.token, SessionTransport::E2ee)
                .await
                .is_ok()
        );
        assert!(matches!(
            auth.authenticate_token(&issued.token, SessionTransport::Plain)
                .await,
            Err(AuthError::InvalidCredential)
        ));
    }

    #[tokio::test]
    async fn plain_minted_tokens_still_work_on_both_surfaces() {
        let auth = service();
        let pairing = auth
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("device".to_owned()))
            .await
            .expect("pairing issues");
        let issued = auth
            .exchange_bootstrap(
                &pairing.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("plain session issues");

        assert!(
            auth.authenticate_token(&issued.token, SessionTransport::Plain)
                .await
                .is_ok()
        );
        assert!(
            auth.authenticate_token(&issued.token, SessionTransport::E2ee)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn legacy_tokens_without_a_transport_claim_decode_as_plain() {
        let auth = service();
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("plain session issues");
        let mut claims = auth
            .signer
            .verify::<serde_json::Value>(&issued.token)
            .expect("issued claims decode");
        claims
            .as_object_mut()
            .expect("claims are an object")
            .remove("tr");
        let legacy_token = auth.signer.issue(&claims).expect("legacy token signs");

        assert!(
            auth.authenticate_token(&legacy_token, SessionTransport::Plain)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn rejects_expired_session_claims_and_parent_sessions_for_websocket_tickets() {
        let service = service();
        let expired = SessionClaims {
            v: 1,
            kind: "session".to_owned(),
            sid: Uuid::new_v4().to_string(),
            sub: "expired".to_owned(),
            scopes: owned_scopes(STANDARD_SCOPES),
            method: "bearer-access-token".to_owned(),
            jkt: None,
            tr: SessionTransport::Plain.claim().to_owned(),
            iat: now_ms() - 2_000,
            exp: now_ms() - 1_000,
        };
        let token = service.signer.issue(&expired).expect("expired token");
        assert!(matches!(
            service
                .authenticate_token(&token, SessionTransport::Plain)
                .await,
            Err(AuthError::InvalidCredential)
        ));

        let issued = service
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session");
        let (ticket, _) = service
            .issue_websocket_ticket(&issued.principal)
            .expect("ticket");
        service
            .state
            .lock()
            .await
            .sessions
            .get_mut(&issued.principal.session_id)
            .expect("session row")
            .expires_at_ms = now_ms() - 1;
        assert!(matches!(
            service.verify_websocket_ticket(&ticket).await,
            Err(AuthError::InvalidCredential)
        ));
    }

    #[tokio::test]
    async fn consumes_pairing_credentials_atomically_under_race() {
        let service = service();
        let pairing = service
            .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
            .await
            .expect("pairing");
        let first = service.exchange_bootstrap(
            &pairing.credential,
            None,
            ClientMetadata::default(),
            None,
            SessionTransport::Plain,
        );
        let second = service.exchange_bootstrap(
            &pairing.credential,
            None,
            ClientMetadata::default(),
            None,
            SessionTransport::Plain,
        );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    }

    #[tokio::test]
    async fn rejects_expired_pairing_credentials_without_consuming_them() {
        let service = service();
        let pairing = service
            .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
            .await
            .expect("pairing");
        service
            .state
            .lock()
            .await
            .pairings
            .get_mut(&pairing.id)
            .expect("pairing row")
            .expires_at_ms = now_ms() - 1;

        assert!(matches!(
            service
                .exchange_bootstrap(
                    &pairing.credential,
                    None,
                    ClientMetadata::default(),
                    None,
                    SessionTransport::Plain,
                )
                .await,
            Err(AuthError::InvalidCredential)
        ));
    }

    #[tokio::test]
    async fn proof_pairings_and_client_access_lifecycle_cover_public_auth_operations() {
        let service = service();
        assert!(matches!(
            service
                .issue_pairing_with_proof(
                    owned_scopes(STANDARD_SCOPES),
                    Some("Proof client".to_owned()),
                    "   ".to_owned(),
                )
                .await,
            Err(AuthError::InvalidCredential)
        ));
        assert!(matches!(
            service.issue_cloud_pairing(String::new()).await,
            Err(AuthError::InvalidCredential)
        ));

        let proof_pairing = service
            .issue_pairing_with_proof(
                owned_scopes(STANDARD_SCOPES),
                Some("Proof client".to_owned()),
                "proof-thumbprint".to_owned(),
            )
            .await
            .expect("proof pairing should issue");
        assert!(matches!(
            service
                .exchange_bootstrap(
                    &proof_pairing.credential,
                    None,
                    ClientMetadata::default(),
                    Some("wrong-thumbprint".to_owned()),
                    SessionTransport::Plain,
                )
                .await,
            Err(AuthError::InvalidCredential)
        ));
        let proof_session = service
            .exchange_bootstrap(
                &proof_pairing.credential,
                None,
                ClientMetadata::default(),
                Some("proof-thumbprint".to_owned()),
                SessionTransport::Plain,
            )
            .await
            .expect("proof pairing should exchange");
        assert_eq!(
            proof_session.principal.proof_key_thumbprint.as_deref(),
            Some("proof-thumbprint")
        );

        let cloud_pairing = service
            .issue_cloud_pairing("cloud-thumbprint".to_owned())
            .await
            .expect("cloud pairing should issue");
        assert!(
            service
                .list_pairings()
                .await
                .iter()
                .any(|pairing| pairing.id == cloud_pairing.id)
        );
        assert!(!service.revoke_pairing("missing-pairing").await.unwrap());
        assert!(service.revoke_pairing(&cloud_pairing.id).await.unwrap());
        assert!(!service.revoke_pairing(&cloud_pairing.id).await.unwrap());

        let current = service
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("current session should issue");
        let other = paired_session(&service, "other").await;
        let connection_id = service
            .mark_connected(&current.principal.session_id, CancellationToken::new())
            .await
            .expect("connection registers");
        let clients = service.list_clients(&current.principal.session_id).await;
        assert_eq!(clients.len(), 3);
        assert!(clients[0].current);
        assert!(clients[0].connected);
        let (revision, pairings, snapshot_clients) =
            service.access_snapshot(&current.principal.session_id).await;
        assert!(revision > 1);
        assert!(pairings.is_empty());
        assert_eq!(snapshot_clients.len(), 3);
        service
            .mark_disconnected(&current.principal.session_id, connection_id)
            .await;

        assert!(matches!(
            service
                .revoke_client(&current.principal.session_id, &current.principal.session_id,)
                .await,
            Err(AuthError::CurrentSessionRevokeNotAllowed)
        ));
        assert!(
            !service
                .revoke_client(&current.principal.session_id, "missing-session")
                .await
                .unwrap()
        );
        assert_eq!(
            service
                .revoke_other_clients(&current.principal.session_id)
                .await
                .expect("other sessions should revoke"),
            2,
        );
        assert_eq!(
            service
                .list_clients(&current.principal.session_id)
                .await
                .len(),
            1
        );
        assert!(matches!(
            service
                .authenticate_token(&other.token, SessionTransport::Plain)
                .await,
            Err(AuthError::InvalidCredential)
        ));
    }

    #[tokio::test]
    async fn auth_error_boundaries_cover_policy_scope_expiry_and_revocation_paths() {
        let unsafe_config = ServerConfig::new(".").with_unsafe_no_auth();
        assert_eq!(
            AuthService::new(&unsafe_config, vec![1_u8; 32])
                .descriptor()
                .policy,
            "unsafe-no-auth"
        );
        let remote_desktop = ServerConfig::new(".")
            .with_bind("0.0.0.0", 3773)
            .with_desktop("remote-desktop")
            .expect("remote desktop config");
        assert_eq!(
            AuthService::new(&remote_desktop, vec![2_u8; 32])
                .descriptor()
                .bootstrap_methods,
            vec!["desktop-bootstrap", "one-time-token"]
        );

        let auth = service();
        assert!(matches!(
            auth.issue_pairing(Vec::new(), None).await,
            Err(AuthError::InvalidScope)
        ));
        let pairing = auth
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("Snapshot".to_owned()))
            .await
            .expect("pairing should issue");
        assert_eq!(auth.access_snapshot("missing").await.1.len(), 1);
        assert!(matches!(
            auth.exchange_bootstrap(
                &pairing.credential,
                Some(vec!["access:write".to_owned()]),
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await,
            Err(AuthError::ScopeNotGranted)
        ));

        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session should issue");
        assert!(matches!(
            auth
                .authorize_session(&issued.principal.session_id, "missing:scope")
                .await,
            Err(AuthError::ScopeRequired(scope)) if scope == "missing:scope"
        ));
        auth.state
            .lock()
            .await
            .sessions
            .get_mut(&issued.principal.session_id)
            .expect("session record")
            .expires_at_ms = now_ms() - 1;
        assert!(matches!(
            auth.authorize_session(&issued.principal.session_id, "orchestration:read")
                .await,
            Err(AuthError::InvalidCredential)
        ));

        let mismatched = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("mismatched session should issue");
        auth.state
            .lock()
            .await
            .sessions
            .get_mut(&mismatched.principal.session_id)
            .expect("mismatched session record")
            .subject = "changed-subject".to_owned();
        assert!(matches!(
            auth.authenticate_token(&mismatched.token, SessionTransport::Plain)
                .await,
            Err(AuthError::InvalidCredential)
        ));

        let revoked_pairing = auth
            .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
            .await
            .expect("revoked pairing should issue");
        auth.state
            .lock()
            .await
            .pairings
            .get_mut(&revoked_pairing.id)
            .expect("revoked pairing record")
            .revoked_at_ms = Some(now_ms());
        assert!(!auth.revoke_pairing(&revoked_pairing.id).await.unwrap());

        let revoked_client = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("revoked client should issue");
        auth.state
            .lock()
            .await
            .sessions
            .get_mut(&revoked_client.principal.session_id)
            .expect("revoked client record")
            .revoked_at_ms = Some(now_ms());
        assert!(
            !auth
                .revoke_client("current", &revoked_client.principal.session_id)
                .await
                .unwrap()
        );
        let removable = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("removable client should issue");
        assert!(
            auth.revoke_client("current", &removable.principal.session_id)
                .await
                .unwrap()
        );
        assert!(matches!(
            auth.mark_connected("missing-session", CancellationToken::new())
                .await,
            Err(AuthError::InvalidCredential)
        ));

        let mut expired_service = service();
        expired_service
            .desktop_bootstrap
            .as_mut()
            .expect("desktop bootstrap")
            .expires_at_ms = now_ms() - 1;
        assert!(matches!(
            expired_service
                .exchange_bootstrap(
                    "desktop-test-seed",
                    None,
                    ClientMetadata::default(),
                    None,
                    SessionTransport::Plain,
                )
                .await,
            Err(AuthError::InvalidCredential)
        ));
        assert!(decode_persisted_scopes(serde_json::json!([])).is_err());
        assert!(decode_persisted_scopes(serde_json::json!(["unknown:scope"])).is_err());
    }

    #[test]
    fn parses_only_unique_known_non_empty_scopes() {
        assert_eq!(
            parse_scopes("orchestration:read terminal:operate").expect("scopes"),
            ["orchestration:read", "terminal:operate"]
        );
        for invalid in ["", "unknown:scope", "orchestration:read orchestration:read"] {
            assert!(matches!(
                parse_scopes(invalid),
                Err(AuthError::InvalidScope)
            ));
        }
        assert!(decode_persisted_scopes(serde_json::json!(42)).is_err());
        assert!(parse_timestamp_ms("not-a-timestamp").is_err());
        assert_eq!(format_iso(i64::MAX), "1970-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn credential_issuance_prunes_expired_state_and_caps_active_memory() {
        let service = service();
        let now = now_ms();
        {
            let mut state = service.state.lock().await;
            for index in 0..MAX_ACTIVE_PAIRINGS {
                let id = format!("pairing-{index}");
                state.pairings.insert(
                    id.clone(),
                    PairingRecord {
                        id,
                        credential: format!("CREDENTIAL{index}"),
                        scopes: owned_scopes(STANDARD_SCOPES),
                        subject: "test".to_owned(),
                        label: None,
                        proof_key_thumbprint: None,
                        created_at_ms: now,
                        expires_at_ms: now + PAIRING_TTL_MS,
                        consumed_at_ms: None,
                        revoked_at_ms: None,
                        reach: None,
                        off_host: None,
                    },
                );
            }
        }
        assert!(matches!(
            service
                .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
                .await,
            Err(AuthError::Internal(message)) if message == "active pairing capacity exceeded"
        ));
        service
            .state
            .lock()
            .await
            .pairings
            .get_mut("pairing-0")
            .expect("pairing fixture")
            .expires_at_ms = now - 1;
        service
            .issue_pairing(owned_scopes(STANDARD_SCOPES), None)
            .await
            .expect("expired pairing frees capacity");
        assert_eq!(
            service.state.lock().await.pairings.len(),
            MAX_ACTIVE_PAIRINGS
        );

        {
            let mut state = service.state.lock().await;
            for index in 0..MAX_ACTIVE_SESSIONS {
                let session_id = format!("session-{index}");
                state.sessions.insert(
                    session_id.clone(),
                    SessionRecord {
                        session_id,
                        subject: "test".to_owned(),
                        scopes: owned_scopes(STANDARD_SCOPES),
                        method: "bearer-access-token".to_owned(),
                        client: ClientMetadata::default(),
                        issued_at_ms: now,
                        expires_at_ms: now + SESSION_TTL_MS,
                        revoked_at_ms: None,
                        last_connected_at_ms: None,
                        connected_count: 0,
                        proof_key_thumbprint: None,
                        transport: SessionTransport::Plain,
                        reach: None,
                        off_host: None,
                        delivery_state: AuthSessionDeliveryState::Active,
                    },
                );
            }
        }
        assert!(matches!(
            service
                .exchange_bootstrap(
                    "desktop-test-seed",
                    None,
                    ClientMetadata::default(),
                    None,
                    SessionTransport::Plain,
                )
                .await,
            Err(AuthError::Internal(message)) if message == "active session capacity exceeded"
        ));
    }

    async fn insert_live_pairing(service: &AuthService, id: &str) {
        let now = now_ms();
        service.state.lock().await.pairings.insert(
            id.to_owned(),
            PairingRecord {
                id: id.to_owned(),
                credential: format!("credential-{id}"),
                scopes: owned_scopes(STANDARD_SCOPES),
                subject: "one-time-token".to_owned(),
                label: None,
                proof_key_thumbprint: None,
                created_at_ms: now,
                expires_at_ms: now + PAIRING_TTL_MS,
                consumed_at_ms: None,
                revoked_at_ms: None,
                reach: Some("another-device".to_owned()),
                off_host: Some(true),
            },
        );
    }

    #[tokio::test]
    async fn replaying_a_consumed_offer_returns_fresh_instead_of_a_dead_code() {
        let service = service();
        let offer = PairingOfferResult {
            id: "offer-consumed".to_owned(),
            code: "code-consumed".to_owned(),
            reach: "another-device".to_owned(),
            endpoint: "http://192.168.1.20:3773".to_owned(),
            name: "Tablet".to_owned(),
            expires_at: format_iso(now_ms() + PAIRING_TTL_MS),
        };
        insert_live_pairing(&service, &offer.id).await;
        service
            .record_pairing_offer(
                "principal",
                "request-key".to_owned(),
                "fingerprint".to_owned(),
                offer.clone(),
            )
            .await
            .expect("offer records");
        assert!(matches!(
            service
                .replay_pairing_offer("principal", "request-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Original(result)) if result.id == offer.id
        ));

        // A device consumes the link; the recorded result now advertises a
        // dead code and must not be replayed as fresh success.
        service.state.lock().await.pairings.remove(&offer.id);
        assert!(matches!(
            service
                .replay_pairing_offer("principal", "request-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Fresh)
        ));
    }

    #[tokio::test]
    async fn pairing_offer_idempotency_is_scoped_to_the_authenticated_principal() {
        let service = service();
        let first = PairingOfferResult {
            id: "offer-a".to_owned(),
            code: "code-a".to_owned(),
            reach: "another-device".to_owned(),
            endpoint: "http://192.168.1.20:3773".to_owned(),
            name: "Tablet A".to_owned(),
            expires_at: format_iso(now_ms() + PAIRING_TTL_MS),
        };
        insert_live_pairing(&service, &first.id).await;
        service
            .record_pairing_offer(
                "principal-a",
                "shared-key".to_owned(),
                "fingerprint-a".to_owned(),
                first.clone(),
            )
            .await
            .expect("first principal records offer");

        assert!(matches!(
            service
                .replay_pairing_offer("principal-a", "shared-key", "fingerprint-a")
                .await,
            Ok(PairingOfferReplay::Original(result)) if result.id == first.id
        ));
        assert!(matches!(
            service
                .replay_pairing_offer("principal-a", "shared-key", "different-input")
                .await,
            Ok(PairingOfferReplay::Conflict)
        ));
        assert!(matches!(
            service
                .replay_pairing_offer("principal-b", "shared-key", "fingerprint-a")
                .await,
            Ok(PairingOfferReplay::Fresh)
        ));

        let second = PairingOfferResult {
            id: "offer-b".to_owned(),
            code: "code-b".to_owned(),
            reach: "another-device".to_owned(),
            endpoint: "http://192.168.1.21:3773".to_owned(),
            name: "Tablet B".to_owned(),
            expires_at: format_iso(now_ms() + PAIRING_TTL_MS),
        };
        insert_live_pairing(&service, &second.id).await;
        service
            .record_pairing_offer(
                "principal-b",
                "shared-key".to_owned(),
                "fingerprint-b".to_owned(),
                second.clone(),
            )
            .await
            .expect("second principal records its own offer");
        assert!(matches!(
            service
                .replay_pairing_offer("principal-b", "shared-key", "fingerprint-b")
                .await,
            Ok(PairingOfferReplay::Original(result)) if result.id == second.id
        ));
    }

    #[tokio::test]
    async fn completed_pairing_offer_replays_and_cancels_after_restart() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database);
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let service = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("secret store"),
            repositories.clone(),
        )
        .await
        .expect("initial service");
        let issued = service
            .issue_share_pairing_offer(
                owned_scopes(STANDARD_SCOPES),
                Some("Tablet".to_owned()),
                "another-device".to_owned(),
                true,
                PairingOfferReservation::new(
                    "principal".to_owned(),
                    "request-key".to_owned(),
                    "fingerprint".to_owned(),
                ),
            )
            .await
            .expect("durable offer reservation");
        let PairingOfferIssuance::Reserved(issued) = issued else {
            panic!("fresh offer must reserve its grant");
        };
        let result = PairingOfferResult {
            id: issued.id.clone(),
            code: "encoded-offer".to_owned(),
            reach: "another-device".to_owned(),
            endpoint: "http://192.168.1.20:3773".to_owned(),
            name: "Tablet".to_owned(),
            expires_at: issued.expires_at,
        };
        service
            .record_pairing_offer(
                "principal",
                "request-key".to_owned(),
                "fingerprint".to_owned(),
                result.clone(),
            )
            .await
            .expect("offer completion");
        drop(service);

        let restarted = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("reopened secret store"),
            repositories.clone(),
        )
        .await
        .expect("restarted service hydrates offer ledger");
        assert!(matches!(
            restarted
                .replay_pairing_offer("principal", "request-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Original(replayed)) if replayed.id == result.id
        ));
        assert_eq!(
            restarted.share_exposure_state().await.desired_exposure,
            "wide"
        );
        assert!(
            restarted
                .cancel_pairing_offer("principal", "request-key".to_owned())
                .await
                .expect("restart-safe cancellation")
        );
        assert_eq!(
            restarted.share_exposure_state().await.desired_exposure,
            "loopback"
        );
        drop(restarted);

        let after_cancel = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("reopened secret store after cancellation"),
            repositories,
        )
        .await
        .expect("tombstone hydrates");
        assert!(matches!(
            after_cancel
                .replay_pairing_offer("principal", "request-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Cancelled)
        ));
        assert_eq!(
            after_cancel.share_exposure_state().await.desired_exposure,
            "loopback"
        );
    }

    #[tokio::test]
    async fn live_service_does_not_recover_another_services_young_pending_offer() {
        let repositories = IndependentAuthRepositories::new().await;
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let first = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("first secret store"),
            repositories.first.clone(),
        )
        .await
        .expect("first live service");
        let second = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("second secret store"),
            repositories.second.clone(),
        )
        .await
        .expect("second live service");

        let issued = first
            .issue_share_pairing_offer(
                owned_scopes(STANDARD_SCOPES),
                Some("Tablet".to_owned()),
                "another-device".to_owned(),
                true,
                PairingOfferReservation::new(
                    "principal".to_owned(),
                    "concurrent-key".to_owned(),
                    "fingerprint".to_owned(),
                ),
            )
            .await
            .expect("first service reserves the offer");
        let PairingOfferIssuance::Reserved(issued) = issued else {
            panic!("fresh offer must reserve its grant");
        };

        assert!(matches!(
            second
                .replay_pairing_offer("principal", "concurrent-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Cancelled)
        ));

        let result = PairingOfferResult {
            id: issued.id,
            code: "encoded-offer".to_owned(),
            reach: "another-device".to_owned(),
            endpoint: "http://192.168.1.20:3773".to_owned(),
            name: "Tablet".to_owned(),
            expires_at: issued.expires_at,
        };
        first
            .record_pairing_offer(
                "principal",
                "concurrent-key".to_owned(),
                "fingerprint".to_owned(),
                result.clone(),
            )
            .await
            .expect("first service completes the offer");
        assert!(matches!(
            second
                .replay_pairing_offer("principal", "concurrent-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Original(replayed)) if replayed.id == result.id
        ));

        drop((first, second));
        repositories.close().await;
    }

    #[tokio::test]
    async fn remote_offer_cancellation_converges_dormant_share_state_and_access_events() {
        let repositories = IndependentAuthRepositories::new().await;
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let first = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("first secret store"),
            repositories.first.clone(),
        )
        .await
        .expect("first live service");
        let second = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("second secret store"),
            repositories.second.clone(),
        )
        .await
        .expect("second live service");

        let issued = first
            .issue_share_pairing_offer(
                owned_scopes(STANDARD_SCOPES),
                Some("Tablet".to_owned()),
                "another-device".to_owned(),
                true,
                PairingOfferReservation::new(
                    "principal".to_owned(),
                    "dormant-key".to_owned(),
                    "fingerprint".to_owned(),
                ),
            )
            .await
            .expect("off-host reservation");
        let PairingOfferIssuance::Reserved(issued) = issued else {
            panic!("fresh offer must reserve its grant");
        };
        first
            .record_pairing_offer(
                "principal",
                "dormant-key".to_owned(),
                "fingerprint".to_owned(),
                PairingOfferResult {
                    id: issued.id.clone(),
                    code: "encoded-offer".to_owned(),
                    reach: "another-device".to_owned(),
                    endpoint: "http://192.168.1.20:3773".to_owned(),
                    name: "Tablet".to_owned(),
                    expires_at: issued.expires_at,
                },
            )
            .await
            .expect("offer completion");
        assert_eq!(first.state.lock().await.live_connections.len(), 0);
        assert_eq!(first.share_exposure_state().await.desired_exposure, "wide");
        let mut access_events = first.subscribe_access();

        assert!(
            second
                .cancel_pairing_offer("principal", "dormant-key".to_owned())
                .await
                .expect("remote cancellation")
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let event = access_events
                    .recv()
                    .await
                    .expect("access channel remains open");
                if matches!(
                    &event.change,
                    AuthAccessChange::PairingLinkRemoved { id } if id == &issued.id
                ) {
                    break event;
                }
            }
        })
        .await
        .expect("dormant grant removal must converge");
        assert!(event.revision > 1);
        assert_eq!(first.state.lock().await.live_connections.len(), 0);
        assert_eq!(
            first.share_exposure_state().await.desired_exposure,
            "loopback"
        );
        drop((first, second));
        repositories.close().await;
    }

    #[tokio::test]
    async fn access_subscriber_keeps_one_service_watcher_without_cached_authority() {
        let repositories = IndependentAuthRepositories::new().await;
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let first = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("first secret store"),
            repositories.first.clone(),
        )
        .await
        .expect("first live service");
        let second = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("second secret store"),
            repositories.second.clone(),
        )
        .await
        .expect("second live service");
        assert!(first.state.lock().await.pairings.is_empty());
        assert!(first.state.lock().await.sessions.is_empty());
        let mut access_events = first.subscribe_access();
        tokio::time::sleep(AUTHORITY_CONVERGENCE_INTERVAL * 2).await;

        let issued = second
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("Tablet".to_owned()))
            .await
            .expect("remote pairing");
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let event = access_events
                    .recv()
                    .await
                    .expect("access channel remains open");
                if matches!(
                    &event.change,
                    AuthAccessChange::PairingLinkUpserted(pairing) if pairing.id == issued.id
                ) {
                    break event;
                }
            }
        })
        .await
        .expect("subscriber-only authority watcher must converge");
        assert!(event.revision > 1);
        drop((access_events, first, second));
        repositories.close().await;
    }

    #[tokio::test]
    async fn cached_grant_keeps_one_service_watcher_without_socket_or_subscriber() {
        let repositories = IndependentAuthRepositories::new().await;
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let first = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("first secret store"),
            repositories.first.clone(),
        )
        .await
        .expect("first live service");
        let second = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("second secret store"),
            repositories.second.clone(),
        )
        .await
        .expect("second live service");
        let issued = first
            .issue_share_pairing_offer(
                owned_scopes(STANDARD_SCOPES),
                Some("Tablet".to_owned()),
                "another-device".to_owned(),
                true,
                PairingOfferReservation::new(
                    "principal".to_owned(),
                    "grant-only-key".to_owned(),
                    "fingerprint".to_owned(),
                ),
            )
            .await
            .expect("off-host reservation");
        assert!(matches!(issued, PairingOfferIssuance::Reserved(_)));
        assert_eq!(first.access_events.receiver_count(), 0);
        assert!(first.state.lock().await.live_connections.is_empty());
        tokio::time::sleep(AUTHORITY_CONVERGENCE_INTERVAL * 2).await;

        assert!(
            second
                .cancel_pairing_offer("principal", "grant-only-key".to_owned())
                .await
                .expect("remote cancellation")
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if first.share_exposure_state().await.desired_exposure == "loopback" {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cached grant watcher must converge without a socket or subscriber");
        drop((first, second));
        repositories.close().await;
    }

    #[tokio::test]
    async fn cached_session_keeps_one_service_watcher_without_live_connection() {
        let repositories = IndependentAuthRepositories::new().await;
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let first = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("first secret store"),
            repositories.first.clone(),
        )
        .await
        .expect("first live service");
        let second = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("second secret store"),
            repositories.second.clone(),
        )
        .await
        .expect("second live service");
        let pairing = first
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("Tablet".to_owned()))
            .await
            .expect("pairing grant");
        let issued = first
            .exchange_bootstrap(
                &pairing.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("cached session");
        assert!(first.state.lock().await.pairings.is_empty());
        assert!(first.state.lock().await.live_connections.is_empty());
        tokio::time::sleep(AUTHORITY_CONVERGENCE_INTERVAL * 2).await;

        assert!(
            second
                .revoke_client("other-session", &issued.principal.session_id)
                .await
                .expect("remote session revocation")
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if first.list_clients("other-session").await.is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cached session watcher must converge without a live connection");
        drop((first, second));
        repositories.close().await;
    }

    #[tokio::test]
    async fn cross_service_authentication_starts_watcher_for_the_cached_session() {
        let repositories = IndependentAuthRepositories::new().await;
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let first = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("first secret store"),
            repositories.first.clone(),
        )
        .await
        .expect("first live service");
        let second = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("second secret store"),
            repositories.second.clone(),
        )
        .await
        .expect("second live service");
        tokio::time::sleep(AUTHORITY_CONVERGENCE_INTERVAL * 2).await;
        let pairing = first
            .issue_pairing(owned_scopes(STANDARD_SCOPES), Some("Tablet".to_owned()))
            .await
            .expect("pairing grant");
        let issued = first
            .exchange_bootstrap(
                &pairing.credential,
                None,
                ClientMetadata::default(),
                None,
                SessionTransport::Plain,
            )
            .await
            .expect("session minted on first service");
        second
            .authenticate_token(&issued.token, SessionTransport::Plain)
            .await
            .expect("second service authenticates durable session");
        let connection_shutdown = CancellationToken::new();
        second
            .mark_connected(&issued.principal.session_id, connection_shutdown.clone())
            .await
            .expect("connection registers");
        tokio::time::sleep(AUTHORITY_CONVERGENCE_INTERVAL * 2).await;

        assert!(
            first
                .revoke_client("other-session", &issued.principal.session_id)
                .await
                .expect("remote session revocation")
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            connection_shutdown.cancelled().await;
        })
        .await
        .expect("authenticated peer session must retain revocation convergence");
        drop((first, second));
        repositories.close().await;
    }

    #[tokio::test]
    async fn unchanged_revision_reconciles_at_the_nearest_authority_expiry() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database);
        let secrets = tempfile::tempdir().expect("secret store directory");
        let secret_store = SecretStore::new(secrets.path())
            .await
            .expect("secret store");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let expiry_lead_ms = i64::try_from((AUTHORITY_CONVERGENCE_INTERVAL * 8).as_millis())
            .expect("authority expiry lead fits i64");
        let expires_at = now_ms().saturating_add(expiry_lead_ms);
        repositories
            .create_auth_pairing_link(PersistedPairingLink {
                id: "expiring-grant".to_owned(),
                credential: "expiring-credential".to_owned(),
                method: "one-time-token".to_owned(),
                scopes: serde_json::json!(owned_scopes(STANDARD_SCOPES)),
                subject: "one-time-token".to_owned(),
                label: Some("Expiring".to_owned()),
                proof_key_thumbprint: None,
                created_at: format_iso(now_ms()),
                expires_at: format_iso(expires_at),
                consumed_at: None,
                revoked_at: None,
                reach: Some("another-device".to_owned()),
                off_host: Some(true),
            })
            .await
            .expect("expiring authority row");
        let service = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            secret_store,
            repositories.clone(),
        )
        .await
        .expect("service hydrates expiring authority");
        assert_eq!(service.list_pairings().await.len(), 1);
        let mut access_events = service.subscribe_access();
        let revision = repositories
            .auth_authority_revision()
            .await
            .expect("authority revision");

        let event = tokio::time::timeout(AUTHORITY_CONVERGENCE_INTERVAL * 16, async {
            loop {
                let event = access_events
                    .recv()
                    .await
                    .expect("access channel remains open");
                if matches!(
                    &event.change,
                    AuthAccessChange::PairingLinkRemoved { id } if id == "expiring-grant"
                ) {
                    break event;
                }
            }
        })
        .await
        .expect("nearest expiry must reconcile without another mutation");
        assert!(event.revision > 1);
        assert_eq!(
            repositories
                .auth_authority_revision()
                .await
                .expect("unchanged authority revision"),
            revision
        );
    }

    #[tokio::test]
    async fn pending_pairing_offer_recovers_for_retry_after_restart() {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("in-memory database opens");
        database
            .call(|connection| {
                crate::persistence::run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("all migrations apply");
        let repositories = Repositories::new(database);
        let secrets = tempfile::tempdir().expect("secret store directory");
        let config = ServerConfig::new(".").with_bind("127.0.0.1", 3773);
        let service = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("secret store"),
            repositories.clone(),
        )
        .await
        .expect("initial service");
        let issued = service
            .issue_share_pairing_offer(
                owned_scopes(STANDARD_SCOPES),
                Some("Tablet".to_owned()),
                "another-device".to_owned(),
                true,
                PairingOfferReservation::new(
                    "principal".to_owned(),
                    "pending-key".to_owned(),
                    "fingerprint".to_owned(),
                ),
            )
            .await
            .expect("pairing and reservation commit");
        let PairingOfferIssuance::Reserved(issued) = issued else {
            panic!("fresh offer must reserve its grant");
        };
        let stale_created_at = "1970-01-01T00:00:00Z".to_owned();
        let pairing_id = issued.id;
        repositories
            .database()
            .call(move |connection| {
                let updated = connection.execute(
                    "UPDATE auth_pairing_links SET created_at = ? WHERE id = ?",
                    [stale_created_at, pairing_id],
                )?;
                assert_eq!(updated, 1);
                Ok(())
            })
            .await
            .expect("pending reservation is aged past the recovery grace");
        drop(service);

        let restarted = AuthService::new_with_persistence(
            &config,
            vec![7_u8; 32],
            SecretStore::new(secrets.path())
                .await
                .expect("reopened secret store"),
            repositories,
        )
        .await
        .expect("pending reservation hydrates");
        assert!(matches!(
            restarted
                .replay_pairing_offer("principal", "pending-key", "fingerprint")
                .await,
            Ok(PairingOfferReplay::Fresh)
        ));
        assert_eq!(
            restarted.share_exposure_state().await.desired_exposure,
            "loopback"
        );
        assert!(matches!(
            restarted
                .issue_share_pairing_offer(
                    owned_scopes(STANDARD_SCOPES),
                    Some("Tablet".to_owned()),
                    "another-device".to_owned(),
                    true,
                    PairingOfferReservation::new(
                        "principal".to_owned(),
                        "pending-key".to_owned(),
                        "fingerprint".to_owned(),
                    ),
                )
                .await,
            Ok(PairingOfferIssuance::Reserved(_))
        ));
    }

    #[tokio::test]
    async fn pairing_offer_idempotency_prunes_expired_entries_and_caps_memory() {
        let service = service();
        let now = now_ms();
        {
            let mut state = service.state.lock().await;
            for index in 0..MAX_ACTIVE_PAIRINGS {
                let id = format!("offer-{index}");
                state.pairing_offer_idempotency.insert(
                    (format!("principal-{index}"), format!("key-{index}")),
                    StoredPairingOffer {
                        input_fingerprint: format!("fingerprint-{index}"),
                        pairing_id: Some(id.clone()),
                        result: Some(PairingOfferResult {
                            id,
                            code: format!("code-{index}"),
                            reach: "another-device".to_owned(),
                            endpoint: "http://192.168.1.20:3773".to_owned(),
                            name: format!("Client {index}"),
                            expires_at: format_iso(now + PAIRING_TTL_MS),
                        }),
                        expires_at_ms: now + PAIRING_TTL_MS,
                    },
                );
            }
        }

        let overflow = PairingOfferResult {
            id: "overflow".to_owned(),
            code: "overflow-code".to_owned(),
            reach: "another-device".to_owned(),
            endpoint: "http://192.168.1.21:3773".to_owned(),
            name: "Overflow client".to_owned(),
            expires_at: format_iso(now + PAIRING_TTL_MS),
        };
        assert!(matches!(
            service
                .record_pairing_offer(
                    "overflow-principal",
                    "overflow-key".to_owned(),
                    "overflow-fingerprint".to_owned(),
                    overflow.clone(),
                )
                .await,
            Err(AuthError::Internal(message))
                if message == "pairing offer idempotency capacity exceeded"
        ));

        service
            .state
            .lock()
            .await
            .pairing_offer_idempotency
            .get_mut(&("principal-0".to_owned(), "key-0".to_owned()))
            .expect("idempotency fixture")
            .expires_at_ms = now - 1;
        service
            .record_pairing_offer(
                "overflow-principal",
                "overflow-key".to_owned(),
                "overflow-fingerprint".to_owned(),
                overflow,
            )
            .await
            .expect("expired idempotency entry frees capacity");
        assert_eq!(
            service.state.lock().await.pairing_offer_idempotency.len(),
            MAX_ACTIVE_PAIRINGS
        );
    }

    #[tokio::test]
    async fn pairing_offer_tombstones_are_bounded_per_principal() {
        let service = service();
        for index in 0..MAX_ACTIVE_PAIRING_OFFERS_PER_PRINCIPAL {
            service
                .cancel_pairing_offer("principal-a", format!("cancelled-{index}"))
                .await
                .expect("principal tombstone within quota");
        }

        assert!(matches!(
            service
                .cancel_pairing_offer("principal-a", "cancelled-overflow".to_owned())
                .await,
            Err(AuthError::Internal(message))
                if message == "pairing offer principal capacity exceeded"
        ));
        service
            .cancel_pairing_offer("principal-b", "cancelled-elsewhere".to_owned())
            .await
            .expect("one principal cannot consume another principal's quota");
    }
}
