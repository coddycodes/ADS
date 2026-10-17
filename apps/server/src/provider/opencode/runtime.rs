use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

use crate::activity::{
    ActivityCapabilities, ActivityHistoryRecovery, ActivityObservationState, ActivitySection,
    ActivitySectionHealth, ProviderActivityMutation,
};

use crate::provider::{
    OptionRefusal, RefusedOption, option_needs_value_refusal, option_on_or_off_refusal,
    option_without_id_refusal, options_conflict_refusal, options_need_model_refusal,
    unsupported_option_refusal,
};

use super::{
    activity::{MAX_LINEAGE_DEPTH, OpenCodeActivityTracker, TEXT_COALESCE_MS, valid_key},
    model::{
        FAST_MODE_LABEL, OpenCodeChildSessionDto, OpenCodeMessageDto, OpenCodeSessionStatusDto,
        OpenCodeStatusMapDto, VARIANT_LABEL, infer_default_variant, merge_assistant_text,
        parse_model_slug,
    },
    sse::OpenCodeSseDecoder,
    v2::{self, FormField, V2Output, V2TurnTranslator},
    v2_activity::{self, V2ActivityNormalizer},
};

const PROVIDER: &str = "opencode";
const FIXED_EVENT_TIME: &str = "2026-07-10T00:00:00.000Z";
const RECONCILIATION_HINT_CAPACITY: usize = 1;
const RECONCILIATION_DEBOUNCE: Duration = Duration::from_millis(250);
const RECONCILIATION_DEFERRED_DELAY: Duration = Duration::from_millis(10);
const RECONCILIATION_TIMEOUT: Duration = Duration::from_secs(5);
/// Bounds each request of an OpenCode 2 rewind, which the provider supervisor awaits.
const REWIND_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds the whole history scan that finds a rewind's target, however many pages it reads.
const REWIND_SCAN_TIMEOUT: Duration = Duration::from_secs(30);
/// Shown while a rewind's outcome is unknown; reverting again settles it.
const UNFINISHED_REVERT: &str =
    "A checkpoint revert didn't finish. Revert to that checkpoint again to finish it.";
/// OpenCode 2's largest message-list page.
const MESSAGE_PAGE_LIMIT: usize = 200;
/// Bounds the OpenCode 2 skill and command reads that route a slash invocation.
const SLASH_CATALOG_TIMEOUT: Duration = Duration::from_secs(5);
const RECONCILIATION_RETRY_MIN: Duration = Duration::from_millis(100);
const RECONCILIATION_RETRY_MAX: Duration = Duration::from_secs(2);
const RECONCILIATION_CHILD_LIMIT: usize = 50;
const RECONCILIATION_HISTORY_LIMIT: usize = 200;
const RECONCILIATION_MUTATION_LIMIT: usize = 256;
const RECONCILIATION_SCOPE_MUTATION_COUNT: usize = 3;
const RECONCILIATION_RECORD_MUTATION_LIMIT: usize =
    RECONCILIATION_MUTATION_LIMIT - RECONCILIATION_SCOPE_MUTATION_COUNT;
