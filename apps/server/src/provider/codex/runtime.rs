use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex as StdMutex, Weak},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use crate::activity::{
    ActivityCapabilities, ActivityHistoryRecovery, ActivityObservationState, ActivitySection,
    ActivitySectionHealth, ProviderActivityMutation,
};
#[cfg(test)]
use crate::test_support::FixtureEvent;

use super::{
    activity::{
        BackgroundSnapshotAuthority, CodexActivityTracker, MAX_TRACKED_ACTORS,
        MAX_TRACKED_WORK_ITEMS, usable_native_id,
    },
    mcp_status::{
        McpOpenCompletion, McpOpenReservation, McpStatusEffect, McpStatusHandle,
        mcp_server_status_from_notification, refresh_mcp_status_snapshot, run_actor,
    },
    model::{
        BuildTurnStartInput, CODEX_HISTORY_TURNS_PAGE_LIMIT, CODEX_REMOTE_JSON_TOO_LARGE,
        CodexProviderSnapshot, CodexRuntimeMode, ReconciliationThread,
        ThreadBackgroundTerminalsListParams, ThreadListParams, ThreadReadParams,
        ThreadReadResponse, TurnsPager, build_initialize_params, build_turn_start_params,
        build_turn_steer_params, decode_background_terminals_list_response,
        decode_thread_list_response, decode_thread_read_response, decode_turns_page,
        delivery_key_in_turns, is_recoverable_thread_resume_error, parse_model_list_response,
        parse_skills_list_response, parse_thread_snapshot, reasoning_effort_label,
        turn_option_label,
    },
    protocol::{IncomingEvent, JsonRpcConnection, ProtocolError},
};
use crate::provider::{
    OptionRefusal, RefusedOption, model_unavailable_refusal, unsupported_option_refusal,
};

const PROVIDER: &str = "codex";
const FIXED_EVENT_TIME: &str = "2026-07-10T00:00:00.000Z";
const FATAL_STDERR_SNIPPETS: &[&str] = &["failed to connect to websocket"];
const RECONCILIATION_HINT_CAPACITY: usize = 1;
const RECONCILIATION_DESCENDANT_LIMIT: u16 = 50;
const RECONCILIATION_HINT_FRESHNESS_LIMIT: usize = 256;
const RECONCILIATION_DESCENDANT_PAGE_LIMIT: usize = 8;
const RECONCILIATION_BACKGROUND_LIMIT: u16 = 128;
const RECONCILIATION_BACKGROUND_PAGE_LIMIT: usize = 8;
const RECONCILIATION_MUTATION_LIMIT: usize = 256;
const RECONCILIATION_SCOPE_MUTATION_COUNT: usize = 2;
const RECONCILIATION_PENDING_MUTATION_LIMIT: usize =
    MAX_TRACKED_ACTORS * 3 + MAX_TRACKED_WORK_ITEMS * 3 + RECONCILIATION_MUTATION_LIMIT;
const MCP_STATUS_PRE_ROOT_BUFFER_LIMIT: usize = 64;

