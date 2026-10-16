use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256, Sha512};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::activity::{
    ACTIVITY_DETAIL_MAX_LENGTH, ACTIVITY_ID_MAX_LENGTH, ACTIVITY_LABEL_MAX_LENGTH,
    ACTIVITY_SUMMARY_MAX_LENGTH, ActivityActorSummary, ActivityCapabilities, ActivityEntry,
    ActivityEntryKind, ActivityEntryTone, ActivityHistoryRecovery, ActivityLifecycle,
    ActivityObservationState, ActivityRecordKind, ActivityWorkItemSummary,
    ProviderActivityControlUpdate, ProviderActivityMutation, ProviderActivityNativeTarget,
};

use super::model::{
    ReconciliationBackgroundTerminal, ReconciliationThread, ReconciliationThreadItem,
    ReconciliationThreadStatus, decode_thread_list_response, decode_thread_read_response,
};
use super::protocol::is_missing_method_error;

pub(crate) const MAX_TRACKED_ACTORS: usize = 256;
pub(crate) const MAX_TRACKED_WORK_ITEMS: usize = 128;
const MAX_SEEN_EVENTS: usize = 2_048;
const MAX_PENDING_DELTAS: usize = 256;
const MAX_MUTATIONS_PER_OUTPUT: usize = 256;
const MAX_RETAINED_KEY_BYTES: usize = 256;
const MAX_INLINE_EVENT_KEY_BYTES: usize = 192;
const MAX_RECONCILED_DESCENDANTS: usize = 50;
const MAX_RECONCILED_TURNS: usize = 20;
const MAX_RECONCILED_ENTRIES: usize = 200;
const MAX_DETAIL_BASELINE_IDENTITIES: usize =
    MAX_RECONCILED_DESCENDANTS * (MAX_RECONCILED_ENTRIES + MAX_RECONCILED_TURNS);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodexActivityStateCounts {
    pub actors: usize,
    pub work_items: usize,
    pub seen_events: usize,
    pub pending_deltas: usize,
}