const RECONCILIATION_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const RECONCILIATION_HISTORY_SLICE_BUDGET: usize = 4;
const ACTIVITY_TEXT_COALESCE_DELAY: Duration = Duration::from_millis(TEXT_COALESCE_MS);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeSessionSnapshot {
    pub thread_id: String,
    pub turns: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeRuntimeEvent {
    pub event_id: String,
    pub provider: String,
    pub created_at: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_event_id: Option<String>,
    #[serde(skip)]
    pub activity: Vec<ProviderActivityMutation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeRuntimeEventStableView {
    #[serde(rename = "type")]
    pub event_type: String,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub payload: Value,
}

impl OpenCodeRuntimeEvent {
    #[must_use]
    pub fn stable_view(&self) -> OpenCodeRuntimeEventStableView {
        OpenCodeRuntimeEventStableView {
            event_type: self.event_type.clone(),
            thread_id: self.thread_id.clone(),
            turn_id: self.turn_id.clone(),
            item_id: self.item_id.clone(),
            request_id: self.request_id.clone(),
            payload: self.payload.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum OpenCodeRuntimeError {
    #[error("OpenCode HTTP request failed: {0}")]
    Http(String),
    #[error("OpenCode response was invalid: {0}")]
    InvalidResponse(String),
    /// The session refuses a turn's options: their shape, or a value the selected model does not
    /// advertise. The same request is refused again on every retry.
    #[error("{0}")]
    UnsupportedOption(RefusedOption),
    #[error("Unknown pending question id {0}")]
    UnknownQuestion(String),
    #[error("Unknown pending permission id {0}")]
    UnknownPermission(String),
    #[error("Session is not started")]
    MissingSession,
    /// A request failed before the turn itself was submitted, so it was not sent.
    #[error("OpenCode request failed before the turn was sent: {0}")]
    NotSent(String),
}

async fn within_rewind_deadline<T>(
    request: impl std::future::Future<Output = Result<T, OpenCodeRuntimeError>>,
) -> Result<T, OpenCodeRuntimeError> {
    tokio::time::timeout(REWIND_REQUEST_TIMEOUT, request)
        .await
        .unwrap_or_else(|_| {
            Err(OpenCodeRuntimeError::Http(
                "OpenCode did not answer in time".to_owned(),
            ))
        })
}

/// A failed HTTP request made before the turn's own request did not send the turn.
fn not_sent(error: OpenCodeRuntimeError) -> OpenCodeRuntimeError {
    match error {
        OpenCodeRuntimeError::Http(detail) => OpenCodeRuntimeError::NotSent(detail),
        error => error,
    }
}

impl OptionRefusal for OpenCodeRuntimeError {
    fn refused_option(&self) -> Option<&RefusedOption> {
        match self {
            Self::UnsupportedOption(refused) => Some(refused),
            _ => None,
        }
    }
}

impl OpenCodeRuntimeError {
    fn refused(detail: impl Into<String>, refusal: impl Into<String>) -> Self {
        Self::UnsupportedOption(RefusedOption::new(detail, refusal))
    }
}

#[derive(Clone)]
pub struct OpenCodeSessionRuntime {
    inner: Arc<RuntimeInner>,
    _external_owner: Option<Arc<RuntimeOwnerLiveness>>,
}

struct RuntimeInner {
    client: reqwest::Client,
    base_url: String,
    thread_id: String,
    directory: String,
    model: Mutex<Option<(String, String)>>,
    selected_variant: Mutex<Option<String>>,
    agent: Option<String>,
    runtime_mode: Mutex<String>,
    session_id: Mutex<Option<String>>,
    active_turn_id: Mutex<Option<String>>,
    active_user_message_id: Mutex<Option<String>>,
    assistant_message_ids: Mutex<HashSet<String>>,
    assistant_text: Mutex<HashMap<String, String>>,
    events_tx: mpsc::UnboundedSender<OpenCodeRuntimeEvent>,
    events_rx: Mutex<mpsc::UnboundedReceiver<OpenCodeRuntimeEvent>>,
    event_stream_ended: CancellationToken,
    event_counter: Mutex<u64>,
    pending_questions: Mutex<HashMap<String, PendingQuestion>>,
    pending_permissions: Mutex<HashMap<String, Option<String>>>,
    event_pump: Mutex<Option<JoinHandle<()>>>,
    reconciliation_hint_tx: mpsc::Sender<()>,
    reconciliation_requests: StdMutex<ReconciliationRequestState>,
    reconciliation_task: StdMutex<Option<JoinHandle<()>>>,
    reconciliation_cancellation: CancellationToken,
    activity_flush_generation: AtomicU64,
    activity_flush_task: StdMutex<Option<ActivityFlushTask>>,
    activity_flush_gate: Mutex<()>,
    owner_state: Arc<RuntimeOwnerState>,
    activity: Mutex<OpenCodeRuntimeActivityState>,
    /// The server speaks the OpenCode 2 `/api/*` wire. Set once, before `start` or `resume`.
    protocol_v2: AtomicBool,
    v2: Mutex<V2State>,
    /// Serializes OpenCode 2 rewinds with the settling a prompt does first, so each unsettled
    /// revert is accounted for once.
    v2_rewind: Mutex<()>,
}

/// OpenCode 2 session state: the root-turn translator, the asks waiting for an answer (keyed by
/// request or form id, with the turn and the session that asked), and the model and agent last
/// sent to the session.
#[derive(Default)]
struct V2State {
    translator: Option<V2TurnTranslator>,
    normalizer: V2ActivityNormalizer,
    permissions: HashMap<String, (Option<String>, String)>,
    forms: HashMap<String, V2PendingForm>,
    /// Asks BiBCode is replying to: OpenCode's own settlement of them is left to the reply, so
    /// the resolution carries BiBCode's decision and is reported once.
    answering: HashSet<String>,
    /// Asks OpenCode settled while BiBCode's reply was in flight: the reply succeeded even if its
    /// HTTP response was lost.
    settled_while_answering: HashSet<String>,
    synced_model: Option<(String, String, Option<String>)>,
    synced_agent: bool,
    /// The runtime mode whose permission rules the session has.
    synced_runtime_mode: Option<String>,
    /// A resumed session checks for a running execution once its event stream is connected, so
    /// the execution's end cannot slip between the check and the subscription.
    adopt_on_connect: bool,
    /// The progress of a checkpoint rewind that has not reported success, counted from BiBCode's
    /// unchanged transcript. See `v2_rollback`.
    rewind: V2RewindProgress,
    /// The last rewind that reported success, as kept with the session: a retry of it (same key,
    /// from a BiBCode transcript that did not record its completion) drops nothing more.
    finished_rewind: Option<V2RewindProgress>,
}

#[derive(Clone, Default, Eq, PartialEq)]
struct V2RewindProgress {
    /// The checkpoint revert's idempotency key: the BiBCode transcript the turns are counted
    /// from. A rewind with another key starts afresh.
    key: Option<String>,
    /// Turns OpenCode is known to have dropped.
    dropped: usize,
    /// A revert whose outcome is unknown: the turns it drops and the user message it staged at.
    unsettled: Option<(usize, String)>,
    /// The rewind reported success.
    finished: bool,
}

impl V2RewindProgress {
    fn for_key(key: &str) -> Self {
        Self {
            key: Some(key.to_owned()),
            ..Self::default()
        }
    }

    /// Turns dropped, or a revert in doubt, that BiBCode's transcript does not show yet.
    fn is_unfinished(&self) -> bool {
        !self.finished && (self.dropped > 0 || self.unsettled.is_some())
    }

    /// The progress as kept in the OpenCode 2 session's metadata (`bibcode.rewind`).
    fn to_json(&self) -> Value {
        let mut record = json!({ "key": self.key, "dropped": self.dropped });
        if let Some((turns, target)) = &self.unsettled {
            record["unsettled"] = json!({ "turns": turns, "target": target });
        }
        if self.finished {
            record["finished"] = json!(true);
        }
        record
    }

    fn from_json(record: &Value) -> Option<Self> {
        let dropped = usize::try_from(record.get("dropped")?.as_u64()?).ok()?;
        let unsettled = match record.get("unsettled") {
            None | Some(Value::Null) => None,
            Some(unsettled) => Some((
                usize::try_from(unsettled.get("turns")?.as_u64()?).ok()?,
                unsettled.get("target")?.as_str()?.to_owned(),
            )),
        };
        Some(Self {
            key: record.get("key").and_then(Value::as_str).map(str::to_owned),
            dropped,
            unsettled,
            finished: record.get("finished").and_then(Value::as_bool) == Some(true),
        })
    }
}

struct V2PendingForm {
    turn_id: Option<String>,
    session_id: String,
    fields: Vec<FormField>,
}

struct RuntimeOwnerState {
    active: StdMutex<bool>,
    cancellation: CancellationToken,
}

impl RuntimeOwnerState {
    fn new() -> Self {
        Self {
            active: StdMutex::new(true),
            cancellation: CancellationToken::new(),
        }
    }

    fn is_active(&self) -> bool {
        *self
            .active
            .lock()
            .expect("OpenCode runtime owner-state mutex poisoned")
    }

    fn deactivate(&self) {
        let mut active = self
            .active
            .lock()
            .expect("OpenCode runtime owner-state mutex poisoned");
        if *active {
            *active = false;
            self.cancellation.cancel();
        }
    }
}

struct RuntimeOwnerLiveness {
    state: Arc<RuntimeOwnerState>,
}

impl Drop for RuntimeOwnerLiveness {
    fn drop(&mut self) {
        self.state.deactivate();
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.reconciliation_cancellation.cancel();
        let task = self
            .activity_flush_task
            .get_mut()
            .expect("OpenCode activity flush task mutex poisoned")
            .take();
        if let Some(task) = task {
            task.cancellation.cancel();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = task.handle.await;
                });
            } else {
                task.handle.abort();
            }
        }
    }
}

#[derive(Clone)]
struct PendingQuestion {
    turn_id: Option<String>,
    questions: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointSupport {
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OpenCodeEventRoute {
    Root,
    VerifiedChild,
    Foreign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PayloadSessionIdentity<'a> {
    Agreed(&'a str),
    Missing,
    Conflicting,
}

struct ActivityFlushTask {
    cancellation: CancellationToken,
    handle: JoinHandle<()>,
}

#[derive(Clone, Debug)]
struct ReconciliationHint {
    immediate: bool,
    force_history: bool,
    dirty_session_ids: HashSet<String>,
}

impl ReconciliationHint {
    fn consume_accepted_snapshot(&mut self) {
        self.force_history = false;
        self.dirty_session_ids.clear();
    }
}

struct ReconciliationRequestState {
    pending_hint: Option<ReconciliationHint>,
    identity: ReconciliationIdentityState,
}

struct ReconciliationIdentityState {
    activity_head: [u8; 32],
    last_reconciliation_fingerprint: Option<[u8; 32]>,
    last_reconciliation_occurrence: Option<[u8; 32]>,
    last_reconciliation_activity_head: Option<[u8; 32]>,
}

impl ReconciliationIdentityState {
    fn new(thread_id: &str, revision: u64) -> Self {
        Self {
            activity_head: initial_reconciliation_causal_head(thread_id, revision),
            last_reconciliation_fingerprint: None,
            last_reconciliation_occurrence: None,
            last_reconciliation_activity_head: None,
        }
    }

    fn reconciliation_occurrence(&mut self, activity: &[ProviderActivityMutation]) -> [u8; 32] {
        let fingerprint = activity_batch_fingerprint(activity);
        if self.last_reconciliation_fingerprint == Some(fingerprint)
            && self.last_reconciliation_activity_head == Some(self.activity_head)
        {
            return self
                .last_reconciliation_occurrence
                .expect("matching reconciliation fingerprint has an occurrence");
        }

        let occurrence = next_reconciliation_causal_occurrence(&self.activity_head, &fingerprint);
        self.activity_head =
            next_activity_causal_head(&self.activity_head, b"reconciliation", &fingerprint);
        self.last_reconciliation_fingerprint = Some(fingerprint);
        self.last_reconciliation_occurrence = Some(occurrence);
        self.last_reconciliation_activity_head = Some(self.activity_head);
        occurrence
    }

    fn observe_activity(&mut self, activity: &[ProviderActivityMutation]) {
        let fingerprint = activity_batch_fingerprint(activity);
        self.activity_head =
            next_activity_causal_head(&self.activity_head, b"provider-event", &fingerprint);
    }
}

#[derive(Clone, Debug, Default)]
struct ReconciliationHistoryCursor {
    signature: String,
    message_index: usize,
    part_index: usize,
}

struct OpenCodeRuntimeActivityState {
    agent_activity_enabled: bool,
    generation: u64,
    reconciliation_pass_cancellation: CancellationToken,
    tracker: Option<OpenCodeActivityTracker>,
    root_session_id: Option<String>,
    child_support: EndpointSupport,
    status_support: EndpointSupport,
    history_support: EndpointSupport,
    history_signatures: HashMap<String, String>,
    history_cursors: HashMap<String, ReconciliationHistoryCursor>,
    capabilities: ActivityCapabilities,
}

enum ReconciliationPassResult {
    Success,
    Deferred,
    Retry,
}

enum ReconciliationApiResult<T> {
    Supported(T),
    NotFound,
}

struct ReconciliationSnapshot {
    child_batches: Vec<(String, Vec<OpenCodeChildSessionDto>)>,
    statuses: OpenCodeStatusMapDto,
    histories: Vec<(String, Vec<OpenCodeMessageDto>, String)>,
    child_support: EndpointSupport,
    status_support: EndpointSupport,
    history_support: EndpointSupport,
}

impl OpenCodeSessionRuntime {
    pub fn new(base_url: &str, thread_id: &str, directory: &str, model: Option<&str>) -> Self {
        Self::new_with_password(base_url, thread_id, directory, model, None)
            .expect("OpenCode client without credentials must be valid")
    }

    pub fn new_with_password(
        base_url: &str,
        thread_id: &str,
        directory: &str,
        model: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, OpenCodeRuntimeError> {
        Self::new_with_options(base_url, thread_id, directory, model, password, None)
    }

    pub fn new_with_options(
        base_url: &str,
        thread_id: &str,
        directory: &str,
        model: Option<&str>,
        password: Option<&str>,
        agent: Option<&str>,
    ) -> Result<Self, OpenCodeRuntimeError> {
        Self::new_with_options_and_reconciliation_revision(
            base_url, thread_id, directory, model, password, agent, 0,
        )
    }

    /// Creates a runtime whose reconciliation identities continue from a durable activity scope.
    #[must_use]
    pub fn new_with_reconciliation_revision(
        base_url: &str,
        thread_id: &str,
        directory: &str,
        model: Option<&str>,
        reconciliation_revision: u64,
    ) -> Self {
        Self::new_with_options_and_reconciliation_revision(
            base_url,
            thread_id,
            directory,
            model,
            None,
            None,
            reconciliation_revision,
        )
        .expect("OpenCode client without credentials must be valid")
    }

    pub(crate) fn new_with_options_and_reconciliation_revision(
        base_url: &str,
        thread_id: &str,
        directory: &str,
        model: Option<&str>,
        password: Option<&str>,
        agent: Option<&str>,
        reconciliation_revision: u64,
    ) -> Result<Self, OpenCodeRuntimeError> {
        Self::new_with_options_reconciliation_revision_and_agent_activity(
            base_url,
            thread_id,
            directory,
            model,
            password,
            agent,
            reconciliation_revision,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_options_reconciliation_revision_and_agent_activity(
        base_url: &str,
        thread_id: &str,
        directory: &str,
        model: Option<&str>,
        password: Option<&str>,
        agent: Option<&str>,
        reconciliation_revision: u64,
        agent_activity_enabled: bool,
    ) -> Result<Self, OpenCodeRuntimeError> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (reconciliation_hint_tx, reconciliation_hint_rx) =
            mpsc::channel(RECONCILIATION_HINT_CAPACITY);
        let reconciliation_cancellation = CancellationToken::new();
        let owner_state = Arc::new(RuntimeOwnerState::new());
        let external_owner = Arc::new(RuntimeOwnerLiveness {
            state: owner_state.clone(),
        });
        let mut headers = HeaderMap::new();
        if let Some(password) = password.filter(|value| !value.is_empty()) {
            let credentials = STANDARD.encode(format!("opencode:{password}"));
            let header = HeaderValue::from_str(&format!("Basic {credentials}"))
                .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
            headers.insert(AUTHORIZATION, header);
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        let runtime = Self {
            _external_owner: Some(external_owner),
            inner: Arc::new(RuntimeInner {
                client,
                base_url: base_url.trim_end_matches('/').to_owned(),
                thread_id: thread_id.to_owned(),
                directory: directory.to_owned(),
                model: Mutex::new(model.and_then(parse_model_slug)),
                selected_variant: Mutex::new(None),
                agent: agent
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                runtime_mode: Mutex::new("approval-required".to_owned()),
                session_id: Mutex::new(None),
                active_turn_id: Mutex::new(None),
                active_user_message_id: Mutex::new(None),
                assistant_message_ids: Mutex::new(HashSet::new()),
                assistant_text: Mutex::new(HashMap::new()),
                events_tx,
                events_rx: Mutex::new(events_rx),
                event_stream_ended: CancellationToken::new(),
                event_counter: Mutex::new(0),
                pending_questions: Mutex::new(HashMap::new()),
                pending_permissions: Mutex::new(HashMap::new()),
                event_pump: Mutex::new(None),
                reconciliation_hint_tx,
                reconciliation_requests: StdMutex::new(ReconciliationRequestState {
                    pending_hint: None,
                    identity: ReconciliationIdentityState::new(thread_id, reconciliation_revision),
                }),
                reconciliation_task: StdMutex::new(None),
                reconciliation_cancellation,
                activity_flush_generation: AtomicU64::new(0),
                activity_flush_task: StdMutex::new(None),
                activity_flush_gate: Mutex::new(()),
                owner_state,
                activity: Mutex::new(OpenCodeRuntimeActivityState {
                    agent_activity_enabled,
                    generation: 0,
                    reconciliation_pass_cancellation: CancellationToken::new(),
                    tracker: None,
                    root_session_id: None,
                    child_support: EndpointSupport::Unknown,
                    status_support: EndpointSupport::Unknown,
                    history_support: EndpointSupport::Unknown,
                    history_signatures: HashMap::new(),
                    history_cursors: HashMap::new(),
                    capabilities: ActivityCapabilities::none(),
                }),
                protocol_v2: AtomicBool::new(false),
                v2: Mutex::new(V2State::default()),
                v2_rewind: Mutex::new(()),
            }),
        };
        runtime.start_reconciliation_worker(reconciliation_hint_rx);
        Ok(runtime)
    }

    pub async fn set_agent_activity_enabled(&self, enabled: bool) {
        {
            let mut activity = self.inner.activity.lock().await;
            if activity.agent_activity_enabled == enabled {
                return;
            }
            activity.agent_activity_enabled = enabled;
            activity.generation = activity.generation.wrapping_add(1);
            activity.reconciliation_pass_cancellation.cancel();
            activity.reconciliation_pass_cancellation = CancellationToken::new();
            let root_session_id = activity.root_session_id.clone();
            activity.tracker = if enabled {
                root_session_id
                    .as_deref()
                    .filter(|root| valid_key(root))
                    .map(OpenCodeActivityTracker::new)
            } else {
                None
            };
            activity.child_support = EndpointSupport::Unknown;
            activity.status_support = EndpointSupport::Unknown;
            activity.history_support = EndpointSupport::Unknown;
            activity.history_signatures.clear();
            activity.history_cursors.clear();
            activity.capabilities = ActivityCapabilities::none();
            if let Some(tracker) = activity.tracker.as_mut() {
                tracker.begin_detail_baseline();
            }
            self.inner
                .reconciliation_requests
                .lock()
                .expect("OpenCode reconciliation request mutex poisoned")
                .pending_hint = None;
        }
        self.cancel_activity_text_flush().await;
        if enabled && self.inner.session_id.lock().await.is_some() {
            self.request_reconciliation(true, None, true).await;
        }
    }

    fn internal(inner: Arc<RuntimeInner>) -> Self {
        Self {
            inner,
            _external_owner: None,
        }
    }

    /// Speak the OpenCode 2 `/api/*` wire. Call before `start` or `resume`.
    pub(crate) fn use_v2_api(&self) {
        self.inner.protocol_v2.store(true, Ordering::SeqCst);
    }

    fn is_v2(&self) -> bool {
        self.inner.protocol_v2.load(Ordering::SeqCst)
    }

    pub async fn start(&self) -> Result<String, OpenCodeRuntimeError> {
        let session_id = if self.is_v2() {
            self.v2_create_session().await?
        } else {
            self.v1_create_session().await?
        };
        self.started(&session_id, json!({})).await?;
        Ok(session_id)
    }

    async fn started(&self, session_id: &str, payload: Value) -> Result<(), OpenCodeRuntimeError> {
        *self.inner.session_id.lock().await = Some(session_id.to_owned());
        if self.is_v2() {
            let mut v2 = self.inner.v2.lock().await;
            v2.translator = Some(V2TurnTranslator::new(session_id));
            v2.normalizer = V2ActivityNormalizer::default();
            v2.permissions.clear();
            v2.forms.clear();
            v2.answering.clear();
            v2.settled_while_answering.clear();
        }
        let resumed_v2 = self.is_v2() && payload.get("resumed").is_some();
        if resumed_v2 {
            self.v2_restore_pending_asks(session_id).await;
        }
        self.reset_activity_root(session_id).await;
        self.emit("session.started", None, None, payload).await;
        self.emit("thread.started", None, None, json!({})).await;
        if resumed_v2 {
            self.inner.v2.lock().await.adopt_on_connect = true;
        }
        self.start_event_pump().await?;
        self.request_reconciliation(true, None, true).await;
        Ok(())
    }

    async fn v2_create_session(&self) -> Result<String, OpenCodeRuntimeError> {
        let runtime_mode = self.inner.runtime_mode.lock().await.clone();
        let model = self.inner.model.lock().await.clone();
        let variant = self.inner.selected_variant.lock().await.clone();
        let body = v2::session_create_body(
            &format!("BiBCode {}", self.inner.thread_id),
            &self.inner.directory,
            &runtime_mode,
            self.inner.agent.as_deref(),
            model.as_ref().map(|(provider_id, model_id)| {
                v2::model_ref(provider_id, model_id, variant.as_deref())
            }),
        );
        let response = self
            .inner
            .client
            .post(self.v2_url(&["session"], false)?)
            .json(&body)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "session.create returned HTTP {}",
                response.status()
            )));
        }
        let value = response
            .json::<Value>()
            .await
            .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
        let session_id = value
            .pointer("/data/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OpenCodeRuntimeError::InvalidResponse("session.create missing id".to_owned())
            })?
            .to_owned();
        let mut v2 = self.inner.v2.lock().await;
        v2.synced_model = model.map(|(provider_id, model_id)| (provider_id, model_id, variant));
        v2.synced_agent = true;
        v2.synced_runtime_mode = Some(runtime_mode);
        Ok(session_id)
    }

    async fn v1_create_session(&self) -> Result<String, OpenCodeRuntimeError> {
        let permission = build_permission_rules(&self.inner.runtime_mode.lock().await);
        let response = self
            .inner
            .client
            .post(self.request_url("/session")?)
            .json(&json!({
                "title": format!("BiBCode {}", self.inner.thread_id),
                "permission": permission,
            }))
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        let value = response
            .json::<Value>()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        let session_id = value
            .get("id")
            .or_else(|| value.get("data").and_then(|data| data.get("id")))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                OpenCodeRuntimeError::InvalidResponse("session.create missing id".to_owned())
            })?
            .to_owned();
        Ok(session_id)
    }

    pub async fn resume(&self, session_id: &str) -> Result<String, OpenCodeRuntimeError> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(OpenCodeRuntimeError::MissingSession);
        }
        let url = if self.is_v2() {
            self.v2_url(&["session", session_id], false)?
        } else {
            self.request_url(&format!("/session/{session_id}"))?
        };
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        // The server no longer has the session, so retrying cannot resume it. Any other failure
        // may be transient and keeps the saved session for the next attempt.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return self.start().await;
        }
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "session resume returned HTTP {}",
                response.status()
            )));
        }
        if self.is_v2() {
            // A stage left by a rewind that did not finish would commit with the next prompt.
            let session = response
                .json::<Value>()
                .await
                .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
            if session
                .pointer("/data/revert")
                .is_some_and(|revert| !revert.is_null())
            {
                within_rewind_deadline(self.v2_clear_revert(session_id)).await?;
            }
            // A checkpoint rewind that had not reported success when the runtime went away
            // continues from the progress kept with the session.
            if let Some(progress) = session
                .pointer("/data/metadata/bibcode/rewind")
                .and_then(V2RewindProgress::from_json)
            {
                let mut v2 = self.inner.v2.lock().await;
                if progress.finished {
                    v2.finished_rewind = Some(progress);
                } else {
                    v2.rewind = progress;
                }
            }
            // The resumed session keeps the model, agent and permission rules it had. A runtime
            // mode change relaunches with a resume, and OpenCode may keep working on its own, so
            // the permission rules apply now; the model and agent go with the next prompt.
            {
                let mut v2 = self.inner.v2.lock().await;
                v2.synced_model = None;
                v2.synced_agent = false;
                v2.synced_runtime_mode = None;
            }
            self.v2_sync_permissions(session_id).await?;
        }
        self.started(session_id, json!({ "resumed": true })).await?;
        Ok(session_id.to_owned())
    }

    pub async fn add_mcp_server(
        &self,
        name: &str,
        url: &str,
        authorization_header: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        if self.is_v2() {
            // OpenCode 2 registers MCP servers only on an experimental route.
            let response = self
                .inner
                .client
                .put(self.v2_url(&["experimental", "mcp", name], true)?)
                .json(&json!({
                    "config": {
                        "type": "remote",
                        "url": url,
                        "headers": { "Authorization": authorization_header },
                        "oauth": false,
                    }
                }))
                .send()
                .await
                .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
            if !response.status().is_success() {
                return Err(OpenCodeRuntimeError::Http(format!(
                    "OpenCode MCP registration returned HTTP {}",
                    response.status()
                )));
            }
            return Ok(());
        }
        let mut endpoint = Url::parse(&format!("{}/mcp", self.inner.base_url))
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        endpoint
            .query_pairs_mut()
            .append_pair("directory", &self.inner.directory);
        let response = self
            .inner
            .client
            .post(endpoint)
            .json(&json!({
                "name": name,
                "config": {
                    "type": "remote",
                    "url": url,
                    "headers": { "Authorization": authorization_header },
                    "oauth": false,
                }
            }))
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "OpenCode MCP registration returned HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    pub async fn send_turn(
        &self,
        text: Option<&str>,
        attachments: Vec<Value>,
        message_id: Option<&str>,
    ) -> Result<String, OpenCodeRuntimeError> {
        let session_id = self.session_id().await?;
        if self.is_v2() {
            let message_id = message_id.map(v2::message_id);
            // The checks run before the turn is announced: when one fails nothing was sent, and
            // a started turn would wait for a completion that never comes.
            let mut body = v2::prompt_body(text, &attachments, message_id.as_deref());
            self.v2_settle_rewind_before_prompt(&session_id).await?;
            self.v2_sync_selection(&session_id)
                .await
                .map_err(not_sent)?;
            // A slash skill with attachments arrives here rather than as a command.
            if let Some((name, rest)) = text.and_then(crate::provider::parse_provider_command)
                && self.v2_is_skill(name).await?
            {
                body = v2::prompt_body(Some(rest), &attachments, message_id.as_deref());
                body["skills"] = json!([{ "id": name }]);
            }
            let turn_id = self.begin_turn().await;
            // A request that failed after it may have reached OpenCode stays ambiguous.
            let mut maybe_delivered = false;
            let result = async {
                let response = self
                    .inner
                    .client
                    .post(self.v2_url(&["session", &session_id, "prompt"], false)?)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|error| {
                        maybe_delivered = !error.is_connect();
                        OpenCodeRuntimeError::Http(error.to_string())
                    })?;
                if !response.status().is_success() {
                    // A refusal (4xx) was not accepted; a server or gateway error may have
                    // followed acceptance.
                    maybe_delivered = !response.status().is_client_error();
                    return Err(OpenCodeRuntimeError::Http(format!(
                        "prompt returned HTTP {}",
                        response.status()
                    )));
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                // Only a request OpenCode never accepted settles the turn now; one it may have
                // accepted keeps its turn, which OpenCode's own events end.
                if !maybe_delivered {
                    self.settle_unsent_turn(turn_id).await;
                }
                return Err(error);
            }
            return Ok(turn_id);
        }
        let turn_id = self.begin_turn().await;
        let mut body = json!({
            "sessionID": session_id,
            "parts": crate::provider::attachments::prompt_parts(text, attachments),
        });
        if let Some(message_id) = message_id {
            body["messageID"] = json!(v2::message_id(message_id));
        }
        if let Some((provider_id, model_id)) = self.inner.model.lock().await.as_ref() {
            body["model"] = json!({
                "providerID": provider_id,
                "modelID": model_id,
            });
        }
        if let Some(variant) = self.inner.selected_variant.lock().await.clone() {
            body["variant"] = Value::String(variant);
        }
        if let Some(agent) = self.inner.agent.as_ref() {
            body["agent"] = json!(agent);
        }
        // A request that failed after it may have reached OpenCode stays ambiguous.
        let mut maybe_delivered = false;
        let result = async {
            let response = self
                .inner
                .client
                .post(self.request_url(&format!("/session/{session_id}/prompt_async"))?)
                .json(&body)
                .send()
                .await
                .map_err(|error| {
                    maybe_delivered = !error.is_connect();
                    OpenCodeRuntimeError::Http(error.to_string())
                })?;
            if !response.status().is_success() {
                // A refusal (4xx) was not accepted; a server or gateway error may have
                // followed acceptance.
                maybe_delivered = !response.status().is_client_error();
                return Err(OpenCodeRuntimeError::Http(format!(
                    "prompt_async returned HTTP {}",
                    response.status()
                )));
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            // Only a request OpenCode never accepted settles the turn now; one it may have
            // accepted keeps its turn, which OpenCode's own events end.
            if !maybe_delivered {
                self.settle_unsent_turn(turn_id).await;
            }
            return Err(error);
        }
        Ok(turn_id)
    }

    pub async fn message_exists(&self, message_id: &str) -> Result<bool, OpenCodeRuntimeError> {
        let session_id = self.session_id().await?;
        if self.is_v2() {
            return self
                .v2_user_message_exists(&session_id, &v2::message_id(message_id))
                .await;
        }
        // Deliveries are sent under the key's `msg_` id. One sent before that carried the raw key
        // (OpenCode accepted it before 1.18), so a delivery found under either form exists.
        let derived = v2::message_id(message_id);
        if self.v1_user_message_exists(&session_id, &derived).await? {
            return Ok(true);
        }
        if derived == message_id {
            return Ok(false);
        }
        self.v1_user_message_exists(&session_id, message_id).await
    }

    /// Whether an OpenCode 1 session has the user message `message_id`. A refused id (HTTP 400:
    /// OpenCode 1.18 and later accept only `msg` ids) cannot name a message there.
    async fn v1_user_message_exists(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<bool, OpenCodeRuntimeError> {
        let response = self
            .inner
            .client
            .get(self.session_request_url(session_id, &["message", message_id])?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND
            || (response.status() == reqwest::StatusCode::BAD_REQUEST
                && !message_id.starts_with("msg"))
        {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "message lookup returned HTTP {}",
                response.status()
            )));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
        let info = value
            .get("info")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                OpenCodeRuntimeError::InvalidResponse(
                    "message lookup response missing info".to_owned(),
                )
            })?;
        if info.get("id").and_then(Value::as_str) != Some(message_id)
            || info.get("sessionID").and_then(Value::as_str) != Some(session_id)
            || info.get("role").and_then(Value::as_str) != Some("user")
            || value.get("parts").and_then(Value::as_array).is_none()
        {
            return Err(OpenCodeRuntimeError::InvalidResponse(
                "message lookup response has invalid identity or shape".to_owned(),
            ));
        }
        Ok(true)
    }

    pub async fn send_command(
        &self,
        command: &str,
        arguments: &str,
        message_id: Option<&str>,
    ) -> Result<String, OpenCodeRuntimeError> {
        let session_id = self.session_id().await?;
        let command = command.trim().trim_start_matches('/');
        if command.is_empty() {
            return Err(OpenCodeRuntimeError::InvalidResponse(
                "command name cannot be empty".to_owned(),
            ));
        }
        if self.is_v2() {
            // OpenCode 2 takes no message id for a command, so a command's delivery cannot be
            // looked up exactly afterwards: if BiBCode stops between OpenCode accepting the
            // command and recording the delivery, restart recovery finds no message and sends
            // it again. The window is that of one database write.
            // The checks run before the turn is announced: when one fails nothing was sent.
            self.v2_settle_rewind_before_prompt(&session_id).await?;
            self.v2_sync_selection(&session_id)
                .await
                .map_err(not_sent)?;
            // OpenCode 2 skills are not commands: a slash skill is a prompt that attaches the
            // skill (and, unlike a command, takes a message id, so it is looked up exactly).
            let (route, body) = if self.v2_is_skill(command).await? {
                let mut body = v2::prompt_body(
                    Some(arguments.trim()),
                    &[],
                    message_id.map(v2::message_id).as_deref(),
                );
                body["skills"] = json!([{ "id": command }]);
                ("prompt", body)
            } else {
                ("command", v2::command_body(command, arguments.trim()))
            };
            let turn_id = self.begin_turn().await;
            // A request that failed after it may have reached OpenCode stays ambiguous.
            let mut maybe_delivered = false;
            let result = async {
                let response = self
                    .inner
                    .client
                    .post(self.v2_url(&["session", &session_id, route], false)?)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|error| {
                        maybe_delivered = !error.is_connect();
                        OpenCodeRuntimeError::Http(error.to_string())
                    })?;
                if !response.status().is_success() {
                    // A refusal (4xx) was not accepted; a server or gateway error may have
                    // followed acceptance.
                    maybe_delivered = !response.status().is_client_error();
                    return Err(OpenCodeRuntimeError::Http(format!(
                        "command returned HTTP {}",
                        response.status()
                    )));
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                // Only a request OpenCode never accepted settles the turn now; one it may have
                // accepted keeps its turn, which OpenCode's own events end.
                if !maybe_delivered {
                    self.settle_unsent_turn(turn_id).await;
                }
                return Err(error);
            }
            return Ok(turn_id);
        }
        let turn_id = self.begin_turn().await;
        let mut body = json!({
            "command": command,
            "arguments": arguments.trim(),
        });
        if let Some(message_id) = message_id {
            body["messageID"] = json!(v2::message_id(message_id));
        }
        if let Some(agent) = self.inner.agent.as_ref() {
            body["agent"] = json!(agent);
        }
        if let Some((provider_id, model_id)) = self.inner.model.lock().await.as_ref() {
            body["model"] = json!(format!("{provider_id}/{model_id}"));
        }
        if let Some(variant) = self.inner.selected_variant.lock().await.clone() {
            body["variant"] = Value::String(variant);
        }
        // A request that failed after it may have reached OpenCode stays ambiguous.
        let mut maybe_delivered = false;
        let result = async {
            let response = self
                .inner
                .client
                .post(self.request_url(&format!("/session/{session_id}/command"))?)
                .json(&body)
                .send()
                .await
                .map_err(|error| {
                    maybe_delivered = !error.is_connect();
                    OpenCodeRuntimeError::Http(error.to_string())
                })?;
            if !response.status().is_success() {
                // A refusal (4xx) was not accepted; a server or gateway error may have
                // followed acceptance.
                maybe_delivered = !response.status().is_client_error();
                return Err(OpenCodeRuntimeError::Http(format!(
                    "command returned HTTP {}",
                    response.status()
                )));
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            // Only a request OpenCode never accepted settles the turn now; one it may have
            // accepted keeps its turn, which OpenCode's own events end.
            if !maybe_delivered {
                self.settle_unsent_turn(turn_id).await;
            }
            return Err(error);
        }
        Ok(turn_id)
    }

    pub async fn set_model(&self, model: &str) -> Result<(), OpenCodeRuntimeError> {
        let parsed = parse_model_slug(model).ok_or_else(|| {
            OpenCodeRuntimeError::InvalidResponse(
                "model selection must use the provider/model format".to_owned(),
            )
        })?;
        *self.inner.model.lock().await = Some(parsed);
        Ok(())
    }

    pub async fn set_variant(&self, variant: Option<String>) {
        *self.inner.selected_variant.lock().await = variant;
    }

    pub async fn set_options(&self, options: Vec<Value>) -> Result<(), OpenCodeRuntimeError> {
        if options.is_empty() {
            self.set_variant(None).await;
            return Ok(());
        }

        let mut fast_mode = None;
        let mut requested_variant = None;
        for option in options {
            let id = option.get("id").and_then(Value::as_str).ok_or_else(|| {
                OpenCodeRuntimeError::refused(
                    "option id must be a string",
                    option_without_id_refusal(),
                )
            })?;
            match id {
                "fastMode" if fast_mode.is_none() => {
                    fast_mode = Some(option.get("value").and_then(Value::as_bool).ok_or_else(
                        || {
                            OpenCodeRuntimeError::refused(
                                "option fastMode requires a boolean value",
                                option_on_or_off_refusal(FAST_MODE_LABEL),
                            )
                        },
                    )?);
                }
                "variant" if requested_variant.is_none() => {
                    requested_variant = Some(
                        option
                            .get("value")
                            .and_then(Value::as_str)
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(|| {
                                OpenCodeRuntimeError::refused(
                                    "option variant requires a non-empty string value",
                                    option_needs_value_refusal(VARIANT_LABEL),
                                )
                            })?,
                    );
                }
                _ => {
                    return Err(OpenCodeRuntimeError::refused(
                        format!("option {id} is not supported by the selected model/session"),
                        unsupported_option_refusal(id),
                    ));
                }
            }
        }

        let (provider_id, variants) = self.advertised_variants().await?;
        let has_variant = |variant: &str| variants.iter().any(|candidate| candidate == variant);
        let selected = match fast_mode {
            Some(true) => {
                if requested_variant.is_some() {
                    return Err(OpenCodeRuntimeError::refused(
                        "fastMode=true conflicts with variant",
                        options_conflict_refusal(FAST_MODE_LABEL, VARIANT_LABEL),
                    ));
                }
                if !has_variant("fast") {
                    return Err(OpenCodeRuntimeError::refused(
                        "option fastMode is not supported by the selected model/session",
                        unsupported_option_refusal(FAST_MODE_LABEL),
                    ));
                }
                Some("fast".to_owned())
            }
            Some(false) | None => match requested_variant {
                Some(variant) if variant != "fast" && has_variant(&variant) => Some(variant),
                Some(variant) => {
                    return Err(OpenCodeRuntimeError::refused(
                        format!("variant {variant} is not advertised by the selected model"),
                        unsupported_option_refusal(&format!("{VARIANT_LABEL} {variant}")),
                    ));
                }
                None => self
                    .inner
                    .selected_variant
                    .lock()
                    .await
                    .as_deref()
                    .filter(|variant| *variant != "fast" && has_variant(variant))
                    .map(str::to_owned)
                    .or_else(|| {
                        let non_fast = variants
                            .iter()
                            .filter(|variant| variant.as_str() != "fast")
                            .map(String::as_str)
                            .collect::<Vec<_>>();
                        infer_default_variant(&provider_id, &non_fast)
                    }),
            },
        };
        self.set_variant(selected).await;
        Ok(())
    }

    async fn advertised_variants(&self) -> Result<(String, Vec<String>), OpenCodeRuntimeError> {
        let (provider_id, model_id) = self.inner.model.lock().await.clone().ok_or_else(|| {
            OpenCodeRuntimeError::refused(
                "options require a selected provider/model",
                options_need_model_refusal(),
            )
        })?;
        if self.is_v2() {
            let response = self
                .inner
                .client
                .get(self.v2_url(&["model"], true)?)
                .send()
                .await
                .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
            if !response.status().is_success() {
                return Err(OpenCodeRuntimeError::Http(format!(
                    "model inventory returned HTTP {}",
                    response.status()
                )));
            }
            let models = response
                .json::<Value>()
                .await
                .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
            let variants =
                v2::model_variants(&models, &provider_id, &model_id).ok_or_else(|| {
                    OpenCodeRuntimeError::InvalidResponse(
                        "selected model has no advertised variants".to_owned(),
                    )
                })?;
            return Ok((provider_id, variants));
        }
        let response = self
            .inner
            .client
            .get(self.request_url("/provider")?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "provider inventory returned HTTP {}",
                response.status()
            )));
        }
        let providers = response
            .json::<Value>()
            .await
            .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
        let variants = providers
            .get("all")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|provider| provider.get("id").and_then(Value::as_str) == Some(&provider_id))
            .and_then(|provider| provider.get("models"))
            .and_then(Value::as_object)
            .and_then(|models| models.get(&model_id))
            .and_then(|model| model.get("variants"))
            .and_then(Value::as_object)
            .map(|variants| variants.keys().cloned().collect::<Vec<_>>())
            .ok_or_else(|| {
                OpenCodeRuntimeError::InvalidResponse(
                    "selected model has no advertised variants".to_owned(),
                )
            })?;
        Ok((provider_id, variants))
    }

    /// Settles a turn announced for a request that failed. The turn is announced before its
    /// request because OpenCode can stream the execution's first events before the request
    /// returns, and they belong to it; when the request fails, the start would otherwise wait for
    /// a completion that never comes (and the supervisor's idle deadline with it). It settles as
    /// cancelled, not failed: the delivery reports the failure and retries or resolves it.
    async fn settle_unsent_turn(&self, turn_id: String) {
        // The completion is published while the turn is still held, so a turn OpenCode starts
        // next (seen once it is cleared) cannot be announced before this one has ended.
        let mut active = self.inner.active_turn_id.lock().await;
        if active.as_deref() != Some(turn_id.as_str()) {
            // OpenCode already settled it (its own events ended the turn).
            return;
        }
        self.emit(
            "turn.completed",
            Some(turn_id),
            None,
            json!({ "state": "cancelled", "stopReason": "not_sent" }),
        )
        .await;
        *active = None;
    }

    async fn begin_turn(&self) -> String {
        self.begin_turn_with(json!({})).await
    }

    /// A turn OpenCode started on its own. It is marked so the supervisor projects it running:
    /// BiBCode's own turns are announced before they are sent, and a send can still fail.
    async fn begin_provider_started_turn(&self) -> String {
        self.begin_turn_with(json!({ "providerStarted": true }))
            .await
    }

    async fn begin_turn_with(&self, payload: Value) -> String {
        let turn_id = format!("turn-{}", Uuid::new_v4());
        *self.inner.active_user_message_id.lock().await = None;
        self.inner.assistant_message_ids.lock().await.clear();
        self.inner.assistant_text.lock().await.clear();
        *self.inner.active_turn_id.lock().await = Some(turn_id.clone());
        self.emit("turn.started", Some(turn_id.clone()), None, payload)
            .await;
        turn_id
    }

    pub async fn configure_runtime_mode(&self, mode: &str) {
        *self.inner.runtime_mode.lock().await = mode.to_owned();
    }

    pub async fn respond_to_permission(
        &self,
        request_id: &str,
        decision: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        if self.is_v2() {
            // The ask stays pending until OpenCode accepts the reply, so a failed reply can be
            // retried.
            let session_id = {
                let mut v2 = self.inner.v2.lock().await;
                let (_, session_id) = v2.permissions.get(request_id).cloned().ok_or_else(|| {
                    OpenCodeRuntimeError::UnknownPermission(request_id.to_owned())
                })?;
                v2.answering.insert(request_id.to_owned());
                session_id
            };
            let sent = async {
                let response = self
                    .inner
                    .client
                    .post(self.v2_url(
                        &["session", &session_id, "permission", request_id, "reply"],
                        false,
                    )?)
                    .json(&json!({ "decision": v2::permission_decision(decision) }))
                    .send()
                    .await
                    .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
                if !response.status().is_success() {
                    return Err(OpenCodeRuntimeError::Http(format!(
                        "permission reply returned HTTP {}",
                        response.status()
                    )));
                }
                Ok(())
            }
            .await;
            let pending = {
                let mut v2 = self.inner.v2.lock().await;
                v2.answering.remove(request_id);
                if !v2.settled_while_answering.remove(request_id) {
                    sent?;
                }
                v2.permissions.remove(request_id)
            };
            let Some((turn_id, _)) = pending else {
                return Ok(());
            };
            self.emit(
                "request.resolved",
                turn_id,
                Some(request_id.to_owned()),
                json!({
                    "requestType": "exec_command_approval",
                    "decision": decision,
                }),
            )
            .await;
            return Ok(());
        }
        let turn_id = self
            .inner
            .pending_permissions
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| OpenCodeRuntimeError::UnknownPermission(request_id.to_owned()))?;
        let reply = match decision {
            "acceptForSession" => "always",
            "accept" => "once",
            _ => "reject",
        };
        let response = self
            .inner
            .client
            .post(self.request_url(&format!("/permission/{request_id}/reply"))?)
            .json(&json!({ "reply": reply }))
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "permission reply returned HTTP {}",
                response.status()
            )));
        }
        self.emit(
            "request.resolved",
            turn_id,
            Some(request_id.to_owned()),
            json!({
                "requestType": "exec_command_approval",
                "decision": decision,
            }),
        )
        .await;
        Ok(())
    }

    pub async fn interrupt_turn(&self) -> Result<(), OpenCodeRuntimeError> {
        let session_id = self.session_id().await?;
        if self.is_v2() {
            let response = self
                .inner
                .client
                .post(self.v2_url(&["session", &session_id, "interrupt"], false)?)
                .send()
                .await
                .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
            if !response.status().is_success() {
                return Err(OpenCodeRuntimeError::Http(format!(
                    "interrupt returned HTTP {}",
                    response.status()
                )));
            }
            return Ok(());
        }
        self.inner
            .client
            .post(self.request_url(&format!("/session/{session_id}/abort"))?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        Ok(())
    }

    pub async fn respond_to_user_input(
        &self,
        request_id: &str,
        answers: Value,
    ) -> Result<(), OpenCodeRuntimeError> {
        if self.is_v2() {
            // The form stays pending until OpenCode accepts the answer, so a failed reply can be
            // retried.
            let (session_id, answer) = {
                let mut v2 = self.inner.v2.lock().await;
                let form = v2
                    .forms
                    .get(request_id)
                    .ok_or_else(|| OpenCodeRuntimeError::UnknownQuestion(request_id.to_owned()))?;
                let pending = (
                    form.session_id.clone(),
                    v2::form_answer(&form.fields, &answers),
                );
                v2.answering.insert(request_id.to_owned());
                pending
            };
            let sent = async {
                let response = self
                    .inner
                    .client
                    .post(self.v2_url(
                        &["session", &session_id, "form", request_id, "reply"],
                        false,
                    )?)
                    .json(&json!({ "answer": answer }))
                    .send()
                    .await
                    .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
                if !response.status().is_success() {
                    return Err(OpenCodeRuntimeError::Http(format!(
                        "form reply returned HTTP {}",
                        response.status()
                    )));
                }
                Ok(())
            }
            .await;
            let pending = {
                let mut v2 = self.inner.v2.lock().await;
                v2.answering.remove(request_id);
                if !v2.settled_while_answering.remove(request_id) {
                    sent?;
                }
                v2.forms.remove(request_id)
            };
            let Some(form) = pending else {
                return Ok(());
            };
            self.emit(
                "user-input.resolved",
                form.turn_id,
                Some(request_id.to_owned()),
                json!({ "answers": answers }),
            )
            .await;
            return Ok(());
        }
        let pending = self
            .inner
            .pending_questions
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| OpenCodeRuntimeError::UnknownQuestion(request_id.to_owned()))?;
        let normalized = pending
            .questions
            .iter()
            .map(|question| {
                let key = question
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let value = answers
                    .get(key)
                    .cloned()
                    .unwrap_or(Value::Array(Vec::new()));
                let values = match value {
                    Value::String(string) => vec![Value::String(string)],
                    Value::Array(array) => array,
                    _ => Vec::new(),
                };
                Value::Array(values)
            })
            .collect::<Vec<_>>();
        self.inner
            .client
            .post(self.request_url(&format!("/question/{request_id}/reply"))?)
            .json(&json!({ "answers": normalized }))
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        self.emit(
            "user-input.resolved",
            pending.turn_id,
            Some(request_id.to_owned()),
            json!({ "answers": answers }),
        )
        .await;
        Ok(())
    }

    /// `rewind_key` identifies the checkpoint revert (the BiBCode transcript it counts from), so
    /// a retry is applied once. OpenCode 1 reverts in one request and does not use it.
    pub async fn rollback_thread(
        &self,
        num_turns: usize,
        rewind_key: &str,
    ) -> Result<OpenCodeSessionSnapshot, OpenCodeRuntimeError> {
        if self.is_v2() {
            return self.v2_rollback(num_turns, rewind_key).await;
        }
        let session_id = self.session_id().await?;
        let messages = self
            .inner
            .client
            .get(self.request_url(&format!("/session/{session_id}/message"))?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?
            .json::<Value>()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        let assistant_messages = messages
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| {
                entry
                    .get("info")
                    .and_then(|info| info.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant")
            })
            .collect::<Vec<_>>();
        let target_message_id = if assistant_messages.len() > num_turns {
            assistant_messages
                .get(assistant_messages.len() - num_turns - 1)
                .and_then(|entry| entry.get("info"))
                .and_then(|info| info.get("id"))
                .and_then(Value::as_str)
        } else {
            None
        };
        self.inner
            .client
            .post(self.request_url(&format!("/session/{session_id}/revert"))?)
            .json(&match target_message_id {
                Some(message_id) => json!({ "messageID": message_id }),
                None => json!({}),
            })
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        let refreshed = self
            .inner
            .client
            .get(self.request_url(&format!("/session/{session_id}/message"))?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?
            .json::<Value>()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        Ok(OpenCodeSessionSnapshot {
            thread_id: self.inner.thread_id.clone(),
            turns: refreshed
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub async fn stop(&self) -> Result<(), OpenCodeRuntimeError> {
        if let Some(task) = self.inner.event_pump.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
        self.cancel_activity_text_flush().await;
        self.inner.reconciliation_cancellation.cancel();
        self.inner
            .activity
            .lock()
            .await
            .reconciliation_pass_cancellation
            .cancel();
        let reconciliation_task = self
            .inner
            .reconciliation_task
            .lock()
            .expect("OpenCode reconciliation task mutex poisoned")
            .take();
        if let Some(task) = reconciliation_task {
            let _ = task.await;
        }
        self.emit(
            "session.exited",
            None,
            None,
            json!({ "reason": "Session stopped." }),
        )
        .await;
        self.end_event_stream();
        Ok(())
    }

    pub async fn next_event(&self) -> Option<OpenCodeRuntimeEvent> {
        let mut events = self.inner.events_rx.lock().await;
        loop {
            if self.inner.event_stream_ended.is_cancelled() {
                return events.try_recv().ok();
            }
            tokio::select! {
                biased;
                event = events.recv() => return event,
                () = self.inner.event_stream_ended.cancelled() => {}
            }
        }
    }

    pub async fn collect_events(&self, expected: usize) -> Vec<OpenCodeRuntimeEventStableView> {
        let mut events = Vec::with_capacity(expected);
        while events.len() < expected {
            let Some(event) = self.next_event().await else {
                break;
            };
            if !event.activity.is_empty() {
                continue;
            }
            events.push(event.stable_view());
        }
        events
    }

    async fn start_event_pump(&self) -> Result<(), OpenCodeRuntimeError> {
        let session_id = self.session_id().await?;
        let runtime = Arc::downgrade(&self.inner);
        let client = self.inner.client.clone();
        let cancellation = self.inner.reconciliation_cancellation.clone();
        let v2 = self.is_v2();
        let url = if v2 {
            self.v2_url(&["event"], false)?
        } else {
            self.request_url("/event")?
        };
        let task = tokio::spawn(async move {
            let response = match tokio::select! {
                () = cancellation.cancelled() => return,
                response = client.get(url).send() => response,
            } {
                Ok(response) => response,
                Err(error) => {
                    if let Some(inner) = runtime.upgrade() {
                        let runtime = OpenCodeSessionRuntime::internal(inner);
                        runtime
                            .emit(
                                "runtime.error",
                                None,
                                None,
                                json!({
                                    "message": error.to_string(),
                                    "class": "transport_error",
                                }),
                            )
                            .await;
                        runtime.end_event_stream();
                    }
                    return;
                }
            };
            // The status used to go unchecked, so an auth failure or a 5xx was
            // parsed as SSE and surfaced as "invalid JSON" — or as nothing at
            // all — with the status lost.
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                let detail = body.trim().chars().take(300).collect::<String>();
                let message = if detail.is_empty() {
                    format!("OpenCode rejected the event stream with HTTP {status}.")
                } else {
                    format!("OpenCode rejected the event stream with HTTP {status}: {detail}")
                };
                let class = if matches!(status.as_u16(), 401 | 403) {
                    "permission_error"
                } else {
                    "provider_error"
                };
                if let Some(inner) = runtime.upgrade() {
                    let runtime = OpenCodeSessionRuntime::internal(inner);
                    runtime
                        .emit(
                            "runtime.error",
                            None,
                            None,
                            json!({ "message": message, "class": class }),
                        )
                        .await;
                    runtime.end_event_stream();
                }
                return;
            }
            let mut decoder = OpenCodeSseDecoder::default();
            let mut response = response;
            loop {
                let chunk = tokio::select! {
                    () = cancellation.cancelled() => return,
                    chunk = response.chunk() => chunk,
                };
                match chunk {
                    Ok(Some(bytes)) => {
                        if let Err(error) = decoder.push(bytes.as_ref()) {
                            if let Some(inner) = runtime.upgrade() {
                                let runtime = OpenCodeSessionRuntime::internal(inner);
                                runtime
                                    .emit("runtime.error", None, None, json!({ "message": error }))
                                    .await;
                                runtime.request_reconciliation(true, None, true).await;
                            }
                            continue;
                        }
                        loop {
                            let buffered_length = decoder.buffered_len();
                            let event = match decoder.take_event() {
                                Ok(Some(event)) => event,
                                Ok(None) if decoder.buffered_len() < buffered_length => continue,
                                Ok(None) => break,
                                Err(error) => {
                                    if let Some(inner) = runtime.upgrade() {
                                        let runtime = OpenCodeSessionRuntime::internal(inner);
                                        runtime
                                            .emit(
                                                "runtime.error",
                                                None,
                                                None,
                                                json!({ "message": error }),
                                            )
                                            .await;
                                        runtime.request_reconciliation(true, None, true).await;
                                    }
                                    continue;
                                }
                            };
                            let Some(inner) = runtime.upgrade() else {
                                return;
                            };
                            let runtime = OpenCodeSessionRuntime::internal(inner);
                            if v2 {
                                runtime.handle_v2_event(event).await;
                            } else {
                                runtime
                                    .handle_sse_event_observed_at(
                                        &session_id,
                                        event,
                                        observation_time_ms(),
                                    )
                                    .await;
                            }
                        }
                    }
                    Ok(None) => {
                        // The stream ending is how a crashed or killed opencode
                        // server presents. Saying nothing made that look like a
                        // hung BiBCode.
                        if let Some(inner) = runtime.upgrade() {
                            let runtime = OpenCodeSessionRuntime::internal(inner);
                            runtime
                                .emit(
                                    "session.exited",
                                    None,
                                    None,
                                    json!({ "reason": "OpenCode closed its event stream." }),
                                )
                                .await;
                            runtime.end_event_stream();
                        }
                        break;
                    }
                    Err(error) => {
                        if let Some(inner) = runtime.upgrade() {
                            let runtime = OpenCodeSessionRuntime::internal(inner);
                            runtime
                                .emit(
                                    "runtime.error",
                                    None,
                                    None,
                                    json!({ "message": error.to_string() }),
                                )
                                .await;
                            runtime.end_event_stream();
                        }
                        break;
                    }
                }
            }
        });
        *self.inner.event_pump.lock().await = Some(task);
        Ok(())
    }

    #[cfg(test)]
    async fn handle_sse_event(&self, session_id: &str, event: Value) {
        self.handle_sse_event_observed_at(session_id, event, 0)
            .await;
    }

    /// Feeds one OpenCode 1-shaped event to activity tracking: verified children update the
    /// tracker, and events about known sessions schedule reconciliation. OpenCode 2 events reach
    /// it through `v2_activity`. Returns whether the event belongs to the root session's chat.
    async fn observe_activity_event(
        &self,
        session_id: &str,
        event: &Value,
        received_at_ms: u64,
    ) -> bool {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let properties = event.get("properties").cloned().unwrap_or(Value::Null);
        let payload_identity = payload_session_identity(event_type, &properties);
        if event_type == "server.connected" {
            if payload_identity == PayloadSessionIdentity::Missing {
                self.request_reconciliation(false, None, true).await;
            }
            return false;
        }
        let payload_session_id = match payload_identity {
            PayloadSessionIdentity::Agreed(session_id) => Some(session_id),
            PayloadSessionIdentity::Missing | PayloadSessionIdentity::Conflicting => None,
        };
        let (route, candidate_is_known, parent_is_known) = {
            let activity = self.inner.activity.lock().await;
            let route = match payload_identity {
                // opencode marks `session.error`'s `sessionID` optional. Routing
                // an identity-less error to `Foreign` dropped it and left the
                // turn open, so the following `session.status{idle}` reported
                // the upstream failure as a completed turn. This stream drives
                // exactly one root session, so attribute it there rather than
                // lose it. `Conflicting` stays foreign — that one is genuinely
                // ambiguous.
                PayloadSessionIdentity::Missing if event_type == "session.error" => {
                    OpenCodeEventRoute::Root
                }
                PayloadSessionIdentity::Missing | PayloadSessionIdentity::Conflicting => {
                    OpenCodeEventRoute::Foreign
                }
                PayloadSessionIdentity::Agreed(candidate) if candidate == session_id => {
                    OpenCodeEventRoute::Root
                }
                PayloadSessionIdentity::Agreed(candidate)
                    if activity
                        .tracker
                        .as_ref()
                        .is_some_and(|tracker| tracker.is_verified_child(candidate)) =>
                {
                    OpenCodeEventRoute::VerifiedChild
                }
                PayloadSessionIdentity::Agreed(_) => OpenCodeEventRoute::Foreign,
            };
            let candidate_is_known = payload_session_id
                .filter(|candidate| valid_key(candidate))
                .is_some_and(|candidate| {
                    activity.root_session_id.as_deref() == Some(candidate)
                        || activity
                            .tracker
                            .as_ref()
                            .is_some_and(|tracker| tracker.is_verified_child(candidate))
                });
            let parent_is_known = properties
                .pointer("/info/parentID")
                .and_then(Value::as_str)
                .filter(|value| valid_key(value))
                .is_some_and(|parent_id| {
                    activity.root_session_id.as_deref() == Some(parent_id)
                        || activity
                            .tracker
                            .as_ref()
                            .is_some_and(|tracker| tracker.is_verified_child(parent_id))
                });
            (route, candidate_is_known, parent_is_known)
        };
        if matches!(
            event_type,
            "session.created"
                | "session.updated"
                | "session.status"
                | "message.updated"
                | "message.part.updated"
                | "message.part.delta"
                | "session.error"
        ) {
            let mut schedule = false;
            let mut dirty_session_id = None;
            if let Some(candidate) = payload_session_id {
                schedule = candidate_is_known
                    || (matches!(event_type, "session.created" | "session.updated")
                        && parent_is_known);
                dirty_session_id =
                    (candidate_is_known && candidate != session_id).then_some(candidate);
            }
            if schedule {
                self.request_reconciliation(false, dirty_session_id, false)
                    .await;
            }
        }
        match route {
            OpenCodeEventRoute::Foreign => false,
            OpenCodeEventRoute::VerifiedChild => {
                let (activity, generation) = {
                    let mut activity = self.inner.activity.lock().await;
                    let generation = activity.generation;
                    let mutations = activity
                        .tracker
                        .as_mut()
                        .map(|tracker| {
                            tracker
                                .handle_observed_event_at(event, received_at_ms)
                                .mutations
                        })
                        .unwrap_or_default();
                    (mutations, generation)
                };
                self.emit_activity(activity, generation).await;
                if is_activity_text_event(event_type, &properties) {
                    self.schedule_activity_text_flush(generation).await;
                }
                false
            }
            OpenCodeEventRoute::Root => true,
        }
    }

    async fn handle_sse_event_observed_at(
        &self,
        session_id: &str,
        event: Value,
        received_at_ms: u64,
    ) {
        if !self
            .observe_activity_event(session_id, &event, received_at_ms)
            .await
        {
            return;
        }
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let properties = event.get("properties").cloned().unwrap_or(Value::Null);
        let turn_id = self.inner.active_turn_id.lock().await.clone();
        match event_type {
            "message.updated" => {
                let Some(info) = properties.get("info") else {
                    return;
                };
                match info.get("role").and_then(Value::as_str) {
                    Some("user") if turn_id.is_some() => {
                        if let Some(message_id) = info.get("id").and_then(Value::as_str) {
                            *self.inner.active_user_message_id.lock().await =
                                Some(message_id.to_owned());
                        }
                    }
                    Some("assistant") => {
                        if let Some(message_id) = info.get("id").and_then(Value::as_str) {
                            self.inner
                                .assistant_message_ids
                                .lock()
                                .await
                                .insert(message_id.to_owned());
                            if info
                                .pointer("/time/completed")
                                .is_some_and(|completed| !completed.is_null())
                            {
                                self.emit_with_item_id(
                                    "message.assistant.completed",
                                    turn_id,
                                    Some(message_id.to_owned()),
                                    None,
                                    json!({}),
                                )
                                .await;
                            }
                        }
                    }
                    _ => {}
                }
            }
            "message.part.updated" => {
                let nested_part = properties.get("part");
                let part = nested_part.unwrap_or(&properties);
                if part
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind != "text")
                {
                    return;
                }
                let item_id = part
                    .get("messageID")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("messageId").and_then(Value::as_str))
                    .map(str::to_owned);
                let message_id = item_id.as_deref().unwrap_or("assistant");
                if nested_part.is_some()
                    && !self
                        .inner
                        .assistant_message_ids
                        .lock()
                        .await
                        .contains(message_id)
                {
                    return;
                }
                let next_text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                let mut assistant_text = self.inner.assistant_text.lock().await;
                let previous = assistant_text.get(message_id).cloned();
                let (latest, delta) = merge_assistant_text(previous.as_deref(), next_text);
                assistant_text.insert(message_id.to_owned(), latest);
                drop(assistant_text);
                if !delta.is_empty() {
                    self.emit_with_item_id(
                        "content.delta",
                        turn_id,
                        item_id,
                        None,
                        json!({ "streamKind": "assistant_text", "delta": delta }),
                    )
                    .await;
                }
            }
            "session.status"
                if properties
                    .get("status")
                    .and_then(|status| status.get("type"))
                    .and_then(Value::as_str)
                    == Some("idle") =>
            {
                if let Some(completed_turn_id) = self.inner.active_turn_id.lock().await.take() {
                    self.emit(
                        "turn.completed",
                        Some(completed_turn_id),
                        None,
                        json!({ "state": "completed", "stopReason": "completed" }),
                    )
                    .await;
                    self.inner.assistant_message_ids.lock().await.clear();
                    self.inner.assistant_text.lock().await.clear();
                    *self.inner.active_user_message_id.lock().await = None;
                }
            }
            "session.error" => {
                let failure =
                    classify_opencode_error(properties.get("error").unwrap_or(&Value::Null));
                let Some(failed_turn_id) = self.inner.active_turn_id.lock().await.take() else {
                    // An error outside a tracked turn used to return silently,
                    // so anything failing post-idle, during compaction, or on a
                    // background path was invisible. Report it as a runtime
                    // error instead of losing it.
                    self.emit(
                        "runtime.error",
                        None,
                        None,
                        json!({ "message": failure.message, "class": failure.class }),
                    )
                    .await;
                    return;
                };
                let message = failure.message.clone();
                let has_assistant_message =
                    !self.inner.assistant_message_ids.lock().await.is_empty();
                let failed_user_message = self.inner.active_user_message_id.lock().await.take();
                if !has_assistant_message
                    && let Some(message_id) = failed_user_message
                    && let Ok(url) =
                        self.request_url(&format!("/session/{session_id}/message/{message_id}"))
                {
                    let _ = self.inner.client.delete(url).send().await;
                }
                // `state`/`stopReason` were hardcoded to a failure, so an
                // abort — the user's own interrupt — was reported as a provider
                // error. `errorMessage` is the contract-canonical field.
                self.emit(
                    "turn.completed",
                    Some(failed_turn_id),
                    None,
                    json!({
                        "state": failure.state,
                        "stopReason": failure.stop_reason,
                        "errorMessage": message,
                        "errorClass": failure.class,
                    }),
                )
                .await;
                self.inner.assistant_message_ids.lock().await.clear();
                self.inner.assistant_text.lock().await.clear();
            }
            "question.asked" => {
                let request_id = properties
                    .get("requestID")
                    .and_then(Value::as_str)
                    .unwrap_or("question-1")
                    .to_owned();
                let questions = properties
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|question| {
                        json!({
                            "id": question.get("header").cloned().unwrap_or(Value::String(String::new())),
                            "header": question.get("header").cloned().unwrap_or(Value::String(String::new())),
                            "question": question.get("question").cloned().unwrap_or(Value::String(String::new())),
                            "options": question.get("options").cloned().unwrap_or(Value::Array(Vec::new())),
                        })
                    })
                    .collect::<Vec<_>>();
                self.inner.pending_questions.lock().await.insert(
                    request_id.clone(),
                    PendingQuestion {
                        turn_id: turn_id.clone(),
                        questions: questions.clone(),
                    },
                );
                self.emit(
                    "user-input.requested",
                    turn_id,
                    Some(request_id),
                    json!({ "questions": questions }),
                )
                .await;
            }
            "permission.asked" => {
                let request_id = properties
                    .get("requestID")
                    .or_else(|| properties.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("permission-1")
                    .to_owned();
                let permission = properties
                    .get("permission")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let patterns = properties
                    .get("patterns")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let detail = if patterns.is_empty() {
                    permission.to_owned()
                } else {
                    format!("{permission}: {patterns}")
                };
                self.inner
                    .pending_permissions
                    .lock()
                    .await
                    .insert(request_id.clone(), turn_id.clone());
                self.emit(
                    "request.opened",
                    turn_id,
                    Some(request_id),
                    json!({
                        "requestType": "exec_command_approval",
                        "detail": detail,
                    }),
                )
                .await;
            }
            unhandled => {
                // Unknown SSE event types were discarded unlogged, with no
                // metric either. Event type only — properties carry content.
                tracing::debug!(
                    provider = "opencode",
                    event_type = unhandled,
                    "dropped an unhandled opencode event"
                );
            }
        }
    }

    async fn handle_v2_event(&self, event: Value) {
        let normalized = self.inner.v2.lock().await.normalizer.normalize(&event);
        if !normalized.is_empty()
            && let Ok(root_session_id) = self.session_id().await
        {
            let received_at_ms = observation_time_ms();
            for activity_event in normalized {
                self.observe_activity_event(&root_session_id, &activity_event, received_at_ms)
                    .await;
            }
        }
        if event.get("type").and_then(Value::as_str) == Some("server.connected") {
            let adopt = std::mem::take(&mut self.inner.v2.lock().await.adopt_on_connect);
            if adopt && let Ok(session_id) = self.session_id().await {
                self.v2_adopt_running_execution(&session_id).await;
            }
        }
        let outputs = match self.inner.v2.lock().await.translator.as_mut() {
            Some(translator) => translator.handle(&event),
            None => return,
        };
        for output in outputs {
            let turn_id = self.inner.active_turn_id.lock().await.clone();
            match output {
                V2Output::ExecutionStarted => {
                    // OpenCode 2 starts executions on its own, such as to deliver a background
                    // subagent's result. Report them as turns rather than hiding them.
                    if turn_id.is_none() {
                        self.begin_provider_started_turn().await;
                    }
                }
                V2Output::TextDelta { message_id, delta } => {
                    self.emit_with_item_id(
                        "content.delta",
                        turn_id,
                        Some(message_id),
                        None,
                        json!({ "streamKind": "assistant_text", "delta": delta }),
                    )
                    .await;
                }
                V2Output::AssistantCompleted { message_id } => {
                    self.emit_with_item_id(
                        "message.assistant.completed",
                        turn_id,
                        Some(message_id),
                        None,
                        json!({}),
                    )
                    .await;
                }
                V2Output::TurnEnded(failure) => {
                    let Some(completed_turn_id) = self.inner.active_turn_id.lock().await.take()
                    else {
                        if let Some(failure) = failure.filter(|failure| failure.state == "failed") {
                            self.emit(
                                "runtime.error",
                                None,
                                None,
                                json!({ "message": failure.message, "class": failure.class }),
                            )
                            .await;
                        }
                        continue;
                    };
                    let payload = match failure {
                        None => json!({ "state": "completed", "stopReason": "completed" }),
                        Some(failure) => json!({
                            "state": failure.state,
                            "stopReason": failure.stop_reason,
                            "errorMessage": failure.message,
                            "errorClass": failure.class,
                        }),
                    };
                    self.emit("turn.completed", Some(completed_turn_id), None, payload)
                        .await;
                }
                V2Output::PermissionAsked {
                    request_id,
                    session_id,
                    detail,
                } => {
                    self.inner
                        .v2
                        .lock()
                        .await
                        .permissions
                        .insert(request_id.clone(), (turn_id.clone(), session_id));
                    self.emit(
                        "request.opened",
                        turn_id,
                        Some(request_id),
                        json!({ "requestType": "exec_command_approval", "detail": detail }),
                    )
                    .await;
                }
                V2Output::PermissionSettled { request_id } => {
                    // Answered elsewhere, such as in the OpenCode terminal.
                    let pending = {
                        let mut v2 = self.inner.v2.lock().await;
                        if v2.answering.contains(&request_id) {
                            v2.settled_while_answering.insert(request_id.clone());
                            None
                        } else {
                            v2.permissions.remove(&request_id)
                        }
                    };
                    if let Some((turn_id, _)) = pending {
                        self.emit(
                            "request.resolved",
                            turn_id,
                            Some(request_id),
                            json!({ "requestType": "exec_command_approval" }),
                        )
                        .await;
                    }
                }
                V2Output::FormAsked {
                    form_id,
                    session_id,
                    questions,
                    fields,
                } => {
                    self.inner.v2.lock().await.forms.insert(
                        form_id.clone(),
                        V2PendingForm {
                            turn_id: turn_id.clone(),
                            session_id,
                            fields,
                        },
                    );
                    self.emit(
                        "user-input.requested",
                        turn_id,
                        Some(form_id),
                        json!({ "questions": questions }),
                    )
                    .await;
                }
                V2Output::FormUnanswerable {
                    form_id,
                    session_id,
                } => {
                    // A form BiBCode cannot show (such as a sign-in link) would block the agent;
                    // cancelling it tells the agent nobody answered.
                    let cancelled =
                        match self.v2_url(&["session", &session_id, "form", &form_id], false) {
                            Ok(url) => match self.inner.client.delete(url).send().await {
                                Ok(response) if response.status().is_success() => Ok(()),
                                Ok(response) => Err(format!("HTTP {}", response.status())),
                                Err(error) => Err(error.to_string()),
                            },
                            Err(error) => Err(error.to_string()),
                        };
                    match cancelled {
                        Ok(()) => tracing::warn!(
                            provider = "opencode",
                            "cancelled an OpenCode form BiBCode cannot show"
                        ),
                        Err(error) => {
                            tracing::warn!(
                                provider = "opencode",
                                %error,
                                "could not cancel an OpenCode form BiBCode cannot show"
                            );
                            self.emit(
                                "runtime.error",
                                turn_id,
                                None,
                                json!({
                                    "message": "OpenCode is waiting for an answer BiBCode can't show. Answer it in the OpenCode terminal, or stop the turn.",
                                    "class": "provider_error",
                                }),
                            )
                            .await;
                        }
                    }
                }
                V2Output::FormSettled { form_id } => {
                    let pending = {
                        let mut v2 = self.inner.v2.lock().await;
                        if v2.answering.contains(&form_id) {
                            v2.settled_while_answering.insert(form_id.clone());
                            None
                        } else {
                            v2.forms.remove(&form_id)
                        }
                    };
                    if let Some(form) = pending {
                        self.emit(
                            "user-input.resolved",
                            form.turn_id,
                            Some(form_id),
                            json!({ "answers": {} }),
                        )
                        .await;
                    }
                }
            }
        }
    }

    /// Sends BiBCode's agent and model selection to an OpenCode 2 session when it changed, since
    /// OpenCode 2 keeps them as session state rather than per prompt.
    async fn v2_sync_selection(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        let model = self.inner.model.lock().await.clone();
        let variant = self.inner.selected_variant.lock().await.clone();
        let (synced_model, synced_agent, synced_runtime_mode) = {
            let v2 = self.inner.v2.lock().await;
            (
                v2.synced_model.clone(),
                v2.synced_agent,
                v2.synced_runtime_mode.clone(),
            )
        };
        if synced_runtime_mode.as_deref() != Some(self.inner.runtime_mode.lock().await.as_str()) {
            self.v2_sync_permissions(session_id).await?;
        }
        if !synced_agent {
            // A resumed session keeps the agent it last had, so no selected agent means
            // OpenCode's default one, as a new session would get.
            let agent = match self.inner.agent.clone() {
                Some(agent) => Some(agent),
                None => self.v2_default_agent().await?,
            };
            if let Some(agent) = agent.as_deref() {
                let response = self
                    .inner
                    .client
                    .post(self.v2_url(&["session", session_id, "agent"], false)?)
                    .json(&json!({ "agent": agent }))
                    .send()
                    .await
                    .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
                if !response.status().is_success() {
                    return Err(OpenCodeRuntimeError::Http(format!(
                        "agent selection returned HTTP {}",
                        response.status()
                    )));
                }
            }
            self.inner.v2.lock().await.synced_agent = true;
        }
        if let Some((provider_id, model_id)) = model {
            let desired = (provider_id, model_id, variant);
            if synced_model.as_ref() != Some(&desired) {
                let response = self
                    .inner
                    .client
                    .post(self.v2_url(&["session", session_id, "model"], false)?)
                    .json(&json!({
                        "model": v2::model_ref(&desired.0, &desired.1, desired.2.as_deref()),
                    }))
                    .send()
                    .await
                    .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
                if !response.status().is_success() {
                    return Err(OpenCodeRuntimeError::Http(format!(
                        "model selection returned HTTP {}",
                        response.status()
                    )));
                }
                self.inner.v2.lock().await.synced_model = Some(desired);
            }
        }
        Ok(())
    }

    /// Routes replies to the permission requests and forms a resumed session and its descendants
    /// are still waiting on: OpenCode's event stream does not replay asks made before BiBCode
    /// connected. The asks are not shown again; BiBCode already recorded those it saw.
    async fn v2_restore_pending_asks(&self, root_session_id: &str) {
        let mut sessions = vec![root_session_id.to_owned()];
        let mut queue = VecDeque::from([(root_session_id.to_owned(), 0_usize)]);
        while let Some((parent, depth)) = queue.pop_front() {
            if depth == MAX_LINEAGE_DEPTH || sessions.len() >= RECONCILIATION_CHILD_LIMIT {
                continue;
            }
            let url = match self
                .v2_url(&["session"], false)
                .and_then(|url| append_query_pair(url, "parentID", &parent))
            {
                Ok(url) => url,
                Err(_) => break,
            };
            let Some(children) = self.v2_get_json(url).await else {
                break;
            };
            let found = children
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|child| child.get("id").and_then(Value::as_str))
                .filter(|child| valid_key(child))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            for child in found {
                if !sessions.contains(&child) {
                    queue.push_back((child.clone(), depth + 1));
                    sessions.push(child);
                }
            }
        }
        for session_id in &sessions {
            let permissions = match self.v2_url(&["session", session_id, "permission"], false) {
                Ok(url) => self.v2_get_json(url).await,
                Err(_) => None,
            };
            let forms = match self.v2_url(&["session", session_id, "form"], false) {
                Ok(url) => self.v2_get_json(url).await,
                Err(_) => None,
            };
            let mut v2 = self.inner.v2.lock().await;
            if let Some(translator) = v2.translator.as_mut() {
                translator.track_descendant(session_id);
            }
            for request_id in permissions
                .iter()
                .filter_map(|response| response.get("data").and_then(Value::as_array))
                .flatten()
                .filter_map(|request| request.get("id").and_then(Value::as_str))
            {
                v2.permissions
                    .insert(request_id.to_owned(), (None, session_id.clone()));
            }
            for form in forms
                .iter()
                .filter_map(|response| response.get("data").and_then(Value::as_array))
                .flatten()
            {
                if let (Some(form_id), Some((_, fields))) = (
                    form.get("id").and_then(Value::as_str),
                    v2::form_questions(form),
                ) {
                    v2.forms.insert(
                        form_id.to_owned(),
                        V2PendingForm {
                            turn_id: None,
                            session_id: session_id.clone(),
                            fields,
                        },
                    );
                }
            }
        }
    }

    /// A resumed session may still be executing (OpenCode kept working while BiBCode was away);
    /// its start is not replayed, so report it as a turn now and let its end complete it.
    async fn v2_adopt_running_execution(&self, session_id: &str) {
        let Ok(url) = self.v2_url(&["session", "active"], false) else {
            return;
        };
        let running = self.v2_get_json(url).await.is_some_and(|active| {
            active
                .get("data")
                .and_then(Value::as_object)
                .is_some_and(|sessions| sessions.contains_key(session_id))
        });
        if running && self.inner.active_turn_id.lock().await.is_none() {
            self.begin_provider_started_turn().await;
        }
    }

    /// A best-effort OpenCode 2 read; failures are logged and read as absent.
    async fn v2_get_json(&self, url: String) -> Option<Value> {
        let result = async {
            let response = self
                .inner
                .client
                .get(url)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!("HTTP {}", response.status()));
            }
            response
                .json::<Value>()
                .await
                .map_err(|error| error.to_string())
        }
        .await;
        result
            .inspect_err(|error| {
                tracing::warn!(provider = "opencode", %error, "OpenCode 2 read failed");
            })
            .ok()
    }

    /// Whether `name` is an OpenCode 2 skill that no command shares: a command wins a name
    /// collision, as it does in the slash menu. An unreadable list fails the turn before it is
    /// sent rather than guess the route.
    async fn v2_is_skill(&self, name: &str) -> Result<bool, OpenCodeRuntimeError> {
        let catalog = |kind: &'static str| async move {
            let url = self.v2_url(&[kind], true)?;
            tokio::time::timeout(SLASH_CATALOG_TIMEOUT, self.v2_get_json(url))
                .await
                .ok()
                .flatten()
                .ok_or_else(|| {
                    OpenCodeRuntimeError::NotSent(format!("the {kind} list could not be read"))
                })
        };
        Ok(v2::catalog::is_skill(&catalog("skill").await?, name)
            && !v2::catalog::is_command(&catalog("command").await?, name))
    }

    /// OpenCode 2 reverts in two phases. Staging at a message drops it and everything after it, so
    /// staging at the first message of the n-th most recent turn drops exactly n turns (every turn
    /// when there are fewer); committing makes that final. Only the conversation is reverted
    /// (`files: false`): checkpoint revert restores the files itself.
    ///
    /// Once a commit lands the conversation has changed for good, so it counts even if its answer
    /// is lost (the target message is gone), and the final read is best effort. A stage that does
    /// not commit is cleared, since OpenCode would commit it with the next prompt. Until a rewind
    /// reports success BiBCode's transcript is unchanged, so the turns already dropped (and a
    /// revert whose outcome could not be read) are remembered: a retry, or a rewind to another
    /// checkpoint, drops only the turns still missing, and prompts wait. The memory lives in this
    /// runtime and, so a BiBCode restart finds it, in the OpenCode session's own metadata
    /// (`bibcode.rewind`), keyed by the checkpoint revert's `rewind_key`: it is written before
    /// each stage and marked finished when the rewind succeeds, and a resumed session restores
    /// it. A retry with the same key continues from it, or drops nothing more once it finished,
    /// so the rewind is applied once even if BiBCode lost its completion; a rewind with another
    /// key starts afresh. The driver keeps the session from idle suspension while it is
    /// unfinished (`has_unfinished_rewind`).
    async fn v2_rollback(
        &self,
        num_turns: usize,
        rewind_key: &str,
    ) -> Result<OpenCodeSessionSnapshot, OpenCodeRuntimeError> {
        let session_id = self.session_id().await?;
        let _rewinding = self.inner.v2_rewind.lock().await;
        // A revert in doubt settles first, whatever rewind it belonged to.
        self.v2_settle_unsettled_revert(&session_id).await?;
        {
            let mut v2 = self.inner.v2.lock().await;
            if v2.rewind.key.is_none() && v2.rewind.is_unfinished() {
                // Progress restored from a record that predates keys: this is its retry.
                v2.rewind.key = Some(rewind_key.to_owned());
            } else if v2.rewind.key.as_deref() != Some(rewind_key) {
                // Progress of another rewind: BiBCode's transcript moved on since. A finished
                // rewind with this key is a retry of it.
                let finished = v2
                    .finished_rewind
                    .take()
                    .filter(|finished| finished.key.as_deref() == Some(rewind_key));
                v2.rewind = finished
                    .map(|finished| V2RewindProgress {
                        finished: false,
                        ..finished
                    })
                    .unwrap_or_else(|| V2RewindProgress::for_key(rewind_key));
            }
        }
        let dropped = self.inner.v2.lock().await.rewind.dropped;
        let remaining = num_turns.checked_sub(dropped).ok_or_else(|| {
            OpenCodeRuntimeError::Http("OpenCode already rewound past this checkpoint".to_owned())
        })?;
        if remaining > 0
            && let Some(target) = tokio::time::timeout(
                REWIND_SCAN_TIMEOUT,
                self.v2_nth_latest_turn_start(&session_id, remaining),
            )
            .await
            .unwrap_or_else(|_| {
                Err(OpenCodeRuntimeError::Http(
                    "OpenCode's history took too long to read".to_owned(),
                ))
            })?
        {
            // Kept with the session before anything changes, so a restart mid-rewind settles it.
            let intent = V2RewindProgress {
                key: Some(rewind_key.to_owned()),
                dropped,
                unsettled: Some((remaining, target.clone())),
                finished: false,
            };
            self.v2_store_rewind(&session_id, &intent).await?;
            let revert = |phase: &'static str| {
                self.v2_url(&["session", &session_id, "revert", phase], false)
            };
            let committed = async {
                within_rewind_deadline(
                    self.v2_send(
                        self.inner
                            .client
                            .post(revert("stage")?)
                            .json(&json!({ "messageID": target, "files": false })),
                        "revert staging",
                    ),
                )
                .await?;
                within_rewind_deadline(
                    self.v2_send(self.inner.client.post(revert("commit")?), "revert commit"),
                )
                .await
            }
            .await;
            if let Err(error) = committed {
                self.inner.v2.lock().await.rewind.unsettled = Some((remaining, target));
                // The commit may have landed with only its answer lost.
                if let Err(settling) = self.v2_settle_unsettled_revert(&session_id).await {
                    tracing::warn!(
                        provider = "opencode",
                        error = %settling,
                        "OpenCode 2 rewind outcome is unknown"
                    );
                    return Err(error);
                }
                if self.inner.v2.lock().await.rewind.dropped < num_turns {
                    return Err(error);
                }
            }
        }
        // Every turn is dropped, but BiBCode's transcript shows that only once this reports
        // success; until then the progress holds prompts and a retry drops nothing more.
        self.inner.v2.lock().await.rewind = V2RewindProgress {
            key: Some(rewind_key.to_owned()),
            dropped: num_turns,
            unsettled: None,
            finished: false,
        };
        // Success needs the record marked finished, or a later runtime would restore the progress
        // and hold prompts for a retry that never comes. A failed write fails the rewind, and its
        // retry (same key) marks it finished, applied or not, without dropping more.
        let finished = V2RewindProgress {
            finished: true,
            ..self.inner.v2.lock().await.rewind.clone()
        };
        self.v2_store_rewind(&session_id, &finished).await?;
        {
            let mut v2 = self.inner.v2.lock().await;
            v2.rewind = V2RewindProgress::default();
            v2.finished_rewind = Some(finished);
            // The dropped turns may have switched the model or agent; the next prompt sets them.
            v2.synced_model = None;
            v2.synced_agent = false;
        }
        let turns = within_rewind_deadline(async {
            Ok(self
                .v2_get_json(self.v2_url(&["session", &session_id, "message"], false)?)
                .await)
        })
        .await
        .ok()
        .flatten()
        .and_then(|messages| messages.get("data").and_then(Value::as_array).cloned())
        .unwrap_or_default();
        Ok(OpenCodeSessionSnapshot {
            thread_id: self.inner.thread_id.clone(),
            turns,
        })
    }

    /// Settles a revert whose outcome is unknown: one that landed (its target message is gone)
    /// counts its turns as dropped, and one that did not is cleared so OpenCode cannot commit it
    /// with the next prompt. Fails, keeping it unsettled, when neither can be confirmed.
    async fn v2_settle_unsettled_revert(
        &self,
        session_id: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        let Some((turns, target)) = self.inner.v2.lock().await.rewind.unsettled.clone() else {
            return Ok(());
        };
        // A revert still staged on the session has not landed, whatever its target's lookup says.
        let staged = within_rewind_deadline(async {
            self.v2_get_json(self.v2_url(&["session", session_id], false)?)
                .await
                .ok_or_else(|| {
                    OpenCodeRuntimeError::Http("the session could not be read".to_owned())
                })
        })
        .await?
        .pointer("/data/revert")
        .is_some_and(|revert| !revert.is_null());
        if staged
            || within_rewind_deadline(self.v2_message_type(session_id, &target))
                .await?
                .is_some()
        {
            within_rewind_deadline(self.v2_clear_revert(session_id)).await?;
        } else {
            self.inner.v2.lock().await.rewind.dropped += turns;
        }
        let progress = {
            let mut v2 = self.inner.v2.lock().await;
            v2.rewind.unsettled = None;
            v2.rewind.clone()
        };
        // The kept record still names the unsettled revert, which settles the same way again.
        if let Err(error) = self.v2_store_rewind(session_id, &progress).await {
            tracing::warn!(
                provider = "opencode",
                %error,
                "OpenCode 2 rewind progress could not be kept with the session"
            );
        }
        Ok(())
    }

    /// Keeps the rewind progress in the OpenCode 2 session's metadata (`bibcode.rewind`), so a
    /// BiBCode restart finds it; an empty progress removes it. OpenCode replaces the metadata
    /// whole, so the rest of it is read and written back.
    async fn v2_store_rewind(
        &self,
        session_id: &str,
        progress: &V2RewindProgress,
    ) -> Result<(), OpenCodeRuntimeError> {
        let url = self.v2_url(&["session", session_id], false)?;
        let session = within_rewind_deadline(async {
            self.v2_get_json(url.clone()).await.ok_or_else(|| {
                OpenCodeRuntimeError::Http("the session could not be read".to_owned())
            })
        })
        .await?;
        let mut metadata = session
            .pointer("/data/metadata")
            .filter(|metadata| metadata.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut bibcode = metadata
            .get("bibcode")
            .filter(|bibcode| bibcode.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let empty = *progress == V2RewindProgress::default();
        if empty && bibcode.get("rewind").is_none() {
            return Ok(());
        }
        if let Some(bibcode) = bibcode.as_object_mut() {
            if empty {
                bibcode.remove("rewind");
            } else {
                bibcode.insert("rewind".to_owned(), progress.to_json());
            }
        }
        if let Some(entries) = metadata.as_object_mut() {
            if bibcode.as_object().is_some_and(serde_json::Map::is_empty) {
                entries.remove("bibcode");
            } else {
                entries.insert("bibcode".to_owned(), bibcode);
            }
        }
        within_rewind_deadline(
            self.v2_send(
                self.inner
                    .client
                    .patch(url)
                    .json(&json!({ "metadata": metadata })),
                "rewind progress",
            ),
        )
        .await
    }

    async fn v2_clear_revert(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        self.v2_send(
            self.inner
                .client
                .delete(self.v2_url(&["session", session_id, "revert"], false)?),
            "revert clearing",
        )
        .await
    }

    /// Whether a checkpoint rewind has not reported success yet (turns already dropped, or a
    /// revert whose outcome is unknown). Only this runtime remembers it, so the session must not
    /// be suspended meanwhile.
    pub async fn has_unfinished_rewind(&self) -> bool {
        self.inner.v2.lock().await.rewind.is_unfinished()
    }

    /// A prompt waits while a checkpoint rewind is unfinished: OpenCode's conversation would no
    /// longer match BiBCode's transcript, and a leftover stage would commit with the prompt.
    async fn v2_settle_rewind_before_prompt(
        &self,
        session_id: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        let _rewinding = self.inner.v2_rewind.lock().await;
        let settled = self.v2_settle_unsettled_revert(session_id).await;
        if settled.is_err() || self.inner.v2.lock().await.rewind.is_unfinished() {
            return Err(OpenCodeRuntimeError::NotSent(UNFINISHED_REVERT.to_owned()));
        }
        Ok(())
    }

    /// The first message of the n-th most recent turn, or of the oldest turn when there are
    /// fewer. Pages through the history, newest first, within OpenCode's page-size limit.
    async fn v2_nth_latest_turn_start(
        &self,
        session_id: &str,
        n: usize,
    ) -> Result<Option<String>, OpenCodeRuntimeError> {
        let mut turns = v2::TurnStartFinder::default();
        let mut cursor: Option<String> = None;
        loop {
            let mut url = Url::parse(&self.v2_url(&["session", session_id, "message"], false)?)
                .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("limit", &MESSAGE_PAGE_LIMIT.to_string());
                match cursor.as_deref() {
                    Some(cursor) => query.append_pair("cursor", cursor),
                    None => query.append_pair("order", "desc"),
                };
            }
            let page =
                within_rewind_deadline(async { Ok(self.v2_get_json(url.to_string()).await) })
                    .await?
                    .ok_or_else(|| {
                        OpenCodeRuntimeError::Http("the message list could not be read".to_owned())
                    })?;
            let messages = page
                .get("data")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for message in messages {
                if let Some(start) = turns.push(message, n) {
                    return Ok(Some(start));
                }
            }
            cursor = page
                .pointer("/cursor/next")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if messages.is_empty() || cursor.is_none() {
                return Ok(turns.finish(n));
            }
        }
    }

    async fn v2_user_message_exists(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<bool, OpenCodeRuntimeError> {
        match self.v2_message_type(session_id, message_id).await? {
            None => Ok(false),
            Some(kind) if kind == "user" => Ok(true),
            Some(_) => Err(OpenCodeRuntimeError::InvalidResponse(
                "message lookup response has invalid identity or shape".to_owned(),
            )),
        }
    }

    /// The type of a session message, or `None` when the session has no such message.
    async fn v2_message_type(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<String>, OpenCodeRuntimeError> {
        let response = self
            .inner
            .client
            .get(self.v2_url(&["session", session_id, "message", message_id], false)?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "message lookup returned HTTP {}",
                response.status()
            )));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
        let message = value.get("data").unwrap_or(&Value::Null);
        match (
            message.get("id").and_then(Value::as_str),
            message.get("type").and_then(Value::as_str),
        ) {
            (Some(id), Some(kind)) if id == message_id => Ok(Some(kind.to_owned())),
            _ => Err(OpenCodeRuntimeError::InvalidResponse(
                "message lookup response has invalid identity or shape".to_owned(),
            )),
        }
    }

    async fn v2_send(
        &self,
        request: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<(), OpenCodeRuntimeError> {
        let response = request
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "{what} returned HTTP {}",
                response.status()
            )));
        }
        Ok(())
    }

    /// Gives an OpenCode 2 session the permission rules of BiBCode's runtime mode.
    async fn v2_sync_permissions(&self, session_id: &str) -> Result<(), OpenCodeRuntimeError> {
        let runtime_mode = self.inner.runtime_mode.lock().await.clone();
        let response = self
            .inner
            .client
            .patch(self.v2_url(&["session", session_id], false)?)
            .json(&json!({ "permissions": v2::permission_rules(&runtime_mode) }))
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "permission rules update returned HTTP {}",
                response.status()
            )));
        }
        self.inner.v2.lock().await.synced_runtime_mode = Some(runtime_mode);
        Ok(())
    }

    /// OpenCode 2's default agent: the first visible primary agent it lists (`build` unless
    /// configured otherwise).
    async fn v2_default_agent(&self) -> Result<Option<String>, OpenCodeRuntimeError> {
        let response = self
            .inner
            .client
            .get(self.v2_url(&["agent"], true)?)
            .send()
            .await
            .map_err(|error| OpenCodeRuntimeError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(OpenCodeRuntimeError::Http(format!(
                "agent list returned HTTP {}",
                response.status()
            )));
        }
        let agents = response
            .json::<Value>()
            .await
            .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
        Ok(agents
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|agent| {
                agent.get("mode").and_then(Value::as_str) == Some("primary")
                    && agent.get("hidden").and_then(Value::as_bool) != Some(true)
            })
            .and_then(|agent| agent.get("id").and_then(Value::as_str))
            .map(str::to_owned))
    }

    /// An OpenCode 2 `/api/...` URL. `location` scopes catalog reads to the session directory.
    fn v2_url(&self, segments: &[&str], location: bool) -> Result<String, OpenCodeRuntimeError> {
        let mut url = Url::parse(&self.inner.base_url).map_err(|error| {
            OpenCodeRuntimeError::InvalidResponse(format!("invalid base URL: {error}"))
        })?;
        {
            let mut path = url.path_segments_mut().map_err(|()| {
                OpenCodeRuntimeError::InvalidResponse(
                    "OpenCode base URL cannot accept relative paths".to_owned(),
                )
            })?;
            path.pop_if_empty();
            path.push("api");
            for segment in segments {
                path.push(segment);
            }
        }
        if location {
            url.query_pairs_mut()
                .append_pair("location[directory]", &self.inner.directory);
        }
        Ok(url.to_string())
    }

    async fn emit(
        &self,
        event_type: &str,
        turn_id: Option<String>,
        request_id: Option<String>,
        payload: Value,
    ) {
        self.emit_with_item_id(event_type, turn_id, None, request_id, payload)
            .await;
    }

    async fn emit_with_item_id(
        &self,
        event_type: &str,
        turn_id: Option<String>,
        item_id: Option<String>,
        request_id: Option<String>,
        payload: Value,
    ) {
        let mut counter = self.inner.event_counter.lock().await;
        *counter += 1;
        let _ = self.inner.events_tx.send(OpenCodeRuntimeEvent {
            event_id: format!("evt-{}", *counter),
            provider: PROVIDER.to_owned(),
            created_at: FIXED_EVENT_TIME.to_owned(),
            event_type: event_type.to_owned(),
            thread_id: self.inner.thread_id.clone(),
            turn_id,
            item_id,
            request_id,
            payload,
            native_event_id: None,
            activity: Vec::new(),
        });
    }

    fn end_event_stream(&self) {
        self.inner.event_stream_ended.cancel();
    }

    async fn reset_activity_root(&self, root_session_id: &str) {
        self.cancel_activity_text_flush().await;
        let mut activity = self.inner.activity.lock().await;
        if !valid_key(root_session_id) {
            activity.tracker = None;
            activity.root_session_id = None;
            activity.child_support = EndpointSupport::Unsupported;
            activity.status_support = EndpointSupport::Unsupported;
            activity.history_support = EndpointSupport::Unsupported;
            activity.history_signatures.clear();
            activity.history_cursors.clear();
            activity.capabilities = ActivityCapabilities::none();
            return;
        }
        if activity.root_session_id.as_deref() == Some(root_session_id)
            && (!activity.agent_activity_enabled || activity.tracker.is_some())
        {
            return;
        }
        activity.tracker = activity
            .agent_activity_enabled
            .then(|| OpenCodeActivityTracker::new(root_session_id));
        activity.root_session_id = Some(root_session_id.to_owned());
        activity.child_support = EndpointSupport::Unknown;
        activity.status_support = EndpointSupport::Unknown;
        activity.history_support = EndpointSupport::Unknown;
        activity.history_signatures.clear();
        activity.history_cursors.clear();
        activity.capabilities = ActivityCapabilities::none();
    }

    async fn schedule_activity_text_flush(&self, activity_generation: u64) {
        {
            let activity = self.inner.activity.lock().await;
            if !activity.agent_activity_enabled || activity.generation != activity_generation {
                return;
            }
        }
        let generation = {
            let _flush_gate = self.inner.activity_flush_gate.lock().await;
            self.inner
                .activity_flush_generation
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1)
        };
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let owner_cancellation = self.inner.owner_state.cancellation.clone();
        let runtime = Arc::downgrade(&self.inner);
        let handle = tokio::spawn(async move {
            tokio::select! {
                () = task_cancellation.cancelled() => return,
                () = owner_cancellation.cancelled() => return,
                () = tokio::time::sleep(ACTIVITY_TEXT_COALESCE_DELAY) => {}
            }
            let Some(inner) = runtime.upgrade() else {
                return;
            };
            let _flush_gate = inner.activity_flush_gate.lock().await;
            if !activity_flush_is_current(&inner, generation, &task_cancellation) {
                return;
            }
            let mutations = {
                let mut activity = inner.activity.lock().await;
                if !activity_flush_is_current(&inner, generation, &task_cancellation) {
                    return;
                }
                if !activity.agent_activity_enabled || activity.generation != activity_generation {
                    return;
                }
                activity
                    .tracker
                    .as_mut()
                    .map(|tracker| tracker.flush_text().mutations)
                    .unwrap_or_default()
            };
            if !activity_flush_is_current(&inner, generation, &task_cancellation) {
                return;
            }
            OpenCodeSessionRuntime::internal(inner.clone())
                .emit_activity_for_flush(
                    mutations,
                    activity_generation,
                    generation,
                    &task_cancellation,
                )
                .await;
        });
        let previous = self
            .inner
            .activity_flush_task
            .lock()
            .expect("OpenCode activity flush task mutex poisoned")
            .replace(ActivityFlushTask {
                cancellation,
                handle,
            });
        if let Some(previous) = previous {
            previous.cancellation.cancel();
            let _ = previous.handle.await;
        }
    }

    async fn cancel_activity_text_flush(&self) {
        {
            let _flush_gate = self.inner.activity_flush_gate.lock().await;
            self.inner
                .activity_flush_generation
                .fetch_add(1, Ordering::SeqCst);
        }
        let task = self
            .inner
            .activity_flush_task
            .lock()
            .expect("OpenCode activity flush task mutex poisoned")
            .take();
        if let Some(task) = task {
            task.cancellation.cancel();
            let _ = task.handle.await;
        }
    }

    fn start_reconciliation_worker(&self, mut hints: mpsc::Receiver<()>) {
        let runtime = Arc::downgrade(&self.inner);
        let cancellation = self.inner.reconciliation_cancellation.clone();
        let task = tokio::spawn(async move {
            let mut retry_delay = RECONCILIATION_RETRY_MIN;
            let mut retry_hint: Option<ReconciliationHint> = None;
            let mut continuation_hint: Option<ReconciliationHint> = None;
            loop {
                let mut hint = if let Some(mut hint) = continuation_hint.take() {
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        () = tokio::time::sleep(RECONCILIATION_DEFERRED_DELAY) => {}
                    }
                    let Some(inner) = runtime.upgrade() else {
                        return;
                    };
                    let next = OpenCodeSessionRuntime::internal(inner).take_reconciliation_hint();
                    merge_one_pending_reconciliation_hint(&mut hint, next);
                    hint
                } else if let Some(hint) = retry_hint.take() {
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        () = tokio::time::sleep(retry_delay) => {}
                    }
                    retry_delay = retry_delay
                        .checked_mul(2)
                        .unwrap_or(RECONCILIATION_RETRY_MAX)
                        .min(RECONCILIATION_RETRY_MAX);
                    hint
                } else {
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        hint = hints.recv() => if hint.is_none() {
                            return;
                        }
                    }
                    let Some(inner) = runtime.upgrade() else {
                        return;
                    };
                    let Some(mut hint) =
                        OpenCodeSessionRuntime::internal(inner).take_reconciliation_hint()
                    else {
                        continue;
                    };
                    if !hint.immediate {
                        let deadline = tokio::time::Instant::now() + RECONCILIATION_DEBOUNCE;
                        loop {
                            tokio::select! {
                                biased;
                                () = cancellation.cancelled() => return,
                                wake = hints.recv() => {
                                    if wake.is_none() {
                                        return;
                                    }
                                    let Some(inner) = runtime.upgrade() else {
                                        return;
                                    };
                                    if let Some(next) = OpenCodeSessionRuntime::internal(inner)
                                        .take_reconciliation_hint()
                                    {
                                        merge_reconciliation_hint(&mut hint, next);
                                        if hint.immediate {
                                            break;
                                        }
                                    }
                                }
                                () = tokio::time::sleep_until(deadline) => break,
                            }
                        }
                    }
                    hint
                };

                let Some(inner) = runtime.upgrade() else {
                    return;
                };
                let pass = OpenCodeSessionRuntime::internal(inner)
                    .reconcile_once(&mut hint)
                    .await;
                match pass {
                    ReconciliationPassResult::Success => {
                        retry_delay = RECONCILIATION_RETRY_MIN;
                    }
                    ReconciliationPassResult::Deferred => {
                        retry_delay = RECONCILIATION_RETRY_MIN;
                        continuation_hint = Some(hint);
                    }
                    ReconciliationPassResult::Retry => {
                        retry_hint = Some(hint);
                    }
                }
            }
        });
        let previous = self
            .inner
            .reconciliation_task
            .lock()
            .expect("OpenCode reconciliation task mutex poisoned")
            .replace(task);
        debug_assert!(previous.is_none());
    }

    async fn request_reconciliation(
        &self,
        immediate: bool,
        dirty_session_id: Option<&str>,
        force_history: bool,
    ) {
        let activity = self.inner.activity.lock().await;
        if !activity.agent_activity_enabled {
            return;
        }
        let should_wake = {
            let mut requests = self
                .inner
                .reconciliation_requests
                .lock()
                .expect("OpenCode reconciliation request mutex poisoned");
            let mut next = ReconciliationHint {
                immediate,
                force_history,
                dirty_session_ids: HashSet::new(),
            };
            if let Some(session_id) = dirty_session_id.filter(|value| valid_key(value)) {
                next.dirty_session_ids.insert(session_id.to_owned());
            }
            match requests.pending_hint.as_mut() {
                Some(existing) => {
                    merge_reconciliation_hint(existing, next);
                    false
                }
                None => {
                    requests.pending_hint = Some(next);
                    true
                }
            }
        };
        drop(activity);
        if should_wake {
            let _ = self.inner.reconciliation_hint_tx.try_send(());
        }
    }

    fn take_reconciliation_hint(&self) -> Option<ReconciliationHint> {
        self.inner
            .reconciliation_requests
            .lock()
            .expect("OpenCode reconciliation request mutex poisoned")
            .pending_hint
            .take()
    }

    async fn reconcile_once(&self, hint: &mut ReconciliationHint) -> ReconciliationPassResult {
        let root_session_id = match self.session_id().await {
            Ok(root_session_id) => root_session_id,
            Err(_) => return ReconciliationPassResult::Success,
        };
        if !valid_key(&root_session_id) {
            return ReconciliationPassResult::Success;
        }
        let (generation, cancellation, signatures) = {
            let activity = self.inner.activity.lock().await;
            if !activity.agent_activity_enabled {
                return ReconciliationPassResult::Success;
            }
            (
                activity.generation,
                activity.reconciliation_pass_cancellation.clone(),
                activity.history_signatures.clone(),
            )
        };
        let snapshot = tokio::select! {
            () = cancellation.cancelled() => return ReconciliationPassResult::Success,
            result = tokio::time::timeout(
                RECONCILIATION_TIMEOUT,
                self.fetch_reconciliation_snapshot(&root_session_id, &signatures, hint),
            ) => result,
        };
        let snapshot = match snapshot {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(())) | Err(_) => {
                self.emit_stale_reconciliation(generation).await;
                return ReconciliationPassResult::Retry;
            }
        };
        hint.consume_accepted_snapshot();
        if self
            .apply_reconciliation_snapshot(&root_session_id, snapshot, generation)
            .await
        {
            ReconciliationPassResult::Deferred
        } else {
            ReconciliationPassResult::Success
        }
    }

    async fn fetch_reconciliation_snapshot(
        &self,
        root_session_id: &str,
        previous_signatures: &HashMap<String, String>,
        hint: &ReconciliationHint,
    ) -> Result<ReconciliationSnapshot, ()> {
        let (known_child_support, known_status_support, known_history_support) = {
            let activity = self.inner.activity.lock().await;
            (
                activity.child_support,
                activity.status_support,
                activity.history_support,
            )
        };
        if known_child_support == EndpointSupport::Unsupported {
            return Ok(ReconciliationSnapshot {
                child_batches: Vec::new(),
                statuses: OpenCodeStatusMapDto::new(),
                histories: Vec::new(),
                child_support: EndpointSupport::Unsupported,
                status_support: known_status_support,
                history_support: known_history_support,
            });
        }
        let mut child_batches = Vec::new();
        let mut admitted = HashSet::from([root_session_id.to_owned()]);
        let mut queue = VecDeque::from([(root_session_id.to_owned(), 0_usize)]);
        let mut sessions = HashMap::new();
        let mut child_support = EndpointSupport::Supported;

        while let Some((parent_session_id, parent_depth)) = queue.pop_front() {
            if parent_depth == MAX_LINEAGE_DEPTH {
                continue;
            }
            let response = if self.is_v2() {
                // One page as large as the reconciliation's child budget covers it.
                let url = self
                    .v2_url(&["session"], false)
                    .and_then(|url| append_query_pair(url, "parentID", &parent_session_id))
                    .and_then(|url| {
                        append_query_pair(url, "limit", &RECONCILIATION_CHILD_LIMIT.to_string())
                    })
                    .map_err(|_| ())?;
                self.fetch_reconciliation_v2(url, v2_activity::children)
                    .await?
            } else {
                let url = self
                    .session_request_url(&parent_session_id, &["children"])
                    .map_err(|_| ())?;
                self.fetch_reconciliation_json::<Vec<OpenCodeChildSessionDto>>(url)
                    .await?
            };
            let mut response = match response {
                ReconciliationApiResult::Supported(response) => response,
                ReconciliationApiResult::NotFound => {
                    if parent_session_id == root_session_id {
                        child_support = EndpointSupport::Unsupported;
                        child_batches.clear();
                        sessions.clear();
                        break;
                    }
                    continue;
                }
            };
            response.sort_by(|left, right| left.id.cmp(&right.id));
            let mut accepted = Vec::new();
            for child in response {
                if sessions.len() == RECONCILIATION_CHILD_LIMIT {
                    break;
                }
                if child.parent_id.as_deref() != Some(parent_session_id.as_str())
                    || !valid_key(&child.id)
                    || child.id == root_session_id
                    || !admitted.insert(child.id.clone())
                {
                    continue;
                }
                queue.push_back((child.id.clone(), parent_depth + 1));
                sessions.insert(child.id.clone(), child.clone());
                accepted.push(child);
            }
            child_batches.push((parent_session_id, accepted));
        }

        if child_support == EndpointSupport::Unsupported {
            return Ok(ReconciliationSnapshot {
                child_batches,
                statuses: OpenCodeStatusMapDto::new(),
                histories: Vec::new(),
                child_support,
                status_support: known_status_support,
                history_support: known_history_support,
            });
        }

        let (statuses, status_support) = if known_status_support == EndpointSupport::Unsupported {
            (OpenCodeStatusMapDto::new(), EndpointSupport::Unsupported)
        } else {
            let response = if self.is_v2() {
                self.fetch_reconciliation_v2(
                    self.v2_url(&["session", "active"], false).map_err(|_| ())?,
                    v2_activity::statuses,
                )
                .await?
            } else {
                self.fetch_reconciliation_json::<OpenCodeStatusMapDto>(
                    self.request_url("/session/status").map_err(|_| ())?,
                )
                .await?
            };
            match response {
                ReconciliationApiResult::Supported(statuses) => {
                    (statuses, EndpointSupport::Supported)
                }
                ReconciliationApiResult::NotFound => {
                    (OpenCodeStatusMapDto::new(), EndpointSupport::Unsupported)
                }
            }
        };

        let mut history_support = known_history_support;
        let mut histories = Vec::new();
        let mut session_ids = sessions.keys().cloned().collect::<Vec<_>>();
        session_ids.sort();
        for session_id in session_ids {
            if history_support == EndpointSupport::Unsupported {
                break;
            }
            let session = sessions
                .get(&session_id)
                .expect("reconciliation session ID comes from the session map");
            let signature = reconciliation_signature(session, statuses.get(&session_id));
            if previous_signatures.get(&session_id) == Some(&signature)
                && !hint.force_history
                && !hint.dirty_session_ids.contains(&session_id)
            {
                continue;
            }
            let response = if self.is_v2() {
                let url = self
                    .v2_url(&["session", &session_id, "message"], false)
                    .and_then(|url| {
                        append_query_pair(url, "limit", &RECONCILIATION_HISTORY_LIMIT.to_string())
                    })
                    .map_err(|_| ())?;
                let history_session_id = session_id.clone();
                self.fetch_reconciliation_v2(url, move |response| {
                    v2_activity::history(&history_session_id, response)
                })
                .await?
            } else {
                let url = self
                    .session_request_url(&session_id, &["message"])
                    .map_err(|_| ())?;
                let url =
                    append_query_pair(url, "limit", &RECONCILIATION_HISTORY_LIMIT.to_string())
                        .map_err(|_| ())?;
                self.fetch_reconciliation_json::<Vec<OpenCodeMessageDto>>(url)
                    .await?
            };
            let messages = match response {
                ReconciliationApiResult::Supported(messages) => {
                    history_support = EndpointSupport::Supported;
                    bound_history(messages)
                }
                ReconciliationApiResult::NotFound => {
                    // OpenCode 2 always has message history; a 404 is a session that went away.
                    if history_support == EndpointSupport::Unknown && self.is_v2() {
                        history_support = EndpointSupport::Supported;
                    } else if history_support == EndpointSupport::Unknown {
                        match self.fetch_root_history_capability(root_session_id).await? {
                            EndpointSupport::Supported => {
                                history_support = EndpointSupport::Supported;
                            }
                            EndpointSupport::Unsupported => {
                                history_support = EndpointSupport::Unsupported;
                                break;
                            }
                            EndpointSupport::Unknown => unreachable!(
                                "root history capability probe returns a stable decision"
                            ),
                        }
                    }
                    continue;
                }
            };
            histories.push((session_id, messages, signature));
        }
        Ok(ReconciliationSnapshot {
            child_batches,
            statuses,
            histories,
            child_support,
            status_support,
            history_support,
        })
    }

    /// An OpenCode 2 reconciliation read, mapped onto the OpenCode 1 shape the tracker reads.
    async fn fetch_reconciliation_v2<T: DeserializeOwned>(
        &self,
        url: String,
        map: impl FnOnce(&Value) -> Value,
    ) -> Result<ReconciliationApiResult<T>, ()> {
        match self.fetch_reconciliation_json::<Value>(url).await? {
            ReconciliationApiResult::Supported(response) => serde_json::from_value(map(&response))
                .map(ReconciliationApiResult::Supported)
                .map_err(|_| ()),
            ReconciliationApiResult::NotFound => Ok(ReconciliationApiResult::NotFound),
        }
    }

    async fn fetch_reconciliation_json<T: DeserializeOwned>(
        &self,
        url: String,
    ) -> Result<ReconciliationApiResult<T>, ()> {
        let mut response = self.inner.client.get(url).send().await.map_err(|_| ())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(ReconciliationApiResult::NotFound);
        }
        if !response.status().is_success() {
            return Err(());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > RECONCILIATION_RESPONSE_BYTES)
            {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map(ReconciliationApiResult::Supported)
            .map_err(|_| ())
    }

    async fn fetch_root_history_capability(
        &self,
        root_session_id: &str,
    ) -> Result<EndpointSupport, ()> {
        let url = self
            .session_request_url(root_session_id, &["message"])
            .map_err(|_| ())?;
        let url = append_query_pair(url, "limit", "1").map_err(|_| ())?;
        match self
            .fetch_reconciliation_json::<Vec<OpenCodeMessageDto>>(url)
            .await?
        {
            ReconciliationApiResult::Supported(_) => Ok(EndpointSupport::Supported),
            ReconciliationApiResult::NotFound => Ok(EndpointSupport::Unsupported),
        }
    }

    async fn apply_reconciliation_snapshot(
        &self,
        root_session_id: &str,
        snapshot: ReconciliationSnapshot,
        generation: u64,
    ) -> bool {
        let mut activity = self.inner.activity.lock().await;
        if !activity.agent_activity_enabled
            || activity.generation != generation
            || activity.root_session_id.as_deref() != Some(root_session_id)
        {
            return false;
        }
        let mut record_mutations = Vec::new();
        let previous_cursors = activity.history_cursors.clone();
        let Some(tracker) = activity.tracker.as_mut() else {
            return false;
        };
        for (parent_session_id, children) in &snapshot.child_batches {
            let value = serde_json::to_value(children)
                .expect("OpenCode child reconciliation DTOs serialize");
            append_bounded_mutations(
                &mut record_mutations,
                tracker
                    .reconcile_children(parent_session_id, &value)
                    .mutations,
                RECONCILIATION_RECORD_MUTATION_LIMIT,
            );
        }
        let mut status_ids = snapshot.statuses.keys().cloned().collect::<Vec<_>>();
        status_ids.sort();
        for session_id in status_ids {
            if !tracker.is_verified_child(&session_id) {
                continue;
            }
            let status = snapshot
                .statuses
                .get(&session_id)
                .expect("status ID comes from the status map");
            let event = json!({
                "type": "session.status",
                "properties": {
                    "sessionID": session_id,
                    "status": status,
                }
            });
            append_bounded_mutations(
                &mut record_mutations,
                tracker.handle_event(&event).mutations,
                RECONCILIATION_RECORD_MUTATION_LIMIT,
            );
        }
        let record_limit = RECONCILIATION_RECORD_MUTATION_LIMIT;
        let mut completed_signatures = Vec::new();
        let mut cursor_updates = Vec::new();
        let mut drained_text_in_history = false;
        for (session_id, messages, signature) in &snapshot.histories {
            let mut cursor = previous_cursors
                .get(session_id)
                .filter(|cursor| cursor.signature == *signature)
                .cloned()
                .unwrap_or_else(|| ReconciliationHistoryCursor {
                    signature: signature.clone(),
                    ..ReconciliationHistoryCursor::default()
                });
            while let Some(message) = messages.get(cursor.message_index) {
                let remaining = record_limit.saturating_sub(record_mutations.len());
                if remaining == 0 {
                    break;
                }
                let mut bounded_message = message.clone();
                if message.parts.is_empty() {
                    if cursor.part_index > 0 {
                        cursor.message_index += 1;
                        cursor.part_index = 0;
                        continue;
                    }
                    bounded_message.parts.clear();
                } else {
                    let Some(part) = message.parts.get(cursor.part_index).cloned() else {
                        cursor.message_index += 1;
                        cursor.part_index = 0;
                        continue;
                    };
                    bounded_message.parts = vec![part];
                }
                let value = serde_json::to_value([bounded_message])
                    .expect("OpenCode history reconciliation DTOs serialize");
                let slice_limit = remaining.min(RECONCILIATION_HISTORY_SLICE_BUDGET);
                let mut candidate = tracker.clone();
                let mut output = candidate.handle_history(session_id, &value).mutations;
                if output.len() > slice_limit {
                    break;
                }
                let flush_limit = slice_limit.saturating_sub(output.len());
                let drained = candidate.flush_text_bounded(flush_limit).mutations;
                drained_text_in_history |= !drained.is_empty();
                output.extend(drained);
                *tracker = candidate;
                record_mutations.extend(output);
                if message.parts.is_empty() {
                    cursor.message_index += 1;
                    cursor.part_index = 0;
                } else {
                    cursor.part_index += 1;
                    if cursor.part_index == message.parts.len() {
                        cursor.message_index += 1;
                        cursor.part_index = 0;
                    }
                }
            }
            if cursor.message_index == messages.len() {
                completed_signatures.push((session_id.clone(), signature.clone()));
                cursor_updates.push((session_id.clone(), None));
            } else {
                cursor_updates.push((session_id.clone(), Some(cursor)));
            }
        }
        if !drained_text_in_history {
            let remaining = record_limit.saturating_sub(record_mutations.len());
            record_mutations.extend(
                tracker
                    .flush_text_bounded(remaining.min(RECONCILIATION_HISTORY_SLICE_BUDGET))
                    .mutations,
            );
        }
        let deferred =
            cursor_updates.iter().any(|(_, cursor)| cursor.is_some()) || tracker.has_pending_text();
        if !deferred {
            tracker.finish_detail_baseline();
        }

        activity.child_support = snapshot.child_support;
        activity.status_support = snapshot.status_support;
        activity.history_support = snapshot.history_support;
        for (session_id, signature) in completed_signatures {
            activity.history_signatures.insert(session_id, signature);
        }
        for (session_id, cursor) in cursor_updates {
            if let Some(cursor) = cursor {
                activity.history_signatures.remove(&session_id);
                activity.history_cursors.insert(session_id, cursor);
            } else {
                activity.history_cursors.remove(&session_id);
            }
        }
        let capabilities = reconciliation_capabilities(
            activity.child_support,
            activity.status_support,
            activity.history_support,
        );
        activity.capabilities = capabilities.clone();
        drop(activity);

        let mut mutations =
            reconciliation_scope_mutations(capabilities, ActivityObservationState::Live);
        append_bounded_mutations(
            &mut mutations,
            record_mutations,
            RECONCILIATION_MUTATION_LIMIT,
        );
        self.emit_reconciliation_activity(mutations, generation)
            .await;
        deferred
    }

    async fn emit_stale_reconciliation(&self, generation: u64) {
        let capabilities = {
            let activity = self.inner.activity.lock().await;
            if !activity.agent_activity_enabled || activity.generation != generation {
                return;
            }
            activity.capabilities.clone()
        };
        self.emit_reconciliation_activity(
            reconciliation_scope_mutations(capabilities, ActivityObservationState::Stale),
            generation,
        )
        .await;
    }

    async fn emit_activity(&self, activity: Vec<ProviderActivityMutation>, generation: u64) {
        self.emit_activity_inner(activity, false, generation, None)
            .await;
    }

    async fn emit_reconciliation_activity(
        &self,
        activity: Vec<ProviderActivityMutation>,
        generation: u64,
    ) {
        self.emit_activity_inner(activity, true, generation, None)
            .await;
    }

    async fn emit_activity_for_flush(
        &self,
        activity: Vec<ProviderActivityMutation>,
        activity_generation: u64,
        flush_generation: u64,
        cancellation: &CancellationToken,
    ) {
        self.emit_activity_inner(
            activity,
            false,
            activity_generation,
            Some((flush_generation, cancellation)),
        )
        .await;
    }

    async fn emit_activity_inner(
        &self,
        activity: Vec<ProviderActivityMutation>,
        is_reconciliation: bool,
        expected_generation: u64,
        flush: Option<(u64, &CancellationToken)>,
    ) {
        if activity.is_empty() {
            return;
        }
        let state = self.inner.activity.lock().await;
        if !state.agent_activity_enabled || state.generation != expected_generation {
            return;
        }
        let root_session_id = state.root_session_id.clone().unwrap_or_default();
        let mut counter = self.inner.event_counter.lock().await;
        let _owner_active = if let Some((generation, cancellation)) = flush {
            let active = self
                .inner
                .owner_state
                .active
                .lock()
                .expect("OpenCode runtime owner-state mutex poisoned");
            if !*active
                || cancellation.is_cancelled()
                || self.inner.activity_flush_generation.load(Ordering::SeqCst) != generation
            {
                return;
            }
            Some(active)
        } else {
            None
        };
        let reconciliation_occurrence = {
            let mut requests = self
                .inner
                .reconciliation_requests
                .lock()
                .expect("OpenCode reconciliation request mutex poisoned");
            if is_reconciliation {
                Some(requests.identity.reconciliation_occurrence(&activity))
            } else {
                requests.identity.observe_activity(&activity);
                None
            }
        };
        let native_event_id = reconciliation_native_event_id(
            &self.inner.thread_id,
            &root_session_id,
            reconciliation_occurrence,
            &activity,
        );
        *counter += 1;
        let _ = self.inner.events_tx.send(OpenCodeRuntimeEvent {
            event_id: format!("evt-{}", *counter),
            provider: PROVIDER.to_owned(),
            created_at: FIXED_EVENT_TIME.to_owned(),
            event_type: "activity.native".to_owned(),
            thread_id: self.inner.thread_id.clone(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            native_event_id: Some(native_event_id),
            activity,
        });
    }

    async fn session_id(&self) -> Result<String, OpenCodeRuntimeError> {
        self.inner
            .session_id
            .lock()
            .await
            .clone()
            .ok_or(OpenCodeRuntimeError::MissingSession)
    }

    fn request_url(&self, path: &str) -> Result<String, OpenCodeRuntimeError> {
        let mut url =
            url::Url::parse(&format!("{}{}", self.inner.base_url, path)).map_err(|error| {
                OpenCodeRuntimeError::InvalidResponse(format!("invalid base URL: {error}"))
            })?;
        url.query_pairs_mut()
            .append_pair("directory", &self.inner.directory);
        Ok(url.to_string())
    }

    fn session_request_url(
        &self,
        session_id: &str,
        suffix: &[&str],
    ) -> Result<String, OpenCodeRuntimeError> {
        let mut url = Url::parse(&self.inner.base_url).map_err(|error| {
            OpenCodeRuntimeError::InvalidResponse(format!("invalid base URL: {error}"))
        })?;
        {
            let mut segments = url.path_segments_mut().map_err(|()| {
                OpenCodeRuntimeError::InvalidResponse(
                    "OpenCode base URL cannot accept relative paths".to_owned(),
                )
            })?;
            segments.pop_if_empty();
            segments.push("session");
            segments.push(session_id);
            for segment in suffix {
                segments.push(segment);
            }
        }
        url.query_pairs_mut()
            .append_pair("directory", &self.inner.directory);
        Ok(url.to_string())
    }
}

