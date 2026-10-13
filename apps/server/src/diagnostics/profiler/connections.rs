//! RPC connection ends and reconnects: the `rpc.disconnect` and `rpc.reconnect` events.
//!
//! A client that keeps losing its connection shows up as a series of disconnects with their
//! reason, close code and how long each direction had been quiet, and as the reconnects that
//! follow. Payloads hold durations, counts, fixed reason names and the client's own profiler
//! id from its correlation header: no address, path, or message content.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{emit, is_enabled, latency::correlation_id};

/// Clients whose last disconnect is remembered to recognise their reconnect; past this, the
/// oldest is forgotten.
const MAX_REMEMBERED_CLIENTS: usize = 256;

static RECENT: LazyLock<Mutex<RecentDisconnects>> = LazyLock::new(Default::default);

/// How one connection ended, as `rpc.disconnect` records it.
pub(crate) struct Disconnect<'a> {
    pub(crate) reason: &'static str,
    pub(crate) close_code: Option<u16>,
    pub(crate) transport: &'static str,
    pub(crate) age: Duration,
    pub(crate) since_inbound: Duration,
    pub(crate) since_outbound: Duration,
    pub(crate) outbound_wait_max: Duration,
    pub(crate) queued_messages: usize,
    pub(crate) in_flight: usize,
    pub(crate) client_id: Option<&'a str>,
}

impl Disconnect<'_> {
    fn payload(&self) -> Value {
        json!({
            "reason": self.reason,
            "closeCode": self.close_code,
            "transport": self.transport,
            "ageMs": millis(self.age),
            "sinceInboundMs": millis(self.since_inbound),
            "sinceOutboundMs": millis(self.since_outbound),
            "outboundWaitMaxMs": millis(self.outbound_wait_max),
            "queuedMessages": self.queued_messages,
            "inFlight": self.in_flight,
            "clientId": self.client_id,
        })
    }
}

/// Records `rpc.disconnect` and remembers its client, so that client's next connection records
/// `rpc.reconnect`. Does nothing while the profiler is off.
pub(crate) fn record_disconnect(disconnect: &Disconnect<'_>) {
    if !is_enabled() {
        return;
    }
    if let Some(client_id) = disconnect.client_id {
        recent().remember(
            client_id,
            Previous {
                at: Instant::now(),
                reason: disconnect.reason,
                close_code: disconnect.close_code,
            },
        );
    }
    emit("rpc.disconnect", disconnect.payload());
}

/// The client a request names in its correlation header, `<clientId>:<n>`. `None`, with
/// nothing read, while the profiler is off.
pub(crate) fn request_client_id(headers: &[(String, String)]) -> Option<String> {
    if !is_enabled() {
        return None;
    }
    client_id(&correlation_id(headers)?)
}

fn client_id(correlation_id: &str) -> Option<String> {
    let client = correlation_id
        .split_once(':')
        .map_or(correlation_id, |(client, _)| client);
    (!client.is_empty()).then(|| client.to_owned())
}

/// Records `rpc.reconnect` when `client_id`'s previous connection ended in this process.
/// `connected_for` is how long the new connection has been open, so `afterMs` runs from the
/// previous end to this connection's start.
pub(crate) fn record_client_identified(
    client_id: &str,
    transport: &'static str,
    connected_for: Duration,
) {
    let Some(previous) = recent().take(client_id) else {
        return;
    };
    emit(
        "rpc.reconnect",
        json!({
            "clientId": client_id,
            "transport": transport,
            "afterMs": millis(previous.at.elapsed().saturating_sub(connected_for)),
            "previousReason": previous.reason,
            "previousCloseCode": previous.close_code,
        }),
    );
}

#[derive(Clone, Copy, Debug)]
struct Previous {
    at: Instant,
    reason: &'static str,
    close_code: Option<u16>,
}

#[derive(Default)]
struct RecentDisconnects {
    by_client: HashMap<String, Previous>,
}

impl RecentDisconnects {
    // ponytail: evicting scans all 256 entries; fine at one disconnect per connection.
    fn remember(&mut self, client_id: &str, previous: Previous) {
        if !self.by_client.contains_key(client_id)
            && self.by_client.len() >= MAX_REMEMBERED_CLIENTS
            && let Some(oldest) = self
                .by_client
                .iter()
                .min_by_key(|(_, previous)| previous.at)
                .map(|(id, _)| id.clone())
        {
            self.by_client.remove(&oldest);
        }
        self.by_client.insert(client_id.to_owned(), previous);
    }

    fn take(&mut self, client_id: &str) -> Option<Previous> {
        self.by_client.remove(client_id)
    }
}

fn recent() -> std::sync::MutexGuard<'static, RecentDisconnects> {
    RECENT.lock().unwrap_or_else(PoisonError::into_inner)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disconnect_records_its_reason_silences_and_client() {
        let payload = Disconnect {
            reason: "client-liveness-timeout",
            close_code: Some(4408),
            transport: "plain",
            age: Duration::from_secs(1800),
            since_inbound: Duration::from_millis(1_500),
            since_outbound: Duration::from_millis(600_250),
            outbound_wait_max: Duration::from_millis(12),
            queued_messages: 0,
            in_flight: 2,
            client_id: Some("19f84254a5cb6bb1"),
        }
        .payload();
        assert_eq!(
            payload,
            json!({
                "reason": "client-liveness-timeout",
                "closeCode": 4408,
                "transport": "plain",
                "ageMs": 1_800_000,
                "sinceInboundMs": 1_500,
                "sinceOutboundMs": 600_250,
                "outboundWaitMaxMs": 12,
                "queuedMessages": 0,
                "inFlight": 2,
                "clientId": "19f84254a5cb6bb1",
            })
        );
    }

    #[test]
    fn the_client_is_the_correlation_id_before_its_counter() {
        assert_eq!(client_id("567de34a:30569").as_deref(), Some("567de34a"));
        assert_eq!(client_id("567de34a").as_deref(), Some("567de34a"));
        assert_eq!(client_id(":7"), None);
    }

    #[test]
    fn a_reconnect_takes_the_clients_last_disconnect_once() {
        let mut recent = RecentDisconnects::default();
        let at = Instant::now();
        recent.remember(
            "c1",
            Previous {
                at,
                reason: "client-liveness-timeout",
                close_code: Some(4408),
            },
        );
        let previous = recent.take("c1").expect("the disconnect is remembered");
        assert_eq!(previous.reason, "client-liveness-timeout");
        assert_eq!(previous.close_code, Some(4408));
        assert!(recent.take("c1").is_none(), "a reconnect is reported once");
    }

    #[test]
    fn the_oldest_client_is_forgotten_past_the_limit() {
        let mut recent = RecentDisconnects::default();
        let start = Instant::now();
        for index in 0..=MAX_REMEMBERED_CLIENTS {
            recent.remember(
                &format!("c{index}"),
                Previous {
                    at: start + Duration::from_millis(u64::try_from(index).expect("index")),
                    reason: "client-gone",
                    close_code: None,
                },
            );
        }
        assert_eq!(recent.by_client.len(), MAX_REMEMBERED_CLIENTS);
        assert!(recent.take("c0").is_none(), "the oldest went first");
        assert!(recent.take(&format!("c{MAX_REMEMBERED_CLIENTS}")).is_some());
    }
}
