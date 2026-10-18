use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

#[cfg(test)]
use std::ops::AsyncFnMut;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{SinkExt, StreamExt, stream};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use super::{
    RpcRegistry, RpcSessionContext,
    byte_budget::{
        RpcOutboundBudget, RpcOutboundProcessBudget, WeightedByteAcquireError, WeightedByteBudget,
        WeightedByteGrant,
    },
    session::{
        PUMP_JOIN_TIMEOUT, PairingConfirmationLatch, RpcInboundFrame, SOCKET_WRITE_TIMEOUT,
        run_session_split_budgeted,
    },
    transport::{
        ConnectionLiveness, E2EE_DEFLATE_FEATURE, E2EE_INTERLEAVE_FEATURE, OutboundFraming,
        observe_inbound,
    },
};
use crate::{
    auth::{
        AuthService, AuthenticatedConnectionGuard, HostIdentity, NOISE_NK_PARAMS, Principal,
        SessionTransport,
    },
    config::ServerConfig,
    http::spawn_session_expiration_guard,
    preview::gateway::registry::ClientReach,
};

pub(crate) const MAX_E2EE_CIPHERTEXT_BYTES: usize = 65_535;
const NOISE_TAG_BYTES: usize = 16;
pub(crate) const E2EE_RECORD_FLAG_FINAL: u8 = 0x00;
pub(crate) const E2EE_RECORD_FLAG_CONTINUATION: u8 = 0x01;
pub(crate) const MAX_E2EE_CHUNK_BYTES: usize = MAX_E2EE_CIPHERTEXT_BYTES - NOISE_TAG_BYTES - 1;
pub(crate) const MAX_E2EE_LOGICAL_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_E2EE_PREAUTH_MESSAGE_BYTES: usize = 64 * 1024;
pub(crate) const MAX_E2EE_RECORDS_PER_MESSAGE: usize = 2_048;
const E2EE_INCOMPLETE_MESSAGE_PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);
const E2EE_LOGICAL_WRITE_BYTES_PER_SECOND: usize = 64 * 1024;
/// One deadline covering upgrade -> handshake -> e2ee_authenticated.
pub(crate) const E2EE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Wrong pinned key: the responder cannot decrypt Message 1 and closes with this code.
pub(crate) const E2EE_HOST_IDENTITY_CLOSE_CODE: u16 = 4403;
pub(crate) const E2EE_MAX_PREAUTH_CONNECTIONS: usize = 32;
pub(crate) const E2EE_MAX_PREAUTH_CONNECTIONS_PER_PEER: usize = 4;
/// Peer/network buckets that already hold a connection may only grow while
/// global pre-auth usage stays at or below this soft cap. The slots above it
/// are reserved for a bucket's first connection, so a fresh legitimate device
/// still finds room while a small number of hostile networks saturate their
/// own buckets. (A wide-prefix holder can still take the reserve with many
/// distinct first-connections; per-address admission cannot price that out.)
const E2EE_PREAUTH_GLOBAL_SOFT_CAP: usize = 24;
const E2EE_PREAUTH_BURST_PER_PEER: u8 = 8;
/// The loopback-forwarder class aggregates every local client that a reverse
/// proxy fronts without appending `X-Forwarded-For`, so its bucket is sized
/// for fleet reconnect storms rather than for one device.
const E2EE_PREAUTH_BURST_FORWARDER: u8 = 32;
const E2EE_PREAUTH_FORWARDER_REFILL_PER_INTERVAL: u8 = 8;
const E2EE_PREAUTH_REFILL_INTERVAL: Duration = Duration::from_secs(1);
const E2EE_PREAUTH_PEER_STATE_TTL: Duration = Duration::from_secs(8);
const E2EE_MAX_PREAUTH_CONNECTIONS_PER_NETWORK: usize = 16;
const E2EE_MAX_PREAUTH_TRACKED_PEERS: usize = 1_024;
const E2EE_MAX_PREAUTH_TRACKED_NETWORKS: usize = 1_024;
pub(crate) const E2EE_MAX_ESTABLISHED_CONNECTIONS: usize = 64;
pub(crate) const E2EE_MAX_ESTABLISHED_CONNECTIONS_PER_PRINCIPAL: usize = 32;
pub(crate) const E2EE_INBOUND_BUFFER_BUDGET_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const E2EE_INBOUND_BUFFER_BUDGET_BYTES_PER_PRINCIPAL: usize = 64 * 1024 * 1024;
pub(crate) const E2EE_OUTBOUND_BUFFER_BUDGET_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const E2EE_OUTBOUND_BUFFER_BUDGET_BYTES_PER_CONNECTION: usize = 64 * 1024 * 1024;

pub(super) struct PlaintextRecords<'a> {
    plaintext: &'a [u8],
    offset: usize,
    emitted_empty: bool,
}

impl<'a> Iterator for PlaintextRecords<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.plaintext.is_empty() {
            if self.emitted_empty {
                return None;
            }
            self.emitted_empty = true;
            return Some((E2EE_RECORD_FLAG_FINAL, &[]));
        }
        if self.offset >= self.plaintext.len() {
            return None;
        }
        let end = self
            .offset
            .saturating_add(MAX_E2EE_CHUNK_BYTES)
            .min(self.plaintext.len());
        let flag = if end == self.plaintext.len() {
            E2EE_RECORD_FLAG_FINAL
        } else {
            E2EE_RECORD_FLAG_CONTINUATION
        };
        let chunk = &self.plaintext[self.offset..end];
        self.offset = end;
        Some((flag, chunk))
    }
}

pub(super) fn plaintext_records(
    plaintext: &[u8],
) -> Result<PlaintextRecords<'_>, E2eeSessionError> {
    if plaintext.len() > MAX_E2EE_LOGICAL_MESSAGE_BYTES {
        return Err(E2eeSessionError::Protocol(
            "outbound message too large".into(),
        ));
    }
    Ok(PlaintextRecords {
        plaintext,
        offset: 0,
        emitted_empty: false,
    })
}

static E2EE_RESOURCE_BUDGET: LazyLock<E2eeResourceBudget> = LazyLock::new(|| {
    E2eeResourceBudget::new(
        E2EE_MAX_ESTABLISHED_CONNECTIONS,
        E2EE_MAX_ESTABLISHED_CONNECTIONS_PER_PRINCIPAL,
        E2EE_INBOUND_BUFFER_BUDGET_BYTES,
        E2EE_INBOUND_BUFFER_BUDGET_BYTES_PER_PRINCIPAL,
        E2EE_OUTBOUND_BUFFER_BUDGET_BYTES,
    )
});

#[derive(Clone)]
pub(crate) struct E2eePreauthAdmission {
    inner: Arc<E2eePreauthAdmissionInner>,
}

struct E2eePreauthAdmissionInner {
    global: Arc<Semaphore>,
    state: Mutex<E2eePreauthState>,
}

#[derive(Default)]
struct E2eePreauthState {
    peers: HashMap<E2eePreauthPeerKey, E2eePreauthPeerState>,
    networks: HashMap<E2eePreauthNetworkKey, E2eePreauthNetworkState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum E2eePreauthPeerKey {
    Public(IpAddr),
    LoopbackForwarder,
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum E2eePreauthNetworkKey {
    V4(u32),
    V6(u128),
}

struct E2eePreauthPeerState {
    active: usize,
    tokens: u8,
    last_refill: tokio::time::Instant,
    last_activity: tokio::time::Instant,
}

struct E2eePreauthNetworkState {
    active: usize,
    last_activity: tokio::time::Instant,
}

/// A pre-auth admission slot; released on drop.
pub(crate) struct E2eePreauthLease {
    inner: Arc<E2eePreauthAdmissionInner>,
    peer_key: E2eePreauthPeerKey,
    network_key: Option<E2eePreauthNetworkKey>,
    _global: OwnedSemaphorePermit,
}

impl E2eePreauthAdmission {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(E2eePreauthAdmissionInner {
                global: Arc::new(Semaphore::new(E2EE_MAX_PREAUTH_CONNECTIONS)),
                state: Mutex::new(E2eePreauthState::default()),
            }),
        }
    }

    /// Global pre-auth slots no connection holds; only the unix media-socket tests ask.
    #[cfg(all(test, unix))]
    pub(crate) fn available_permits(&self) -> usize {
        self.inner.global.available_permits()
    }

    /// Admits one pre-auth connection from `peer_ip` against the global, per-peer, and
    /// per-network budgets; the error is the refusal reason (`busy` or `rate`).
    pub(crate) fn try_admit(
        &self,
        peer_ip: IpAddr,
        now: tokio::time::Instant,
    ) -> Result<E2eePreauthLease, &'static str> {
        let global = Arc::clone(&self.inner.global)
            .try_acquire_owned()
            .map_err(|_| "busy")?;
        let (peer_key, network_key) = classify_preauth_peer(peer_ip);
        let mut admission = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        admission.peers.retain(|_, state| {
            state.active > 0
                || now.saturating_duration_since(state.last_activity) < E2EE_PREAUTH_PEER_STATE_TTL
        });
        admission.networks.retain(|_, state| {
            state.active > 0
                || now.saturating_duration_since(state.last_activity) < E2EE_PREAUTH_PEER_STATE_TTL
        });
        let (burst, refill_per_interval) = match peer_key {
            E2eePreauthPeerKey::LoopbackForwarder => (
                E2EE_PREAUTH_BURST_FORWARDER,
                E2EE_PREAUTH_FORWARDER_REFILL_PER_INTERVAL,
            ),
            E2eePreauthPeerKey::Public(_) | E2eePreauthPeerKey::Unspecified => {
                (E2EE_PREAUTH_BURST_PER_PEER, 1)
            }
        };
        if !admission.peers.contains_key(&peer_key) {
            if admission.peers.len() >= E2EE_MAX_PREAUTH_TRACKED_PEERS {
                return Err("busy");
            }
            admission.peers.insert(
                peer_key,
                E2eePreauthPeerState {
                    active: 0,
                    tokens: burst,
                    last_refill: now,
                    last_activity: now,
                },
            );
        }
        if let Some(network_key) = network_key
            && !admission.networks.contains_key(&network_key)
        {
            if admission.networks.len() >= E2EE_MAX_PREAUTH_TRACKED_NETWORKS {
                return Err("busy");
            }
            admission.networks.insert(
                network_key,
                E2eePreauthNetworkState {
                    active: 0,
                    last_activity: now,
                },
            );
        }

        let peer_state = admission
            .peers
            .get_mut(&peer_key)
            .expect("pre-auth peer inserted before admission");
        let elapsed_intervals = now
            .saturating_duration_since(peer_state.last_refill)
            .as_secs()
            / E2EE_PREAUTH_REFILL_INTERVAL.as_secs();
        if elapsed_intervals > 0 {
            let replenished = u8::try_from(elapsed_intervals)
                .unwrap_or(u8::MAX)
                .saturating_mul(refill_per_interval);
            peer_state.tokens = peer_state.tokens.saturating_add(replenished).min(burst);
            peer_state.last_refill += E2EE_PREAUTH_REFILL_INTERVAL
                .saturating_mul(u32::try_from(elapsed_intervals).unwrap_or(u32::MAX));
        }
        peer_state.last_activity = now;
        if !matches!(peer_key, E2eePreauthPeerKey::LoopbackForwarder)
            && peer_state.active >= E2EE_MAX_PREAUTH_CONNECTIONS_PER_PEER
        {
            return Err("busy");
        }
        if peer_state.tokens == 0 {
            return Err("rate");
        }
        if network_key.is_some_and(|network_key| {
            admission
                .networks
                .get(&network_key)
                .is_some_and(|state| state.active >= E2EE_MAX_PREAUTH_CONNECTIONS_PER_NETWORK)
        }) {
            return Err("busy");
        }
        // Established buckets only grow while global usage stays within the
        // soft cap; the remaining slots are a bucket's first-connection
        // reserve. `global` is already held, so available permits reflect this
        // admission.
        let bucket_established = match network_key {
            Some(network_key) => admission
                .networks
                .get(&network_key)
                .is_some_and(|state| state.active > 0),
            None => admission
                .peers
                .get(&peer_key)
                .is_some_and(|state| state.active > 0),
        };
        if bucket_established {
            let used_including_this =
                E2EE_MAX_PREAUTH_CONNECTIONS - self.inner.global.available_permits();
            if used_including_this > E2EE_PREAUTH_GLOBAL_SOFT_CAP {
                return Err("busy");
            }
        }

        let peer_state = admission
            .peers
            .get_mut(&peer_key)
            .expect("pre-auth peer remains present");
        peer_state.tokens -= 1;
        peer_state.active += 1;
        if let Some(network_key) = network_key {
            let network_state = admission
                .networks
                .get_mut(&network_key)
                .expect("pre-auth network inserted before admission");
            network_state.active += 1;
            network_state.last_activity = now;
        }
        drop(admission);
        Ok(E2eePreauthLease {
            inner: Arc::clone(&self.inner),
            peer_key,
            network_key,
            _global: global,
        })
    }

    #[cfg(test)]
    fn peer_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .peers
            .len()
    }

    #[cfg(test)]
    fn network_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .networks
            .len()
    }
}