fn append_query_pair(url: String, key: &str, value: &str) -> Result<String, OpenCodeRuntimeError> {
    let mut url = Url::parse(&url)
        .map_err(|error| OpenCodeRuntimeError::InvalidResponse(error.to_string()))?;
    url.query_pairs_mut().append_pair(key, value);
    Ok(url.to_string())
}

fn merge_reconciliation_hint(target: &mut ReconciliationHint, next: ReconciliationHint) {
    target.immediate |= next.immediate;
    target.force_history |= next.force_history;
    if target.force_history {
        target.dirty_session_ids.clear();
        return;
    }
    for session_id in next.dirty_session_ids {
        if target.dirty_session_ids.contains(&session_id) {
            continue;
        }
        if target.dirty_session_ids.len() == RECONCILIATION_CHILD_LIMIT {
            target.force_history = true;
            target.dirty_session_ids.clear();
            return;
        }
        target.dirty_session_ids.insert(session_id);
    }
}

fn merge_one_pending_reconciliation_hint(
    continuation: &mut ReconciliationHint,
    pending: Option<ReconciliationHint>,
) {
    if let Some(pending) = pending {
        merge_reconciliation_hint(continuation, pending);
    }
}

fn initial_reconciliation_causal_head(thread_id: &str, revision: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"opencode:reconciliation-causal:v1");
    hasher.update([0]);
    hasher.update(
        u64::try_from(thread_id.len())
            .expect("OpenCode thread ID length fits u64")
            .to_be_bytes(),
    );
    hasher.update(thread_id.as_bytes());
    hasher.update(revision.to_be_bytes());
    hasher.finalize().into()
}