#[derive(Clone, Debug)]
pub struct CodexSessionOptions {
    pub version: String,
    pub thread_id: String,
    pub cwd: String,
    pub runtime_mode: CodexRuntimeMode,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub effort: Option<String>,
    pub resume_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingRequestKind {
    CommandApproval,
    FileChangeApproval,
    UserInput,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub provider: String,
    pub status: String,
    pub runtime_mode: CodexRuntimeMode,
    pub thread_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub resume_cursor: Option<String>,
    pub active_turn_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResult {
    pub thread_id: String,
    pub turn_id: String,
    pub resume_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEvent {
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
    pub activity: Vec<crate::activity::ProviderActivityMutation>,
    #[serde(skip)]
    pub(crate) activity_controls: Vec<crate::activity::ProviderActivityControlUpdate>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEventStableView {
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

impl RuntimeEvent {
    #[must_use]
    pub fn stable_view(&self) -> RuntimeEventStableView {
        RuntimeEventStableView {
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
pub enum RuntimeError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("Codex session is missing a provider thread id")]
    MissingProviderThreadId,
    #[error("Unknown pending request id {request_id}")]
    PendingRequestNotFound { request_id: String },
    #[error("Invalid Codex payload: {message}")]
    InvalidPayload { message: String },
    /// Codex's model list refuses the turn: it does not offer the session's model, or the model
    /// does not advertise one of the turn's option values. The same request is refused again on
    /// every retry.
    #[error("{0}")]
    UnsupportedOption(RefusedOption),
    /// The thread reports no `historyMode` (a Codex that predates history modes), or Codex
    /// rejects `thread/turns/list` or `thread/revert` as missing (`thread/revert` arrived in
    /// 0.148). Codex 0.151 is the first to create the paginated threads it rewinds.
    #[error(
        "This Codex version can't rewind conversations. Update Codex to 0.151.0 or later, then start a new thread to use Revert."
    )]
    RewindNeedsNewerCodex,
    /// The thread has legacy history, which `thread/revert` rejects.
    #[error(
        "This conversation uses Codex's older history format, which can't be rewound. Start a new thread on Codex 0.151.0 or later to use Revert."
    )]
    RewindNeedsNewThread,
}

impl OptionRefusal for RuntimeError {
    fn refused_option(&self) -> Option<&RefusedOption> {
        match self {
            Self::UnsupportedOption(refused) => Some(refused),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct CodexSessionRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    options: CodexSessionOptions,
    turn_options: Mutex<CodexTurnOptions>,
    connection: Mutex<JsonRpcConnection>,
    session: Mutex<ProviderSession>,
    events_tx: mpsc::UnboundedSender<RuntimeEvent>,
    events_rx: Mutex<mpsc::UnboundedReceiver<RuntimeEvent>>,
    event_counter: Mutex<u64>,
    pending_requests: Mutex<HashMap<String, PendingRequest>>,
    task: StdMutex<Option<JoinHandle<()>>>,
    reconciliation_hint_tx: mpsc::Sender<()>,
    reconciliation_pending_hint: StdMutex<Option<ReconciliationHint>>,
    reconciliation_task: StdMutex<Option<JoinHandle<()>>>,
    reconciliation_cancellation: CancellationToken,
    explicit_close: Mutex<bool>,
    /// The provider thread's `historyMode`; only a `paginated` thread can be rewound.
    history_mode: Mutex<Option<String>>,
    activity: Mutex<RuntimeActivityState>,
    mcp_status: McpStatusHandle,
    mcp_opening: Mutex<Option<McpOpenReservation>>,
    mcp_status_actor_task: StdMutex<Option<JoinHandle<()>>>,
    mcp_status_effect_task: StdMutex<Option<JoinHandle<()>>>,
    #[cfg(test)]
    mcp_status_publication_barrier: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    #[cfg(test)]
    mcp_status_completion_barrier: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    #[cfg(test)]
    startup_response_events: StdMutex<Option<(Arc<FixtureEvent>, Arc<FixtureEvent>)>>,
}

#[derive(Clone, Default)]
struct CodexTurnOptions {
    service_tier: Option<String>,
    effort: Option<String>,
}

struct RuntimeActivityState {
    agent_activity_enabled: bool,
    tracker: CodexActivityTracker,
    next_receive_sequence: u128,
    next_reconciliation_sequence: u64,
    reconciled_root_thread_id: Option<String>,
    thread_list_support: ReconciliationMethodSupport,
    thread_read_support: ReconciliationMethodSupport,
    background_list_support: ReconciliationMethodSupport,
    warned_incompatible_methods: HashSet<&'static str>,
    warned_undecodable_replies: HashSet<(&'static str, &'static str)>,
    capabilities: ActivityCapabilities,
    reconciliation_epoch: u64,
    reconciliation_pass_cancellation: CancellationToken,
    pending_hinted_descendants: VecDeque<String>,
    pending_hinted_descendant_ids: HashSet<String>,
    resolved_hinted_descendant_ids: HashSet<String>,
    hint_source_versions: HashMap<String, Option<u64>>,
    pending_reconciliation_mutations: Vec<ProviderActivityMutation>,
    pending_reconciliation_controls: Vec<StagedReconciliationControl>,
    control_revision: u64,
    control_revision_exhausted: bool,
    live_control_revisions: HashMap<ActivityControlSubject, u64>,
    pending_reconciliation_topology_valid: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ActivityControlSubject {
    Actor(String),
    Work(String),
}

#[derive(Clone, Debug)]
struct StagedReconciliationControl {
    update: crate::activity::ProviderActivityControlUpdate,
    observed_control_revision: u64,
}

fn activity_control_subject(
    update: &crate::activity::ProviderActivityControlUpdate,
) -> ActivityControlSubject {
    match update {
        crate::activity::ProviderActivityControlUpdate::ActorTarget { actor_id, .. } => {
            ActivityControlSubject::Actor(actor_id.clone())
        }
        crate::activity::ProviderActivityControlUpdate::WorkTarget { work_item_id, .. } => {
            ActivityControlSubject::Work(work_item_id.clone())
        }
    }
}

impl RuntimeActivityState {
    fn refresh_history_recovery_capability(&mut self) {
        self.capabilities.history_recovery =
            history_recovery_for_support(self.thread_list_support, self.thread_read_support);
    }

    fn enqueue_hinted_descendants(&mut self, native_thread_ids: &[String]) {
        if self.thread_read_support == ReconciliationMethodSupport::Unsupported {
            return;
        }
        for native_thread_id in native_thread_ids {
            if native_thread_id.is_empty()
                || self
                    .pending_hinted_descendant_ids
                    .contains(native_thread_id)
                || self.pending_hinted_descendants.len()
                    == usize::from(RECONCILIATION_DESCENDANT_LIMIT)
            {
                continue;
            }
            self.pending_hinted_descendants
                .push_back(native_thread_id.clone());
            self.pending_hinted_descendant_ids
                .insert(native_thread_id.clone());
        }
    }

    fn resolved_hints_for_source(
        &self,
        source_thread_id: &str,
        source_version: Option<u64>,
    ) -> HashSet<String> {
        if self.hint_source_versions.get(source_thread_id) == Some(&source_version) {
            return self.resolved_hinted_descendant_ids.clone();
        }
        HashSet::new()
    }

    fn record_hint_source_version(
        &mut self,
        source_thread_id: &str,
        source_version: Option<u64>,
        hinted_descendant_ids: &[String],
    ) {
        if self.hint_source_versions.get(source_thread_id) != Some(&source_version) {
            for native_thread_id in hinted_descendant_ids {
                self.resolved_hinted_descendant_ids.remove(native_thread_id);
            }
        }
        if !self.hint_source_versions.contains_key(source_thread_id)
            && self.hint_source_versions.len() == RECONCILIATION_HINT_FRESHNESS_LIMIT
        {
            self.hint_source_versions.clear();
        }
        self.hint_source_versions
            .insert(source_thread_id.to_owned(), source_version);
    }

    fn mark_hinted_descendant_resolved(&mut self, native_thread_id: &str) {
        if self.resolved_hinted_descendant_ids.len() == RECONCILIATION_HINT_FRESHNESS_LIMIT
            && !self
                .resolved_hinted_descendant_ids
                .contains(native_thread_id)
        {
            self.resolved_hinted_descendant_ids.clear();
            self.hint_source_versions.clear();
        }
        self.resolved_hinted_descendant_ids
            .insert(native_thread_id.to_owned());
    }

    fn pending_hinted_descendants_snapshot(&self) -> Vec<String> {
        self.pending_hinted_descendants.iter().cloned().collect()
    }

    fn remove_pending_hinted_descendant(&mut self, native_thread_id: &str) {
        if !self.pending_hinted_descendant_ids.remove(native_thread_id) {
            return;
        }
        self.pending_hinted_descendants
            .retain(|pending| pending != native_thread_id);
    }

    fn retain_retry_source(&mut self, native_thread_id: &str) {
        if self
            .pending_hinted_descendant_ids
            .contains(native_thread_id)
        {
            return;
        }
        if self.pending_hinted_descendants.len() == usize::from(RECONCILIATION_DESCENDANT_LIMIT)
            && let Some(displaced) = self.pending_hinted_descendants.pop_back()
        {
            self.pending_hinted_descendant_ids.remove(&displaced);
        }
        self.enqueue_hinted_descendants(&[native_thread_id.to_owned()]);
    }

    fn clear_pending_hinted_descendants(&mut self) {
        self.pending_hinted_descendants.clear();
        self.pending_hinted_descendant_ids.clear();
    }

    fn clear_hint_freshness(&mut self) {
        self.resolved_hinted_descendant_ids.clear();
        self.hint_source_versions.clear();
    }

    fn stage_reconciliation_mutations(
        &mut self,
        mutations: impl IntoIterator<Item = ProviderActivityMutation>,
    ) {
        self.pending_reconciliation_mutations.extend(mutations);
        let pending = bounded_pending_reconciliation_mutations(std::mem::take(
            &mut self.pending_reconciliation_mutations,
        ));
        self.pending_reconciliation_mutations = pending.mutations;
        self.pending_reconciliation_topology_valid = pending.topology_valid;
    }

    fn stage_reconciliation_controls(
        &mut self,
        observed_control_revision: u64,
        controls: impl IntoIterator<Item = crate::activity::ProviderActivityControlUpdate>,
    ) {
        if self.control_revision_exhausted {
            return;
        }
        for update in controls {
            let subject = activity_control_subject(&update);
            if self
                .live_control_revisions
                .get(&subject)
                .is_some_and(|revision| *revision > observed_control_revision)
            {
                continue;
            }
            self.pending_reconciliation_controls
                .retain(|pending| activity_control_subject(&pending.update) != subject);
            if self.pending_reconciliation_controls.len() < RECONCILIATION_MUTATION_LIMIT {
                self.pending_reconciliation_controls
                    .push(StagedReconciliationControl {
                        update,
                        observed_control_revision,
                    });
            }
        }
    }

    fn record_live_controls(
        &mut self,
        controls: &[crate::activity::ProviderActivityControlUpdate],
    ) {
        if controls.is_empty() {
            return;
        }
        let Some(revision) = self.control_revision.checked_add(1) else {
            self.control_revision_exhausted = true;
            self.pending_reconciliation_controls.clear();
            return;
        };
        self.control_revision = revision;
        for update in controls {
            let subject = activity_control_subject(update);
            self.live_control_revisions
                .insert(subject.clone(), revision);
            self.pending_reconciliation_controls
                .retain(|pending| activity_control_subject(&pending.update) != subject);
        }
    }

    fn current_reconciliation_controls(
        &self,
    ) -> Vec<crate::activity::ProviderActivityControlUpdate> {
        if self.control_revision_exhausted {
            return Vec::new();
        }
        self.pending_reconciliation_controls
            .iter()
            .filter(|pending| {
                let subject = activity_control_subject(&pending.update);
                self.live_control_revisions
                    .get(&subject)
                    .is_none_or(|revision| *revision <= pending.observed_control_revision)
            })
            .map(|pending| pending.update.clone())
            .collect()
    }

    fn reconciliation_actor_was_superseded(
        &self,
        observed_control_revision: u64,
        native_thread_id: &str,
    ) -> bool {
        if self.control_revision_exhausted {
            return true;
        }
        let Some(actor_id) = self.tracker.actor_control_id(native_thread_id) else {
            return false;
        };
        self.live_control_revisions
            .get(&ActivityControlSubject::Actor(actor_id.to_owned()))
            .is_some_and(|revision| *revision > observed_control_revision)
    }

    fn exclude_superseded_reconciliation_hint_receivers(
        &mut self,
        observed_control_revision: u64,
        thread: &ReconciliationThread,
        excluded_native_thread_ids: &mut HashSet<String>,
    ) -> bool {
        let receiver_ids = self
            .tracker
            .validated_reconciliation_hint_receiver_ids(thread);
        let mut excluded_any = false;
        for native_thread_id in receiver_ids {
            if self
                .reconciliation_actor_was_superseded(observed_control_revision, &native_thread_id)
            {
                excluded_native_thread_ids.insert(native_thread_id.clone());
                self.retain_retry_source(&native_thread_id);
                excluded_any = true;
            }
        }
        excluded_any
    }

    fn install_root_thread_id(&mut self, native_thread_id: &str) {
        if !self.tracker.is_root_thread(native_thread_id) {
            self.clear_pending_hinted_descendants();
            self.clear_hint_freshness();
            self.pending_reconciliation_mutations.clear();
            self.pending_reconciliation_controls.clear();
            self.control_revision = 0;
            self.control_revision_exhausted = false;
            self.live_control_revisions.clear();
            self.pending_reconciliation_topology_valid = true;
        }
        self.tracker.set_root_thread_id(native_thread_id);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconciliationMethodSupport {
    Unknown,
    Supported,
    Unsupported,
}

fn history_recovery_for_support(
    list_support: ReconciliationMethodSupport,
    read_support: ReconciliationMethodSupport,
) -> ActivityHistoryRecovery {
    match (list_support, read_support) {
        (ReconciliationMethodSupport::Supported, ReconciliationMethodSupport::Supported) => {
            ActivityHistoryRecovery::Full
        }
        (ReconciliationMethodSupport::Unsupported, ReconciliationMethodSupport::Unsupported) => {
            ActivityHistoryRecovery::None
        }
        _ => ActivityHistoryRecovery::Bounded,
    }
}

#[derive(Clone, Copy, Debug)]
struct ReconciliationHint {
    immediate: bool,
    epoch: u64,
}

#[derive(Clone, Debug)]
struct ReconciliationPass {
    epoch: u64,
    root_thread_id: String,
    cancellation: CancellationToken,
    observed_control_revision: u64,
}

enum ReconciliationEmission {
    Successful {
        capabilities: ActivityCapabilities,
        mutations: Vec<ProviderActivityMutation>,
    },
    Stale,
    Warning(&'static str),
}

#[derive(Clone)]
struct PendingRequest {
    kind: PendingRequestKind,
    wire_id: Value,
    turn_id: Option<String>,
}

pub async fn probe_provider(
    connection: &JsonRpcConnection,
    version: &str,
    cwd: &str,
    custom_models: &[String],
) -> Result<CodexProviderSnapshot, RuntimeError> {
    connection
        .request("initialize", build_initialize_params(version))
        .await?;
    connection.notify_without_params("initialized").await?;
    let account = connection.request("account/read", json!({})).await?;

    let mut raw_models = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let request_payload = cursor
            .as_ref()
            .map_or_else(|| json!({}), |value| json!({ "cursor": value }));
        let response = connection.request("model/list", request_payload).await?;
        raw_models.extend(
            response
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| RuntimeError::InvalidPayload {
                    message: "model/list response missing data array".to_owned(),
                })?,
        );
        cursor = response
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let models = parse_model_list_response(&json!({ "data": raw_models }), custom_models)
        .map_err(|message| RuntimeError::InvalidPayload { message })?;

    let skills_response = connection
        .request("skills/list", json!({ "cwds": [cwd] }))
        .await?;
    let skills = parse_skills_list_response(&skills_response, cwd)
        .map_err(|message| RuntimeError::InvalidPayload { message })?;

    Ok(CodexProviderSnapshot {
        account,
        version: Some(version.to_owned()),
        models,
        skills,
    })
}

async fn run_mcp_status_effects(
    runtime: Weak<RuntimeInner>,
    handle: McpStatusHandle,
    mut effects_rx: mpsc::UnboundedReceiver<McpStatusEffect>,
    cancellation: CancellationToken,
) {
    let mut loads = JoinSet::new();
    loop {
        let effect = tokio::select! {
            biased;
            () = cancellation.cancelled() => break,
            effect = effects_rx.recv() => match effect {
                Some(effect) => effect,
                None => break,
            },
            Some(_) = loads.join_next(), if !loads.is_empty() => continue,
        };
        match effect {
            McpStatusEffect::Load {
                epoch,
                generation,
                root,
            } => {
                let Some(inner) = runtime.upgrade() else {
                    break;
                };
                let connection = inner.connection.lock().await.clone();
                drop(inner);
                let handle = handle.clone();
                loads.spawn(async move {
                    let result = refresh_mcp_status_snapshot(&connection, &root).await;
                    let _ = handle.load_finished(epoch, generation, result).await;
                });
            }
            McpStatusEffect::Snapshot(servers) => {
                let Some(inner) = runtime.upgrade() else {
                    break;
                };
                #[cfg(test)]
                if let Some((blocked, release)) =
                    inner.mcp_status_publication_barrier.lock().await.take()
                {
                    let _ = blocked.send(());
                    let _ = release.await;
                }
                CodexSessionRuntime { inner }
                    .emit(
                        "mcp.status.updated",
                        None,
                        None,
                        json!({ "servers": servers }),
                    )
                    .await;
            }
            McpStatusEffect::Warning(detail) => {
                let Some(inner) = runtime.upgrade() else {
                    break;
                };
                CodexSessionRuntime { inner }
                    .emit("runtime.warning", None, None, json!({ "message": detail }))
                    .await;
            }
            McpStatusEffect::Complete(waiters) => {
                #[cfg(test)]
                if let Some(inner) = runtime.upgrade()
                    && let Some((blocked, release)) =
                        inner.mcp_status_completion_barrier.lock().await.take()
                {
                    let _ = blocked.send(());
                    let _ = release.await;
                }
                for waiter in waiters {
                    let _ = waiter.send(Ok(()));
                }
            }
        }
    }
    loads.abort_all();
    while loads.join_next().await.is_some() {}
}

fn mcp_status_error(message: String) -> RuntimeError {
    RuntimeError::InvalidPayload { message }
}

async fn await_mcp_status_completion(
    completion: impl std::future::Future<Output = Result<Result<(), String>, oneshot::error::RecvError>>,
) -> Result<(), RuntimeError> {
    completion
        .await
        .map_err(|_| mcp_status_error("MCP status actor dropped a completion waiter".to_owned()))?
        .map_err(mcp_status_error)
}

impl CodexSessionRuntime {
    pub fn new(
        options: CodexSessionOptions,
        connection: JsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
    ) -> Self {
        Self::new_with_agent_activity_enabled(options, connection, incoming, true)
    }

    pub(crate) fn new_with_agent_activity_enabled(
        options: CodexSessionOptions,
        connection: JsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
        agent_activity_enabled: bool,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (reconciliation_hint_tx, reconciliation_hint_rx) =
            mpsc::channel(RECONCILIATION_HINT_CAPACITY);
        let reconciliation_cancellation = CancellationToken::new();
        let (mcp_status, mcp_status_rx) =
            McpStatusHandle::channel(MCP_STATUS_PRE_ROOT_BUFFER_LIMIT);
        let mcp_opening = mcp_status
            .reserve_open()
            .expect("fresh Codex MCP status mailbox accepts its opening");
        let (mcp_status_effects_tx, mcp_status_effects_rx) = mpsc::unbounded_channel();
        let turn_options = CodexTurnOptions {
            service_tier: options.service_tier.clone(),
            effort: options.effort.clone(),
        };
        let session = ProviderSession {
            provider: PROVIDER.to_owned(),
            status: "connecting".to_owned(),
            runtime_mode: options.runtime_mode,
            thread_id: options.thread_id.clone(),
            cwd: options.cwd.clone(),
            model: options.model.clone(),
            resume_cursor: options.resume_cursor.clone(),
            active_turn_id: None,
        };
        let inner = Arc::new(RuntimeInner {
            options,
            turn_options: Mutex::new(turn_options),
            connection: Mutex::new(connection.clone()),
            session: Mutex::new(session),
            events_tx,
            events_rx: Mutex::new(events_rx),
            event_counter: Mutex::new(0),
            pending_requests: Mutex::new(HashMap::new()),
            task: StdMutex::new(None),
            reconciliation_hint_tx,
            reconciliation_pending_hint: StdMutex::new(None),
            reconciliation_task: StdMutex::new(None),
            reconciliation_cancellation: reconciliation_cancellation.clone(),
            explicit_close: Mutex::new(false),
            history_mode: Mutex::new(None),
            mcp_status: mcp_status.clone(),
            mcp_opening: Mutex::new(Some(mcp_opening)),
            mcp_status_actor_task: StdMutex::new(None),
            mcp_status_effect_task: StdMutex::new(None),
            #[cfg(test)]
            mcp_status_publication_barrier: Mutex::new(None),
            #[cfg(test)]
            mcp_status_completion_barrier: Mutex::new(None),
            #[cfg(test)]
            startup_response_events: StdMutex::new(None),
            activity: Mutex::new(RuntimeActivityState {
                agent_activity_enabled,
                tracker: CodexActivityTracker::new(None),
                next_receive_sequence: 0,
                next_reconciliation_sequence: 0,
                reconciled_root_thread_id: None,
                thread_list_support: ReconciliationMethodSupport::Unknown,
                thread_read_support: ReconciliationMethodSupport::Unknown,
                background_list_support: ReconciliationMethodSupport::Unknown,
                warned_incompatible_methods: HashSet::new(),
                warned_undecodable_replies: HashSet::new(),
                capabilities: ActivityCapabilities {
                    actors: true,
                    attributed_activity: true,
                    background_work: false,
                    history_recovery: ActivityHistoryRecovery::Bounded,
                    terminal_observation: false,
                    targeted_actor_cancellation: true,
                },
                reconciliation_epoch: 0,
                reconciliation_pass_cancellation: CancellationToken::new(),
                pending_hinted_descendants: VecDeque::new(),
                pending_hinted_descendant_ids: HashSet::new(),
                resolved_hinted_descendant_ids: HashSet::new(),
                hint_source_versions: HashMap::new(),
                pending_reconciliation_mutations: Vec::new(),
                pending_reconciliation_controls: Vec::new(),
                control_revision: 0,
                control_revision_exhausted: false,
                live_control_revisions: HashMap::new(),
                pending_reconciliation_topology_valid: true,
            }),
        });
        let runtime = Self { inner };
        let actor_task = tokio::spawn(run_actor(mcp_status_rx, mcp_status_effects_tx));
        let effect_task = tokio::spawn(run_mcp_status_effects(
            Arc::downgrade(&runtime.inner),
            mcp_status,
            mcp_status_effects_rx,
            reconciliation_cancellation,
        ));
        *runtime
            .inner
            .mcp_status_actor_task
            .lock()
            .expect("Codex MCP status actor task mutex poisoned") = Some(actor_task);
        *runtime
            .inner
            .mcp_status_effect_task
            .lock()
            .expect("Codex MCP status effect task mutex poisoned") = Some(effect_task);
        runtime.start_reconciliation_worker(reconciliation_hint_rx);
        let previous = runtime.attach_incoming(connection, incoming);
        debug_assert!(previous.is_none());
        runtime
    }

    pub async fn set_agent_activity_enabled(&self, enabled: bool) {
        let root_thread_id = self.inner.session.lock().await.resume_cursor.clone();
        let should_reconcile = {
            let mut activity = self.inner.activity.lock().await;
            if activity.agent_activity_enabled == enabled {
                return;
            }
            activity.agent_activity_enabled = enabled;
            activity.reconciliation_epoch = activity.reconciliation_epoch.wrapping_add(1);
            activity.reconciliation_pass_cancellation.cancel();
            activity.reconciliation_pass_cancellation = CancellationToken::new();
            activity.tracker = CodexActivityTracker::new(root_thread_id.as_deref());
            activity.clear_pending_hinted_descendants();
            activity.clear_hint_freshness();
            activity.pending_reconciliation_mutations.clear();
            activity.pending_reconciliation_controls.clear();
            activity.control_revision = 0;
            activity.control_revision_exhausted = false;
            activity.live_control_revisions.clear();
            activity.pending_reconciliation_topology_valid = true;
            if enabled {
                activity.tracker.begin_detail_baseline();
            }
            activity.reconciled_root_thread_id = None;
            self.inner
                .reconciliation_pending_hint
                .lock()
                .expect("Codex reconciliation pending-hint mutex poisoned")
                .take();
            enabled && root_thread_id.is_some()
        };
        if should_reconcile {
            self.request_reconciliation(true).await;
        }
    }

    pub async fn set_turn_options(&self, service_tier: Option<String>, effort: Option<String>) {
        *self.inner.turn_options.lock().await = CodexTurnOptions {
            service_tier,
            effort,
        };
    }

    /// Checks the turn's options against Codex's model list, which, as in the catalog, also
    /// offers the instance's `custom_models` with the options of the first listed model.
    /// `provider_label` is the instance's label, which the refusal for a model offered by neither
    /// names.
    pub async fn validate_turn_options(
        &self,
        service_tier: Option<&str>,
        effort: Option<&str>,
        provider_label: &str,
        custom_models: &[String],
    ) -> Result<(), RuntimeError> {
        if service_tier.is_none() && effort.is_none() {
            return Ok(());
        }
        let model = self
            .inner
            .session
            .lock()
            .await
            .model
            .clone()
            .ok_or_else(|| RuntimeError::InvalidPayload {
                message: "Codex did not report the initialized model".to_owned(),
            })?;
        let connection = self.inner.connection.lock().await.clone();
        let mut data = Vec::new();
        let mut cursor = None;
        loop {
            let response = connection
                .request(
                    "model/list",
                    cursor
                        .as_ref()
                        .map_or_else(|| json!({}), |value| json!({ "cursor": value })),
                )
                .await?;
            data.extend(
                response
                    .get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| RuntimeError::InvalidPayload {
                        message: "model/list response missing data array".to_owned(),
                    })?,
            );
            cursor = response
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let models = parse_model_list_response(&json!({ "data": data }), custom_models)
            .map_err(|message| RuntimeError::InvalidPayload { message })?;
        let capabilities = models
            .into_iter()
            .find(|candidate| candidate.slug == model)
            .map(|candidate| candidate.capabilities)
            .ok_or_else(|| {
                RuntimeError::UnsupportedOption(RefusedOption::new(
                    format!("Codex did not advertise capabilities for model {model}"),
                    model_unavailable_refusal(&model, provider_label),
                ))
            })?;
        for (id, value) in [("serviceTier", service_tier), ("reasoningEffort", effort)] {
            let Some(value) = value else {
                continue;
            };
            let descriptor = capabilities
                .get("optionDescriptors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|descriptor| descriptor.get("id").and_then(Value::as_str) == Some(id));
            let supported = descriptor
                .and_then(|descriptor| descriptor.get("options"))
                .and_then(Value::as_array)
                .is_some_and(|options| {
                    options
                        .iter()
                        .any(|option| option.get("id").and_then(Value::as_str) == Some(value))
                });
            if !supported {
                let label = descriptor
                    .and_then(|descriptor| descriptor.get("label"))
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| turn_option_label(id));
                let value_label = if id == "reasoningEffort" {
                    reasoning_effort_label(value)
                } else {
                    value
                };
                return Err(RuntimeError::UnsupportedOption(RefusedOption::new(
                    format!("Codex model {model} does not advertise {id}={value}"),
                    unsupported_option_refusal(&format!("{label} {value_label}")),
                )));
            }
        }
        Ok(())
    }

    pub async fn start(&self) -> Result<ProviderSession, RuntimeError> {
        let mcp_completion = self.claim_mcp_opening().await?;
        self.start_with_mcp_opening(mcp_completion).await
    }

    #[cfg(test)]
    pub(crate) fn observe_startup_responses_for_test(
        &self,
        initialize_response_ready: Arc<FixtureEvent>,
        thread_response_ready: Arc<FixtureEvent>,
    ) {
        *self
            .inner
            .startup_response_events
            .lock()
            .expect("Codex startup response event mutex poisoned") =
            Some((initialize_response_ready, thread_response_ready));
    }

    #[cfg(test)]
    pub(crate) async fn observe_initialize_request_queued_for_test(
        &self,
        event: Arc<FixtureEvent>,
    ) {
        self.inner
            .connection
            .lock()
            .await
            .observe_request_queued_for_test("initialize", event);
    }

    #[cfg(test)]
    fn publish_initialize_response_ready(&self) {
        if let Some((event, _)) = self
            .inner
            .startup_response_events
            .lock()
            .expect("Codex startup response event mutex poisoned")
            .as_ref()
        {
            event.publish();
        }
    }

    #[cfg(test)]
    fn publish_thread_response_ready(&self) {
        if let Some((_, event)) = self
            .inner
            .startup_response_events
            .lock()
            .expect("Codex startup response event mutex poisoned")
            .as_ref()
        {
            event.publish();
        }
    }

    async fn claim_mcp_opening(&self) -> Result<McpOpenCompletion, RuntimeError> {
        let reservation = self.inner.mcp_opening.lock().await.take();
        let completion = if let Some(reservation) = reservation {
            reservation
                .into_completion()
                .await
                .map_err(mcp_status_error)?
        } else {
            self.inner
                .mcp_status
                .begin_open()
                .await
                .map_err(mcp_status_error)?
        };
        Ok(completion)
    }

    async fn start_with_mcp_opening(
        &self,
        mcp_completion: McpOpenCompletion,
    ) -> Result<ProviderSession, RuntimeError> {
        self.emit("session.connecting", None, None, json!({})).await;
        let connection = self.inner.connection.lock().await.clone();
        let open_payload = json!({
            "cwd": self.inner.options.cwd,
            "approvalPolicy": match self.inner.options.runtime_mode {
                CodexRuntimeMode::ApprovalRequired => "untrusted",
                CodexRuntimeMode::AutoAcceptEdits => "on-request",
                CodexRuntimeMode::FullAccess => "never",
            },
            "sandbox": match self.inner.options.runtime_mode {
                CodexRuntimeMode::ApprovalRequired => "read-only",
                CodexRuntimeMode::AutoAcceptEdits => "workspace-write",
                CodexRuntimeMode::FullAccess => "danger-full-access",
            },
            "model": self.inner.options.model,
            "serviceTier": self.inner.options.service_tier,
        });

        let resume_thread_id = self
            .inner
            .session
            .lock()
            .await
            .resume_cursor
            .clone()
            .or_else(|| self.inner.options.resume_cursor.clone());

        let open_result = async {
            connection
                .request(
                    "initialize",
                    build_initialize_params(&self.inner.options.version),
                )
                .await?;
            #[cfg(test)]
            self.publish_initialize_response_ready();
            connection.notify_without_params("initialized").await?;
            let opened = if let Some(resume_thread_id) = resume_thread_id {
                match connection
                    .request(
                        "thread/resume",
                        json!({
                            "threadId": resume_thread_id,
                            "cwd": self.inner.options.cwd,
                            "approvalPolicy": match self.inner.options.runtime_mode {
                                CodexRuntimeMode::ApprovalRequired => "untrusted",
                                CodexRuntimeMode::AutoAcceptEdits => "on-request",
                                CodexRuntimeMode::FullAccess => "never",
                            },
                            "sandbox": match self.inner.options.runtime_mode {
                                CodexRuntimeMode::ApprovalRequired => "read-only",
                                CodexRuntimeMode::AutoAcceptEdits => "workspace-write",
                                CodexRuntimeMode::FullAccess => "danger-full-access",
                            },
                            "model": self.inner.options.model,
                            "serviceTier": self.inner.options.service_tier,
                        }),
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(ProtocolError::RemoteRequest { message, .. })
                        if is_recoverable_thread_resume_error(&message) =>
                    {
                        connection.request("thread/start", open_payload).await?
                    }
                    Err(error) => return Err(error.into()),
                }
            } else {
                connection.request("thread/start", open_payload).await?
            };
            #[cfg(test)]
            self.publish_thread_response_ready();
            let provider_thread_id = opened
                .get("thread")
                .and_then(|thread| thread.get("id"))
                .and_then(Value::as_str)
                .ok_or_else(|| RuntimeError::InvalidPayload {
                    message: "thread/start response missing thread.id".to_owned(),
                })?
                .to_owned();
            Ok::<_, RuntimeError>((opened, provider_thread_id))
        }
        .await;
        let (opened, provider_thread_id) = match open_result {
            Ok(opened) => opened,
            Err(error) => {
                mcp_completion.cancel().await;
                return Err(error);
            }
        };

        let mut session = self.inner.session.lock().await;
        session.status = "ready".to_owned();
        session.cwd = opened
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or(&self.inner.options.cwd)
            .to_owned();
        session.model = opened
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.inner.options.model.clone());
        session.resume_cursor = Some(provider_thread_id.clone());
        drop(session);
        *self.inner.history_mode.lock().await = thread_history_mode(opened.get("thread"));
        let mut activity = self.inner.activity.lock().await;
        if activity.agent_activity_enabled {
            activity.install_root_thread_id(&provider_thread_id);
        }
        drop(activity);

        self.inner
            .mcp_status
            .bind_root(provider_thread_id)
            .await
            .map_err(mcp_status_error)?;
        await_mcp_status_completion(mcp_completion).await?;
        self.emit("session.ready", None, None, json!({})).await;
        Ok(self.inner.session.lock().await.clone())
    }

    pub async fn refresh_mcp_status(&self) {
        let completion = match self.inner.mcp_status.refresh().await {
            Ok(completion) => completion,
            Err(message) => {
                self.emit("runtime.warning", None, None, json!({ "message": message }))
                    .await;
                return;
            }
        };
        if let Err(error) = await_mcp_status_completion(completion).await {
            self.emit(
                "runtime.warning",
                None,
                None,
                json!({ "message": error.to_string() }),
            )
            .await;
        }
    }

    pub async fn reconnect(
        &self,
        connection: JsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
    ) -> Result<ProviderSession, RuntimeError> {
        *self.inner.explicit_close.lock().await = false;
        {
            let mut activity = self.inner.activity.lock().await;
            activity.reconciliation_epoch = activity.reconciliation_epoch.wrapping_add(1);
            activity.reconciliation_pass_cancellation.cancel();
            activity.reconciliation_pass_cancellation = CancellationToken::new();
            activity.hint_source_versions.clear();
        }
        if let Some(previous) = self.detach_incoming() {
            let _ = previous.await;
        }
        let mcp_completion = self.claim_mcp_opening().await?;
        let previous = self.attach_incoming(connection.clone(), incoming);
        debug_assert!(previous.is_none());
        *self.inner.connection.lock().await = connection;
        let resume_cursor = self.inner.session.lock().await.resume_cursor.clone();
        self.inner.options_resume_cursor_set(resume_cursor).await;
        let session = self.start_with_mcp_opening(mcp_completion).await?;
        self.request_reconciliation_immediate().await;
        Ok(session)
    }

    pub async fn send_turn(
        &self,
        input: Option<String>,
        attachments: Vec<Value>,
        interaction_mode: Option<String>,
        client_user_message_id: Option<String>,
    ) -> Result<TurnStartResult, RuntimeError> {
        let provider_thread_id = self.provider_thread_id().await?;
        let session = self.inner.session.lock().await.clone();
        let turn_options = self.inner.turn_options.lock().await.clone();
        let payload = build_turn_start_params(&BuildTurnStartInput {
            thread_id: provider_thread_id.clone(),
            runtime_mode: self.inner.options.runtime_mode,
            client_user_message_id,
            prompt: input,
            attachments,
            model: session.model.clone(),
            service_tier: turn_options.service_tier,
            effort: turn_options.effort,
            interaction_mode,
        });
        let response = self
            .inner
            .connection
            .lock()
            .await
            .clone()
            .request("turn/start", payload)
            .await?;
        let turn_id = response
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| RuntimeError::InvalidPayload {
                message: "turn/start response missing turn.id".to_owned(),
            })?
            .to_owned();
        let mut session = self.inner.session.lock().await;
        if session.status != "running" || session.active_turn_id.is_none() {
            session.active_turn_id = Some(turn_id.clone());
        }
        session.status = "running".to_owned();
        Ok(TurnStartResult {
            thread_id: session.thread_id.clone(),
            turn_id,
            resume_cursor: session.resume_cursor.clone(),
        })
    }

    pub async fn steer_turn(
        &self,
        input: Option<String>,
        attachments: Vec<Value>,
        expected_turn_id: String,
        client_user_message_id: Option<String>,
    ) -> Result<String, RuntimeError> {
        let thread_id = self.provider_thread_id().await?;
        let payload = build_turn_steer_params(
            &thread_id,
            input.as_deref(),
            &attachments,
            &expected_turn_id,
            client_user_message_id.as_deref(),
        );
        let response = self
            .inner
            .connection
            .lock()
            .await
            .clone()
            .request("turn/steer", payload)
            .await?;
        response
            .get("turnId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| RuntimeError::InvalidPayload {
                message: "turn/steer response missing turnId".to_owned(),
            })
    }

    /// Pages the thread newest first for the user message delivered with `delivery_key`. Absent
    /// is proven only by the last page; a thread that cannot be paged to its end is an error, so
    /// a delivered message is never sent twice.
    pub async fn delivery_exists(&self, delivery_key: &str) -> Result<bool, RuntimeError> {
        let provider_thread_id = self.provider_thread_id().await?;
        let connection = self.inner.connection.lock().await.clone();
        let invalid = |message| RuntimeError::InvalidPayload { message };
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        for _ in 0..DELIVERY_READBACK_PAGE_LIMIT {
            let mut params = json!({
                "threadId": provider_thread_id,
                "limit": CODEX_HISTORY_TURNS_PAGE_LIMIT,
                "sortDirection": "desc",
                "itemsView": "full",
            });
            if let Some(cursor) = &cursor {
                params["cursor"] = json!(cursor);
            }
            let page = connection.request("thread/turns/list", params).await?;
            let (turns, next_cursor) = decode_turns_page(page).map_err(invalid)?;
            if delivery_key_in_turns(&turns, delivery_key).map_err(invalid)? {
                return Ok(true);
            }
            let Some(next_cursor) = next_cursor else {
                return Ok(false);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(invalid(
                    "thread/turns/list repeated a cursor before the last page".to_owned(),
                ));
            }
            cursor = Some(next_cursor);
        }
        Err(invalid(format!(
            "thread/turns/list had more than {DELIVERY_READBACK_PAGE_LIMIT} pages"
        )))
    }

    pub async fn set_goal(&self, objective: &str) -> Result<(), RuntimeError> {
        let objective = objective.trim();
        if objective.is_empty() || objective.chars().count() > 4_000 {
            return Err(RuntimeError::InvalidPayload {
                message: "goal objective must contain between 1 and 4000 characters".to_owned(),
            });
        }
        let provider_thread_id = self.provider_thread_id().await?;
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .request(
                "thread/goal/set",
                json!({
                    "threadId": provider_thread_id,
                    "objective": objective,
                    "status": "active",
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn interrupt_turn(&self, turn_id: Option<String>) -> Result<(), RuntimeError> {
        let provider_thread_id = self.provider_thread_id().await?;
        let active_turn_id = if let Some(turn_id) = turn_id {
            Some(turn_id)
        } else {
            self.inner.session.lock().await.active_turn_id.clone()
        };
        let Some(active_turn_id) = active_turn_id else {
            return Ok(());
        };
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .request(
                "turn/interrupt",
                json!({
                    "threadId": provider_thread_id,
                    "turnId": active_turn_id,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn interrupt_targeted_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(), RuntimeError> {
        if !usable_native_id(thread_id) || !usable_native_id(turn_id) {
            return Err(RuntimeError::InvalidPayload {
                message:
                    "targeted native IDs must satisfy the non-empty 256-byte whitespace-free bound"
                        .to_owned(),
            });
        }
        let provider_thread_id = self.provider_thread_id().await?;
        if thread_id == provider_thread_id {
            return Err(RuntimeError::InvalidPayload {
                message: "the root provider thread cannot be targeted as a child".to_owned(),
            });
        }
        let is_current_target = self
            .inner
            .activity
            .lock()
            .await
            .tracker
            .is_current_target(thread_id, turn_id);
        if !is_current_target {
            return Err(RuntimeError::InvalidPayload {
                message: "the targeted child turn is not the current active turn".to_owned(),
            });
        }
        let connection = self.inner.connection.lock().await.clone();
        connection
            .request(
                "turn/interrupt",
                json!({
                    "threadId": thread_id,
                    "turnId": turn_id,
                }),
            )
            .await?;
        Ok(())
    }

    /// Drops the newest `num_turns` turns from the Codex conversation history with
    /// `thread/revert`. Codex does not touch files; checkpoint revert restores them from
    /// Coddy Studio's own Git refs once this succeeds.
    pub async fn rollback_thread(&self, num_turns: u64) -> Result<(), RuntimeError> {
        if num_turns == 0 {
            return Ok(());
        }
        let provider_thread_id = self.provider_thread_id().await?;
        match self.inner.history_mode.lock().await.as_deref() {
            Some("paginated") => {}
            Some(_) => return Err(RuntimeError::RewindNeedsNewThread),
            None => return Err(RuntimeError::RewindNeedsNewerCodex),
        }
        let connection = self.inner.connection.lock().await.clone();
        let rewind = async {
            let before_turn_id =
                oldest_rolled_back_turn_id(&connection, &provider_thread_id, num_turns).await?;
            Ok::<_, RuntimeError>(
                connection
                    .request(
                        "thread/revert",
                        json!({
                            "threadId": provider_thread_id,
                            "beforeTurnId": before_turn_id,
                        }),
                    )
                    .await?,
            )
        };
        let response = rewind.await.map_err(|error| match error {
            RuntimeError::Protocol(error) if error.is_missing_method() => {
                RuntimeError::RewindNeedsNewerCodex
            }
            error => error,
        })?;
        parse_thread_snapshot(&response)
            .map_err(|message| RuntimeError::InvalidPayload { message })?;
        let mut session = self.inner.session.lock().await;
        session.status = "ready".to_owned();
        session.active_turn_id = None;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        *self.inner.explicit_close.lock().await = true;
        let task = self
            .inner
            .task
            .lock()
            .expect("Codex incoming task mutex poisoned")
            .take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
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
            .expect("Codex reconciliation task mutex poisoned")
            .take();
        if let Some(task) = reconciliation_task {
            let _ = task.await;
        }
        let _ = self.inner.mcp_status.shutdown().await;
        let mcp_actor_task = self
            .inner
            .mcp_status_actor_task
            .lock()
            .expect("Codex MCP status actor task mutex poisoned")
            .take();
        if let Some(task) = mcp_actor_task {
            let _ = task.await;
        }
        let mcp_effect_task = self
            .inner
            .mcp_status_effect_task
            .lock()
            .expect("Codex MCP status effect task mutex poisoned")
            .take();
        if let Some(task) = mcp_effect_task {
            let _ = task.await;
        }
        let connection = self.inner.connection.lock().await.clone();
        let _ = connection.request("shutdown", Value::Null).await;
        connection.close().await;
        Ok(())
    }

    pub async fn respond_to_request(
        &self,
        request_id: &str,
        decision: &str,
    ) -> Result<(), RuntimeError> {
        let pending = self
            .inner
            .pending_requests
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| RuntimeError::PendingRequestNotFound {
                request_id: request_id.to_owned(),
            })?;
        self.emit(
            "request.resolved",
            pending.turn_id.clone(),
            Some(request_id.to_owned()),
            json!({
                "requestType": request_type(pending.kind),
                "decision": decision,
            }),
        )
        .await;
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .respond(
                pending.wire_id,
                json!({
                    "decision": decision,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn respond_to_user_input(
        &self,
        request_id: &str,
        answers: Value,
    ) -> Result<(), RuntimeError> {
        let pending = self
            .inner
            .pending_requests
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| RuntimeError::PendingRequestNotFound {
                request_id: request_id.to_owned(),
            })?;
        let wire_answers = answers
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(question_id, value)| {
                let answers = match value {
                    Value::String(answer) => vec![Value::String(answer)],
                    Value::Array(array) => array,
                    _ => Vec::new(),
                };
                (
                    question_id,
                    json!({
                        "answers": answers,
                    }),
                )
            })
            .collect::<serde_json::Map<String, Value>>();
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .respond(
                pending.wire_id,
                json!({
                    "answers": Value::Object(wire_answers.clone()),
                }),
            )
            .await?;
        self.emit(
            "user-input.resolved",
            pending.turn_id,
            Some(request_id.to_owned()),
            json!({
                "answers": normalize_user_input_answers(Value::Object(wire_answers)),
            }),
        )
        .await;
        Ok(())
    }

    pub async fn next_event(&self) -> Option<RuntimeEvent> {
        self.inner.events_rx.lock().await.recv().await
    }

    pub async fn collect_events(&self, expected: usize) -> Vec<RuntimeEventStableView> {
        let mut events = Vec::with_capacity(expected);
        while events.len() < expected {
            let Some(event) = self.next_event().await else {
                break;
            };
            events.push(event.stable_view());
        }
        events
    }

    fn attach_incoming(
        &self,
        connection: JsonRpcConnection,
        mut incoming: mpsc::UnboundedReceiver<IncomingEvent>,
    ) -> Option<JoinHandle<()>> {
        let previous = self.detach_incoming();
        let runtime = self.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = incoming.recv().await {
                runtime.handle_incoming(connection.clone(), event).await;
            }
        });
        *self
            .inner
            .task
            .lock()
            .expect("Codex incoming task mutex poisoned") = Some(task);
        previous
    }

    fn detach_incoming(&self) -> Option<JoinHandle<()>> {
        let mut task_slot = self
            .inner
            .task
            .lock()
            .expect("Codex incoming task mutex poisoned");
        let previous = task_slot.take();
        if let Some(previous) = previous.as_ref() {
            previous.abort();
        }
        previous
    }

    fn start_reconciliation_worker(&self, mut hints: mpsc::Receiver<()>) {
        let runtime = self.clone();
        let cancellation = self.inner.reconciliation_cancellation.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    hint = hints.recv() => if hint.is_none() {
                        return;
                    },
                }
                let Some(mut hint) = runtime.take_reconciliation_hint() else {
                    continue;
                };
                if hint.epoch != runtime.reconciliation_epoch().await {
                    continue;
                }
                if !hint.immediate {
                    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
                    loop {
                        tokio::select! {
                            biased;
                            () = cancellation.cancelled() => return,
                            wake = hints.recv() => {
                                if wake.is_none() {
                                    return;
                                }
                                if let Some(next) = runtime.take_reconciliation_hint() {
                                    hint = merge_reconciliation_hints(hint, next);
                                    if hint.immediate {
                                        break;
                                    }
                                }
                            }
                            () = tokio::time::sleep_until(deadline) => break,
                        }
                    }
                }
                if hint.epoch != runtime.reconciliation_epoch().await {
                    continue;
                }
                runtime.reconcile_once().await;
            }
        });
        let previous = self
            .inner
            .reconciliation_task
            .lock()
            .expect("Codex reconciliation task mutex poisoned")
            .replace(task);
        debug_assert!(previous.is_none());
    }

    async fn request_reconciliation(&self, immediate: bool) {
        let epoch = {
            let activity = self.inner.activity.lock().await;
            if !activity.agent_activity_enabled {
                return;
            }
            activity.reconciliation_epoch
        };
        let next = ReconciliationHint { immediate, epoch };
        let mut pending = self
            .inner
            .reconciliation_pending_hint
            .lock()
            .expect("Codex reconciliation pending-hint mutex poisoned");
        *pending = Some(
            pending
                .take()
                .map_or(next, |current| merge_reconciliation_hints(current, next)),
        );
        drop(pending);
        let _ = self.inner.reconciliation_hint_tx.try_send(());
    }

    async fn request_reconciliation_immediate(&self) {
        self.request_reconciliation(true).await;
    }

    fn take_reconciliation_hint(&self) -> Option<ReconciliationHint> {
        self.inner
            .reconciliation_pending_hint
            .lock()
            .expect("Codex reconciliation pending-hint mutex poisoned")
            .take()
    }

    /// A reconciliation reply that does not decode is a failed read, never a missing method: the
    /// thread or page is skipped and the next pass asks again. Logs once per method and cause,
    /// with no reply content.
    async fn warn_undecodable_reply(&self, method: &'static str, thread_id: &str, error: &str) {
        let cause = if error == CODEX_REMOTE_JSON_TOO_LARGE {
            "too_large"
        } else {
            "unexpected_shape"
        };
        if self
            .inner
            .activity
            .lock()
            .await
            .warned_undecodable_replies
            .insert((method, cause))
        {
            tracing::warn!(
                provider = PROVIDER,
                method,
                thread_id,
                cause,
                "skipped a Codex reconciliation reply activity cannot decode; later passes ask again"
            );
        }
    }

    async fn reconcile_once(&self) {
        let root_thread_id = match self.provider_thread_id().await {
            Ok(root_thread_id) => root_thread_id,
            Err(_) => return,
        };
        let (epoch, cancellation, observed_control_revision) = {
            let activity = self.inner.activity.lock().await;
            if !activity.agent_activity_enabled || !activity.tracker.is_root_thread(&root_thread_id)
            {
                return;
            }
            (
                activity.reconciliation_epoch,
                activity.reconciliation_pass_cancellation.clone(),
                activity.control_revision,
            )
        };
        let pass = ReconciliationPass {
            epoch,
            root_thread_id: root_thread_id.clone(),
            cancellation,
            observed_control_revision,
        };
        let connection = self.inner.connection.lock().await.clone();
        let pending_hinted_descendants = self
            .inner
            .activity
            .lock()
            .await
            .pending_hinted_descendants_snapshot();
        let mut read_candidates = VecDeque::new();
        let mut queued_read_candidate_ids = HashSet::new();
        let mut deferred_read_candidates = false;
        for thread_id in pending_hinted_descendants {
            enqueue_reconciliation_candidate(
                &mut read_candidates,
                &mut queued_read_candidate_ids,
                thread_id,
            );
        }
        let root_read_enabled = self.inner.activity.lock().await.thread_read_support
            != ReconciliationMethodSupport::Unsupported;
        let mut read_succeeded = false;
        let mut read_incompatible: Option<&'static str> = None;
        if root_read_enabled {
            let response =
                match read_thread_history(&connection, &root_thread_id, &pass.cancellation).await {
                    Ok(response) => Some(response),
                    Err(HistoryReadFailure::MissingMethod(method)) => {
                        read_incompatible = Some(method);
                        None
                    }
                    Err(HistoryReadFailure::Undecodable { method, error }) => {
                        self.warn_undecodable_reply(method, &root_thread_id, &error)
                            .await;
                        None
                    }
                    Err(HistoryReadFailure::Cancelled) => return,
                    Err(HistoryReadFailure::Request) => {
                        self.emit_reconciliation(&pass, ReconciliationEmission::Stale)
                            .await;
                        return;
                    }
                };
            if let Some(response) = response
                .filter(|response| response.thread.id.as_deref() == Some(root_thread_id.as_str()))
            {
                read_succeeded = true;
                let mut activity = self.inner.activity.lock().await;
                if !self.reconciliation_is_current_locked(&pass, &activity) {
                    return;
                }
                let source_version = response.thread.updated_at;
                let mut excluded_hints =
                    activity.resolved_hints_for_source(&root_thread_id, source_version);
                if activity.exclude_superseded_reconciliation_hint_receivers(
                    pass.observed_control_revision,
                    &response.thread,
                    &mut excluded_hints,
                ) {
                    deferred_read_candidates = true;
                }
                let hints = activity
                    .tracker
                    .reconcile_sub_agent_hints_with_projection_limit_excluding(
                        &response.thread,
                        usize::from(RECONCILIATION_DESCENDANT_LIMIT)
                            .saturating_sub(queued_read_candidate_ids.len()),
                        &excluded_hints,
                    );
                let hinted_descendant_ids = hints.hinted_descendant_ids.clone();
                activity.record_hint_source_version(
                    &root_thread_id,
                    source_version,
                    &hinted_descendant_ids,
                );
                activity.enqueue_hinted_descendants(&hinted_descendant_ids);
                activity.stage_reconciliation_mutations(hints.mutations);
                activity
                    .stage_reconciliation_controls(pass.observed_control_revision, hints.controls);
                drop(activity);
                for thread_id in hinted_descendant_ids {
                    if !thread_id.is_empty()
                        && !queued_read_candidate_ids.contains(&thread_id)
                        && queued_read_candidate_ids.len()
                            == usize::from(RECONCILIATION_DESCENDANT_LIMIT)
                    {
                        deferred_read_candidates = true;
                    }
                    enqueue_reconciliation_candidate(
                        &mut read_candidates,
                        &mut queued_read_candidate_ids,
                        thread_id,
                    );
                }
            }
        }
        if read_incompatible.is_some() || read_succeeded {
            let emit_read_warning = {
                let mut activity = self.inner.activity.lock().await;
                if !self.reconciliation_is_current_locked(&pass, &activity) {
                    return;
                }
                if read_incompatible.is_some() {
                    activity.thread_read_support = ReconciliationMethodSupport::Unsupported;
                    activity.clear_pending_hinted_descendants();
                } else {
                    activity.thread_read_support = ReconciliationMethodSupport::Supported;
                }
                activity.refresh_history_recovery_capability();
                read_incompatible
                    .filter(|method| activity.warned_incompatible_methods.insert(method))
            };
            if let Some(method) = emit_read_warning {
                self.emit_reconciliation(
                    &pass,
                    ReconciliationEmission::Warning(unsupported_history_method_warning(method)),
                )
                .await;
            }
        }

        let mut listed_threads = Vec::new();
        let list_enabled = self.inner.activity.lock().await.thread_list_support
            != ReconciliationMethodSupport::Unsupported;
        let mut list_succeeded = false;
        let mut list_incompatible = false;
        if list_enabled {
            let mut cursor = None;
            let mut seen_cursors = HashSet::new();
            let mut page_count = 0;
            while page_count < RECONCILIATION_DESCENDANT_PAGE_LIMIT {
                page_count += 1;
                let params = serde_json::to_value(ThreadListParams {
                    ancestor_thread_id: &root_thread_id,
                    limit: RECONCILIATION_DESCENDANT_LIMIT,
                    cursor: cursor.as_deref(),
                })
                .expect("Codex thread/list params serialize");
                let response = match connection
                    .request_cancellable("thread/list", params, &pass.cancellation)
                    .await
                {
                    Ok(response) => response,
                    Err(error) if error.is_missing_method() => {
                        list_incompatible = true;
                        break;
                    }
                    Err(ProtocolError::Cancelled { .. }) => return,
                    Err(_) => {
                        self.emit_reconciliation(&pass, ReconciliationEmission::Stale)
                            .await;
                        return;
                    }
                };
                let response = match decode_thread_list_response(response) {
                    Ok(response) => response,
                    Err(error) => {
                        self.warn_undecodable_reply("thread/list", &root_thread_id, &error)
                            .await;
                        break;
                    }
                };
                list_succeeded = true;
                listed_threads.extend(
                    response
                        .data
                        .into_iter()
                        .take(usize::from(RECONCILIATION_DESCENDANT_LIMIT)),
                );
                let Some(next_cursor) = response.next_cursor else {
                    break;
                };
                if !seen_cursors.insert(next_cursor.clone()) {
                    break;
                }
                cursor = Some(next_cursor);
            }
        }
        let (list_support, emit_list_warning) = {
            let mut activity = self.inner.activity.lock().await;
            if !self.reconciliation_is_current_locked(&pass, &activity) {
                return;
            }
            if list_incompatible {
                activity.thread_list_support = ReconciliationMethodSupport::Unsupported;
            } else if list_succeeded {
                activity.thread_list_support = ReconciliationMethodSupport::Supported;
            }
            activity.refresh_history_recovery_capability();
            let emit_warning =
                list_incompatible && activity.warned_incompatible_methods.insert("thread/list");
            (activity.thread_list_support, emit_warning)
        };
        if emit_list_warning {
            self.emit_reconciliation(
                &pass,
                ReconciliationEmission::Warning("Codex activity method thread/list is unsupported"),
            )
            .await;
        }

        let descendants = {
            let mut activity = self.inner.activity.lock().await;
            if !self.reconciliation_is_current_locked(&pass, &activity) {
                return;
            }
            let mut current_listed_threads = Vec::with_capacity(listed_threads.len());
            for thread in &listed_threads {
                let Some(native_thread_id) = thread.id.as_deref() else {
                    current_listed_threads.push(thread.clone());
                    continue;
                };
                if activity.reconciliation_actor_was_superseded(
                    pass.observed_control_revision,
                    native_thread_id,
                ) {
                    activity.retain_retry_source(native_thread_id);
                    deferred_read_candidates = true;
                } else {
                    current_listed_threads.push(thread.clone());
                }
            }
            let mut descendants = activity
                .tracker
                .reconcile_descendants_with_projection_limit(
                    &current_listed_threads,
                    usize::from(RECONCILIATION_DESCENDANT_LIMIT)
                        .saturating_sub(queued_read_candidate_ids.len()),
                );
            activity
                .stage_reconciliation_mutations(std::mem::take(&mut descendants.output.mutations));
            activity.stage_reconciliation_controls(
                pass.observed_control_revision,
                std::mem::take(&mut descendants.output.controls),
            );
            descendants
        };
        let read_enabled = read_incompatible.is_none()
            && self.inner.activity.lock().await.thread_read_support
                != ReconciliationMethodSupport::Unsupported;
        for thread_id in descendants.threads_to_read {
            enqueue_reconciliation_candidate(
                &mut read_candidates,
                &mut queued_read_candidate_ids,
                thread_id,
            );
        }
        if read_enabled {
            while let Some(thread_id) = read_candidates.pop_front() {
                let response =
                    match read_thread_history(&connection, &thread_id, &pass.cancellation).await {
                        Ok(response) => response,
                        Err(HistoryReadFailure::MissingMethod(method)) => {
                            read_incompatible = Some(method);
                            break;
                        }
                        Err(HistoryReadFailure::Undecodable { method, error }) => {
                            self.warn_undecodable_reply(method, &thread_id, &error)
                                .await;
                            continue;
                        }
                        Err(HistoryReadFailure::Cancelled) => return,
                        Err(HistoryReadFailure::Request) => {
                            self.emit_reconciliation(&pass, ReconciliationEmission::Stale)
                                .await;
                            return;
                        }
                    };
                if response.thread.id.as_deref() != Some(thread_id.as_str()) {
                    let mut activity = self.inner.activity.lock().await;
                    if !self.reconciliation_is_current_locked(&pass, &activity) {
                        return;
                    }
                    activity.remove_pending_hinted_descendant(&thread_id);
                    activity.mark_hinted_descendant_resolved(&thread_id);
                    continue;
                }
                let nested_hinted_descendants = {
                    let mut activity = self.inner.activity.lock().await;
                    if !self.reconciliation_is_current_locked(&pass, &activity) {
                        return;
                    }
                    if activity.reconciliation_actor_was_superseded(
                        pass.observed_control_revision,
                        &thread_id,
                    ) {
                        activity.retain_retry_source(&thread_id);
                        deferred_read_candidates = true;
                        continue;
                    }
                    let descendants = activity
                        .tracker
                        .reconcile_descendants(std::slice::from_ref(&response.thread));
                    let accepted = descendants
                        .accepted_thread_ids
                        .iter()
                        .any(|accepted| accepted == &thread_id);
                    activity.stage_reconciliation_mutations(descendants.output.mutations);
                    activity.stage_reconciliation_controls(
                        pass.observed_control_revision,
                        descendants.output.controls,
                    );
                    if !accepted {
                        activity.remove_pending_hinted_descendant(&thread_id);
                        activity.mark_hinted_descendant_resolved(&thread_id);
                        continue;
                    }
                    let source_version = response.thread.updated_at;
                    let mut excluded_hints =
                        activity.resolved_hints_for_source(&thread_id, source_version);
                    if activity.exclude_superseded_reconciliation_hint_receivers(
                        pass.observed_control_revision,
                        &response.thread,
                        &mut excluded_hints,
                    ) {
                        deferred_read_candidates = true;
                    }
                    let hints = activity
                        .tracker
                        .reconcile_sub_agent_hints_with_projection_limit_excluding(
                            &response.thread,
                            usize::from(RECONCILIATION_DESCENDANT_LIMIT)
                                .saturating_sub(queued_read_candidate_ids.len()),
                            &excluded_hints,
                        );
                    let nested_hinted_descendants = hints.hinted_descendant_ids.clone();
                    activity.remove_pending_hinted_descendant(&thread_id);
                    activity.record_hint_source_version(
                        &thread_id,
                        source_version,
                        &nested_hinted_descendants,
                    );
                    activity.enqueue_hinted_descendants(&nested_hinted_descendants);
                    activity.stage_reconciliation_mutations(hints.mutations);
                    activity.stage_reconciliation_controls(
                        pass.observed_control_revision,
                        hints.controls,
                    );
                    let history = activity.tracker.reconcile_thread_history(&response.thread);
                    activity.stage_reconciliation_mutations(history.mutations);
                    activity.stage_reconciliation_controls(
                        pass.observed_control_revision,
                        history.controls,
                    );
                    nested_hinted_descendants
                };
                read_succeeded = true;
                for nested_thread_id in &nested_hinted_descendants {
                    if !nested_thread_id.is_empty()
                        && !queued_read_candidate_ids.contains(nested_thread_id)
                        && queued_read_candidate_ids.len()
                            == usize::from(RECONCILIATION_DESCENDANT_LIMIT)
                    {
                        deferred_read_candidates = true;
                    }
                    enqueue_reconciliation_candidate(
                        &mut read_candidates,
                        &mut queued_read_candidate_ids,
                        nested_thread_id.clone(),
                    );
                }
                let mut activity = self.inner.activity.lock().await;
                if !self.reconciliation_is_current_locked(&pass, &activity) {
                    return;
                }
                let all_nested_hints_retained =
                    nested_hinted_descendants.iter().all(|nested_thread_id| {
                        queued_read_candidate_ids.contains(nested_thread_id)
                            || activity
                                .pending_hinted_descendant_ids
                                .contains(nested_thread_id)
                    });
                if all_nested_hints_retained {
                    activity.mark_hinted_descendant_resolved(&thread_id);
                } else {
                    activity.retain_retry_source(&thread_id);
                    deferred_read_candidates = true;
                }
            }
        }
        let (read_support, emit_read_warning) = {
            let mut activity = self.inner.activity.lock().await;
            if !self.reconciliation_is_current_locked(&pass, &activity) {
                return;
            }
            if read_incompatible.is_some() {
                activity.thread_read_support = ReconciliationMethodSupport::Unsupported;
                activity.clear_pending_hinted_descendants();
            } else if read_succeeded {
                activity.thread_read_support = ReconciliationMethodSupport::Supported;
            }
            activity.refresh_history_recovery_capability();
            let emit_warning = read_incompatible
                .filter(|method| activity.warned_incompatible_methods.insert(method));
            (activity.thread_read_support, emit_warning)
        };
        if let Some(method) = emit_read_warning {
            self.emit_reconciliation(
                &pass,
                ReconciliationEmission::Warning(unsupported_history_method_warning(method)),
            )
            .await;
        }

        let background_enabled = self.inner.activity.lock().await.background_list_support
            != ReconciliationMethodSupport::Unsupported;
        let mut background_terminals = Vec::new();
        let mut background_page_decoded = false;
        let mut background_authority = BackgroundSnapshotAuthority::Partial;
        let mut background_incompatible = false;
        if background_enabled {
            let mut background_cursor = None;
            let mut seen_background_cursors = HashSet::new();
            let mut page_count = 0;
            loop {
                if page_count == RECONCILIATION_BACKGROUND_PAGE_LIMIT {
                    break;
                }
                page_count += 1;
                let params = serde_json::to_value(ThreadBackgroundTerminalsListParams {
                    thread_id: &root_thread_id,
                    limit: RECONCILIATION_BACKGROUND_LIMIT,
                    cursor: background_cursor.as_deref(),
                })
                .expect("Codex background-terminal list params serialize");
                let response = match connection
                    .request_cancellable(
                        "thread/backgroundTerminals/list",
                        params,
                        &pass.cancellation,
                    )
                    .await
                {
                    Ok(response) => response,
                    Err(error) if error.is_missing_method() => {
                        background_incompatible = true;
                        break;
                    }
                    Err(ProtocolError::Cancelled { .. }) => return,
                    Err(_) => {
                        self.emit_reconciliation(&pass, ReconciliationEmission::Stale)
                            .await;
                        return;
                    }
                };
                let response = match decode_background_terminals_list_response(response) {
                    Ok(response) => response,
                    Err(error) => {
                        self.warn_undecodable_reply(
                            "thread/backgroundTerminals/list",
                            &root_thread_id,
                            &error,
                        )
                        .await;
                        break;
                    }
                };
                background_page_decoded = true;
                background_terminals.extend(
                    response
                        .data
                        .into_iter()
                        .take(usize::from(RECONCILIATION_BACKGROUND_LIMIT)),
                );
                let Some(next_cursor) = response.next_cursor else {
                    background_authority = BackgroundSnapshotAuthority::Complete;
                    break;
                };
                if !seen_background_cursors.insert(next_cursor.clone()) {
                    break;
                }
                background_cursor = Some(next_cursor);
            }
        }
        if background_page_decoded {
            let observed_at = current_timestamp();
            let mut activity = self.inner.activity.lock().await;
            if !self.reconciliation_is_current_locked(&pass, &activity) {
                return;
            }
            let background = activity.tracker.reconcile_background_terminals(
                &background_terminals,
                &observed_at,
                background_authority,
            );
            activity.stage_reconciliation_mutations(background.mutations);
        }
        let (background_support, emit_background_warning) = {
            let mut activity = self.inner.activity.lock().await;
            if !self.reconciliation_is_current_locked(&pass, &activity) {
                return;
            }
            if background_incompatible {
                activity.background_list_support = ReconciliationMethodSupport::Unsupported;
            } else if background_page_decoded {
                activity.background_list_support = ReconciliationMethodSupport::Supported;
            }
            let emit_warning = background_incompatible
                && activity
                    .warned_incompatible_methods
                    .insert("thread/backgroundTerminals/list");
            (activity.background_list_support, emit_warning)
        };
        if emit_background_warning {
            self.emit_reconciliation(
                &pass,
                ReconciliationEmission::Warning(
                    "Codex activity method thread/backgroundTerminals/list is unsupported",
                ),
            )
            .await;
        }

        {
            let mut activity = self.inner.activity.lock().await;
            if !self.reconciliation_is_current_locked(&pass, &activity) {
                return;
            }
            activity.tracker.finish_detail_baseline();
        }

        let capabilities = ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: background_support == ReconciliationMethodSupport::Supported,
            history_recovery: history_recovery_for_support(list_support, read_support),
            terminal_observation: false,
            targeted_actor_cancellation: true,
        };
        let background_health = match background_support {
            ReconciliationMethodSupport::Supported => ActivitySectionHealth::live(),
            ReconciliationMethodSupport::Unsupported => ActivitySectionHealth::try_error(
                "Codex background tasks are unavailable for this runtime",
                false,
            )
            .expect("bounded background activity health"),
            ReconciliationMethodSupport::Unknown => ActivitySectionHealth::unsupported(),
        };
        let scope_mutations = vec![
            ProviderActivityMutation::SetScope {
                capabilities: capabilities.clone(),
                observation_state: ActivityObservationState::Live,
            },
            ProviderActivityMutation::SetSectionHealth {
                section: ActivitySection::BackgroundTasks,
                health: background_health,
            },
        ];
        self.emit_reconciliation(
            &pass,
            ReconciliationEmission::Successful {
                capabilities,
                mutations: scope_mutations,
            },
        )
        .await;
        if deferred_read_candidates {
            self.request_reconciliation(false).await;
        }
    }

    async fn handle_incoming(&self, connection: JsonRpcConnection, event: IncomingEvent) {
        if *self.inner.explicit_close.lock().await {
            return;
        }
        match event {
            IncomingEvent::Notification {
                method,
                params,
                emitted_at_ms,
            } => {
                self.handle_notification(method, params, emitted_at_ms)
                    .await;
            }
            IncomingEvent::NotificationBarrier { processed } => {
                let _ = processed.send(());
            }
            IncomingEvent::Request {
                correlation_id,
                wire_id,
                method,
                params,
            } => {
                if let Err(error) = self
                    .handle_request(connection, correlation_id, wire_id, method, params)
                    .await
                {
                    self.emit(
                        "runtime.error",
                        None,
                        None,
                        json!({ "message": error.to_string() }),
                    )
                    .await;
                }
            }
            IncomingEvent::Stderr { message } => {
                let event_type = if FATAL_STDERR_SNIPPETS
                    .iter()
                    .any(|snippet| message.to_ascii_lowercase().contains(snippet))
                {
                    "runtime.error"
                } else {
                    "runtime.warning"
                };
                let payload = if event_type == "runtime.error" {
                    json!({
                        "message": message,
                        "class": "provider_error",
                    })
                } else {
                    json!({
                        "message": message,
                    })
                };
                self.emit(event_type, None, None, payload).await;
            }
            IncomingEvent::Closed { reason } => {
                if *self.inner.explicit_close.lock().await {
                    return;
                }
                let mut session = self.inner.session.lock().await;
                let active_turn_id = session.active_turn_id.take();
                session.status = "closed".to_owned();
                drop(session);
                if let Some(turn_id) = active_turn_id {
                    self.emit(
                        "turn.completed",
                        Some(turn_id),
                        None,
                        json!({
                            "state": "failed",
                            "errorMessage": reason.clone(),
                        }),
                    )
                    .await;
                }
                self.emit("session.exited", None, None, json!({ "reason": reason }))
                    .await;
            }
        }
    }

    async fn handle_notification(&self, method: String, params: Value, emitted_at_ms: u64) {
        if method == "mcpServer/startupStatus/updated" {
            let notification_root = match params.get("threadId") {
                None | Some(Value::Null) => None,
                Some(Value::String(thread_id)) => Some(thread_id.clone()),
                Some(_) => return,
            };
            if let Some(server) = mcp_server_status_from_notification(&params) {
                let _ = self
                    .inner
                    .mcp_status
                    .notification(notification_root, server)
                    .await;
            }
            return;
        }
        let notification_thread_id = params.get("threadId").and_then(Value::as_str);
        let session_root_thread_id = self.inner.session.lock().await.resume_cursor.clone();
        let (
            receive_sequence,
            activity_epoch,
            activity,
            activity_controls,
            request_reconciliation,
            is_root,
            is_verified_child,
        ) = {
            let mut state = self.inner.activity.lock().await;
            if !state.agent_activity_enabled {
                let is_root = notification_thread_id
                    .is_none_or(|thread_id| session_root_thread_id.as_deref() == Some(thread_id));
                (
                    0,
                    state.reconciliation_epoch,
                    Vec::new(),
                    Vec::new(),
                    false,
                    is_root,
                    false,
                )
            } else {
                let receive_sequence = state.next_receive_sequence;
                let Some(next_receive_sequence) = state.next_receive_sequence.checked_add(1) else {
                    return;
                };
                state.next_receive_sequence = next_receive_sequence;
                let output = state.tracker.handle_notification(
                    &method,
                    &params,
                    emitted_at_ms,
                    receive_sequence,
                );
                state.enqueue_hinted_descendants(&output.hinted_descendant_ids);
                state.record_live_controls(&output.controls);
                let is_root = notification_thread_id
                    .is_some_and(|thread_id| state.tracker.is_root_thread(thread_id));
                let is_verified_child = notification_thread_id
                    .is_some_and(|thread_id| state.tracker.is_verified_child(thread_id));
                (
                    receive_sequence,
                    state.reconciliation_epoch,
                    output.mutations,
                    output.controls,
                    output.request_reconciliation,
                    is_root,
                    is_verified_child,
                )
            }
        };
        if !activity.is_empty() || !activity_controls.is_empty() {
            self.emit_activity(
                receive_sequence,
                activity_epoch,
                activity,
                activity_controls,
            )
            .await;
        }
        if request_reconciliation {
            self.request_reconciliation(false).await;
        }
        if is_verified_child {
            return;
        }
        if notification_thread_id.is_some() && !is_root {
            return;
        }
        match method.as_str() {
            "thread/started" => {
                if let Some(thread_id) = params
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(Value::as_str)
                {
                    self.inner.session.lock().await.resume_cursor = Some(thread_id.to_owned());
                    *self.inner.history_mode.lock().await =
                        thread_history_mode(params.get("thread"));
                    let request_reconciliation = {
                        let mut activity = self.inner.activity.lock().await;
                        if !activity.agent_activity_enabled {
                            false
                        } else {
                            activity.install_root_thread_id(thread_id);
                            if activity.reconciled_root_thread_id.as_deref() == Some(thread_id) {
                                false
                            } else {
                                activity.reconciled_root_thread_id = Some(thread_id.to_owned());
                                true
                            }
                        }
                    };
                    if request_reconciliation {
                        self.request_reconciliation(true).await;
                    }
                }
            }
            "turn/started" => {
                let turn_id = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(turn_id) = turn_id.clone() {
                    let mut session = self.inner.session.lock().await;
                    session.status = "running".to_owned();
                    session.active_turn_id = Some(turn_id.clone());
                    drop(session);
                    self.emit("turn.started", Some(turn_id), None, json!({}))
                        .await;
                }
            }
            "thread/tokenUsage/updated" => {
                let root_thread_id = self.inner.session.lock().await.resume_cursor.clone();
                if let Some(root_thread_id) = root_thread_id
                    && let Some((turn_id, payload)) =
                        normalize_token_usage_notification(&params, &root_thread_id)
                {
                    self.emit("thread.token-usage.updated", turn_id, None, payload)
                        .await;
                }
            }
            "turn/completed" => {
                let turn_id = params
                    .get("turn")
                    .and_then(|turn| turn.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let reported_state = params
                    .get("turn")
                    .and_then(|turn| turn.get("status"))
                    .and_then(Value::as_str);
                let error = params
                    .get("turn")
                    .and_then(|turn| turn.get("error"))
                    .filter(|error| codex_error_is_informative(error))
                    .cloned();
                // A turn carrying an error is never a success. `status` used to
                // default to "completed", so a frame with a missing,
                // non-string, or unrecognised status reported the provider's
                // own failure to the user as a completed turn.
                //
                // The documented codex vocabulary is `completed | interrupted |
                // failed`; an interrupt is the user's doing and keeps its state
                // even when codex attaches an error explaining the abort.
                let state = if error.is_some()
                    && !matches!(reported_state, Some("interrupted" | "cancelled"))
                {
                    "failed"
                } else {
                    reported_state.unwrap_or("completed")
                };
                let mut session = self.inner.session.lock().await;
                session.status = if state == "failed" { "error" } else { "ready" }.to_owned();
                session.active_turn_id = None;
                drop(session);
                let mut payload = json!({ "state": state });
                if let Some(error) = error {
                    // Codex classifies its own failures in `codexErrorInfo`.
                    // Copying the wire object verbatim leaked a provider shape
                    // into the payload and discarded that classification, so the
                    // user saw neither what failed nor whose fault it was.
                    let classified = classify_codex_error(&error);
                    payload["errorMessage"] = json!(classified.message);
                    payload["errorClass"] = json!(classified.class);
                }
                self.emit("turn.completed", turn_id, None, payload).await;
            }
            "item/agentMessage/delta" => {
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .filter(|item_id| !item_id.is_empty())
                    .map(str::to_owned);
                let delta = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.emit_with_item_id(
                    "content.delta",
                    turn_id,
                    item_id,
                    None,
                    json!({
                        "streamKind": "assistant_text",
                        "delta": delta,
                    }),
                )
                .await;
            }
            "item/started" => {
                if let Some((turn_id, payload)) = command_item_event_payload(&params, false) {
                    self.emit("item.started", Some(turn_id), None, payload)
                        .await;
                }
            }
            "item/completed" => {
                if params.pointer("/item/type").and_then(Value::as_str) == Some("agentMessage")
                    && let Some(turn_id) = params
                        .get("turnId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                    && let Some(item_id) = params
                        .pointer("/item/id")
                        .and_then(Value::as_str)
                        .filter(|item_id| !item_id.is_empty())
                        .map(str::to_owned)
                {
                    self.emit_with_item_id(
                        "message.assistant.completed",
                        Some(turn_id),
                        Some(item_id),
                        None,
                        json!({}),
                    )
                    .await;
                } else if let Some((turn_id, payload)) = command_item_event_payload(&params, true) {
                    self.emit("item.completed", Some(turn_id), None, payload)
                        .await;
                }
            }
            unhandled => {
                // Unknown notification methods were dropped in total silence,
                // so we had no field evidence of what codex sends that Coddy Studio
                // ignores. Method names only — params carry conversation content.
                tracing::debug!(
                    provider = PROVIDER,
                    method = unhandled,
                    "dropped an unhandled codex notification"
                );
            }
        }
    }

    async fn handle_request(
        &self,
        connection: JsonRpcConnection,
        correlation_id: String,
        wire_id: Value,
        method: String,
        params: Value,
    ) -> Result<(), RuntimeError> {
        match method.as_str() {
            "item/commandExecution/requestApproval" => {
                let request_id = format!("approval:{correlation_id}");
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.inner.pending_requests.lock().await.insert(
                    request_id.clone(),
                    PendingRequest {
                        kind: PendingRequestKind::CommandApproval,
                        wire_id,
                        turn_id: turn_id.clone(),
                    },
                );
                let detail = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .or_else(|| params.get("command").and_then(Value::as_str))
                    .unwrap_or_default();
                self.emit(
                    "request.opened",
                    turn_id,
                    Some(request_id),
                    json!({
                        "requestType": request_type(PendingRequestKind::CommandApproval),
                        "detail": detail,
                    }),
                )
                .await;
                Ok(())
            }
            "item/fileChange/requestApproval" => {
                let request_id = format!("approval:{correlation_id}");
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.inner.pending_requests.lock().await.insert(
                    request_id.clone(),
                    PendingRequest {
                        kind: PendingRequestKind::FileChangeApproval,
                        wire_id,
                        turn_id: turn_id.clone(),
                    },
                );
                let detail = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.emit(
                    "request.opened",
                    turn_id,
                    Some(request_id),
                    json!({
                        "requestType": request_type(PendingRequestKind::FileChangeApproval),
                        "detail": detail,
                    }),
                )
                .await;
                Ok(())
            }
            "item/tool/requestUserInput" => {
                let request_id = format!("user-input:{correlation_id}");
                let turn_id = params
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.inner.pending_requests.lock().await.insert(
                    request_id.clone(),
                    PendingRequest {
                        kind: PendingRequestKind::UserInput,
                        wire_id,
                        turn_id: turn_id.clone(),
                    },
                );
                self.emit(
                    "user-input.requested",
                    turn_id,
                    Some(request_id),
                    json!({
                        "questions": normalize_questions(params.get("questions").cloned().unwrap_or(Value::Null)),
                    }),
                )
                .await;
                Ok(())
            }
            _ => {
                connection
                    .respond_error(
                        wire_id,
                        super::protocol::JsonRpcErrorShape {
                            code: -32601,
                            message: format!("Method not found: {method}"),
                            data: None,
                        },
                    )
                    .await?;
                Ok(())
            }
        }
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
        let event = RuntimeEvent {
            event_id: format!("evt-{}", *counter),
            provider: PROVIDER.to_owned(),
            created_at: FIXED_EVENT_TIME.to_owned(),
            event_type: event_type.to_owned(),
            thread_id: self.inner.options.thread_id.clone(),
            turn_id,
            item_id,
            request_id,
            payload,
            native_event_id: None,
            activity: Vec::new(),
            activity_controls: Vec::new(),
        };
        let _ = self.inner.events_tx.send(event);
    }

    async fn emit_activity(
        &self,
        receive_sequence: u128,
        activity_epoch: u64,
        activity: Vec<crate::activity::ProviderActivityMutation>,
        activity_controls: Vec<crate::activity::ProviderActivityControlUpdate>,
    ) {
        let mut counter = self.inner.event_counter.lock().await;
        let state = self.inner.activity.lock().await;
        if !state.agent_activity_enabled || state.reconciliation_epoch != activity_epoch {
            return;
        }
        *counter += 1;
        let event = RuntimeEvent {
            event_id: format!("evt-{}", *counter),
            provider: PROVIDER.to_owned(),
            created_at: FIXED_EVENT_TIME.to_owned(),
            event_type: "activity.native".to_owned(),
            thread_id: self.inner.options.thread_id.clone(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload: json!({}),
            native_event_id: Some(format!("codex:activity:{receive_sequence}")),
            activity,
            activity_controls,
        };
        let _ = self.inner.events_tx.send(event);
    }

    fn reconciliation_is_current_locked(
        &self,
        pass: &ReconciliationPass,
        activity: &RuntimeActivityState,
    ) -> bool {
        !pass.cancellation.is_cancelled()
            && activity.agent_activity_enabled
            && activity.reconciliation_epoch == pass.epoch
            && activity.tracker.is_root_thread(&pass.root_thread_id)
    }

    async fn reconciliation_epoch(&self) -> u64 {
        self.inner.activity.lock().await.reconciliation_epoch
    }

    async fn emit_reconciliation(
        &self,
        pass: &ReconciliationPass,
        emission: ReconciliationEmission,
    ) {
        let mut counter = self.inner.event_counter.lock().await;
        let mut activity_state = self.inner.activity.lock().await;
        if !self.reconciliation_is_current_locked(pass, &activity_state) {
            return;
        }
        if let ReconciliationEmission::Successful {
            capabilities,
            mutations,
        } = emission
        {
            activity_state.capabilities = capabilities;
            if !activity_state.pending_reconciliation_topology_valid {
                *counter += 1;
                let _ = self.inner.events_tx.send(RuntimeEvent {
                    event_id: format!("evt-{}", *counter),
                    provider: PROVIDER.to_owned(),
                    created_at: FIXED_EVENT_TIME.to_owned(),
                    event_type: "runtime.warning".to_owned(),
                    thread_id: self.inner.options.thread_id.clone(),
                    turn_id: None,
                    item_id: None,
                    request_id: None,
                    payload: json!({
                        "message": "Codex activity reconciliation cannot safely order staged topology"
                    }),
                    native_event_id: None,
                    activity: Vec::new(),
                    activity_controls: Vec::new(),
                });
                return;
            }
            let mut scope_mutations = Some(mutations);
            loop {
                let prefix = scope_mutations.take().unwrap_or_default();
                let record_capacity = RECONCILIATION_MUTATION_LIMIT - prefix.len();
                let record_count =
                    record_capacity.min(activity_state.pending_reconciliation_mutations.len());
                let final_batch =
                    record_count == activity_state.pending_reconciliation_mutations.len();
                let mut activity = prefix;
                activity.extend(
                    activity_state.pending_reconciliation_mutations[..record_count]
                        .iter()
                        .cloned(),
                );

                let sequence = activity_state.next_reconciliation_sequence;
                let Some(next) = sequence.checked_add(1) else {
                    return;
                };
                activity_state.next_reconciliation_sequence = next;
                *counter += 1;
                let event = RuntimeEvent {
                    event_id: format!("evt-{}", *counter),
                    provider: PROVIDER.to_owned(),
                    created_at: FIXED_EVENT_TIME.to_owned(),
                    event_type: "activity.native".to_owned(),
                    thread_id: self.inner.options.thread_id.clone(),
                    turn_id: None,
                    item_id: None,
                    request_id: None,
                    payload: json!({}),
                    native_event_id: Some(format!("codex:reconciliation:{sequence}")),
                    activity,
                    activity_controls: if final_batch {
                        activity_state.current_reconciliation_controls()
                    } else {
                        Vec::new()
                    },
                };
                if self.inner.events_tx.send(event).is_err() {
                    return;
                }
                activity_state
                    .pending_reconciliation_mutations
                    .drain(..record_count);
                if final_batch {
                    activity_state.pending_reconciliation_controls.clear();
                }
                if activity_state.pending_reconciliation_mutations.is_empty() {
                    return;
                }
            }
        }
        let (event_type, payload, native_event_id, activity) = match emission {
            ReconciliationEmission::Successful { .. } => unreachable!("handled above"),
            ReconciliationEmission::Stale => {
                let sequence = activity_state.next_reconciliation_sequence;
                let Some(next) = sequence.checked_add(1) else {
                    return;
                };
                activity_state.next_reconciliation_sequence = next;
                (
                    "activity.native",
                    json!({}),
                    Some(format!("codex:reconciliation:{sequence}")),
                    vec![ProviderActivityMutation::SetScope {
                        capabilities: activity_state.capabilities.clone(),
                        observation_state: ActivityObservationState::Stale,
                    }],
                )
            }
            ReconciliationEmission::Warning(message) => (
                "runtime.warning",
                json!({"message": message}),
                None,
                Vec::new(),
            ),
        };
        *counter += 1;
        let event = RuntimeEvent {
            event_id: format!("evt-{}", *counter),
            provider: PROVIDER.to_owned(),
            created_at: FIXED_EVENT_TIME.to_owned(),
            event_type: event_type.to_owned(),
            thread_id: self.inner.options.thread_id.clone(),
            turn_id: None,
            item_id: None,
            request_id: None,
            payload,
            native_event_id,
            activity,
            activity_controls: Vec::new(),
        };
        let _ = self.inner.events_tx.send(event);
    }

    async fn provider_thread_id(&self) -> Result<String, RuntimeError> {
        self.inner
            .session
            .lock()
            .await
            .resume_cursor
            .clone()
            .ok_or(RuntimeError::MissingProviderThreadId)
    }
}

impl RuntimeInner {
    async fn options_resume_cursor_set(&self, resume_cursor: Option<String>) {
        let mut session = self.session.lock().await;
        session.resume_cursor = resume_cursor;
    }
}

fn merge_reconciliation_hints(
    current: ReconciliationHint,
    next: ReconciliationHint,
) -> ReconciliationHint {
    match next.epoch.cmp(&current.epoch) {
        std::cmp::Ordering::Greater => next,
        std::cmp::Ordering::Equal => ReconciliationHint {
            immediate: current.immediate || next.immediate,
            epoch: current.epoch,
        },
        std::cmp::Ordering::Less => current,
    }
}

fn enqueue_reconciliation_candidate(
    candidates: &mut VecDeque<String>,
    candidate_ids: &mut HashSet<String>,
    native_thread_id: String,
) {
    if native_thread_id.is_empty()
        || candidate_ids.len() == usize::from(RECONCILIATION_DESCENDANT_LIMIT)
        || !candidate_ids.insert(native_thread_id.clone())
    {
        return;
    }
    candidates.push_back(native_thread_id);
}

#[cfg(test)]
fn bounded_reconciliation_mutations(
    mut scope_mutations: Vec<ProviderActivityMutation>,
    record_mutations: Vec<ProviderActivityMutation>,
) -> Vec<ProviderActivityMutation> {
    let record_capacity = RECONCILIATION_MUTATION_LIMIT - scope_mutations.len();
    scope_mutations.extend(bounded_record_reconciliation_mutations(
        record_mutations,
        record_capacity,
        true,
    ));
    scope_mutations
}

#[cfg(test)]
fn bounded_record_reconciliation_mutations(
    record_mutations: Vec<ProviderActivityMutation>,
    capacity: usize,
    assert_structural_bound: bool,
) -> Vec<ProviderActivityMutation> {
    let mut structural_mutations = Vec::new();
    let mut history_entries = Vec::new();
    for (index, mutation) in record_mutations.into_iter().enumerate() {
        match mutation {
            ProviderActivityMutation::AppendEntry(entry) => {
                history_entries.push((index, entry));
            }
            mutation => structural_mutations.push(mutation),
        }
    }

    let mut retained_actor_upserts = HashMap::new();
    structural_mutations = structural_mutations
        .into_iter()
        .rev()
        .filter(|mutation| {
            let ProviderActivityMutation::UpsertActor(actor) = mutation else {
                return true;
            };
            let retained = retained_actor_upserts.entry(actor.id.clone()).or_insert(0);
            if *retained == 2 {
                return false;
            }
            *retained += 1;
            true
        })
        .collect::<Vec<_>>();
    structural_mutations.reverse();

    // Reconciliation admits at most 50 descendants and 128 background items.
    // Root-history hints can add a provisional actor upsert before the existing
    // discovery and terminal upserts. Retaining only the newest two states for
    // each actor preserves the established structural budget and final state.
    if assert_structural_bound {
        assert!(
            structural_mutations.len() <= capacity,
            "bounded Codex reconciliation structural mutations"
        );
    } else if structural_mutations.len() > capacity {
        structural_mutations.drain(..structural_mutations.len() - capacity);
    }
    let history_capacity = capacity - structural_mutations.len();
    history_entries.sort_by(|(left_index, left), (right_index, right)| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left_index.cmp(right_index))
    });
    let drop_count = history_entries.len().saturating_sub(history_capacity);

    structural_mutations.extend(
        history_entries
            .into_iter()
            .skip(drop_count)
            .map(|(_, entry)| ProviderActivityMutation::AppendEntry(entry)),
    );
    structural_mutations
}

struct BoundedPendingReconciliation {
    mutations: Vec<ProviderActivityMutation>,
    topology_valid: bool,
}

struct PendingActorOrdering {
    mutations: Vec<ProviderActivityMutation>,
    topology_valid: bool,
}

struct PendingActorMutationState {
    original_index: usize,
    actor: crate::activity::ActivityActorSummary,
}

fn bounded_pending_reconciliation_mutations(
    mutations: Vec<ProviderActivityMutation>,
) -> BoundedPendingReconciliation {
    let mut actor_upserts = Vec::new();
    let mut work_item_upserts = Vec::new();
    let mut actor_removals = Vec::new();
    let mut work_item_removals = Vec::new();
    let mut history_entries = Vec::new();
    let mut other_mutations = Vec::new();
    for (index, mutation) in mutations.into_iter().enumerate() {
        match mutation {
            ProviderActivityMutation::UpsertActor(actor) => actor_upserts.push((index, actor)),
            ProviderActivityMutation::UpsertWorkItem(work_item) => {
                work_item_upserts.push((index, work_item));
            }
            ProviderActivityMutation::RemoveActor { actor_id } => {
                actor_removals.push((index, actor_id));
            }
            ProviderActivityMutation::RemoveWorkItem { work_item_id } => {
                work_item_removals.push((index, work_item_id));
            }
            ProviderActivityMutation::AppendEntry(entry) => history_entries.push((index, entry)),
            mutation => other_mutations.push((index, mutation)),
        }
    }

    let mut retained_actor_upserts = HashMap::new();
    actor_upserts = actor_upserts
        .into_iter()
        .rev()
        .filter(|(_, actor)| {
            let retained = retained_actor_upserts.entry(actor.id.clone()).or_insert(0);
            if *retained == 2 {
                return false;
            }
            *retained += 1;
            true
        })
        .collect();
    actor_upserts.reverse();
    let latest_actor_upsert = actor_upserts
        .iter()
        .map(|(index, actor)| (actor.id.clone(), *index))
        .collect::<HashMap<_, _>>();

    let mut retained_work_item_upserts = HashMap::new();
    work_item_upserts = work_item_upserts
        .into_iter()
        .rev()
        .filter(|(_, work_item)| {
            let retained = retained_work_item_upserts
                .entry(work_item.id.clone())
                .or_insert(0);
            if *retained == 2 {
                return false;
            }
            *retained += 1;
            true
        })
        .collect();
    work_item_upserts.reverse();
    let latest_work_item_upsert = work_item_upserts
        .iter()
        .map(|(index, work_item)| (work_item.id.clone(), *index))
        .collect::<HashMap<_, _>>();

    let mut retained_actor_removals = HashSet::new();
    actor_removals = actor_removals
        .into_iter()
        .rev()
        .filter(|(index, actor_id)| {
            retained_actor_removals.insert(actor_id.clone())
                && latest_actor_upsert
                    .get(actor_id)
                    .is_none_or(|upsert_index| index > upsert_index)
        })
        .collect();
    actor_removals.reverse();
    let mut retained_work_item_removals = HashSet::new();
    work_item_removals = work_item_removals
        .into_iter()
        .rev()
        .filter(|(index, work_item_id)| {
            retained_work_item_removals.insert(work_item_id.clone())
                && latest_work_item_upsert
                    .get(work_item_id)
                    .is_none_or(|upsert_index| index > upsert_index)
        })
        .collect();
    work_item_removals.reverse();

    let actor_upserts = topologically_order_pending_actor_upserts(actor_upserts);
    other_mutations.sort_by_key(|(index, _)| *index);
    work_item_upserts.sort_by_key(|(index, _)| *index);
    work_item_removals.sort_by_key(|(index, _)| *index);
    actor_removals.sort_by_key(|(index, _)| *index);
    let structural_count = other_mutations.len()
        + actor_upserts.mutations.len()
        + work_item_upserts.len()
        + work_item_removals.len()
        + actor_removals.len();
    assert!(
        structural_count <= RECONCILIATION_PENDING_MUTATION_LIMIT,
        "bounded Codex pending reconciliation structural mutations"
    );

    history_entries.sort_by(|(left_index, left), (right_index, right)| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left_index.cmp(right_index))
    });
    let first_event_record_capacity =
        RECONCILIATION_MUTATION_LIMIT - RECONCILIATION_SCOPE_MUTATION_COUNT;
    let history_capacity = if structural_count <= first_event_record_capacity {
        first_event_record_capacity - structural_count
    } else {
        RECONCILIATION_PENDING_MUTATION_LIMIT - structural_count
    };
    let drop_count = history_entries.len().saturating_sub(history_capacity);

    let mutations = other_mutations
        .into_iter()
        .map(|(_, mutation)| mutation)
        .chain(actor_upserts.mutations)
        .chain(
            work_item_upserts
                .into_iter()
                .map(|(_, work_item)| ProviderActivityMutation::UpsertWorkItem(work_item)),
        )
        .chain(
            history_entries
                .into_iter()
                .skip(drop_count)
                .map(|(_, entry)| ProviderActivityMutation::AppendEntry(entry)),
        )
        .chain(
            work_item_removals
                .into_iter()
                .map(|(_, work_item_id)| ProviderActivityMutation::RemoveWorkItem { work_item_id }),
        )
        .chain(
            actor_removals
                .into_iter()
                .map(|(_, actor_id)| ProviderActivityMutation::RemoveActor { actor_id }),
        )
        .collect();
    BoundedPendingReconciliation {
        mutations,
        topology_valid: actor_upserts.topology_valid,
    }
}

fn topologically_order_pending_actor_upserts(
    actor_upserts: Vec<(usize, crate::activity::ActivityActorSummary)>,
) -> PendingActorOrdering {
    let states = actor_upserts
        .into_iter()
        .map(|(original_index, actor)| PendingActorMutationState {
            original_index,
            actor,
        })
        .collect::<Vec<_>>();
    let fallback = || {
        states
            .iter()
            .map(|state| ProviderActivityMutation::UpsertActor(state.actor.clone()))
            .collect::<Vec<_>>()
    };
    let mut states_by_actor = HashMap::<String, Vec<usize>>::new();
    for (state_index, state) in states.iter().enumerate() {
        states_by_actor
            .entry(state.actor.id.clone())
            .or_default()
            .push(state_index);
    }

    let mut outgoing = vec![Vec::<usize>::new(); states.len()];
    let mut indegree = vec![0_usize; states.len()];
    let mut add_edge = |from: usize, to: usize| {
        if from != to && !outgoing[from].contains(&to) {
            outgoing[from].push(to);
            indegree[to] += 1;
        }
    };
    for actor_states in states_by_actor.values() {
        for pair in actor_states.windows(2) {
            add_edge(pair[0], pair[1]);
        }
    }
    for (state_index, state) in states.iter().enumerate() {
        if state.actor.name.is_empty() {
            continue;
        }
        let Some(parent_actor_id) = state.actor.parent_actor_id.as_ref() else {
            continue;
        };
        let Some(parent_states) = states_by_actor.get(parent_actor_id) else {
            continue;
        };
        let parent_state = parent_states
            .iter()
            .rev()
            .copied()
            .find(|parent_state| {
                !states[*parent_state].actor.name.is_empty()
                    && states[*parent_state].original_index <= state.original_index
            })
            .or_else(|| {
                parent_states
                    .iter()
                    .copied()
                    .find(|parent_state| !states[*parent_state].actor.name.is_empty())
            });
        if let Some(parent_state) = parent_state {
            add_edge(parent_state, state_index);
        }
    }

    let mut ready = states
        .iter()
        .enumerate()
        .filter(|(state_index, _)| indegree[*state_index] == 0)
        .map(|(state_index, state)| (state.original_index, state_index))
        .collect::<BTreeSet<_>>();
    let mut ordered_indexes = Vec::with_capacity(states.len());
    while let Some((_, state_index)) = ready.pop_first() {
        ordered_indexes.push(state_index);
        for dependent in &outgoing[state_index] {
            indegree[*dependent] -= 1;
            if indegree[*dependent] == 0 {
                ready.insert((states[*dependent].original_index, *dependent));
            }
        }
    }
    if ordered_indexes.len() != states.len()
        || !pending_actor_transitions_are_valid(&states, &ordered_indexes)
    {
        return PendingActorOrdering {
            mutations: fallback(),
            topology_valid: false,
        };
    }
    PendingActorOrdering {
        mutations: ordered_indexes
            .into_iter()
            .map(|state_index| {
                ProviderActivityMutation::UpsertActor(states[state_index].actor.clone())
            })
            .collect(),
        topology_valid: true,
    }
}

fn pending_actor_transitions_are_valid(
    states: &[PendingActorMutationState],
    ordered_indexes: &[usize],
) -> bool {
    let materializing_actor_ids = states
        .iter()
        .filter_map(|state| {
            let actor = &state.actor;
            (!actor.name.is_empty()).then(|| actor.id.clone())
        })
        .collect::<HashSet<_>>();
    let mut materialized = HashSet::new();
    let mut parents = HashMap::<String, Option<String>>::new();
    for state_index in ordered_indexes {
        let actor = &states[*state_index].actor;
        if actor.name.is_empty() {
            continue;
        }
        if actor
            .parent_actor_id
            .as_ref()
            .is_some_and(|parent_actor_id| {
                materializing_actor_ids.contains(parent_actor_id)
                    && !materialized.contains(parent_actor_id)
            })
        {
            return false;
        }
        parents.insert(actor.id.clone(), actor.parent_actor_id.clone());
        materialized.insert(actor.id.clone());

        let mut ancestry = HashSet::from([actor.id.as_str()]);
        let mut cursor = actor.parent_actor_id.as_deref();
        while let Some(parent_actor_id) = cursor {
            if !ancestry.insert(parent_actor_id) {
                return false;
            }
            cursor = parents
                .get(parent_actor_id)
                .and_then(|parent| parent.as_deref());
        }
    }
    true
}

/// The thread's `historyMode` (`legacy` or `paginated`), absent from Codex before history modes.
fn thread_history_mode(thread: Option<&Value>) -> Option<String> {
    thread
        .and_then(|thread| thread.get("historyMode"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Codex's `thread/turns/list` page-size ceiling.
const TURNS_PAGE_LIMIT: u64 = 100;

/// `thread/revert` drops `beforeTurnId` and every later turn, so dropping `num_turns` turns
/// targets the `num_turns`-th newest one. When Codex holds fewer turns (a fallback
/// `thread/start` replaced the thread), its oldest turn is the target and all are dropped.
async fn oldest_rolled_back_turn_id(
    connection: &JsonRpcConnection,
    thread_id: &str,
    num_turns: u64,
) -> Result<String, RuntimeError> {
    let mut remaining = num_turns;
    let mut cursor: Option<String> = None;
    let mut oldest = None;
    loop {
        let mut params = json!({
            "threadId": thread_id,
            "limit": remaining.min(TURNS_PAGE_LIMIT),
            "sortDirection": "desc",
            "itemsView": "notLoaded",
        });
        if let Some(cursor) = &cursor {
            params["cursor"] = json!(cursor);
        }
        let page = connection.request("thread/turns/list", params).await?;
        let turns = page.get("data").and_then(Value::as_array).ok_or_else(|| {
            RuntimeError::InvalidPayload {
                message: "thread/turns/list response missing data array".to_owned(),
            }
        })?;
        for turn in turns {
            if remaining == 0 {
                break;
            }
            let turn_id = turn
                .get("id")
                .and_then(Value::as_str)
                .filter(|turn_id| !turn_id.is_empty())
                .ok_or_else(|| RuntimeError::InvalidPayload {
                    message: "thread/turns/list turn missing id".to_owned(),
                })?;
            oldest = Some(turn_id.to_owned());
            remaining -= 1;
        }
        let next_cursor = page.get("nextCursor").and_then(Value::as_str);
        if remaining == 0
            || turns.is_empty()
            || next_cursor.is_none_or(|next| cursor.as_deref() == Some(next))
        {
            break;
        }
        cursor = next_cursor.map(str::to_owned);
    }
    oldest.ok_or_else(|| RuntimeError::InvalidPayload {
        message: "thread/turns/list returned no turns to revert".to_owned(),
    })
}

/// Pages delivery readback scans (2,000 turns) before the delivery is reported unknown.
const DELIVERY_READBACK_PAGE_LIMIT: usize = 100;

/// Why a reconciliation history read produced no thread.
enum HistoryReadFailure {
    /// Codex answered that this method does not exist.
    MissingMethod(&'static str),
    /// Codex answered, but the reply does not decode or exceeds the bound after trimming.
    Undecodable {
        method: &'static str,
        error: String,
    },
    Cancelled,
    /// Any other request failure, which makes the pass stale.
    Request,
}

fn history_request_failure(method: &'static str, error: ProtocolError) -> HistoryReadFailure {
    if error.is_missing_method() {
        HistoryReadFailure::MissingMethod(method)
    } else if matches!(error, ProtocolError::Cancelled { .. }) {
        HistoryReadFailure::Cancelled
    } else {
        HistoryReadFailure::Request
    }
}

fn unsupported_history_method_warning(method: &str) -> &'static str {
    if method == "thread/turns/list" {
        "Codex activity method thread/turns/list is unsupported"
    } else {
        "Codex activity method thread/read is unsupported"
    }
}

/// Reads a thread's metadata, then its newest turns from `thread/turns/list`, newest first,
/// until activity has the turns it keeps. A reply for another thread is returned without turns.
async fn read_thread_history(
    connection: &JsonRpcConnection,
    thread_id: &str,
    cancellation: &CancellationToken,
) -> Result<ThreadReadResponse, HistoryReadFailure> {
    let params = serde_json::to_value(ThreadReadParams {
        thread_id,
        include_turns: false,
    })
    .expect("Codex thread/read params serialize");
    let metadata = connection
        .request_cancellable("thread/read", params, cancellation)
        .await
        .map_err(|error| history_request_failure("thread/read", error))?;
    if metadata.pointer("/thread/id").and_then(Value::as_str) != Some(thread_id) {
        return decode_thread_read_response(metadata).map_err(|error| {
            HistoryReadFailure::Undecodable {
                method: "thread/read",
                error,
            }
        });
    }
    let undecodable = |error| HistoryReadFailure::Undecodable {
        method: "thread/turns/list",
        error,
    };
    let mut pager = TurnsPager::new(thread_id);
    while let Some(params) = pager.next_params() {
        let page = connection
            .request_cancellable("thread/turns/list", params, cancellation)
            .await
            .map_err(|error| history_request_failure("thread/turns/list", error))?;
        pager.accept_page(page).map_err(undecodable)?;
    }
    pager.into_thread(metadata).map_err(undecodable)
}

fn current_timestamp() -> String {
    format_observation_timestamp(OffsetDateTime::now_utc())
}

fn format_observation_timestamp(timestamp: OffsetDateTime) -> String {
    timestamp
        .format(&Rfc3339)
        .expect("current UTC observation time must format as RFC 3339")
}

/// A classified codex failure.
pub(crate) struct CodexFailure {
    pub(crate) class: &'static str,
    pub(crate) message: String,
}

/// Whether a `turn/completed` error field actually describes a failure.
///
/// `Value::get` yields `Some(Null)` for a present-but-null key and codex emits
/// `"error": null` on a healthy turn, so treating the option itself as "a
/// failure happened" reported every successful turn as failed — with the
/// generic fallback text, precisely because there was nothing to describe.
///
/// Requiring real content is also the safer rule rather than a looser one: a
/// turn that codex genuinely failed still says so in `status`, which
/// `reported_state` honours on its own. This clause exists only to catch a
/// missing or unrecognised status, and for that an empty error carries no
/// signal at all.
fn codex_error_is_informative(error: &Value) -> bool {
    match error {
        Value::Null => false,
        Value::String(message) => !message.trim().is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Array(items) => !items.is_empty(),
        _ => true,
    }
}

/// The text for a failure whose `codexErrorInfo` is absent or newer than the
/// variants Coddy Studio knows.
const CODEX_UNCLASSIFIED_FAILURE: &str = "Codex reported a failed turn.";

/// The error class and fallback text for one `codexErrorInfo` variant, or
/// `None` for a variant newer than the supported codex schema.
///
/// Names are the app-server v2 wire names: serde `rename_all = "camelCase"`
/// on `CodexErrorInfo` in `codex-rs/app-server-protocol`. The PascalCase
/// names in the prose App Server docs are the Rust identifiers and never
/// reach the wire; the snake_case form belongs to core rollout records, which
/// the app server translates before sending.
fn describe_codex_error_variant(variant: &str) -> Option<(&'static str, &'static str)> {
    let described = match variant {
        "unauthorized" => (
            "permission_error",
            "Codex is not authorized to reach the upstream API.",
        ),
        "badRequest" => ("validation_error", "The upstream API rejected the request."),
        "activeTurnNotSteerable" => (
            "validation_error",
            "The active turn cannot accept new input until it finishes.",
        ),
        "contextWindowExceeded" => (
            "provider_error",
            "The conversation exceeded the model's context window.",
        ),
        "sessionBudgetExceeded" => ("provider_error", "The session's budget was exhausted."),
        "usageLimitExceeded" => (
            "provider_error",
            "The upstream API usage limit was exceeded.",
        ),
        "rateLimitExceeded" => (
            "provider_error",
            "The upstream API rate limit was exceeded.",
        ),
        "flexUnavailable" => (
            "provider_error",
            "Flex processing is not available right now.",
        ),
        "serverOverloaded" => ("provider_error", "The upstream API is overloaded."),
        "cyberPolicy" => (
            "provider_error",
            "The request was blocked by the cybersecurity policy.",
        ),
        "misalignmentPolicyViolation" => (
            "provider_error",
            "The request was blocked by the misalignment policy.",
        ),
        "tooManyDenials" => (
            "provider_error",
            "Codex stopped after too many denied requests.",
        ),
        "httpConnectionFailed" => (
            "provider_error",
            "Codex could not connect to the upstream API.",
        ),
        "responseStreamConnectionFailed" | "responseStreamDisconnected" => (
            "provider_error",
            "The upstream API response stream was interrupted.",
        ),
        "responseTooManyFailedAttempts" => (
            "provider_error",
            "The upstream API failed repeatedly and Codex stopped retrying.",
        ),
        "internalServerError" => (
            "provider_error",
            "The upstream API reported an internal error.",
        ),
        "threadRollbackFailed" => ("provider_error", "Codex could not roll back the thread."),
        "sandboxError" => ("provider_error", "Codex's sandbox reported an error."),
        "other" => ("provider_error", "Codex reported an unclassified error."),
        _ => return None,
    };
    Some(described)
}

/// Classify a codex `TurnError` by its `codexErrorInfo` discriminator.
///
/// On the app-server v2 wire a unit variant is a camelCase string
/// (`"unauthorized"`), and a variant with data is an externally tagged
/// single-key object (`{"httpConnectionFailed": {"httpStatusCode": 502}}`)
/// whose upstream HTTP status, when known, sits inside the variant object.
/// A variant newer than the supported schema keeps the generic text but still
/// names itself in the message.
pub(crate) fn classify_codex_error(error: &Value) -> CodexFailure {
    let (info, detail) = match error.get("codexErrorInfo") {
        Some(Value::String(variant)) => (Some(variant.as_str()), None),
        Some(Value::Object(tagged)) if tagged.len() == 1 => tagged
            .iter()
            .next()
            .map_or((None, None), |(variant, detail)| {
                (Some(variant.as_str()), Some(detail))
            }),
        _ => (None, None),
    };
    let (class, fallback) = info
        .and_then(describe_codex_error_variant)
        .unwrap_or(("provider_error", CODEX_UNCLASSIFIED_FAILURE));
    let mut message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or(fallback)
        .to_owned();
    if let Some(info) = info
        && !message.contains(info)
    {
        message.push_str(&format!(" ({info})"));
    }
    if let Some(status) = detail
        .and_then(|detail| detail.get("httpStatusCode"))
        .and_then(Value::as_i64)
    {
        message.push_str(&format!(" (HTTP {status})"));
    }
    CodexFailure { class, message }
}

fn request_type(kind: PendingRequestKind) -> &'static str {
    match kind {
        PendingRequestKind::CommandApproval => "command_execution_approval",
        PendingRequestKind::FileChangeApproval => "file_change_approval",
        PendingRequestKind::UserInput => "tool_user_input",
    }
}

fn command_item_event_payload(params: &Value, completed: bool) -> Option<(String, Value)> {
    let turn_id = params.get("turnId").and_then(Value::as_str)?.to_owned();
    let item = params.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("commandExecution") {
        return None;
    }
    let detail = item
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = if completed {
        json!({
            "itemType": "command_execution",
            "status": "completed",
            "title": "Ran command",
            "detail": detail,
        })
    } else {
        json!({
            "itemType": "command_execution",
            "title": "Ran command",
            "detail": detail,
        })
    };
    Some((turn_id, payload))
}

fn normalize_questions(value: Value) -> Value {
    let questions = value.as_array().cloned().unwrap_or_default();
    Value::Array(
        questions
            .into_iter()
            .filter_map(|question| {
                let id = question.get("id").and_then(Value::as_str)?;
                let header = question.get("header").and_then(Value::as_str)?;
                let prompt = question.get("question").and_then(Value::as_str)?;
                let options = question
                    .get("options")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                Some(json!({
                    "id": id,
                    "header": header,
                    "question": prompt,
                    "options": options,
                    "multiSelect": false,
                }))
            })
            .collect(),
    )
}

fn normalize_user_input_answers(value: Value) -> Value {
    let answers = value.as_object().cloned().unwrap_or_default();
    let mut normalized = serde_json::Map::new();
    for (question_id, answer_value) in answers {
        let answer_array = answer_value
            .get("answers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if answer_array.len() == 1 {
            normalized.insert(question_id, answer_array[0].clone());
        } else {
            normalized.insert(question_id, Value::Array(answer_array));
        }
    }
    Value::Object(normalized)
}

fn normalize_token_usage_notification(
    params: &Value,
    root_thread_id: &str,
) -> Option<(Option<String>, Value)> {
    if params.get("threadId").and_then(Value::as_str)? != root_thread_id {
        return None;
    }
    let token_usage = params.get("tokenUsage")?;
    let last = token_usage.get("last")?;
    let used_tokens = last.get("totalTokens").and_then(Value::as_u64)?;
    if used_tokens == 0 {
        return None;
    }

    let mut usage = serde_json::Map::new();
    usage.insert("usedTokens".to_owned(), json!(used_tokens));
    usage.insert("lastUsedTokens".to_owned(), json!(used_tokens));

    if let Some(total_processed_tokens) = token_usage
        .get("total")
        .and_then(|total| total.get("totalTokens"))
        .and_then(Value::as_u64)
    {
        usage.insert(
            "totalProcessedTokens".to_owned(),
            json!(total_processed_tokens),
        );
    }
    if let Some(max_tokens) = token_usage
        .get("modelContextWindow")
        .and_then(Value::as_u64)
        .filter(|max_tokens| *max_tokens > 0)
    {
        usage.insert("maxTokens".to_owned(), json!(max_tokens));
    }
    for (source_key, usage_key, last_usage_key) in [
        ("inputTokens", "inputTokens", "lastInputTokens"),
        (
            "cachedInputTokens",
            "cachedInputTokens",
            "lastCachedInputTokens",
        ),
        ("outputTokens", "outputTokens", "lastOutputTokens"),
        (
            "reasoningOutputTokens",
            "reasoningOutputTokens",
            "lastReasoningOutputTokens",
        ),
    ] {
        if let Some(value) = last.get(source_key).and_then(Value::as_u64) {
            usage.insert(usage_key.to_owned(), json!(value));
            usage.insert(last_usage_key.to_owned(), json!(value));
        }
    }
    usage.insert("compactsAutomatically".to_owned(), Value::Bool(true));

    let turn_id = params
        .get("turnId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((turn_id, json!({ "usage": usage })))
}

#[cfg(test)]
mod tests {
    use std::{future::Future, task::Poll};

    use super::*;
    use crate::{
        activity::{
            ActivityProjection, ActivityRecordKind, ActivityRecordSummary, ActivityRepository,
            ActivityScopeRef, ActivityScopeSeed,
        },
        persistence::{Database, run_migrations},
        provider::codex::{
            mcp_status::{
                MCP_STATUS_PAGE_LIMIT, MCP_STATUS_PAGE_SIZE, MCP_STATUS_REQUEST_TIMEOUT,
                McpServerState, McpServerStatus,
            },
            model::ReconciliationThread,
            protocol::ConnectionConfig,
        },
    };
    use tokio::{
        io::{
            AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream,
            duplex,
        },
        sync::oneshot,
        time::timeout,
    };

    fn runtime_test_connection() -> (
        JsonRpcConnection,
        mpsc::UnboundedReceiver<IncomingEvent>,
        DuplexStream,
        DuplexStream,
        DuplexStream,
    ) {
        let (runtime_stdout, peer_stdout) = duplex(16 * 1024);
        let (peer_stdin, runtime_stdin) = duplex(16 * 1024);
        let (peer_stderr, runtime_stderr) = duplex(16 * 1024);
        let (connection, incoming) = JsonRpcConnection::spawn(
            runtime_stdout,
            runtime_stdin,
            runtime_stderr,
            ConnectionConfig::default(),
        );
        (connection, incoming, peer_stdout, peer_stdin, peer_stderr)
    }

    async fn read_runtime_test_json(reader: &mut (impl AsyncBufRead + Unpin)) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("JSON-RPC line");
        serde_json::from_str(&line).expect("JSON-RPC message")
    }

    /// Serves the `thread/turns/list` page the runtime asks for after a metadata `thread/read`
    /// whose reply names the requested thread. A pass cancelled meanwhile asks for none.
    async fn answer_runtime_test_turn_pages(
        reader: &mut (impl AsyncBufRead + Unpin),
        writer: &mut (impl AsyncWrite + Unpin),
        read: &Value,
        read_result: &Value,
    ) {
        assert_eq!(read["params"]["includeTurns"], false);
        if read_result.pointer("/thread/id") != read["params"].get("threadId") {
            return;
        }
        let Ok(page) =
            tokio::time::timeout(Duration::from_millis(500), read_runtime_test_json(reader)).await
        else {
            return;
        };
        assert_eq!(page["method"], "thread/turns/list");
        assert_eq!(page["params"]["threadId"], read["params"]["threadId"]);
        let mut turns = read_result
            .pointer("/thread/turns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        turns.reverse();
        write_runtime_test_json(
            writer,
            json!({
                "id": page["id"].clone(),
                "result": {"data": turns, "nextCursor": null, "backwardsCursor": null}
            }),
        )
        .await;
    }

    /// Writes a `thread/read` reply, then serves the turns page the runtime asks for next.
    async fn write_runtime_test_history(
        reader: &mut (impl AsyncBufRead + Unpin),
        writer: &mut (impl AsyncWrite + Unpin),
        read: &Value,
        reply: Value,
    ) {
        let result = reply["result"].clone();
        write_runtime_test_json(writer, reply).await;
        answer_runtime_test_turn_pages(reader, writer, read, &result).await;
    }

    async fn write_runtime_test_json(writer: &mut (impl AsyncWrite + Unpin), message: Value) {
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&message).expect("JSON-RPC serialization")
                )
                .as_bytes(),
            )
            .await
            .expect("JSON-RPC write");
        writer.flush().await.expect("JSON-RPC flush");
    }

    fn assert_mcp_status_list_request(request: &Value, thread_id: &str, cursor: Option<&str>) {
        let mut expected_params = json!({
            "threadId": thread_id,
            "limit": MCP_STATUS_PAGE_SIZE,
            "detail": "toolsAndAuthOnly",
        });
        if let Some(cursor) = cursor {
            expected_params["cursor"] = json!(cursor);
        }
        assert_eq!(request["method"], "mcpServerStatus/list");
        assert_eq!(request["params"], expected_params);
    }

    fn reconciliation_test_options() -> CodexSessionOptions {
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        }
    }

    #[tokio::test]
    async fn send_turn_keeps_the_active_turn_id_while_running() {
        // A follow-up response must not redirect Stop away from the provider's active turn.
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        runtime.inner.session.lock().await.resume_cursor = Some("provider-thread".to_owned());
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            for id in ["active", "follow-up"] {
                let request = read_runtime_test_json(&mut reader).await;
                assert_eq!(request["method"], "turn/start");
                write_runtime_test_json(
                    &mut writer,
                    json!({"id":request["id"], "result":{"turn":{"id":id}}}),
                )
                .await;
            }
            let interrupt = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({"id":interrupt["id"], "result":{}})).await;
            interrupt
        });
        for _ in 0..2 {
            runtime
                .send_turn(Some("hello".into()), vec![], None, None)
                .await
                .unwrap();
        }
        runtime.interrupt_turn(None).await.unwrap();
        let interrupt = peer.await.unwrap();
        assert_eq!(interrupt["params"]["turnId"], "active");
    }

    #[tokio::test]
    async fn codex_option_update_changes_the_next_turn_payload() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        runtime.inner.session.lock().await.resume_cursor = Some("provider-thread".to_owned());
        runtime
            .set_turn_options(Some("fast".to_owned()), Some("high".to_owned()))
            .await;

        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let request = read_runtime_test_json(&mut reader).await;
            assert_eq!(request["method"], "turn/start");
            assert_eq!(request["params"]["serviceTier"], "fast");
            assert_eq!(request["params"]["effort"], "high");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": { "turn": { "id": "turn-1" } },
                }),
            )
            .await;
        });

        runtime
            .send_turn(Some("hello".to_owned()), Vec::new(), None, None)
            .await
            .expect("turn start");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn codex_turn_option_validation_uses_the_exact_paginated_model() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        runtime.inner.session.lock().await.model = Some("gpt-target".to_owned());
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            for (cursor, response) in [
                (
                    None,
                    json!({
                        "data": [{
                            "model": "gpt-other",
                            "serviceTiers": [{ "id": "slow" }],
                            "supportedReasoningEfforts": [{ "reasoningEffort": "low" }]
                        }],
                        "nextCursor": "next"
                    }),
                ),
                (
                    Some("next"),
                    json!({
                        "data": [{
                            "model": "gpt-target",
                            "serviceTiers": [{ "id": "fast" }],
                            "supportedReasoningEfforts": [{ "reasoningEffort": "high" }]
                        }],
                        "nextCursor": null
                    }),
                ),
                (
                    None,
                    json!({
                        "data": [{
                            "model": "gpt-target",
                            "serviceTiers": [{ "id": "fast" }],
                            "supportedReasoningEfforts": [{ "reasoningEffort": "high" }]
                        }],
                        "nextCursor": null
                    }),
                ),
                (
                    None,
                    json!({
                        "data": [{
                            "model": "gpt-other",
                            "serviceTiers": [{ "id": "fast" }],
                            "supportedReasoningEfforts": [{ "reasoningEffort": "high" }]
                        }],
                        "nextCursor": null
                    }),
                ),
                (
                    None,
                    json!({
                        "data": [{
                            "model": "gpt-other",
                            "serviceTiers": [{ "id": "fast" }],
                            "supportedReasoningEfforts": [{ "reasoningEffort": "high" }]
                        }],
                        "nextCursor": null
                    }),
                ),
            ] {
                let request = read_runtime_test_json(&mut reader).await;
                assert_eq!(request["method"], "model/list");
                assert_eq!(
                    request["params"],
                    cursor.map_or_else(|| json!({}), |value| json!({ "cursor": value }))
                );
                write_runtime_test_json(
                    &mut writer,
                    json!({ "jsonrpc": "2.0", "id": request["id"], "result": response }),
                )
                .await;
            }
            let request = read_runtime_test_json(&mut reader).await;
            assert_eq!(request["method"], "model/list");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": { "code": -32000, "message": "model list unavailable" }
                }),
            )
            .await;
        });

        runtime
            .validate_turn_options(Some("fast"), Some("high"), "Work Codex", &[])
            .await
            .expect("exact model options are advertised");
        let refused = runtime
            .validate_turn_options(Some("slow"), Some("high"), "Work Codex", &[])
            .await
            .expect_err("an unadvertised service tier is refused");
        assert_eq!(
            refused.option_refusal(),
            Some("Service Tier slow is not supported by the selected model.")
        );
        // A model the list does not offer is refused on every retry too, naming the instance.
        let missing = runtime
            .validate_turn_options(Some("fast"), None, "Work Codex", &[])
            .await
            .expect_err("a model missing from the list is refused");
        assert_eq!(
            missing.option_refusal(),
            Some("gpt-target is not available in Work Codex.")
        );
        // As a custom model of the instance, the same model has the first listed model's options.
        runtime
            .validate_turn_options(
                Some("fast"),
                Some("high"),
                "Work Codex",
                &["gpt-target".to_owned()],
            )
            .await
            .expect("a custom model takes the catalog's fallback options");
        // A failed model list says nothing about the options, so a retry may succeed.
        let unavailable = runtime
            .validate_turn_options(Some("fast"), None, "Work Codex", &[])
            .await
            .expect_err("a failed model list fails validation");
        assert_eq!(unavailable.option_refusal(), None, "{unavailable}");
        peer.await.expect("peer");
    }

    #[derive(Debug, Eq, PartialEq)]
    struct ReconciliationStateSnapshot {
        tracker: String,
        thread_list_support: ReconciliationMethodSupport,
        thread_read_support: ReconciliationMethodSupport,
        background_list_support: ReconciliationMethodSupport,
        warned_incompatible_methods: Vec<&'static str>,
        capabilities: ActivityCapabilities,
        next_reconciliation_sequence: u64,
    }

    fn reconciliation_state_snapshot(state: &RuntimeActivityState) -> ReconciliationStateSnapshot {
        let mut warned_incompatible_methods = state
            .warned_incompatible_methods
            .iter()
            .copied()
            .collect::<Vec<_>>();
        warned_incompatible_methods.sort_unstable();
        ReconciliationStateSnapshot {
            tracker: format!("{:?}", state.tracker),
            thread_list_support: state.thread_list_support,
            thread_read_support: state.thread_read_support,
            background_list_support: state.background_list_support,
            warned_incompatible_methods,
            capabilities: state.capabilities.clone(),
            next_reconciliation_sequence: state.next_reconciliation_sequence,
        }
    }

    #[test]
    fn history_recovery_support_matrix_has_only_two_endpoint_combinations() {
        let support_levels = [
            ReconciliationMethodSupport::Unknown,
            ReconciliationMethodSupport::Supported,
            ReconciliationMethodSupport::Unsupported,
        ];
        for list_support in support_levels {
            for read_support in support_levels {
                let expected = match (list_support, read_support) {
                    (
                        ReconciliationMethodSupport::Supported,
                        ReconciliationMethodSupport::Supported,
                    ) => ActivityHistoryRecovery::Full,
                    (
                        ReconciliationMethodSupport::Unsupported,
                        ReconciliationMethodSupport::Unsupported,
                    ) => ActivityHistoryRecovery::None,
                    _ => ActivityHistoryRecovery::Bounded,
                };
                assert_eq!(
                    history_recovery_for_support(list_support, read_support),
                    expected,
                    "unexpected history recovery for {list_support:?}/{read_support:?}"
                );
            }
        }
    }

    async fn drain_runtime_events(runtime: &CodexSessionRuntime) -> Vec<RuntimeEvent> {
        let mut receiver = runtime.inner.events_rx.lock().await;
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    fn actor_control_targets(
        events: &[RuntimeEvent],
        expected_actor_id: &str,
    ) -> Vec<Option<(String, String)>> {
        events
            .iter()
            .flat_map(|event| &event.activity_controls)
            .filter_map(|update| match update {
                crate::activity::ProviderActivityControlUpdate::ActorTarget {
                    actor_id,
                    target,
                } if actor_id == expected_actor_id => Some(
                    target
                        .as_ref()
                        .and_then(|target| target.codex_turn_ids())
                        .map(|(thread_id, turn_id)| (thread_id.to_owned(), turn_id.to_owned())),
                ),
                crate::activity::ProviderActivityControlUpdate::ActorTarget { .. }
                | crate::activity::ProviderActivityControlUpdate::WorkTarget { .. } => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn turn_completed_carrying_an_error_is_never_reported_as_success() {
        // `status` used to default to "completed", so any frame whose status was
        // missing, non-string, or unrecognised projected the provider's own
        // failure to the user as a completed turn.
        for status in [
            None,
            Some(json!(null)),
            Some(json!(7)),
            Some(json!("something-this-build-does-not-know")),
            Some(json!("completed")),
        ] {
            let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
            let runtime = CodexSessionRuntime::new(
                reconciliation_test_options(),
                connection.clone(),
                incoming,
            );
            let mut turn = serde_json::Map::new();
            turn.insert("id".to_owned(), json!("turn-1"));
            turn.insert("error".to_owned(), json!({ "message": "upstream refused" }));
            if let Some(status) = status.clone() {
                turn.insert("status".to_owned(), status);
            }

            runtime
                .handle_notification(
                    "turn/completed".to_owned(),
                    json!({ "turn": Value::Object(turn) }),
                    1_000,
                )
                .await;

            let events = drain_runtime_events(&runtime).await;
            let completion = events
                .iter()
                .find(|event| event.event_type == "turn.completed")
                .expect("completion event");
            assert_eq!(
                completion.payload["state"], "failed",
                "a turn carrying an error must not be reported as success (status {status:?})"
            );
            assert_eq!(
                runtime.inner.session.lock().await.status,
                "error",
                "session status must follow the failed turn (status {status:?})"
            );
        }
    }

    #[tokio::test]
    async fn turn_completed_without_an_error_keeps_its_reported_state() {
        for (status, expected) in [
            (Some(json!("completed")), "completed"),
            (Some(json!("cancelled")), "cancelled"),
            (None, "completed"),
        ] {
            let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
            let runtime = CodexSessionRuntime::new(
                reconciliation_test_options(),
                connection.clone(),
                incoming,
            );
            let mut turn = serde_json::Map::new();
            turn.insert("id".to_owned(), json!("turn-1"));
            if let Some(status) = status.clone() {
                turn.insert("status".to_owned(), status);
            }

            runtime
                .handle_notification(
                    "turn/completed".to_owned(),
                    json!({ "turn": Value::Object(turn) }),
                    1_000,
                )
                .await;

            let events = drain_runtime_events(&runtime).await;
            let completion = events
                .iter()
                .find(|event| event.event_type == "turn.completed")
                .expect("completion event");
            assert_eq!(completion.payload["state"], expected);
        }
    }

    #[tokio::test]
    async fn interrupted_codex_turn_carrying_an_error_stays_interrupted() {
        // `interrupted` is codex's documented status for a user abort; flipping
        // it to `failed` because codex attached an explanation would report the
        // user's own interrupt as a provider failure.
        let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
        let runtime =
            CodexSessionRuntime::new(reconciliation_test_options(), connection.clone(), incoming);

        runtime
            .handle_notification(
                "turn/completed".to_owned(),
                json!({
                    "turn": {
                        "id": "turn-1",
                        "status": "interrupted",
                        "error": { "message": "aborted by user" }
                    }
                }),
                1_000,
            )
            .await;

        let events = drain_runtime_events(&runtime).await;
        let completion = events
            .iter()
            .find(|event| event.event_type == "turn.completed")
            .expect("completion event");
        assert_eq!(completion.payload["state"], "interrupted");
    }

    #[tokio::test]
    async fn a_successful_codex_turn_with_an_explicit_null_error_stays_completed() {
        // codex emits `"error": null` on a healthy turn. `Value::get` returns
        // `Some(Null)` for a present-but-null key, so treating the option as
        // "an error exists" reported every successful turn as failed — with the
        // generic fallback message, because there was no error to describe.
        for error in [json!(null), json!(""), json!("   "), json!({})] {
            let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
            let runtime = CodexSessionRuntime::new(
                reconciliation_test_options(),
                connection.clone(),
                incoming,
            );

            runtime
                .handle_notification(
                    "turn/completed".to_owned(),
                    json!({ "turn": { "id": "turn-1", "status": "completed", "error": error } }),
                    1_000,
                )
                .await;

            let events = drain_runtime_events(&runtime).await;
            let completion = events
                .iter()
                .find(|event| event.event_type == "turn.completed")
                .expect("completion event");
            assert_eq!(completion.payload["state"], "completed", "error={error}");
        }
    }

    /// `turn/completed` frames shaped like codex app-server 0.163.0's
    /// `TurnError`: `codexErrorInfo` is a camelCase string for unit variants and
    /// an externally tagged single-key object for variants with data, whose
    /// `httpStatusCode` lives inside the variant object.
    #[tokio::test]
    async fn codex_failure_carries_its_wire_error_classification() {
        for (info, expected_name, expected_status, expected_class) in [
            (
                json!("unauthorized"),
                "unauthorized",
                None,
                "permission_error",
            ),
            (json!("badRequest"), "badRequest", None, "validation_error"),
            (
                json!("usageLimitExceeded"),
                "usageLimitExceeded",
                None,
                "provider_error",
            ),
            (
                json!({ "httpConnectionFailed": { "httpStatusCode": 502 } }),
                "httpConnectionFailed",
                Some(502),
                "provider_error",
            ),
            (
                json!({ "responseStreamDisconnected": { "httpStatusCode": null } }),
                "responseStreamDisconnected",
                None,
                "provider_error",
            ),
            (
                json!({ "activeTurnNotSteerable": { "turnKind": "review" } }),
                "activeTurnNotSteerable",
                None,
                "validation_error",
            ),
        ] {
            let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
            let runtime = CodexSessionRuntime::new(
                reconciliation_test_options(),
                connection.clone(),
                incoming,
            );

            runtime
                .handle_notification(
                    "turn/completed".to_owned(),
                    json!({
                        "turn": {
                            "id": "turn-1",
                            "items": [],
                            "status": "failed",
                            "error": {
                                "message": "upstream said no",
                                "codexErrorInfo": info,
                                "additionalDetails": null
                            }
                        }
                    }),
                    1_000,
                )
                .await;

            let events = drain_runtime_events(&runtime).await;
            let completion = events
                .iter()
                .find(|event| event.event_type == "turn.completed")
                .expect("completion event");
            assert_eq!(completion.payload["state"], "failed");
            assert_eq!(
                completion.payload["errorClass"], expected_class,
                "{expected_name}"
            );
            let message = completion.payload["errorMessage"]
                .as_str()
                .expect("error message");
            assert!(
                message.starts_with("upstream said no"),
                "codex's own message must lead: {message:?}"
            );
            assert!(
                message.contains(&format!("({expected_name})")),
                "{expected_name} missing from {message:?}"
            );
            match expected_status {
                Some(status) => assert!(
                    message.contains(&format!("(HTTP {status})")),
                    "HTTP {status} missing from {message:?}"
                ),
                None => assert!(!message.contains("HTTP"), "{message:?}"),
            }
            // The provider's wire object must not leak into the payload.
            assert!(completion.payload.get("error").is_none());
        }
    }

    /// Every `CodexErrorInfo` variant in the schema generated from the supported
    /// codex binary has its own classification arm. Regenerating the fixture
    /// for a newer codex that adds a variant fails here until it gets one.
    #[test]
    fn every_codex_error_info_schema_variant_has_a_classification() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/codex/codex-error-info-schema.json"
        ))
        .expect("codex error info schema fixture");
        let members = fixture
            .pointer("/CodexErrorInfo/anyOf")
            .and_then(Value::as_array)
            .expect("CodexErrorInfo anyOf");
        let mut wire_values = Vec::new();
        for member in members {
            if let Some(names) = member.get("enum").and_then(Value::as_array) {
                wire_values.extend(names.iter().map(|name| {
                    let name = name.as_str().expect("string variant").to_owned();
                    (name.clone(), json!(name))
                }));
            } else if let Some(required) = member.get("required").and_then(Value::as_array) {
                let [key] = required.as_slice() else {
                    panic!("an object variant is externally tagged by one key: {member}");
                };
                let key = key.as_str().expect("variant key").to_owned();
                wire_values.push((key.clone(), json!({ key: {} })));
            }
        }
        assert!(
            wire_values.len() >= 20,
            "the fixture parsed fewer variants than 0.163.0 defines (15 + 5)"
        );

        for (name, wire) in wire_values {
            let failure = classify_codex_error(&json!({
                "message": "",
                "codexErrorInfo": wire,
            }));
            assert!(
                !failure.message.starts_with(CODEX_UNCLASSIFIED_FAILURE),
                "{name} has no classification arm: {:?}",
                failure.message
            );
            assert!(
                failure.message.ends_with(&format!("({name})")),
                "{name} missing from {:?}",
                failure.message
            );
        }
    }

    /// A variant newer than the supported schema still reaches the user by name
    /// instead of disappearing into the generic text.
    #[test]
    fn unknown_codex_error_info_variant_keeps_its_name() {
        for wire in [
            json!("futureError"),
            json!({ "futureError": { "httpStatusCode": 503 } }),
        ] {
            let failure = classify_codex_error(&json!({
                "message": "",
                "codexErrorInfo": wire,
            }));
            assert_eq!(failure.class, "provider_error");
            assert!(
                failure.message.starts_with(CODEX_UNCLASSIFIED_FAILURE),
                "{:?}",
                failure.message
            );
            assert!(
                failure.message.contains("(futureError)"),
                "{:?}",
                failure.message
            );
        }
    }

    #[tokio::test]
    async fn cancelled_turn_carrying_an_error_stays_cancelled() {
        let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
        let runtime =
            CodexSessionRuntime::new(reconciliation_test_options(), connection.clone(), incoming);

        runtime
            .handle_notification(
                "turn/completed".to_owned(),
                json!({
                    "turn": {
                        "id": "turn-1",
                        "status": "cancelled",
                        "error": { "message": "aborted by user" }
                    }
                }),
                1_000,
            )
            .await;

        let events = drain_runtime_events(&runtime).await;
        let completion = events
            .iter()
            .find(|event| event.event_type == "turn.completed")
            .expect("completion event");
        assert_eq!(completion.payload["state"], "cancelled");
    }

    #[tokio::test]
    async fn unexpected_transport_close_fails_the_active_turn_before_session_exit() {
        let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
        let runtime =
            CodexSessionRuntime::new(reconciliation_test_options(), connection.clone(), incoming);
        {
            let mut session = runtime.inner.session.lock().await;
            session.status = "running".to_owned();
            session.active_turn_id = Some("turn-1".to_owned());
        }

        runtime
            .handle_incoming(
                connection,
                IncomingEvent::Closed {
                    reason: "transport failed".to_owned(),
                },
            )
            .await;

        let events = drain_runtime_events(&runtime).await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "turn.completed");
        assert_eq!(events[0].turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            events[0].payload,
            json!({
                "state": "failed",
                "errorMessage": "transport failed",
            })
        );
        assert_eq!(events[1].event_type, "session.exited");
        assert_eq!(events[1].payload, json!({ "reason": "transport failed" }));

        let session = runtime.inner.session.lock().await;
        assert_eq!(session.status, "closed");
        assert_eq!(session.active_turn_id, None);
        drop(session);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), runtime.next_event())
                .await
                .is_err(),
            "the runtime retains its event sender after loss; consumers must handle session.exited"
        );
    }

    async fn assert_mcp_status_discovery_failure(responses: Vec<Value>, expected_warning: &str) {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let mut expected_cursor = None;
            for response in responses {
                let request = read_runtime_test_json(&mut reader).await;
                assert_mcp_status_list_request(
                    &request,
                    "provider-root",
                    expected_cursor.as_deref(),
                );
                expected_cursor = response
                    .get("nextCursor")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let response = if response.get("error").is_some() {
                    json!({ "id": request["id"], "error": response["error"] })
                } else {
                    json!({ "id": request["id"], "result": response })
                };
                write_runtime_test_json(&mut writer, response).await;
            }
            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime
            .start()
            .await
            .expect("status discovery is best effort");
        let events = timeout(Duration::from_secs(1), async {
            let mut events = Vec::new();
            loop {
                let event = runtime.next_event().await.expect("runtime event");
                let ready = event.event_type == "session.ready";
                events.push(event);
                if ready {
                    return events;
                }
            }
        })
        .await
        .expect("session becomes ready");
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "session.connecting",
                "mcp.status.updated",
                "runtime.warning",
                "session.ready"
            ]
        );
        assert_eq!(events[0].payload, json!({}));
        assert_eq!(events[1].payload, json!({ "servers": [] }));
        assert!(
            events[2].payload["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected_warning))
        );
        assert_eq!(events[3].payload, json!({}));

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_list_remote_errors_do_not_block_session_start() {
        assert_mcp_status_discovery_failure(
            vec![json!({
                "error": { "code": -32000, "message": "MCP unavailable" }
            })],
            "MCP unavailable",
        )
        .await;
    }

    #[tokio::test]
    async fn mcp_status_list_malformed_pages_do_not_block_session_start() {
        assert_mcp_status_discovery_failure(
            vec![json!({ "data": "invalid", "nextCursor": null })],
            "missing data array",
        )
        .await;
    }

    #[tokio::test]
    async fn mcp_status_list_repeated_cursors_do_not_block_session_start() {
        assert_mcp_status_discovery_failure(
            vec![
                json!({ "data": [], "nextCursor": "repeat" }),
                json!({ "data": [], "nextCursor": "repeat" }),
            ],
            "repeated nextCursor",
        )
        .await;
    }

    #[tokio::test]
    async fn mcp_status_list_blank_and_non_string_cursors_warn_then_ready() {
        for next_cursor in [json!("   "), json!(7)] {
            assert_mcp_status_discovery_failure(
                vec![json!({ "data": [], "nextCursor": next_cursor })],
                "response has invalid nextCursor",
            )
            .await;
        }
    }

    #[tokio::test]
    async fn mcp_status_list_ninth_distinct_cursor_is_bounded_before_session_ready() {
        assert_mcp_status_discovery_failure(
            (0..MCP_STATUS_PAGE_LIMIT)
                .map(|index| json!({ "data": [], "nextCursor": format!("cursor-{index}") }))
                .collect(),
            "exceeded page limit",
        )
        .await;
    }

    #[test]
    fn mcp_status_official_startup_notifications_map_exactly() {
        let cases = [
            (
                json!({ "name": "context7", "status": "starting" }),
                McpServerStatus {
                    name: "context7".to_owned(),
                    state: McpServerState::Starting,
                    detail: None,
                },
            ),
            (
                json!({ "name": "context7", "status": "ready" }),
                McpServerStatus {
                    name: "context7".to_owned(),
                    state: McpServerState::Connected,
                    detail: None,
                },
            ),
            (
                json!({ "name": "context7", "status": "cancelled" }),
                McpServerStatus {
                    name: "context7".to_owned(),
                    state: McpServerState::Disconnected,
                    detail: None,
                },
            ),
            (
                json!({
                    "name": "context7",
                    "status": "failed",
                    "error": " transport failed "
                }),
                McpServerStatus {
                    name: "context7".to_owned(),
                    state: McpServerState::Error,
                    detail: Some("transport failed".to_owned()),
                },
            ),
            (
                json!({
                    "name": "context7",
                    "status": "failed",
                    "error": " OAuth expired ",
                    "failureReason": "reauthenticationRequired"
                }),
                McpServerStatus {
                    name: "context7".to_owned(),
                    state: McpServerState::NeedsAuth,
                    detail: Some("OAuth expired".to_owned()),
                },
            ),
        ];

        for (params, expected) in cases {
            assert_eq!(mcp_server_status_from_notification(&params), Some(expected));
        }
    }

    #[tokio::test]
    async fn mcp_status_initial_notifications_before_initialize_are_staged_and_malformed_roots_are_ignored()
     {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            assert_eq!(initialize["method"], "initialize");

            for params in [
                json!({ "threadId": "provider-root", "name": "matching", "status": "ready" }),
                json!({ "name": "app-missing", "status": "ready" }),
                json!({ "threadId": null, "name": "app-null", "status": "ready" }),
                json!({ "threadId": "foreign-root", "name": "foreign", "status": "ready" }),
                json!({ "threadId": 7, "name": "numeric", "status": "ready" }),
                json!({ "threadId": {}, "name": "object", "status": "ready" }),
                json!({ "threadId": [], "name": "array", "status": "ready" }),
            ] {
                write_runtime_test_json(
                    &mut writer,
                    json!({
                        "method": "mcpServer/startupStatus/updated",
                        "params": params
                    }),
                )
                .await;
            }

            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            assert_eq!(start["method"], "thread/start");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "baseline",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime.start().await.expect("runtime starts");
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                (
                    "mcp.status.updated",
                    json!({
                        "servers": [
                            { "name": "app-missing", "state": "connected" },
                            { "name": "app-null", "state": "connected" },
                            { "name": "baseline", "state": "starting" },
                            { "name": "matching", "state": "connected" }
                        ]
                    }),
                ),
                ("session.ready", json!({})),
            ]
        );

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_reconnect_stages_new_and_app_scoped_notifications_before_initialize() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (release_old_peer, hold_old_peer) = oneshot::channel();
        let old_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "old-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "old-only",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
            let _ = hold_old_peer.await;
        });
        runtime.start().await.expect("initial runtime starts");
        let _ = drain_runtime_events(&runtime).await;

        let (replacement, replacement_incoming, new_stdout, new_stdin, _new_stderr) =
            runtime_test_connection();
        let new_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(new_stdin);
            let mut writer = new_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            assert_eq!(initialize["method"], "initialize");

            for params in [
                json!({ "threadId": "new-root", "name": "new-root-update", "status": "ready" }),
                json!({ "name": "app-missing", "status": "ready" }),
                json!({ "threadId": null, "name": "app-null", "status": "ready" }),
                json!({ "threadId": "old-root", "name": "late-old", "status": "ready" }),
            ] {
                write_runtime_test_json(
                    &mut writer,
                    json!({
                        "method": "mcpServer/startupStatus/updated",
                        "params": params
                    }),
                )
                .await;
            }

            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"]["threadId"], "old-root");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": resume["id"], "result": { "thread": { "id": "new-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "new-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "baseline",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime
            .reconnect(replacement, replacement_incoming)
            .await
            .expect("runtime reconnects");
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                (
                    "mcp.status.updated",
                    json!({
                        "servers": [
                            { "name": "app-missing", "state": "connected" },
                            { "name": "app-null", "state": "connected" },
                            { "name": "baseline", "state": "starting" },
                            { "name": "new-root-update", "state": "connected" }
                        ]
                    }),
                ),
                ("session.ready", json!({})),
            ]
        );

        runtime.shutdown().await.expect("runtime shuts down");
        new_peer.await.expect("replacement peer");
        release_old_peer.send(()).expect("release old peer");
        old_peer.await.expect("old peer");
    }

    #[tokio::test]
    async fn mcp_status_notifications_on_both_open_boundaries_win_and_foreign_root_is_excluded() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let peer_runtime = runtime.clone();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            assert_eq!(start["method"], "thread/start");

            for params in [
                json!({
                        "threadId": "provider-root",
                        "name": "before-response",
                        "status": "ready"
                }),
                json!({
                        "threadId": "foreign-root",
                        "name": "foreign",
                        "status": "ready"
                }),
            ] {
                peer_runtime
                    .handle_notification("mcpServer/startupStatus/updated".to_owned(), params, 0)
                    .await;
            }

            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "provider-root", None);
            peer_runtime
                .handle_notification(
                    "mcpServer/startupStatus/updated".to_owned(),
                    json!({
                            "threadId": "provider-root",
                            "name": "after-response",
                            "status": "ready"
                    }),
                    0,
                )
                .await;
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [
                            {
                                "name": "before-response",
                                "serverInfo": null,
                                "tools": {},
                                "resources": [],
                                "resourceTemplates": [],
                                "authStatus": "unsupported"
                            },
                            {
                                "name": "after-response",
                                "serverInfo": null,
                                "tools": {},
                                "resources": [],
                                "resourceTemplates": [],
                                "authStatus": "unsupported"
                            }
                        ],
                        "nextCursor": null
                    }
                }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime.start().await.expect("runtime starts");
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                (
                    "mcp.status.updated",
                    json!({
                        "servers": [
                            { "name": "after-response", "state": "connected" },
                            { "name": "before-response", "state": "connected" }
                        ]
                    }),
                ),
                ("session.ready", json!({})),
            ]
        );

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_open_and_two_public_refreshes_share_one_request_and_snapshot() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (opening_seen_tx, opening_seen_rx) = oneshot::channel();
        let (bind_root_tx, bind_root_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            assert_eq!(start["method"], "thread/start");
            opening_seen_tx.send(()).expect("report opening request");
            bind_root_rx.await.expect("release root binding");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;

            let mut first_page_requests = 0;
            loop {
                let request = read_runtime_test_json(&mut reader).await;
                if request["method"] == "mcpServerStatus/list" {
                    first_page_requests += 1;
                    assert_mcp_status_list_request(&request, "provider-root", None);
                    write_runtime_test_json(
                        &mut writer,
                        json!({
                            "id": request["id"],
                            "result": {
                                "data": [{
                                    "name": "shared",
                                    "serverInfo": null,
                                    "tools": {},
                                    "resources": [],
                                    "resourceTemplates": [],
                                    "authStatus": "unsupported"
                                }],
                                "nextCursor": null
                            }
                        }),
                    )
                    .await;
                    continue;
                }
                assert_eq!(request["method"], "shutdown");
                write_runtime_test_json(
                    &mut writer,
                    json!({ "id": request["id"], "result": null }),
                )
                .await;
                return first_page_requests;
            }
        });

        let start_runtime = runtime.clone();
        let start = tokio::spawn(async move { start_runtime.start().await });
        opening_seen_rx.await.expect("opening request arrives");
        let mut first_refresh = Box::pin(runtime.refresh_mcp_status());
        let mut second_refresh = Box::pin(runtime.refresh_mcp_status());
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(first_refresh.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(second_refresh.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        let (completion_blocked_tx, completion_blocked_rx) = oneshot::channel();
        let (release_completion_tx, release_completion_rx) = oneshot::channel();
        *runtime.inner.mcp_status_completion_barrier.lock().await =
            Some((completion_blocked_tx, release_completion_rx));

        bind_root_tx.send(()).expect("bind provider root");
        let connecting = runtime.next_event().await.expect("connecting event");
        assert_eq!(connecting.event_type, "session.connecting");
        let shared_snapshot = runtime.next_event().await.expect("shared MCP snapshot");
        assert_eq!(shared_snapshot.event_type, "mcp.status.updated");
        assert_eq!(
            shared_snapshot.payload,
            json!({
                "servers": [{ "name": "shared", "state": "starting" }]
            })
        );
        completion_blocked_rx
            .await
            .expect("completion remains blocked after snapshot publication");
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(first_refresh.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(second_refresh.as_mut().poll(cx)))
                .await
                .is_pending()
        );

        release_completion_tx
            .send(())
            .expect("release shared caller completion");
        let ((), (), started) = tokio::join!(&mut first_refresh, &mut second_refresh, start);
        started
            .expect("start task")
            .expect("runtime starts after the shared snapshot");
        let ready = runtime.next_event().await.expect("session ready event");
        assert_eq!(
            (ready.event_type.as_str(), ready.payload),
            ("session.ready", json!({}))
        );
        assert!(drain_runtime_events(&runtime).await.is_empty());

        runtime.shutdown().await.expect("runtime shuts down");
        assert_eq!(peer.await.expect("peer"), 1);
    }

    #[tokio::test]
    async fn mcp_status_matching_root_lifecycle_after_ready_emits_and_foreign_root_does_not() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (send_updates, receive_updates) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": list["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;

            receive_updates
                .await
                .expect("release lifecycle notifications");
            for notification in [
                json!({
                    "method": "mcpServer/startupStatus/updated",
                    "params": {
                        "threadId": "foreign-root",
                        "name": "foreign",
                        "status": "ready"
                    }
                }),
                json!({
                    "method": "mcpServer/startupStatus/updated",
                    "params": {
                        "threadId": "provider-root",
                        "name": "context7",
                        "status": "ready"
                    }
                }),
            ] {
                write_runtime_test_json(&mut writer, notification).await;
            }

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime.start().await.expect("runtime starts");
        let ready_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            ready_events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                ("mcp.status.updated", json!({ "servers": [] })),
                ("session.ready", json!({})),
            ]
        );
        send_updates.send(()).expect("send lifecycle notifications");
        let lifecycle = timeout(Duration::from_secs(1), runtime.next_event())
            .await
            .expect("matching lifecycle event arrives")
            .expect("runtime event channel remains open");
        assert_eq!(lifecycle.event_type, "mcp.status.updated");
        assert_eq!(
            lifecycle.payload,
            json!({
                "servers": [{ "name": "context7", "state": "connected" }]
            })
        );
        tokio::task::yield_now().await;
        assert!(drain_runtime_events(&runtime).await.is_empty());

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_status_effect_worker_preserves_committed_snapshot_order() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": list["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime.start().await.expect("runtime starts");
        let _ = drain_runtime_events(&runtime).await;
        let (snapshot_a_blocked_tx, snapshot_a_blocked_rx) = oneshot::channel();
        let (release_snapshot_a_tx, release_snapshot_a_rx) = oneshot::channel();
        *runtime.inner.mcp_status_publication_barrier.lock().await =
            Some((snapshot_a_blocked_tx, release_snapshot_a_rx));

        runtime
            .handle_notification(
                "mcpServer/startupStatus/updated".to_owned(),
                json!({
                    "threadId": "provider-root",
                    "name": "alpha",
                    "status": "ready"
                }),
                0,
            )
            .await;
        snapshot_a_blocked_rx
            .await
            .expect("snapshot A reaches publication barrier");
        runtime
            .handle_notification(
                "mcpServer/startupStatus/updated".to_owned(),
                json!({
                    "threadId": "provider-root",
                    "name": "beta",
                    "status": "ready"
                }),
                0,
            )
            .await;
        let actor_snapshot_b = runtime
            .inner
            .mcp_status
            .snapshot_for_test()
            .await
            .expect("actor snapshot B");
        assert_eq!(
            actor_snapshot_b
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );

        release_snapshot_a_tx
            .send(())
            .expect("release snapshot A publication");
        let first = timeout(Duration::from_secs(1), runtime.next_event())
            .await
            .expect("snapshot A arrives")
            .expect("runtime event channel remains open");
        let second = timeout(Duration::from_secs(1), runtime.next_event())
            .await
            .expect("snapshot B arrives")
            .expect("runtime event channel remains open");
        assert_eq!(
            [first.payload, second.payload],
            [
                json!({
                    "servers": [{ "name": "alpha", "state": "connected" }]
                }),
                json!({ "servers": actor_snapshot_b }),
            ]
        );

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_actor_orders_reconnect_failure_before_ready_and_drops_old_root() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (release_old_peer, hold_old_peer) = oneshot::channel();
        let old_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "old-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "old-only",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
            let _ = hold_old_peer.await;
        });

        runtime.start().await.expect("initial runtime starts");
        let initial_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            initial_events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                (
                    "mcp.status.updated",
                    json!({
                        "servers": [{ "name": "old-only", "state": "starting" }]
                    }),
                ),
                ("session.ready", json!({})),
            ]
        );

        let (replacement, replacement_incoming, new_stdout, new_stdin, _new_stderr) =
            runtime_test_connection();
        let peer_runtime = runtime.clone();
        let new_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(new_stdin);
            let mut writer = new_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"]["threadId"], "old-root");
            let incoming_gate = peer_runtime.inner.explicit_close.lock().await;
            write_runtime_test_json(
                &mut writer,
                json!({
                    "method": "mcpServer/startupStatus/updated",
                    "params": {
                        "threadId": "new-root",
                        "name": "new-only",
                        "status": "ready"
                    }
                }),
            )
            .await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": resume["id"], "result": { "thread": { "id": "new-root" } } }),
            )
            .await;
            assert!(
                timeout(
                    Duration::from_millis(50),
                    read_runtime_test_json(&mut reader)
                )
                .await
                .is_err(),
                "the thread response must not overtake the earlier wire notification"
            );
            drop(incoming_gate);
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "new-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "error": { "code": -32000, "message": "MCP unavailable on reconnect" }
                }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime
            .reconnect(replacement, replacement_incoming)
            .await
            .expect("runtime reconnects despite MCP discovery failure");
        let reconnect_events = drain_runtime_events(&runtime).await;
        let event_types = reconnect_events
            .iter()
            .skip(1)
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>();
        assert_eq!(reconnect_events[0].event_type, "session.connecting");
        assert_eq!(
            event_types,
            vec!["mcp.status.updated", "runtime.warning", "session.ready"]
        );
        assert_eq!(reconnect_events[0].payload, json!({}));
        let snapshot_names = reconnect_events[1].payload["servers"]
            .as_array()
            .expect("complete MCP snapshot")
            .iter()
            .map(|server| server["name"].as_str().expect("server name"))
            .collect::<Vec<_>>();
        assert_eq!(snapshot_names, vec!["new-only"]);
        assert!(
            reconnect_events[2].payload["message"]
                .as_str()
                .is_some_and(|message| message.contains("MCP unavailable on reconnect"))
        );
        assert_eq!(reconnect_events[3].payload, json!({}));

        runtime.shutdown().await.expect("runtime shuts down");
        new_peer.await.expect("replacement peer");
        release_old_peer.send(()).expect("release old peer");
        old_peer.await.expect("old peer");
    }

    #[tokio::test]
    async fn mcp_status_failed_open_does_not_strand_later_refresh() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            assert_eq!(start["method"], "thread/start");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": start["id"],
                    "error": { "code": -32000, "message": "thread open failed" }
                }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        assert!(runtime.start().await.is_err());
        timeout(Duration::from_millis(100), runtime.refresh_mcp_status())
            .await
            .expect("refresh after failed open must resolve");

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_aborted_start_releases_opening_for_refresh_and_retry() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (start_seen_tx, start_seen_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;

            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let abandoned_start = read_runtime_test_json(&mut reader).await;
            assert_eq!(abandoned_start["method"], "thread/start");
            start_seen_tx.send(()).expect("report abandoned start");

            let retry_initialize = read_runtime_test_json(&mut reader).await;
            assert_eq!(retry_initialize["method"], "initialize");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": retry_initialize["id"], "result": {} }),
            )
            .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let retry_start = read_runtime_test_json(&mut reader).await;
            assert_eq!(retry_start["method"], "thread/start");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": retry_start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": list["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        let abandoned = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.start().await }
        });
        start_seen_rx.await.expect("peer receives abandoned start");
        abandoned.abort();
        assert!(
            abandoned
                .await
                .expect_err("start task must be aborted")
                .is_cancelled()
        );

        timeout(Duration::from_millis(100), runtime.refresh_mcp_status())
            .await
            .expect("refresh after aborted start must resolve");
        runtime.start().await.expect("retry start succeeds");

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_aborted_reconnect_preserves_old_root_for_refresh_and_retry() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (release_old_peer, hold_old_peer) = oneshot::channel();
        let old_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "old-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "old-only",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
            let _ = hold_old_peer.await;
        });
        runtime.start().await.expect("initial runtime starts");
        let _ = drain_runtime_events(&runtime).await;

        let (replacement, replacement_incoming, new_stdout, new_stdin, _new_stderr) =
            runtime_test_connection();
        let (resume_seen_tx, resume_seen_rx) = oneshot::channel();
        let new_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(new_stdin);
            let mut writer = new_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let abandoned_resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(abandoned_resume["method"], "thread/resume");
            assert_eq!(abandoned_resume["params"]["threadId"], "old-root");
            resume_seen_tx.send(()).expect("report abandoned resume");

            let mut retry_initialize = read_runtime_test_json(&mut reader).await;
            if retry_initialize["method"] == "mcpServerStatus/list" {
                assert_mcp_status_list_request(&retry_initialize, "old-root", None);
                write_runtime_test_json(
                    &mut writer,
                    json!({
                        "id": retry_initialize["id"],
                        "error": { "code": -32000, "message": "refresh unavailable" }
                    }),
                )
                .await;
                retry_initialize = read_runtime_test_json(&mut reader).await;
            }
            assert_eq!(retry_initialize["method"], "initialize");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": retry_initialize["id"], "result": {} }),
            )
            .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let retry_resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(retry_resume["method"], "thread/resume");
            assert_eq!(retry_resume["params"]["threadId"], "old-root");
            write_runtime_test_json(
                &mut writer,
                json!({ "id": retry_resume["id"], "result": { "thread": { "id": "old-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": list["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        let abandoned = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.reconnect(replacement, replacement_incoming).await }
        });
        resume_seen_rx
            .await
            .expect("peer receives abandoned resume");
        abandoned.abort();
        assert!(
            abandoned
                .await
                .expect_err("reconnect task must be aborted")
                .is_cancelled()
        );

        timeout(Duration::from_millis(100), runtime.refresh_mcp_status())
            .await
            .expect("old-root refresh after aborted reconnect must resolve");
        assert_eq!(
            runtime
                .inner
                .mcp_status
                .snapshot_for_test()
                .await
                .expect("actor snapshot")
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>(),
            vec!["old-only"]
        );
        runtime.start().await.expect("retry resume succeeds");

        runtime.shutdown().await.expect("runtime shuts down");
        new_peer.await.expect("replacement peer");
        release_old_peer.send(()).expect("release old peer");
        old_peer.await.expect("old peer");
    }

    #[tokio::test]
    async fn mcp_status_failed_reconnect_leaves_old_root_refreshable() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (release_old_peer, hold_old_peer) = oneshot::channel();
        let old_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "old-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": list["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;
            let _ = hold_old_peer.await;
        });
        runtime.start().await.expect("initial runtime starts");
        let _ = drain_runtime_events(&runtime).await;

        let (replacement, replacement_incoming, new_stdout, new_stdin, _new_stderr) =
            runtime_test_connection();
        let new_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(new_stdin);
            let mut writer = new_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(resume["method"], "thread/resume");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": resume["id"],
                    "error": { "code": -32000, "message": "replacement unavailable" }
                }),
            )
            .await;

            let refresh = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&refresh, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": refresh["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;
            let shutdown = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        assert!(
            runtime
                .reconnect(replacement, replacement_incoming)
                .await
                .is_err()
        );
        timeout(Duration::from_millis(100), runtime.refresh_mcp_status())
            .await
            .expect("old-root refresh after failed reconnect must resolve");

        runtime.shutdown().await.expect("runtime shuts down");
        new_peer.await.expect("replacement peer");
        release_old_peer.send(()).expect("release old peer");
        old_peer.await.expect("old peer");
    }

    #[tokio::test]
    async fn mcp_status_reconnect_pre_root_update_wins_successful_list_before_ready() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (release_old_peer, hold_old_peer) = oneshot::channel();
        let old_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "old-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "context7",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
            let _ = hold_old_peer.await;
        });

        runtime.start().await.expect("initial runtime starts");
        let initial_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            initial_events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["session.connecting", "mcp.status.updated", "session.ready"]
        );
        assert_eq!(
            initial_events[1].payload,
            json!({ "servers": [{ "name": "context7", "state": "starting" }] })
        );

        let (replacement, replacement_incoming, new_stdout, new_stdin, _new_stderr) =
            runtime_test_connection();
        let peer_runtime = runtime.clone();
        let (send_later_updates, receive_later_updates) = oneshot::channel();
        let new_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(new_stdin);
            let mut writer = new_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"]["threadId"], "old-root");
            peer_runtime
                .handle_notification(
                    "mcpServer/startupStatus/updated".to_owned(),
                    json!({
                            "threadId": "new-root",
                            "name": "context7",
                            "status": "ready"
                    }),
                    0,
                )
                .await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": resume["id"], "result": { "thread": { "id": "new-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "new-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"],
                    "result": {
                        "data": [{
                            "name": "context7",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;

            receive_later_updates
                .await
                .expect("release later lifecycle updates");
            for (name, status) in [("context7", "ready"), ("barrier", "ready")] {
                write_runtime_test_json(
                    &mut writer,
                    json!({
                        "method": "mcpServer/startupStatus/updated",
                        "params": {
                            "threadId": "new-root",
                            "name": name,
                            "status": status
                        }
                    }),
                )
                .await;
            }

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime
            .reconnect(replacement, replacement_incoming)
            .await
            .expect("runtime reconnects after successful MCP discovery");
        let reconnect_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            reconnect_events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["session.connecting", "mcp.status.updated", "session.ready"]
        );
        assert_eq!(
            reconnect_events[1].payload,
            json!({ "servers": [{ "name": "context7", "state": "connected" }] })
        );

        send_later_updates
            .send(())
            .expect("send later lifecycle updates");
        let barrier = timeout(Duration::from_secs(1), runtime.next_event())
            .await
            .expect("barrier lifecycle event arrives")
            .expect("runtime event channel remains open");
        assert_eq!(barrier.event_type, "mcp.status.updated");
        assert_eq!(
            barrier.payload,
            json!({
                "servers": [
                    { "name": "barrier", "state": "connected" },
                    { "name": "context7", "state": "connected" }
                ]
            })
        );
        tokio::task::yield_now().await;
        assert!(drain_runtime_events(&runtime).await.is_empty());

        runtime.shutdown().await.expect("runtime shuts down");
        new_peer.await.expect("replacement peer");
        release_old_peer.send(()).expect("release old peer");
        old_peer.await.expect("old peer");
    }

    #[tokio::test(start_paused = true)]
    async fn mcp_status_silent_list_times_out_without_leaking_or_blocking_ready() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let pending_connection = connection.clone();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let (list_seen, wait_for_list) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&list, "provider-root", None);
            list_seen.send(()).expect("record silent list request");

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        let start_runtime = runtime.clone();
        let start = tokio::spawn(async move { start_runtime.start().await });
        wait_for_list.await.expect("silent list request arrives");
        assert_eq!(pending_connection.pending_request_count().await, 1);

        tokio::time::advance(MCP_STATUS_REQUEST_TIMEOUT + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        start
            .await
            .expect("start task")
            .expect("MCP timeout is non-fatal");

        assert_eq!(pending_connection.pending_request_count().await, 0);
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec![
                "session.connecting",
                "mcp.status.updated",
                "runtime.warning",
                "session.ready"
            ]
        );
        assert_eq!(events[1].payload, json!({ "servers": [] }));
        assert!(
            events[2].payload["message"]
                .as_str()
                .is_some_and(|message| message.contains("request timed out"))
        );

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_authoritative_empty_and_terminal_eighth_page_succeed() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({ "id": start["id"], "result": { "thread": { "id": "provider-root" } } }),
            )
            .await;
            let initial_list = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&initial_list, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({ "id": initial_list["id"], "result": { "data": [], "nextCursor": null } }),
            )
            .await;

            for page in 0..MCP_STATUS_PAGE_LIMIT {
                let request = read_runtime_test_json(&mut reader).await;
                let expected_cursor = (page > 0).then(|| format!("cursor-{page}"));
                assert_mcp_status_list_request(
                    &request,
                    "provider-root",
                    expected_cursor.as_deref(),
                );
                let terminal = page + 1 == MCP_STATUS_PAGE_LIMIT;
                let data = if terminal {
                    vec![json!({
                        "name": "terminal",
                        "serverInfo": null,
                        "tools": {},
                        "resources": [],
                        "resourceTemplates": [],
                        "authStatus": "unsupported"
                    })]
                } else {
                    Vec::new()
                };
                write_runtime_test_json(
                    &mut writer,
                    json!({
                        "id": request["id"],
                        "result": {
                            "data": data,
                            "nextCursor": if terminal {
                                Value::Null
                            } else {
                                json!(format!("cursor-{}", page + 1))
                            }
                        }
                    }),
                )
                .await;
            }

            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime.start().await.expect("runtime starts");
        let initial_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            initial_events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                ("mcp.status.updated", json!({ "servers": [] })),
                ("session.ready", json!({})),
            ]
        );

        runtime.refresh_mcp_status().await;
        let terminal_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            terminal_events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![(
                "mcp.status.updated",
                json!({
                    "servers": [{ "name": "terminal", "state": "starting" }]
                }),
            )]
        );

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test]
    async fn mcp_status_official_pages_emit_ordered_replacing_snapshots_without_warnings() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new_with_agent_activity_enabled(
            reconciliation_test_options(),
            connection,
            incoming,
            false,
        );
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, json!({ "id": initialize["id"], "result": {} }))
                .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            assert_eq!(start["method"], "thread/start");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": start["id"],
                    "result": {
                        "thread": { "id": "provider-root" },
                        "cwd": "/tmp/project",
                        "model": "gpt-5.3-codex"
                    }
                }),
            )
            .await;

            let first_page = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&first_page, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": first_page["id"],
                    "result": {
                        "data": [{
                            "name": "context7",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "unsupported"
                        }],
                        "nextCursor": "next-page"
                    }
                }),
            )
            .await;
            let second_page = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&second_page, "provider-root", Some("next-page"));
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": second_page["id"],
                    "result": {
                        "data": [
                            {
                                "name": "atlassian",
                                "serverInfo": null,
                                "tools": {},
                                "resources": [],
                                "resourceTemplates": [],
                                "authStatus": "notLoggedIn"
                            },
                            {
                                "name": "oauth",
                                "serverInfo": null,
                                "tools": {},
                                "resources": [],
                                "resourceTemplates": [],
                                "authStatus": "oAuth"
                            }
                        ],
                        "nextCursor": null
                    }
                }),
            )
            .await;

            let replacement = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&replacement, "provider-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": replacement["id"],
                    "result": {
                        "data": [{
                            "name": "oauth",
                            "serverInfo": null,
                            "tools": {},
                            "resources": [],
                            "resourceTemplates": [],
                            "authStatus": "oAuth"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(&mut writer, json!({ "id": shutdown["id"], "result": null }))
                .await;
        });

        runtime.start().await.expect("runtime starts");
        let initial_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            initial_events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![
                ("session.connecting", json!({})),
                (
                    "mcp.status.updated",
                    json!({
                        "servers": [
                            {
                                "name": "atlassian",
                                "state": "needs-auth",
                                "detail": "Authentication required."
                            },
                            { "name": "context7", "state": "starting" },
                            { "name": "oauth", "state": "starting" }
                        ]
                    }),
                ),
                ("session.ready", json!({})),
            ]
        );

        runtime.refresh_mcp_status().await;
        let replacement_events = drain_runtime_events(&runtime).await;
        assert_eq!(
            replacement_events
                .iter()
                .map(|event| (event.event_type.as_str(), event.payload.clone()))
                .collect::<Vec<_>>(),
            vec![(
                "mcp.status.updated",
                json!({
                    "servers": [{ "name": "oauth", "state": "starting" }]
                }),
            )]
        );

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_old_epoch_cannot_mutate_or_emit_after_response_wins() {
        let (connection_a, incoming_a, peer_stdout_a, peer_stdin_a, _peer_stderr_a) =
            runtime_test_connection();
        let runtime =
            CodexSessionRuntime::new(reconciliation_test_options(), connection_a, incoming_a);
        let (send_root_tx, send_root_rx) = oneshot::channel();
        let (background_seen_tx, background_seen_rx) = oneshot::channel();
        let (release_background_tx, release_background_rx) = oneshot::channel();
        let (background_replied_tx, background_replied_rx) = oneshot::channel();
        let old_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin_a);
            let mut writer = peer_stdout_a;
            let initialize = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({"id": initialize["id"].clone(), "result": {}}),
            )
            .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let start = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": start["id"].clone(),
                    "result": {
                        "thread": {"id": "old-root"},
                        "cwd": "/tmp/project",
                        "model": "gpt-5.3-codex"
                    }
                }),
            )
            .await;
            let mcp_status = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&mcp_status, "old-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": mcp_status["id"].clone(),
                    "result": { "data": [], "nextCursor": null }
                }),
            )
            .await;
            send_root_rx.await.expect("release old root notification");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "method": "thread/started",
                    "params": {"thread": {"id": "old-root"}},
                    "emittedAtMs": 1_000
                }),
            )
            .await;
            let root_read = read_runtime_test_json(&mut reader).await;
            assert_eq!(root_read["method"], "thread/read");
            assert_eq!(root_read["params"]["threadId"], "old-root");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": root_read["id"].clone(),
                    "result": {
                        "thread": {
                            "id": "old-root",
                            "createdAt": 1,
                            "updatedAt": 2,
                            "status": {"type": "idle"},
                            "turns": []
                        }
                    }
                }),
            )
            .await;
            answer_runtime_test_turn_pages(
                &mut reader,
                &mut writer,
                &root_read,
                &json!({
                    "thread": {
                        "id": "old-root",
                        "createdAt": 1,
                        "updatedAt": 2,
                        "status": {"type": "idle"},
                        "turns": []
                    }
                }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_eq!(list["method"], "thread/list");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"].clone(),
                    "result": {
                        "data": [{
                            "id": "old-child",
                            "parentThreadId": "old-root",
                            "createdAt": 1,
                            "updatedAt": 2,
                            "status": {"type": "idle"}
                        }],
                        "nextCursor": null,
                        "backwardsCursor": null
                    }
                }),
            )
            .await;
            let read = read_runtime_test_json(&mut reader).await;
            assert_eq!(read["method"], "thread/read");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": read["id"].clone(),
                    "result": {
                        "thread": {
                            "id": "old-child",
                            "parentThreadId": "old-root",
                            "createdAt": 1,
                            "updatedAt": 2,
                            "status": {"type": "idle"},
                            "turns": []
                        }
                    }
                }),
            )
            .await;
            answer_runtime_test_turn_pages(
                &mut reader,
                &mut writer,
                &read,
                &json!({
                    "thread": {
                        "id": "old-child",
                        "parentThreadId": "old-root",
                        "createdAt": 1,
                        "updatedAt": 2,
                        "status": {"type": "idle"},
                        "turns": []
                    }
                }),
            )
            .await;
            let background = read_runtime_test_json(&mut reader).await;
            assert_eq!(background["method"], "thread/backgroundTerminals/list");
            let _ = background_seen_tx.send(());
            release_background_rx
                .await
                .expect("release old background response");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": background["id"].clone(),
                    "result": {
                        "data": [{
                            "itemId": "old-background",
                            "processId": "old-process",
                            "command": "old command"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
            let _ = background_replied_tx.send(());
        });

        runtime.start().await.expect("old runtime starts");
        runtime.collect_events(2).await;
        send_root_tx.send(()).expect("send old root notification");
        background_seen_rx
            .await
            .expect("old pass reaches final request");
        let activity_guard = runtime.inner.activity.lock().await;
        let old_pass_cancellation = activity_guard.reconciliation_pass_cancellation.clone();

        let (connection_b, incoming_b, peer_stdout_b, peer_stdin_b, _peer_stderr_b) =
            runtime_test_connection();
        let (initialize_seen_tx, initialize_seen_rx) = oneshot::channel();
        let (release_initialize_tx, release_initialize_rx) = oneshot::channel();
        let new_peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin_b);
            let mut writer = peer_stdout_b;
            let initialize = read_runtime_test_json(&mut reader).await;
            initialize_seen_tx
                .send(())
                .expect("record replacement initialize");
            release_initialize_rx
                .await
                .expect("release replacement initialize");
            write_runtime_test_json(
                &mut writer,
                json!({"id": initialize["id"].clone(), "result": {}}),
            )
            .await;
            assert_eq!(
                read_runtime_test_json(&mut reader).await["method"],
                "initialized"
            );
            let resume = read_runtime_test_json(&mut reader).await;
            assert_eq!(resume["method"], "thread/resume");
            assert_eq!(resume["params"]["threadId"], "old-root");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": resume["id"].clone(),
                    "result": {
                        "thread": {"id": "new-root"},
                        "cwd": "/tmp/project",
                        "model": "gpt-5.3-codex"
                    }
                }),
            )
            .await;
            let mcp_status = read_runtime_test_json(&mut reader).await;
            assert_mcp_status_list_request(&mcp_status, "new-root", None);
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": mcp_status["id"].clone(),
                    "result": { "data": [], "nextCursor": null }
                }),
            )
            .await;
            let root_read = read_runtime_test_json(&mut reader).await;
            assert_eq!(root_read["method"], "thread/read");
            assert_eq!(root_read["params"]["threadId"], "new-root");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": root_read["id"].clone(),
                    "result": {
                        "thread": {
                            "id": "new-root",
                            "createdAt": 1,
                            "updatedAt": 2,
                            "status": {"type": "idle"},
                            "turns": []
                        }
                    }
                }),
            )
            .await;
            answer_runtime_test_turn_pages(
                &mut reader,
                &mut writer,
                &root_read,
                &json!({
                    "thread": {
                        "id": "new-root",
                        "createdAt": 1,
                        "updatedAt": 2,
                        "status": {"type": "idle"},
                        "turns": []
                    }
                }),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            assert_eq!(list["method"], "thread/list");
            assert_eq!(list["params"]["ancestorThreadId"], "new-root");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": list["id"].clone(),
                    "result": {
                        "data": [],
                        "nextCursor": null,
                        "backwardsCursor": null
                    }
                }),
            )
            .await;
            let background = read_runtime_test_json(&mut reader).await;
            assert_eq!(background["method"], "thread/backgroundTerminals/list");
            assert_eq!(background["params"]["threadId"], "new-root");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": background["id"].clone(),
                    "result": {"data": [], "nextCursor": null}
                }),
            )
            .await;
            let shutdown = read_runtime_test_json(&mut reader).await;
            assert_eq!(shutdown["method"], "shutdown");
            write_runtime_test_json(
                &mut writer,
                json!({"id": shutdown["id"].clone(), "result": null}),
            )
            .await;
        });
        let reconnect_runtime = runtime.clone();
        let reconnect =
            tokio::spawn(
                async move { reconnect_runtime.reconnect(connection_b, incoming_b).await },
            );
        assert!(
            timeout(
                Duration::from_millis(100),
                old_pass_cancellation.cancelled()
            )
            .await
            .is_err(),
            "the replacement transition must serialize behind the activity state boundary"
        );
        release_background_tx
            .send(())
            .expect("release old background response");
        background_replied_rx
            .await
            .expect("old response is on the transport");
        tokio::task::yield_now().await;
        let state_before_transition = reconciliation_state_snapshot(&activity_guard);
        let _ = drain_runtime_events(&runtime).await;
        drop(activity_guard);
        initialize_seen_rx
            .await
            .expect("replacement transition reaches initialize");
        let state_after_transition = {
            let activity = runtime.inner.activity.lock().await;
            reconciliation_state_snapshot(&activity)
        };
        assert_eq!(
            state_after_transition, state_before_transition,
            "the old pass must not mutate reconciliation state after the replacement transition"
        );
        let leaked_events = drain_runtime_events(&runtime)
            .await
            .into_iter()
            .filter(|event| {
                event
                    .native_event_id
                    .as_deref()
                    .is_some_and(|native_id| native_id.starts_with("codex:reconciliation:"))
                    || (event.event_type == "runtime.warning"
                        && event.payload["message"]
                            .as_str()
                            .is_some_and(|message| message.contains("Codex activity method")))
            })
            .collect::<Vec<_>>();
        assert!(
            leaked_events.is_empty(),
            "the old pass must not publish reconciliation events after the replacement transition"
        );
        release_initialize_tx
            .send(())
            .expect("release replacement initialize");
        reconnect
            .await
            .expect("reconnect task")
            .expect("runtime reconnects");

        let reconciliation = timeout(Duration::from_secs(1), async {
            loop {
                let event = runtime.next_event().await.expect("runtime event");
                if event.native_event_id.as_deref() == Some("codex:reconciliation:0") {
                    break event;
                }
            }
        })
        .await
        .expect("new-root reconciliation");
        assert!(
            reconciliation
                .activity
                .iter()
                .all(|mutation| match mutation {
                    ProviderActivityMutation::UpsertActor(actor) => {
                        actor.id != "codex:thread:old-child"
                    }
                    ProviderActivityMutation::UpsertWorkItem(work_item) => {
                        work_item.id != "codex:item:old-background"
                    }
                    _ => true,
                }),
            "the cancelled old epoch must not emit records into the replacement scope"
        );
        assert_eq!(
            runtime.inner.activity.lock().await.tracker.state_counts(),
            crate::provider::codex::activity::CodexActivityStateCounts {
                actors: 0,
                work_items: 0,
                seen_events: 0,
                pending_deltas: 0,
            }
        );

        runtime.shutdown().await.expect("runtime shuts down");
        old_peer.await.expect("old peer");
        new_peer.await.expect("new peer");
    }

    #[tokio::test]
    async fn immediate_hint_supersedes_a_queued_deferred_hint() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        runtime.inner.session.lock().await.resume_cursor = Some("provider-root".to_owned());
        runtime
            .inner
            .activity
            .lock()
            .await
            .tracker
            .set_root_thread_id("provider-root");

        let (list_seen_tx, list_seen_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            let mut list_seen_tx = Some(list_seen_tx);
            loop {
                let request = read_runtime_test_json(&mut reader).await;
                match request["method"].as_str().expect("request method") {
                    "thread/read" => {
                        assert_eq!(request["params"]["threadId"], "provider-root");
                        write_runtime_test_json(
                            &mut writer,
                            json!({
                                "id": request["id"].clone(),
                                "result": {
                                    "thread": {
                                        "id": "provider-root",
                                        "createdAt": 1,
                                        "updatedAt": 2,
                                        "status": {"type": "idle"},
                                        "turns": []
                                    }
                                }
                            }),
                        )
                        .await;
                    }
                    "thread/turns/list" => {
                        write_runtime_test_json(
                            &mut writer,
                            json!({
                                "id": request["id"].clone(),
                                "result": {"data": [], "nextCursor": null}
                            }),
                        )
                        .await;
                    }
                    "thread/list" => {
                        if let Some(list_seen_tx) = list_seen_tx.take() {
                            let _ = list_seen_tx.send(());
                        }
                        write_runtime_test_json(
                            &mut writer,
                            json!({
                                "id": request["id"].clone(),
                                "result": {
                                    "data": [],
                                    "nextCursor": null,
                                    "backwardsCursor": null
                                }
                            }),
                        )
                        .await;
                    }
                    "thread/backgroundTerminals/list" => {
                        write_runtime_test_json(
                            &mut writer,
                            json!({
                                "id": request["id"].clone(),
                                "result": {"data": [], "nextCursor": null}
                            }),
                        )
                        .await;
                    }
                    "shutdown" => {
                        write_runtime_test_json(
                            &mut writer,
                            json!({"id": request["id"].clone(), "result": null}),
                        )
                        .await;
                        break;
                    }
                    other => panic!("unexpected immediate-hint request: {other}"),
                }
            }
        });

        runtime.request_reconciliation(false).await;
        runtime.request_reconciliation(true).await;
        let immediate = timeout(Duration::from_millis(100), list_seen_rx).await;

        runtime.shutdown().await.expect("runtime shuts down");
        peer.await.expect("peer");
        assert!(
            immediate.is_ok(),
            "an immediate hint must replace a queued deferred hint without waiting 250ms"
        );
    }

    async fn configure_reconciliation_test_runtime(runtime: &CodexSessionRuntime) {
        runtime.inner.session.lock().await.resume_cursor = Some("provider-root".to_owned());
        let mut activity = runtime.inner.activity.lock().await;
        activity.install_root_thread_id("provider-root");
        activity.tracker.begin_detail_baseline();
    }

    #[test]
    fn background_observation_timestamp_format_is_canonical() {
        let observed_at = OffsetDateTime::from_unix_timestamp_nanos(1_786_710_896_789_000_000)
            .expect("fixed observation timestamp");

        assert_eq!(
            format_observation_timestamp(observed_at),
            "2026-08-14T12:34:56.789Z"
        );
    }

    #[tokio::test]
    async fn background_reconciliation_stamps_work_with_its_observation_time() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;

            let root_read = read_runtime_test_json(&mut reader).await;
            assert_eq!(root_read["method"], "thread/read");
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &root_read,
                root_reconciliation_response(&root_read),
            )
            .await;

            let thread_list = read_runtime_test_json(&mut reader).await;
            assert_eq!(thread_list["method"], "thread/list");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": thread_list["id"].clone(),
                    "result": {"data": [], "nextCursor": null, "backwardsCursor": null}
                }),
            )
            .await;

            let background = read_runtime_test_json(&mut reader).await;
            assert_eq!(background["method"], "thread/backgroundTerminals/list");
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": background["id"].clone(),
                    "result": {
                        "data": [{
                            "itemId": "background-live",
                            "processId": "process-live",
                            "command": "sleep 90"
                        }],
                        "nextCursor": null
                    }
                }),
            )
            .await;
        });

        let before_ms = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
        runtime.reconcile_once().await;
        let after_ms = OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
        peer.await.expect("reconciliation peer");

        let events = drain_runtime_events(&runtime).await;
        let started_at = events
            .iter()
            .flat_map(|event| &event.activity)
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertWorkItem(work_item)
                    if work_item.id == "codex:item:background-live" =>
                {
                    Some(work_item.started_at.as_str())
                }
                _ => None,
            })
            .expect("reconciled background work item");
        let started_at_ms = OffsetDateTime::parse(started_at, &Rfc3339)
            .expect("canonical RFC 3339 background timestamp")
            .unix_timestamp_nanos()
            / 1_000_000;

        assert!(
            (before_ms..=after_ms).contains(&started_at_ms),
            "background observation timestamp {started_at_ms}ms must be within reconciliation bounds {before_ms}ms..={after_ms}ms"
        );
    }

    fn verified_reconciliation_child() -> ReconciliationThread {
        decode_thread_read_response(json!({
            "thread": {
                "id": "durable-child",
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "active", "activeFlags": []},
                "turns": []
            }
        }))
        .expect("verified child")
        .thread
    }

    async fn emit_successful_reconciliation(runtime: &CodexSessionRuntime) {
        let (pass, capabilities) = {
            let activity = runtime.inner.activity.lock().await;
            (
                ReconciliationPass {
                    epoch: activity.reconciliation_epoch,
                    root_thread_id: "provider-root".to_owned(),
                    cancellation: activity.reconciliation_pass_cancellation.clone(),
                    observed_control_revision: activity.control_revision,
                },
                activity.capabilities.clone(),
            )
        };
        runtime
            .emit_reconciliation(
                &pass,
                ReconciliationEmission::Successful {
                    capabilities,
                    mutations: Vec::new(),
                },
            )
            .await;
    }

    #[tokio::test]
    async fn newer_live_start_supersedes_staged_reconciliation_revocation() {
        // Mutation caught: publishing an old reconciled None after a newer live target.
        let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity
                .tracker
                .reconcile_descendants(&[verified_reconciliation_child()]);
            activity.tracker.handle_notification(
                "turn/started",
                &json!({
                    "threadId": "durable-child",
                    "turn": {"id": "turn-1", "status": "inProgress", "startedAt": 3}
                }),
                3_000,
                0,
            );
            let terminal = activity.tracker.handle_notification(
                "turn/completed",
                &json!({
                    "threadId": "durable-child",
                    "turn": {"id": "turn-1", "status": "completed", "completedAt": 4}
                }),
                4_000,
                1,
            );
            let observed_control_revision = activity.control_revision;
            activity.stage_reconciliation_controls(observed_control_revision, terminal.controls);
        }

        runtime
            .handle_notification(
                "turn/started".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "turn-2", "status": "inProgress", "startedAt": 5}
                }),
                5_000,
            )
            .await;
        emit_successful_reconciliation(&runtime).await;

        let events = drain_runtime_events(&runtime).await;
        let reconciliation = events
            .iter()
            .find(|event| {
                event
                    .native_event_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("codex:reconciliation:"))
            })
            .expect("reconciliation event");
        assert!(reconciliation.activity_controls.is_empty());
        assert!(
            runtime
                .inner
                .activity
                .lock()
                .await
                .tracker
                .is_current_target("durable-child", "turn-2")
        );
    }

    #[tokio::test]
    async fn newer_live_completion_supersedes_staged_reconciliation_target() {
        // Mutation caught: publishing an old reconciled Some after live completion.
        let (connection, incoming, _stdout, _stdin, _stderr) = runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity
                .tracker
                .reconcile_descendants(&[verified_reconciliation_child()]);
            let active = activity.tracker.handle_notification(
                "turn/started",
                &json!({
                    "threadId": "durable-child",
                    "turn": {"id": "turn-1", "status": "inProgress", "startedAt": 3}
                }),
                3_000,
                0,
            );
            let observed_control_revision = activity.control_revision;
            activity.stage_reconciliation_controls(observed_control_revision, active.controls);
        }

        runtime
            .handle_notification(
                "turn/completed".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "turn-1", "status": "completed", "completedAt": 4}
                }),
                4_000,
            )
            .await;
        emit_successful_reconciliation(&runtime).await;

        let events = drain_runtime_events(&runtime).await;
        let reconciliation = events
            .iter()
            .find(|event| {
                event
                    .native_event_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("codex:reconciliation:"))
            })
            .expect("reconciliation event");
        assert!(reconciliation.activity_controls.is_empty());
        assert!(
            !runtime
                .inner
                .activity
                .lock()
                .await
                .tracker
                .is_current_target("durable-child", "turn-1")
        );
    }

    async fn run_reverse_control_race_peer(
        peer_stdin: DuplexStream,
        peer_stdout: DuplexStream,
        child_read_seen: oneshot::Sender<()>,
        stale_child_response: oneshot::Receiver<Value>,
        expect_interrupt: bool,
    ) -> Option<Value> {
        let mut reader = BufReader::new(peer_stdin);
        let mut writer = peer_stdout;

        let root_read = read_runtime_test_json(&mut reader).await;
        assert_eq!(root_read["method"], "thread/read");
        write_runtime_test_history(
            &mut reader,
            &mut writer,
            &root_read,
            root_reconciliation_response(&root_read),
        )
        .await;

        let list = read_runtime_test_json(&mut reader).await;
        assert_eq!(list["method"], "thread/list");
        write_runtime_test_json(
            &mut writer,
            json!({
                "id": list["id"].clone(),
                "result": {
                    "data": [
                        {
                            "id": "durable-child",
                            "parentThreadId": "provider-root",
                            "createdAt": 1,
                            "updatedAt": 2,
                            "status": {"type": "active", "activeFlags": []}
                        },
                        {
                            "id": "unrelated-child",
                            "parentThreadId": "provider-root",
                            "createdAt": 1,
                            "updatedAt": 2,
                            "status": {"type": "active", "activeFlags": []}
                        }
                    ],
                    "nextCursor": null,
                    "backwardsCursor": null
                }
            }),
        )
        .await;

        let child_read = read_runtime_test_json(&mut reader).await;
        assert_eq!(child_read["method"], "thread/read");
        assert_eq!(child_read["params"]["threadId"], "durable-child");
        child_read_seen.send(()).expect("signal pending child read");
        let mut stale = stale_child_response
            .await
            .expect("release stale child read");
        stale["id"] = child_read["id"].clone();
        write_runtime_test_history(&mut reader, &mut writer, &child_read, stale).await;

        let unrelated_read = read_runtime_test_json(&mut reader).await;
        assert_eq!(unrelated_read["method"], "thread/read");
        assert_eq!(unrelated_read["params"]["threadId"], "unrelated-child");
        write_runtime_test_json(
            &mut writer,
            json!({
                "id": unrelated_read["id"].clone(),
                "result": {
                    "thread": {
                        "id": "unrelated-child",
                        "parentThreadId": "provider-root",
                        "createdAt": 1,
                        "updatedAt": 6,
                        "status": {"type": "active", "activeFlags": []},
                        "turns": [{
                            "id": "unrelated-turn",
                            "status": "inProgress",
                            "startedAt": 6,
                            "items": []
                        }]
                    }
                }
            }),
        )
        .await;
        answer_runtime_test_turn_pages(
            &mut reader,
            &mut writer,
            &unrelated_read,
            &json!({
                "thread": {
                    "id": "unrelated-child",
                    "parentThreadId": "provider-root",
                    "createdAt": 1,
                    "updatedAt": 6,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": [{
                        "id": "unrelated-turn",
                        "status": "inProgress",
                        "startedAt": 6,
                        "items": []
                    }]
                }
            }),
        )
        .await;

        let background = read_runtime_test_json(&mut reader).await;
        assert_eq!(background["method"], "thread/backgroundTerminals/list");
        write_runtime_test_json(
            &mut writer,
            json!({
                "id": background["id"].clone(),
                "result": {"data": [], "nextCursor": null}
            }),
        )
        .await;

        let request = timeout(
            Duration::from_millis(500),
            read_runtime_test_json(&mut reader),
        )
        .await;
        match (expect_interrupt, request) {
            (true, Ok(request)) => {
                write_runtime_test_json(
                    &mut writer,
                    json!({"id": request["id"].clone(), "result": {}}),
                )
                .await;
                Some(request)
            }
            (false, Err(_)) => None,
            (true, Err(_)) => panic!("expected exact child interrupt"),
            (false, Ok(request)) => Some(request),
        }
    }

    fn hint_race_history_response(
        request: &Value,
        owner_thread_id: &str,
        durable_hint_kind: &str,
    ) -> Value {
        let mut thread = json!({
            "id": owner_thread_id,
            "createdAt": 1,
            "updatedAt": 5,
            "status": {"type": "active", "activeFlags": []},
            "turns": [{
                "id": "hint-turn",
                "startedAt": 5,
                "completedAt": 5,
                "items": [
                    {
                        "id": "stale-interrupt",
                        "type": "subAgentActivity",
                        "agentThreadId": "durable-child",
                        "agentPath": "/root/durable-child",
                        "kind": durable_hint_kind
                    },
                    {
                        "id": "unrelated-start",
                        "type": "subAgentActivity",
                        "agentThreadId": "unrelated-child",
                        "agentPath": "/root/unrelated-child",
                        "kind": "started"
                    }
                ]
            }]
        });
        if owner_thread_id != "provider-root" {
            thread["parentThreadId"] = json!("provider-root");
        }
        json!({"id": request["id"].clone(), "result": {"thread": thread}})
    }

    async fn run_hint_control_race_peer(
        peer_stdin: DuplexStream,
        peer_stdout: DuplexStream,
        nested_owner: bool,
        durable_hint_kind: &'static str,
        hint_read_seen: oneshot::Sender<()>,
        release_hint_read: oneshot::Receiver<()>,
        expect_interrupt: bool,
    ) -> Option<Value> {
        let mut reader = BufReader::new(peer_stdin);
        let mut writer = peer_stdout;
        let mut hint_read_seen = Some(hint_read_seen);
        let mut release_hint_read = Some(release_hint_read);

        let root_read = read_runtime_test_json(&mut reader).await;
        assert_eq!(root_read["method"], "thread/read");
        if nested_owner {
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &root_read,
                root_reconciliation_response(&root_read),
            )
            .await;
        } else {
            hint_read_seen
                .take()
                .expect("root hint signal")
                .send(())
                .expect("signal pending root history");
            release_hint_read
                .take()
                .expect("root hint release")
                .await
                .expect("release root history");
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &root_read,
                hint_race_history_response(&root_read, "provider-root", durable_hint_kind),
            )
            .await;
        }

        let list = read_runtime_test_json(&mut reader).await;
        assert_eq!(list["method"], "thread/list");
        write_runtime_test_json(
            &mut writer,
            json!({
                "id": list["id"].clone(),
                "result": {"data": [], "nextCursor": null, "backwardsCursor": null}
            }),
        )
        .await;

        if nested_owner {
            let parent_read = read_runtime_test_json(&mut reader).await;
            assert_eq!(parent_read["method"], "thread/read");
            assert_eq!(parent_read["params"]["threadId"], "nested-parent");
            hint_read_seen
                .take()
                .expect("nested hint signal")
                .send(())
                .expect("signal pending nested history");
            release_hint_read
                .take()
                .expect("nested hint release")
                .await
                .expect("release nested history");
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &parent_read,
                hint_race_history_response(&parent_read, "nested-parent", durable_hint_kind),
            )
            .await;
        }

        loop {
            let request = read_runtime_test_json(&mut reader).await;
            if request["method"] == "thread/backgroundTerminals/list" {
                write_runtime_test_json(
                    &mut writer,
                    json!({"id": request["id"].clone(), "result": {"data": [], "nextCursor": null}}),
                )
                .await;
                break;
            }
            assert_eq!(request["method"], "thread/read");
            let child_thread_id = request["params"]["threadId"]
                .as_str()
                .expect("child thread ID");
            assert!(matches!(
                child_thread_id,
                "durable-child" | "unrelated-child"
            ));
            let turns = if child_thread_id == "unrelated-child" {
                json!([{
                    "id": "unrelated-turn",
                    "status": "inProgress",
                    "startedAt": 6,
                    "items": []
                }])
            } else {
                json!([])
            };
            let parent_thread_id = if nested_owner {
                "nested-parent"
            } else {
                "provider-root"
            };
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": request["id"].clone(),
                    "result": {
                        "thread": {
                            "id": child_thread_id,
                            "parentThreadId": parent_thread_id,
                            "createdAt": 1,
                            "updatedAt": 6,
                            "status": {"type": "active", "activeFlags": []},
                            "turns": turns
                        }
                    }
                }),
            )
            .await;
            answer_runtime_test_turn_pages(
                &mut reader,
                &mut writer,
                &request,
                &json!({
                    "thread": {
                        "id": child_thread_id,
                        "parentThreadId": parent_thread_id,
                        "createdAt": 1,
                        "updatedAt": 6,
                        "status": {"type": "active", "activeFlags": []},
                        "turns": turns
                    }
                }),
            )
            .await;
        }

        let request = timeout(
            Duration::from_millis(500),
            read_runtime_test_json(&mut reader),
        )
        .await;
        let Ok(request) = request else {
            assert!(!expect_interrupt, "expected exact interrupt request");
            return None;
        };
        if request["method"] == "turn/interrupt" {
            write_runtime_test_json(
                &mut writer,
                json!({"id": request["id"].clone(), "result": {}}),
            )
            .await;
        }
        Some(request)
    }

    async fn assert_live_target_survives_stale_hint_history(nested_owner: bool) {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        let durable_parent_id = if nested_owner {
            "nested-parent"
        } else {
            "provider-root"
        };
        let descendants = [
            decode_thread_read_response(json!({
                "thread": {
                    "id": "nested-parent",
                    "parentThreadId": "provider-root",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("nested parent")
            .thread,
            decode_thread_read_response(json!({
                "thread": {
                    "id": "durable-child",
                    "parentThreadId": durable_parent_id,
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("durable child")
            .thread,
        ];
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity.tracker.reconcile_descendants(&descendants);
            if nested_owner {
                activity.enqueue_hinted_descendants(&["nested-parent".to_owned()]);
            }
        }
        let (read_seen_tx, read_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let peer = tokio::spawn(run_hint_control_race_peer(
            peer_stdin,
            peer_stdout,
            nested_owner,
            "interrupted",
            read_seen_tx,
            release_rx,
            true,
        ));
        let reconcile_runtime = runtime.clone();
        let reconciliation = tokio::spawn(async move {
            reconcile_runtime.reconcile_once().await;
        });
        read_seen_rx.await.expect("hint history read is pending");

        runtime
            .handle_notification(
                "turn/started".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "live-turn", "status": "inProgress", "startedAt": 5}
                }),
                5_000,
            )
            .await;
        release_tx.send(()).expect("release stale hint history");
        reconciliation.await.expect("reconciliation task");

        let activity = runtime.inner.activity.lock().await;
        assert!(
            activity
                .tracker
                .is_current_target("durable-child", "live-turn"),
            "a stale hinted receiver must not mutate the newer live tracker target"
        );
        assert!(
            activity
                .tracker
                .is_current_target("unrelated-child", "unrelated-turn"),
            "unrelated hinted receivers must still reconcile"
        );
        assert!(
            activity
                .pending_hinted_descendants_snapshot()
                .iter()
                .any(|thread_id| thread_id == "durable-child"),
            "the superseded receiver must remain queued for a bounded fresh retry"
        );
        drop(activity);
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            actor_control_targets(&events, "codex:thread:durable-child").last(),
            Some(&Some(("durable-child".to_owned(), "live-turn".to_owned())))
        );
        assert_eq!(
            actor_control_targets(&events, "codex:thread:unrelated-child").last(),
            Some(&Some((
                "unrelated-child".to_owned(),
                "unrelated-turn".to_owned()
            )))
        );
        runtime
            .interrupt_targeted_turn("durable-child", "live-turn")
            .await
            .expect("newer live target remains cancellable");
        let request = peer.await.expect("peer").expect("interrupt request");
        assert_eq!(request["method"], "turn/interrupt");
        assert_eq!(
            request["params"],
            json!({"threadId": "durable-child", "turnId": "live-turn"})
        );
    }

    #[tokio::test]
    async fn live_start_before_stale_root_hint_preserves_tracker_overlay_and_exact_interrupt() {
        assert_live_target_survives_stale_hint_history(false).await;
    }

    #[tokio::test]
    async fn targeted_interrupts_do_not_wait_for_an_unrelated_provider_response() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        let descendants = [
            decode_thread_read_response(json!({
                "thread": {
                    "id": "child-a",
                    "parentThreadId": "provider-root",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("child a")
            .thread,
            decode_thread_read_response(json!({
                "thread": {
                    "id": "child-b",
                    "parentThreadId": "provider-root",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("child b")
            .thread,
        ];
        runtime
            .inner
            .activity
            .lock()
            .await
            .tracker
            .reconcile_descendants(&descendants);
        for (thread_id, turn_id) in [("child-a", "turn-a"), ("child-b", "turn-b")] {
            runtime
                .handle_notification(
                    "turn/started".to_owned(),
                    json!({
                        "threadId": thread_id,
                        "turn": {"id": turn_id, "status": "inProgress", "startedAt": 3}
                    }),
                    3_000,
                )
                .await;
        }

        let first_runtime = runtime.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .interrupt_targeted_turn("child-a", "turn-a")
                .await
        });
        let mut reader = BufReader::new(peer_stdin);
        let mut writer = peer_stdout;
        let first_request = read_runtime_test_json(&mut reader).await;
        assert_eq!(first_request["method"], "turn/interrupt");
        assert_eq!(
            first_request["params"],
            json!({"threadId": "child-a", "turnId": "turn-a"})
        );

        let second_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            second_runtime
                .interrupt_targeted_turn("child-b", "turn-b")
                .await
        });
        let second_request = timeout(Duration::from_secs(1), read_runtime_test_json(&mut reader))
            .await
            .expect("a second exact interrupt must not wait for the first provider response");
        assert_eq!(second_request["method"], "turn/interrupt");
        assert_eq!(
            second_request["params"],
            json!({"threadId": "child-b", "turnId": "turn-b"})
        );

        write_runtime_test_json(
            &mut writer,
            json!({"id": second_request["id"].clone(), "result": {}}),
        )
        .await;
        write_runtime_test_json(
            &mut writer,
            json!({"id": first_request["id"].clone(), "result": {}}),
        )
        .await;
        second
            .await
            .expect("second interrupt task")
            .expect("second interrupt succeeds");
        first
            .await
            .expect("first interrupt task")
            .expect("first interrupt succeeds");
    }

    #[tokio::test]
    async fn live_start_before_stale_nested_hint_preserves_receiver_target_and_exact_interrupt() {
        assert_live_target_survives_stale_hint_history(true).await;
    }

    #[tokio::test]
    async fn live_completion_before_stale_active_root_hint_preserves_terminal_target_without_io() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        runtime
            .inner
            .activity
            .lock()
            .await
            .tracker
            .reconcile_descendants(&[verified_reconciliation_child()]);
        runtime
            .handle_notification(
                "turn/started".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "live-turn", "status": "inProgress", "startedAt": 3}
                }),
                3_000,
            )
            .await;
        let (read_seen_tx, read_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let peer = tokio::spawn(run_hint_control_race_peer(
            peer_stdin,
            peer_stdout,
            false,
            "started",
            read_seen_tx,
            release_rx,
            false,
        ));
        let reconcile_runtime = runtime.clone();
        let reconciliation = tokio::spawn(async move {
            reconcile_runtime.reconcile_once().await;
        });
        read_seen_rx.await.expect("root hint history is pending");

        runtime
            .handle_notification(
                "turn/completed".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "live-turn", "status": "completed", "completedAt": 5}
                }),
                5_000,
            )
            .await;
        release_tx.send(()).expect("release stale active hint");
        reconciliation.await.expect("reconciliation task");

        let activity = runtime.inner.activity.lock().await;
        assert!(
            !activity
                .tracker
                .is_current_target("durable-child", "live-turn")
        );
        assert!(
            activity
                .tracker
                .is_current_target("unrelated-child", "unrelated-turn")
        );
        assert!(
            activity
                .pending_hinted_descendants_snapshot()
                .iter()
                .any(|thread_id| thread_id == "durable-child")
        );
        drop(activity);
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            actor_control_targets(&events, "codex:thread:durable-child").last(),
            Some(&None)
        );
        let error = runtime
            .interrupt_targeted_turn("durable-child", "live-turn")
            .await
            .expect_err("completed target must fail before provider I/O");
        assert!(error.to_string().contains("active"));
        let follow_up = peer.await.expect("peer");
        assert!(
            follow_up
                .as_ref()
                .is_none_or(|request| request["method"] != "turn/interrupt"),
            "a fresh bounded reconciliation retry is allowed, but targeted provider I/O is not"
        );
    }

    #[tokio::test]
    async fn live_start_before_stale_child_read_revocation_preserves_tracker_and_exact_interrupt() {
        // Mutation caught: filtering stale publication only after reconciliation erased tracker state.
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        runtime
            .inner
            .activity
            .lock()
            .await
            .tracker
            .reconcile_descendants(&[verified_reconciliation_child()]);
        let (read_seen_tx, read_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let peer = tokio::spawn(run_reverse_control_race_peer(
            peer_stdin,
            peer_stdout,
            read_seen_tx,
            release_rx,
            true,
        ));
        let reconcile_runtime = runtime.clone();
        let reconciliation = tokio::spawn(async move {
            reconcile_runtime.reconcile_once().await;
        });
        read_seen_rx.await.expect("child read is pending");

        runtime
            .handle_notification(
                "turn/started".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "live-turn", "status": "inProgress", "startedAt": 5}
                }),
                5_000,
            )
            .await;
        release_tx
            .send(json!({
                "result": {
                    "thread": {
                        "id": "durable-child",
                        "parentThreadId": "provider-root",
                        "createdAt": 1,
                        "updatedAt": 4,
                        "status": {"type": "idle"},
                        "turns": []
                    }
                }
            }))
            .expect("release stale revocation");
        reconciliation.await.expect("reconciliation task");

        assert!(
            runtime
                .inner
                .activity
                .lock()
                .await
                .tracker
                .is_current_target("durable-child", "live-turn")
        );
        assert!(
            runtime
                .inner
                .activity
                .lock()
                .await
                .tracker
                .is_current_target("unrelated-child", "unrelated-turn")
        );
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            actor_control_targets(&events, "codex:thread:durable-child").last(),
            Some(&Some(("durable-child".to_owned(), "live-turn".to_owned())))
        );
        assert_eq!(
            actor_control_targets(&events, "codex:thread:unrelated-child").last(),
            Some(&Some((
                "unrelated-child".to_owned(),
                "unrelated-turn".to_owned()
            )))
        );
        runtime
            .interrupt_targeted_turn("durable-child", "live-turn")
            .await
            .expect("live target remains cancellable");
        let request = peer.await.expect("peer").expect("interrupt request");
        assert_eq!(request["method"], "turn/interrupt");
        assert_eq!(
            request["params"],
            json!({"threadId": "durable-child", "turnId": "live-turn"})
        );
    }

    #[tokio::test]
    async fn live_completion_before_stale_child_read_target_preserves_terminal_tracker_without_io()
    {
        // Mutation caught: filtering stale publication only after reconciliation reinstalled a target.
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        runtime
            .inner
            .activity
            .lock()
            .await
            .tracker
            .reconcile_descendants(&[verified_reconciliation_child()]);
        runtime
            .handle_notification(
                "turn/started".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "live-turn", "status": "inProgress", "startedAt": 3}
                }),
                3_000,
            )
            .await;
        let (read_seen_tx, read_seen_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let peer = tokio::spawn(run_reverse_control_race_peer(
            peer_stdin,
            peer_stdout,
            read_seen_tx,
            release_rx,
            false,
        ));
        let reconcile_runtime = runtime.clone();
        let reconciliation = tokio::spawn(async move {
            reconcile_runtime.reconcile_once().await;
        });
        read_seen_rx.await.expect("child read is pending");

        runtime
            .handle_notification(
                "turn/completed".to_owned(),
                json!({
                    "threadId": "durable-child",
                    "turn": {"id": "live-turn", "status": "completed", "completedAt": 4}
                }),
                4_000,
            )
            .await;
        release_tx
            .send(json!({
                "result": {
                    "thread": {
                        "id": "durable-child",
                        "parentThreadId": "provider-root",
                        "createdAt": 1,
                        "updatedAt": 5,
                        "status": {"type": "active", "activeFlags": []},
                        "turns": [{
                            "id": "live-turn",
                            "status": "inProgress",
                            "startedAt": 3,
                            "items": []
                        }]
                    }
                }
            }))
            .expect("release stale active turn");
        reconciliation.await.expect("reconciliation task");

        assert!(
            !runtime
                .inner
                .activity
                .lock()
                .await
                .tracker
                .is_current_target("durable-child", "live-turn")
        );
        assert!(
            runtime
                .inner
                .activity
                .lock()
                .await
                .tracker
                .is_current_target("unrelated-child", "unrelated-turn")
        );
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            actor_control_targets(&events, "codex:thread:durable-child").last(),
            Some(&None)
        );
        assert_eq!(
            actor_control_targets(&events, "codex:thread:unrelated-child").last(),
            Some(&Some((
                "unrelated-child".to_owned(),
                "unrelated-turn".to_owned()
            )))
        );
        let error = runtime
            .interrupt_targeted_turn("durable-child", "live-turn")
            .await
            .expect_err("completed target must fail before provider I/O");
        assert!(error.to_string().contains("active"));
        let follow_up = peer.await.expect("peer");
        assert!(
            follow_up
                .as_ref()
                .is_none_or(|request| request["method"] != "turn/interrupt"),
            "a fresh bounded reconciliation retry is allowed, but targeted provider I/O is not"
        );
    }

    fn root_reconciliation_response(request: &Value) -> Value {
        json!({
            "id": request["id"].clone(),
            "result": {
                "thread": {
                    "id": "provider-root",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "idle"},
                    "turns": []
                }
            }
        })
    }

    fn child_list_reconciliation_response(request: &Value) -> Value {
        json!({
            "id": request["id"].clone(),
            "result": {
                "data": [{
                    "id": "durable-child",
                    "parentThreadId": "provider-root",
                    "agentNickname": "Durable child",
                    "createdAt": 3,
                    "updatedAt": 4,
                    "status": {"type": "active", "activeFlags": []}
                }],
                "nextCursor": null,
                "backwardsCursor": null
            }
        })
    }

    fn child_read_reconciliation_response(request: &Value) -> Value {
        json!({
            "id": request["id"].clone(),
            "result": {
                "thread": {
                    "id": "durable-child",
                    "parentThreadId": "provider-root",
                    "agentNickname": "Durable child",
                    "createdAt": 3,
                    "updatedAt": 4,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }
        })
    }

    fn durable_child_actor_mutation_count(events: &[RuntimeEvent]) -> usize {
        events
            .iter()
            .flat_map(|event| event.activity.iter())
            .filter(|mutation| {
                matches!(
                    mutation,
                    ProviderActivityMutation::UpsertActor(actor)
                        if actor.name == "Durable child"
                )
            })
            .count()
    }

    #[tokio::test]
    async fn reconciliation_retries_unpublished_child_after_later_transient_failure() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;
            for pass in 0..2 {
                let root_read = read_runtime_test_json(&mut reader).await;
                assert_eq!(root_read["method"], "thread/read");
                write_runtime_test_history(
                    &mut reader,
                    &mut writer,
                    &root_read,
                    root_reconciliation_response(&root_read),
                )
                .await;

                let list = read_runtime_test_json(&mut reader).await;
                assert_eq!(list["method"], "thread/list");
                write_runtime_test_json(&mut writer, child_list_reconciliation_response(&list))
                    .await;

                if pass == 0 {
                    let child_read = read_runtime_test_json(&mut reader).await;
                    assert_eq!(child_read["method"], "thread/read");
                    assert_eq!(child_read["params"]["threadId"], "durable-child");
                    write_runtime_test_history(
                        &mut reader,
                        &mut writer,
                        &child_read,
                        child_read_reconciliation_response(&child_read),
                    )
                    .await;
                }

                let background = read_runtime_test_json(&mut reader).await;
                assert_eq!(background["method"], "thread/backgroundTerminals/list");
                let response = if pass == 0 {
                    json!({
                        "id": background["id"].clone(),
                        "error": {"code": -32000, "message": "temporary failure"}
                    })
                } else {
                    json!({
                        "id": background["id"].clone(),
                        "result": {"data": [], "nextCursor": null}
                    })
                };
                write_runtime_test_json(&mut writer, response).await;
            }
        });

        runtime.reconcile_once().await;
        runtime.reconcile_once().await;
        peer.await.expect("reconciliation peer");

        let events = drain_runtime_events(&runtime).await;
        assert_eq!(
            durable_child_actor_mutation_count(&events),
            1,
            "the unchanged recovery pass must publish the child mutation retained before the transient failure"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event
                    .native_event_id
                    .as_deref()
                    .is_some_and(|id| { id.starts_with("codex:reconciliation:") }))
                .count(),
            2,
            "the failed pass should publish only stale health and the recovery pass one current snapshot"
        );
    }

    #[tokio::test]
    async fn same_root_generation_replacement_retries_unpublished_child_without_old_pass_leakage() {
        let (connection, incoming, peer_stdout, peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        let (background_seen_tx, background_seen_rx) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(peer_stdin);
            let mut writer = peer_stdout;

            let root_read = read_runtime_test_json(&mut reader).await;
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &root_read,
                root_reconciliation_response(&root_read),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, child_list_reconciliation_response(&list)).await;
            let child_read = read_runtime_test_json(&mut reader).await;
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &child_read,
                child_read_reconciliation_response(&child_read),
            )
            .await;
            let cancelled_background = read_runtime_test_json(&mut reader).await;
            assert_eq!(
                cancelled_background["method"],
                "thread/backgroundTerminals/list"
            );
            background_seen_tx
                .send(())
                .expect("report cancellable background request");

            let root_read = read_runtime_test_json(&mut reader).await;
            write_runtime_test_history(
                &mut reader,
                &mut writer,
                &root_read,
                root_reconciliation_response(&root_read),
            )
            .await;
            let list = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(&mut writer, child_list_reconciliation_response(&list)).await;
            let background = read_runtime_test_json(&mut reader).await;
            write_runtime_test_json(
                &mut writer,
                json!({
                    "id": background["id"].clone(),
                    "result": {"data": [], "nextCursor": null}
                }),
            )
            .await;
        });

        let first_runtime = runtime.clone();
        let first_pass = tokio::spawn(async move { first_runtime.reconcile_once().await });
        background_seen_rx
            .await
            .expect("first pass reaches the final request after child processing");
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity.reconciliation_epoch = activity.reconciliation_epoch.wrapping_add(1);
            activity.reconciliation_pass_cancellation.cancel();
            activity.reconciliation_pass_cancellation = CancellationToken::new();
            activity.hint_source_versions.clear();
        }
        first_pass.await.expect("cancelled reconciliation task");
        assert!(
            drain_runtime_events(&runtime).await.is_empty(),
            "the cancelled old pass must not publish its staged child"
        );

        runtime.reconcile_once().await;
        peer.await.expect("same-root recovery peer");
        let events = drain_runtime_events(&runtime).await;
        assert_eq!(durable_child_actor_mutation_count(&events), 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| event
                    .native_event_id
                    .as_deref()
                    .is_some_and(|id| { id.starts_with("codex:reconciliation:") }))
                .count(),
            1,
            "only the current generation may publish reconciliation data"
        );
    }

    #[test]
    fn bounded_reconciliation_discards_only_superseded_provisional_actor_upserts() {
        let mut record_mutations = Vec::new();
        for child_index in 0..usize::from(RECONCILIATION_DESCENDANT_LIMIT) {
            let actor_id = format!("codex:thread:child-{child_index}");
            for status in ["starting", "running", "completed"] {
                record_mutations.push(
                    ProviderActivityMutation::upsert_actor(
                        actor_id.clone(),
                        None,
                        format!("Child {child_index}"),
                        status,
                    )
                    .expect("valid actor mutation"),
                );
            }
        }
        for work_index in 0..usize::from(RECONCILIATION_BACKGROUND_LIMIT) {
            record_mutations.push(
                ProviderActivityMutation::remove_work_item(format!(
                    "codex:item:background-{work_index}"
                ))
                .expect("valid work mutation"),
            );
        }
        let scope_mutations = vec![
            ProviderActivityMutation::remove_actor("codex:scope:one")
                .expect("valid scope placeholder"),
            ProviderActivityMutation::remove_actor("codex:scope:two")
                .expect("valid scope placeholder"),
        ];

        let mutations = bounded_reconciliation_mutations(scope_mutations, record_mutations);

        assert_eq!(mutations.len(), 230);
        for child_index in 0..usize::from(RECONCILIATION_DESCENDANT_LIMIT) {
            let actor_id = format!("codex:thread:child-{child_index}");
            let actors = mutations
                .iter()
                .filter_map(|mutation| match mutation {
                    ProviderActivityMutation::UpsertActor(actor) if actor.id == actor_id => {
                        Some(actor)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(actors.len(), 2);
            assert_eq!(
                actors[0].status,
                crate::activity::ActivityLifecycle::Running
            );
            assert_eq!(
                actors[1].status,
                crate::activity::ActivityLifecycle::Completed
            );
        }
    }

    #[tokio::test]
    async fn pending_reconciliation_buffer_reserves_capacity_for_current_scope_health() {
        let pending = (0..RECONCILIATION_MUTATION_LIMIT)
            .map(|index| {
                ProviderActivityMutation::upsert_actor(
                    format!("codex:thread:pending-{index}"),
                    None,
                    format!("Pending {index}"),
                    "running",
                )
                .expect("valid pending actor")
            })
            .collect::<Vec<_>>();
        let (connection, incoming, _peer_stdout, _peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        runtime
            .inner
            .activity
            .lock()
            .await
            .stage_reconciliation_mutations(pending);
        let pass = {
            let activity = runtime.inner.activity.lock().await;
            ReconciliationPass {
                epoch: activity.reconciliation_epoch,
                root_thread_id: "provider-root".to_owned(),
                cancellation: activity.reconciliation_pass_cancellation.clone(),
                observed_control_revision: activity.control_revision,
            }
        };
        let capabilities = ActivityCapabilities::structured_full(false);
        let scope = vec![
            ProviderActivityMutation::SetScope {
                capabilities: capabilities.clone(),
                observation_state: ActivityObservationState::Live,
            },
            ProviderActivityMutation::SetSectionHealth {
                section: ActivitySection::BackgroundTasks,
                health: ActivitySectionHealth::live(),
            },
        ];
        runtime
            .emit_reconciliation(
                &pass,
                ReconciliationEmission::Successful {
                    capabilities,
                    mutations: scope,
                },
            )
            .await;
        let publications = drain_runtime_events(&runtime).await;

        assert_eq!(publications.len(), 2);
        assert_eq!(
            publications[0].activity.len(),
            RECONCILIATION_MUTATION_LIMIT
        );
        assert_eq!(publications[1].activity.len(), 2);
        assert!(matches!(
            publications[0].activity.first(),
            Some(ProviderActivityMutation::SetScope { .. })
        ));
        assert!(matches!(
            publications[0].activity.get(1),
            Some(ProviderActivityMutation::SetSectionHealth { .. })
        ));
    }

    #[tokio::test]
    async fn saturated_pending_reconciliation_preserves_actor_topology_across_published_batches() {
        let (connection, incoming, _peer_stdout, _peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;

        let parent_id = "codex:thread:saturation-parent";
        let child_id = "codex:thread:saturation-child";
        let mut acknowledged = vec![
            ProviderActivityMutation::upsert_actor(parent_id, None, "Saturation parent", "running")
                .expect("valid parent actor"),
        ];
        acknowledged.extend((0..253).map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("codex:thread:saturation-filler-{index:03}"),
                None,
                format!("Saturation filler {index:03}"),
                "running",
            )
            .expect("valid filler actor")
        }));
        acknowledged.push(
            ProviderActivityMutation::upsert_actor(
                child_id,
                Some(parent_id),
                "Saturation child",
                "running",
            )
            .expect("valid child actor"),
        );
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity.stage_reconciliation_mutations(acknowledged);
        }
        let pass = {
            let activity = runtime.inner.activity.lock().await;
            ReconciliationPass {
                epoch: activity.reconciliation_epoch,
                root_thread_id: "provider-root".to_owned(),
                cancellation: activity.reconciliation_pass_cancellation.clone(),
                observed_control_revision: activity.control_revision,
            }
        };
        let capabilities = ActivityCapabilities::structured_full(false);
        runtime
            .emit_reconciliation(
                &pass,
                ReconciliationEmission::Successful {
                    capabilities: capabilities.clone(),
                    mutations: vec![
                        ProviderActivityMutation::SetScope {
                            capabilities: capabilities.clone(),
                            observation_state: ActivityObservationState::Live,
                        },
                        ProviderActivityMutation::SetSectionHealth {
                            section: ActivitySection::BackgroundTasks,
                            health: ActivitySectionHealth::live(),
                        },
                    ],
                },
            )
            .await;

        let events = drain_runtime_events(&runtime).await;
        assert!(
            !events.is_empty(),
            "saturation must publish reconciliation data"
        );
        assert!(
            events
                .iter()
                .all(|event| event.activity.len() <= RECONCILIATION_MUTATION_LIMIT),
            "every published batch must stay within the repository limit"
        );

        let database = Database::open_in_memory().await.expect("activity database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("activity migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let scope = ActivityScopeSeed::thread(
            "scope:codex:saturation",
            "fixture-thread",
            PROVIDER,
            Some(PROVIDER),
            capabilities,
        )
        .expect("valid activity scope");
        projection
            .ensure_scope(scope.clone())
            .await
            .expect("activity scope");
        for event in &events {
            projection
                .apply(
                    &scope.scope_id,
                    event
                        .native_event_id
                        .clone()
                        .expect("reconciliation event key"),
                    event.activity.clone(),
                    event.created_at.clone(),
                )
                .await
                .expect("every published batch must be repository-valid");
        }

        let snapshot = projection
            .snapshot(&ActivityScopeRef::Thread {
                thread_id: "fixture-thread".to_owned(),
            })
            .await
            .expect("projected activity snapshot");
        assert_eq!(snapshot.counts.subagents.active, 255);
        for (actor_id, expected_parent) in [(parent_id, None), (child_id, Some(parent_id))] {
            let detail = projection
                .list_detail(
                    &scope.scope,
                    &scope.scope_id,
                    ActivityRecordKind::Actor,
                    actor_id,
                    None,
                    1,
                )
                .await
                .expect("acknowledged actor must be retained");
            let ActivityRecordSummary::Actor(actor) = detail.record else {
                panic!("actor detail returned a work item");
            };
            assert_eq!(actor.parent_actor_id.as_deref(), expected_parent);
        }
        assert!(
            runtime
                .inner
                .activity
                .lock()
                .await
                .pending_reconciliation_mutations
                .is_empty(),
            "successful publication must acknowledge every staged mutation"
        );
    }

    #[tokio::test]
    async fn saturated_pending_reconciliation_orders_each_retained_reparent_state() {
        let (connection, incoming, _peer_stdout, _peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;

        let parent_one = "codex:thread:reparent-p1";
        let child = "codex:thread:reparent-c";
        let parent_two = "codex:thread:reparent-p2";
        let grandparent = "codex:thread:reparent-d";
        let mut acknowledged = vec![
            ProviderActivityMutation::upsert_actor(parent_one, None, "Parent one", "running")
                .expect("valid first parent state"),
            ProviderActivityMutation::upsert_actor(
                child,
                Some(parent_one),
                "Reparented child",
                "running",
            )
            .expect("valid first child state"),
            ProviderActivityMutation::upsert_actor(parent_two, None, "Parent two", "running")
                .expect("valid second parent"),
            ProviderActivityMutation::upsert_actor(
                child,
                Some(parent_two),
                "Reparented child",
                "waiting",
            )
            .expect("valid second child state"),
            ProviderActivityMutation::upsert_actor(grandparent, None, "Grandparent", "running")
                .expect("valid grandparent"),
            ProviderActivityMutation::upsert_actor(
                parent_one,
                Some(grandparent),
                "Parent one",
                "waiting",
            )
            .expect("valid reparented first parent"),
        ];
        acknowledged.extend((0..249).map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("codex:thread:reparent-filler-{index:03}"),
                None,
                format!("Reparent filler {index:03}"),
                "running",
            )
            .expect("valid filler actor")
        }));
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity.stage_reconciliation_mutations(acknowledged);
        }
        let pass = {
            let activity = runtime.inner.activity.lock().await;
            ReconciliationPass {
                epoch: activity.reconciliation_epoch,
                root_thread_id: "provider-root".to_owned(),
                cancellation: activity.reconciliation_pass_cancellation.clone(),
                observed_control_revision: activity.control_revision,
            }
        };
        let capabilities = ActivityCapabilities::structured_full(false);
        runtime
            .emit_reconciliation(
                &pass,
                ReconciliationEmission::Successful {
                    capabilities: capabilities.clone(),
                    mutations: vec![
                        ProviderActivityMutation::SetScope {
                            capabilities: capabilities.clone(),
                            observation_state: ActivityObservationState::Live,
                        },
                        ProviderActivityMutation::SetSectionHealth {
                            section: ActivitySection::BackgroundTasks,
                            health: ActivitySectionHealth::live(),
                        },
                    ],
                },
            )
            .await;

        let events = drain_runtime_events(&runtime).await;
        assert_eq!(events.len(), 2, "saturation must exercise continuation");
        assert!(
            events
                .iter()
                .all(|event| event.activity.len() <= RECONCILIATION_MUTATION_LIMIT)
        );
        let retained_parents = |actor_id: &str| {
            events
                .iter()
                .flat_map(|event| event.activity.iter())
                .filter_map(|mutation| match mutation {
                    ProviderActivityMutation::UpsertActor(actor) if actor.id == actor_id => {
                        Some(actor.parent_actor_id.as_deref())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            retained_parents(child),
            [Some(parent_one), Some(parent_two)]
        );
        assert_eq!(retained_parents(parent_one), [None, Some(grandparent)]);

        let database = Database::open_in_memory().await.expect("activity database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("activity migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let scope = ActivityScopeSeed::thread(
            "scope:codex:reparent",
            "fixture-thread",
            PROVIDER,
            Some(PROVIDER),
            capabilities,
        )
        .expect("valid activity scope");
        projection
            .ensure_scope(scope.clone())
            .await
            .expect("activity scope");
        for event in &events {
            projection
                .apply(
                    &scope.scope_id,
                    event
                        .native_event_id
                        .clone()
                        .expect("reconciliation event key"),
                    event.activity.clone(),
                    event.created_at.clone(),
                )
                .await
                .expect("every retained reparent state must be repository-valid");
        }

        let snapshot = projection
            .snapshot(&scope.scope)
            .await
            .expect("projected activity snapshot");
        assert_eq!(snapshot.counts.subagents.active, 253);
        for (actor_id, expected_parent) in
            [(child, Some(parent_two)), (parent_one, Some(grandparent))]
        {
            let detail = projection
                .list_detail(
                    &scope.scope,
                    &scope.scope_id,
                    ActivityRecordKind::Actor,
                    actor_id,
                    None,
                    1,
                )
                .await
                .expect("reparented actor detail");
            let ActivityRecordSummary::Actor(actor) = detail.record else {
                panic!("actor detail returned a work item");
            };
            assert_eq!(actor.parent_actor_id.as_deref(), expected_parent);
        }
    }

    #[tokio::test]
    async fn cyclic_pending_reconciliation_warns_without_acknowledging_staged_states() {
        let (connection, incoming, _peer_stdout, _peer_stdin, _peer_stderr) =
            runtime_test_connection();
        let runtime = CodexSessionRuntime::new(reconciliation_test_options(), connection, incoming);
        configure_reconciliation_test_runtime(&runtime).await;
        let capabilities = ActivityCapabilities::structured_full(false);
        let database = Database::open_in_memory().await.expect("activity database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("activity migrations");
        let projection = ActivityProjection::new(ActivityRepository::new(database));
        let scope = ActivityScopeSeed::thread(
            "scope:codex:cycle",
            "fixture-thread",
            PROVIDER,
            Some(PROVIDER),
            capabilities.clone(),
        )
        .expect("valid activity scope");
        projection
            .ensure_scope(scope.clone())
            .await
            .expect("activity scope");
        let snapshot_before = projection
            .snapshot(&scope.scope)
            .await
            .expect("activity snapshot before cyclic reconciliation");
        {
            let mut activity = runtime.inner.activity.lock().await;
            activity.stage_reconciliation_mutations([
                ProviderActivityMutation::upsert_actor(
                    "codex:thread:cycle-a",
                    None,
                    "Cycle A",
                    "running",
                )
                .expect("valid initial actor"),
                ProviderActivityMutation::upsert_actor(
                    "codex:thread:cycle-b",
                    Some("codex:thread:cycle-a"),
                    "Cycle B",
                    "running",
                )
                .expect("valid child actor"),
                ProviderActivityMutation::upsert_actor(
                    "codex:thread:cycle-a",
                    Some("codex:thread:cycle-b"),
                    "Cycle A",
                    "waiting",
                )
                .expect("individually valid cyclic reparent"),
            ]);
            assert!(!activity.pending_reconciliation_topology_valid);
        }
        let pass = {
            let activity = runtime.inner.activity.lock().await;
            ReconciliationPass {
                epoch: activity.reconciliation_epoch,
                root_thread_id: "provider-root".to_owned(),
                cancellation: activity.reconciliation_pass_cancellation.clone(),
                observed_control_revision: activity.control_revision,
            }
        };
        runtime
            .emit_reconciliation(
                &pass,
                ReconciliationEmission::Successful {
                    capabilities: capabilities.clone(),
                    mutations: vec![ProviderActivityMutation::SetScope {
                        capabilities,
                        observation_state: ActivityObservationState::Live,
                    }],
                },
            )
            .await;

        let events = drain_runtime_events(&runtime).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "runtime.warning");
        assert!(events[0].activity.is_empty());
        let mut applied_activity_batches = 0;
        for event in &events {
            if event.activity.is_empty() {
                continue;
            }
            applied_activity_batches += 1;
            projection
                .apply(
                    &scope.scope_id,
                    event.native_event_id.clone().expect("activity event key"),
                    event.activity.clone(),
                    event.created_at.clone(),
                )
                .await
                .expect("every emitted activity batch must be repository-valid");
        }
        assert_eq!(
            applied_activity_batches, 0,
            "unsafe topology must not emit an activity batch"
        );
        let snapshot_after = projection
            .snapshot(&scope.scope)
            .await
            .expect("activity snapshot after cyclic reconciliation");
        assert_eq!(
            snapshot_after, snapshot_before,
            "warning-only reconciliation must not mutate the repository scope or records"
        );
        assert!(snapshot_after.actors.is_empty());
        assert!(snapshot_after.work_items.is_empty());
        let activity = runtime.inner.activity.lock().await;
        assert_eq!(activity.pending_reconciliation_mutations.len(), 3);
        assert!(!activity.pending_reconciliation_topology_valid);
    }

    #[test]
    fn user_input_normalization_filters_questions_and_flattens_single_answers() {
        assert_eq!(
            normalize_questions(json!([
                {
                    "id":"choice",
                    "header":"Choice",
                    "question":"Pick one",
                    "options":[{"label":"A","description":"First"}]
                },
                {"id":"missing-fields"}
            ])),
            json!([{
                "id":"choice",
                "header":"Choice",
                "question":"Pick one",
                "options":[{"label":"A","description":"First"}],
                "multiSelect":false
            }])
        );
        assert_eq!(normalize_questions(Value::Null), json!([]));
        assert_eq!(
            normalize_user_input_answers(json!({
                "choice":{"answers":["A"]},
                "multiple":{"answers":["A","B"]},
                "missing":{}
            })),
            json!({"choice":"A","multiple":["A","B"],"missing":[]})
        );
        assert_eq!(normalize_user_input_answers(Value::Null), json!({}));
    }

    #[test]
    fn token_usage_normalization_separates_active_and_lifetime_totals() {
        let params = json!({
            "threadId": "root-1",
            "turnId": "turn-1",
            "tokenUsage": {
                "last": {
                    "inputTokens": 1_000,
                    "cachedInputTokens": 500,
                    "outputTokens": 50,
                    "reasoningOutputTokens": 25,
                    "totalTokens": 1_075
                },
                "total": {
                    "inputTokens": 9_000,
                    "cachedInputTokens": 5_000,
                    "outputTokens": 800,
                    "reasoningOutputTokens": 400,
                    "totalTokens": 10_200
                },
                "modelContextWindow": 258_400
            }
        });

        let (turn_id, payload) =
            normalize_token_usage_notification(&params, "root-1").expect("valid root usage");
        assert_eq!(turn_id.as_deref(), Some("turn-1"));
        assert_eq!(
            payload,
            json!({
                "usage": {
                    "usedTokens": 1_075,
                    "totalProcessedTokens": 10_200,
                    "maxTokens": 258_400,
                    "inputTokens": 1_000,
                    "cachedInputTokens": 500,
                    "outputTokens": 50,
                    "reasoningOutputTokens": 25,
                    "lastUsedTokens": 1_075,
                    "lastInputTokens": 1_000,
                    "lastCachedInputTokens": 500,
                    "lastOutputTokens": 50,
                    "lastReasoningOutputTokens": 25,
                    "compactsAutomatically": true
                }
            })
        );
    }

    #[test]
    fn token_usage_normalization_rejects_non_root_or_invalid_active_usage() {
        let cases = [
            (
                "child thread",
                json!({
                    "threadId": "child-1",
                    "tokenUsage": { "last": { "totalTokens": 1 } }
                }),
            ),
            (
                "zero active usage",
                json!({
                    "threadId": "root-1",
                    "tokenUsage": { "last": { "totalTokens": 0 } }
                }),
            ),
            (
                "missing active usage",
                json!({
                    "threadId": "root-1",
                    "tokenUsage": { "last": {} }
                }),
            ),
            (
                "negative usage",
                json!({
                    "threadId": "root-1",
                    "tokenUsage": { "last": { "totalTokens": -1 } }
                }),
            ),
        ];

        for (name, params) in cases {
            assert!(
                normalize_token_usage_notification(&params, "root-1").is_none(),
                "{name} must be ignored"
            );
        }
    }

    #[test]
    fn token_usage_normalization_omits_malformed_optional_categories() {
        let params = json!({
            "threadId": "root-1",
            "turnId": "turn-1",
            "tokenUsage": {
                "last": {
                    "totalTokens": 12,
                    "inputTokens": "invalid",
                    "cachedInputTokens": -1,
                    "outputTokens": 0,
                    "reasoningOutputTokens": 2
                },
                "total": { "totalTokens": "invalid" },
                "modelContextWindow": 0
            }
        });

        let (_, payload) =
            normalize_token_usage_notification(&params, "root-1").expect("valid active usage");
        assert_eq!(
            payload,
            json!({
                "usage": {
                    "usedTokens": 12,
                    "outputTokens": 0,
                    "reasoningOutputTokens": 2,
                    "lastUsedTokens": 12,
                    "lastOutputTokens": 0,
                    "lastReasoningOutputTokens": 2,
                    "compactsAutomatically": true
                }
            })
        );
    }
}