impl Drop for E2eePreauthLease {
    fn drop(&mut self) {
        let now = tokio::time::Instant::now();
        let mut admission = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(state) = admission.peers.get_mut(&self.peer_key) {
            state.active = state.active.saturating_sub(1);
            state.last_activity = now;
        }
        if let Some(network_key) = self.network_key
            && let Some(state) = admission.networks.get_mut(&network_key)
        {
            state.active = state.active.saturating_sub(1);
            state.last_activity = now;
        }
    }
}

/// Resolves the admission identity for a pre-auth connection. A loopback
/// socket is a local reverse proxy or a direct local client: when it carries
/// `X-Forwarded-For`, the **last** entry — the hop appended by that implicitly
/// trusted local proxy — is the real client and keys per-peer/per-network
/// admission; earlier entries are client-controlled and ignored. Remote
/// sockets can never reach this branch, and an unparseable value falls into
/// the strict unspecified bucket rather than the exempt forwarder class.
pub(crate) fn effective_preauth_peer(socket_peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
    let socket_peer = unmap_ipv4(socket_peer);
    if !socket_peer.is_loopback() {
        return socket_peer;
    }
    let Some(last_hop) = forwarded_for
        .and_then(|header| header.split(',').next_back())
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    else {
        return socket_peer;
    };
    last_hop
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .map(unmap_ipv4)
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

fn unmap_ipv4(peer_ip: IpAddr) -> IpAddr {
    match peer_ip {
        IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some() => {
            IpAddr::V4(ip.to_ipv4_mapped().expect("mapped IPv4 checked"))
        }
        peer_ip => peer_ip,
    }
}

fn classify_preauth_peer(peer_ip: IpAddr) -> (E2eePreauthPeerKey, Option<E2eePreauthNetworkKey>) {
    let peer_ip = unmap_ipv4(peer_ip);
    if peer_ip.is_unspecified() {
        return (E2eePreauthPeerKey::Unspecified, None);
    }
    if peer_ip.is_loopback() {
        return (E2eePreauthPeerKey::LoopbackForwarder, None);
    }
    let network = match peer_ip {
        IpAddr::V4(ip) => {
            E2eePreauthNetworkKey::V4(u32::from(ip) & u32::from_be_bytes([255, 255, 255, 0]))
        }
        IpAddr::V6(ip) => E2eePreauthNetworkKey::V6(u128::from(ip) & (u128::MAX << 64)),
    };
    (E2eePreauthPeerKey::Public(peer_ip), Some(network))
}

struct E2eeResourceBudget {
    global_established: Arc<Semaphore>,
    per_principal_established: usize,
    global_inbound: Arc<WeightedByteBudget>,
    per_principal_inbound: usize,
    global_outbound: RpcOutboundProcessBudget,
    principals: Mutex<HashMap<String, std::sync::Weak<PrincipalResourceBudget>>>,
}

struct PrincipalResourceBudget {
    established: Arc<Semaphore>,
    inbound: Arc<WeightedByteBudget>,
}

impl E2eeResourceBudget {
    fn new(
        global_established: usize,
        per_principal_established: usize,
        global_inbound: usize,
        per_principal_inbound: usize,
        global_outbound: usize,
    ) -> Self {
        Self {
            global_established: Arc::new(Semaphore::new(global_established)),
            per_principal_established,
            global_inbound: Arc::new(WeightedByteBudget::new(global_inbound)),
            per_principal_inbound,
            global_outbound: RpcOutboundProcessBudget::new(global_outbound),
            principals: Mutex::new(HashMap::new()),
        }
    }

    fn try_reserve(&self) -> Result<E2eeEstablishedReservation<'_>, &'static str> {
        let global = Arc::clone(&self.global_established)
            .try_acquire_owned()
            .map_err(|_| "protocol")?;
        Ok(E2eeEstablishedReservation {
            budget: self,
            global,
        })
    }

    fn principal_budget(&self, principal_id: &str) -> Arc<PrincipalResourceBudget> {
        let mut principals = self
            .principals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        principals.retain(|_, budget| budget.strong_count() > 0);
        if let Some(budget) = principals
            .get(principal_id)
            .and_then(std::sync::Weak::upgrade)
        {
            return budget;
        }
        let budget = Arc::new(PrincipalResourceBudget {
            established: Arc::new(Semaphore::new(self.per_principal_established)),
            inbound: Arc::new(WeightedByteBudget::new(self.per_principal_inbound)),
        });
        principals.insert(principal_id.to_owned(), Arc::downgrade(&budget));
        budget
    }

    fn global_inbound(&self) -> Arc<WeightedByteBudget> {
        Arc::clone(&self.global_inbound)
    }

    fn global_outbound(&self) -> RpcOutboundProcessBudget {
        self.global_outbound.clone()
    }
}

struct E2eeEstablishedReservation<'a> {
    budget: &'a E2eeResourceBudget,
    global: OwnedSemaphorePermit,
}

impl E2eeEstablishedReservation<'_> {
    fn bind_principal(self, principal_id: &str) -> Result<E2eeEstablishedPermit, &'static str> {
        let principal_budget = self.budget.principal_budget(principal_id);
        let principal = Arc::clone(&principal_budget.established)
            .try_acquire_owned()
            .map_err(|_| "protocol")?;
        Ok(E2eeEstablishedPermit {
            _global: self.global,
            _principal: Some(principal),
            principal_budget: Some(principal_budget),
        })
    }

    fn unpartitioned(self) -> E2eeEstablishedPermit {
        E2eeEstablishedPermit {
            _global: self.global,
            _principal: None,
            principal_budget: None,
        }
    }
}

struct E2eeEstablishedPermit {
    _global: OwnedSemaphorePermit,
    _principal: Option<OwnedSemaphorePermit>,
    principal_budget: Option<Arc<PrincipalResourceBudget>>,
}

impl E2eeEstablishedPermit {
    fn principal_inbound(&self) -> Option<Arc<WeightedByteBudget>> {
        self.principal_budget
            .as_ref()
            .map(|budget| Arc::clone(&budget.inbound))
    }
}

struct BudgetedPlaintext {
    plaintext: Vec<u8>,
    permits: Option<E2eeRecordPermit>,
}

struct E2eeInboundBudget {
    _permits: Option<E2eeRecordPermit>,
}

struct E2eeRecordPermit {
    global: WeightedByteGrant,
    principal: Option<WeightedByteGrant>,
    connection: WeightedByteGrant,
}

impl E2eeRecordPermit {
    fn merge(&mut self, other: Self) {
        self.global
            .merge(other.global)
            .expect("record global grants share one budget");
        match (&mut self.principal, other.principal) {
            (Some(principal), Some(other)) => principal
                .merge(other)
                .expect("record principal grants share one budget"),
            (None, Some(other)) => self.principal = Some(other),
            (_, None) => {}
        }
        self.connection
            .merge(other.connection)
            .expect("record connection grants share one budget");
    }
}

#[derive(Debug, Error)]
pub(crate) enum E2eeSessionError {
    #[error("Noise handshake rejected")]
    Handshake,
    #[error("E2EE protocol violation: {0}")]
    Protocol(String),
    #[error("E2EE cryptographic operation failed: {0}")]
    Crypto(String),
    #[error("E2EE transport closed")]
    Closed,
    #[error("E2EE transport operation timed out")]
    Timeout,
}

pub(crate) struct E2eeChannel {
    transport: snow::TransportState,
    assembling: Vec<u8>,
    assembling_permits: Option<E2eeRecordPermit>,
    assembling_records: usize,
    decrypt_scratch: Vec<u8>,
}

struct DecryptedRecord {
    final_record: bool,
    chunk: Vec<u8>,
}

impl E2eeChannel {
    fn has_incomplete_message(&self) -> bool {
        self.assembling_records > 0
    }

    #[cfg(test)]
    pub(crate) async fn respond<Rx, Tx>(
        host_identity: &HostIdentity,
        mut recv_binary_frame: Rx,
        mut send_binary_frame: Tx,
    ) -> Result<Self, E2eeSessionError>
    where
        Rx: AsyncFnMut() -> Option<Vec<u8>>,
        Tx: AsyncFnMut(Vec<u8>) -> Result<(), E2eeSessionError>,
    {
        let message_a = recv_binary_frame().await.ok_or(E2eeSessionError::Closed)?;
        let (channel, message_b) = Self::respond_to_message_a(host_identity, &[], &message_a)?;
        send_binary_frame(message_b).await?;
        Ok(channel)
    }

    /// Answers the client's Noise_NK message A under `prologue`: the channel and message B
    /// to send back. `Handshake` means the client pinned another host key or prologue.
    fn respond_to_message_a(
        host_identity: &HostIdentity,
        prologue: &[u8],
        message_a: &[u8],
    ) -> Result<(Self, Vec<u8>), E2eeSessionError> {
        if message_a.len() > MAX_E2EE_CIPHERTEXT_BYTES {
            return Err(E2eeSessionError::Protocol(
                "oversized handshake frame".into(),
            ));
        }
        let params = NOISE_NK_PARAMS
            .parse()
            .map_err(|error| E2eeSessionError::Protocol(format!("noise params: {error:?}")))?;
        let mut responder = snow::Builder::new(params)
            .local_private_key(host_identity.private_key_bytes())
            .map_err(|error| E2eeSessionError::Protocol(format!("local key: {error:?}")))?
            .prologue(prologue)
            .map_err(|error| E2eeSessionError::Protocol(format!("prologue: {error:?}")))?
            .build_responder()
            .map_err(|error| E2eeSessionError::Protocol(format!("responder: {error:?}")))?;

        let mut payload = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let payload_len = responder
            .read_message(message_a, &mut payload)
            .map_err(|_| E2eeSessionError::Handshake)?;
        if payload_len != 0 {
            return Err(E2eeSessionError::Protocol(
                "message 1 carried a non-empty handshake payload".into(),
            ));
        }

        let mut message_b = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = responder
            .write_message(&[], &mut message_b)
            .map_err(|error| E2eeSessionError::Crypto(format!("message B: {error:?}")))?;
        message_b.truncate(len);

        let transport = responder
            .into_transport_mode()
            .map_err(|error| E2eeSessionError::Crypto(format!("transport: {error:?}")))?;
        Ok((
            Self {
                transport,
                assembling: Vec::new(),
                assembling_permits: None,
                assembling_records: 0,
                decrypt_scratch: vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES],
            },
            message_b,
        ))
    }

    /// Encrypts one logical message as records of at most [`MAX_E2EE_CHUNK_BYTES`], every
    /// one but the last flagged as a continuation.
    pub(crate) fn encrypt_message(
        &mut self,
        plaintext: &[u8],
    ) -> Result<Vec<Vec<u8>>, E2eeSessionError> {
        plaintext_records(plaintext)?
            .map(|(flag, chunk)| self.encrypt_record(flag, chunk))
            .collect()
    }

    pub(super) fn encrypt_record(
        &mut self,
        flag: u8,
        chunk: &[u8],
    ) -> Result<Vec<u8>, E2eeSessionError> {
        let mut record = Vec::with_capacity(1 + chunk.len());
        record.push(flag);
        record.extend_from_slice(chunk);
        let mut frame = vec![0_u8; record.len() + NOISE_TAG_BYTES];
        let len = self
            .transport
            .write_message(&record, &mut frame)
            .map_err(|error| E2eeSessionError::Crypto(format!("encrypt: {error:?}")))?;
        frame.truncate(len);
        Ok(frame)
    }

    pub(crate) fn decrypt_frame(
        &mut self,
        frame: &[u8],
        max_message_bytes: usize,
    ) -> Result<Option<Vec<u8>>, E2eeSessionError> {
        let record = self.decrypt_record(frame)?;
        self.assemble_decrypted_record(record, max_message_bytes, None)
            .map(|message| message.map(|message| message.plaintext))
    }

    fn decrypt_record(&mut self, frame: &[u8]) -> Result<DecryptedRecord, E2eeSessionError> {
        if frame.len() > MAX_E2EE_CIPHERTEXT_BYTES {
            return Err(E2eeSessionError::Protocol("oversized frame".into()));
        }
        let len = self
            .transport
            .read_message(frame, &mut self.decrypt_scratch)
            .map_err(|error| E2eeSessionError::Crypto(format!("decrypt: {error:?}")))?;
        if len == 0 {
            return Err(E2eeSessionError::Protocol("empty record".into()));
        }
        let flag = self.decrypt_scratch[0];
        let final_record = match flag {
            E2EE_RECORD_FLAG_FINAL => true,
            E2EE_RECORD_FLAG_CONTINUATION => false,
            other => {
                return Err(E2eeSessionError::Protocol(format!(
                    "unknown record flag {other}"
                )));
            }
        };
        let chunk = self.decrypt_scratch[1..len].to_vec();
        if !final_record && chunk.is_empty() {
            return Err(E2eeSessionError::Protocol(
                "empty continuation record".into(),
            ));
        }
        Ok(DecryptedRecord {
            final_record,
            chunk,
        })
    }

    fn validate_decrypted_record(
        &self,
        record: &DecryptedRecord,
        max_message_bytes: usize,
    ) -> Result<(), E2eeSessionError> {
        if self.assembling_records >= MAX_E2EE_RECORDS_PER_MESSAGE {
            return Err(E2eeSessionError::Protocol("record count overflow".into()));
        }
        if self.assembling.len().saturating_add(record.chunk.len()) > max_message_bytes {
            return Err(E2eeSessionError::Protocol("reassembly overflow".into()));
        }
        Ok(())
    }

    fn assemble_decrypted_record(
        &mut self,
        record: DecryptedRecord,
        max_message_bytes: usize,
        permit: Option<E2eeRecordPermit>,
    ) -> Result<Option<BudgetedPlaintext>, E2eeSessionError> {
        self.validate_decrypted_record(&record, max_message_bytes)?;
        self.assembling_records += 1;
        if !record.final_record {
            self.assembling.extend_from_slice(&record.chunk);
            if let Some(permit) = permit {
                if let Some(assembling) = &mut self.assembling_permits {
                    assembling.merge(permit);
                } else {
                    self.assembling_permits = Some(permit);
                }
            }
            return Ok(None);
        }

        let mut message = std::mem::take(&mut self.assembling);
        message.extend_from_slice(&record.chunk);
        self.assembling_records = 0;
        let mut permits = self.assembling_permits.take();
        if let Some(permit) = permit {
            if let Some(assembling) = &mut permits {
                assembling.merge(permit);
            } else {
                permits = Some(permit);
            }
        }
        Ok(Some(BudgetedPlaintext {
            plaintext: message,
            permits,
        }))
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct E2eeAuthMessage {
    pub r#type: String,
    #[serde(default)]
    pub pairing: Option<String>,
    #[serde(default)]
    pub bearer: Option<String>,
    /// Channel features the client supports; older clients send none.
    #[serde(default)]
    pub features: Vec<String>,
}

/// The channel features both sides agreed on in the auth exchange.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct E2eeFeatures {
    pub(crate) interleave: bool,
    pub(crate) deflate: bool,
}