fn next_reconciliation_causal_occurrence(current: &[u8; 32], fingerprint: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"opencode:reconciliation-occurrence:v2");
    hasher.update([0]);
    hasher.update(current);
    hasher.update(fingerprint);
    hasher.finalize().into()
}

fn next_activity_causal_head(
    current: &[u8; 32],
    source: &[u8],
    fingerprint: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"opencode:activity-causal-head:v2");
    hasher.update([0]);
    hasher.update(current);
    hasher.update(
        u64::try_from(source.len())
            .expect("activity causal source length fits u64")
            .to_be_bytes(),
    );
    hasher.update(source);
    hasher.update(fingerprint);
    hasher.finalize().into()
}

fn activity_batch_fingerprint(activity: &[ProviderActivityMutation]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"opencode:activity-mutations:v2");
    hasher.update([0]);
    hasher.update(
        u64::try_from(activity.len())
            .expect("activity mutation count fits u64")
            .to_be_bytes(),
    );
    for mutation in activity {
        match mutation {
            ProviderActivityMutation::SetScope {
                capabilities,
                observation_state,
            } => {
                hasher.update([0]);
                update_serialized_fingerprint(&mut hasher, &(capabilities, observation_state));
            }
            ProviderActivityMutation::SetSectionHealth { section, health } => {
                hasher.update([1]);
                update_serialized_fingerprint(&mut hasher, &(section, health));
            }
            ProviderActivityMutation::UpsertActor(actor) => {
                hasher.update([2]);
                update_serialized_fingerprint(&mut hasher, actor);
            }
            ProviderActivityMutation::RemoveActor { actor_id } => {
                hasher.update([3]);
                update_serialized_fingerprint(&mut hasher, actor_id);
            }
            ProviderActivityMutation::UpsertWorkItem(work_item) => {
                hasher.update([4]);
                update_serialized_fingerprint(&mut hasher, work_item);
            }
            ProviderActivityMutation::RemoveWorkItem { work_item_id } => {
                hasher.update([5]);
                update_serialized_fingerprint(&mut hasher, work_item_id);
            }
            ProviderActivityMutation::AppendEntry(entry) => {
                hasher.update([6]);
                update_serialized_fingerprint(&mut hasher, entry);
            }
        }
    }
    hasher.finalize().into()
}