#[derive(Debug, Default)]
pub struct CodexActivityOutput {
    pub mutations: Vec<ProviderActivityMutation>,
    pub(crate) controls: Vec<ProviderActivityControlUpdate>,
    pub request_reconciliation: bool,
    pub hinted_descendant_ids: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct CodexDescendantReconciliation {
    pub output: CodexActivityOutput,
    pub threads_to_read: Vec<String>,
    pub accepted_thread_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackgroundSnapshotAuthority {
    Partial,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActorReopenAuthority<'a> {
    None,
    ProviderTimestamp(&'a str),
}

struct ValidatedSubAgentHint<'a> {
    native_thread_id: &'a str,
    fallback_name: String,
    status: ActivityLifecycle,
}

impl CodexActivityOutput {
    fn push(&mut self, mutation: ProviderActivityMutation) {
        if self.mutations.len() < MAX_MUTATIONS_PER_OUTPUT {
            self.mutations.push(mutation);
        }
    }

    fn push_hint(&mut self, native_thread_id: &str) {
        if self.hinted_descendant_ids.len() < MAX_RECONCILED_DESCENDANTS
            && !self
                .hinted_descendant_ids
                .iter()
                .any(|existing| existing == native_thread_id)
        {
            self.hinted_descendant_ids.push(native_thread_id.to_owned());
        }
    }

    fn push_control(&mut self, control: ProviderActivityControlUpdate) {
        if self.controls.len() < MAX_MUTATIONS_PER_OUTPUT {
            self.controls.push(control);
        }
    }

    fn push_actor_update(&mut self, update: ActivityActorUpdate) {
        self.push(ProviderActivityMutation::UpsertActor(update.actor));
        if let Some(control) = update.control {
            self.push_control(control);
        }
    }

    #[allow(dead_code, reason = "used by the pending runtime hint integration")]
    fn merge(&mut self, other: Self) {
        for mutation in other.mutations {
            self.push(mutation);
        }
        for native_thread_id in other.hinted_descendant_ids {
            self.push_hint(&native_thread_id);
        }
        for control in other.controls {
            self.push_control(control);
        }
        self.request_reconciliation |= other.request_reconciliation;
    }
}

#[derive(Clone, Debug)]
struct ActivityActorState {
    canonical_id: String,
    parent_actor_id: Option<String>,
    name: String,
    role: Option<String>,
    status: ActivityLifecycle,
    summary: Option<String>,
    started_at: String,
    updated_at: String,
    terminal_at: Option<String>,
    active_turn_id: Option<String>,
}

struct ActivityActorUpdate {
    actor: ActivityActorSummary,
    control: Option<ProviderActivityControlUpdate>,
}

impl ActivityActorState {
    fn to_summary(&self) -> Option<ActivityActorSummary> {
        ActivityActorSummary::try_new(
            self.canonical_id.clone(),
            self.parent_actor_id.as_deref(),
            self.name.clone(),
            self.role.as_deref(),
            Some("codex"),
            self.status,
            self.summary.as_deref(),
            self.started_at.clone(),
            self.updated_at.clone(),
            self.terminal_at.as_deref(),
        )
        .ok()
    }
}

#[derive(Clone, Debug)]
struct ActivityWorkItemState {
    canonical_id: String,
    owner_actor_id: Option<String>,
    name: String,
    work_kind: String,
    status: ActivityLifecycle,
    summary: Option<String>,
    started_at: String,
    updated_at: String,
    terminal_at: Option<String>,
}

impl ActivityWorkItemState {
    fn to_summary(&self) -> Option<ActivityWorkItemSummary> {
        ActivityWorkItemSummary::try_new(
            self.canonical_id.clone(),
            self.owner_actor_id.as_deref(),
            self.name.clone(),
            self.work_kind.clone(),
            None,
            None,
            self.status,
            self.summary.as_deref(),
            self.started_at.clone(),
            self.updated_at.clone(),
            self.terminal_at.as_deref(),
        )
        .ok()
    }
}

#[derive(Debug)]
struct PendingTextDelta {
    text: String,
}

#[derive(Debug, Default)]
struct DetailIdentityBaseline {
    active: bool,
    saturated: bool,
    identities: HashSet<String>,
}

#[derive(Debug, Default)]
struct BoundedSeenSet {
    order: VecDeque<String>,
    values: HashSet<String>,
}

impl BoundedSeenSet {
    fn insert(&mut self, key: String) -> bool {
        let key = retained_key("seen", &key);
        if self.values.contains(&key) {
            return false;
        }
        if self.order.len() == MAX_SEEN_EVENTS
            && let Some(evicted) = self.order.pop_front()
        {
            self.values.remove(&evicted);
        }
        self.values.insert(key.clone());
        self.order.push_back(key);
        true
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn contains(&self, key: &str) -> bool {
        self.values.contains(&retained_key("seen", key))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CanonicalDigest {
    primary: [u8; 32],
    secondary: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct CanonicalIdGenerator {
    digest: fn(&[u8]) -> CanonicalDigest,
}

impl Default for CanonicalIdGenerator {
    fn default() -> Self {
        Self {
            digest: canonical_digest,
        }
    }
}

impl CanonicalIdGenerator {
    #[cfg(test)]
    fn with_digest(digest: fn(&[u8]) -> CanonicalDigest) -> Self {
        Self { digest }
    }

    fn resolve(&self, prefix: &str, native: &str) -> Option<String> {
        if canonical_id_can_inline(prefix, native) {
            return Some(format!("{prefix}{native}"));
        }
        let digest = (self.digest)(native.as_bytes());
        Some(format!(
            "{prefix}h{}-{}",
            hex_digest(&digest.primary),
            hex_digest(&digest.secondary)
        ))
    }
}

#[derive(Debug)]
pub(crate) struct CodexActivityTracker {
    root_thread_id: Option<String>,
    actors_by_thread: HashMap<String, ActivityActorState>,
    provisional_actor_keys: HashSet<String>,
    work_items_by_native_id: HashMap<String, ActivityWorkItemState>,
    seen_native_events: BoundedSeenSet,
    completed_delta_streams: BoundedSeenSet,
    completed_commentary_semantics: BoundedSeenSet,
    pending_deltas: HashMap<String, PendingTextDelta>,
    reconciled_thread_versions: HashMap<String, u64>,
    detail_baseline: DetailIdentityBaseline,
    canonical_ids: CanonicalIdGenerator,
    sub_agent_hint_projection: SubAgentHintProjection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubAgentHintProjection {
    StructuredChat,
    ListTriggerOnly,
}

impl CodexActivityTracker {
    #[must_use]
    pub fn new(root_thread_id: Option<&str>) -> Self {
        Self::new_with_hint_projection(root_thread_id, SubAgentHintProjection::StructuredChat)
    }

    #[must_use]
    pub(crate) fn new_for_terminal_observation(root_thread_id: Option<&str>) -> Self {
        Self::new_with_hint_projection(root_thread_id, SubAgentHintProjection::ListTriggerOnly)
    }

    fn new_with_hint_projection(
        root_thread_id: Option<&str>,
        sub_agent_hint_projection: SubAgentHintProjection,
    ) -> Self {
        Self {
            root_thread_id: root_thread_id.map(thread_key),
            actors_by_thread: HashMap::new(),
            provisional_actor_keys: HashSet::new(),
            work_items_by_native_id: HashMap::new(),
            seen_native_events: BoundedSeenSet::default(),
            completed_delta_streams: BoundedSeenSet::default(),
            completed_commentary_semantics: BoundedSeenSet::default(),
            pending_deltas: HashMap::new(),
            reconciled_thread_versions: HashMap::new(),
            detail_baseline: DetailIdentityBaseline::default(),
            canonical_ids: CanonicalIdGenerator::default(),
            sub_agent_hint_projection,
        }
    }

    pub(crate) fn begin_detail_baseline(&mut self) {
        self.detail_baseline = DetailIdentityBaseline {
            active: true,
            saturated: false,
            identities: HashSet::new(),
        };
    }

    pub(crate) fn finish_detail_baseline(&mut self) {
        self.detail_baseline.active = false;
    }

    fn suppress_detail_identity(&mut self, identity: String) -> bool {
        let identity = retained_key("detail-baseline", &identity);
        if self.detail_baseline.active {
            if self.detail_baseline.identities.len() < MAX_DETAIL_BASELINE_IDENTITIES {
                self.detail_baseline.identities.insert(identity);
            } else {
                self.detail_baseline.saturated = true;
            }
            return true;
        }
        self.detail_baseline.saturated || self.detail_baseline.identities.contains(&identity)
    }

    pub fn seed_actor(&mut self, native_thread_id: &str) {
        let native_key = thread_key(native_thread_id);
        if native_thread_id.is_empty()
            || self.actors_by_thread.contains_key(&native_key)
            || self.actors_by_thread.len() >= MAX_TRACKED_ACTORS
        {
            return;
        }
        let Some(canonical_id) = self
            .canonical_ids
            .resolve("codex:thread:", native_thread_id)
        else {
            return;
        };
        self.actors_by_thread.insert(
            native_key.clone(),
            ActivityActorState {
                canonical_id,
                parent_actor_id: None,
                name: bounded_actor_name(native_thread_id),
                role: None,
                status: ActivityLifecycle::Unknown,
                summary: None,
                started_at: unix_millis_to_timestamp(0),
                updated_at: unix_millis_to_timestamp(0),
                terminal_at: None,
                active_turn_id: None,
            },
        );
        self.provisional_actor_keys.remove(&native_key);
    }

    pub fn set_root_thread_id(&mut self, native_thread_id: &str) {
        let root_thread_id = thread_key(native_thread_id);
        if self.root_thread_id.as_deref() == Some(root_thread_id.as_str()) {
            return;
        }
        self.root_thread_id = Some(root_thread_id);
        self.actors_by_thread.clear();
        self.provisional_actor_keys.clear();
        self.work_items_by_native_id.clear();
        self.seen_native_events = BoundedSeenSet::default();
        self.completed_delta_streams = BoundedSeenSet::default();
        self.completed_commentary_semantics = BoundedSeenSet::default();
        self.pending_deltas.clear();
        self.reconciled_thread_versions.clear();
        self.detail_baseline = DetailIdentityBaseline::default();
    }

    #[must_use]
    pub fn is_root_thread(&self, native_thread_id: &str) -> bool {
        self.root_thread_id.as_deref() == Some(thread_key(native_thread_id).as_str())
    }

    #[must_use]
    pub fn is_verified_child(&self, native_thread_id: &str) -> bool {
        let native_key = thread_key(native_thread_id);
        self.actors_by_thread.contains_key(&native_key)
            && !self.provisional_actor_keys.contains(&native_key)
    }

    pub(crate) fn actor_control_id(&self, native_thread_id: &str) -> Option<&str> {
        let native_key = thread_key(native_thread_id);
        (!self.provisional_actor_keys.contains(&native_key))
            .then(|| self.actors_by_thread.get(&native_key))
            .flatten()
            .map(|actor| actor.canonical_id.as_str())
    }

    #[must_use]
    pub(crate) fn is_current_target(&self, native_thread_id: &str, turn_id: &str) -> bool {
        usable_native_id(native_thread_id)
            && usable_native_id(turn_id)
            && !self.is_root_thread(native_thread_id)
            && self.is_verified_child(native_thread_id)
            && self
                .actors_by_thread
                .get(&thread_key(native_thread_id))
                .is_some_and(|actor| {
                    !actor.status.is_terminal() && actor.active_turn_id.as_deref() == Some(turn_id)
                })
    }

    fn update_active_turn_control(
        &mut self,
        native_thread_id: &str,
        turn_id: Option<&str>,
        active: bool,
    ) -> Option<ProviderActivityControlUpdate> {
        if !usable_native_id(native_thread_id)
            || self.is_root_thread(native_thread_id)
            || !self.is_verified_child(native_thread_id)
        {
            return None;
        }
        let native_key = thread_key(native_thread_id);
        let actor = self.actors_by_thread.get_mut(&native_key)?;
        if active {
            let turn_id = turn_id.filter(|turn_id| usable_native_id(turn_id))?;
            if actor.status.is_terminal() {
                return None;
            }
            if actor.active_turn_id.as_deref() == Some(turn_id) {
                return None;
            }
            if actor.active_turn_id.is_some() {
                actor.active_turn_id = None;
                return Some(ProviderActivityControlUpdate::ActorTarget {
                    actor_id: actor.canonical_id.clone(),
                    target: None,
                });
            }
            actor.active_turn_id = Some(turn_id.to_owned());
            return Some(ProviderActivityControlUpdate::ActorTarget {
                actor_id: actor.canonical_id.clone(),
                target: Some(ProviderActivityNativeTarget::codex_turn(
                    native_thread_id.to_owned(),
                    turn_id.to_owned(),
                )),
            });
        }
        if actor.active_turn_id.as_deref() != turn_id {
            return None;
        }
        actor.active_turn_id = None;
        Some(ProviderActivityControlUpdate::ActorTarget {
            actor_id: actor.canonical_id.clone(),
            target: None,
        })
    }

    fn clear_active_turn_control(
        &mut self,
        native_thread_id: &str,
    ) -> Option<ProviderActivityControlUpdate> {
        let actor = self
            .actors_by_thread
            .get_mut(&thread_key(native_thread_id))?;
        actor.active_turn_id.take()?;
        Some(ProviderActivityControlUpdate::ActorTarget {
            actor_id: actor.canonical_id.clone(),
            target: None,
        })
    }

    #[must_use]
    pub fn state_counts(&self) -> CodexActivityStateCounts {
        CodexActivityStateCounts {
            actors: self.actors_by_thread.len(),
            work_items: self.work_items_by_native_id.len(),
            seen_events: self.seen_native_events.len(),
            pending_deltas: self.pending_deltas.len(),
        }
    }

    #[must_use]
    pub fn map_status(value: &str) -> ActivityLifecycle {
        match value {
            "pending" | "pendingInit" | "starting" => ActivityLifecycle::Starting,
            "inProgress" | "running" | "active" => ActivityLifecycle::Running,
            "waiting" | "idle" => ActivityLifecycle::Waiting,
            "completed" => ActivityLifecycle::Completed,
            "failed" | "errored" | "systemError" => ActivityLifecycle::Failed,
            "cancelled" | "declined" => ActivityLifecycle::Cancelled,
            "interrupted" | "shutdown" => ActivityLifecycle::Interrupted,
            _ => ActivityLifecycle::Unknown,
        }
    }

    pub fn handle_envelope(
        &mut self,
        envelope: &Value,
        receive_sequence: u128,
    ) -> CodexActivityOutput {
        if let Some(method) = envelope.get("method").and_then(Value::as_str) {
            let Some(params) = envelope.get("params") else {
                return CodexActivityOutput::default();
            };
            let emitted_at_ms = envelope
                .get("emittedAtMs")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            return self.handle_notification(method, params, emitted_at_ms, receive_sequence);
        }
        self.handle_response(envelope)
    }

    pub fn handle_notification(
        &mut self,
        method: &str,
        params: &Value,
        emitted_at_ms: u64,
        receive_sequence: u128,
    ) -> CodexActivityOutput {
        match method {
            "item/agentMessage/delta" => self.handle_text_delta(method, params, receive_sequence),
            "item/reasoning/summaryTextDelta" => {
                self.handle_text_delta(method, params, receive_sequence)
            }
            "item/reasoning/summaryPartAdded" => CodexActivityOutput::default(),
            "item/started" | "item/completed" => {
                self.handle_item_notification(method, params, emitted_at_ms)
            }
            "turn/started" | "turn/completed" => {
                self.handle_turn_notification(method, params, emitted_at_ms, true)
            }
            "thread/status/changed" => self.handle_thread_status(params, emitted_at_ms),
            _ => CodexActivityOutput::default(),
        }
    }

    fn handle_response(&mut self, envelope: &Value) -> CodexActivityOutput {
        let Some(request_id) = envelope.get("id").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let recovery_method = if request_id.starts_with("recovery-list") {
            "thread/list"
        } else if request_id.starts_with("recovery-read") {
            "thread/read"
        } else {
            ""
        };
        if let Some(error) = envelope.get("error")
            && request_id.starts_with("recovery-")
            && let Some(code) = error.get("code").and_then(Value::as_i64)
            && is_missing_method_error(
                code,
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                recovery_method,
            )
        {
            let mut output = CodexActivityOutput::default();
            output.push(ProviderActivityMutation::SetScope {
                capabilities: ActivityCapabilities {
                    actors: true,
                    attributed_activity: true,
                    background_work: false,
                    history_recovery: ActivityHistoryRecovery::None,
                    terminal_observation: false,
                    targeted_actor_cancellation: self.sub_agent_hint_projection
                        == SubAgentHintProjection::StructuredChat,
                },
                observation_state: ActivityObservationState::Live,
            });
            return output;
        }
        let Some(result) = envelope.get("result") else {
            return CodexActivityOutput::default();
        };
        if request_id.starts_with("recovery-list") {
            return self.handle_thread_list(result);
        }
        if request_id.starts_with("recovery-read") {
            return self.handle_thread_read(result);
        }
        CodexActivityOutput::default()
    }

    fn handle_item_notification(
        &mut self,
        method: &str,
        params: &Value,
        emitted_at_ms: u64,
    ) -> CodexActivityOutput {
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let Some(turn_id) = params.get("turnId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let Some(item) = params.get("item").and_then(Value::as_object) else {
            return CodexActivityOutput::default();
        };
        let Some(item_id) = item.get("id").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        if item_type == "collabAgentToolCall" {
            return self.handle_collaboration(method, params, item, emitted_at_ms);
        }
        if item_type == "subAgentActivity" {
            return self.handle_sub_agent_activity(
                thread_id,
                item.get("agentThreadId").and_then(Value::as_str),
                item.get("agentPath").and_then(Value::as_str),
                item.get("kind").and_then(Value::as_str),
                item_timestamp_ms(params, emitted_at_ms),
            );
        }
        if item_type == "commandExecution" && self.is_root_thread(thread_id) {
            return CodexActivityOutput {
                request_reconciliation: true,
                ..CodexActivityOutput::default()
            };
        }
        if matches!(
            item_type,
            "dynamicToolCall" | "mcpToolCall" | "commandExecution" | "agentMessage" | "reasoning"
        ) && self.suppress_detail_identity(item_detail_identity(thread_id, turn_id, item_id))
        {
            return CodexActivityOutput::default();
        }
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .map(Self::map_status)
            .unwrap_or_else(|| {
                if method == "item/completed" {
                    ActivityLifecycle::Completed
                } else {
                    ActivityLifecycle::Running
                }
            });
        let event_key = event_fallback_key(method, thread_id, turn_id, item_id, status.as_str());
        if !self.actors_by_thread.contains_key(&thread_key(thread_id)) {
            return CodexActivityOutput::default();
        }
        if !self.seen_native_events.insert(event_key.clone()) {
            return CodexActivityOutput::default();
        }
        let timestamp_ms = item_timestamp_ms(params, emitted_at_ms);
        let timestamp = unix_millis_to_timestamp(timestamp_ms);
        match item_type {
            "dynamicToolCall" | "mcpToolCall" => {
                self.tool_entry(event_key, thread_id, item, method, status, timestamp)
            }
            "commandExecution" => {
                self.command_entry(event_key, thread_id, item, method, status, timestamp)
            }
            "agentMessage" if method == "item/completed" => {
                self.flush_completed_text(thread_id, turn_id, item_id, item, timestamp)
            }
            "reasoning" if method == "item/completed" => self
                .flush_completed_reasoning(event_key, thread_id, turn_id, item_id, item, timestamp),
            _ => CodexActivityOutput::default(),
        }
    }

    fn handle_sub_agent_activity(
        &mut self,
        owning_thread_id: &str,
        native_thread_id: Option<&str>,
        agent_path: Option<&str>,
        kind: Option<&str>,
        timestamp_ms: u64,
    ) -> CodexActivityOutput {
        let Some(hint) =
            self.validate_sub_agent_hint(owning_thread_id, native_thread_id, agent_path, kind)
        else {
            return CodexActivityOutput::default();
        };
        if self.sub_agent_hint_projection == SubAgentHintProjection::ListTriggerOnly {
            return CodexActivityOutput {
                request_reconciliation: true,
                ..CodexActivityOutput::default()
            };
        }
        let parent_actor_id = self
            .actors_by_thread
            .get(&thread_key(owning_thread_id))
            .map(|actor| actor.canonical_id.clone())
            .filter(|_| !self.is_root_thread(owning_thread_id));
        let provider_timestamp = checked_provider_timestamp_millis(timestamp_ms);
        let timestamp = provider_timestamp
            .clone()
            .unwrap_or_else(|| unix_millis_to_timestamp(timestamp_ms));
        let native_key = thread_key(hint.native_thread_id);
        let existed = self.actors_by_thread.contains_key(&native_key);
        let reopen_authority = if hint.status.is_terminal() {
            ActorReopenAuthority::None
        } else {
            provider_timestamp.as_deref().map_or(
                ActorReopenAuthority::None,
                ActorReopenAuthority::ProviderTimestamp,
            )
        };
        let Some(actor_update) = self.upsert_actor_state(
            hint.native_thread_id,
            parent_actor_id.as_deref(),
            Some(&hint.fallback_name),
            None,
            hint.status,
            None,
            &timestamp,
            false,
            reopen_authority,
        ) else {
            return CodexActivityOutput::default();
        };
        if !existed {
            self.provisional_actor_keys.insert(native_key);
        }
        let mut output = CodexActivityOutput {
            request_reconciliation: true,
            ..CodexActivityOutput::default()
        };
        output.push_actor_update(actor_update);
        output.push_hint(hint.native_thread_id);
        output
    }

    fn validate_sub_agent_hint<'a>(
        &self,
        owning_thread_id: &str,
        native_thread_id: Option<&'a str>,
        agent_path: Option<&str>,
        kind: Option<&str>,
    ) -> Option<ValidatedSubAgentHint<'a>> {
        if !self.is_root_thread(owning_thread_id) && !self.is_verified_child(owning_thread_id) {
            return None;
        }
        let native_thread_id = native_thread_id.filter(|id| usable_native_id(id))?;
        if self.is_root_thread(native_thread_id)
            || self.receiver_is_sender_or_ancestor(native_thread_id, owning_thread_id)
        {
            return None;
        }
        let fallback_name = agent_path?
            .rsplit('/')
            .map(str::trim)
            .find(|segment| !segment.is_empty())?;
        let status = match kind? {
            "started" | "interacted" => ActivityLifecycle::Running,
            "interrupted" => ActivityLifecycle::Interrupted,
            _ => return None,
        };
        Some(ValidatedSubAgentHint {
            native_thread_id,
            fallback_name: bounded_label(fallback_name),
            status,
        })
    }

    fn handle_collaboration(
        &mut self,
        method: &str,
        params: &Value,
        item: &Map<String, Value>,
        emitted_at_ms: u64,
    ) -> CodexActivityOutput {
        let Some(sender_thread_id) = item.get("senderThreadId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let Some(receiver_thread_ids) = item.get("receiverThreadIds").and_then(Value::as_array)
        else {
            return CodexActivityOutput::default();
        };
        let Some(item_id) = item.get("id").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let Some(tool) = item.get("tool").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let thread_id = params
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or(sender_thread_id);
        let turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let in_scope = |candidate: &str| {
            let key = thread_key(candidate);
            self.root_thread_id.as_deref() == Some(key.as_str())
                || self.actors_by_thread.contains_key(&key)
        };
        if !in_scope(thread_id) || !in_scope(sender_thread_id) {
            return CodexActivityOutput::default();
        }
        let root_thread_id = self.root_thread_id.clone();
        let accepted_receivers = receiver_thread_ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|receiver| root_thread_id.as_deref() != Some(thread_key(receiver).as_str()))
            .filter(|receiver| !self.receiver_is_sender_or_ancestor(receiver, sender_thread_id))
            .take(MAX_MUTATIONS_PER_OUTPUT)
            .collect::<Vec<_>>();
        if accepted_receivers.is_empty() {
            return CodexActivityOutput::default();
        }
        let status_key = item.get("status").and_then(Value::as_str).unwrap_or(method);
        let event_key = event_fallback_key(method, thread_id, turn_id, item_id, status_key);
        if !self.seen_native_events.insert(event_key) {
            return CodexActivityOutput::default();
        }

        let timestamp_ms = item_timestamp_ms(params, emitted_at_ms);
        let provider_timestamp = checked_provider_timestamp_millis(timestamp_ms);
        let timestamp = provider_timestamp
            .clone()
            .unwrap_or_else(|| unix_millis_to_timestamp(timestamp_ms));
        let reopen_authority = if tool == "resumeAgent" {
            provider_timestamp.as_deref().map_or(
                ActorReopenAuthority::None,
                ActorReopenAuthority::ProviderTimestamp,
            )
        } else {
            ActorReopenAuthority::None
        };
        let agents_states = item.get("agentsStates").and_then(Value::as_object);
        let parent_actor_id = self
            .actors_by_thread
            .get(&thread_key(sender_thread_id))
            .map(|actor| actor.canonical_id.clone());
        let mut output = CodexActivityOutput {
            request_reconciliation: matches!(
                tool,
                "spawnAgent" | "resumeAgent" | "wait" | "closeAgent"
            ),
            ..CodexActivityOutput::default()
        };
        for receiver in accepted_receivers {
            let native_state = agents_states
                .and_then(|states| states.get(receiver))
                .and_then(Value::as_object);
            let native_status = native_state
                .and_then(|state| state.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("starting");
            let status = Self::map_status(native_status);
            let summary = native_state
                .and_then(|state| state.get("message"))
                .and_then(Value::as_str)
                .map(bounded_summary);
            if let Some(actor_update) = self.upsert_actor_state(
                receiver,
                parent_actor_id.as_deref(),
                None,
                None,
                status,
                summary,
                &timestamp,
                false,
                reopen_authority,
            ) {
                output.push_actor_update(actor_update);
            }
        }
        output
    }

    fn receiver_is_sender_or_ancestor(&self, receiver: &str, sender: &str) -> bool {
        let receiver_key = thread_key(receiver);
        let sender_key = thread_key(sender);
        if receiver_key == sender_key {
            return true;
        }
        let Some(receiver_id) = self
            .actors_by_thread
            .get(&receiver_key)
            .map(|actor| actor.canonical_id.as_str())
        else {
            return false;
        };
        let mut current = self.actors_by_thread.get(&sender_key);
        let mut visited = HashSet::new();
        while let Some(actor) = current {
            if actor.canonical_id == receiver_id {
                return true;
            }
            if !visited.insert(actor.canonical_id.as_str()) {
                return true;
            }
            current = actor.parent_actor_id.as_deref().and_then(|parent_id| {
                self.actors_by_thread
                    .values()
                    .find(|candidate| candidate.canonical_id == parent_id)
            });
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_actor_state(
        &mut self,
        native_thread_id: &str,
        parent_actor_id: Option<&str>,
        name: Option<&str>,
        role: Option<&str>,
        status: ActivityLifecycle,
        summary: Option<String>,
        timestamp: &str,
        authoritative: bool,
        reopen_authority: ActorReopenAuthority<'_>,
    ) -> Option<ActivityActorUpdate> {
        if native_thread_id.is_empty() {
            return None;
        }
        let native_key = thread_key(native_thread_id);
        if let Some(existing) = self.actors_by_thread.get_mut(&native_key) {
            if existing.status.is_terminal() && !status.is_terminal() {
                let ActorReopenAuthority::ProviderTimestamp(provider_timestamp) = reopen_authority
                else {
                    return None;
                };
                if !activity_timestamp_is_not_older(provider_timestamp, &existing.updated_at) {
                    return None;
                }
            }
            if existing.status.is_terminal()
                && status.is_terminal()
                && !authoritative
                && existing.status != status
            {
                return None;
            }
            if existing.status.is_terminal()
                && status.is_terminal()
                && authoritative
                && !activity_timestamp_is_not_older(timestamp, &existing.updated_at)
            {
                return None;
            }
            if status.is_terminal()
                && existing.active_turn_id.is_some()
                && !activity_timestamp_is_not_older(timestamp, &existing.updated_at)
            {
                return None;
            }
            let next_summary = summary.or_else(|| existing.summary.clone());
            let next_parent = if authoritative {
                parent_actor_id
                    .map(str::to_owned)
                    .or_else(|| existing.parent_actor_id.clone())
            } else {
                existing.parent_actor_id.clone()
            };
            let next_name = name
                .map(bounded_label)
                .unwrap_or_else(|| existing.name.clone());
            let next_role = role.map(bounded_label).or_else(|| existing.role.clone());
            let updated_at = latest_activity_timestamp([
                existing.started_at.as_str(),
                existing.updated_at.as_str(),
                timestamp,
            ])?;
            let clear_active_target = status.is_terminal() && existing.active_turn_id.is_some();
            if existing.status == status
                && existing.summary == next_summary
                && existing.parent_actor_id == next_parent
                && existing.name == next_name
                && existing.role == next_role
                && !clear_active_target
            {
                if !status.is_terminal() {
                    existing.updated_at = updated_at;
                }
                return None;
            }
            let mut candidate = existing.clone();
            candidate.parent_actor_id = next_parent;
            candidate.name = next_name;
            candidate.role = next_role;
            candidate.status = status;
            candidate.summary = next_summary;
            candidate.updated_at = updated_at.clone();
            candidate.terminal_at = status.is_terminal().then_some(updated_at);
            if status.is_terminal() {
                candidate.active_turn_id = None;
            }
            let actor = candidate.to_summary()?;
            candidate.started_at.clone_from(&actor.started_at);
            candidate.updated_at.clone_from(&actor.updated_at);
            candidate.terminal_at.clone_from(&actor.terminal_at);
            *existing = candidate;
            let control = clear_active_target.then(|| ProviderActivityControlUpdate::ActorTarget {
                actor_id: actor.id.clone(),
                target: None,
            });
            return Some(ActivityActorUpdate { actor, control });
        }
        if self.actors_by_thread.len() >= MAX_TRACKED_ACTORS {
            return None;
        }
        let canonical_id = self
            .canonical_ids
            .resolve("codex:thread:", native_thread_id)?;
        let actor = ActivityActorState {
            canonical_id,
            parent_actor_id: parent_actor_id.map(str::to_owned),
            name: name
                .map(bounded_label)
                .unwrap_or_else(|| bounded_actor_name(native_thread_id)),
            role: role.map(bounded_label),
            status,
            summary,
            started_at: timestamp.to_owned(),
            updated_at: timestamp.to_owned(),
            terminal_at: status.is_terminal().then(|| timestamp.to_owned()),
            active_turn_id: None,
        };
        let summary = actor.to_summary()?;
        self.actors_by_thread.insert(native_key, actor);
        Some(ActivityActorUpdate {
            actor: summary,
            control: None,
        })
    }

    fn materialize_provisional_actor(
        &mut self,
        native_key: &str,
        parent_actor_id: Option<&str>,
        name: Option<&str>,
        role: Option<&str>,
        created_at_ms: u64,
        updated_at: &str,
    ) -> Option<ActivityActorSummary> {
        let existing = self.actors_by_thread.get_mut(native_key)?;
        let mut candidate = existing.clone();
        candidate.parent_actor_id = parent_actor_id.map(str::to_owned);
        if let Some(name) = name {
            candidate.name = bounded_label(name);
        }
        if let Some(role) = role {
            candidate.role = Some(bounded_label(role));
        }
        candidate.started_at = unix_millis_to_timestamp(created_at_ms);
        candidate.updated_at = latest_activity_timestamp([
            candidate.started_at.as_str(),
            existing.updated_at.as_str(),
            updated_at,
        ])?;
        candidate.terminal_at = candidate
            .status
            .is_terminal()
            .then(|| candidate.updated_at.clone());
        let actor = candidate.to_summary()?;
        candidate.started_at.clone_from(&actor.started_at);
        candidate.updated_at.clone_from(&actor.updated_at);
        candidate.terminal_at.clone_from(&actor.terminal_at);
        *existing = candidate;
        Some(actor)
    }

    fn handle_text_delta(
        &mut self,
        method: &str,
        params: &Value,
        receive_sequence: u128,
    ) -> CodexActivityOutput {
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        if !self.actors_by_thread.contains_key(&thread_key(thread_id)) {
            return CodexActivityOutput::default();
        }
        let Some(turn_id) = params.get("turnId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let item_id = params
            .get("itemId")
            .or_else(|| params.get("item_id"))
            .and_then(Value::as_str)
            .unwrap_or("summary");
        if self.suppress_detail_identity(item_detail_identity(thread_id, turn_id, item_id)) {
            return CodexActivityOutput::default();
        }
        let delta = params
            .get("delta")
            .or_else(|| params.get("text"))
            .and_then(Value::as_str);
        let Some(delta) = delta else {
            return CodexActivityOutput::default();
        };
        let buffer_key = delta_stream_key(thread_id, turn_id, item_id);
        if self.completed_delta_streams.contains(&buffer_key) {
            return CodexActivityOutput::default();
        }
        let summary_index = params.get("summaryIndex").and_then(Value::as_u64);
        let replay_key = delta_replay_key(method, &buffer_key, receive_sequence, summary_index);
        if !self.seen_native_events.insert(replay_key) {
            return CodexActivityOutput::default();
        }
        if !self.pending_deltas.contains_key(&buffer_key)
            && self.pending_deltas.len() >= MAX_PENDING_DELTAS
        {
            return CodexActivityOutput::default();
        }
        let pending = self
            .pending_deltas
            .entry(buffer_key)
            .or_insert_with(|| PendingTextDelta {
                text: String::new(),
            });
        append_utf8_bounded(&mut pending.text, delta, ACTIVITY_DETAIL_MAX_LENGTH);
        CodexActivityOutput::default()
    }

    fn flush_completed_text(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        item: &Map<String, Value>,
        timestamp: String,
    ) -> CodexActivityOutput {
        let buffer_key = delta_stream_key(thread_id, turn_id, item_id);
        self.completed_delta_streams.insert(buffer_key.clone());
        let pending = self.pending_deltas.remove(&buffer_key);
        let detail = item
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(bounded_detail)
            .or_else(|| {
                pending
                    .filter(|pending| !pending.text.is_empty())
                    .map(|pending| pending.text)
            });
        let Some(detail) = detail else {
            return CodexActivityOutput::default();
        };
        let semantic_identity =
            length_prefixed_key("commentary", &[thread_id, turn_id, detail.as_str()]);
        let semantic_digest = canonical_digest(semantic_identity.as_bytes());
        let semantic_key = format!(
            "commentary:{}-{}",
            hex_digest(&semantic_digest.primary),
            hex_digest(&semantic_digest.secondary)
        );
        if !self
            .completed_commentary_semantics
            .insert(semantic_key.clone())
        {
            return CodexActivityOutput::default();
        }
        let mut output = CodexActivityOutput::default();
        if let Some(entry) = make_entry(
            &mut self.canonical_ids,
            semantic_key,
            ActivityRecordKind::Actor,
            actor_id(&self.actors_by_thread, thread_id),
            ActivityEntryKind::Commentary,
            "Commentary",
            Some(&detail),
            ActivityEntryTone::Info,
            timestamp,
        ) {
            output.push(ProviderActivityMutation::AppendEntry(entry));
        }
        output
    }

    fn flush_completed_reasoning(
        &mut self,
        entry_key: String,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
        item: &Map<String, Value>,
        timestamp: String,
    ) -> CodexActivityOutput {
        let buffer_key = delta_stream_key(thread_id, turn_id, item_id);
        self.completed_delta_streams.insert(buffer_key.clone());
        let pending = self.pending_deltas.remove(&buffer_key);
        let detail = item
            .get("summary")
            .and_then(Value::as_array)
            .and_then(|parts| reasoning_summary_detail(parts))
            .or_else(|| {
                pending
                    .filter(|pending| !pending.text.is_empty())
                    .map(|pending| pending.text)
            });
        let Some(detail) = detail else {
            return CodexActivityOutput::default();
        };
        let mut output = CodexActivityOutput::default();
        if let Some(entry) = make_entry(
            &mut self.canonical_ids,
            entry_key,
            ActivityRecordKind::Actor,
            actor_id(&self.actors_by_thread, thread_id),
            ActivityEntryKind::Commentary,
            "Reasoning summary",
            Some(&detail),
            ActivityEntryTone::Info,
            timestamp,
        ) {
            output.push(ProviderActivityMutation::AppendEntry(entry));
        }
        output
    }

    fn tool_entry(
        &mut self,
        entry_key: String,
        thread_id: &str,
        item: &Map<String, Value>,
        method: &str,
        status: ActivityLifecycle,
        timestamp: String,
    ) -> CodexActivityOutput {
        let tool = item
            .get("tool")
            .and_then(Value::as_str)
            .map(bounded_label)
            .unwrap_or_else(|| "Tool".to_owned());
        let phase = if method == "item/started" {
            "started"
        } else if status == ActivityLifecycle::Failed {
            "failed"
        } else {
            "completed"
        };
        let detail = if contains_redacted_value(item) {
            Some(if method == "item/started" {
                "[redacted tool detail]"
            } else {
                "[redacted tool result]"
            })
        } else {
            None
        };
        let tone = if status == ActivityLifecycle::Failed {
            ActivityEntryTone::Error
        } else if method == "item/completed" {
            ActivityEntryTone::Success
        } else {
            ActivityEntryTone::Tool
        };
        let mut output = CodexActivityOutput::default();
        if let Some(entry) = make_entry(
            &mut self.canonical_ids,
            entry_key,
            ActivityRecordKind::Actor,
            actor_id(&self.actors_by_thread, thread_id),
            ActivityEntryKind::Tool,
            &format!("{tool} {phase}"),
            detail,
            tone,
            timestamp,
        ) {
            output.push(ProviderActivityMutation::AppendEntry(entry));
        }
        output
    }

    fn command_entry(
        &mut self,
        entry_key: String,
        thread_id: &str,
        item: &Map<String, Value>,
        method: &str,
        status: ActivityLifecycle,
        timestamp: String,
    ) -> CodexActivityOutput {
        let phase = if method == "item/started" {
            "started"
        } else if status == ActivityLifecycle::Failed {
            "failed"
        } else if status == ActivityLifecycle::Cancelled {
            "cancelled"
        } else {
            "completed"
        };
        let detail = item
            .get("aggregatedOutput")
            .and_then(Value::as_str)
            .and_then(safe_command_output);
        let tone = if status == ActivityLifecycle::Failed {
            ActivityEntryTone::Error
        } else if status == ActivityLifecycle::Cancelled {
            ActivityEntryTone::Warning
        } else if method == "item/completed" {
            ActivityEntryTone::Success
        } else {
            ActivityEntryTone::Tool
        };
        let mut output = CodexActivityOutput::default();
        if let Some(entry) = make_entry(
            &mut self.canonical_ids,
            entry_key,
            ActivityRecordKind::Actor,
            actor_id(&self.actors_by_thread, thread_id),
            ActivityEntryKind::Command,
            &format!("Command {phase}"),
            detail,
            tone,
            timestamp,
        ) {
            output.push(ProviderActivityMutation::AppendEntry(entry));
        }
        output
    }

    fn handle_turn_notification(
        &mut self,
        method: &str,
        params: &Value,
        emitted_at_ms: u64,
        project_terminal_actor: bool,
    ) -> CodexActivityOutput {
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        let turn = params.get("turn").unwrap_or(params);
        let status = turn
            .get("status")
            .and_then(Value::as_str)
            .map(Self::map_status)
            .unwrap_or_else(|| {
                if method == "turn/started" {
                    ActivityLifecycle::Running
                } else {
                    ActivityLifecycle::Unknown
                }
            });
        let validated_turn_id = turn
            .get("id")
            .or_else(|| params.get("turnId"))
            .and_then(Value::as_str)
            .filter(|turn_id| usable_native_id(turn_id));
        let turn_id = validated_turn_id.unwrap_or("turn");
        let completion_conflicts_with_active_turn = method == "turn/completed"
            && self
                .actors_by_thread
                .get(&thread_key(thread_id))
                .and_then(|actor| actor.active_turn_id.as_deref())
                .is_some_and(|active_turn_id| Some(active_turn_id) != validated_turn_id);
        let suppress_detail =
            self.suppress_detail_identity(turn_detail_identity(thread_id, turn_id));
        let entry_key = event_fallback_key(method, thread_id, turn_id, turn_id, status.as_str());
        if !self.actors_by_thread.contains_key(&thread_key(thread_id)) {
            if method != "turn/completed"
                || !self.is_root_thread(thread_id)
                || !self.seen_native_events.insert(entry_key)
            {
                return CodexActivityOutput::default();
            }
            return CodexActivityOutput {
                mutations: Vec::new(),
                controls: Vec::new(),
                request_reconciliation: true,
                hinted_descendant_ids: Vec::new(),
            };
        }
        if !self.seen_native_events.insert(entry_key.clone()) {
            return CodexActivityOutput::default();
        }
        let error_detail = turn
            .get("error")
            .and_then(Value::as_object)
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(bounded_detail);
        let (kind, title, tone) = match status {
            ActivityLifecycle::Failed => (
                ActivityEntryKind::Error,
                "Turn failed",
                ActivityEntryTone::Error,
            ),
            ActivityLifecycle::Interrupted => (
                ActivityEntryKind::State,
                "Turn interrupted",
                ActivityEntryTone::Warning,
            ),
            ActivityLifecycle::Cancelled => (
                ActivityEntryKind::State,
                "Turn cancelled",
                ActivityEntryTone::Warning,
            ),
            ActivityLifecycle::Completed => (
                ActivityEntryKind::State,
                "Turn completed",
                ActivityEntryTone::Success,
            ),
            _ if error_detail.is_some() => (
                ActivityEntryKind::Error,
                "Turn failed",
                ActivityEntryTone::Error,
            ),
            _ => (
                ActivityEntryKind::State,
                "Turn running",
                ActivityEntryTone::Info,
            ),
        };
        let timestamp_ms = turn
            .get("completedAt")
            .and_then(Value::as_u64)
            .map(|seconds| seconds.saturating_mul(1_000))
            .or_else(|| {
                turn.get("startedAt")
                    .and_then(Value::as_u64)
                    .map(|seconds| seconds.saturating_mul(1_000))
            })
            .unwrap_or(emitted_at_ms);
        let terminal_actor_timestamp = (method == "turn/completed" && status.is_terminal())
            .then(|| {
                turn.get("completedAt")
                    .and_then(Value::as_u64)
                    .and_then(checked_provider_timestamp_seconds)
                    .or_else(|| checked_provider_timestamp_millis(emitted_at_ms))
            })
            .flatten();
        let mut output = CodexActivityOutput {
            mutations: Vec::new(),
            controls: Vec::new(),
            request_reconciliation: method == "turn/completed" && status.is_terminal(),
            hinted_descendant_ids: Vec::new(),
        };
        let timestamp = unix_millis_to_timestamp(timestamp_ms);
        if !suppress_detail
            && let Some(entry) = make_entry(
                &mut self.canonical_ids,
                entry_key,
                ActivityRecordKind::Actor,
                actor_id(&self.actors_by_thread, thread_id),
                kind,
                title,
                error_detail.as_deref(),
                tone,
                timestamp.clone(),
            )
        {
            output.push(ProviderActivityMutation::AppendEntry(entry));
        }
        if method == "turn/started" {
            if status.is_terminal() {
                if let Some(control) = self.clear_active_turn_control(thread_id) {
                    output.push_control(control);
                }
            } else {
                let provider_timestamp = checked_provider_timestamp_millis(timestamp_ms);
                let reopen_authority = provider_timestamp.as_deref().map_or(
                    ActorReopenAuthority::None,
                    ActorReopenAuthority::ProviderTimestamp,
                );
                if let Some(actor_update) = self.upsert_actor_state(
                    thread_id,
                    None,
                    None,
                    None,
                    ActivityLifecycle::Running,
                    None,
                    &timestamp,
                    true,
                    reopen_authority,
                ) {
                    output.push_actor_update(actor_update);
                }
                if let Some(control) =
                    self.update_active_turn_control(thread_id, validated_turn_id, true)
                {
                    output.push_control(control);
                }
            }
        } else if method == "turn/completed"
            && let Some(control) =
                self.update_active_turn_control(thread_id, validated_turn_id, false)
        {
            output.push_control(control);
        }
        if project_terminal_actor
            && method == "turn/completed"
            && status.is_terminal()
            && !completion_conflicts_with_active_turn
            && let Some(terminal_actor_timestamp) = terminal_actor_timestamp.as_deref()
            && let Some(actor_update) = self.upsert_actor_state(
                thread_id,
                None,
                None,
                None,
                status,
                None,
                terminal_actor_timestamp,
                true,
                ActorReopenAuthority::None,
            )
        {
            output.push_actor_update(actor_update);
        }
        output
    }

    fn handle_thread_status(&mut self, params: &Value, emitted_at_ms: u64) -> CodexActivityOutput {
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return CodexActivityOutput::default();
        };
        if !self.actors_by_thread.contains_key(&thread_key(thread_id)) {
            return CodexActivityOutput::default();
        }
        let Some(status_value) = params.get("status") else {
            return CodexActivityOutput::default();
        };
        let (status, allow_reopen) = if let Some(legacy_status) = status_value.as_str() {
            (Self::map_status(legacy_status), false)
        } else {
            let Ok(native_status) =
                serde_json::from_value::<ReconciliationThreadStatus>(status_value.clone())
            else {
                return CodexActivityOutput::default();
            };
            (
                Self::map_status(native_status.activity_status()),
                matches!(native_status, ReconciliationThreadStatus::Active { .. }),
            )
        };
        if status == ActivityLifecycle::Unknown {
            return CodexActivityOutput::default();
        }
        let provider_timestamp = checked_provider_timestamp_millis(emitted_at_ms);
        let timestamp = provider_timestamp
            .clone()
            .unwrap_or_else(|| unix_millis_to_timestamp(emitted_at_ms));
        let reopen_authority = if allow_reopen {
            provider_timestamp.as_deref().map_or(
                ActorReopenAuthority::None,
                ActorReopenAuthority::ProviderTimestamp,
            )
        } else {
            ActorReopenAuthority::None
        };
        let mut output = CodexActivityOutput::default();
        if let Some(actor_update) = self.upsert_actor_state(
            thread_id,
            None,
            None,
            None,
            status,
            None,
            &timestamp,
            false,
            reopen_authority,
        ) {
            output.push_actor_update(actor_update);
        }
        output
    }

    pub(crate) fn reconcile_descendants(
        &mut self,
        threads: &[ReconciliationThread],
    ) -> CodexDescendantReconciliation {
        self.reconcile_descendants_with_projection_limit(threads, MAX_RECONCILED_DESCENDANTS)
    }

    pub(crate) fn reconcile_descendants_with_projection_limit(
        &mut self,
        threads: &[ReconciliationThread],
        projection_limit: usize,
    ) -> CodexDescendantReconciliation {
        let mut reconciliation = CodexDescendantReconciliation::default();
        let mut remaining = (0..threads.len()).collect::<Vec<_>>();
        let mut accepted_native_keys = HashSet::new();
        let projection_limit = projection_limit.min(MAX_RECONCILED_DESCENDANTS);
        loop {
            let mut made_progress = false;
            remaining.retain(|index| {
                if accepted_native_keys.len() == projection_limit {
                    return false;
                }
                let thread = &threads[*index];
                let Some(native_thread_id) = thread.id.as_deref().filter(|id| usable_native_id(id))
                else {
                    return false;
                };
                let Some(parent_native_id) = thread
                    .parent_thread_id
                    .as_deref()
                    .filter(|id| usable_native_id(id))
                else {
                    return false;
                };
                if self.is_root_thread(native_thread_id) {
                    return false;
                }
                let parent_key = thread_key(parent_native_id);
                let native_key = thread_key(native_thread_id);
                if native_key == parent_key
                    || self.receiver_is_sender_or_ancestor(native_thread_id, parent_native_id)
                {
                    return false;
                }
                let parent_actor_id = if self.root_thread_id.as_deref() == Some(parent_key.as_str())
                {
                    None
                } else if let Some(parent) = self
                    .actors_by_thread
                    .get(&parent_key)
                    .filter(|_| !self.provisional_actor_keys.contains(&parent_key))
                {
                    Some(parent.canonical_id.clone())
                } else {
                    return true;
                };
                if !accepted_native_keys.insert(native_key.clone()) {
                    return false;
                }
                let existed = self.actors_by_thread.contains_key(&native_key);
                let was_provisional = self.provisional_actor_keys.contains(&native_key);
                let provisional_before = was_provisional
                    .then(|| self.actors_by_thread.get(&native_key).cloned())
                    .flatten();
                let updated_version = thread.updated_at.unwrap_or_default();
                let status = thread
                    .status
                    .as_ref()
                    .map(|status| status.activity_status())
                    .map(Self::map_status)
                    .unwrap_or(ActivityLifecycle::Unknown);
                let allow_reopen = matches!(
                    thread.status,
                    Some(ReconciliationThreadStatus::Active { .. })
                );
                let provider_timestamp = thread
                    .updated_at
                    .and_then(checked_provider_timestamp_seconds);
                let timestamp = provider_timestamp.clone().unwrap_or_else(|| {
                    unix_millis_to_timestamp(updated_version.saturating_mul(1_000))
                });
                let reopen_authority = if allow_reopen {
                    provider_timestamp.as_deref().map_or(
                        ActorReopenAuthority::None,
                        ActorReopenAuthority::ProviderTimestamp,
                    )
                } else {
                    ActorReopenAuthority::None
                };
                let mut actor_update = self.upsert_actor_state(
                    native_thread_id,
                    parent_actor_id.as_deref(),
                    thread.agent_nickname.as_deref().or(thread.name.as_deref()),
                    thread.agent_role.as_deref(),
                    status,
                    None,
                    &timestamp,
                    true,
                    reopen_authority,
                );
                if !existed {
                    if let Some(update) = actor_update.as_mut() {
                        update.actor.started_at = unix_millis_to_timestamp(
                            thread.created_at.unwrap_or_default().saturating_mul(1_000),
                        );
                        if let Some(state) = self.actors_by_thread.get_mut(&native_key) {
                            state.started_at.clone_from(&update.actor.started_at);
                        }
                    }
                } else if was_provisional {
                    actor_update = self
                        .materialize_provisional_actor(
                            &native_key,
                            parent_actor_id.as_deref(),
                            thread.agent_nickname.as_deref().or(thread.name.as_deref()),
                            thread.agent_role.as_deref(),
                            thread.created_at.unwrap_or_default().saturating_mul(1_000),
                            &timestamp,
                        )
                        .map(|actor| ActivityActorUpdate {
                            actor,
                            control: None,
                        });
                    if actor_update.is_none() {
                        if let Some(previous) = provisional_before {
                            self.actors_by_thread.insert(native_key.clone(), previous);
                        }
                        accepted_native_keys.remove(&native_key);
                        return false;
                    }
                }
                if let Some(update) = actor_update {
                    reconciliation.output.push_actor_update(update);
                }
                if self.reconciled_thread_versions.get(&native_key) != Some(&updated_version) {
                    reconciliation
                        .threads_to_read
                        .push(native_thread_id.to_owned());
                }
                self.provisional_actor_keys.remove(&native_key);
                reconciliation
                    .accepted_thread_ids
                    .push(native_thread_id.to_owned());
                made_progress = true;
                false
            });
            if !made_progress || remaining.is_empty() {
                break;
            }
        }
        reconciliation
    }

    #[allow(dead_code, reason = "used by the pending runtime hint integration")]
    pub(crate) fn reconcile_sub_agent_hints(
        &mut self,
        thread: &ReconciliationThread,
    ) -> CodexActivityOutput {
        self.reconcile_sub_agent_hints_with_projection_limit(thread, MAX_RECONCILED_DESCENDANTS)
    }

    pub(crate) fn reconcile_sub_agent_hints_with_projection_limit(
        &mut self,
        thread: &ReconciliationThread,
        projection_limit: usize,
    ) -> CodexActivityOutput {
        self.reconcile_sub_agent_hints_with_projection_limit_excluding(
            thread,
            projection_limit,
            &HashSet::new(),
        )
    }

    pub(crate) fn validated_reconciliation_hint_receiver_ids(
        &self,
        thread: &ReconciliationThread,
    ) -> HashSet<String> {
        let Some(owning_thread_id) = thread.id.as_deref().filter(|id| usable_native_id(id)) else {
            return HashSet::new();
        };
        if !self.is_root_thread(owning_thread_id) && !self.is_verified_child(owning_thread_id) {
            return HashSet::new();
        }
        let mut receiver_ids = HashSet::new();
        let mut normalized_hint_count = 0;
        'turns: for turn in thread.turns.iter().rev().take(MAX_RECONCILED_TURNS).rev() {
            for item in &turn.items {
                let ReconciliationThreadItem::SubAgentActivity {
                    agent_thread_id,
                    agent_path,
                    kind,
                    ..
                } = item
                else {
                    continue;
                };
                if normalized_hint_count == MAX_RECONCILED_ENTRIES {
                    break 'turns;
                }
                normalized_hint_count += 1;
                if let Some(hint) = self.validate_sub_agent_hint(
                    owning_thread_id,
                    agent_thread_id.as_deref(),
                    agent_path.as_deref(),
                    kind.as_deref(),
                ) {
                    receiver_ids.insert(hint.native_thread_id.to_owned());
                }
            }
        }
        receiver_ids
    }

    pub(crate) fn reconcile_sub_agent_hints_with_projection_limit_excluding(
        &mut self,
        thread: &ReconciliationThread,
        projection_limit: usize,
        excluded_native_thread_ids: &HashSet<String>,
    ) -> CodexActivityOutput {
        let Some(owning_thread_id) = thread.id.as_deref().filter(|id| usable_native_id(id)) else {
            return CodexActivityOutput::default();
        };
        if !self.is_root_thread(owning_thread_id) && !self.is_verified_child(owning_thread_id) {
            return CodexActivityOutput::default();
        }
        let recent_turns = thread
            .turns
            .iter()
            .rev()
            .take(MAX_RECONCILED_TURNS)
            .collect::<Vec<_>>();
        let mut output = CodexActivityOutput::default();
        let mut normalized_hint_count = 0;
        'turns: for turn in recent_turns.into_iter().rev() {
            let timestamp_ms = turn
                .completed_at
                .or(turn.started_at)
                .or(thread.updated_at)
                .unwrap_or_default()
                .saturating_mul(1_000);
            for item in &turn.items {
                let ReconciliationThreadItem::SubAgentActivity {
                    agent_thread_id,
                    agent_path,
                    kind,
                    ..
                } = item
                else {
                    continue;
                };
                if normalized_hint_count == MAX_RECONCILED_ENTRIES
                    || output.hinted_descendant_ids.len() == MAX_RECONCILED_DESCENDANTS
                {
                    break 'turns;
                }
                normalized_hint_count += 1;
                let already_hinted = agent_thread_id.as_deref().is_some_and(|native_thread_id| {
                    output
                        .hinted_descendant_ids
                        .iter()
                        .any(|existing| existing == native_thread_id)
                });
                if already_hinted {
                    continue;
                }
                if agent_thread_id.as_ref().is_some_and(|native_thread_id| {
                    excluded_native_thread_ids.contains(native_thread_id)
                }) {
                    continue;
                }
                if output.hinted_descendant_ids.len() < projection_limit {
                    let mut hint_output = self.handle_sub_agent_activity(
                        owning_thread_id,
                        agent_thread_id.as_deref(),
                        agent_path.as_deref(),
                        kind.as_deref(),
                        timestamp_ms,
                    );
                    if hint_output.hinted_descendant_ids.is_empty()
                        && let Some(hint) = self.validate_sub_agent_hint(
                            owning_thread_id,
                            agent_thread_id.as_deref(),
                            agent_path.as_deref(),
                            kind.as_deref(),
                        )
                    {
                        hint_output.push_hint(hint.native_thread_id);
                        hint_output.request_reconciliation = true;
                    }
                    output.merge(hint_output);
                } else if let Some(hint) = self.validate_sub_agent_hint(
                    owning_thread_id,
                    agent_thread_id.as_deref(),
                    agent_path.as_deref(),
                    kind.as_deref(),
                ) {
                    output.push_hint(hint.native_thread_id);
                    output.request_reconciliation = true;
                }
            }
        }
        output
    }

    pub(crate) fn reconcile_thread_history(
        &mut self,
        thread: &ReconciliationThread,
    ) -> CodexActivityOutput {
        let Some(native_thread_id) = thread.id.as_deref().filter(|id| usable_native_id(id)) else {
            return CodexActivityOutput::default();
        };
        let native_key = thread_key(native_thread_id);
        if !self.actors_by_thread.contains_key(&native_key) {
            return CodexActivityOutput::default();
        }
        let Some(created_at) = thread.created_at else {
            self.reconciled_thread_versions
                .insert(native_key, thread.updated_at.unwrap_or_default());
            return CodexActivityOutput::default();
        };
        let mut output = CodexActivityOutput::default();
        if let Some(started_at) = checked_provider_timestamp_seconds(created_at)
            && let Some(actor) = self.actors_by_thread.get_mut(&native_key)
            && activity_timestamp_is_unix_epoch(&actor.started_at)
            && let Some(updated_at) = latest_activity_timestamp([
                actor.started_at.as_str(),
                actor.updated_at.as_str(),
                started_at.as_str(),
            ])
        {
            let mut candidate = actor.clone();
            candidate.started_at = started_at;
            candidate.updated_at = updated_at;
            if let Some(summary) = candidate.to_summary() {
                candidate.started_at.clone_from(&summary.started_at);
                candidate.updated_at.clone_from(&summary.updated_at);
                candidate.terminal_at.clone_from(&summary.terminal_at);
                *actor = candidate;
                output.push(ProviderActivityMutation::UpsertActor(summary));
            }
        }
        let topology_turns = thread
            .turns
            .iter()
            .filter(|turn| turn.started_at.is_some_and(|at| at >= created_at))
            .rev()
            .take(MAX_RECONCILED_TURNS)
            .collect::<Vec<_>>();
        let mut retained_entries = Vec::with_capacity(MAX_RECONCILED_ENTRIES);
        let mut normalized_entry_count = 0;
        'turns: for turn in &topology_turns {
            let Some(turn_id) = turn.id.as_deref().filter(|id| usable_native_id(id)) else {
                continue;
            };
            let emitted_at_ms = turn
                .completed_at
                .or(turn.started_at)
                .or(thread.updated_at)
                .unwrap_or_default()
                .saturating_mul(1_000);
            let terminal_status = turn
                .status
                .as_deref()
                .filter(|status| CodexActivityTracker::map_status(status).is_terminal());
            if let Some(status) = terminal_status {
                if normalized_entry_count == MAX_RECONCILED_ENTRIES {
                    break;
                }
                normalized_entry_count += 1;
                let turn_output = self.handle_turn_notification(
                    "turn/completed",
                    &serde_json::json!({
                        "threadId": native_thread_id,
                        "turn": {
                            "id": turn_id,
                            "status": status,
                            "error": turn.error.as_ref().and_then(|error| {
                                error.message.as_deref().map(|message| {
                                    serde_json::json!({"message": message})
                                })
                            }),
                            "startedAt": turn.started_at,
                            "completedAt": turn.completed_at,
                        }
                    }),
                    emitted_at_ms,
                    false,
                );
                for control in turn_output.controls {
                    output.push_control(control);
                }
                retained_entries.extend(turn_output.mutations.into_iter().filter(|mutation| {
                    matches!(mutation, ProviderActivityMutation::AppendEntry(_))
                }));
            }
            for item in turn.items.iter().rev() {
                let Ok(item) = serde_json::to_value(item) else {
                    continue;
                };
                if !reconciliation_item_can_append_entry(&item) {
                    continue;
                }
                if normalized_entry_count == MAX_RECONCILED_ENTRIES {
                    break 'turns;
                }
                normalized_entry_count += 1;
                let item_output = self.handle_item_notification(
                    "item/completed",
                    &serde_json::json!({
                        "threadId": native_thread_id,
                        "turnId": turn_id,
                        "item": item,
                        "completedAtMs": emitted_at_ms,
                    }),
                    emitted_at_ms,
                );
                retained_entries.extend(item_output.mutations.into_iter().filter(|mutation| {
                    matches!(mutation, ProviderActivityMutation::AppendEntry(_))
                }));
            }
        }
        retained_entries.reverse();
        for mutation in retained_entries {
            output.push(mutation);
        }
        let active_turn_ids = if matches!(
            thread.status,
            Some(ReconciliationThreadStatus::Active { .. })
        ) {
            topology_turns
                .iter()
                .filter(|turn| turn.completed_at.is_none())
                .filter_map(|turn| {
                    let turn_id = turn
                        .id
                        .as_deref()
                        .filter(|turn_id| usable_native_id(turn_id))?;
                    let status = turn.status.as_deref().map(Self::map_status)?;
                    matches!(
                        status,
                        ActivityLifecycle::Starting
                            | ActivityLifecycle::Running
                            | ActivityLifecycle::Waiting
                    )
                    .then_some(turn_id)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if let [turn_id] = active_turn_ids.as_slice() {
            if let Some(control) =
                self.update_active_turn_control(native_thread_id, Some(turn_id), true)
            {
                output.push_control(control);
            }
        } else if let Some(control) = self.clear_active_turn_control(native_thread_id) {
            output.push_control(control);
        }
        let native_status_allows_terminal_projection = matches!(
            thread.status,
            None | Some(ReconciliationThreadStatus::NotLoaded)
                | Some(ReconciliationThreadStatus::Idle)
                | Some(ReconciliationThreadStatus::Unknown)
        );
        if native_status_allows_terminal_projection
            && let Some(turn) = topology_turns.first()
            && let Some(status) = turn
                .status
                .as_deref()
                .map(Self::map_status)
                .filter(|status| status.is_terminal())
            && let Some(timestamp) = turn
                .completed_at
                .and_then(checked_provider_timestamp_seconds)
                .or_else(|| {
                    thread
                        .updated_at
                        .and_then(checked_provider_timestamp_seconds)
                })
            && let Some(actor_update) = self.upsert_actor_state(
                native_thread_id,
                None,
                None,
                None,
                status,
                None,
                &timestamp,
                true,
                ActorReopenAuthority::None,
            )
        {
            output.push_actor_update(actor_update);
        }
        self.reconciled_thread_versions
            .insert(native_key, thread.updated_at.unwrap_or_default());
        output
    }

    pub(crate) fn reconcile_background_terminals(
        &mut self,
        terminals: &[ReconciliationBackgroundTerminal],
        timestamp: &str,
        authority: BackgroundSnapshotAuthority,
    ) -> CodexActivityOutput {
        let mut output = CodexActivityOutput::default();
        let mut accepted_native_keys = HashSet::new();
        for terminal in terminals {
            let Some(native_id) = terminal
                .item_id
                .as_deref()
                .filter(|id| usable_native_id(id))
            else {
                continue;
            };
            let native_key = work_key(native_id);
            if accepted_native_keys.len() == MAX_TRACKED_WORK_ITEMS {
                break;
            }
            if !accepted_native_keys.insert(native_key) {
                continue;
            }
            let name = terminal
                .command
                .as_deref()
                .filter(|command| !command.trim().is_empty())
                .map(bounded_label)
                .unwrap_or_else(|| "Background terminal".to_owned());
            let summary = terminal.process_id.as_deref().map(bounded_summary);
            if let Some(work_item) = self.upsert_work_item_state(
                native_id,
                None,
                name,
                ActivityLifecycle::Running,
                summary,
                timestamp,
            ) {
                output.push(ProviderActivityMutation::UpsertWorkItem(work_item));
            }
        }
        if authority == BackgroundSnapshotAuthority::Partial {
            return output;
        }
        for (native_key, existing) in &mut self.work_items_by_native_id {
            if existing.status != ActivityLifecycle::Running
                || accepted_native_keys.contains(native_key)
            {
                continue;
            }
            let Some(updated_at) = latest_activity_timestamp([
                existing.started_at.as_str(),
                existing.updated_at.as_str(),
                timestamp,
            ]) else {
                continue;
            };
            let mut interrupted = existing.clone();
            interrupted.status = ActivityLifecycle::Interrupted;
            interrupted.updated_at = updated_at.clone();
            interrupted.terminal_at = Some(updated_at);
            let Some(summary) = interrupted.to_summary() else {
                continue;
            };
            *existing = interrupted;
            output.push(ProviderActivityMutation::UpsertWorkItem(summary));
        }
        output
    }

    fn handle_thread_list(&mut self, result: &Value) -> CodexActivityOutput {
        decode_thread_list_response(result.clone()).map_or_else(
            |_| CodexActivityOutput::default(),
            |response| self.reconcile_descendants(&response.data).output,
        )
    }

    fn handle_thread_read(&mut self, result: &Value) -> CodexActivityOutput {
        decode_thread_read_response(result.clone()).map_or_else(
            |_| CodexActivityOutput::default(),
            |response| self.reconcile_thread_history(&response.thread),
        )
    }

    fn upsert_work_item_state(
        &mut self,
        native_id: &str,
        owner_actor_id: Option<&str>,
        name: String,
        status: ActivityLifecycle,
        summary: Option<String>,
        timestamp: &str,
    ) -> Option<ActivityWorkItemSummary> {
        let native_key = work_key(native_id);
        if let Some(existing) = self.work_items_by_native_id.get_mut(&native_key) {
            if existing.status.is_terminal() && !status.is_terminal() {
                return None;
            }
            if existing.status.is_terminal() && status.is_terminal() && existing.status != status {
                return None;
            }
            if existing.status == status && existing.summary == summary {
                return None;
            }
            existing.status = status;
            existing.summary = summary;
            existing.updated_at = timestamp.to_owned();
            existing.terminal_at = status.is_terminal().then(|| timestamp.to_owned());
            return existing.to_summary();
        }
        if native_id.is_empty() || self.work_items_by_native_id.len() >= MAX_TRACKED_WORK_ITEMS {
            return None;
        }
        let canonical_id = self.canonical_ids.resolve("codex:item:", native_id)?;
        let state = ActivityWorkItemState {
            canonical_id,
            owner_actor_id: owner_actor_id.map(str::to_owned),
            name,
            work_kind: "background".to_owned(),
            status,
            summary,
            started_at: timestamp.to_owned(),
            updated_at: timestamp.to_owned(),
            terminal_at: status.is_terminal().then(|| timestamp.to_owned()),
        };
        let summary = state.to_summary()?;
        self.work_items_by_native_id.insert(native_key, state);
        Some(summary)
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct CodexActivityFixtureAdapter {
    tracker: CodexActivityTracker,
    next_receive_sequence: u128,
}

#[doc(hidden)]
impl CodexActivityFixtureAdapter {
    #[must_use]
    pub fn new(root_thread_id: Option<&str>) -> Self {
        Self {
            tracker: CodexActivityTracker::new(root_thread_id),
            next_receive_sequence: 0,
        }
    }

    pub fn seed_actor(&mut self, native_thread_id: &str) {
        self.tracker.seed_actor(native_thread_id);
    }

    #[must_use]
    pub fn state_counts(&self) -> CodexActivityStateCounts {
        self.tracker.state_counts()
    }

    #[must_use]
    pub fn map_status(value: &str) -> ActivityLifecycle {
        CodexActivityTracker::map_status(value)
    }

    pub fn handle_envelope(&mut self, envelope: &Value) -> CodexActivityOutput {
        if self.tracker.root_thread_id.is_none()
            && let Some(params) = envelope.get("params")
            && let Some(thread_id) = params.get("threadId").and_then(Value::as_str)
            && let Some(sender_thread_id) = params
                .pointer("/item/senderThreadId")
                .and_then(Value::as_str)
            && thread_id == sender_thread_id
        {
            self.tracker.root_thread_id = Some(thread_key(thread_id));
        }
        let receive_sequence = self.take_receive_sequence();
        self.tracker.handle_envelope(envelope, receive_sequence)
    }

    pub fn handle_notification(
        &mut self,
        method: &str,
        params: &Value,
        emitted_at_ms: u64,
    ) -> CodexActivityOutput {
        let receive_sequence = self.take_receive_sequence();
        self.tracker
            .handle_notification(method, params, emitted_at_ms, receive_sequence)
    }

    pub fn handle_notification_with_sequence(
        &mut self,
        method: &str,
        params: &Value,
        emitted_at_ms: u64,
        receive_sequence: u64,
    ) -> CodexActivityOutput {
        let receive_sequence = u128::from(receive_sequence);
        self.next_receive_sequence = self.next_receive_sequence.max(receive_sequence + 1);
        self.tracker
            .handle_notification(method, params, emitted_at_ms, receive_sequence)
    }

    fn take_receive_sequence(&mut self) -> u128 {
        let sequence = self.next_receive_sequence;
        self.next_receive_sequence = self
            .next_receive_sequence
            .checked_add(1)
            .expect("activity fixture receive sequence exhausted");
        sequence
    }
}

#[allow(clippy::too_many_arguments)]
fn make_entry(
    canonical_ids: &mut CanonicalIdGenerator,
    id: String,
    owner_kind: ActivityRecordKind,
    owner_id: String,
    kind: ActivityEntryKind,
    title: &str,
    detail: Option<&str>,
    tone: ActivityEntryTone,
    timestamp: String,
) -> Option<ActivityEntry> {
    ActivityEntry::try_new(
        canonical_ids.resolve("codex:event:", &id)?,
        owner_kind,
        owner_id,
        kind,
        bounded_label(title),
        detail.map(bounded_detail).as_deref(),
        tone,
        timestamp,
    )
    .ok()
}

fn actor_id(
    actors_by_thread: &HashMap<String, ActivityActorState>,
    native_thread_id: &str,
) -> String {
    actors_by_thread
        .get(&thread_key(native_thread_id))
        .map(|actor| actor.canonical_id.clone())
        .unwrap_or_default()
}

fn event_fallback_key(
    method: &str,
    thread_id: &str,
    turn_id: &str,
    item_id: &str,
    status_key: &str,
) -> String {
    let normalized_method = method_key(method);
    let components = [
        normalized_method.as_str(),
        thread_id,
        turn_id,
        item_id,
        status_key,
    ];
    let legacy_length = components
        .iter()
        .map(|component| component.len())
        .sum::<usize>()
        + components.len()
        - 1;
    if legacy_length <= MAX_INLINE_EVENT_KEY_BYTES
        && components
            .iter()
            .all(|component| is_legacy_event_component(component))
    {
        return components.join(":");
    }
    length_prefixed_key("event", &components)
}

fn item_detail_identity(thread_id: &str, turn_id: &str, item_id: &str) -> String {
    length_prefixed_key("detail-item", &[thread_id, turn_id, item_id])
}

fn turn_detail_identity(thread_id: &str, turn_id: &str) -> String {
    length_prefixed_key("detail-turn", &[thread_id, turn_id])
}

fn delta_replay_key(
    method: &str,
    buffer_key: &str,
    receive_sequence: u128,
    summary_index: Option<u64>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(method.as_bytes());
    digest.update([0]);
    digest.update(buffer_key.as_bytes());
    digest.update(receive_sequence.to_be_bytes());
    if let Some(summary_index) = summary_index {
        digest.update([1]);
        digest.update(summary_index.to_be_bytes());
    } else {
        digest.update([0]);
    }
    format!("delta:{}", hex_digest(&digest.finalize()))
}

fn method_key(method: &str) -> String {
    let mut output = String::with_capacity(method.len() + 4);
    for character in method.chars() {
        if character == '/' || character == '_' {
            output.push('-');
        } else if character.is_ascii_uppercase() {
            output.push('-');
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character.to_ascii_lowercase());
        }
    }
    output
}

fn is_legacy_event_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn length_prefixed_key(namespace: &str, components: &[&str]) -> String {
    let projected_length = namespace.len()
        + 3
        + components
            .iter()
            .map(|component| decimal_digits(component.len()) + 2 + component.len())
            .sum::<usize>();
    if projected_length <= MAX_INLINE_EVENT_KEY_BYTES {
        let mut output = String::with_capacity(projected_length);
        output.push_str("v1:");
        output.push_str(namespace);
        for component in components {
            let _ = write!(output, "|{}#{component}", component.len());
        }
        return output;
    }

    let mut digest = Sha256::new();
    digest.update(namespace.as_bytes());
    for component in components {
        digest.update(component.len().to_be_bytes());
        digest.update(component.as_bytes());
    }
    format!("v1h:{}", hex_digest(&digest.finalize()))
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        digits += 1;
        value /= 10;
    }
    digits
}

fn delta_stream_key(thread_id: &str, turn_id: &str, item_id: &str) -> String {
    length_prefixed_key("stream", &[thread_id, turn_id, item_id])
}

pub(crate) fn usable_native_id(native_id: &str) -> bool {
    !native_id.is_empty()
        && native_id.len() <= MAX_RETAINED_KEY_BYTES
        && !native_id.chars().any(char::is_whitespace)
}

fn thread_key(native_thread_id: &str) -> String {
    retained_key("thread", native_thread_id)
}

fn work_key(native_item_id: &str) -> String {
    retained_key("work", native_item_id)
}

fn retained_key(namespace: &str, native: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(namespace.len().to_be_bytes());
    digest.update(namespace.as_bytes());
    digest.update(native.len().to_be_bytes());
    digest.update(native.as_bytes());
    let key = format!("{namespace}:{}", hex_digest(&digest.finalize()));
    debug_assert!(key.len() <= MAX_RETAINED_KEY_BYTES);
    key
}

fn canonical_id_can_inline(prefix: &str, native: &str) -> bool {
    if native.is_empty()
        || native.chars().any(char::is_whitespace)
        || is_reserved_canonical_hash(native)
    {
        return false;
    }
    let mut units = prefix.encode_utf16().count();
    for character in native.chars() {
        units += character.len_utf16();
        if units > ACTIVITY_ID_MAX_LENGTH {
            return false;
        }
    }
    true
}

fn is_reserved_canonical_hash(native: &str) -> bool {
    native
        .strip_prefix('h')
        .and_then(|value| value.split('-').next())
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn canonical_digest(bytes: &[u8]) -> CanonicalDigest {
    let primary: [u8; 32] = Sha256::digest(bytes).into();
    let sha512 = Sha512::digest(bytes);
    let mut secondary = [0_u8; 32];
    secondary.copy_from_slice(&sha512[..32]);
    CanonicalDigest { primary, secondary }
}

fn bounded_label(value: &str) -> String {
    clip_utf16_units(value, ACTIVITY_LABEL_MAX_LENGTH)
}

fn bounded_actor_name(native_thread_id: &str) -> String {
    let native = clip_utf16_units(
        native_thread_id,
        ACTIVITY_LABEL_MAX_LENGTH.saturating_sub("Codex ".len()),
    );
    format!("Codex {native}")
}

fn bounded_summary(value: &str) -> String {
    clip_utf16_units(value, ACTIVITY_SUMMARY_MAX_LENGTH)
}

fn bounded_detail(value: &str) -> String {
    clip_utf8_bytes(value, ACTIVITY_DETAIL_MAX_LENGTH)
}

fn safe_command_output(value: &str) -> Option<&'static str> {
    // App Server's aggregatedOutput is raw process output and can contain
    // credentials or environment values. Until a structured allow-list exists,
    // expose only the fact that non-empty output was present.
    (!value.is_empty()).then_some("[redacted command output]")
}

fn reasoning_summary_detail(parts: &[Value]) -> Option<String> {
    let mut summary = String::new();
    for part in parts.iter().filter_map(Value::as_str) {
        if part.is_empty() {
            continue;
        }
        if !summary.is_empty() {
            append_utf8_bounded(&mut summary, "\n", ACTIVITY_DETAIL_MAX_LENGTH);
        }
        append_utf8_bounded(&mut summary, part, ACTIVITY_DETAIL_MAX_LENGTH);
        if summary.len() == ACTIVITY_DETAIL_MAX_LENGTH {
            break;
        }
    }
    (!summary.is_empty()).then_some(summary)
}

fn reconciliation_item_can_append_entry(item: &Value) -> bool {
    let Some(item) = item.as_object() else {
        return false;
    };
    let valid_id = item
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(usable_native_id);
    if !valid_id {
        return false;
    }
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => item
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some("reasoning") => item
            .get("summary")
            .and_then(Value::as_array)
            .and_then(|parts| reasoning_summary_detail(parts))
            .is_some(),
        Some("commandExecution" | "dynamicToolCall" | "mcpToolCall") => true,
        _ => false,
    }
}

fn clip_utf16_units(value: &str, maximum: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|character| {
            let next = units + character.len_utf16();
            if next > maximum {
                return false;
            }
            units = next;
            true
        })
        .collect()
}

fn clip_utf8_bytes(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn append_utf8_bounded(target: &mut String, delta: &str, maximum: usize) {
    if target.len() >= maximum {
        return;
    }
    let remaining = maximum - target.len();
    if delta.len() <= remaining {
        target.push_str(delta);
        return;
    }
    let mut end = remaining;
    while !delta.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&delta[..end]);
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn item_timestamp_ms(params: &Value, fallback: u64) -> u64 {
    params
        .get("completedAtMs")
        .or_else(|| params.get("startedAtMs"))
        .and_then(Value::as_u64)
        .unwrap_or(fallback)
}

fn checked_provider_timestamp_seconds(seconds: u64) -> Option<String> {
    seconds
        .checked_mul(1_000)
        .and_then(checked_provider_timestamp_millis)
}

fn checked_provider_timestamp_millis(milliseconds: u64) -> Option<String> {
    if milliseconds == 0 {
        return None;
    }
    let nanos = i128::from(milliseconds).checked_mul(1_000_000)?;
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(nanos).ok()?;
    timestamp
        .format(&Rfc3339)
        .ok()
        .map(normalize_fractional_seconds)
}

fn unix_millis_to_timestamp(milliseconds: u64) -> String {
    checked_provider_timestamp_millis(milliseconds)
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

fn activity_timestamp_is_unix_epoch(timestamp: &str) -> bool {
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .is_ok_and(|timestamp| timestamp.unix_timestamp_nanos() == 0)
}

fn latest_activity_timestamp(timestamps: [&str; 3]) -> Option<String> {
    let mut latest = None;
    for timestamp in timestamps {
        let parsed = OffsetDateTime::parse(timestamp, &Rfc3339).ok()?;
        if latest
            .as_ref()
            .is_none_or(|(latest_timestamp, _)| parsed > *latest_timestamp)
        {
            latest = Some((parsed, timestamp));
        }
    }
    latest.map(|(_, timestamp)| timestamp.to_owned())
}

fn activity_timestamp_is_not_older(candidate: &str, existing: &str) -> bool {
    let Ok(candidate) = OffsetDateTime::parse(candidate, &Rfc3339) else {
        return false;
    };
    let Ok(existing) = OffsetDateTime::parse(existing, &Rfc3339) else {
        return false;
    };
    candidate >= existing
}

fn normalize_fractional_seconds(mut timestamp: String) -> String {
    let Some(dot) = timestamp.find('.') else {
        return timestamp;
    };
    let Some(zone) = timestamp.rfind('Z') else {
        return timestamp;
    };
    let trimmed = timestamp[dot + 1..zone].trim_end_matches('0').len();
    if trimmed == 0 {
        timestamp.replace_range(dot..zone, "");
    } else {
        timestamp.replace_range(dot + 1 + trimmed..zone, "");
    }
    timestamp
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    mod targeted_control {
        use crate::activity::{
            ActivityActorControlState, ActivityControlRegistry, ActivityScopeRef,
            ProviderActivityControlUpdate,
        };

        use super::*;

        fn verified_child(tracker: &mut CodexActivityTracker, thread_id: &str) {
            let child = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": thread_id,
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("verified child thread")
            .thread;
            let reconciliation = tracker.reconcile_descendants(&[child]);
            assert_eq!(reconciliation.accepted_thread_ids, [thread_id]);
        }

        fn target_ids(update: &ProviderActivityControlUpdate) -> Option<(&str, &str)> {
            match update {
                ProviderActivityControlUpdate::ActorTarget {
                    target: Some(target),
                    ..
                } => target.codex_turn_ids(),
                ProviderActivityControlUpdate::ActorTarget { target: None, .. }
                | ProviderActivityControlUpdate::WorkTarget { .. } => None,
            }
        }

        #[test]
        fn verified_child_turn_lifecycle_installs_and_removes_exact_target() {
            // Mutation caught: exposing a target before verification or retaining it after completion.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");

            let started = tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "child-turn-7", "status": "inProgress"}
                }),
                3_000,
                0,
            );
            assert!(matches!(
                started.controls.as_slice(),
                [ProviderActivityControlUpdate::ActorTarget { actor_id, .. }]
                    if actor_id == "codex:thread:child-2"
            ));
            assert_eq!(
                target_ids(&started.controls[0]),
                Some(("child-2", "child-turn-7"))
            );

            let completed = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {
                        "id": "child-turn-7",
                        "status": "completed",
                        "completedAt": 4
                    }
                }),
                4_000,
                1,
            );
            assert!(matches!(
                completed.controls.as_slice(),
                [ProviderActivityControlUpdate::ActorTarget {
                    actor_id,
                    target: None,
                }] if actor_id == "codex:thread:child-2"
            ));
        }

        #[test]
        fn reconciled_active_child_turn_installs_the_same_exact_target() {
            // Mutation caught: limiting exact handles to live notifications and losing reconnect recovery.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");
            let child = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "child-2",
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": 3,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": [{
                        "id": "child-turn-7",
                        "status": "inProgress",
                        "startedAt": 3,
                        "items": []
                    }]
                }
            }))
            .expect("active child history")
            .thread;

            let output = tracker.reconcile_thread_history(&child);
            assert_eq!(output.controls.len(), 1);
            assert_eq!(
                target_ids(&output.controls[0]),
                Some(("child-2", "child-turn-7"))
            );
        }

        #[test]
        fn stale_completion_cannot_terminalize_or_replace_a_new_active_turn() {
            // Mutation caught: letting an old completion invalidate a later canonical reopening.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");
            tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "inProgress"}
                }),
                3_000,
                0,
            );
            tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "completed", "completedAt": 4}
                }),
                4_000,
                1,
            );
            tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-2", "status": "inProgress", "startedAt": 5}
                }),
                5_000,
                2,
            );

            let stale = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {
                        "id": "late-old-turn",
                        "status": "failed",
                        "completedAt": 6
                    }
                }),
                6_000,
                3,
            );

            assert!(stale.controls.is_empty());
            assert!(stale.mutations.iter().all(|mutation| !matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor) if actor.status.is_terminal()
            )));
            assert!(tracker.is_current_target("child-2", "turn-2"));
        }

        #[test]
        fn terminal_started_notification_revokes_an_existing_target() {
            // Mutation caught: retaining an opaque target after terminal provider evidence.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");
            tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "inProgress"}
                }),
                3_000,
                0,
            );

            let terminal = tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "completed"}
                }),
                4_000,
                1,
            );

            assert!(matches!(
                terminal.controls.as_slice(),
                [ProviderActivityControlUpdate::ActorTarget { target: None, .. }]
            ));
            assert!(!tracker.is_current_target("child-2", "turn-1"));
        }

        #[test]
        fn terminal_thread_status_revokes_target_before_status_only_reopen() {
            // Mutation caught: preserving a completed turn across a status-only actor reopen.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");
            tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "inProgress", "startedAt": 3}
                }),
                3_000,
                0,
            );

            let terminal = tracker.handle_notification(
                "thread/status/changed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "status": {"type": "systemError"}
                }),
                4_000,
                1,
            );
            assert!(matches!(
                terminal.controls.as_slice(),
                [ProviderActivityControlUpdate::ActorTarget { target: None, .. }]
            ));

            tracker.handle_notification(
                "thread/status/changed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "status": {"type": "active", "activeFlags": []}
                }),
                5_000,
                2,
            );
            assert!(!tracker.is_current_target("child-2", "turn-1"));
        }

        #[test]
        fn terminal_descendant_reconciliation_revokes_target_before_status_only_reopen() {
            // Mutation caught: preserving a reconciled terminal actor's old active turn.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");
            tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "inProgress", "startedAt": 3}
                }),
                3_000,
                0,
            );

            let terminal_child = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "child-2",
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": 4,
                    "status": {"type": "systemError"},
                    "turns": []
                }
            }))
            .expect("terminal child")
            .thread;
            let terminal = tracker.reconcile_descendants(&[terminal_child]);
            assert!(matches!(
                terminal.output.controls.as_slice(),
                [ProviderActivityControlUpdate::ActorTarget { target: None, .. }]
            ));

            let reopened_child = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "child-2",
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": 5,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("reopened child")
            .thread;
            tracker.reconcile_descendants(&[reopened_child]);
            assert!(!tracker.is_current_target("child-2", "turn-1"));
        }

        #[test]
        fn stale_terminal_status_cannot_revoke_a_newer_active_turn() {
            // Mutation caught: clearing a current handle with older terminal status evidence.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            verified_child(&mut tracker, "child-2");
            tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-2", "status": "inProgress", "startedAt": 5}
                }),
                5_000,
                0,
            );

            let stale = tracker.handle_notification(
                "thread/status/changed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "status": {"type": "systemError"}
                }),
                4_000,
                1,
            );

            assert!(stale.controls.is_empty());
            assert!(tracker.is_current_target("child-2", "turn-2"));
        }

        #[test]
        fn ambiguous_or_untrusted_turn_evidence_never_becomes_available() {
            // Mutation caught: accepting root, provisional, malformed, stale, or conflicting handles.
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.handle_notification(
                "item/started",
                &serde_json::json!({
                    "threadId": "root-1",
                    "turnId": "root-turn",
                    "item": {
                        "id": "spawn",
                        "type": "subAgentActivity",
                        "agentThreadId": "provisional-child",
                        "agentPath": "/root/provisional",
                        "kind": "started"
                    }
                }),
                1_000,
                0,
            );
            assert!(
                tracker
                    .handle_notification(
                        "turn/started",
                        &serde_json::json!({
                            "threadId": "provisional-child",
                            "turn": {"id": "provisional-turn"}
                        }),
                        2_000,
                        1,
                    )
                    .controls
                    .is_empty()
            );
            assert!(
                tracker
                    .handle_notification(
                        "turn/started",
                        &serde_json::json!({
                            "threadId": "root-1",
                            "turn": {"id": "root-turn"}
                        }),
                        2_000,
                        2,
                    )
                    .controls
                    .is_empty()
            );

            verified_child(&mut tracker, "child-2");
            for (sequence, turn) in [
                serde_json::json!({}),
                serde_json::json!({"id": "x".repeat(MAX_RETAINED_KEY_BYTES + 1)}),
                serde_json::json!({"id": "contains whitespace"}),
                serde_json::json!({"id": "terminal-turn", "status": "completed"}),
            ]
            .into_iter()
            .enumerate()
            {
                assert!(
                    tracker
                        .handle_notification(
                            "turn/started",
                            &serde_json::json!({"threadId": "child-2", "turn": turn}),
                            3_000 + sequence as u64,
                            3 + sequence as u128,
                        )
                        .controls
                        .is_empty()
                );
            }

            let first = tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "current-turn", "status": "inProgress"}
                }),
                4_000,
                7,
            );
            assert_eq!(
                target_ids(&first.controls[0]),
                Some(("child-2", "current-turn"))
            );
            let conflict = tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "conflicting-turn", "status": "inProgress"}
                }),
                4_001,
                8,
            );
            assert!(matches!(
                conflict.controls.as_slice(),
                [ProviderActivityControlUpdate::ActorTarget { target: None, .. }]
            ));
            let stale = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "current-turn", "status": "completed", "completedAt": 5}
                }),
                5_000,
                9,
            );
            assert!(stale.controls.is_empty());
        }

        #[tokio::test]
        async fn reopened_child_turn_advances_control_revision() {
            // Mutation caught: reusing a cancellation fence when the canonical actor gets a new turn.
            let registry = ActivityControlRegistry::new();
            let registration = registry.register_runtime(
                ActivityScopeRef::Thread {
                    thread_id: "thread-1".to_owned(),
                },
                "scope-1".to_owned(),
                Some("codex".to_owned()),
            );
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            let child = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "child-2",
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": 2,
                    "status": {"type": "active", "activeFlags": []},
                    "turns": []
                }
            }))
            .expect("verified child")
            .thread;
            let observed = tracker.reconcile_descendants(&[child]);
            registry
                .observe_provider_batch(&registration, &observed.output.mutations, &[])
                .await;

            let first = tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "inProgress"}
                }),
                3_000,
                0,
            );
            registry
                .observe_provider_batch(&registration, &first.mutations, &first.controls)
                .await;
            let first_revision = registry
                .snapshot("scope-1")
                .await
                .actors
                .into_iter()
                .find(|actor| actor.actor_id == "codex:thread:child-2")
                .expect("first control")
                .control_revision;

            let completed = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-1", "status": "completed", "completedAt": 4}
                }),
                4_000,
                1,
            );
            registry
                .observe_provider_batch(&registration, &completed.mutations, &completed.controls)
                .await;
            let reopened = tracker.handle_notification(
                "turn/started",
                &serde_json::json!({
                    "threadId": "child-2",
                    "turn": {"id": "turn-2", "status": "inProgress"}
                }),
                5_000,
                2,
            );
            registry
                .observe_provider_batch(&registration, &reopened.mutations, &reopened.controls)
                .await;
            let control = registry
                .snapshot("scope-1")
                .await
                .actors
                .into_iter()
                .find(|actor| actor.actor_id == "codex:thread:child-2")
                .expect("reopened control");
            assert_eq!(control.state, ActivityActorControlState::Available);
            assert!(control.control_revision > first_revision);
        }
    }

    #[test]
    fn authoritative_background_snapshot_interrupts_disappeared_running_work() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        let first = tracker.reconcile_background_terminals(
            &[ReconciliationBackgroundTerminal {
                item_id: Some("background-1".to_owned()),
                process_id: Some("process-1".to_owned()),
                command: Some("cargo test".to_owned()),
            }],
            "2026-07-24T12:00:00Z",
            BackgroundSnapshotAuthority::Complete,
        );
        assert!(matches!(
            first.mutations.as_slice(),
            [ProviderActivityMutation::UpsertWorkItem(work_item)]
                if work_item.id == "codex:item:background-1"
                    && work_item.status == ActivityLifecycle::Running
        ));

        let disappeared = tracker.reconcile_background_terminals(
            &[],
            "2026-07-24T12:00:01Z",
            BackgroundSnapshotAuthority::Complete,
        );
        assert!(matches!(
            disappeared.mutations.as_slice(),
            [ProviderActivityMutation::UpsertWorkItem(work_item)]
                if work_item.id == "codex:item:background-1"
                    && work_item.name == "cargo test"
                    && work_item.status == ActivityLifecycle::Interrupted
        ));
        assert!(
            tracker
                .reconcile_background_terminals(
                    &[],
                    "2026-07-24T12:00:02Z",
                    BackgroundSnapshotAuthority::Complete,
                )
                .mutations
                .is_empty(),
            "an authoritative empty snapshot must terminalize a running item only once"
        );
    }

    #[test]
    fn partial_background_snapshot_upserts_prefix_without_interrupting_omissions() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.reconcile_background_terminals(
            &[ReconciliationBackgroundTerminal {
                item_id: Some("background-prior".to_owned()),
                process_id: Some("process-prior".to_owned()),
                command: Some("prior command".to_owned()),
            }],
            "2026-07-24T12:00:00Z",
            BackgroundSnapshotAuthority::Complete,
        );

        let partial = tracker.reconcile_background_terminals(
            &[ReconciliationBackgroundTerminal {
                item_id: Some("background-prefix".to_owned()),
                process_id: Some("process-prefix".to_owned()),
                command: Some("prefix command".to_owned()),
            }],
            "2026-07-24T12:00:01Z",
            BackgroundSnapshotAuthority::Partial,
        );

        assert!(matches!(
            partial.mutations.as_slice(),
            [ProviderActivityMutation::UpsertWorkItem(work_item)]
                if work_item.id == "codex:item:background-prefix"
                    && work_item.status == ActivityLifecycle::Running
        ));
        assert_eq!(
            tracker
                .work_items_by_native_id
                .get(&work_key("background-prior"))
                .map(|work_item| work_item.status),
            Some(ActivityLifecycle::Running)
        );
    }

    #[test]
    fn reconciliation_publishes_changed_list_and_read_history_without_resume_baseline() {
        fn thread(
            updated_at: u64,
            status: Value,
            nickname: &str,
            turns: Value,
        ) -> ReconciliationThread {
            decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "dormant-child",
                    "parentThreadId": "root-1",
                    "agentNickname": nickname,
                    "createdAt": 1_u64,
                    "updatedAt": updated_at,
                    "status": status,
                    "turns": turns,
                }
            }))
            .expect("official thread/read response")
            .thread
        }

        let initial = thread(
            1,
            serde_json::json!({"type": "active", "activeFlags": []}),
            "Dormant child",
            serde_json::json!([]),
        );
        let mut list_tracker = CodexActivityTracker::new(Some("root-1"));
        list_tracker.seed_actor("root-1");
        list_tracker.begin_detail_baseline();
        list_tracker.reconcile_descendants(std::slice::from_ref(&initial));
        list_tracker.finish_detail_baseline();

        let changed_list_thread = thread(
            2,
            serde_json::json!({"type": "notLoaded"}),
            "Changed only by reconciliation",
            serde_json::json!([]),
        );
        let changed_list =
            list_tracker.reconcile_descendants(std::slice::from_ref(&changed_list_thread));

        let mut read_tracker = CodexActivityTracker::new(Some("root-1"));
        read_tracker.seed_actor("root-1");
        read_tracker.begin_detail_baseline();
        read_tracker.reconcile_descendants(std::slice::from_ref(&initial));
        read_tracker.reconcile_thread_history(&initial);
        read_tracker.finish_detail_baseline();

        let changed_read_thread = thread(
            2,
            serde_json::json!({"type": "idle"}),
            "Dormant child",
            serde_json::json!([{
                "id": "dormant-turn",
                "status": "completed",
                "startedAt": 1_u64,
                "completedAt": 2_u64,
                "items": [{
                    "type": "agentMessage",
                    "id": "dormant-message",
                    "text": "reconciliation is not live evidence"
                }]
            }]),
        );
        let changed_read = read_tracker.reconcile_thread_history(&changed_read_thread);

        assert!(
            !changed_list.output.mutations.is_empty() && !changed_read.mutations.is_empty(),
            "reconciliation was unexpectedly suppressed: list={:?}; read={:?}",
            changed_list.output.mutations,
            changed_read.mutations
        );
    }

    #[test]
    fn live_actor_transition_is_published_after_reconciliation() {
        let decoded = decode_thread_list_response(serde_json::json!({
            "data": [{
                "id": "terminal-parent",
                "parentThreadId": "root-1",
                "createdAt": 1_u64,
                "updatedAt": 2_u64,
                "status": {"type": "idle"}
            }, {
                "id": "dormant-child",
                "parentThreadId": "terminal-parent",
                "createdAt": 1_u64,
                "updatedAt": 1_u64,
                "status": {"type": "notLoaded"}
            }],
            "nextCursor": null
        }))
        .expect("official thread/list response");
        let terminal_parent = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "terminal-parent",
                "parentThreadId": "root-1",
                "createdAt": 1_u64,
                "updatedAt": 2_u64,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "terminal-parent-turn",
                    "status": "completed",
                    "startedAt": 1_u64,
                    "completedAt": 2_u64,
                    "items": []
                }]
            }
        }))
        .expect("official thread/read response");

        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("root-1");
        tracker.begin_detail_baseline();
        tracker.reconcile_descendants(&decoded.data);
        tracker.reconcile_thread_history(&terminal_parent.thread);
        tracker.finish_detail_baseline();

        let duplicate_terminal_envelope = serde_json::json!({
            "method": "thread/status/changed",
            "params": {
                "threadId": "terminal-parent",
                "status": {"type": "idle"}
            }
        });
        let duplicate_terminal = tracker.handle_envelope(&duplicate_terminal_envelope, 1);

        let child_transition_envelope = serde_json::json!({
            "method": "thread/status/changed",
            "params": {
                "threadId": "dormant-child",
                "status": {"type": "active", "activeFlags": []}
            }
        });
        let child_transition = tracker.handle_envelope(&child_transition_envelope, 2);

        assert!(
            !child_transition.mutations.is_empty(),
            "live actor transition was suppressed: duplicate={:?}; child={:?}",
            duplicate_terminal.mutations,
            child_transition.mutations
        );
    }

    #[test]
    fn background_reconciliation_publishes_lifecycle_without_resume_baseline() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("root-1");
        tracker.begin_detail_baseline();
        let active = tracker.reconcile_background_terminals(
            &[ReconciliationBackgroundTerminal {
                item_id: Some("dormant-background".to_owned()),
                process_id: Some("process-1".to_owned()),
                command: Some("cargo test".to_owned()),
            }],
            "2026-07-30T12:00:00Z",
            BackgroundSnapshotAuthority::Complete,
        );
        tracker.finish_detail_baseline();

        let terminal = tracker.reconcile_background_terminals(
            &[],
            "2026-07-30T12:00:02Z",
            BackgroundSnapshotAuthority::Complete,
        );

        assert!(
            !active.mutations.is_empty() && !terminal.mutations.is_empty(),
            "background reconciliation was unexpectedly suppressed: active={:?}; terminal={:?}",
            active.mutations,
            terminal.mutations
        );
    }

    #[test]
    fn recovered_terminal_turn_states_match_live_entry_identity_and_tone() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "root-1",
                "createdAt": 1,
                "updatedAt": 5,
                "status": {"type": "idle"},
                "turns": [
                    {
                        "id": "completed-turn",
                        "status": "completed",
                        "startedAt": 1,
                        "completedAt": 2,
                        "items": []
                    },
                    {
                        "id": "failed-turn",
                        "status": "failed",
                        "error": {"message": "provider failed"},
                        "startedAt": 2,
                        "completedAt": 3,
                        "items": []
                    },
                    {
                        "id": "interrupted-turn",
                        "status": "interrupted",
                        "startedAt": 3,
                        "completedAt": 4,
                        "items": []
                    },
                    {
                        "id": "cancelled-turn",
                        "status": "cancelled",
                        "startedAt": 4,
                        "completedAt": 5,
                        "items": []
                    }
                ]
            }
        }))
        .expect("official thread/read response");
        let mut recovered_tracker = CodexActivityTracker::new(Some("root-1"));
        recovered_tracker.seed_actor("child-1");
        let recovered = recovered_tracker.reconcile_thread_history(&response.thread);

        let mut live_tracker = CodexActivityTracker::new(Some("root-1"));
        live_tracker.seed_actor("child-1");
        let live = [
            ("completed-turn", "completed", None, 2_u64),
            ("failed-turn", "failed", Some("provider failed"), 3_u64),
            ("interrupted-turn", "interrupted", None, 4_u64),
            ("cancelled-turn", "cancelled", None, 5_u64),
        ]
        .into_iter()
        .flat_map(|(turn_id, status, error, completed_at)| {
            live_tracker
                .handle_notification(
                    "turn/completed",
                    &serde_json::json!({
                        "threadId": "child-1",
                        "turn": {
                            "id": turn_id,
                            "status": status,
                            "error": error.map(|message| serde_json::json!({"message": message})),
                            "completedAt": completed_at
                        }
                    }),
                    completed_at.saturating_mul(1_000),
                    completed_at.into(),
                )
                .mutations
        })
        .collect::<Vec<_>>();
        let stable_entry_view = |mutations: &[ProviderActivityMutation]| {
            mutations
                .iter()
                .filter_map(|mutation| match mutation {
                    ProviderActivityMutation::AppendEntry(entry) => Some((
                        entry.id.clone(),
                        entry.kind,
                        entry.title.clone(),
                        entry.detail.clone(),
                        entry.tone,
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            stable_entry_view(&recovered.mutations),
            stable_entry_view(&live)
        );
        assert_eq!(stable_entry_view(&recovered.mutations).len(), 4);
        assert!(stable_entry_view(&recovered.mutations).len() <= MAX_RECONCILED_ENTRIES);
        assert!(recovered.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.status == ActivityLifecycle::Cancelled
        )));
    }

    #[test]
    fn recovered_history_repairs_seeded_actor_start_from_thread_created_at_seconds() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "terminal-root",
                "parentThreadId": null,
                "createdAt": 1_785_235_917_u64,
                "updatedAt": 1_785_235_920_u64,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "completed-turn",
                    "status": "completed",
                    "startedAt": 1_785_235_917_u64,
                    "completedAt": 1_785_235_920_u64,
                    "items": []
                }]
            }
        }))
        .expect("official thread/read response");
        let mut tracker = CodexActivityTracker::new(Some("terminal-root"));
        tracker.seed_actor("terminal-root");
        let synthetic = tracker.handle_notification(
            "thread/status/changed",
            &serde_json::json!({
                "threadId": "terminal-root",
                "status": {"type": "active", "activeFlags": []},
            }),
            1_785_235_918_000_u64,
            0,
        );
        assert!(synthetic.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.started_at == "1970-01-01T00:00:00.000000000Z"
        )));

        let recovered = tracker.reconcile_thread_history(&response.thread);

        let actor = recovered
            .mutations
            .iter()
            .rev()
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertActor(actor) => Some(actor),
                _ => None,
            })
            .expect("recovered root actor");
        assert_eq!(actor.started_at, "2026-07-28T10:51:57.000000000Z");
        assert_eq!(actor.updated_at, "2026-07-28T10:52:00.000000000Z");
        assert_eq!(
            actor.terminal_at.as_deref(),
            Some("2026-07-28T10:52:00.000000000Z")
        );
    }

    #[test]
    fn contradictory_recovered_creation_time_does_not_corrupt_terminal_actor() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "terminal-root",
                "parentThreadId": null,
                "createdAt": 1_785_235_920_u64,
                "updatedAt": 1_785_235_920_u64,
                "status": { "type": "idle" },
                "turns": []
            }
        }))
        .expect("official thread/read response");
        let mut tracker = CodexActivityTracker::new(Some("terminal-root"));
        tracker.seed_actor("terminal-root");
        assert!(
            tracker
                .upsert_actor_state(
                    "terminal-root",
                    None,
                    None,
                    None,
                    ActivityLifecycle::Completed,
                    None,
                    "2026-07-28T10:51:59Z",
                    true,
                    ActorReopenAuthority::None,
                )
                .is_some(),
            "the seeded actor must first reach a valid terminal state"
        );

        let recovered = tracker.reconcile_thread_history(&response.thread);

        assert!(
            recovered.mutations.is_empty(),
            "contradictory recovered chronology must not emit an actor mutation"
        );
        let actor = tracker
            .actors_by_thread
            .get(&thread_key("terminal-root"))
            .expect("tracked root actor");
        assert_eq!(actor.started_at, "1970-01-01T00:00:00.000000000Z");
        assert_eq!(actor.updated_at, "2026-07-28T10:51:59.000000000Z");
        assert_eq!(
            actor.terminal_at.as_deref(),
            Some("2026-07-28T10:51:59.000000000Z")
        );
        assert!(
            actor.to_summary().is_some(),
            "rejected recovery must leave the prior valid actor intact"
        );
    }

    #[test]
    fn reconciliation_requires_child_and_turn_creation_boundaries() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "root-1",
                "createdAt": 1_784_898_792_u64,
                "updatedAt": 1_784_898_793_u64,
                "status": {"type": "idle"},
                "turns": [
                    {
                        "id": "inherited-root-turn",
                        "status": "completed",
                        "startedAt": 1_784_898_737_u64,
                        "completedAt": 1_784_898_738_u64,
                        "items": [{
                            "type": "agentMessage",
                            "id": "root-message",
                            "text": "inherited root commentary"
                        }]
                    },
                    {
                        "id": "undated-inherited-turn",
                        "status": "completed",
                        "completedAt": 1_784_898_791_u64,
                        "items": [{
                            "type": "agentMessage",
                            "id": "undated-message",
                            "text": "undated ancestor commentary"
                        }]
                    },
                    {
                        "id": "own-child-turn",
                        "status": "completed",
                        "startedAt": 1_784_898_792_u64,
                        "completedAt": 1_784_898_793_u64,
                        "items": [{
                            "type": "agentMessage",
                            "id": "child-message",
                            "text": "own full final commentary"
                        }]
                    }
                ]
            }
        }))
        .expect("official thread/read response");
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("child-1");

        let output = tracker.reconcile_thread_history(&response.thread);
        let commentary = output
            .mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.kind == ActivityEntryKind::Commentary =>
                {
                    entry.detail.as_deref()
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(commentary, ["own full final commentary"]);
        assert!(output.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == "codex:thread:child-1"
                    && actor.status == ActivityLifecycle::Completed
        )));

        let mut missing_created_at = response.thread.clone();
        missing_created_at.created_at = None;
        let mut untrusted_tracker = CodexActivityTracker::new(Some("root-1"));
        untrusted_tracker.seed_actor("child-1");
        assert!(
            untrusted_tracker
                .reconcile_thread_history(&missing_created_at)
                .mutations
                .is_empty(),
            "history without a child creation boundary must fail closed"
        );
    }

    #[test]
    fn authoritative_active_snapshot_obeys_terminal_reopen_chronology() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "root-1",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "child-turn",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": []
                }]
            }
        }))
        .expect("official thread/read response");
        let terminal_tracker = || {
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.reconcile_descendants(std::slice::from_ref(&response.thread));
            tracker.reconcile_thread_history(&response.thread);
            tracker
        };
        let active_thread = |updated_at| {
            let mut thread = response.thread.clone();
            thread.status = Some(ReconciliationThreadStatus::Active {
                active_flags: Vec::new(),
            });
            thread.updated_at = Some(updated_at);
            thread
        };

        let mut older_tracker = terminal_tracker();
        let older_output = older_tracker.reconcile_descendants(&[active_thread(1)]);
        assert!(
            older_output
                .output
                .mutations
                .iter()
                .all(|mutation| !matches!(
                    mutation,
                    ProviderActivityMutation::UpsertActor(actor)
                        if actor.status == ActivityLifecycle::Running
                )),
            "an older Active snapshot must not reopen a terminal actor"
        );

        let mut equal_tracker = terminal_tracker();
        let equal_output = equal_tracker.reconcile_descendants(&[active_thread(2)]);
        assert!(
            equal_output
                .output
                .mutations
                .iter()
                .any(|mutation| matches!(
                    mutation,
                    ProviderActivityMutation::UpsertActor(actor)
                        if actor.status == ActivityLifecycle::Running
                            && actor.terminal_at.is_none()
                ))
        );
    }

    #[test]
    fn live_and_reconciled_history_deduplicate_semantically_identical_commentary() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "root-1",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "turn-1",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": [{
                        "type": "agentMessage",
                        "id": "msg_history",
                        "text": "same result"
                    }]
                }]
            }
        }))
        .expect("official thread/read response");

        let mut live_first = CodexActivityTracker::new(Some("root-1"));
        live_first.seed_actor("child-1");
        let live = live_first.handle_notification(
            "item/agentMessage/delta",
            &serde_json::json!({
                "threadId": "child-1",
                "turnId": "turn-1",
                "itemId": "item-2",
                "delta": "same "
            }),
            1_000,
            0,
        );
        assert!(live.mutations.is_empty());
        let completed = live_first.handle_notification(
            "item/completed",
            &serde_json::json!({
                "threadId": "child-1",
                "turnId": "turn-1",
                "item": {
                    "type": "agentMessage",
                    "id": "item-2",
                    "text": "same result"
                }
            }),
            2_000,
            1,
        );
        assert!(matches!(
            completed.mutations.as_slice(),
            [ProviderActivityMutation::AppendEntry(entry)]
                if entry.id.starts_with("codex:event:commentary:")
                    && entry.detail.as_deref() == Some("same result")
        ));
        let live_commentary_id = completed
            .mutations
            .iter()
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.kind == ActivityEntryKind::Commentary =>
                {
                    Some(entry.id.clone())
                }
                _ => None,
            })
            .expect("live commentary entry");
        let repaired = live_first.reconcile_thread_history(&response.thread);
        let repaired_entries = repaired
            .mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ProviderActivityMutation::AppendEntry(entry) => Some(entry),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            repaired_entries.as_slice(),
            [entry]
                if entry.id
                    == "codex:event:turn-completed:child-1:turn-1:turn-1:completed"
                    && entry.title == "Turn completed"
        ));

        let mut reconciliation_first = CodexActivityTracker::new(Some("root-1"));
        reconciliation_first.seed_actor("child-1");
        let recovered = reconciliation_first.reconcile_thread_history(&response.thread);
        assert!(
            recovered
                .mutations
                .iter()
                .any(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
        );
        let recovered_commentary_id = recovered
            .mutations
            .iter()
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.kind == ActivityEntryKind::Commentary =>
                {
                    Some(entry.id.clone())
                }
                _ => None,
            })
            .expect("recovered commentary entry");
        assert_eq!(
            recovered_commentary_id, live_commentary_id,
            "live and recovered commentary must retain one durable identity across restarts"
        );
        assert!(
            reconciliation_first
                .handle_notification(
                    "item/completed",
                    &serde_json::json!({
                        "threadId": "child-1",
                        "turnId": "turn-1",
                        "item": {
                            "type": "agentMessage",
                            "id": "item-2",
                            "text": "same result"
                        }
                    }),
                    2_000,
                    1,
                )
                .mutations
                .is_empty(),
            "late live completion must not duplicate an item recovered from history"
        );
    }

    #[test]
    fn retained_work_items_reject_terminal_state_corrections() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        let completed = tracker.upsert_work_item_state(
            "work-1",
            None,
            "Background work".to_owned(),
            ActivityLifecycle::Completed,
            Some("done".to_owned()),
            "2026-07-24T12:00:00Z",
        );
        let late_failure = tracker.upsert_work_item_state(
            "work-1",
            None,
            "Background work".to_owned(),
            ActivityLifecycle::Failed,
            Some("late correction".to_owned()),
            "2026-07-24T12:00:01Z",
        );

        assert!(completed.is_some());
        assert!(
            late_failure.is_none(),
            "non-authoritative terminal updates must not rewrite terminal work"
        );
    }

    #[test]
    fn retained_native_keys_are_byte_bounded() {
        let oversized_thread = format!("child:{}", "x".repeat(32_000));
        let oversized_turn = format!("turn:{}", "y".repeat(32_000));
        let oversized_item = format!("item:{}", "z".repeat(32_000));
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor(&oversized_thread);
        let params = serde_json::json!({
            "threadId": oversized_thread,
            "turnId": oversized_turn,
            "itemId": oversized_item,
            "delta": "visible"
        });

        tracker.handle_notification("item/agentMessage/delta", &params, 1_000, 0);

        assert!(
            tracker
                .actors_by_thread
                .keys()
                .chain(tracker.pending_deltas.keys())
                .chain(tracker.seen_native_events.values.iter())
                .all(|key| key.len() <= MAX_RETAINED_KEY_BYTES),
            "every retained native/event key must have a fixed byte bound"
        );
    }

    #[test]
    fn completed_delta_tombstones_remain_bounded() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("child-1");
        for index in 0..(MAX_SEEN_EVENTS + 500) {
            tracker.handle_notification(
                "item/completed",
                &serde_json::json!({
                    "threadId": "child-1",
                    "turnId": "turn-1",
                    "item": {
                        "id": format!("item-{index}"),
                        "type": "agentMessage",
                        "text": "done"
                    }
                }),
                index as u64,
                index as u128,
            );
        }

        assert_eq!(tracker.completed_delta_streams.len(), MAX_SEEN_EVENTS);
    }

    #[test]
    fn reasoning_replay_identity_includes_summary_index() {
        let first = delta_replay_key("item/reasoning/summaryTextDelta", "stream", 42, Some(0));
        let second = delta_replay_key("item/reasoning/summaryTextDelta", "stream", 42, Some(1));

        assert_ne!(first, second);
    }

    #[test]
    fn canonical_hash_namespace_cannot_alias_a_native_id() {
        let generator = CanonicalIdGenerator::default();
        let hashed = generator
            .resolve("codex:thread:", "contains whitespace")
            .expect("hashed canonical id");
        let reserved_native = hashed.trim_start_matches("codex:thread:");
        let direct = generator
            .resolve("codex:thread:", reserved_native)
            .expect("reserved-looking native id");

        assert_ne!(hashed, direct);
    }

    #[test]
    fn canonical_fallbacks_are_order_independent_under_primary_digest_collision() {
        fn colliding_digest(native: &[u8]) -> CanonicalDigest {
            let secondary = if native == b"first value" {
                [1; 32]
            } else {
                [2; 32]
            };
            CanonicalDigest {
                primary: [7; 32],
                secondary,
            }
        }

        let generator = CanonicalIdGenerator::with_digest(colliding_digest);
        let first_forward = generator
            .resolve("codex:thread:", "first value")
            .expect("first forward");
        let second_forward = generator
            .resolve("codex:thread:", "second value")
            .expect("second forward");
        let second_reverse = generator
            .resolve("codex:thread:", "second value")
            .expect("second reverse");
        let first_reverse = generator
            .resolve("codex:thread:", "first value")
            .expect("first reverse");

        assert_eq!(first_forward, first_reverse);
        assert_eq!(second_forward, second_reverse);
        assert_ne!(first_forward, second_forward);
    }

    #[test]
    fn canonical_fallbacks_do_not_depend_on_evictable_history() {
        fn colliding_digest(native: &[u8]) -> CanonicalDigest {
            let secondary: [u8; 32] = Sha256::digest(native).into();
            CanonicalDigest {
                primary: [7; 32],
                secondary,
            }
        }

        let generator = CanonicalIdGenerator::with_digest(colliding_digest);
        let live_id = generator
            .resolve("codex:thread:", "live actor")
            .expect("live actor id");
        for index in 0..10_000 {
            let _ = generator.resolve("codex:thread:", &format!("other actor {index}"));
        }
        let live_id_after_pressure = generator
            .resolve("codex:thread:", "live actor")
            .expect("live actor id after pressure");
        let colliding_id = generator
            .resolve("codex:thread:", "colliding actor")
            .expect("colliding actor id");

        assert_eq!(live_id, live_id_after_pressure);
        assert_ne!(live_id, colliding_id);
    }

    #[test]
    fn root_command_start_requests_background_terminal_reconciliation() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        let output = tracker.handle_notification(
            "item/started",
            &serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "command-1",
                    "type": "commandExecution",
                    "status": "inProgress",
                    "command": "sleep 600"
                }
            }),
            1_000,
            0,
        );

        assert!(output.request_reconciliation);
        assert!(output.mutations.is_empty());
    }

    #[test]
    fn root_command_completion_requests_background_terminal_reconciliation() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        let output = tracker.handle_notification(
            "item/completed",
            &serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "command-1",
                    "type": "commandExecution",
                    "status": "completed",
                    "command": "sleep 600"
                }
            }),
            2_000,
            1,
        );

        assert!(output.request_reconciliation);
        assert!(output.mutations.is_empty());
    }

    #[test]
    fn unconfigured_tracker_does_not_trust_notification_sender_as_root() {
        let mut tracker = CodexActivityTracker::new(None);
        let params = serde_json::json!({
            "threadId": "foreign-root",
            "turnId": "turn-1",
            "item": {
                "id": "spawn-1",
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "status": "inProgress",
                "senderThreadId": "foreign-root",
                "receiverThreadIds": ["foreign-child"],
                "agentsStates": {}
            }
        });

        let output = tracker.handle_notification("item/started", &params, 1_000, 0);

        assert!(output.mutations.is_empty());
        assert!(tracker.root_thread_id.is_none());
    }

    #[test]
    fn sub_agent_activity_seeds_canonical_actor_and_read_hint() {
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");

        let output = tracker.handle_notification(
            "item/started",
            &serde_json::json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-child",
                    "type": "subAgentActivity",
                    "agentThreadId": "child-1",
                    "agentPath": "/root/reviewer",
                    "kind": "started"
                }
            }),
            2_000,
            1,
        );

        assert!(matches!(
            output.mutations.as_slice(),
            [ProviderActivityMutation::UpsertActor(actor)]
                if actor.id == "codex:thread:child-1"
                    && actor.name == "reviewer"
                    && actor.parent_actor_id.is_none()
                    && actor.status == ActivityLifecycle::Running
                    && actor.started_at == "1970-01-01T00:00:02.000000000Z"
                    && actor.updated_at == "1970-01-01T00:00:02.000000000Z"
                    && actor.terminal_at.is_none()
        ));
        assert_eq!(output.hinted_descendant_ids, ["child-1"]);
        assert!(output.request_reconciliation);
    }

    #[test]
    fn sub_agent_activity_preserves_topology_lifecycle_and_deduplicates() {
        let valid_child = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 1,
                "status": {"type": "active", "activeFlags": []},
                "turns": []
            }
        }))
        .expect("valid child")
        .thread;
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");
        tracker.reconcile_descendants(&[valid_child]);

        let nested = serde_json::json!({
            "threadId": "child-1",
            "turnId": "child-turn",
            "item": {
                "id": "spawn-nested",
                "type": "subAgentActivity",
                "agentThreadId": "child-nested",
                "agentPath": "/root/reviewer/nested-reviewer",
                "kind": "interacted"
            }
        });
        let nested_output = tracker.handle_notification("item/started", &nested, 3_000, 2);
        let nested_actor = nested_output
            .mutations
            .iter()
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertActor(actor) => Some(actor),
                _ => None,
            })
            .expect("nested provisional actor");
        assert_eq!(
            nested_actor.parent_actor_id.as_deref(),
            Some("codex:thread:child-1")
        );

        let interrupted_output = tracker.handle_notification(
            "item/completed",
            &serde_json::json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "interrupt-child",
                    "type": "subAgentActivity",
                    "agentThreadId": "interrupted-child",
                    "agentPath": "/root/interrupted-child",
                    "kind": "interrupted"
                }
            }),
            4_000,
            3,
        );
        let interrupted_actor = interrupted_output
            .mutations
            .iter()
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertActor(actor) => Some(actor),
                _ => None,
            })
            .expect("interrupted provisional actor");
        assert_eq!(interrupted_actor.status, ActivityLifecycle::Interrupted);
        assert_eq!(
            interrupted_actor.terminal_at,
            Some(interrupted_actor.updated_at.clone())
        );

        let duplicate = tracker.handle_notification("item/started", &nested, 3_000, 4);
        assert!(duplicate.mutations.is_empty());
        assert!(duplicate.hinted_descendant_ids.is_empty());
        assert!(!duplicate.request_reconciliation);
    }

    #[test]
    fn sub_agent_activity_without_provider_timestamp_cannot_reopen_interrupted_actor() {
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");
        let hint = |kind: &str, id: &str| {
            serde_json::json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": id,
                    "type": "subAgentActivity",
                    "agentThreadId": "child-1",
                    "agentPath": "/root/reviewer",
                    "kind": kind
                }
            })
        };

        let interrupted = tracker.handle_notification(
            "item/completed",
            &hint("interrupted", "interrupt-child"),
            0,
            1,
        );
        assert!(matches!(
            interrupted.mutations.as_slice(),
            [ProviderActivityMutation::UpsertActor(actor)]
                if actor.status == ActivityLifecycle::Interrupted
        ));

        let timestamp_less_reopen =
            tracker.handle_notification("item/started", &hint("started", "restart-child"), 0, 2);
        assert!(timestamp_less_reopen.mutations.is_empty());
        assert!(timestamp_less_reopen.hinted_descendant_ids.is_empty());
        assert!(!timestamp_less_reopen.request_reconciliation);
        assert_eq!(
            tracker
                .actors_by_thread
                .get(&thread_key("child-1"))
                .map(|actor| actor.status),
            Some(ActivityLifecycle::Interrupted)
        );
    }

    #[test]
    fn sub_agent_reconciliation_materializes_provisional_metadata_before_promotion() {
        let thread =
            |id: &str, parent_thread_id: &str, created_at: u64, updated_at: u64, status: Value| {
                decode_thread_read_response(serde_json::json!({
                    "thread": {
                        "id": id,
                        "parentThreadId": parent_thread_id,
                        "agentNickname": "reviewer",
                        "createdAt": created_at,
                        "updatedAt": updated_at,
                        "status": status,
                        "turns": []
                    }
                }))
                .expect("reconciliation thread")
                .thread
            };
        let active = || serde_json::json!({"type": "active", "activeFlags": []});

        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");
        tracker.reconcile_descendants(&[thread("parent-1", "provider-root", 1, 1, active())]);
        tracker.handle_notification(
            "item/started",
            &serde_json::json!({
                "threadId": "parent-1",
                "turnId": "parent-turn",
                "item": {
                    "id": "spawn-child",
                    "type": "subAgentActivity",
                    "agentThreadId": "child-1",
                    "agentPath": "/root/parent/reviewer",
                    "kind": "started"
                }
            }),
            2_000,
            1,
        );

        let authoritative =
            tracker.reconcile_descendants(&[thread("child-1", "provider-root", 1, 3, active())]);
        assert_eq!(authoritative.accepted_thread_ids, ["child-1"]);
        assert!(matches!(
            authoritative.output.mutations.as_slice(),
            [ProviderActivityMutation::UpsertActor(actor)]
                if actor.id == "codex:thread:child-1"
                    && actor.parent_actor_id.is_none()
                    && actor.status == ActivityLifecycle::Running
                    && actor.started_at == "1970-01-01T00:00:01.000000000Z"
                    && actor.updated_at == "1970-01-01T00:00:03.000000000Z"
        ));
        assert!(tracker.is_verified_child("child-1"));

        let mut stale_tracker = CodexActivityTracker::new(Some("provider-root"));
        stale_tracker.seed_actor("provider-root");
        stale_tracker.handle_notification(
            "item/completed",
            &serde_json::json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "interrupt-stale-child",
                    "type": "subAgentActivity",
                    "agentThreadId": "stale-child",
                    "agentPath": "/root/reviewer",
                    "kind": "interrupted"
                }
            }),
            4_000,
            1,
        );
        let stale = stale_tracker.reconcile_descendants(&[thread(
            "stale-child",
            "provider-root",
            1,
            2,
            active(),
        )]);
        assert_eq!(stale.accepted_thread_ids, ["stale-child"]);
        assert!(matches!(
            stale.output.mutations.as_slice(),
            [ProviderActivityMutation::UpsertActor(actor)]
                if actor.id == "codex:thread:stale-child"
                    && actor.status == ActivityLifecycle::Interrupted
                    && actor.started_at == "1970-01-01T00:00:01.000000000Z"
                    && actor.updated_at == "1970-01-01T00:00:04.000000000Z"
                    && actor.terminal_at.as_deref()
                        == Some("1970-01-01T00:00:04.000000000Z")
        ));
        assert!(stale_tracker.is_verified_child("stale-child"));
    }

    #[test]
    fn sub_agent_activity_rejects_malformed_unknown_and_foreign_hints() {
        fn assert_default(output: CodexActivityOutput) {
            assert!(output.mutations.is_empty());
            assert!(output.hinted_descendant_ids.is_empty());
            assert!(!output.request_reconciliation);
        }

        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("verified-child");
        let valid_item = serde_json::json!({
            "id": "sub-agent-1",
            "type": "subAgentActivity",
            "agentThreadId": "discovered-child",
            "agentPath": "/root/discovered-child",
            "kind": "started"
        });
        let invalid_params = [
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-missing-thread",
                    "type": "subAgentActivity",
                    "agentPath": "/root/discovered-child",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-thread-with-whitespace",
                    "type": "subAgentActivity",
                    "agentThreadId": "child one",
                    "agentPath": "/root/discovered-child",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-empty-thread",
                    "type": "subAgentActivity",
                    "agentThreadId": "",
                    "agentPath": "/root/discovered-child",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-whitespace-thread",
                    "type": "subAgentActivity",
                    "agentThreadId": " ",
                    "agentPath": "/root/discovered-child",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-missing-path",
                    "type": "subAgentActivity",
                    "agentThreadId": "discovered-child",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-empty-path",
                    "type": "subAgentActivity",
                    "agentThreadId": "discovered-child",
                    "agentPath": "",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-whitespace-path",
                    "type": "subAgentActivity",
                    "agentThreadId": "discovered-child",
                    "agentPath": " ",
                    "kind": "started"
                }
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-unknown-kind",
                    "type": "subAgentActivity",
                    "agentThreadId": "discovered-child",
                    "agentPath": "/root/discovered-child",
                    "kind": "completed"
                }
            }),
            serde_json::json!({
                "threadId": "foreign-thread",
                "turnId": "turn-1",
                "item": valid_item
            }),
            serde_json::json!({
                "threadId": "root-1",
                "turnId": "turn-1",
                "item": {
                    "id": "sub-agent-root-self-link",
                    "type": "subAgentActivity",
                    "agentThreadId": "root-1",
                    "agentPath": "/root",
                    "kind": "started"
                }
            }),
        ];

        for params in invalid_params {
            assert_default(tracker.handle_notification("item/completed", &params, 1_000, 0));
        }

        let provisional = tracker.handle_notification(
            "item/started",
            &serde_json::json!({
                "threadId": "root-1",
                "turnId": "root-turn",
                "item": {
                    "id": "provisional-owner",
                    "type": "subAgentActivity",
                    "agentThreadId": "provisional-owner",
                    "agentPath": "/root/provisional-owner",
                    "kind": "started"
                }
            }),
            2_000,
            1,
        );
        assert_eq!(provisional.hinted_descendant_ids, ["provisional-owner"]);
        assert_default(tracker.handle_notification(
            "item/started",
            &serde_json::json!({
                "threadId": "provisional-owner",
                "turnId": "provisional-turn",
                "item": {
                    "id": "unverified-owner-hint",
                    "type": "subAgentActivity",
                    "agentThreadId": "nested-unverified",
                    "agentPath": "/root/provisional-owner/nested-unverified",
                    "kind": "started"
                }
            }),
            3_000,
            2,
        ));

        let nested_thread = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "nested-verified",
                "parentThreadId": "verified-child",
                "createdAt": 1,
                "updatedAt": 1,
                "turns": []
            }
        }))
        .expect("nested verified child")
        .thread;
        tracker.reconcile_descendants(&[nested_thread]);
        assert_default(tracker.handle_notification(
            "item/started",
            &serde_json::json!({
                "threadId": "nested-verified",
                "turnId": "nested-turn",
                "item": {
                    "id": "parent-cycle",
                    "type": "subAgentActivity",
                    "agentThreadId": "verified-child",
                    "agentPath": "/root/verified-child",
                    "kind": "started"
                }
            }),
            4_000,
            3,
        ));
    }

    #[test]
    fn sub_agent_history_recovers_bounded_root_and_nested_hints() {
        let root = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "provider-root",
                "updatedAt": 30,
                "turns": [{
                    "id": "root-turn",
                    "startedAt": 10,
                    "completedAt": 20,
                    "items": [{
                        "id": "spawn-child",
                        "type": "subAgentActivity",
                        "agentThreadId": "child-1",
                        "agentPath": "/root/reviewer",
                        "kind": "started"
                    }]
                }]
            }
        }))
        .expect("root history")
        .thread;
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");
        let root_output = tracker.reconcile_sub_agent_hints(&root);
        assert_eq!(root_output.hinted_descendant_ids, ["child-1"]);
        assert!(root_output.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == "codex:thread:child-1"
                    && actor.updated_at == "1970-01-01T00:00:20.000000000Z"
        )));

        let mut child = root.clone();
        child.id = Some("child-1".to_owned());
        child.parent_thread_id = Some("provider-root".to_owned());
        child.created_at = Some(10);
        child.turns[0].items = serde_json::from_value(serde_json::json!([{
            "id": "spawn-nested",
            "type": "subAgentActivity",
            "agentThreadId": "child-nested",
            "agentPath": "/root/reviewer/nested-reviewer",
            "kind": "interacted"
        }]))
        .expect("typed nested hint");
        tracker.reconcile_descendants(std::slice::from_ref(&child));
        let nested_output = tracker.reconcile_sub_agent_hints(&child);
        assert_eq!(nested_output.hinted_descendant_ids, ["child-nested"]);
        assert!(nested_output.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == "codex:thread:child-nested"
                    && actor.parent_actor_id.as_deref() == Some("codex:thread:child-1")
        )));

        let turns = (0..21)
            .map(|index| {
                serde_json::json!({
                    "id": format!("turn-{index}"),
                    "startedAt": index + 1,
                    "items": if index == 0 {
                        serde_json::json!([{
                            "id": "too-old",
                            "type": "subAgentActivity",
                            "agentThreadId": "too-old-child",
                            "agentPath": "/root/too-old-child",
                            "kind": "started"
                        }])
                    } else if index == 20 {
                        serde_json::json!([{
                            "id": "newest",
                            "type": "subAgentActivity",
                            "agentThreadId": "newest-child",
                            "agentPath": "/root/newest-child",
                            "kind": "started"
                        }])
                    } else {
                        serde_json::json!([])
                    }
                })
            })
            .collect::<Vec<_>>();
        let bounded = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "provider-root",
                "updatedAt": 30,
                "turns": turns
            }
        }))
        .expect("bounded root history")
        .thread;
        let mut bounded_tracker = CodexActivityTracker::new(Some("provider-root"));
        bounded_tracker.seed_actor("provider-root");
        let bounded_output = bounded_tracker.reconcile_sub_agent_hints(&bounded);
        assert_eq!(bounded_output.hinted_descendant_ids, ["newest-child"]);
        assert!(!bounded_tracker.is_verified_child("too-old-child"));

        let bounded_hints = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "provider-root",
                "updatedAt": 30,
                "turns": [{
                    "id": "bounded-hints",
                    "startedAt": 30,
                    "items": (0..(MAX_RECONCILED_DESCENDANTS + 10))
                        .map(|index| serde_json::json!({
                            "id": format!("hint-{index}"),
                            "type": "subAgentActivity",
                            "agentThreadId": format!("bounded-child-{index}"),
                            "agentPath": format!("/root/bounded-child-{index}"),
                            "kind": "started"
                        }))
                        .collect::<Vec<_>>()
                }]
            }
        }))
        .expect("bounded historical hints")
        .thread;
        let mut hint_bound_tracker = CodexActivityTracker::new(Some("provider-root"));
        hint_bound_tracker.seed_actor("provider-root");
        let hint_bound_output = hint_bound_tracker.reconcile_sub_agent_hints(&bounded_hints);
        assert_eq!(
            hint_bound_output.hinted_descendant_ids.len(),
            MAX_RECONCILED_DESCENDANTS
        );
        assert!(hint_bound_output.mutations.len() <= MAX_MUTATIONS_PER_OUTPUT);
    }

    #[test]
    fn sub_agent_history_projection_limit_retains_deferred_read_hints_without_actor_mutations() {
        let thread = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "provider-root",
                "updatedAt": 3,
                "turns": [{
                    "id": "root-turn",
                    "startedAt": 1,
                    "completedAt": 3,
                    "items": (0..3)
                        .map(|index| serde_json::json!({
                            "id": format!("spawn-{index}"),
                            "type": "subAgentActivity",
                            "agentThreadId": format!("child-{index}"),
                            "agentPath": format!("/root/child-{index}"),
                            "kind": "started"
                        }))
                        .collect::<Vec<_>>()
                }]
            }
        }))
        .expect("root history")
        .thread;
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");

        let output = tracker.reconcile_sub_agent_hints_with_projection_limit(&thread, 1);

        assert_eq!(
            output.hinted_descendant_ids,
            ["child-0", "child-1", "child-2"]
        );
        assert!(matches!(
            output.mutations.as_slice(),
            [ProviderActivityMutation::UpsertActor(actor)]
                if actor.id == "codex:thread:child-0"
        ));
        assert!(!tracker.is_verified_child("child-1"));
        assert!(!tracker.is_verified_child("child-2"));
        assert_eq!(tracker.state_counts().actors, 2);
    }

    #[test]
    fn sub_agent_history_excludes_only_already_reconciled_hint_ids() {
        let thread = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "provider-root",
                "updatedAt": 3,
                "turns": [{
                    "id": "root-turn",
                    "startedAt": 1,
                    "completedAt": 3,
                    "items": (0..3)
                        .map(|index| serde_json::json!({
                            "id": format!("spawn-{index}"),
                            "type": "subAgentActivity",
                            "agentThreadId": format!("child-{index}"),
                            "agentPath": format!("/root/child-{index}"),
                            "kind": "started"
                        }))
                        .collect::<Vec<_>>()
                }]
            }
        }))
        .expect("root history")
        .thread;
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");
        let excluded = HashSet::from(["child-0".to_owned()]);

        let output = tracker
            .reconcile_sub_agent_hints_with_projection_limit_excluding(&thread, 1, &excluded);

        assert_eq!(output.hinted_descendant_ids, ["child-1", "child-2"]);
        assert!(matches!(
            output.mutations.as_slice(),
            [ProviderActivityMutation::UpsertActor(actor)]
                if actor.id == "codex:thread:child-1"
        ));
        assert_eq!(tracker.state_counts().actors, 2);
    }

    #[test]
    fn sub_agent_descendant_reconciliation_reports_only_accepted_reads() {
        let thread = |id: &str, parent_thread_id: &str| {
            decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": id,
                    "parentThreadId": parent_thread_id,
                    "createdAt": 1,
                    "updatedAt": 2,
                    "turns": []
                }
            }))
            .expect("descendant thread")
            .thread
        };

        let mut tracker = CodexActivityTracker::new(Some("provider-root"));
        tracker.seed_actor("provider-root");
        let reconciliation = tracker.reconcile_descendants(&[thread("child-1", "provider-root")]);
        assert_eq!(reconciliation.accepted_thread_ids, ["child-1"]);

        let mut mismatched_tracker = CodexActivityTracker::new(Some("provider-root"));
        mismatched_tracker.seed_actor("provider-root");
        let mismatched = mismatched_tracker.reconcile_descendants(&[
            thread("self-parent", "self-parent"),
            thread("unresolved-parent", "foreign-parent"),
        ]);
        assert!(mismatched.accepted_thread_ids.is_empty());
    }

    #[test]
    fn descendant_reconciliation_projection_limit_caps_only_accepted_rows() {
        let thread = |id: &str, parent_thread_id: Option<&str>| {
            decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": id,
                    "parentThreadId": parent_thread_id,
                    "createdAt": 1,
                    "updatedAt": 2,
                    "turns": []
                }
            }))
            .expect("descendant thread")
            .thread
        };
        let mut tracker = CodexActivityTracker::new(Some("provider-root"));

        let reconciliation = tracker.reconcile_descendants_with_projection_limit(
            &[
                thread("invalid-child", None),
                thread("accepted-child", Some("provider-root")),
                thread("deferred-child", Some("provider-root")),
            ],
            1,
        );

        assert_eq!(reconciliation.accepted_thread_ids, ["accepted-child"]);
        assert_eq!(reconciliation.threads_to_read, ["accepted-child"]);
        assert!(!tracker.is_verified_child("deferred-child"));
    }

    #[test]
    fn verified_child_terminal_turn_updates_actor_and_requests_reconciliation() {
        for (status, expected_status, error, expected_title) in [
            (
                "completed",
                ActivityLifecycle::Completed,
                None,
                "Turn completed",
            ),
            (
                "failed",
                ActivityLifecycle::Failed,
                Some("provider failed"),
                "Turn failed",
            ),
            (
                "interrupted",
                ActivityLifecycle::Interrupted,
                None,
                "Turn interrupted",
            ),
            (
                "cancelled",
                ActivityLifecycle::Cancelled,
                None,
                "Turn cancelled",
            ),
        ] {
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.seed_actor("child-1");
            let output = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-1",
                    "turn": {
                        "id": format!("{status}-turn"),
                        "status": status,
                        "error": error.map(|message| serde_json::json!({"message": message})),
                        "startedAt": 1,
                        "completedAt": 2
                    }
                }),
                9_000,
                0,
            );

            assert!(
                output.request_reconciliation,
                "{status} child completion must trigger a deferred recovery pass"
            );
            assert!(
                output.mutations.iter().any(|mutation| matches!(
                    mutation,
                    ProviderActivityMutation::AppendEntry(entry)
                        if entry.owner_id == "codex:thread:child-1"
                            && entry.title == expected_title
                            && entry.detail.as_deref() == error
                            && entry.created_at == "1970-01-01T00:00:02.000000000Z"
                )),
                "{output:#?}"
            );
            assert!(output.mutations.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor)
                    if actor.id == "codex:thread:child-1"
                        && actor.status == expected_status
                        && actor.updated_at == "1970-01-01T00:00:02.000000000Z"
                        && actor.terminal_at.as_deref()
                            == Some("1970-01-01T00:00:02.000000000Z")
            )));
        }
    }

    #[test]
    fn live_terminal_actor_authority_uses_only_valid_completion_or_envelope_time() {
        for completed_at in [
            Value::Null,
            serde_json::json!(0_u64),
            serde_json::json!(u64::MAX),
        ] {
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.seed_actor("child-1");
            let output = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-1",
                    "turn": {
                        "id": "fallback-turn",
                        "status": "completed",
                        "startedAt": 1,
                        "completedAt": completed_at
                    }
                }),
                4_000,
                0,
            );
            assert!(output.mutations.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor)
                    if actor.updated_at == "1970-01-01T00:00:04.000000000Z"
                        && actor.terminal_at.as_deref()
                            == Some("1970-01-01T00:00:04.000000000Z")
            )));
        }

        let mut valid_completion = CodexActivityTracker::new(Some("root-1"));
        valid_completion.seed_actor("child-1");
        let output = valid_completion.handle_notification(
            "turn/completed",
            &serde_json::json!({
                "threadId": "child-1",
                "turn": {
                    "id": "completed-turn",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2
                }
            }),
            4_000,
            0,
        );
        assert!(output.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.updated_at == "1970-01-01T00:00:02.000000000Z"
        )));

        for (completed_at, emitted_at_ms) in [
            (Value::Null, 0_u64),
            (serde_json::json!(0_u64), 0_u64),
            (serde_json::json!(u64::MAX), u64::MAX),
        ] {
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.seed_actor("child-1");
            let output = tracker.handle_notification(
                "turn/completed",
                &serde_json::json!({
                    "threadId": "child-1",
                    "turn": {
                        "id": "untrusted-turn",
                        "status": "completed",
                        "startedAt": 1,
                        "completedAt": completed_at
                    }
                }),
                emitted_at_ms,
                0,
            );
            assert!(output.request_reconciliation);
            assert!(
                output
                    .mutations
                    .iter()
                    .any(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            );
            assert!(
                output
                    .mutations
                    .iter()
                    .all(|mutation| !matches!(mutation, ProviderActivityMutation::UpsertActor(_)))
            );
        }
    }

    #[test]
    fn recovery_terminal_actor_authority_uses_only_valid_completion_or_thread_update_time() {
        for completed_at in [
            Value::Null,
            serde_json::json!(0_u64),
            serde_json::json!(u64::MAX),
        ] {
            let response = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "child-1",
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": 4,
                    "status": {"type": "notLoaded"},
                    "turns": [{
                        "id": "fallback-turn",
                        "status": "completed",
                        "startedAt": 1,
                        "completedAt": completed_at,
                        "items": []
                    }]
                }
            }))
            .expect("official thread/read response");
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.seed_actor("child-1");
            let output = tracker.reconcile_thread_history(&response.thread);
            assert!(output.mutations.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor)
                    if actor.updated_at == "1970-01-01T00:00:04.000000000Z"
                        && actor.terminal_at.as_deref()
                            == Some("1970-01-01T00:00:04.000000000Z")
            )));
        }

        let valid_completion = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "root-1",
                "createdAt": 1,
                "updatedAt": 4,
                "status": {"type": "notLoaded"},
                "turns": [{
                    "id": "completed-turn",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": []
                }]
            }
        }))
        .expect("official thread/read response");
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("child-1");
        let output = tracker.reconcile_thread_history(&valid_completion.thread);
        assert!(output.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.updated_at == "1970-01-01T00:00:02.000000000Z"
        )));

        for (completed_at, updated_at) in [
            (Value::Null, 0_u64),
            (serde_json::json!(0_u64), 0_u64),
            (serde_json::json!(u64::MAX), u64::MAX),
        ] {
            let response = decode_thread_read_response(serde_json::json!({
                "thread": {
                    "id": "child-1",
                    "parentThreadId": "root-1",
                    "createdAt": 1,
                    "updatedAt": updated_at,
                    "status": {"type": "notLoaded"},
                    "turns": [{
                        "id": "untrusted-turn",
                        "status": "completed",
                        "startedAt": 1,
                        "completedAt": completed_at,
                        "items": []
                    }]
                }
            }))
            .expect("official thread/read response");
            let mut tracker = CodexActivityTracker::new(Some("root-1"));
            tracker.seed_actor("child-1");
            let output = tracker.reconcile_thread_history(&response.thread);
            assert!(
                output
                    .mutations
                    .iter()
                    .any(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            );
            assert!(output.mutations.iter().all(|mutation| !matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor) if actor.status.is_terminal()
            )));
        }
    }

    #[test]
    fn same_second_disabled_completion_is_not_replayed_when_reconciliation_resumes() {
        let response = decode_thread_read_response(serde_json::json!({
            "thread": {
                "id": "child-1",
                "parentThreadId": "root-1",
                "createdAt": 1,
                "updatedAt": 3,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "same-second-turn",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 3,
                    "items": [{
                        "id": "same-second-message",
                        "type": "agentMessage",
                        "text": "same-second-disabled-detail"
                    }]
                }]
            }
        }))
        .expect("official thread/read response");
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("child-1");
        tracker.begin_detail_baseline();

        let output = tracker.reconcile_thread_history(&response.thread);
        tracker.finish_detail_baseline();

        assert!(output.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == "codex:thread:child-1"
                    && actor.status == ActivityLifecycle::Completed
        )));
        assert!(output.mutations.iter().all(|mutation| !matches!(
            mutation,
            ProviderActivityMutation::AppendEntry(entry)
                if entry.detail.as_deref() == Some("same-second-disabled-detail")
        )));
    }

    #[test]
    fn root_terminal_turn_requests_reconciliation_without_activity_projection() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        let params = serde_json::json!({
            "threadId": "root-1",
            "turn": {
                "id": "root-turn",
                "status": "completed",
                "completedAt": 2
            }
        });
        let output = tracker.handle_notification("turn/completed", &params, 9_000, 0);

        assert!(output.request_reconciliation);
        assert!(
            output.mutations.is_empty(),
            "the root conversation turn must not become attributed activity"
        );
        assert_eq!(tracker.state_counts().actors, 0);
        let duplicate = tracker.handle_notification("turn/completed", &params, 10_000, 1);
        assert!(!duplicate.request_reconciliation);
        assert!(duplicate.mutations.is_empty());
        let distinct = tracker.handle_notification(
            "turn/completed",
            &serde_json::json!({
                "threadId": "root-1",
                "turn": {
                    "id": "later-root-turn",
                    "status": "completed",
                    "completedAt": 3
                }
            }),
            11_000,
            2,
        );
        assert!(distinct.request_reconciliation);
        assert!(distinct.mutations.is_empty());
    }

    #[test]
    fn older_terminal_notification_cannot_replace_latest_child_outcome() {
        let mut tracker = CodexActivityTracker::new(Some("root-1"));
        tracker.seed_actor("child-1");
        let latest = tracker.handle_notification(
            "turn/completed",
            &serde_json::json!({
                "threadId": "child-1",
                "turn": {
                    "id": "latest-turn",
                    "status": "failed",
                    "error": {"message": "latest provider error"},
                    "completedAt": 3
                }
            }),
            9_000,
            0,
        );
        assert!(latest.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.status == ActivityLifecycle::Failed
        )));
        assert!(latest.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::AppendEntry(entry)
                if entry.detail.as_deref() == Some("latest provider error")
        )));

        let older = tracker.handle_notification(
            "turn/completed",
            &serde_json::json!({
                "threadId": "child-1",
                "turn": {
                    "id": "older-turn",
                    "status": "completed",
                    "completedAt": 2
                }
            }),
            10_000,
            1,
        );

        assert!(
            older
                .mutations
                .iter()
                .all(|mutation| !matches!(mutation, ProviderActivityMutation::UpsertActor(_)))
        );
        let actor = tracker
            .actors_by_thread
            .get(&thread_key("child-1"))
            .expect("retained child actor");
        assert_eq!(actor.status, ActivityLifecycle::Failed);
        assert_eq!(actor.updated_at, "1970-01-01T00:00:03.000000000Z");
    }
}

fn contains_redacted_value(value: &Map<String, Value>) -> bool {
    fn is_redacted(value: &Value) -> bool {
        match value {
            Value::String(value) => value.starts_with("[redacted "),
            Value::Array(values) => values.iter().any(is_redacted),
            Value::Object(values) => values.values().any(is_redacted),
            _ => false,
        }
    }
    value.values().any(is_redacted)
}