impl E2eeFeatures {
    /// What this server confirms of a client's offer.
    fn agreed(offered: &[String], compression_enabled: bool) -> Self {
        let offers = |name: &str| offered.iter().any(|feature| feature == name);
        Self {
            interleave: offers(E2EE_INTERLEAVE_FEATURE),
            deflate: offers(E2EE_DEFLATE_FEATURE) && compression_enabled,
        }
    }
}

/// `BIBCODE_REMOTE_COMPRESSION=0` keeps every message uncompressed.
fn remote_compression_enabled() -> bool {
    compression_allowed(std::env::var_os("BIBCODE_REMOTE_COMPRESSION").as_deref())
}

fn compression_allowed(setting: Option<&std::ffi::OsStr>) -> bool {
    setting.is_none_or(|value| value != "0")
}

fn confirm_features(reply: &mut serde_json::Value, features: E2eeFeatures) {
    let mut names = Vec::new();
    if features.interleave {
        names.push(E2EE_INTERLEAVE_FEATURE);
    }
    if features.deflate {
        names.push(E2EE_DEFLATE_FEATURE);
    }
    if !names.is_empty() {
        reply
            .as_object_mut()
            .expect("static authenticated reply is an object")
            .insert("features".to_owned(), json!(names));
    }
}

pub(crate) fn e2ee_authenticated_json(features: E2eeFeatures) -> Vec<u8> {
    let mut reply = json!({ "type": "e2ee_authenticated" });
    confirm_features(&mut reply, features);
    serde_json::to_vec(&reply).expect("static JSON")
}

pub(crate) fn e2ee_authenticated_with_credential_json(
    credential: &str,
    environment_id: &str,
    storage_instance_id: Option<&str>,
    pairing_confirmation_required: bool,
    features: E2eeFeatures,
) -> Vec<u8> {
    let mut reply = json!({
        "type": "e2ee_authenticated",
        "credential": credential,
        "environmentId": environment_id,
    });
    if let Some(storage_instance_id) = storage_instance_id {
        reply
            .as_object_mut()
            .expect("static authenticated reply is an object")
            .insert(
                "storageInstanceId".to_owned(),
                serde_json::Value::String(storage_instance_id.to_owned()),
            );
    }
    if pairing_confirmation_required {
        reply
            .as_object_mut()
            .expect("static authenticated reply is an object")
            .insert(
                "pairingConfirmationRequired".to_owned(),
                serde_json::Value::Bool(true),
            );
    }
    confirm_features(&mut reply, features);
    serde_json::to_vec(&reply).expect("static JSON")
}

pub(crate) fn e2ee_error_json(code: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "type": "e2ee_error", "code": code })).expect("static JSON")
}

pub(crate) enum E2eeAccept {
    Authenticated {
        principal: Principal,
        minted: Option<MintedE2eeSession>,
    },
    Unauthenticated,
}

pub(crate) struct MintedE2eeSession {
    pub credential: String,
    delivery_guard: Option<Box<MintedSessionDeliveryGuard>>,
}

struct MintedSessionDeliveryGuard {
    auth: Option<AuthService>,
    session_id: Option<String>,
    confirmation: PairingConfirmationLatch,
}

impl MintedSessionDeliveryGuard {
    fn new(auth: AuthService, session_id: String) -> Self {
        Self {
            auth: Some(auth),
            session_id: Some(session_id),
            confirmation: PairingConfirmationLatch::default(),
        }
    }

    fn confirmation_latch(&self) -> PairingConfirmationLatch {
        self.confirmation.clone()
    }
}

impl Drop for MintedSessionDeliveryGuard {
    fn drop(&mut self) {
        let (Some(auth), Some(session_id)) = (self.auth.take(), self.session_id.take()) else {
            return;
        };
        if self.confirmation.is_confirmed() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::error!(
                session_id,
                "unable to schedule compensation for an unconfirmed E2EE credential"
            );
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = auth.revoke_failed_pairing_session(&session_id).await {
                tracing::error!(
                    session_id,
                    ?error,
                    "failed to compensate an unconfirmed E2EE credential"
                );
            }
        });
    }
}

struct EstablishedE2eeAdmission {
    accept: E2eeAccept,
    _permit: E2eeEstablishedPermit,
}

enum EstablishOutcome {
    Accepted {
        channel: E2eeChannel,
        admission: EstablishedE2eeAdmission,
        features: E2eeFeatures,
    },
    Rejected {
        channel: E2eeChannel,
        code: &'static str,
    },
}

/// Runs the complete `/ws-e2ee` lifecycle: Noise NK, encrypted credential
/// bootstrap, then the unchanged RPC protocol over encrypted records.
#[allow(
    clippy::too_many_arguments,
    reason = "Explicit transport handoff from the HTTP upgrade"
)]
pub(crate) async fn run_e2ee_session(
    socket: WebSocket,
    peer_ip: IpAddr,
    reach: Option<ClientReach>,
    preauth_admission: E2eePreauthAdmission,
    auth: AuthService,
    registry: RpcRegistry,
    config: Arc<ServerConfig>,
    session_shutdown: CancellationToken,
) {
    let Ok(preauth_permit) = preauth_admission.try_admit(peer_ip, tokio::time::Instant::now())
    else {
        let mut socket = socket;
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 1013,
                reason: "busy".into(),
            })))
            .await;
        return;
    };

    let (mut ws_writer, mut ws_reader) = socket.split();
    let deadline = Instant::now() + E2EE_HANDSHAKE_TIMEOUT;
    let established = timeout_at(deadline, async {
        let mut channel = accept_noise_nk(
            &mut ws_writer,
            &mut ws_reader,
            auth.host_identity(),
            &[],
            deadline,
        )
        .await?;

        let auth_bytes = loop {
            let frame = next_binary_frame(&mut ws_reader)
                .await
                .ok_or(E2eeSessionError::Closed)?;
            if let Some(message) = channel.decrypt_frame(&frame, MAX_E2EE_PREAUTH_MESSAGE_BYTES)? {
                break message;
            }
        };
        let Ok(message) = serde_json::from_slice::<E2eeAuthMessage>(&auth_bytes) else {
            return Ok(EstablishOutcome::Rejected {
                channel,
                code: "protocol",
            });
        };
        if message.r#type != "e2ee_auth" {
            return Ok(EstablishOutcome::Rejected {
                channel,
                code: "protocol",
            });
        }
        let features = E2eeFeatures::agreed(&message.features, remote_compression_enabled());

        let established_reservation = match E2EE_RESOURCE_BUDGET.try_reserve() {
            Ok(reservation) => reservation,
            Err(code) => return Ok(EstablishOutcome::Rejected { channel, code }),
        };

        let accept = if config.unsafe_no_auth {
            E2eeAccept::Unauthenticated
        } else {
            match (message.pairing, message.bearer) {
                (Some(pairing), None) => {
                    // The server decides from the consumed grant whether
                    // delivery must be confirmed; a client cannot opt out of
                    // the durable-delivery guard through the wire message.
                    let Ok((issued, confirmation_required)) = auth
                        .exchange_pairing_bootstrap(&pairing, e2ee_client_metadata())
                        .await
                    else {
                        return Ok(EstablishOutcome::Rejected {
                            channel,
                            code: "unauthorized",
                        });
                    };
                    let delivery_guard = confirmation_required.then(|| {
                        Box::new(MintedSessionDeliveryGuard::new(
                            auth.clone(),
                            issued.principal.session_id.clone(),
                        ))
                    });
                    E2eeAccept::Authenticated {
                        principal: issued.principal,
                        minted: Some(MintedE2eeSession {
                            credential: issued.token,
                            delivery_guard,
                        }),
                    }
                }
                (None, Some(bearer)) => {
                    let Ok(principal) = auth
                        .authenticate_token(&bearer, SessionTransport::E2ee)
                        .await
                    else {
                        return Ok(EstablishOutcome::Rejected {
                            channel,
                            code: "unauthorized",
                        });
                    };
                    E2eeAccept::Authenticated {
                        principal,
                        minted: None,
                    }
                }
                _ => {
                    return Ok(EstablishOutcome::Rejected {
                        channel,
                        code: "protocol",
                    });
                }
            }
        };
        let established_permit = match &accept {
            E2eeAccept::Authenticated { principal, .. } => {
                match established_reservation.bind_principal(&principal.session_id) {
                    Ok(permit) => permit,
                    Err(code) => return Ok(EstablishOutcome::Rejected { channel, code }),
                }
            }
            E2eeAccept::Unauthenticated => established_reservation.unpartitioned(),
        };

        let storage_instance_id = config.storage_instance_id.as_ref().map(ToString::to_string);
        let reply = match &accept {
            E2eeAccept::Authenticated {
                minted: Some(minted),
                ..
            } => e2ee_authenticated_with_credential_json(
                &minted.credential,
                &config.environment_id,
                storage_instance_id.as_deref(),
                minted.delivery_guard.is_some(),
                features,
            ),
            E2eeAccept::Authenticated { minted: None, .. } | E2eeAccept::Unauthenticated => {
                e2ee_authenticated_json(features)
            }
        };
        send_encrypted_frames(&mut ws_writer, &mut channel, &reply).await?;
        Ok::<_, E2eeSessionError>(EstablishOutcome::Accepted {
            channel,
            admission: EstablishedE2eeAdmission {
                accept,
                _permit: established_permit,
            },
            features,
        })
    })
    .await;
    drop(preauth_permit);

    let (channel, admission, features) = match established {
        Ok(Ok(EstablishOutcome::Accepted {
            channel,
            admission,
            features,
        })) => (channel, admission, features),
        Ok(Ok(EstablishOutcome::Rejected { mut channel, code })) => {
            let _ =
                send_encrypted_frames(&mut ws_writer, &mut channel, &e2ee_error_json(code)).await;
            let _ = ws_writer.close().await;
            return;
        }
        // `accept_noise_nk` already closed with `host-identity`.
        Ok(Err(E2eeSessionError::Handshake)) => return,
        Ok(Err(_)) | Err(_) => {
            let _ = ws_writer.close().await;
            return;
        }
    };

    run_established_e2ee(
        ws_writer,
        ws_reader,
        channel,
        admission,
        reach,
        features,
        auth,
        registry,
        session_shutdown,
    )
    .await;
}

/// Answers a Noise_NK handshake as `identity` under `prologue`: reads message A, writes
/// message B, and returns the channel, all before `deadline`. A client that pinned another
/// key (or prologue) gets close code 4403 `host-identity` and `Handshake`.
pub(crate) async fn accept_noise_nk(
    writer: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    reader: &mut futures_util::stream::SplitStream<WebSocket>,
    identity: &HostIdentity,
    prologue: &[u8],
    deadline: Instant,
) -> Result<E2eeChannel, E2eeSessionError> {
    let message_a = timeout_at(deadline, next_binary_frame(reader))
        .await
        .map_err(|_| E2eeSessionError::Timeout)?
        .ok_or(E2eeSessionError::Closed)?;
    let (channel, message_b) =
        match E2eeChannel::respond_to_message_a(identity, prologue, &message_a) {
            Ok(accepted) => accepted,
            Err(E2eeSessionError::Handshake) => {
                let _ = send_frame_before(
                    writer,
                    Message::Close(Some(CloseFrame {
                        code: E2EE_HOST_IDENTITY_CLOSE_CODE,
                        reason: "host-identity".into(),
                    })),
                    Instant::now() + SOCKET_WRITE_TIMEOUT,
                )
                .await;
                return Err(E2eeSessionError::Handshake);
            }
            Err(error) => return Err(error),
        };
    send_frame_before(
        writer,
        Message::Binary(message_b.into()),
        deadline.min(Instant::now() + SOCKET_WRITE_TIMEOUT),
    )
    .await?;
    Ok(channel)
}