fn update_serialized_fingerprint<T: Serialize>(hasher: &mut Sha256, value: &T) {
    let encoded = serde_json::to_vec(value).expect("provider activity mutation fields serialize");
    hasher.update(
        u64::try_from(encoded.len())
            .expect("serialized activity mutation length fits u64")
            .to_be_bytes(),
    );
    hasher.update(encoded);
}

fn reconciliation_native_event_id(
    thread_id: &str,
    root_session_id: &str,
    reconciliation_occurrence: Option<[u8; 32]>,
    activity: &[ProviderActivityMutation],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(thread_id.as_bytes());
    hasher.update([0]);
    hasher.update(root_session_id.as_bytes());
    hasher.update([0]);
    if let Some(occurrence) = reconciliation_occurrence {
        hasher.update(b"reconciliation");
        hasher.update([0]);
        hasher.update(occurrence);
        hasher.update([0]);
        hasher.update(activity_batch_fingerprint(activity));
    } else {
        // Preserve the established content-addressed IDs for live provider activity frames.
        hasher.update(format!("{activity:?}").as_bytes());
    }
    let digest = hasher.finalize();
    let suffix = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("opencode:activity:{suffix}")
}

fn reconciliation_signature(
    session: &OpenCodeChildSessionDto,
    status: Option<&OpenCodeSessionStatusDto>,
) -> String {
    let value = serde_json::to_vec(&(session, status))
        .expect("OpenCode reconciliation signature DTOs serialize");
    let mut hasher = Sha256::new();
    hasher.update(value);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bound_history(messages: Vec<OpenCodeMessageDto>) -> Vec<OpenCodeMessageDto> {
    let mut bounded = Vec::new();
    let mut part_count = 0;
    for mut message in messages.into_iter().take(RECONCILIATION_HISTORY_LIMIT) {
        let remaining = RECONCILIATION_HISTORY_LIMIT.saturating_sub(part_count);
        message.parts.truncate(remaining);
        part_count = part_count.saturating_add(message.parts.len());
        bounded.push(message);
        if part_count == RECONCILIATION_HISTORY_LIMIT {
            break;
        }
    }
    bounded
}

fn append_bounded_mutations(
    target: &mut Vec<ProviderActivityMutation>,
    mutations: Vec<ProviderActivityMutation>,
    limit: usize,
) {
    let remaining = limit.saturating_sub(target.len());
    target.extend(mutations.into_iter().take(remaining));
}

fn observation_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn reconciliation_scope_mutations(
    capabilities: ActivityCapabilities,
    observation_state: ActivityObservationState,
) -> Vec<ProviderActivityMutation> {
    let subagent_health = if observation_state == ActivityObservationState::Stale {
        ActivitySectionHealth::try_stale(
            "OpenCode subagent observation is temporarily unavailable",
            true,
        )
        .expect("bounded OpenCode subagent stale health")
    } else if capabilities.actors {
        ActivitySectionHealth::live()
    } else {
        ActivitySectionHealth::try_stale(
            "OpenCode subagent activity is unavailable for this runtime",
            false,
        )
        .expect("bounded OpenCode subagent unavailable health")
    };
    vec![
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state,
        },
        ProviderActivityMutation::SetSectionHealth {
            section: ActivitySection::Subagents,
            health: subagent_health,
        },
        ProviderActivityMutation::SetSectionHealth {
            section: ActivitySection::BackgroundTasks,
            health: ActivitySectionHealth::unsupported(),
        },
    ]
}

