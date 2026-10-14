use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{Mutex, broadcast},
    time::Instant,
};
use url::Url;
use uuid::Uuid;

pub mod gateway;
pub mod tunnel;

/// How long an `openRequested` event stays claimable.
const OPEN_REQUEST_TTL: Duration = Duration::from_secs(60);
/// Unclaimed open requests one thread may hold; a command looping on `$BROWSER` cannot
/// flood the event channel past this.
pub const MAX_PENDING_OPEN_REQUESTS: usize = 64;
/// The contract's preview URL cap.
const MAX_URL_LENGTH: usize = 2048;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum PreviewViewportSetting {
    #[default]
    #[serde(rename = "fill")]
    Fill,
    #[serde(rename = "freeform")]
    Freeform { width: u32, height: u32 },
    #[serde(rename = "preset")]
    Preset {
        preset_id: String,
        width: u32,
        height: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum PreviewNavStatus {
    Idle,
    Loading {
        url: String,
        title: String,
    },
    Success {
        url: String,
        title: String,
    },
    LoadFailed {
        url: String,
        title: String,
        code: i32,
        description: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionSnapshot {
    pub thread_id: String,
    pub tab_id: String,
    pub nav_status: PreviewNavStatus,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub viewport: PreviewViewportSetting,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewListResult {
    pub sessions: Vec<PreviewSessionSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum PreviewEvent {
    #[serde(rename = "opened")]
    Opened {
        thread_id: String,
        tab_id: String,
        created_at: String,
        snapshot: PreviewSessionSnapshot,
    },
    #[serde(rename = "navigated")]
    Navigated {
        thread_id: String,
        tab_id: String,
        created_at: String,
        snapshot: PreviewSessionSnapshot,
    },
    #[serde(rename = "resized")]
    Resized {
        thread_id: String,
        tab_id: String,
        created_at: String,
        snapshot: PreviewSessionSnapshot,
    },
    #[serde(rename = "failed")]
    Failed {
        thread_id: String,
        tab_id: String,
        created_at: String,
        url: String,
        title: String,
        code: i32,
        description: String,
    },
    #[serde(rename = "closed")]
    Closed {
        thread_id: String,
        tab_id: String,
        created_at: String,
    },
    /// A command in the thread asked to open `url`; the first client to claim `request_id`
    /// opens it.
    #[serde(rename = "openRequested")]
    OpenRequested {
        thread_id: String,
        request_id: String,
        url: String,
        created_at: String,
    },
}

impl PreviewEvent {
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Opened { .. } => "opened",
            Self::Navigated { .. } => "navigated",
            Self::Resized { .. } => "resized",
            Self::Failed { .. } => "failed",
            Self::Closed { .. } => "closed",
            Self::OpenRequested { .. } => "openRequested",
        }
    }

    #[must_use]
    pub fn tab_id(&self) -> Option<&str> {
        match self {
            Self::Opened { tab_id, .. }
            | Self::Navigated { tab_id, .. }
            | Self::Resized { tab_id, .. }
            | Self::Failed { tab_id, .. }
            | Self::Closed { tab_id, .. } => Some(tab_id),
            Self::OpenRequested { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum PreviewError {
    #[error("Unknown preview session: thread={thread_id}, tab={tab_id}")]
    SessionLookup { thread_id: String, tab_id: String },
    #[error("Invalid preview URL ({reason}; input length {input_length}).")]
    InvalidUrl {
        input_length: usize,
        reason: &'static str,
        protocol: Option<String>,
    },
}

#[derive(Debug, Error)]
pub enum OpenRequestError {
    #[error(transparent)]
    InvalidUrl(#[from] PreviewError),
    #[error("This thread already has {MAX_PENDING_OPEN_REQUESTS} unclaimed open requests.")]
    TooManyPending,
}

/// An announced open request. `delivered` is whether any client was subscribed to preview
/// events when it was announced; when none was, nobody will claim it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRequestReceipt {
    pub request_id: String,
    pub delivered: bool,
}

#[derive(Default)]
struct PreviewState {
    sessions: BTreeMap<String, PreviewSessionSnapshot>,
    /// Unclaimed open requests by id: their thread and expiry.
    open_requests: HashMap<String, (String, Instant)>,
}

impl PreviewState {
    fn prune_open_requests(&mut self, now: Instant) {
        self.open_requests
            .retain(|_, (_, expires_at)| *expires_at > now);
    }
}

#[derive(Clone)]
pub struct PreviewManager {
    state: Arc<Mutex<PreviewState>>,
    events: broadcast::Sender<PreviewEvent>,
    /// Receivers of `events` held by the server itself, which never claim open requests.
    internal_subscribers: Arc<AtomicUsize>,
}

/// A preview event subscription held by the server itself (the gateway's tab follower). It
/// does not count as a client when an open request's delivery is decided.
pub struct InternalPreviewEvents {
    pub receiver: broadcast::Receiver<PreviewEvent>,
    count: Arc<AtomicUsize>,
}

impl Drop for InternalPreviewEvents {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Default for PreviewManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewManager {
    #[must_use]
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            state: Arc::new(Mutex::new(PreviewState::default())),
            events,
            internal_subscribers: Arc::default(),
        }
    }

    /// A client subscription: it counts toward open-request delivery.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<PreviewEvent> {
        self.events.subscribe()
    }

    /// A subscription for the server's own observers; see [`InternalPreviewEvents`].
    #[must_use]
    pub fn subscribe_internal_events(&self) -> InternalPreviewEvents {
        self.internal_subscribers.fetch_add(1, Ordering::Relaxed);
        InternalPreviewEvents {
            receiver: self.events.subscribe(),
            count: self.internal_subscribers.clone(),
        }
    }

    pub async fn open(
        &self,
        thread_id: &str,
        url: Option<&str>,
    ) -> Result<PreviewSessionSnapshot, PreviewError> {
        let tab_id = format!("tab_{}", Uuid::new_v4().simple());
        let updated_at = now_iso();
        let snapshot = match url {
            Some(raw) => PreviewSessionSnapshot {
                thread_id: thread_id.to_owned(),
                tab_id: tab_id.clone(),
                nav_status: PreviewNavStatus::Loading {
                    url: normalize_url(raw)?,
                    title: String::new(),
                },
                can_go_back: false,
                can_go_forward: false,
                viewport: PreviewViewportSetting::Fill,
                updated_at: updated_at.clone(),
            },
            None => PreviewSessionSnapshot {
                thread_id: thread_id.to_owned(),
                tab_id: tab_id.clone(),
                nav_status: PreviewNavStatus::Idle,
                can_go_back: false,
                can_go_forward: false,
                viewport: PreviewViewportSetting::Fill,
                updated_at: updated_at.clone(),
            },
        };
        let key = composite_key(thread_id, &tab_id);
        self.state
            .lock()
            .await
            .sessions
            .insert(key, snapshot.clone());
        let _ = self.events.send(PreviewEvent::Opened {
            thread_id: thread_id.to_owned(),
            tab_id,
            created_at: updated_at,
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    pub async fn navigate(
        &self,
        thread_id: &str,
        tab_id: &str,
        url: &str,
        resolved_title: Option<&str>,
    ) -> Result<PreviewSessionSnapshot, PreviewError> {
        let url = normalize_url(url)?;
        let mut state = self.state.lock().await;
        let key = composite_key(thread_id, tab_id);
        let current =
            state
                .sessions
                .get(&key)
                .cloned()
                .ok_or_else(|| PreviewError::SessionLookup {
                    thread_id: thread_id.to_owned(),
                    tab_id: tab_id.to_owned(),
                })?;
        let previous_title = match current.nav_status {
            PreviewNavStatus::Idle => String::new(),
            PreviewNavStatus::Loading { ref title, .. }
            | PreviewNavStatus::Success { ref title, .. }
            | PreviewNavStatus::LoadFailed { ref title, .. } => title.clone(),
        };
        let updated = PreviewSessionSnapshot {
            thread_id: thread_id.to_owned(),
            tab_id: tab_id.to_owned(),
            nav_status: PreviewNavStatus::Success {
                url,
                title: resolved_title.unwrap_or(previous_title.as_str()).to_owned(),
            },
            can_go_back: current.can_go_back,
            can_go_forward: current.can_go_forward,
            viewport: current.viewport,
            updated_at: now_iso(),
        };
        state.sessions.insert(key, updated.clone());
        let _ = self.events.send(PreviewEvent::Navigated {
            thread_id: thread_id.to_owned(),
            tab_id: tab_id.to_owned(),
            created_at: updated.updated_at.clone(),
            snapshot: updated.clone(),
        });
        Ok(updated)
    }

    pub async fn report_status(
        &self,
        thread_id: &str,
        tab_id: &str,
        nav_status: PreviewNavStatus,
        can_go_back: bool,
        can_go_forward: bool,
    ) -> Result<(), PreviewError> {
        let mut state = self.state.lock().await;
        let key = composite_key(thread_id, tab_id);
        let current =
            state
                .sessions
                .get(&key)
                .cloned()
                .ok_or_else(|| PreviewError::SessionLookup {
                    thread_id: thread_id.to_owned(),
                    tab_id: tab_id.to_owned(),
                })?;
        let updated = PreviewSessionSnapshot {
            thread_id: thread_id.to_owned(),
            tab_id: tab_id.to_owned(),
            nav_status: nav_status.clone(),
            can_go_back,
            can_go_forward,
            viewport: current.viewport,
            updated_at: now_iso(),
        };
        state.sessions.insert(key, updated.clone());
        let event = match nav_status {
            PreviewNavStatus::LoadFailed {
                url,
                title,
                code,
                description,
            } => PreviewEvent::Failed {
                thread_id: thread_id.to_owned(),
                tab_id: tab_id.to_owned(),
                created_at: updated.updated_at.clone(),
                url,
                title,
                code,
                description,
            },
            _ => PreviewEvent::Navigated {
                thread_id: thread_id.to_owned(),
                tab_id: tab_id.to_owned(),
                created_at: updated.updated_at.clone(),
                snapshot: updated,
            },
        };
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn resize(
        &self,
        thread_id: &str,
        tab_id: &str,
        viewport: PreviewViewportSetting,
    ) -> Result<PreviewSessionSnapshot, PreviewError> {
        let mut state = self.state.lock().await;
        let key = composite_key(thread_id, tab_id);
        let current =
            state
                .sessions
                .get(&key)
                .cloned()
                .ok_or_else(|| PreviewError::SessionLookup {
                    thread_id: thread_id.to_owned(),
                    tab_id: tab_id.to_owned(),
                })?;
        let updated = PreviewSessionSnapshot {
            viewport,
            updated_at: now_iso(),
            ..current
        };
        state.sessions.insert(key, updated.clone());
        let _ = self.events.send(PreviewEvent::Resized {
            thread_id: thread_id.to_owned(),
            tab_id: tab_id.to_owned(),
            created_at: updated.updated_at.clone(),
            snapshot: updated.clone(),
        });
        Ok(updated)
    }

    pub async fn refresh(&self, thread_id: &str, tab_id: &str) -> Result<(), PreviewError> {
        let state = self.state.lock().await;
        let key = composite_key(thread_id, tab_id);
        if state.sessions.contains_key(&key) {
            Ok(())
        } else {
            Err(PreviewError::SessionLookup {
                thread_id: thread_id.to_owned(),
                tab_id: tab_id.to_owned(),
            })
        }
    }

    pub async fn close(&self, thread_id: &str, tab_id: Option<&str>) -> Result<(), PreviewError> {
        let mut state = self.state.lock().await;
        let targets = match tab_id {
            Some(tab_id) => state
                .sessions
                .keys()
                .filter(|key| *key == &composite_key(thread_id, tab_id))
                .cloned()
                .collect::<Vec<_>>(),
            None => state
                .sessions
                .keys()
                .filter(|key| key.starts_with(&format!("{thread_id}\u{0}")))
                .cloned()
                .collect::<Vec<_>>(),
        };
        let created_at = now_iso();
        for key in targets {
            if let Some(snapshot) = state.sessions.remove(&key) {
                let _ = self.events.send(PreviewEvent::Closed {
                    thread_id: snapshot.thread_id,
                    tab_id: snapshot.tab_id,
                    created_at: created_at.clone(),
                });
            }
        }
        Ok(())
    }

    /// Announces that a command in `thread_id` wants `url` opened. Returns the request id that
    /// clients race to claim with [`Self::claim_open_request`], and whether any client could
    /// see it.
    pub async fn request_open(
        &self,
        thread_id: &str,
        url: &str,
    ) -> Result<OpenRequestReceipt, OpenRequestError> {
        let input_length = url.len();
        let url = normalize_url(url)?;
        // The caller is a command, not a validated client, so enforce the contract's URL cap
        // here: a longer URL would make every subscriber fail to decode the event.
        if url.len() > MAX_URL_LENGTH {
            return Err(PreviewError::InvalidUrl {
                input_length,
                reason: "too-long",
                protocol: None,
            }
            .into());
        }
        let request_id = format!("open_{}", Uuid::new_v4().simple());
        let now = Instant::now();
        {
            let mut state = self.state.lock().await;
            state.prune_open_requests(now);
            let pending = state
                .open_requests
                .values()
                .filter(|(thread, _)| thread == thread_id)
                .count();
            if pending >= MAX_PENDING_OPEN_REQUESTS {
                return Err(OpenRequestError::TooManyPending);
            }
            state.open_requests.insert(
                request_id.clone(),
                (thread_id.to_owned(), now + OPEN_REQUEST_TTL),
            );
        }
        let delivered =
            self.events.receiver_count() > self.internal_subscribers.load(Ordering::Relaxed);
        let _ = self.events.send(PreviewEvent::OpenRequested {
            thread_id: thread_id.to_owned(),
            request_id: request_id.clone(),
            url,
            created_at: now_iso(),
        });
        Ok(OpenRequestReceipt {
            request_id,
            delivered,
        })
    }

    /// Claims an open request. Only the first claim of a live request returns `true`.
    pub async fn claim_open_request(&self, request_id: &str) -> bool {
        let mut state = self.state.lock().await;
        state.prune_open_requests(Instant::now());
        state.open_requests.remove(request_id).is_some()
    }

    pub async fn list(&self, thread_id: &str) -> PreviewListResult {
        let state = self.state.lock().await;
        let mut sessions = state
            .sessions
            .values()
            .filter(|snapshot| snapshot.thread_id == thread_id)
            .cloned()
            .collect::<Vec<_>>();
        // RFC 3339 strings with different fractional digits do not sort as text.
        sessions.sort_by_cached_key(|snapshot| {
            OffsetDateTime::parse(&snapshot.updated_at, &Rfc3339).ok()
        });
        PreviewListResult { sessions }
    }
}

fn composite_key(thread_id: &str, tab_id: &str) -> String {
    format!("{thread_id}\u{0}{tab_id}")
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

fn normalize_url(raw: &str) -> Result<String, PreviewError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PreviewError::InvalidUrl {
            input_length: raw.len(),
            reason: "empty",
            protocol: None,
        });
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else if is_loopback_host(trimmed) {
        format!("http://{trimmed}")
    } else {
        format!("https://{trimmed}")
    };

    let protocol = candidate
        .split_once(':')
        .map(|(scheme, _)| format!("{scheme}:"));
    let parsed = Url::parse(&candidate).map_err(|_| PreviewError::InvalidUrl {
        input_length: raw.len(),
        reason: "parse",
        protocol: protocol.clone(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(PreviewError::InvalidUrl {
            input_length: raw.len(),
            reason: "unsupported-protocol",
            protocol,
        });
    }
    Ok(parsed.to_string())
}

fn is_loopback_host(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "0.0.0.0", "[::1]", "[::]"]
        .into_iter()
        .any(|host| {
            lower == host
                || lower
                    .strip_prefix(host)
                    .is_some_and(|suffix| suffix.starts_with(':') || suffix.starts_with('/'))
        })
}

#[cfg(test)]
mod normalization_tests {
    use super::*;

    #[test]
    fn bare_loopback_ports_use_http() {
        assert_eq!(
            normalize_url("localhost:4173").expect("localhost URL"),
            "http://localhost:4173/"
        );
        assert_eq!(
            normalize_url("0.0.0.0:8080/path").expect("wildcard loopback URL"),
            "http://0.0.0.0:8080/path"
        );
    }

    #[tokio::test]
    async fn list_sorts_by_instant_not_text() {
        let manager = PreviewManager::new();
        let earlier = manager.open("t", None).await.unwrap();
        let later = manager.open("t", None).await.unwrap();
        {
            let mut state = manager.state.lock().await;
            // As text `.5Z` sorts before `Z`, though it is half a second later.
            for (tab, stamp) in [
                (&earlier, "2026-10-08T00:00:01Z"),
                (&later, "2026-10-08T00:00:01.5Z"),
            ] {
                let key = composite_key("t", &tab.tab_id);
                state.sessions.get_mut(&key).unwrap().updated_at = stamp.to_owned();
            }
        }
        let listed = manager.list("t").await.sessions;
        assert_eq!(
            listed.iter().map(|tab| &tab.tab_id).collect::<Vec<_>>(),
            [&earlier.tab_id, &later.tab_id]
        );
    }

    #[test]
    fn qualified_unsupported_protocol_is_rejected() {
        assert!(matches!(
            normalize_url("file:///tmp/index.html"),
            Err(PreviewError::InvalidUrl {
                reason: "unsupported-protocol",
                ..
            })
        ));
    }
}