/// The next binary frame; `None` once the client closes, errs, or sends text.
pub(crate) async fn next_binary_frame(
    reader: &mut futures_util::stream::SplitStream<WebSocket>,
) -> Option<Vec<u8>> {
    loop {
        match reader.next().await? {
            Ok(Message::Binary(frame)) => return Some(frame.to_vec()),
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(Message::Text(_) | Message::Close(_)) | Err(_) => return None,
        }
    }
}

async fn send_encrypted_frames(
    writer: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    channel: &mut E2eeChannel,
    plaintext: &[u8],
) -> Result<(), E2eeSessionError> {
    let deadline = Instant::now() + SOCKET_WRITE_TIMEOUT;
    for (flag, chunk) in plaintext_records(plaintext)? {
        let frame = channel.encrypt_record(flag, chunk)?;
        send_frame_before(writer, Message::Binary(frame.into()), deadline).await?;
    }
    Ok(())
}

/// Writes one frame before `deadline`. A missed deadline is `Timeout` and a
/// rejected write is `Closed`.
async fn send_frame_before(
    writer: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    frame: Message,
    deadline: Instant,
) -> Result<(), E2eeSessionError> {
    timeout_at(deadline, writer.send(frame))
        .await
        .map_err(|_| E2eeSessionError::Timeout)?
        .map_err(|_| E2eeSessionError::Closed)
}

#[allow(
    clippy::too_many_arguments,
    reason = "Explicit authenticated transport handoff"
)]
async fn run_established_e2ee<W, R>(
    mut ws_writer: W,
    ws_reader: R,
    channel: E2eeChannel,
    admission: EstablishedE2eeAdmission,
    reach: Option<ClientReach>,
    features: E2eeFeatures,
    auth: AuthService,
    registry: RpcRegistry,
    session_shutdown: CancellationToken,
) where
    W: futures_util::Sink<Message> + Unpin + Send + 'static,
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin + Send + 'static,
{
    let EstablishedE2eeAdmission {
        accept,
        _permit: established_permit,
    } = admission;
    let ((context, expiration_guard, connection_guard, pairing_delivery_guard), established_permit) =
        match accept {
            E2eeAccept::Authenticated { principal, minted } => {
                let session_id = principal.session_id.clone();
                let Ok((connection_guard, admitted_permit)) = register_e2ee_connection(
                    &auth,
                    &session_id,
                    session_shutdown.clone(),
                    established_permit,
                )
                .await
                else {
                    session_shutdown.cancel();
                    let _ = timeout(SOCKET_WRITE_TIMEOUT, ws_writer.close()).await;
                    return;
                };
                let established_permit = admitted_permit;
                let expires_at_ms = principal.expires_at_ms;
                let (context, pairing_delivery_guard) = match minted {
                    Some(minted) => match minted.delivery_guard {
                        Some(delivery_guard) => {
                            let latch = delivery_guard.confirmation_latch();
                            (
                                RpcSessionContext::authenticated_pending_pairing(
                                    principal,
                                    auth.clone(),
                                    latch,
                                ),
                                Some(delivery_guard),
                            )
                        }
                        None => (
                            RpcSessionContext::authenticated(principal, auth.clone()),
                            None,
                        ),
                    },
                    None => (
                        RpcSessionContext::authenticated(principal, auth.clone()),
                        None,
                    ),
                };
                let session = (
                    context,
                    Some(spawn_session_expiration_guard(
                        expires_at_ms,
                        session_shutdown.clone(),
                    )),
                    Some(connection_guard),
                    pairing_delivery_guard,
                );
                (session, established_permit)
            }
            E2eeAccept::Unauthenticated => (
                (RpcSessionContext::unauthenticated(), None, None, None),
                established_permit,
            ),
        };
    let principal_inbound_permits = established_permit.principal_inbound();
    let global_inbound_permits = E2EE_RESOURCE_BUDGET.global_inbound();
    let channel = Arc::new(Mutex::new(channel));
    let liveness = ConnectionLiveness::new();
    let mut ws_reader = observe_inbound(Arc::clone(&liveness), ws_reader);
    let (inbound_tx, inbound_rx) =
        tokio::sync::mpsc::channel::<Result<RpcInboundFrame, axum::Error>>(64);

    let inbound_shutdown = session_shutdown.clone();
    let inbound_channel = Arc::clone(&channel);
    let inbound_connection_permits =
        Arc::new(WeightedByteBudget::new(MAX_E2EE_LOGICAL_MESSAGE_BYTES));
    let inbound_liveness = Arc::clone(&liveness);
    let inbound_pump = tokio::spawn(async move {
        // Each logical message gets one absolute, size-derived assembly
        // deadline, established at its first record and extended as records
        // arrive. Byte admission for every record — including the first — is
        // bounded by it, so neither a dribbling sender nor pool pressure from
        // other principals can park this pump or the global pool indefinitely,
        // while a compliant slow sender is never cut off. The resetting
        // progress deadline still cuts idle senders early.
        let mut assembly: Option<InboundAssemblyState> = None;
        loop {
            let frame = match &assembly {
                Some(state) => {
                    let read_deadline = state.progress_deadline.min(state.absolute_deadline);
                    tokio::select! {
                        () = inbound_shutdown.cancelled() => break,
                        frame = timeout_at(read_deadline, ws_reader.next()) => {
                            match frame {
                                Ok(Some(frame)) => frame,
                                Ok(None) | Err(_) => break,
                            }
                        }
                    }
                }
                None => {
                    tokio::select! {
                        () = inbound_shutdown.cancelled() => break,
                        frame = ws_reader.next() => {
                            let Some(frame) = frame else { break };
                            frame
                        }
                    }
                }
            };
            let message = match frame {
                Ok(Message::Binary(bytes)) => {
                    let record = {
                        let mut channel = inbound_channel.lock().expect("E2EE channel lock");
                        let record = match channel.decrypt_record(&bytes) {
                            Ok(record) => record,
                            Err(_) => break,
                        };
                        if channel
                            .validate_decrypted_record(&record, MAX_E2EE_LOGICAL_MESSAGE_BYTES)
                            .is_err()
                        {
                            break;
                        }
                        record
                    };
                    let arrived_at = Instant::now();
                    let chunk_len = record.chunk.len();
                    let admission_deadline = match &assembly {
                        Some(state) => inbound_assembly_deadline(
                            state.started_at,
                            state.received_bytes.saturating_add(chunk_len),
                        ),
                        None => inbound_assembly_deadline(arrived_at, chunk_len),
                    };
                    let permit = match acquire_inbound_bytes(
                        chunk_len,
                        &global_inbound_permits,
                        principal_inbound_permits.as_ref(),
                        &inbound_connection_permits,
                        &inbound_shutdown,
                        Some(admission_deadline),
                    )
                    .await
                    {
                        Ok(permit) => permit,
                        Err(_) => break,
                    };
                    let decrypted = inbound_channel
                        .lock()
                        .expect("E2EE channel lock")
                        .assemble_decrypted_record(record, MAX_E2EE_LOGICAL_MESSAGE_BYTES, permit);
                    let incomplete = inbound_channel
                        .lock()
                        .expect("E2EE channel lock")
                        .has_incomplete_message();
                    assembly = if incomplete {
                        let (started_at, received_bytes) = match assembly.take() {
                            Some(state) => (
                                state.started_at,
                                state.received_bytes.saturating_add(chunk_len),
                            ),
                            None => (arrived_at, chunk_len),
                        };
                        Some(InboundAssemblyState {
                            started_at,
                            received_bytes,
                            absolute_deadline: inbound_assembly_deadline(
                                started_at,
                                received_bytes,
                            ),
                            progress_deadline: Instant::now()
                                + E2EE_INCOMPLETE_MESSAGE_PROGRESS_TIMEOUT,
                        })
                    } else {
                        None
                    };
                    match decrypted {
                        Ok(Some(BudgetedPlaintext { plaintext, permits })) => {
                            RpcInboundFrame::guarded(
                                Message::Binary(plaintext.into()),
                                E2eeInboundBudget { _permits: permits },
                            )
                        }
                        Ok(None) => continue,
                        Err(_) => break,
                    }
                }
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                // `observe_inbound` recorded the close code for the session's report.
                Ok(Message::Close(_)) => break,
                Err(_) | Ok(Message::Text(_)) => break,
            };
            if inbound_tx.send(Ok(message)).await.is_err() {
                break;
            }
        }
        // A pump that stopped on its own lost the client; an end recorded before (its Close
        // frame, the heartbeat, the writer) still wins.
        if !inbound_shutdown.is_cancelled() {
            inbound_liveness.record_client_gone();
        }
        inbound_shutdown.cancel();
    });

    let reader_stream = stream::unfold(inbound_rx, |mut receiver| async {
        receiver.recv().await.map(|item| (item, receiver))
    });
    // The session's writer task owns the socket sink and encrypts one record
    // at a time, so control records can overtake a large message.
    run_session_split_budgeted(
        ws_writer,
        OutboundFraming::Encrypted {
            channel: Arc::clone(&channel),
            interleave: features.interleave,
            deflate: features.deflate,
        },
        liveness,
        reader_stream,
        registry,
        context
            .with_reach(reach)
            .with_channel(crate::device::media::MediaChannel::E2ee),
        session_shutdown.clone(),
        Some(RpcOutboundBudget::new(
            E2EE_RESOURCE_BUDGET.global_outbound(),
            E2EE_OUTBOUND_BUFFER_BUDGET_BYTES_PER_CONNECTION,
        )),
    )
    .await;

    session_shutdown.cancel();
    if let Some(expiration_guard) = expiration_guard {
        let _ = expiration_guard.await;
    }
    reap_pump(inbound_pump).await;
    if let Some(connection_guard) = connection_guard {
        connection_guard.close().await;
    }
    drop(pairing_delivery_guard);
}

async fn register_e2ee_connection(
    auth: &AuthService,
    session_id: &str,
    session_shutdown: CancellationToken,
    established_permit: E2eeEstablishedPermit,
) -> Result<(AuthenticatedConnectionGuard, E2eeEstablishedPermit), crate::auth::AuthError> {
    match auth
        .mark_connected_guard(session_id, session_shutdown)
        .await
    {
        Ok(connection_guard) => Ok((connection_guard, established_permit)),
        Err(error) => {
            drop(established_permit);
            Err(error)
        }
    }
}

#[cfg(test)]
async fn decrypt_inbound_frame_budgeted(
    channel: &mut E2eeChannel,
    frame: &[u8],
    max_message_bytes: usize,
    global_budget: &Arc<WeightedByteBudget>,
    principal_budget: Option<&Arc<WeightedByteBudget>>,
    connection_budget: &Arc<WeightedByteBudget>,
    deadline: Option<Instant>,
) -> Result<Option<BudgetedPlaintext>, E2eeSessionError> {
    let record = channel.decrypt_record(frame)?;
    channel.validate_decrypted_record(&record, max_message_bytes)?;
    let permit = acquire_inbound_bytes(
        record.chunk.len(),
        global_budget,
        principal_budget,
        connection_budget,
        &CancellationToken::new(),
        deadline,
    )
    .await?;
    channel.assemble_decrypted_record(record, max_message_bytes, permit)
}

/// Rolling per-message assembly bound for the inbound pump.
struct InboundAssemblyState {
    started_at: Instant,
    received_bytes: usize,
    absolute_deadline: Instant,
    progress_deadline: Instant,
}

/// Absolute inbound assembly bound: the five-second base plus one second
/// per 64 KiB received. This inbound resource limit is independent of the
/// outbound writer's 30-second base and 16 KiB/s floor.
fn inbound_assembly_deadline(started_at: Instant, received_bytes: usize) -> Instant {
    let size_seconds = received_bytes.div_ceil(E2EE_LOGICAL_WRITE_BYTES_PER_SECOND);
    let allowance = Duration::from_secs(u64::try_from(size_seconds).unwrap_or(u64::MAX));
    started_at
        .checked_add(SOCKET_WRITE_TIMEOUT.saturating_add(allowance))
        .unwrap_or(started_at)
}