fn reconciliation_capabilities(
    child_support: EndpointSupport,
    status_support: EndpointSupport,
    history_support: EndpointSupport,
) -> ActivityCapabilities {
    if child_support != EndpointSupport::Supported {
        return ActivityCapabilities::none();
    }
    ActivityCapabilities {
        actors: true,
        attributed_activity: true,
        background_work: false,
        history_recovery: if status_support == EndpointSupport::Supported
            && history_support == EndpointSupport::Supported
        {
            ActivityHistoryRecovery::Full
        } else {
            ActivityHistoryRecovery::Bounded
        },
        terminal_observation: false,
        targeted_actor_cancellation: false,
    }
}

fn build_permission_rules(runtime_mode: &str) -> Vec<Value> {
    if runtime_mode == "full-access" {
        return vec![json!({ "permission": "*", "pattern": "*", "action": "allow" })];
    }
    [
        "*",
        "bash",
        "edit",
        "webfetch",
        "websearch",
        "codesearch",
        "external_directory",
        "doom_loop",
    ]
    .into_iter()
    .map(|permission| json!({ "permission": permission, "pattern": "*", "action": "ask" }))
    .chain(std::iter::once(json!({
        "permission": "question",
        "pattern": "*",
        "action": "allow",
    })))
    .collect()
}

/// A classified opencode failure: the canonical turn state, the stop reason,
/// the `RuntimeErrorClass`, and a message that is useful even when opencode
/// supplied no text.
#[derive(Debug)]
pub(crate) struct OpenCodeFailure {
    pub(crate) state: &'static str,
    pub(crate) stop_reason: &'static str,
    pub(crate) class: &'static str,
    pub(crate) message: String,
}

/// Classify an opencode error object, which is discriminated on `name`.
///
/// opencode already computes the distinction this needs. Reading only
/// `data.message` reported an aborted turn as a provider failure, and left
/// `MessageOutputLengthError` — whose data is empty — indistinguishable from an
/// unexplained BiBCode fault.
/// Verified against the opencode SDK's generated types: the discriminant is
/// literally `"APIError"` (all caps) even though the TypeScript alias is
/// `ApiError`, and `ProviderAuthError`, `UnknownError`,
/// `MessageOutputLengthError` and `MessageAbortedError` are the rest of the
/// documented union. `ContextOverflowError`, `ContentFilterError` and
/// `StructuredOutputError` were read out of the installed 1.18.18 binary but do
/// not appear in the current SDK types; their arms are harmless if the name
/// never arrives, and the `_` arm covers anything unrecognised.
pub(crate) fn classify_opencode_error(error: &Value) -> OpenCodeFailure {
    let name = error.get("name").and_then(Value::as_str);
    let (state, stop_reason, class, fallback) = match name {
        // An abort is the user's own doing, not a provider failure.
        Some("MessageAbortedError") => {
            ("cancelled", "cancelled", "unknown", "The turn was stopped.")
        }
        Some("MessageOutputLengthError") => (
            "failed",
            "max_tokens",
            "provider_error",
            "The model reached its maximum output length.",
        ),
        Some("ContentFilterError") => (
            "failed",
            "refusal",
            "provider_error",
            "The provider's content filter stopped this turn.",
        ),
        Some("ProviderAuthError") => (
            "failed",
            "error",
            "permission_error",
            "The upstream provider rejected the credentials.",
        ),
        Some("ContextOverflowError") => (
            "failed",
            "error",
            "provider_error",
            "The conversation exceeded the model's context window.",
        ),
        Some("APIError") => (
            "failed",
            "error",
            "provider_error",
            "The upstream provider's API returned an error.",
        ),
        _ => (
            "failed",
            "error",
            "provider_error",
            "OpenCode session failed.",
        ),
    };
    let mut message = error
        .pointer("/data/message")
        .and_then(Value::as_str)
        .or_else(|| error.get("message").and_then(Value::as_str))
        .or_else(|| error.as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or(fallback)
        .to_owned();
    // Detail opencode already carries and BiBCode used to drop. It is what tells
    // the user this was the upstream API rather than BiBCode.
    if let Some(provider) = error.pointer("/data/providerID").and_then(Value::as_str) {
        message.push_str(&format!(" (provider: {provider})"));
    }
    if let Some(status) = error.pointer("/data/statusCode").and_then(Value::as_i64) {
        message.push_str(&format!(" (HTTP {status})"));
    }
    if error.pointer("/data/isRetryable").and_then(Value::as_bool) == Some(true) {
        message.push_str(" (the provider reported this as retryable)");
    }
    OpenCodeFailure {
        state,
        stop_reason,
        class,
        message,
    }
}

fn payload_session_identity<'a>(
    event_type: &str,
    properties: &'a Value,
) -> PayloadSessionIdentity<'a> {
    let mut fields = vec![
        properties.get("sessionID"),
        properties.pointer("/info/sessionID"),
        properties.pointer("/part/sessionID"),
    ];
    if matches!(event_type, "session.created" | "session.updated") {
        fields.push(properties.pointer("/info/id"));
    }
    let mut agreed = None;
    for field in fields.into_iter().flatten() {
        let Some(candidate) = field
            .as_str()
            .map(str::trim)
            .filter(|candidate| valid_key(candidate))
        else {
            return PayloadSessionIdentity::Conflicting;
        };
        if agreed.is_some_and(|current| current != candidate) {
            return PayloadSessionIdentity::Conflicting;
        }
        agreed = Some(candidate);
    }
    agreed.map_or(
        PayloadSessionIdentity::Missing,
        PayloadSessionIdentity::Agreed,
    )
}

fn is_activity_text_event(event_type: &str, properties: &Value) -> bool {
    match event_type {
        "message.part.updated" => {
            properties
                .get("part")
                .unwrap_or(properties)
                .get("type")
                .and_then(Value::as_str)
                == Some("text")
        }
        "message.part.delta" => properties.get("field").and_then(Value::as_str) == Some("text"),
        _ => false,
    }
}

fn activity_flush_is_current(
    inner: &RuntimeInner,
    generation: u64,
    cancellation: &CancellationToken,
) -> bool {
    inner.owner_state.is_active()
        && !cancellation.is_cancelled()
        && inner.activity_flush_generation.load(Ordering::SeqCst) == generation
}

#[cfg(test)]
mod error_classification_tests {
    use super::classify_opencode_error;
    use serde_json::json;

    #[test]
    fn an_abort_is_not_reported_as_a_provider_failure() {
        // opencode names a user abort `MessageAbortedError`. BiBCode read only
        // `data.message` and hardcoded `stopReason: "error"`, so the user's own
        // interrupt was presented as a provider failure.
        let failure = classify_opencode_error(&json!({
            "name": "MessageAbortedError",
            "data": { "message": "aborted" }
        }));
        assert_eq!(failure.state, "cancelled");
        assert_eq!(failure.stop_reason, "cancelled");
    }

    #[test]
    fn an_empty_output_length_error_still_explains_itself() {
        // `MessageOutputLengthError` carries no data, so the old code fell
        // through to the literal "OpenCode session failed." — a benign
        // max-output stop was indistinguishable from an unexplained fault.
        let failure = classify_opencode_error(&json!({ "name": "MessageOutputLengthError" }));
        assert_eq!(failure.state, "failed");
        assert_eq!(failure.stop_reason, "max_tokens");
        assert!(
            failure.message.contains("maximum output length"),
            "got {:?}",
            failure.message
        );
        assert_ne!(failure.message, "OpenCode session failed.");
    }

    #[test]
    fn an_auth_failure_is_a_permission_error_and_names_the_provider() {
        let failure = classify_opencode_error(&json!({
            "name": "ProviderAuthError",
            "data": { "message": "no key", "providerID": "anthropic" }
        }));
        assert_eq!(failure.class, "permission_error");
        assert!(
            failure.message.contains("anthropic"),
            "got {:?}",
            failure.message
        );
    }

    #[test]
    fn an_api_error_carries_the_upstream_status_and_retryability() {
        // The discriminant is literally "APIError" (all caps) in opencode's
        // generated SDK types, even though the TypeScript alias is `ApiError`.
        let failure = classify_opencode_error(&json!({
            "name": "APIError",
            "data": { "message": "overloaded", "statusCode": 529, "isRetryable": true }
        }));
        assert_eq!(failure.class, "provider_error");
        assert!(failure.message.contains("529"), "got {:?}", failure.message);
        assert!(
            failure.message.contains("retryable"),
            "got {:?}",
            failure.message
        );
    }