async fn acquire_inbound_bytes(
    bytes: usize,
    global_budget: &Arc<WeightedByteBudget>,
    principal_budget: Option<&Arc<WeightedByteBudget>>,
    connection_budget: &Arc<WeightedByteBudget>,
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<Option<E2eeRecordPermit>, E2eeSessionError> {
    if bytes == 0 {
        return Ok(None);
    }
    let connection = Arc::clone(connection_budget)
        .acquire_cancellable(bytes, cancellation, deadline)
        .await
        .map_err(map_inbound_budget_error)?;
    let principal = if let Some(principal_budget) = principal_budget {
        Some(
            Arc::clone(principal_budget)
                .acquire_cancellable(bytes, cancellation, deadline)
                .await
                .map_err(map_inbound_budget_error)?,
        )
    } else {
        None
    };
    let global = Arc::clone(global_budget)
        .acquire_cancellable(bytes, cancellation, deadline)
        .await
        .map_err(map_inbound_budget_error)?;
    Ok(Some(E2eeRecordPermit {
        global,
        principal,
        connection,
    }))
}

fn map_inbound_budget_error(error: WeightedByteAcquireError) -> E2eeSessionError {
    match error {
        WeightedByteAcquireError::Oversized => {
            E2eeSessionError::Protocol("record buffer budget overflow".into())
        }
        WeightedByteAcquireError::Cancelled => E2eeSessionError::Closed,
        WeightedByteAcquireError::Deadline => E2eeSessionError::Timeout,
    }
}

async fn reap_pump(mut pump: tokio::task::JoinHandle<()>) {
    if timeout(PUMP_JOIN_TIMEOUT, &mut pump).await.is_err() {
        pump.abort();
        let _ = pump.await;
    }
}

fn e2ee_client_metadata() -> crate::auth::ClientMetadata {
    crate::auth::ClientMetadata {
        label: None,
        ip_address: None,
        user_agent: None,
        device_type: "unknown".to_owned(),
        os: None,
        browser: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{HostIdentity, SessionTransport};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct E2eeTransportConstantsFixture {
        max_ciphertext_bytes: usize,
        max_chunk_bytes: usize,
        max_logical_message_bytes: usize,
        max_records_per_message: usize,
        max_preauth_message_bytes: usize,
        logical_write_bytes_per_second: usize,
        socket_write_timeout_ms: u64,
    }

    #[test]
    fn shared_fixture_matches_server_transport_constants() {
        let fixture: E2eeTransportConstantsFixture = serde_json::from_str(include_str!(
            "../../../../packages/shared/fixtures/e2ee-transport-constants.json"
        ))
        .expect("shared E2EE transport constants fixture");

        assert_eq!(MAX_E2EE_CIPHERTEXT_BYTES, fixture.max_ciphertext_bytes);
        assert_eq!(MAX_E2EE_CHUNK_BYTES, fixture.max_chunk_bytes);
        assert_eq!(
            MAX_E2EE_LOGICAL_MESSAGE_BYTES,
            fixture.max_logical_message_bytes
        );
        assert_eq!(
            MAX_E2EE_RECORDS_PER_MESSAGE,
            fixture.max_records_per_message
        );
        assert_eq!(
            MAX_E2EE_PREAUTH_MESSAGE_BYTES,
            fixture.max_preauth_message_bytes
        );
        assert_eq!(
            E2EE_LOGICAL_WRITE_BYTES_PER_SECOND,
            fixture.logical_write_bytes_per_second
        );
        assert_eq!(
            SOCKET_WRITE_TIMEOUT,
            Duration::from_millis(fixture.socket_write_timeout_ms)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn preauth_admission_partitions_slots_and_refills_peer_tokens() {
        let admission = E2eePreauthAdmission::new();
        let peer = "198.51.100.42".parse().expect("peer IP");
        let now = tokio::time::Instant::now();
        let mut leases = (0..E2EE_MAX_PREAUTH_CONNECTIONS_PER_PEER)
            .map(|_| admission.try_admit(peer, now).expect("per-peer slot"))
            .collect::<Vec<_>>();
        assert!(matches!(admission.try_admit(peer, now), Err("busy")));

        leases.clear();
        for _ in 0..E2EE_MAX_PREAUTH_CONNECTIONS_PER_PEER {
            drop(
                admission
                    .try_admit(peer, now)
                    .expect("remaining burst token"),
            );
        }
        assert!(matches!(admission.try_admit(peer, now), Err("rate")));

        tokio::time::advance(Duration::from_secs(1)).await;
        drop(
            admission
                .try_admit(peer, tokio::time::Instant::now())
                .expect("one token refills per second"),
        );
    }

    #[tokio::test]
    async fn established_networks_stop_at_the_soft_cap_and_fresh_networks_use_the_reserve() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let mut leases = Vec::new();
        // One hostile /24 saturates its per-network cap.
        for host in 1..=16 {
            let peer = std::net::Ipv4Addr::new(198, 18, 1, host).into();
            leases.push(admission.try_admit(peer, now).expect("first network slot"));
        }
        // A second hostile /24 grows only until the global soft cap.
        let mut second_network_slots = 0;
        for host in 1..=16_u8 {
            let peer = std::net::Ipv4Addr::new(198, 18, 2, host).into();
            match admission.try_admit(peer, now) {
                Ok(lease) => {
                    leases.push(lease);
                    second_network_slots += 1;
                }
                Err("busy") => break,
                Err(other) => panic!("unexpected admission error: {other}"),
            }
        }
        assert_eq!(
            second_network_slots,
            E2EE_PREAUTH_GLOBAL_SOFT_CAP - E2EE_MAX_PREAUTH_CONNECTIONS_PER_NETWORK,
            "established buckets stop growing at the global soft cap"
        );

        // A fresh network's first connection still finds room in the reserve.
        let fresh = admission
            .try_admit("203.0.113.1".parse().expect("fresh peer IP"), now)
            .expect("a fresh network uses the reserved headroom");
        // The hostile networks cannot follow it into the reserve.
        assert!(matches!(
            admission.try_admit(IpAddr::from(std::net::Ipv4Addr::new(198, 18, 2, 99)), now),
            Err("busy")
        ));
        drop(fresh);
        drop(leases);
    }

    #[tokio::test]
    async fn one_public_subnet_cannot_consume_more_than_half_the_global_pool() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let mut leases = Vec::new();
        for host in 1..=16 {
            let peer = std::net::Ipv4Addr::new(203, 0, 113, host).into();
            leases.push(admission.try_admit(peer, now).expect("subnet slot"));
        }
        let seventeenth = std::net::Ipv4Addr::new(203, 0, 113, 17).into();
        assert!(matches!(admission.try_admit(seventeenth, now), Err("busy")));
        drop(leases);
    }

    #[test]
    fn preauth_network_keys_canonicalize_ipv4_24_and_ipv6_64() {
        let (_, first_v4) = classify_preauth_peer("192.0.2.1".parse().expect("first IPv4"));
        let (_, same_v4) = classify_preauth_peer("192.0.2.200".parse().expect("same IPv4 /24"));
        let (_, other_v4) = classify_preauth_peer("192.0.3.1".parse().expect("other IPv4 /24"));
        assert_eq!(first_v4, same_v4);
        assert_ne!(first_v4, other_v4);

        let (_, first_v6) = classify_preauth_peer("2001:db8:1::1".parse().expect("first IPv6"));
        let (_, same_v6) =
            classify_preauth_peer("2001:db8:1::ffff".parse().expect("same IPv6 /64"));
        let (_, other_v6) = classify_preauth_peer("2001:db8:2::1".parse().expect("other IPv6 /64"));
        assert_eq!(first_v6, same_v6);
        assert_ne!(first_v6, other_v6);
    }

    #[tokio::test]
    async fn one_ipv6_64_cannot_consume_more_than_half_the_global_pool() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let mut leases = Vec::new();
        for host in 1..=16 {
            let peer = std::net::Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, host).into();
            leases.push(admission.try_admit(peer, now).expect("IPv6 subnet slot"));
        }
        let seventeenth = std::net::Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, 17).into();
        assert!(matches!(admission.try_admit(seventeenth, now), Err("busy")));
        drop(leases);
    }

    #[tokio::test]
    async fn loopback_forwarder_can_use_global_capacity_without_the_public_peer_cap() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let mut leases = Vec::new();
        for _ in 0..5 {
            leases.push(
                admission
                    .try_admit("127.0.0.1".parse().expect("loopback"), now)
                    .expect("trusted loopback-forwarder slot"),
            );
        }
        drop(leases);
    }

    #[tokio::test]
    async fn missing_connect_info_uses_the_strict_unspecified_bucket() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let unspecified = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        let mut leases = (0..E2EE_MAX_PREAUTH_CONNECTIONS_PER_PEER)
            .map(|_| {
                admission
                    .try_admit(unspecified, now)
                    .expect("strict unspecified slot")
            })
            .collect::<Vec<_>>();
        assert!(matches!(admission.try_admit(unspecified, now), Err("busy")));
        leases.clear();
    }

    // The documented residual: a wide-prefix holder can take the reserve with
    // many distinct first-connections, and the hard global cap is the final
    // bound. Per-address admission cannot price that attacker out.
    #[tokio::test]
    async fn unrelated_public_networks_still_stop_at_the_global_cap() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let mut leases = Vec::new();
        for network in 0..E2EE_MAX_PREAUTH_CONNECTIONS {
            let peer = std::net::Ipv4Addr::new(198, 19, u8::try_from(network).unwrap(), 1).into();
            leases.push(admission.try_admit(peer, now).expect("global slot"));
        }
        assert!(matches!(
            admission.try_admit("203.0.113.1".parse().expect("overflow peer"), now),
            Err("busy")
        ));
        drop(leases);
    }

    #[test]
    fn forwarded_loopback_peers_key_on_the_proxy_appended_client_address() {
        let loopback: IpAddr = "127.0.0.1".parse().expect("loopback");
        let client: IpAddr = "203.0.113.7".parse().expect("client");

        // The last entry is the hop the trusted local proxy appended.
        assert_eq!(
            effective_preauth_peer(loopback, Some("203.0.113.7")),
            client
        );
        assert_eq!(
            effective_preauth_peer(loopback, Some("6.6.6.6, 203.0.113.7")),
            client
        );
        assert_eq!(
            effective_preauth_peer(loopback, Some(" [2001:db8::1] ")),
            "2001:db8::1".parse::<IpAddr>().expect("v6 client")
        );
        // A mapped-IPv6 loopback socket still trusts the header.
        assert_eq!(
            effective_preauth_peer(
                "::ffff:127.0.0.1".parse().expect("mapped"),
                Some("203.0.113.7")
            ),
            client
        );
        // Only loopback sockets may carry the header; remote peers keep their
        // socket address no matter what they send.
        let remote: IpAddr = "198.51.100.9".parse().expect("remote");
        assert_eq!(effective_preauth_peer(remote, Some("203.0.113.7")), remote);
        // No header: the exempt forwarder class remains.
        assert_eq!(effective_preauth_peer(loopback, None), loopback);
        assert_eq!(effective_preauth_peer(loopback, Some("  ")), loopback);
        // Garbage falls into the strict unspecified bucket, not the exempt one.
        assert_eq!(
            effective_preauth_peer(loopback, Some("not-an-ip")),
            IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn forwarder_token_bucket_absorbs_a_fleet_reconnect_storm() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        let loopback: IpAddr = "127.0.0.1".parse().expect("loopback");
        // A whole fleet reconnecting through a proxy that sets no header burns
        // one shared bucket; it must hold far more than one device's burst.
        for _ in 0..u64::from(E2EE_PREAUTH_BURST_FORWARDER) {
            drop(
                admission
                    .try_admit(loopback, now)
                    .expect("forwarder burst slot"),
            );
        }
        assert!(matches!(admission.try_admit(loopback, now), Err("rate")));
        tokio::time::advance(E2EE_PREAUTH_REFILL_INTERVAL).await;
        let refilled = tokio::time::Instant::now();
        for _ in 0..u64::from(E2EE_PREAUTH_FORWARDER_REFILL_PER_INTERVAL) {
            drop(
                admission
                    .try_admit(loopback, refilled)
                    .expect("forwarder refill slot"),
            );
        }
        assert!(matches!(
            admission.try_admit(loopback, refilled),
            Err("rate")
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_preauth_peer_and_network_entries_are_pruned_without_exceeding_the_map_cap() {
        let admission = E2eePreauthAdmission::new();
        let now = tokio::time::Instant::now();
        for network in 0..1_024_u16 {
            let peer = std::net::Ipv6Addr::new(0x2001, 0xdb8, network, 0, 0, 0, 0, 1).into();
            drop(
                admission
                    .try_admit(peer, now)
                    .expect("tracked peer and network"),
            );
        }
        let overflow = std::net::Ipv6Addr::new(0x2001, 0xdb8, 1_024, 0, 0, 0, 0, 1).into();
        assert!(matches!(admission.try_admit(overflow, now), Err("busy")));

        tokio::time::advance(E2EE_PREAUTH_PEER_STATE_TTL).await;
        drop(
            admission
                .try_admit(overflow, tokio::time::Instant::now())
                .expect("expired entries are pruned before admission"),
        );
        assert_eq!(admission.peer_count(), 1);
        assert_eq!(admission.network_count(), 1);
    }

    #[tokio::test]
    async fn incomplete_minted_session_delivery_is_compensated() {
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        let auth = AuthService::new(&config, vec![7_u8; 32]);
        let offer = auth
            .issue_share_pairing(
                crate::auth::STANDARD_SCOPES
                    .iter()
                    .map(|scope| (*scope).to_owned())
                    .collect(),
                None,
                "another-device".to_owned(),
                true,
            )
            .await
            .expect("off-host share pairing");
        let (issued, confirmation_required) = auth
            .exchange_pairing_bootstrap(&offer.credential, e2ee_client_metadata())
            .await
            .expect("minted E2EE session");
        assert!(confirmation_required);
        let session_id = issued.principal.session_id.clone();

        drop(MintedSessionDeliveryGuard::new(
            auth.clone(),
            session_id.clone(),
        ));

        timeout(Duration::from_secs(1), async {
            loop {
                if auth.list_clients("other-session").await.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("compensating revocation completes");
        assert!(matches!(
            auth.authenticate_token(&issued.token, SessionTransport::E2ee)
                .await,
            Err(crate::auth::AuthError::InvalidCredential)
        ));
    }

    struct SnowInitiator {
        transport: snow::TransportState,
    }

    async fn establish() -> (SnowInitiator, E2eeChannel) {
        let identity = HostIdentity::generate_ephemeral();
        let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
            .remote_public_key(identity.public_key_bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len_a = initiator.write_message(&[], &mut message_a).unwrap();
        message_a.truncate(len_a);

        let (to_responder, mut responder_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let (to_initiator, mut initiator_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        to_responder.send(message_a).await.unwrap();
        let responder = E2eeChannel::respond(
            &identity,
            async || responder_rx.recv().await,
            |frame| {
                let sender = to_initiator.clone();
                async move {
                    sender
                        .send(frame)
                        .await
                        .map_err(|_| E2eeSessionError::Closed)
                }
            },
        )
        .await
        .unwrap();
        let message_b = initiator_rx.recv().await.unwrap();
        let mut payload = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = initiator.read_message(&message_b, &mut payload).unwrap();
        assert_eq!(len, 0, "message B carries an empty handshake payload");
        let transport = initiator.into_transport_mode().unwrap();
        (SnowInitiator { transport }, responder)
    }

    fn initiator_encrypt(initiator: &mut SnowInitiator, records: &[Vec<u8>]) -> Vec<Vec<u8>> {
        records
            .iter()
            .map(|record| {
                let mut frame = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
                let len = initiator
                    .transport
                    .write_message(record, &mut frame)
                    .unwrap();
                frame.truncate(len);
                frame
            })
            .collect()
    }

    fn record(flag: u8, chunk: &[u8]) -> Vec<u8> {
        let mut record = Vec::with_capacity(1 + chunk.len());
        record.push(flag);
        record.extend_from_slice(chunk);
        record
    }

    #[test]
    fn plaintext_record_iteration_is_lazy_and_preserves_flags() {
        let big = vec![b'a'; MAX_E2EE_CHUNK_BYTES * 2 + 5];
        let mut records = plaintext_records(&big).expect("valid message");
        let first = records.next().expect("first record");
        assert_eq!(first.0, E2EE_RECORD_FLAG_CONTINUATION);
        assert_eq!(first.1.len(), MAX_E2EE_CHUNK_BYTES);
        assert_eq!(records.count(), 2);
    }

    #[test]
    fn pairing_reply_omits_an_absent_storage_identity() {
        let absent =
            serde_json::from_slice::<serde_json::Value>(&e2ee_authenticated_with_credential_json(
                "credential",
                "environment",
                None,
                true,
                E2eeFeatures::default(),
            ))
            .expect("absent storage reply");
        assert!(absent.get("storageInstanceId").is_none());

        let present =
            serde_json::from_slice::<serde_json::Value>(&e2ee_authenticated_with_credential_json(
                "credential",
                "environment",
                Some("storage"),
                true,
                E2eeFeatures::default(),
            ))
            .expect("present storage reply");
        assert_eq!(present["storageInstanceId"], "storage");
    }

    #[test]
    fn authenticated_replies_confirm_interleave_only_when_requested() {
        let plain: serde_json::Value =
            serde_json::from_slice(&e2ee_authenticated_json(E2eeFeatures::default()))
                .expect("plain reply");
        assert!(plain.get("features").is_none());
        let interleaved: serde_json::Value =
            serde_json::from_slice(&e2ee_authenticated_json(E2eeFeatures {
                interleave: true,
                deflate: false,
            }))
            .expect("interleaved reply");
        assert_eq!(interleaved["features"], json!(["interleave-v1"]));
        let minted: serde_json::Value =
            serde_json::from_slice(&e2ee_authenticated_with_credential_json(
                "credential",
                "environment",
                None,
                false,
                E2eeFeatures {
                    interleave: true,
                    deflate: false,
                },
            ))
            .expect("minted reply");
        assert_eq!(minted["features"], json!(["interleave-v1"]));
    }

    #[tokio::test(start_paused = true)]
    async fn deflate_compresses_large_data_messages_and_leaves_small_ones_plain() {
        let (mut initiator, responder) = establish().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(futures_util::sink::unfold(
            (),
            move |(), message: Message| {
                let recorded = Arc::clone(&sink_recorded);
                async move {
                    recorded.lock().expect("recorded frames").push(message);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        ));
        let (data_sender, data) = tokio::sync::mpsc::channel(4);
        let (_control_sender, control) =
            tokio::sync::mpsc::channel(super::super::transport::CONTROL_LANE_CAPACITY);
        // Three records uncompressed, far above the 16 KiB threshold.
        let large = encrypted_response(MAX_E2EE_CHUNK_BYTES * 3);
        let small = encrypted_response(64);
        data_sender.try_send(large).expect("queue large frame");
        data_sender.try_send(small).expect("queue small frame");
        drop(data_sender);
        super::super::transport::run_writer(
            sink,
            OutboundFraming::Encrypted {
                channel: Arc::new(Mutex::new(responder)),
                interleave: false,
                deflate: true,
            },
            ConnectionLiveness::new(),
            data,
            control,
            CancellationToken::new(),
        )
        .await;

        let mut messages: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut assembling = Vec::new();
        for message in recorded.lock().expect("recorded frames").iter() {
            let Message::Binary(frame) = message else {
                panic!("encrypted records are binary");
            };
            let mut plaintext = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
            let len = initiator
                .transport
                .read_message(frame, &mut plaintext)
                .expect("decrypt record");
            assembling.extend_from_slice(&plaintext[1..len]);
            if plaintext[0] & !super::super::transport::RECORD_FLAG_DEFLATE == 0x00 {
                messages.push((plaintext[0], std::mem::take(&mut assembling)));
            }
        }
        assert_eq!(messages.len(), 2);
        let (flag, compressed) = &messages[0];
        assert_eq!(*flag, super::super::transport::RECORD_FLAG_DEFLATE);
        let declared = u32::from_be_bytes(compressed[..4].try_into().unwrap()) as usize;
        let mut inflated = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::DeflateDecoder::new(&compressed[4..]),
            &mut inflated,
        )
        .expect("raw deflate");
        assert_eq!(inflated.len(), declared);
        serde_json::from_slice::<serde_json::Value>(&inflated).expect("the original JSON");
        assert!(inflated.len() > MAX_E2EE_CHUNK_BYTES * 3);
        assert!(
            inflated
                .windows(MAX_E2EE_CHUNK_BYTES)
                .any(|window| window.iter().all(|byte| *byte == b'x'))
        );
        assert!(
            compressed.len() < MAX_E2EE_CHUNK_BYTES,
            "compressed to one record"
        );
        assert_eq!(messages[1].0, 0x00, "a small message stays uncompressed");
    }

    #[test]
    fn deflate_is_confirmed_only_when_offered() {
        let offered = |features: &[&str]| {
            E2eeFeatures::agreed(
                &features.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>(),
                true,
            )
        };
        assert_eq!(offered(&[]), E2eeFeatures::default());
        let both = offered(&["interleave-v1", "deflate-v1"]);
        assert_eq!(
            both,
            E2eeFeatures {
                interleave: true,
                deflate: true
            }
        );
        let reply: serde_json::Value =
            serde_json::from_slice(&e2ee_authenticated_json(both)).expect("reply");
        assert_eq!(reply["features"], json!(["interleave-v1", "deflate-v1"]));
        let switched_off = E2eeFeatures::agreed(
            &["interleave-v1".to_owned(), "deflate-v1".to_owned()],
            false,
        );
        assert!(switched_off.interleave && !switched_off.deflate);
    }

    #[test]
    fn remote_compression_switch_turns_off_only_on_zero() {
        assert!(compression_allowed(None));
        assert!(compression_allowed(Some("1".as_ref())));
        assert!(!compression_allowed(Some("0".as_ref())));
    }

    #[tokio::test(start_paused = true)]
    async fn interleave_sends_control_records_between_encrypted_records() {
        let (mut initiator, responder) = establish().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(futures_util::sink::unfold(
            (),
            move |(), message: Message| {
                let recorded = Arc::clone(&sink_recorded);
                async move {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    recorded.lock().expect("recorded frames").push(message);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        ));
        let (data_sender, data) = tokio::sync::mpsc::channel(4);
        let (control_sender, control) =
            tokio::sync::mpsc::channel(super::super::transport::CONTROL_LANE_CAPACITY);
        data_sender
            .try_send(encrypted_response(MAX_E2EE_CHUNK_BYTES * 4))
            .expect("queue frame");
        let writer = tokio::spawn(super::super::transport::run_writer(
            sink,
            OutboundFraming::Encrypted {
                channel: Arc::new(Mutex::new(responder)),
                interleave: true,
                deflate: false,
            },
            ConnectionLiveness::new(),
            data,
            control,
            CancellationToken::new(),
        ));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        control_sender
            .try_send(crate::rpc::ServerMessage::Pong)
            .expect("queue pong");
        drop(data_sender);
        writer.await.expect("writer joins");

        let mut flags = Vec::new();
        let mut control_plaintext = Vec::new();
        for message in recorded.lock().expect("recorded frames").iter() {
            let Message::Binary(frame) = message else {
                panic!("encrypted records are binary");
            };
            let mut plaintext = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
            let len = initiator
                .transport
                .read_message(frame, &mut plaintext)
                .expect("decrypt record");
            flags.push(plaintext[0]);
            if plaintext[0] == super::super::transport::RECORD_FLAG_CONTROL {
                control_plaintext = plaintext[1..len].to_vec();
            }
        }
        let control_at = flags
            .iter()
            .position(|flag| *flag == super::super::transport::RECORD_FLAG_CONTROL)
            .expect("a control record");
        assert!(control_at > 0 && control_at < flags.len() - 1, "{flags:?}");
        assert_eq!(control_plaintext, br#"{"_tag":"Pong"}"#);
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_reap_releases_permits_live_row_and_subscriptions() {
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("config");
        let auth = AuthService::new(&config, vec![7_u8; 32]);
        let issued = timeout(
            Duration::from_secs(1),
            auth.exchange_bootstrap(
                "desktop-test-seed",
                None,
                e2ee_client_metadata(),
                None,
                SessionTransport::E2ee,
            ),
        )
        .await
        .expect("bounded bootstrap")
        .expect("session");
        let principal = timeout(
            Duration::from_secs(1),
            auth.authenticate_token(&issued.token, SessionTransport::E2ee),
        )
        .await
        .expect("bounded authentication")
        .expect("principal");
        let session_id = principal.session_id.clone();
        let budget = E2eeResourceBudget::new(1, 1, 1024, 1024, 1024);
        let permit = budget
            .try_reserve()
            .expect("global permit")
            .bind_principal(&session_id)
            .expect("principal permit");
        let started = CancellationToken::new();
        let closed = CancellationToken::new();
        let mut registry = RpcRegistry::empty();
        let handler_started = started.clone();
        let handler_closed = closed.clone();
        registry.register_stream("subscribeServerConfig", move |_, cancellation| {
            let (sender, receiver) = tokio::sync::mpsc::channel(1);
            let started = handler_started.clone();
            let closed = handler_closed.clone();
            tokio::spawn(async move {
                started.cancel();
                timeout(Duration::from_secs(55), cancellation.cancelled())
                    .await
                    .expect("bounded subscription cancellation");
                timeout(Duration::from_secs(1), sender.closed())
                    .await
                    .expect("bounded subscription receiver closure");
                closed.cancel();
            });
            receiver
        });
        let (mut initiator, channel) = timeout(Duration::from_secs(1), establish())
            .await
            .expect("bounded Noise setup");
        let request = serde_json::json!({
            "_tag": "Request", "id": "1", "tag": "subscribeServerConfig", "payload": {}, "headers": [],
        })
        .to_string();
        let records = initiator_encrypt(&mut initiator, &[record(0x00, request.as_bytes())]);
        let (inbound, receiver) = tokio::sync::mpsc::channel(1);
        let reader = futures_util::stream::unfold(receiver, |mut receiver| async {
            timeout(Duration::from_secs(60), receiver.recv())
                .await
                .ok()
                .flatten()
                .map(|frame| (Ok::<_, axum::Error>(frame), receiver))
        });
        let shutdown = CancellationToken::new();
        let session = tokio::spawn(run_established_e2ee(
            futures_util::sink::drain(),
            Box::pin(reader),
            channel,
            EstablishedE2eeAdmission {
                accept: E2eeAccept::Authenticated {
                    principal,
                    minted: None,
                },
                _permit: permit,
            },
            None,
            E2eeFeatures {
                interleave: true,
                deflate: false,
            },
            auth.clone(),
            registry,
            shutdown,
        ));
        timeout(
            Duration::from_secs(1),
            inbound.send(Message::Binary(records[0].clone().into())),
        )
        .await
        .expect("bounded request send")
        .expect("watch request");
        timeout(Duration::from_secs(1), started.cancelled())
            .await
            .expect("authorized stream handler starts");
        assert_eq!(
            timeout(
                Duration::from_secs(1),
                auth.live_connection_count_for_test(&session_id)
            )
            .await
            .expect("bounded live-row read"),
            1
        );
        assert!(
            budget.try_reserve().is_err(),
            "established permit held while live"
        );
        let frozen_at = Instant::now(); // keep reader open, deliver no more frames
        let cleanup_deadline = frozen_at + Duration::from_secs(51);
        timeout_at(cleanup_deadline, session)
            .await
            .expect("reaped within 50 s plus 1 s scheduling tolerance")
            .expect("session joins");
        timeout_at(cleanup_deadline, closed.cancelled())
            .await
            .expect("subscription receiver closed within the same freeze bound");
        assert_eq!(
            timeout_at(
                cleanup_deadline,
                auth.live_connection_count_for_test(&session_id)
            )
            .await
            .expect("bounded live-row read"),
            0,
            "the live-connection row is gone"
        );
        assert!(
            budget
                .try_reserve()
                .expect("global permit released")
                .bind_principal(&session_id)
                .is_ok(),
            "principal permit released"
        );
        assert!(frozen_at.elapsed() <= Duration::from_secs(51));
        drop(inbound);
    }

    #[tokio::test]
    async fn small_round_trip_uses_a_single_final_record() {
        let (mut initiator, mut responder) = establish().await;
        let frames = initiator_encrypt(
            &mut initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"{\"hello\":true}")],
        );
        assert_eq!(frames.len(), 1);
        let message = responder
            .decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES)
            .unwrap()
            .unwrap();
        assert_eq!(message, b"{\"hello\":true}");
        let frames = responder.encrypt_message(b"{\"ok\":1}").unwrap();
        assert_eq!(frames.len(), 1);
        let mut plaintext = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len = initiator
            .transport
            .read_message(&frames[0], &mut plaintext)
            .unwrap();
        assert_eq!(
            &plaintext[..len],
            record(E2EE_RECORD_FLAG_FINAL, b"{\"ok\":1}").as_slice()
        );
    }

    fn encrypted_response(bytes: usize) -> crate::rpc::session::RpcOutboundFrame {
        crate::rpc::session::RpcOutboundFrame::plain(crate::rpc::ServerMessage::success(
            crate::rpc::RequestId::try_from("1").expect("request id"),
            Some(json!({ "data": "x".repeat(bytes) })),
        ))
    }

    /// Runs the shared writer over an encrypted channel with a sink that takes
    /// `delay` per record, and reports the records, whether the session was
    /// cancelled, and the elapsed time.
    async fn run_encrypted_writer(
        delay: Duration,
        bytes: usize,
    ) -> (SnowInitiator, Vec<Message>, bool, Duration) {
        let (initiator, responder) = establish().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let sink_recorded = Arc::clone(&recorded);
        let sink = Box::pin(futures_util::sink::unfold(
            (),
            move |(), message: Message| {
                let recorded = Arc::clone(&sink_recorded);
                async move {
                    tokio::time::sleep(delay).await;
                    recorded.lock().expect("recorded frames").push(message);
                    Ok::<_, std::convert::Infallible>(())
                }
            },
        ));
        let (data_sender, data) = tokio::sync::mpsc::channel(4);
        let (_control_sender, control) =
            tokio::sync::mpsc::channel(super::super::transport::CONTROL_LANE_CAPACITY);
        let shutdown = CancellationToken::new();
        data_sender
            .try_send(encrypted_response(bytes))
            .expect("queue frame");
        drop(data_sender);
        let started = Instant::now();
        super::super::transport::run_writer(
            sink,
            OutboundFraming::Encrypted {
                channel: Arc::new(Mutex::new(responder)),
                interleave: false,
                deflate: false,
            },
            ConnectionLiveness::new(),
            data,
            control,
            shutdown.clone(),
        )
        .await;
        let recorded = recorded.lock().expect("recorded frames").clone();
        (
            initiator,
            recorded,
            shutdown.is_cancelled(),
            Instant::now() - started,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn encrypted_records_accept_progress_across_record_deadlines() {
        let (_initiator, recorded, cancelled, elapsed) =
            run_encrypted_writer(Duration::from_secs(1), MAX_E2EE_CHUNK_BYTES * 5).await;
        assert!(!cancelled);
        assert_eq!(recorded.len(), 6);
        assert_eq!(elapsed, Duration::from_secs(6));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_encrypted_record_ends_the_session_after_twenty_seconds() {
        let (_initiator, _recorded, cancelled, elapsed) =
            run_encrypted_writer(Duration::from_secs(3_600), 1024).await;
        assert!(cancelled);
        assert_eq!(elapsed, Duration::from_secs(20));
    }

    #[tokio::test(start_paused = true)]
    async fn a_trickling_encrypted_sink_fails_the_message_deadline() {
        // Three records at 19 s each; the message deadline is 30 s + ~8.5 s.
        let (_initiator, _recorded, cancelled, elapsed) =
            run_encrypted_writer(Duration::from_secs(19), 140_000).await;
        assert!(cancelled);
        assert!(
            elapsed > Duration::from_secs(38) && elapsed < Duration::from_secs(39),
            "the message deadline fires at about 38.5 s, not {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_encrypted_sink_that_keeps_the_floor_finishes_eight_mebibytes() {
        let (_initiator, recorded, cancelled, _elapsed) =
            run_encrypted_writer(Duration::from_secs(1), 8 * 1024 * 1024).await;
        assert!(!cancelled);
        assert_eq!(recorded.len(), 129);
    }

    #[tokio::test]
    async fn decrypt_scratch_is_reused_between_records() {
        let (mut initiator, mut responder) = establish().await;
        let scratch = responder.decrypt_scratch.as_ptr();
        let frames = initiator_encrypt(
            &mut initiator,
            &[
                record(E2EE_RECORD_FLAG_FINAL, b"first"),
                record(E2EE_RECORD_FLAG_FINAL, b"second"),
            ],
        );

        for frame in frames {
            responder
                .decrypt_frame(&frame, MAX_E2EE_LOGICAL_MESSAGE_BYTES)
                .expect("decrypt record")
                .expect("complete message");
            assert_eq!(responder.decrypt_scratch.as_ptr(), scratch);
        }
    }

    #[tokio::test]
    async fn large_messages_fragment_and_reassemble() {
        let (mut initiator, mut responder) = establish().await;
        let big = vec![b'a'; MAX_E2EE_CHUNK_BYTES * 2 + 5];
        let frames = responder.encrypt_message(&big).unwrap();
        assert_eq!(frames.len(), 3);
        for frame in &frames {
            assert!(frame.len() <= MAX_E2EE_CIPHERTEXT_BYTES);
        }
        let mut assembled = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            let mut plaintext = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
            let len = initiator
                .transport
                .read_message(frame, &mut plaintext)
                .unwrap();
            let expected_flag = if index == frames.len() - 1 {
                E2EE_RECORD_FLAG_FINAL
            } else {
                E2EE_RECORD_FLAG_CONTINUATION
            };
            assert_eq!(plaintext[0], expected_flag);
            assembled.extend_from_slice(&plaintext[1..len]);
        }
        assert_eq!(assembled, big);
    }

    #[tokio::test]
    async fn oversized_ciphertext_frames_are_rejected_before_decryption() {
        let (_initiator, mut responder) = establish().await;
        let oversized = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES + 1];
        assert!(matches!(
            responder.decrypt_frame(&oversized, MAX_E2EE_LOGICAL_MESSAGE_BYTES),
            Err(E2eeSessionError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn reassembly_respects_the_caller_supplied_cap() {
        let (mut initiator, mut responder) = establish().await;
        let continuation = record(
            E2EE_RECORD_FLAG_CONTINUATION,
            &vec![0_u8; MAX_E2EE_CHUNK_BYTES],
        );
        let overflow = record(E2EE_RECORD_FLAG_FINAL, &[0_u8; 19]);
        let frames = initiator_encrypt(&mut initiator, &[continuation, overflow]);
        assert!(
            responder
                .decrypt_frame(&frames[0], MAX_E2EE_PREAUTH_MESSAGE_BYTES)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            responder.decrypt_frame(&frames[1], MAX_E2EE_PREAUTH_MESSAGE_BYTES),
            Err(E2eeSessionError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn logical_message_overflow_is_rejected() {
        let (mut initiator, mut responder) = establish().await;
        let continuation = record(
            E2EE_RECORD_FLAG_CONTINUATION,
            &vec![0_u8; MAX_E2EE_CHUNK_BYTES],
        );
        let needed = MAX_E2EE_LOGICAL_MESSAGE_BYTES / MAX_E2EE_CHUNK_BYTES + 1;
        let mut overflowed = false;
        for _ in 0..=needed {
            let frames = initiator_encrypt(&mut initiator, std::slice::from_ref(&continuation));
            match responder.decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES) {
                Ok(None) => {}
                Err(E2eeSessionError::Protocol(_)) => {
                    overflowed = true;
                    break;
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
        assert!(overflowed);
    }

    #[tokio::test]
    async fn partial_continuation_records_remain_wire_compatible() {
        let (mut initiator, mut responder) = establish().await;
        let frames = initiator_encrypt(
            &mut initiator,
            &[
                record(E2EE_RECORD_FLAG_CONTINUATION, b"tiny"),
                record(E2EE_RECORD_FLAG_FINAL, b" continuation"),
            ],
        );

        assert!(
            responder
                .decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            responder
                .decrypt_frame(&frames[1], MAX_E2EE_LOGICAL_MESSAGE_BYTES)
                .unwrap()
                .unwrap(),
            b"tiny continuation"
        );
    }

    #[tokio::test]
    async fn inbound_empty_continuations_and_excessive_fragmentation_are_rejected() {
        let (mut initiator, mut responder) = establish().await;
        let empty_continuation = initiator_encrypt(
            &mut initiator,
            &[record(E2EE_RECORD_FLAG_CONTINUATION, b"")],
        )
        .pop()
        .unwrap();
        assert!(matches!(
            responder.decrypt_frame(&empty_continuation, MAX_E2EE_LOGICAL_MESSAGE_BYTES),
            Err(E2eeSessionError::Protocol(_))
        ));

        let (mut initiator, mut responder) = establish().await;
        for _ in 0..MAX_E2EE_RECORDS_PER_MESSAGE {
            let continuation = initiator_encrypt(
                &mut initiator,
                &[record(E2EE_RECORD_FLAG_CONTINUATION, b"x")],
            )
            .pop()
            .unwrap();
            assert!(
                responder
                    .decrypt_frame(&continuation, MAX_E2EE_LOGICAL_MESSAGE_BYTES)
                    .expect("record within fragmentation bound")
                    .is_none()
            );
        }
        let overflow = initiator_encrypt(&mut initiator, &[record(E2EE_RECORD_FLAG_FINAL, b"")])
            .pop()
            .unwrap();
        assert!(matches!(
            responder.decrypt_frame(&overflow, MAX_E2EE_LOGICAL_MESSAGE_BYTES),
            Err(E2eeSessionError::Protocol(_))
        ));
    }

    #[tokio::test]
    async fn completed_messages_retain_their_global_buffer_budget() {
        let (mut initiator, mut responder) = establish().await;
        let continuation = vec![b'f'; MAX_E2EE_CHUNK_BYTES];
        let message_bytes = continuation.len() + b"second".len();
        let global_budget = Arc::new(WeightedByteBudget::new(message_bytes));
        let connection_budget = Arc::new(WeightedByteBudget::new(message_bytes));
        let frames = initiator_encrypt(
            &mut initiator,
            &[
                record(E2EE_RECORD_FLAG_CONTINUATION, &continuation),
                record(E2EE_RECORD_FLAG_FINAL, b"second"),
            ],
        );

        assert!(
            decrypt_inbound_frame_budgeted(
                &mut responder,
                &frames[0],
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &connection_budget,
                None,
            )
            .await
            .unwrap()
            .is_none()
        );
        let completed = decrypt_inbound_frame_budgeted(
            &mut responder,
            &frames[1],
            MAX_E2EE_LOGICAL_MESSAGE_BYTES,
            &global_budget,
            None,
            &connection_budget,
            None,
        )
        .await
        .unwrap()
        .expect("completed message");

        assert_eq!(completed.plaintext.len(), MAX_E2EE_CHUNK_BYTES + 6);
        assert!(completed.plaintext.starts_with(&continuation));
        assert!(completed.plaintext.ends_with(b"second"));
        assert!(Arc::clone(&global_budget).try_acquire(1).is_none());
        assert!(Arc::clone(&connection_budget).try_acquire(1).is_none());
        drop(completed);
        assert!(Arc::clone(&global_budget).try_acquire(1).is_some());
        assert!(Arc::clone(&connection_budget).try_acquire(1).is_some());
    }

    #[tokio::test]
    async fn inbound_global_pressure_waits_for_capacity_instead_of_closing_the_victim() {
        let (mut first_initiator, mut first_responder) = establish().await;
        let (mut second_initiator, mut second_responder) = establish().await;
        let global_budget = Arc::new(WeightedByteBudget::new(MAX_E2EE_CHUNK_BYTES));
        let first_connection_budget = Arc::new(WeightedByteBudget::new(MAX_E2EE_CHUNK_BYTES));
        let second_connection_budget = Arc::new(WeightedByteBudget::new(MAX_E2EE_CHUNK_BYTES));
        let first_frame = initiator_encrypt(
            &mut first_initiator,
            &[record(
                E2EE_RECORD_FLAG_CONTINUATION,
                &vec![0_u8; MAX_E2EE_CHUNK_BYTES],
            )],
        )
        .pop()
        .unwrap();
        let second_frame = initiator_encrypt(
            &mut second_initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"other connection")],
        )
        .pop()
        .unwrap();

        assert!(
            decrypt_inbound_frame_budgeted(
                &mut first_responder,
                &first_frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &first_connection_budget,
                None,
            )
            .await
            .unwrap()
            .is_none()
        );
        let waiting = tokio::spawn(async move {
            decrypt_inbound_frame_budgeted(
                &mut second_responder,
                &second_frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &second_connection_budget,
                None,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "global pressure must backpressure");

        drop(first_responder);
        assert!(
            waiting
                .await
                .expect("waiting decrypt task")
                .expect("capacity becomes available")
                .is_some()
        );
    }

    #[tokio::test]
    async fn principal_pressure_backpressures_a_second_connection_without_closing_it() {
        let (mut first_initiator, mut first_responder) = establish().await;
        let (mut second_initiator, mut second_responder) = establish().await;
        let held_bytes = b"held!".len();
        let global_budget = Arc::new(WeightedByteBudget::new(held_bytes * 2));
        let principal_budget = Arc::new(WeightedByteBudget::new(held_bytes));
        let first_connection_budget = Arc::new(WeightedByteBudget::new(held_bytes));
        let second_connection_budget = Arc::new(WeightedByteBudget::new(held_bytes));
        let first_frame = initiator_encrypt(
            &mut first_initiator,
            &[record(E2EE_RECORD_FLAG_CONTINUATION, b"held!")],
        )
        .pop()
        .unwrap();
        let second_frame = initiator_encrypt(
            &mut second_initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"other")],
        )
        .pop()
        .unwrap();

        assert!(
            decrypt_inbound_frame_budgeted(
                &mut first_responder,
                &first_frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                Some(&principal_budget),
                &first_connection_budget,
                None,
            )
            .await
            .unwrap()
            .is_none()
        );
        let waiting = tokio::spawn(async move {
            decrypt_inbound_frame_budgeted(
                &mut second_responder,
                &second_frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                Some(&principal_budget),
                &second_connection_budget,
                None,
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "principal pressure must backpressure instead of returning Protocol"
        );

        drop(first_responder);
        assert!(
            waiting
                .await
                .expect("waiting decrypt task")
                .expect("principal capacity becomes available")
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_admission_waits_under_transient_pressure_within_the_message_deadline() {
        let (mut initiator, mut responder) = establish().await;
        let frame = initiator_encrypt(
            &mut initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"blocked")],
        )
        .pop()
        .unwrap();
        let global_budget = Arc::new(WeightedByteBudget::new(b"blocked".len()));
        let held = Arc::clone(&global_budget)
            .try_acquire(b"blocked".len())
            .expect("hold global capacity");
        let connection_budget = Arc::new(WeightedByteBudget::new(b"blocked".len()));
        // The first record of a message carries the same absolute size-derived
        // deadline the pump derives: base timeout plus per-64 KiB allowance.
        let deadline = inbound_assembly_deadline(Instant::now(), b"blocked".len());
        let waiting = tokio::spawn(async move {
            decrypt_inbound_frame_budgeted(
                &mut responder,
                &frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &connection_budget,
                Some(deadline),
            )
            .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert!(
            !waiting.is_finished(),
            "shared capacity pressure has no flat five-second disconnect"
        );
        drop(held);
        assert!(
            waiting
                .await
                .expect("waiting decrypt task")
                .expect("global capacity becomes available")
                .is_some()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_admission_fails_at_the_absolute_message_deadline() {
        let (mut initiator, mut responder) = establish().await;
        let frame = initiator_encrypt(
            &mut initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"blocked")],
        )
        .pop()
        .unwrap();
        let global_budget = Arc::new(WeightedByteBudget::new(b"blocked".len()));
        let _held = Arc::clone(&global_budget)
            .try_acquire(b"blocked".len())
            .expect("hold global capacity");
        let connection_budget = Arc::new(WeightedByteBudget::new(b"blocked".len()));
        let deadline = inbound_assembly_deadline(Instant::now(), b"blocked".len());
        let waiting = tokio::spawn(async move {
            decrypt_inbound_frame_budgeted(
                &mut responder,
                &frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &connection_budget,
                Some(deadline),
            )
            .await
        });
        tokio::task::yield_now().await;
        // Base 5 s plus the one-second allowance for a sub-64 KiB record.
        tokio::time::advance(Duration::from_secs(7)).await;

        let result = timeout(Duration::from_secs(1), waiting)
            .await
            .expect("the wait is bounded by the absolute per-message deadline")
            .expect("waiting decrypt task");
        assert!(
            matches!(result, Err(E2eeSessionError::Timeout)),
            "capacity never freed: the message admission fails instead of \
             parking the pool indefinitely"
        );
    }

    #[tokio::test]
    async fn one_connection_cannot_monopolize_the_global_budget() {
        let (mut first_initiator, first_responder) = establish().await;
        let (mut second_initiator, mut second_responder) = establish().await;
        // Production ratio: the per-connection cap is half the global pool.
        let global_budget = Arc::new(WeightedByteBudget::new(MAX_E2EE_CHUNK_BYTES * 4));
        let first_connection_budget = Arc::new(WeightedByteBudget::new(MAX_E2EE_CHUNK_BYTES * 2));
        let second_connection_budget = Arc::new(WeightedByteBudget::new(MAX_E2EE_CHUNK_BYTES * 2));
        let continuation = record(
            E2EE_RECORD_FLAG_CONTINUATION,
            &vec![0_u8; MAX_E2EE_CHUNK_BYTES],
        );
        let first_frames = initiator_encrypt(
            &mut first_initiator,
            &[continuation.clone(), continuation.clone(), continuation],
        );
        let second_frame = initiator_encrypt(
            &mut second_initiator,
            &[record(E2EE_RECORD_FLAG_FINAL, b"second connection")],
        )
        .pop()
        .unwrap();

        // The first connection's third record exhausts its own tier and must
        // wait there — not fail the protocol — while the global pool still
        // holds capacity for other connections.
        let hog_global = Arc::clone(&global_budget);
        let hog_connection = Arc::clone(&first_connection_budget);
        let hog = tokio::spawn(async move {
            let mut responder = first_responder;
            for frame in &first_frames {
                decrypt_inbound_frame_budgeted(
                    &mut responder,
                    frame,
                    MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                    &hog_global,
                    None,
                    &hog_connection,
                    None,
                )
                .await
                .expect("continuation records are legal")
                .map(|_| ())
                .ok_or(())
                .expect_err("continuations never complete a message");
            }
        });
        tokio::task::yield_now().await;
        assert!(
            !hog.is_finished(),
            "the third record waits on the connection tier"
        );

        assert!(
            decrypt_inbound_frame_budgeted(
                &mut second_responder,
                &second_frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &second_connection_budget,
                None,
            )
            .await
            .unwrap()
            .is_some(),
            "the per-connection tier leaves global capacity for other connections"
        );
        hog.abort();
        let _ = hog.await;
    }

    #[tokio::test]
    async fn many_tiny_records_remain_within_the_byte_budget() {
        let (mut initiator, mut responder) = establish().await;
        let continuation_count = 1_026;
        let mut records = (0..continuation_count)
            .map(|_| record(E2EE_RECORD_FLAG_CONTINUATION, b"x"))
            .collect::<Vec<_>>();
        records.push(record(E2EE_RECORD_FLAG_FINAL, b"done"));
        let frames = initiator_encrypt(&mut initiator, &records);
        let message_bytes = continuation_count + b"done".len();
        let global_budget = Arc::new(WeightedByteBudget::new(message_bytes));
        let connection_budget = Arc::new(WeightedByteBudget::new(message_bytes));
        let mut completed = None;

        for frame in &frames {
            completed = decrypt_inbound_frame_budgeted(
                &mut responder,
                frame,
                MAX_E2EE_LOGICAL_MESSAGE_BYTES,
                &global_budget,
                None,
                &connection_budget,
                None,
            )
            .await
            .expect("legal tiny fragment");
        }

        let completed = completed.expect("final record completes message");
        assert_eq!(completed.plaintext.len(), message_bytes);
        assert!(completed.plaintext.ends_with(b"done"));
        // The accounting is byte-exact: the budgets sized to the plaintext are
        // fully consumed while the message is held, and fully refunded on drop.
        assert!(Arc::clone(&global_budget).try_acquire(1).is_none());
        assert!(Arc::clone(&connection_budget).try_acquire(1).is_none());
        drop(completed);
        assert!(
            Arc::clone(&global_budget)
                .try_acquire(message_bytes)
                .is_some()
        );
        assert!(
            Arc::clone(&connection_budget)
                .try_acquire(message_bytes)
                .is_some()
        );
    }

    #[test]
    fn established_connection_capacity_is_released_on_drop() {
        let budget = E2eeResourceBudget::new(1, 1, 1, 1, 1);
        let permit = budget
            .try_reserve()
            .expect("first connection")
            .bind_principal("principal")
            .expect("first principal connection");
        assert!(matches!(budget.try_reserve(), Err("protocol")));
        drop(permit);
        assert!(budget.try_reserve().is_ok());
    }

    #[tokio::test]
    async fn revoked_registration_releases_e2ee_capacity_before_socket_close() {
        let config = ServerConfig::new(".")
            .with_bind("127.0.0.1", 3773)
            .with_desktop("desktop-test-seed")
            .expect("desktop config");
        let auth = AuthService::new(&config, vec![7_u8; 32]);
        let issued = auth
            .exchange_bootstrap(
                "desktop-test-seed",
                None,
                e2ee_client_metadata(),
                None,
                SessionTransport::E2ee,
            )
            .await
            .expect("E2EE session");
        let principal = auth
            .authenticate_token(&issued.token, SessionTransport::E2ee)
            .await
            .expect("authentication completes before the race pause");
        let budget = E2eeResourceBudget::new(1, 1, 8, 4, 1);
        let permit = budget
            .try_reserve()
            .expect("established slot")
            .bind_principal(&principal.session_id)
            .expect("principal slot");
        let principal_inbound = permit.principal_inbound().expect("principal byte budget");
        let global_inbound = budget.global_inbound();
        let session_shutdown = CancellationToken::new();
        let (paused_tx, paused_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();

        let (registration, revocation) = tokio::join!(
            async {
                paused_tx
                    .send(())
                    .expect("signal pause after principal budget binding");
                resume_rx.await.expect("resume registration");
                register_e2ee_connection(
                    &auth,
                    &principal.session_id,
                    session_shutdown.clone(),
                    permit,
                )
                .await
            },
            async {
                paused_rx.await.expect("registration reached race pause");
                let revoked = auth
                    .revoke_client("other-session", &principal.session_id)
                    .await
                    .expect("revoke authenticated principal");
                resume_tx.send(()).expect("release registration");
                revoked
            },
        );

        assert!(revocation);
        assert!(matches!(
            registration,
            Err(crate::auth::AuthError::InvalidCredential)
        ));
        assert!(session_shutdown.is_cancelled());
        assert!(Arc::clone(&global_inbound).try_acquire(8).is_some());
        assert!(Arc::clone(&principal_inbound).try_acquire(4).is_some());
        assert!(
            budget
                .try_reserve()
                .expect("global established capacity is immediately reusable")
                .bind_principal(&principal.session_id)
                .is_ok(),
            "principal established capacity is immediately reusable"
        );
    }

    #[tokio::test]
    async fn tampered_frames_fail_closed() {
        let (mut initiator, mut responder) = establish().await;
        let mut frames = initiator_encrypt(&mut initiator, &[record(E2EE_RECORD_FLAG_FINAL, b"x")]);
        let last = frames[0].len() - 1;
        frames[0][last] ^= 0x01;
        assert!(matches!(
            responder.decrypt_frame(&frames[0], MAX_E2EE_LOGICAL_MESSAGE_BYTES),
            Err(E2eeSessionError::Crypto(_))
        ));
    }

    #[tokio::test]
    async fn non_empty_message_one_payload_is_a_protocol_violation() {
        let identity = HostIdentity::generate_ephemeral();
        let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
            .remote_public_key(identity.public_key_bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len_a = initiator.write_message(b"sneaky", &mut message_a).unwrap();
        message_a.truncate(len_a);
        let (to_responder, mut responder_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        to_responder.send(message_a).await.unwrap();
        let result = E2eeChannel::respond(
            &identity,
            async || responder_rx.recv().await,
            |_frame| async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(E2eeSessionError::Protocol(_))));
    }

    #[tokio::test]
    async fn wrong_pinned_key_fails_the_handshake() {
        let identity = HostIdentity::generate_ephemeral();
        let wrong = HostIdentity::generate_ephemeral();
        let mut initiator = snow::Builder::new(crate::auth::NOISE_NK_PARAMS.parse().unwrap())
            .remote_public_key(wrong.public_key_bytes())
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut message_a = vec![0_u8; MAX_E2EE_CIPHERTEXT_BYTES];
        let len_a = initiator.write_message(&[], &mut message_a).unwrap();
        message_a.truncate(len_a);
        let (to_responder, mut responder_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        to_responder.send(message_a).await.unwrap();
        let result = E2eeChannel::respond(
            &identity,
            async || responder_rx.recv().await,
            |_frame| async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(E2eeSessionError::Handshake)));
    }
}