    #[test]
    fn an_unrecognised_error_name_still_fails_closed_with_a_message() {
        let failure = classify_opencode_error(&json!({ "name": "SomethingNewerOpenCodeSends" }));
        assert_eq!(failure.state, "failed");
        assert_eq!(failure.class, "provider_error");
        assert!(!failure.message.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use serde_json::json;
    use tokio::{net::TcpListener, sync::Mutex};

    async fn runtime_with_verified_child(
        history_support: super::EndpointSupport,
    ) -> super::OpenCodeSessionRuntime {
        let runtime = super::OpenCodeSessionRuntime::new(
            "http://127.0.0.1:1",
            "unit-thread",
            "/tmp/unit",
            None,
        );
        runtime.reset_activity_root("root").await;
        let mut activity = runtime.inner.activity.lock().await;
        activity
            .tracker
            .as_mut()
            .expect("activity tracker")
            .reconcile_children(
                "root",
                &json!([{
                    "id": "child",
                    "parentID": "root",
                    "title": "Verified child",
                    "time": { "created": 1, "updated": 1 }
                }]),
            );
        activity.history_support = history_support;
        drop(activity);
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "child-message",
                    "type": "message.updated",
                    "properties": {
                        "sessionID": "child",
                        "info": {
                            "id": "message",
                            "sessionID": "child",
                            "role": "assistant"
                        }
                    }
                }),
            )
            .await;
        runtime
    }

    /// A turn announced for a request OpenCode 1 refused (or that never reached it) settles as
    /// cancelled, so its start does not wait for a completion that never comes.
    #[tokio::test]
    async fn a_failed_opencode_1_request_settles_the_turn_it_announced() {
        let app = Router::new()
            .route(
                "/session/{session_id}/prompt_async",
                post(|| async { StatusCode::BAD_REQUEST }),
            )
            .route(
                "/session/{session_id}/command",
                post(|| async { StatusCode::BAD_REQUEST }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "unit-thread",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("session-1".to_owned());

        runtime
            .send_turn(Some("hello"), Vec::new(), Some("delivery-1"))
            .await
            .expect_err("the prompt is refused");
        runtime
            .send_command("review", "", Some("delivery-2"))
            .await
            .expect_err("the command is refused");

        for _ in 0..2 {
            let started = runtime.next_event().await.expect("turn.started");
            assert_eq!(started.event_type, "turn.started");
            let completed = runtime.next_event().await.expect("turn.completed");
            assert_eq!(completed.event_type, "turn.completed");
            assert_eq!(completed.turn_id, started.turn_id);
            assert_eq!(completed.payload["state"], "cancelled");
        }
        assert!(runtime.inner.active_turn_id.lock().await.is_none());
        server.abort();
    }

    /// A gateway error can follow acceptance (OpenCode behind a proxy), so it keeps the turn too.
    #[tokio::test]
    async fn a_gateway_error_keeps_the_turn_it_announced() {
        let app = Router::new().route(
            "/session/{session_id}/prompt_async",
            post(|| async { StatusCode::GATEWAY_TIMEOUT }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "unit-thread",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("session-1".to_owned());

        runtime
            .send_turn(Some("hello"), Vec::new(), Some("delivery-1"))
            .await
            .expect_err("the gateway timed out");
        let started = runtime.next_event().await.expect("turn.started");
        assert_eq!(started.event_type, "turn.started");
        assert_no_queued_event(&runtime).await;
        assert_eq!(
            runtime.inner.active_turn_id.lock().await.as_deref(),
            started.turn_id.as_deref()
        );
        server.abort();
    }

    /// A request whose connection drops after it was sent may have been accepted: its turn stays
    /// active, for OpenCode's own events to end, rather than being settled as not sent.
    #[tokio::test]
    async fn an_ambiguous_opencode_request_keeps_the_turn_it_announced() {
        use tokio::io::AsyncReadExt;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            // Read the request, then drop the connection without answering.
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = [0_u8; 4096];
            let _ = socket.read(&mut buffer).await;
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "unit-thread",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("session-1".to_owned());

        let turn = runtime
            .send_turn(Some("hello"), Vec::new(), Some("delivery-1"))
            .await
            .expect_err("the answer is lost");
        let started = runtime.next_event().await.expect("turn.started");
        assert_eq!(started.event_type, "turn.started", "{turn:?}");
        assert_no_queued_event(&runtime).await;
        assert_eq!(
            runtime.inner.active_turn_id.lock().await.as_deref(),
            started.turn_id.as_deref()
        );
        server.abort();
    }

    #[tokio::test]
    async fn prompt_async_serializes_the_delivery_key_as_message_id() {
        type Bodies = Arc<Mutex<Vec<serde_json::Value>>>;

        async fn prompt(
            State(bodies): State<Bodies>,
            Json(body): Json<serde_json::Value>,
        ) -> StatusCode {
            bodies.lock().await.push(body);
            StatusCode::NO_CONTENT
        }

        let bodies: Bodies = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/session/{session_id}/prompt_async", post(prompt))
            .with_state(bodies.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "unit-thread",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("session-1".to_owned());

        runtime
            .send_turn(Some("hello"), Vec::new(), Some("delivery-1"))
            .await
            .expect("send turn");
        // Durable delivery keys are UUIDs; OpenCode 1.18 rejects a `messageID` that does not
        // start with `msg` (HTTP 400), so the key becomes a deterministic `msg_` id.
        runtime
            .send_turn(
                Some("hello"),
                Vec::new(),
                Some("e310d932-acde-44f5-aa48-2a07b9c93343"),
            )
            .await
            .expect("send turn");

        assert_eq!(
            bodies.lock().await.as_slice(),
            [
                json!({
                    "sessionID": "session-1",
                    "messageID": "msg_delivery1",
                    "parts": [{"type": "text", "text": "hello"}]
                }),
                json!({
                    "sessionID": "session-1",
                    "messageID": "msg_e310d932acde44f5aa482a07b9c93343",
                    "parts": [{"type": "text", "text": "hello"}]
                })
            ]
        );
        server.abort();
    }

    /// Delivers a turn under a UUID delivery key to a real OpenCode 1 server and reads it back.
    /// Set `BIBCODE_OPENCODE_V1_LIVE` (the server URL), `BIBCODE_OPENCODE_V1_PASSWORD` and
    /// `BIBCODE_OPENCODE_V1_MODEL`, a scripted local endpoint so no provider credits are spent.
    #[tokio::test]
    #[ignore = "needs a live OpenCode 1 server"]
    async fn live_opencode_1_durable_delivery() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
        let runtime =
            super::OpenCodeSessionRuntime::new_with_options_reconciliation_revision_and_agent_activity(
                &var("BIBCODE_OPENCODE_V1_LIVE"),
                "live-thread",
                "/tmp",
                Some(&var("BIBCODE_OPENCODE_V1_MODEL")),
                Some(&var("BIBCODE_OPENCODE_V1_PASSWORD")),
                None,
                0,
                false,
            )
            .expect("runtime");
        runtime.start().await.expect("session starts");
        let key = uuid::Uuid::new_v4().to_string();
        runtime
            .send_turn(Some("SAY_HELLO please"), Vec::new(), Some(&key))
            .await
            .expect("OpenCode 1 accepts the delivery");
        // `prompt_async` stores the message after it answers.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !runtime.message_exists(&key).await.expect("lookup") {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the delivered message reads back");
        assert!(
            !runtime
                .message_exists(&uuid::Uuid::new_v4().to_string())
                .await
                .expect("lookup")
        );
        runtime.stop().await.expect("stop");
    }

    /// A delivery sent before BiBCode derived `msg_` ids carried the raw key as its OpenCode 1
    /// message id (accepted before 1.18). Reconciliation finds it under either form, so it is not
    /// sent again; OpenCode 1.18 and later refuse a raw id (HTTP 400), which means no such
    /// message exists there.
    #[tokio::test]
    async fn reconciliation_finds_a_delivery_under_its_legacy_raw_id() {
        const LEGACY: &str = "e310d932-acde-44f5-aa48-2a07b9c93343";
        const REFUSED: &str = "3f0c2a5e-0000-4000-8000-000000000001";
        const ABSENT: &str = "3f0c2a5e-0000-4000-8000-000000000002";
        async fn message(
            Path((_session_id, message_id)): Path<(String, String)>,
        ) -> impl IntoResponse {
            if message_id == LEGACY {
                return Json(json!({
                    "info": { "id": LEGACY, "sessionID": "session-1", "role": "user" },
                    "parts": []
                }))
                .into_response();
            }
            if message_id == REFUSED {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "name": "BadRequest", "data": { "kind": "Params" } })),
                )
                    .into_response();
            }
            StatusCode::NOT_FOUND.into_response()
        }

        let app = Router::new().route("/session/{session_id}/message/{message_id}", get(message));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "unit-thread",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("session-1".to_owned());

        assert!(runtime.message_exists(LEGACY).await.expect("lookup"));
        assert!(!runtime.message_exists(REFUSED).await.expect("lookup"));
        assert!(!runtime.message_exists(ABSENT).await.expect("lookup"));
        server.abort();
    }

    #[tokio::test]
    async fn exact_message_readback_distinguishes_found_absent_and_unavailable() {
        async fn message(
            Path((_session_id, message_id)): Path<(String, String)>,
        ) -> impl IntoResponse {
            match message_id.as_str() {
                "msg_deliveryfound" => Json(json!({
                    "info": {
                        "id": "msg_deliveryfound",
                        "sessionID": "session-1",
                        "role": "user"
                    },
                    "parts": []
                }))
                .into_response(),
                "msg_deliveryabsent" | "delivery-absent" => StatusCode::NOT_FOUND.into_response(),
                "msg_deliverymalformed" => Json(json!({})).into_response(),
                "msg_deliveryassistant" => Json(json!({
                    "info": {
                        "id": "msg_deliveryassistant",
                        "sessionID": "session-1",
                        "role": "assistant"
                    },
                    "parts": []
                }))
                .into_response(),
                "msg_deliverysystem" => Json(json!({
                    "info": {
                        "id": "msg_deliverysystem",
                        "sessionID": "session-1",
                        "role": "system"
                    },
                    "parts": []
                }))
                .into_response(),
                "msg_deliverymissingrole" => Json(json!({
                    "info": {
                        "id": "msg_deliverymissingrole",
                        "sessionID": "session-1"
                    },
                    "parts": []
                }))
                .into_response(),
                "msg_deliverynonstringrole" => Json(json!({
                    "info": {
                        "id": "msg_deliverynonstringrole",
                        "sessionID": "session-1",
                        "role": 7
                    },
                    "parts": []
                }))
                .into_response(),
                _ => StatusCode::BAD_GATEWAY.into_response(),
            }
        }

        let app = Router::new().route("/session/{session_id}/message/{message_id}", get(message));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "unit-thread",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("session-1".to_owned());

        assert!(runtime.message_exists("delivery-found").await.unwrap());
        assert!(!runtime.message_exists("delivery-absent").await.unwrap());
        assert!(runtime.message_exists("delivery-malformed").await.is_err());
        for message_id in [
            "delivery-assistant",
            "delivery-system",
            "delivery-missing-role",
            "delivery-non-string-role",
        ] {
            assert!(
                runtime.message_exists(message_id).await.is_err(),
                "{message_id} must not reconcile as the durable user message"
            );
        }
        assert!(
            runtime
                .message_exists("delivery-unavailable")
                .await
                .is_err()
        );
        server.abort();
    }

    async fn assert_no_queued_event(runtime: &super::OpenCodeSessionRuntime) {
        assert!(matches!(
            runtime.inner.events_rx.lock().await.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    async fn next_activity_event(
        runtime: &super::OpenCodeSessionRuntime,
    ) -> super::OpenCodeRuntimeEvent {
        loop {
            let event = runtime.next_event().await.expect("runtime event");
            if !event.activity.is_empty() {
                return event;
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn lone_child_snapshot_flushes_at_coalescing_boundary_when_history_is_unsupported() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "child-snapshot",
                    "type": "message.part.updated",
                    "properties": {
                        "sessionID": "child",
                        "part": {
                            "id": "part",
                            "sessionID": "child",
                            "messageID": "message",
                            "type": "text",
                            "text": "lone snapshot"
                        }
                    }
                }),
            )
            .await;

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(99)).await;
        assert_no_queued_event(&runtime).await;
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        let event = runtime
            .inner
            .events_rx
            .lock()
            .await
            .try_recv()
            .expect("coalesced snapshot activity");
        assert_eq!(event.event_type, "activity.native");
        assert!(event.native_event_id.is_some());
        assert!(matches!(
            event.activity.as_slice(),
            [crate::activity::ProviderActivityMutation::AppendEntry(entry)]
                if entry.detail.as_deref() == Some("lone snapshot")
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn lone_child_delta_flushes_when_history_is_transiently_unavailable() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unknown).await;
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "child-delta",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "lone delta"
                    }
                }),
            )
            .await;

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        let event = runtime
            .inner
            .events_rx
            .lock()
            .await
            .try_recv()
            .expect("coalesced delta activity");
        assert!(matches!(
            event.activity.as_slice(),
            [crate::activity::ProviderActivityMutation::AppendEntry(entry)]
                if entry.detail.as_deref() == Some("lone delta")
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn empty_history_does_not_strand_lone_child_text() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Supported).await;
        runtime
            .inner
            .activity
            .lock()
            .await
            .tracker
            .as_mut()
            .expect("activity tracker")
            .handle_history("child", &json!([]));
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "child-delta-empty-history",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "after empty history"
                    }
                }),
            )
            .await;

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert!(runtime.inner.events_rx.lock().await.try_recv().is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn rescheduled_child_text_flush_emits_one_batch_from_latest_generation() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        for (id, delta) in [("first", "first "), ("second", "second")] {
            runtime
                .handle_sse_event(
                    "root",
                    json!({
                        "id": id,
                        "type": "message.part.delta",
                        "properties": {
                            "sessionID": "child",
                            "messageID": "message",
                            "partID": "part",
                            "field": "text",
                            "delta": delta
                        }
                    }),
                )
                .await;
            tokio::task::yield_now().await;
            if id == "first" {
                tokio::time::advance(std::time::Duration::from_millis(50)).await;
            }
        }

        tokio::time::advance(std::time::Duration::from_millis(99)).await;
        assert_no_queued_event(&runtime).await;
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        let event = runtime
            .inner
            .events_rx
            .lock()
            .await
            .try_recv()
            .expect("latest generation flush");
        assert_eq!(event.activity.len(), 2);
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_no_queued_event(&runtime).await;
    }

    #[tokio::test(start_paused = true)]
    async fn root_reset_and_stop_cancel_pending_child_text_flushes() {
        let reset_runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        reset_runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "before-reset",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "must not escape reset"
                    }
                }),
            )
            .await;
        tokio::task::yield_now().await;
        reset_runtime.reset_activity_root("replacement-root").await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_no_queued_event(&reset_runtime).await;

        let stop_runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        stop_runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "before-stop",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "must not escape stop"
                    }
                }),
            )
            .await;
        tokio::task::yield_now().await;
        stop_runtime.stop().await.expect("stop");
        let stopped = stop_runtime
            .inner
            .events_rx
            .lock()
            .await
            .try_recv()
            .expect("session exited event");
        assert_eq!(stopped.event_type, "session.exited");
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert_no_queued_event(&stop_runtime).await;
    }

    #[tokio::test(start_paused = true)]
    async fn disabling_activity_cancels_pending_child_text_flush() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "before-disable",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "must not escape disabled activity"
                    }
                }),
            )
            .await;
        tokio::task::yield_now().await;

        runtime.set_agent_activity_enabled(false).await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        assert_no_queued_event(&runtime).await;
    }

    #[tokio::test(start_paused = true)]
    async fn owner_drop_releases_pending_child_text_flush_task() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "before-owner-drop",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "must not retain owner"
                    }
                }),
            )
            .await;
        tokio::task::yield_now().await;
        let owner = std::sync::Arc::downgrade(&runtime.inner);
        drop(runtime);
        tokio::task::yield_now().await;
        assert!(owner.upgrade().is_none());
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert!(owner.upgrade().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn owner_drop_after_timer_wake_cancels_blocked_flush_without_late_event() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "after-wake-owner-drop",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "must not emit after owner drop"
                    }
                }),
            )
            .await;
        tokio::task::yield_now().await;

        let gate_acquired = std::sync::Arc::new(tokio::sync::Notify::new());
        let release_gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let blocker_runtime = std::sync::Arc::downgrade(&runtime.inner);
        let blocker_gate_acquired = gate_acquired.clone();
        let blocker_release_gate = release_gate.clone();
        let blocker = tokio::spawn(async move {
            let inner = blocker_runtime
                .upgrade()
                .expect("runtime before owner drop");
            let _flush_gate = inner.activity_flush_gate.lock().await;
            blocker_gate_acquired.notify_one();
            blocker_release_gate.notified().await;
        });
        gate_acquired.notified().await;

        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        for _ in 0..10 {
            if std::sync::Arc::strong_count(&runtime.inner) >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            std::sync::Arc::strong_count(&runtime.inner),
            3,
            "external owner, gate blocker, and awakened flush task must each retain the runtime"
        );

        let (_replacement_tx, replacement_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut late_events = {
            let mut events = runtime.inner.events_rx.lock().await;
            std::mem::replace(&mut *events, replacement_rx)
        };
        let runtime_weak = std::sync::Arc::downgrade(&runtime.inner);
        drop(runtime);
        release_gate.notify_one();
        blocker.await.expect("gate blocker");
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        let late_event = late_events.try_recv();
        assert!(
            matches!(
                late_event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty
                    | tokio::sync::mpsc::error::TryRecvError::Disconnected)
            ),
            "late event after owner drop: {late_event:?}"
        );
        assert!(
            runtime_weak.upgrade().is_none(),
            "awakened flush task must release the runtime after external-owner loss"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_one_external_clone_keeps_pending_text_for_the_remaining_owner() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        let remaining_owner = runtime.clone();
        runtime
            .handle_sse_event(
                "root",
                json!({
                    "id": "drop-one-owner",
                    "type": "message.part.delta",
                    "properties": {
                        "sessionID": "child",
                        "messageID": "message",
                        "partID": "part",
                        "field": "text",
                        "delta": "remaining owner receives this"
                    }
                }),
            )
            .await;
        tokio::task::yield_now().await;
        drop(runtime);
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        let event = remaining_owner
            .inner
            .events_rx
            .lock()
            .await
            .try_recv()
            .expect("remaining external owner activity");
        assert!(matches!(
            event.activity.as_slice(),
            [crate::activity::ProviderActivityMutation::AppendEntry(entry)]
                if entry.detail.as_deref() == Some("remaining owner receives this")
        ));
    }

    #[tokio::test]
    async fn conflicting_and_missing_session_ids_cannot_mutate_root_state() {
        let runtime = runtime_with_verified_child(super::EndpointSupport::Unsupported).await;
        let turn_id = runtime.begin_turn().await;
        assert_eq!(
            runtime
                .inner
                .events_rx
                .lock()
                .await
                .try_recv()
                .expect("turn started")
                .turn_id
                .as_deref(),
            Some(turn_id.as_str())
        );

        for event in [
            json!({
                "type": "message.updated",
                "properties": {
                    "sessionID": "root",
                    "info": {
                        "id": "conflicting-child-message",
                        "sessionID": "child",
                        "role": "assistant"
                    }
                }
            }),
            json!({
                "type": "message.updated",
                "properties": {
                    "sessionID": "root",
                    "info": {
                        "id": "conflicting-foreign-message",
                        "sessionID": "foreign",
                        "role": "assistant"
                    }
                }
            }),
            json!({
                "type": "question.asked",
                "properties": {
                    "requestID": "sessionless-question",
                    "questions": [{ "header": "Choice", "question": "Continue?" }]
                }
            }),
            json!({
                "type": "permission.asked",
                "properties": {
                    "requestID": "sessionless-permission",
                    "permission": "bash"
                }
            }),
            json!({
                "type": "session.status",
                "properties": { "status": { "type": "idle" } }
            }),
        ] {
            runtime.handle_sse_event("root", event).await;
        }

        assert!(runtime.inner.assistant_message_ids.lock().await.is_empty());
        assert!(runtime.inner.pending_questions.lock().await.is_empty());
        assert!(runtime.inner.pending_permissions.lock().await.is_empty());
        assert_eq!(
            runtime.inner.active_turn_id.lock().await.as_deref(),
            Some(turn_id.as_str())
        );
        assert_no_queued_event(&runtime).await;
    }

    #[test]
    fn permission_rules_match_runtime_mode() {
        assert_eq!(
            super::build_permission_rules("full-access"),
            vec![json!({ "permission": "*", "pattern": "*", "action": "allow" })]
        );
        let approval = super::build_permission_rules("approval-required");
        assert!(approval.contains(&json!({
            "permission": "bash",
            "pattern": "*",
            "action": "ask"
        })));
        assert!(approval.contains(&json!({
            "permission": "question",
            "pattern": "*",
            "action": "allow"
        })));
    }

    #[test]
    fn reconciliation_identity_reuses_only_without_an_intervening_activity_emission() {
        let stale = super::reconciliation_scope_mutations(
            crate::activity::ActivityCapabilities::none(),
            crate::activity::ActivityObservationState::Stale,
        );
        let ordinary = super::reconciliation_scope_mutations(
            crate::activity::ActivityCapabilities::none(),
            crate::activity::ActivityObservationState::Live,
        );
        let mut identity = super::ReconciliationIdentityState::new("identity-thread", 7);

        let first = identity.reconciliation_occurrence(&stale);
        assert_eq!(
            first,
            identity.reconciliation_occurrence(&stale),
            "a consecutive transport retry reuses its native occurrence"
        );

        identity.observe_activity(&ordinary);
        assert_ne!(
            first,
            identity.reconciliation_occurrence(&stale),
            "ordinary activity is a causal barrier even when the retry mutations are identical"
        );
    }

    #[test]
    fn retained_continuation_merges_one_queued_force_history_hint_without_losing_it() {
        let mut continuation = super::ReconciliationHint {
            immediate: false,
            force_history: false,
            dirty_session_ids: std::collections::HashSet::from(["a".to_owned()]),
        };
        let queued = super::ReconciliationHint {
            immediate: true,
            force_history: true,
            dirty_session_ids: std::collections::HashSet::from(["z".to_owned()]),
        };

        super::merge_one_pending_reconciliation_hint(&mut continuation, Some(queued));

        assert!(continuation.immediate);
        assert!(continuation.force_history);
        assert!(
            continuation.dirty_session_ids.is_empty(),
            "force-history must dominate both retained and queued dirty hints"
        );
    }

    #[tokio::test]
    async fn retained_text_continuation_cannot_starve_a_queued_dirty_reconciliation() {
        type Histories = Arc<Mutex<HashMap<String, serde_json::Value>>>;

        async fn children(Path(session_id): Path<String>) -> Json<serde_json::Value> {
            if session_id == "root" {
                Json(json!([
                    {
                        "id": "a",
                        "parentID": "root",
                        "title": "A",
                        "time": { "created": 1 }
                    },
                    {
                        "id": "z",
                        "parentID": "root",
                        "title": "Z",
                        "time": { "created": 1 }
                    }
                ]))
            } else {
                Json(json!([]))
            }
        }

        async fn statuses() -> Json<serde_json::Value> {
            Json(json!({}))
        }

        async fn messages(
            Path(session_id): Path<String>,
            State(histories): State<Histories>,
        ) -> Json<serde_json::Value> {
            Json(
                histories
                    .lock()
                    .await
                    .get(&session_id)
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
        }

        let histories: Histories = Arc::new(Mutex::new(HashMap::from([
            ("a".to_owned(), json!([])),
            ("z".to_owned(), json!([])),
        ])));
        let app = Router::new()
            .route("/session/{session_id}/children", get(children))
            .route("/session/status", get(statuses))
            .route("/session/{session_id}/message", get(messages))
            .with_state(histories.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let runtime = super::OpenCodeSessionRuntime::new(
            &format!("http://{address}"),
            "queued-reconciliation",
            "/tmp/unit",
            None,
        );
        *runtime.inner.session_id.lock().await = Some("root".to_owned());
        runtime.reset_activity_root("root").await;
        runtime.request_reconciliation(true, None, true).await;
        let initial = next_activity_event(&runtime).await;
        assert!(initial.activity.iter().any(|mutation| matches!(
            mutation,
            crate::activity::ProviderActivityMutation::UpsertActor(actor)
                if actor.id == "opencode:session:z"
        )));

        {
            let mut activity = runtime.inner.activity.lock().await;
            let tracker = activity.tracker.as_mut().expect("tracker");
            tracker.handle_event(&json!({
                "id": "a-message",
                "type": "message.updated",
                "properties": {
                    "sessionID": "a",
                    "info": {
                        "id": "a-assistant",
                        "sessionID": "a",
                        "role": "assistant"
                    }
                }
            }));
            for index in 0..256 {
                let output = tracker.handle_event_at(
                    &json!({
                        "id": format!("a-delta-{index}"),
                        "type": "message.part.delta",
                        "properties": {
                            "sessionID": "a",
                            "messageID": "a-assistant",
                            "partID": "a-text",
                            "field": "text",
                            "delta": format!("a-{index}")
                        }
                    }),
                    1,
                );
                assert!(output.mutations.is_empty());
            }
        }
        histories.lock().await.insert(
            "z".to_owned(),
            json!([{
                "info": {
                    "id": "z-assistant",
                    "sessionID": "z",
                    "role": "assistant",
                    "time": { "created": 2 }
                },
                "parts": [{
                    "id": "z-text",
                    "sessionID": "z",
                    "messageID": "z-assistant",
                    "type": "text",
                    "text": "queued z history",
                    "time": { "end": 3 }
                }]
            }]),
        );

        runtime.request_reconciliation(true, Some("a"), false).await;
        let first_continuation = next_activity_event(&runtime).await;
        assert_eq!(
            first_continuation
                .activity
                .iter()
                .filter(|mutation| matches!(
                    mutation,
                    crate::activity::ProviderActivityMutation::AppendEntry(_)
                ))
                .count(),
            4
        );
        runtime.request_reconciliation(true, Some("z"), false).await;

        let mut recovered_z = false;
        for _ in 0..3 {
            let event = next_activity_event(&runtime).await;
            assert!(event.activity.len() <= 256);
            recovered_z |= event.activity.iter().any(|mutation| {
                matches!(
                    mutation,
                    crate::activity::ProviderActivityMutation::AppendEntry(entry)
                        if entry.detail.as_deref() == Some("queued z history")
                )
            });
            if recovered_z {
                break;
            }
        }
        assert!(
            recovered_z,
            "sustained retained-text continuation must yield to queued z reconciliation"
        );

        runtime.stop().await.expect("stop");
        server.abort();
    }

    #[tokio::test]
    async fn unit_runtime_events_cover_constructor_collection_and_sse_fallbacks() {
        let runtime = super::OpenCodeSessionRuntime::new(
            "http://127.0.0.1:1/",
            "unit-thread",
            "/tmp/unit",
            Some("openai/gpt-5"),
        );
        let password_runtime = super::OpenCodeSessionRuntime::new_with_password(
            "http://127.0.0.1:1",
            "password-thread",
            "/tmp/unit",
            None,
            Some("secret"),
        )
        .unwrap();
        assert!(
            password_runtime
                .request_url("/event")
                .unwrap()
                .contains("directory=")
        );
        let invalid = super::OpenCodeSessionRuntime::new(
            "not a valid base",
            "invalid-thread",
            "/tmp/unit",
            None,
        );
        assert!(invalid.request_url("/event").is_err());

        runtime
            .handle_sse_event(
                "session-1",
                json!({"type":"message.updated","properties":{"sessionID":"other"}}),
            )
            .await;
        runtime
            .handle_sse_event(
                "session-1",
                json!({"type":"message.updated","properties":{}}),
            )
            .await;
        let turn_id = runtime.begin_turn().await;
        runtime.handle_sse_event("session-1", json!({"type":"message.updated","properties":{"info":{"sessionID":"session-1","role":"assistant","id":"assistant-1"}}})).await;
        runtime.handle_sse_event("session-1", json!({"type":"message.part.updated","properties":{"part":{"sessionID":"session-1","type":"tool","messageID":"assistant-1"}}})).await;
        runtime.handle_sse_event("session-1", json!({"type":"message.part.updated","properties":{"part":{"sessionID":"session-1","type":"text","messageID":"assistant-1","text":"hello"}}})).await;
        runtime.handle_sse_event("session-1", json!({"type":"question.asked","properties":{"sessionID":"session-1","questions":[{"header":"Choice","question":"Continue?"}]}})).await;
        runtime.handle_sse_event("session-1", json!({"type":"permission.asked","properties":{"sessionID":"session-1","permission":"bash","patterns":["git status"]}})).await;
        runtime.handle_sse_event("session-1", json!({"type":"session.status","properties":{"sessionID":"session-1","status":{"type":"idle"}}})).await;
        runtime
            .handle_sse_event(
                "session-1",
                json!({"type":"session.error","properties":{"sessionID":"session-1"}}),
            )
            .await;

        let events = runtime.collect_events(5).await;
        assert_eq!(events[0].event_type, "turn.started");
        assert_eq!(events[0].turn_id.as_deref(), Some(turn_id.as_str()));
        assert_eq!(events[1].event_type, "content.delta");
        assert_eq!(events[2].event_type, "user-input.requested");
        assert_eq!(events[3].event_type, "request.opened");
        assert_eq!(events[4].event_type, "turn.completed");
    }
}
