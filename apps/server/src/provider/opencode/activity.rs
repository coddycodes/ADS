use std::collections::{HashMap, HashSet, VecDeque};

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::activity::{
    ActivityActorSummary, ActivityEntry, ActivityEntryKind, ActivityEntryTone, ActivityLifecycle,
    ActivityRecordKind, ProviderActivityMutation,
};

const MAX_CHILDREN: usize = 256;
pub(crate) const MAX_RECONCILED_CHILDREN: usize = 128;
pub(crate) const MAX_LINEAGE_DEPTH: usize = 16;
const MAX_SEEN_ENTRIES: usize = 2_048;
const MAX_TEXT_STREAMS: usize = 256;
const MAX_MUTATIONS: usize = 256;
const MAX_HISTORY_MESSAGES: usize = 200;
const MAX_HISTORY_PARTS: usize = 200;
const MAX_TEXT_BYTES: usize = 16_384;
const MAX_PENDING_TEXT_EVENTS: usize = 256;
const MAX_LIVE_TEXT_EVENTS: usize = MAX_TEXT_BYTES;
// Cover every part in the supported terminal reconciliation slice, one root
// history page, and the bounded in-flight text queue without unbounded growth.
const MAX_DETAIL_BASELINE_IDENTITIES: usize =
    MAX_RECONCILED_CHILDREN * MAX_HISTORY_PARTS + MAX_HISTORY_PARTS + MAX_PENDING_TEXT_EVENTS;
pub(crate) const TEXT_COALESCE_MS: u64 = 100;
const MAX_NATIVE_ID_BYTES: usize = 64;
const MAX_FORMATTABLE_UNIX_NANOSECONDS: i128 = 253_402_300_799_999_999_999;
const TRUNCATION_MARKER: &str = "[truncated; recover from history]";

#[derive(Debug, Default)]
pub struct OpenCodeActivityOutput {
    pub mutations: Vec<ProviderActivityMutation>,
}

impl OpenCodeActivityOutput {
    #[allow(clippy::result_large_err)]
    fn push(&mut self, mutation: ProviderActivityMutation) -> Result<(), ProviderActivityMutation> {
        if self.mutations.len() < MAX_MUTATIONS {
            self.mutations.push(mutation);
            Ok(())
        } else {
            Err(mutation)
        }
    }
    fn extend(
        &mut self,
        mutations: Vec<ProviderActivityMutation>,
    ) -> Vec<ProviderActivityMutation> {
        let mut mutations = mutations.into_iter();
        while let Some(mutation) = mutations.next() {
            if let Err(deferred) = self.push(mutation) {
                return std::iter::once(deferred).chain(mutations).collect();
            }
        }
        Vec::new()
    }

    fn is_full(&self) -> bool {
        self.mutations.len() == MAX_MUTATIONS
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenCodeActivityStateCounts {
    pub children: usize,
    pub text_streams: usize,
    pub seen_entries: usize,
}

#[derive(Clone, Debug)]
struct BoundedSeenSet {
    order: VecDeque<String>,
    values: HashSet<String>,
}

impl BoundedSeenSet {
    fn contains(&self, value: &str) -> bool {
        self.values.contains(value)
    }

    fn insert(&mut self, value: String) -> bool {
        if !self.values.insert(value.clone()) {
            return false;
        }
        if self.order.len() == MAX_SEEN_ENTRIES
            && let Some(oldest) = self.order.pop_front()
        {
            self.values.remove(&oldest);
        }
        self.order.push_back(value);
        true
    }

    fn len(&self) -> usize {
        self.values.len()
    }
}

#[derive(Clone, Debug)]
struct OpenCodeChildState {
    parent_session_id: String,
    actor_id: String,
    title: String,
    status: ActivityLifecycle,
    started_at_ms: u64,
    started_at: String,
    updated_at: String,
    terminal_at: Option<String>,
}

#[derive(Clone, Debug)]
struct QuarantinedChild {
    id: String,
    parent_session_id: String,
    title: String,
    created_at_ms: Option<u64>,
}

#[derive(Clone, Debug)]
struct PendingTextEntry {
    id: String,
    semantic: String,
    detail: String,
    at_ms: u64,
    created_at: Option<String>,
    snapshot_base: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SequencedObservationTimestamp {
    unix_nanos: i128,
    created_at: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildReconcileOutcome {
    Rejected,
    Existing,
    Inserted,
}

#[derive(Clone, Debug, Default)]
struct BoundedTextAccumulator {
    normalized: String,
    source_bytes: usize,
    live_segments: VecDeque<String>,
    live_bytes: usize,
    coverage_saturated: bool,
    pending: VecDeque<PendingTextEntry>,
    pending_bytes: usize,
    pending_at_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenCodeActivityTracker {
    root_session_id: String,
    last_observed_event_at_ns: Option<i128>,
    text_drain_cursor: Option<(String, String)>,
    children: HashMap<String, OpenCodeChildState>,
    message_text: HashMap<(String, String), BoundedTextAccumulator>,
    seen_entries: BoundedSeenSet,
    seen_event_ids: BoundedSeenSet,
    assistant_messages: BoundedSeenSet,
    quarantined_children: VecDeque<QuarantinedChild>,
    detail_baseline_active: bool,
    detail_baseline_saturated: bool,
    detail_baseline_identities: HashSet<String>,
}

impl OpenCodeActivityTracker {
    pub(crate) fn new(root_session_id: &str) -> Self {
        Self {
            root_session_id: root_session_id.to_owned(),
            last_observed_event_at_ns: None,
            text_drain_cursor: None,
            children: HashMap::new(),
            message_text: HashMap::new(),
            seen_entries: BoundedSeenSet {
                order: VecDeque::new(),
                values: HashSet::new(),
            },
            seen_event_ids: BoundedSeenSet {
                order: VecDeque::new(),
                values: HashSet::new(),
            },
            assistant_messages: BoundedSeenSet {
                order: VecDeque::new(),
                values: HashSet::new(),
            },
            quarantined_children: VecDeque::new(),
            detail_baseline_active: false,
            detail_baseline_saturated: false,
            detail_baseline_identities: HashSet::new(),
        }
    }

    pub(crate) fn begin_detail_baseline(&mut self) {
        self.detail_baseline_active = true;
        self.detail_baseline_saturated = false;
        self.detail_baseline_identities.clear();
    }

    pub(crate) fn finish_detail_baseline(&mut self) {
        self.detail_baseline_active = false;
    }

    fn suppress_part_identity(
        &mut self,
        session_id: &str,
        message_id: &str,
        part_id: &str,
    ) -> bool {
        let identity = part_detail_identity(session_id, message_id, part_id);
        if self.detail_baseline_active {
            if self.detail_baseline_identities.len() < MAX_DETAIL_BASELINE_IDENTITIES {
                self.detail_baseline_identities.insert(identity);
            } else {
                self.detail_baseline_saturated = true;
            }
            return true;
        }
        self.detail_baseline_saturated || self.detail_baseline_identities.contains(&identity)
    }

    pub(crate) fn state_counts(&self) -> OpenCodeActivityStateCounts {
        OpenCodeActivityStateCounts {
            children: self.children.len(),
            text_streams: self.message_text.len(),
            seen_entries: self.seen_entries.len(),
        }
    }

    pub(crate) fn is_verified_child(&self, session_id: &str) -> bool {
        self.children.contains_key(session_id)
    }

    pub(crate) fn reconcile_children(
        &mut self,
        parent_session_id: &str,
        response: &Value,
    ) -> OpenCodeActivityOutput {
        let mut output = OpenCodeActivityOutput::default();
        let Some(children) = response.as_array() else {
            return output;
        };
        for child in children {
            if self.reconcile_child(parent_session_id, child, &mut output)
                == ChildReconcileOutcome::Inserted
            {
                self.promote_quarantined(&mut output);
            }
        }
        output
    }

    pub(crate) fn reconcile_children_limited(
        &mut self,
        parent_session_id: &str,
        response: &Value,
        limit: usize,
    ) -> (OpenCodeActivityOutput, Vec<String>) {
        let mut output = OpenCodeActivityOutput::default();
        let mut accepted = Vec::new();
        let Some(children) = response.as_array() else {
            return (output, accepted);
        };
        for child in children {
            if accepted.len() >= limit {
                break;
            }
            let Some(id) = string(child, "id").map(str::to_owned) else {
                continue;
            };
            let outcome = self.reconcile_child(parent_session_id, child, &mut output);
            if outcome != ChildReconcileOutcome::Rejected {
                accepted.push(id);
            }
            if outcome == ChildReconcileOutcome::Inserted {
                let remaining = limit.saturating_sub(accepted.len());
                accepted.extend(self.promote_quarantined_limited(&mut output, remaining));
            }
        }
        (output, accepted)
    }

    fn reconcile_child(
        &mut self,
        parent_session_id: &str,
        child: &Value,
        output: &mut OpenCodeActivityOutput,
    ) -> ChildReconcileOutcome {
        let Some(id) = string(child, "id") else {
            return ChildReconcileOutcome::Rejected;
        };
        if !valid_key(id) {
            return ChildReconcileOutcome::Rejected;
        }
        if !self.is_verified_parent(parent_session_id) {
            self.quarantine_child(child);
            return ChildReconcileOutcome::Rejected;
        }
        if id == self.root_session_id || string(child, "parentID") != Some(parent_session_id) {
            return ChildReconcileOutcome::Rejected;
        }
        if let Some(existing) = self.children.get(id) {
            return if existing.parent_session_id == parent_session_id {
                ChildReconcileOutcome::Existing
            } else {
                ChildReconcileOutcome::Rejected
            };
        }
        let Some(depth) = self.lineage_depth(parent_session_id) else {
            return ChildReconcileOutcome::Rejected;
        };
        if depth >= MAX_LINEAGE_DEPTH || self.children.len() >= MAX_CHILDREN {
            return ChildReconcileOutcome::Rejected;
        }
        let started_at_ms =
            first_valid_timestamp([child.pointer("/time/created")]).unwrap_or_default();
        let timestamp = timestamp(started_at_ms);
        let canonical_actor_id = actor_id(id);
        let parent_actor_id =
            (parent_session_id != self.root_session_id).then(|| actor_id(parent_session_id));
        let title = string(child, "title")
            .map(bounded_label)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "OpenCode child".to_owned());
        let state = OpenCodeChildState {
            parent_session_id: parent_session_id.to_owned(),
            actor_id: canonical_actor_id.clone(),
            title,
            status: ActivityLifecycle::Waiting,
            started_at_ms,
            started_at: timestamp.clone(),
            updated_at: timestamp,
            terminal_at: None,
        };
        let Some(summary) = actor_summary(&state, parent_actor_id.as_deref()) else {
            return ChildReconcileOutcome::Rejected;
        };
        if output
            .push(ProviderActivityMutation::UpsertActor(summary))
            .is_err()
        {
            return ChildReconcileOutcome::Rejected;
        }
        self.children.insert(id.to_owned(), state);
        ChildReconcileOutcome::Inserted
    }

    pub(crate) fn handle_event(&mut self, event: &Value) -> OpenCodeActivityOutput {
        self.handle_event_at(
            event,
            event
                .pointer("/properties/time")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
        )
    }

    pub(crate) fn handle_event_at(
        &mut self,
        event: &Value,
        received_at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let event_type = string(event, "type");
        let properties = event.get("properties").unwrap_or(&Value::Null);
        match event_type {
            Some("session.status") => self.handle_status(properties),
            Some("session.created") | Some("session.updated") => {
                if let Some(info) = properties.get("info") {
                    self.handle_session_info(properties, info)
                } else {
                    OpenCodeActivityOutput::default()
                }
            }
            Some("message.updated") => self.handle_message_info(properties),
            Some("message.part.updated") => self.handle_part(properties),
            Some("message.part.delta") => {
                self.handle_text_delta(properties, received_at_ms, None, raw_string(event, "id"))
            }
            Some("command.executed") => {
                self.handle_command(properties, raw_string(event, "id"), received_at_ms)
            }
            Some("session.error") => self.handle_session_error(properties, received_at_ms),
            _ => OpenCodeActivityOutput::default(),
        }
    }

    pub(crate) fn handle_observed_event_at(
        &mut self,
        event: &Value,
        received_at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let event_type = string(event, "type");
        if event_type == Some("command.executed") {
            return self.handle_event_at(event, received_at_ms);
        }
        if !matches!(event_type, Some("message.part.delta" | "session.error")) {
            return self.handle_event(event);
        }
        let candidate_at_ms = first_valid_timestamp([event.pointer("/properties/time")])
            .or_else(|| {
                (received_at_ms != 0 && formatted_timestamp(received_at_ms).is_some())
                    .then_some(received_at_ms)
            })
            .unwrap_or_default();
        if event_type == Some("session.error") {
            return self.handle_event_at(event, candidate_at_ms);
        }
        let sequenced =
            sequence_observation_timestamp(self.last_observed_event_at_ns, candidate_at_ms);
        let event_semantic = event
            .get("properties")
            .and_then(|properties| string(properties, "sessionID"))
            .zip(raw_string(event, "id"))
            .map(|(session_id, event_id)| format!("delta:{session_id}:{event_id}"));
        let was_seen = event_semantic
            .as_deref()
            .is_some_and(|semantic| self.seen_event_ids.contains(semantic));
        let output = self.handle_text_delta(
            event.get("properties").unwrap_or(&Value::Null),
            candidate_at_ms,
            sequenced.as_ref().map(|value| value.created_at.as_str()),
            raw_string(event, "id"),
        );
        if !was_seen
            && event_semantic
                .as_deref()
                .is_some_and(|semantic| self.seen_event_ids.contains(semantic))
            && let Some(sequenced) = sequenced
        {
            self.last_observed_event_at_ns = Some(sequenced.unix_nanos);
        }
        output
    }

    pub(crate) fn handle_history(
        &mut self,
        session_id: &str,
        messages: &Value,
    ) -> OpenCodeActivityOutput {
        let mut output = OpenCodeActivityOutput::default();
        if !self.children.contains_key(session_id) {
            return output;
        }
        let mut part_count = 0;
        for message in messages
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_HISTORY_MESSAGES)
        {
            if output.is_full() {
                break;
            }
            let info = message.get("info").unwrap_or(&Value::Null);
            if string(info, "sessionID") != Some(session_id) {
                continue;
            }
            let deferred =
                output.extend(self.handle_message_info_value(session_id, info).mutations);
            debug_assert!(deferred.is_empty());
            for part in message
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if output.is_full() || part_count == MAX_HISTORY_PARTS {
                    break;
                }
                part_count += 1;
                let deferred =
                    output.extend(self.handle_part_value(session_id, part, info).mutations);
                debug_assert!(deferred.is_empty());
            }
            if part_count == MAX_HISTORY_PARTS {
                break;
            }
        }
        output
    }

    pub(crate) fn flush_text(&mut self) -> OpenCodeActivityOutput {
        self.flush_text_bounded(MAX_MUTATIONS)
    }

    pub(crate) fn flush_text_bounded(&mut self, limit: usize) -> OpenCodeActivityOutput {
        let limit = limit.min(MAX_MUTATIONS);
        let mut keys = self.message_text.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        let mut output = OpenCodeActivityOutput::default();
        let start = self
            .text_drain_cursor
            .as_ref()
            .and_then(|cursor| keys.iter().position(|key| key > cursor))
            .unwrap_or_default();
        for offset in 0..keys.len() {
            if output.mutations.len() == limit {
                break;
            }
            let (session_id, part_id) = &keys[(start + offset) % keys.len()];
            self.flush_text_stream(session_id, part_id, &mut output, limit);
            self.text_drain_cursor = Some((session_id.clone(), part_id.clone()));
        }
        output
    }

    pub(crate) fn has_pending_text(&self) -> bool {
        self.message_text
            .values()
            .any(|stream| !stream.pending.is_empty())
    }

    fn is_verified_parent(&self, session_id: &str) -> bool {
        session_id == self.root_session_id || self.children.contains_key(session_id)
    }

    fn quarantine_child(&mut self, child: &Value) {
        let (Some(id), Some(parent_session_id)) = (string(child, "id"), string(child, "parentID"))
        else {
            return;
        };
        if !valid_key(id) || !valid_key(parent_session_id) {
            return;
        }
        if self.quarantined_children.len() == MAX_CHILDREN {
            self.quarantined_children.pop_front();
        }
        self.quarantined_children.push_back(QuarantinedChild {
            id: id.to_owned(),
            parent_session_id: parent_session_id.to_owned(),
            title: string(child, "title")
                .map(bounded_label)
                .unwrap_or_default(),
            created_at_ms: child.pointer("/time/created").and_then(Value::as_u64),
        });
    }

    fn promote_quarantined(&mut self, output: &mut OpenCodeActivityOutput) {
        let _ = self.promote_quarantined_limited(output, MAX_CHILDREN);
    }

    fn promote_quarantined_limited(
        &mut self,
        output: &mut OpenCodeActivityOutput,
        limit: usize,
    ) -> Vec<String> {
        let mut accepted = Vec::new();
        loop {
            let initial_len = self.quarantined_children.len();
            let mut remaining = VecDeque::new();
            while let Some(child) = self.quarantined_children.pop_front() {
                if accepted.len() == limit {
                    remaining.push_back(child);
                    remaining.append(&mut self.quarantined_children);
                    break;
                }
                if self.is_verified_parent(&child.parent_session_id) {
                    let id = child.id.clone();
                    let parent = child.parent_session_id.clone();
                    let value = serde_json::json!({
                        "id": child.id,
                        "parentID": child.parent_session_id,
                        "title": child.title,
                        "time": { "created": child.created_at_ms }
                    });
                    if self.reconcile_child(&parent, &value, output)
                        == ChildReconcileOutcome::Inserted
                    {
                        accepted.push(id);
                    }
                } else {
                    remaining.push_back(child);
                }
            }
            self.quarantined_children = remaining;
            if accepted.len() == limit || self.quarantined_children.len() >= initial_len {
                break;
            }
        }
        accepted
    }

    fn lineage_depth(&self, session_id: &str) -> Option<usize> {
        let mut current = session_id;
        let mut depth = 0;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current) {
                return None;
            }
            if current == self.root_session_id {
                return Some(depth);
            }
            let child = self.children.get(current)?;
            current = &child.parent_session_id;
            depth += 1;
            if depth > MAX_LINEAGE_DEPTH {
                return None;
            }
        }
    }

    fn handle_session_info(&mut self, properties: &Value, info: &Value) -> OpenCodeActivityOutput {
        let Some(session_id) = string(properties, "sessionID") else {
            return OpenCodeActivityOutput::default();
        };
        if string(info, "id") != Some(session_id) || string(info, "parentID").is_none() {
            return OpenCodeActivityOutput::default();
        }
        self.reconcile_children(
            string(info, "parentID").unwrap_or_default(),
            &Value::Array(vec![info.clone()]),
        )
    }

    fn handle_status(&mut self, properties: &Value) -> OpenCodeActivityOutput {
        let Some(session_id) = string(properties, "sessionID") else {
            return OpenCodeActivityOutput::default();
        };
        let status = match properties.pointer("/status/type").and_then(Value::as_str) {
            Some("busy") => ActivityLifecycle::Running,
            Some("retry") | Some("idle") | None => ActivityLifecycle::Waiting,
            _ => return OpenCodeActivityOutput::default(),
        };
        self.set_status(session_id, status, epoch())
    }

    fn handle_message_info(&mut self, properties: &Value) -> OpenCodeActivityOutput {
        let Some(session_id) = string(properties, "sessionID") else {
            return OpenCodeActivityOutput::default();
        };
        let info = properties.get("info").unwrap_or(&Value::Null);
        self.handle_message_info_value(session_id, info)
    }

    fn handle_message_info_value(
        &mut self,
        session_id: &str,
        info: &Value,
    ) -> OpenCodeActivityOutput {
        if string(info, "sessionID") != Some(session_id)
            || string(info, "role") != Some("assistant")
        {
            return OpenCodeActivityOutput::default();
        }
        if !valid_key(session_id) || !self.children.contains_key(session_id) {
            return OpenCodeActivityOutput::default();
        }
        let Some(message_id) = string(info, "id").filter(|message_id| valid_key(message_id)) else {
            return OpenCodeActivityOutput::default();
        };
        self.assistant_messages
            .insert(format!("assistant:{session_id}:{message_id}"));
        // History snapshots carry an unfinished message's completion time as `null`.
        let completed = info
            .pointer("/time/completed")
            .filter(|completed| !completed.is_null());
        let when = completed
            .or_else(|| info.pointer("/time/created"))
            .and_then(Value::as_u64)
            .map(timestamp)
            .unwrap_or_else(epoch);
        if let Some(error) = info.get("error").filter(|error| !error.is_null()) {
            // An abort is a cancellation, as on the live `session.error` path.
            let aborted = string(error, "name") == Some("MessageAbortedError")
                || string(error, "type") == Some("aborted");
            let status = if aborted {
                ActivityLifecycle::Cancelled
            } else {
                ActivityLifecycle::Failed
            };
            return self.set_status(session_id, status, when);
        }
        if completed.is_some() || string(info, "finish").is_some() {
            return self.set_status(session_id, ActivityLifecycle::Completed, when);
        }
        OpenCodeActivityOutput::default()
    }

    fn handle_session_error(
        &mut self,
        properties: &Value,
        received_at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let Some(session_id) = string(properties, "sessionID") else {
            return OpenCodeActivityOutput::default();
        };
        let name = properties
            .pointer("/error/name")
            .and_then(Value::as_str)
            .or_else(|| string(properties, "name"));
        match name {
            Some("MessageAbortedError") => self.set_status(
                session_id,
                ActivityLifecycle::Cancelled,
                timestamp(received_at_ms),
            ),
            _ => OpenCodeActivityOutput::default(),
        }
    }

    fn set_status(
        &mut self,
        session_id: &str,
        status: ActivityLifecycle,
        at: String,
    ) -> OpenCodeActivityOutput {
        let Some(child) = self.children.get_mut(session_id) else {
            return OpenCodeActivityOutput::default();
        };
        let at = if at == epoch() {
            child.updated_at.clone()
        } else {
            at
        };
        if child.status.is_terminal()
            && (!status.is_terminal()
                || child.terminal_at.as_deref().is_some_and(|terminal_at| {
                    terminal_at > at.as_str()
                        || (terminal_at == at.as_str()
                            && terminal_precedence(child.status) >= terminal_precedence(status))
                }))
        {
            return OpenCodeActivityOutput::default();
        }
        if child.status == status
            && (!status.is_terminal()
                || child
                    .terminal_at
                    .as_deref()
                    .is_some_and(|terminal_at| terminal_at >= at.as_str()))
        {
            return OpenCodeActivityOutput::default();
        }
        child.status = status;
        child.updated_at = at.clone();
        if status.is_terminal() {
            child.terminal_at = Some(at);
        }
        let mut output = OpenCodeActivityOutput::default();
        let parent = (child.parent_session_id != self.root_session_id)
            .then(|| actor_id(&child.parent_session_id));
        if let Some(summary) = actor_summary(child, parent.as_deref()) {
            let result = output.push(ProviderActivityMutation::UpsertActor(summary));
            debug_assert!(result.is_ok());
        }
        output
    }

    fn handle_part(&mut self, properties: &Value) -> OpenCodeActivityOutput {
        let Some(session_id) = string(properties, "sessionID") else {
            return OpenCodeActivityOutput::default();
        };
        let part = properties.get("part").unwrap_or(&Value::Null);
        self.handle_part_value(session_id, part, properties)
    }

    fn handle_part_value(
        &mut self,
        session_id: &str,
        part: &Value,
        enclosing: &Value,
    ) -> OpenCodeActivityOutput {
        if string(part, "sessionID") != Some(session_id) || !self.children.contains_key(session_id)
        {
            return OpenCodeActivityOutput::default();
        }
        let Some(message_id) = string(part, "messageID") else {
            return OpenCodeActivityOutput::default();
        };
        if !valid_key(message_id) {
            return OpenCodeActivityOutput::default();
        }
        if !self
            .assistant_messages
            .contains(&format!("assistant:{session_id}:{message_id}"))
        {
            return OpenCodeActivityOutput::default();
        }
        let Some(part_id) = string(part, "id").filter(|part_id| valid_key(part_id)) else {
            return OpenCodeActivityOutput::default();
        };
        if matches!(string(part, "type"), Some("text" | "tool"))
            && self.suppress_part_identity(session_id, message_id, part_id)
        {
            return OpenCodeActivityOutput::default();
        }
        let at_ms = self.resolve_part_timestamp_ms(session_id, part, enclosing);
        match string(part, "type") {
            Some("text") => self.handle_text_part(session_id, part, at_ms),
            Some("tool") => self.handle_tool_part(session_id, part, at_ms),
            _ => OpenCodeActivityOutput::default(),
        }
    }

    fn resolve_part_timestamp_ms(&self, session_id: &str, part: &Value, enclosing: &Value) -> u64 {
        let part_timestamp = match string(part, "type") {
            Some("text") => {
                first_valid_timestamp([part.pointer("/time/end"), part.pointer("/time/start")])
            }
            Some("tool")
                if matches!(
                    part.pointer("/state/status").and_then(Value::as_str),
                    Some("completed" | "error")
                ) =>
            {
                first_valid_timestamp([
                    part.pointer("/state/time/end"),
                    part.pointer("/state/time/start"),
                ])
            }
            Some("tool") => first_valid_timestamp([part.pointer("/state/time/start"), None]),
            _ => None,
        };
        part_timestamp
            .or_else(|| {
                first_valid_timestamp([
                    enclosing.pointer("/time/completed"),
                    enclosing.pointer("/time/created"),
                    enclosing.get("time").filter(|time| !time.is_object()),
                ])
            })
            .or_else(|| {
                self.children
                    .get(session_id)
                    .map(|child| child.started_at_ms)
            })
            .unwrap_or_default()
    }

    fn handle_text_delta(
        &mut self,
        properties: &Value,
        at_ms: u64,
        created_at: Option<&str>,
        event_id: Option<&str>,
    ) -> OpenCodeActivityOutput {
        let Some(session_id) = string(properties, "sessionID") else {
            return OpenCodeActivityOutput::default();
        };
        let Some(message_id) = string(properties, "messageID") else {
            return OpenCodeActivityOutput::default();
        };
        let Some(part_id) = string(properties, "partID") else {
            return OpenCodeActivityOutput::default();
        };
        if !valid_key(message_id) || !valid_key(part_id) {
            return OpenCodeActivityOutput::default();
        }
        if !self.children.contains_key(session_id) || string(properties, "field") != Some("text") {
            return OpenCodeActivityOutput::default();
        }
        if !self
            .assistant_messages
            .contains(&format!("assistant:{session_id}:{message_id}"))
        {
            return OpenCodeActivityOutput::default();
        }
        if self.suppress_part_identity(session_id, message_id, part_id) {
            return OpenCodeActivityOutput::default();
        }
        let Some(event_id) = event_id.filter(|value| valid_key(value)) else {
            return OpenCodeActivityOutput::default();
        };
        let event_semantic = format!("delta:{session_id}:{event_id}");
        if self.seen_event_ids.contains(&event_semantic) {
            return OpenCodeActivityOutput::default();
        }
        let delta = raw_string(properties, "delta").unwrap_or_default();
        if delta.is_empty() {
            return OpenCodeActivityOutput::default();
        }
        let detail = bounded_text_with_marker(delta);
        let id = entry_id(message_id, part_id, &format!("event:{}", digest(event_id)));
        let semantic = format!("text-delta:{session_id}:{message_id}:{part_id}:{event_id}");
        let (output, accepted) = self.enqueue_delta(
            session_id,
            part_id,
            PendingTextEntry {
                id,
                semantic,
                detail,
                at_ms,
                created_at: created_at.map(str::to_owned),
                snapshot_base: None,
            },
        );
        if accepted {
            self.seen_event_ids.insert(event_semantic);
            if let Some(stream) = self
                .message_text
                .get_mut(&(session_id.to_owned(), part_id.to_owned()))
            {
                push_live_segment(stream, delta);
            }
        }
        output
    }

    fn handle_text_part(
        &mut self,
        session_id: &str,
        part: &Value,
        at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let (Some(message_id), Some(part_id), Some(text)) = (
            string(part, "messageID"),
            string(part, "id"),
            raw_string(part, "text").filter(|text| !text.is_empty()),
        ) else {
            return OpenCodeActivityOutput::default();
        };
        if !valid_key(message_id) || !valid_key(part_id) {
            return OpenCodeActivityOutput::default();
        }
        let normalized = bounded_text(text);
        let key = (session_id.to_owned(), part_id.to_owned());
        if !self.message_text.contains_key(&key) && self.message_text.len() >= MAX_TEXT_STREAMS {
            return OpenCodeActivityOutput::default();
        }
        let source_bytes = text.len();
        let (previous, previous_source_bytes, live_segments) = self
            .message_text
            .get(&key)
            .map(|value| {
                (
                    value.normalized.clone(),
                    value.source_bytes,
                    value.live_segments.clone(),
                )
            })
            .unwrap_or_default();
        if source_bytes <= previous_source_bytes && previous.starts_with(&normalized) {
            return OpenCodeActivityOutput::default();
        }
        if self
            .message_text
            .get(&key)
            .is_some_and(|stream| stream.coverage_saturated)
        {
            let pending = saturation_marker(
                session_id,
                message_id,
                part_id,
                &normalized,
                source_bytes,
                at_ms,
            );
            if !self.enqueue_snapshot(&key, pending) {
                return OpenCodeActivityOutput::default();
            }
            let stream = self.message_text.get_mut(&key).expect("text stream");
            stream.normalized = normalized;
            stream.source_bytes = source_bytes;
            clear_live_segments(stream);
            stream.coverage_saturated = false;
            return OpenCodeActivityOutput::default();
        }
        let candidate_suffix = append_only_suffix(text, &previous, previous_source_bytes)
            .map(bounded_text)
            .or_else(|| normalized.strip_prefix(&previous).map(str::to_owned));
        if let Some(matched) = candidate_suffix
            .as_deref()
            .and_then(|suffix| match_newest_live_segments(&live_segments, suffix))
        {
            let stream = self.message_text.entry(key).or_default();
            stream.normalized = normalized;
            stream.source_bytes = source_bytes;
            remove_live_segments(stream, &matched);
            return OpenCodeActivityOutput::default();
        }
        let live_suffix = concatenate_live_segments(&live_segments);
        let suffix = if let Some(candidate_suffix) = candidate_suffix.as_deref() {
            if live_suffix.starts_with(candidate_suffix) {
                ""
            } else if let Some(uncovered) = candidate_suffix.strip_prefix(&live_suffix) {
                uncovered
            } else {
                candidate_suffix
            }
        } else {
            normalized.as_str()
        };
        if suffix.is_empty() {
            let stream = self.message_text.entry(key).or_default();
            stream.normalized = normalized;
            stream.source_bytes = source_bytes;
            clear_live_segments(stream);
            return OpenCodeActivityOutput::default();
        }
        let snapshot_base = self
            .message_text
            .get(&key)
            .and_then(|stream| stream.pending.back())
            .and_then(|pending| pending.snapshot_base)
            .unwrap_or(previous_source_bytes);
        let semantic = format!(
            "text-snapshot:{session_id}:{message_id}:{part_id}:{}:{source_bytes}:{}",
            snapshot_base,
            digest(&normalized)
        );
        if self.seen_entries.contains(&semantic)
            || self.message_text.get(&key).is_some_and(|stream| {
                stream
                    .pending
                    .iter()
                    .any(|pending| pending.semantic == semantic)
            })
        {
            let stream = self.message_text.entry(key).or_default();
            stream.normalized = normalized;
            stream.source_bytes = source_bytes;
            clear_live_segments(stream);
            return OpenCodeActivityOutput::default();
        }
        let detail = if text.len() > MAX_TEXT_BYTES {
            mark_truncated(suffix)
        } else {
            suffix.to_owned()
        };
        let pending = PendingTextEntry {
            id: entry_id(
                message_id,
                part_id,
                &format!(
                    "snapshot:{snapshot_base}:{source_bytes}:{}",
                    digest(&normalized)
                ),
            ),
            semantic,
            detail,
            at_ms,
            created_at: None,
            snapshot_base: Some(snapshot_base),
        };
        if !self.enqueue_snapshot(&key, pending) {
            return OpenCodeActivityOutput::default();
        }
        let stream = self.message_text.get_mut(&key).expect("text stream");
        stream.normalized = normalized;
        stream.source_bytes = source_bytes;
        clear_live_segments(stream);
        OpenCodeActivityOutput::default()
    }

    fn enqueue_delta(
        &mut self,
        session_id: &str,
        part_id: &str,
        pending: PendingTextEntry,
    ) -> (OpenCodeActivityOutput, bool) {
        let key = (session_id.to_owned(), part_id.to_owned());
        if !self.message_text.contains_key(&key) && self.message_text.len() >= MAX_TEXT_STREAMS {
            return (OpenCodeActivityOutput::default(), false);
        }
        let mut output = OpenCodeActivityOutput::default();
        let needs_flush = self.message_text.get(&key).is_some_and(|stream| {
            stream.pending.len() == MAX_PENDING_TEXT_EVENTS
                || stream.pending_bytes.saturating_add(pending.detail.len()) > MAX_TEXT_BYTES
        });
        if needs_flush {
            self.flush_text_stream(session_id, part_id, &mut output, MAX_MUTATIONS);
            if output.is_full() {
                return (output, false);
            }
        }
        let at_ms = pending.at_ms;
        let stream = self.message_text.entry(key).or_default();
        stream.pending_bytes = stream.pending_bytes.saturating_add(pending.detail.len());
        stream.pending.push_back(pending);
        if stream.pending_at_ms.is_none() {
            stream.pending_at_ms = Some(at_ms);
        }
        if at_ms.saturating_sub(stream.pending_at_ms.unwrap_or(at_ms)) >= TEXT_COALESCE_MS {
            self.flush_text_stream(session_id, part_id, &mut output, MAX_MUTATIONS);
        }
        (output, true)
    }

    fn enqueue_snapshot(&mut self, key: &(String, String), pending: PendingTextEntry) -> bool {
        let stream = self.message_text.entry(key.clone()).or_default();
        if let Some(last) = stream
            .pending
            .back_mut()
            .filter(|last| last.snapshot_base.is_some())
        {
            let replacement_bytes = last.detail.len().saturating_add(pending.detail.len());
            let total_bytes = stream
                .pending_bytes
                .saturating_sub(last.detail.len())
                .saturating_add(replacement_bytes);
            if replacement_bytes > MAX_TEXT_BYTES || total_bytes > MAX_TEXT_BYTES {
                return false;
            }
            last.detail.push_str(&pending.detail);
            last.id = pending.id;
            last.semantic = pending.semantic;
            last.at_ms = pending.at_ms;
            last.created_at = pending.created_at;
            stream.pending_bytes = total_bytes;
            return true;
        }
        if stream.pending.len() == MAX_PENDING_TEXT_EVENTS
            || stream.pending_bytes.saturating_add(pending.detail.len()) > MAX_TEXT_BYTES
        {
            return false;
        }
        stream.pending_bytes = stream.pending_bytes.saturating_add(pending.detail.len());
        if stream.pending_at_ms.is_none() {
            stream.pending_at_ms = Some(pending.at_ms);
        }
        stream.pending.push_back(pending);
        true
    }

    fn handle_tool_part(
        &mut self,
        session_id: &str,
        part: &Value,
        at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let (Some(message_id), Some(part_id), Some(call_id), Some(tool), Some(state)) = (
            string(part, "messageID"),
            string(part, "id"),
            string(part, "callID"),
            string(part, "tool"),
            part.pointer("/state/status").and_then(Value::as_str),
        ) else {
            return OpenCodeActivityOutput::default();
        };
        if !valid_key(message_id) || !valid_key(part_id) || !valid_key(call_id) {
            return OpenCodeActivityOutput::default();
        }
        if !matches!(state, "pending" | "running" | "completed" | "error") {
            return OpenCodeActivityOutput::default();
        }
        let semantic = format!("tool:{session_id}:{message_id}:{part_id}:{call_id}:{state}");
        if self.seen_entries.contains(&semantic) {
            return OpenCodeActivityOutput::default();
        }
        let (kind, tone, suffix) = match state {
            "error" => (ActivityEntryKind::Error, ActivityEntryTone::Error, "error"),
            "completed" => (
                ActivityEntryKind::Tool,
                ActivityEntryTone::Success,
                "completed",
            ),
            _ => (ActivityEntryKind::Tool, ActivityEntryTone::Tool, state),
        };
        let output = self.entry(
            session_id,
            entry_id(message_id, part_id, &format!("{state}:{}", digest(call_id))),
            kind,
            format!("{} {}", bounded_label(tool), suffix),
            None,
            tone,
            at_ms,
        );
        if !output.mutations.is_empty() {
            self.seen_entries.insert(semantic);
        }
        output
    }

    fn handle_command(
        &mut self,
        properties: &Value,
        event_id: Option<&str>,
        received_at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let (Some(session_id), Some(message_id), Some(name)) = (
            string(properties, "sessionID"),
            string(properties, "messageID"),
            string(properties, "name"),
        ) else {
            return OpenCodeActivityOutput::default();
        };
        if !valid_key(session_id) || !valid_key(message_id) || !valid_key(name) {
            return OpenCodeActivityOutput::default();
        }
        let Some(event_id) = event_id.filter(|value| valid_key(value)) else {
            return OpenCodeActivityOutput::default();
        };
        if !self.children.contains_key(session_id)
            || !self
                .assistant_messages
                .contains(&format!("assistant:{session_id}:{message_id}"))
        {
            return OpenCodeActivityOutput::default();
        }
        let arguments = string(properties, "arguments").unwrap_or_default();
        let semantic = format!(
            "command:{event_id}:{session_id}:{message_id}:{name}:{}",
            digest(arguments)
        );
        if self.seen_entries.contains(&semantic) {
            return OpenCodeActivityOutput::default();
        }
        let at_ms = first_valid_timestamp([properties.get("time")])
            .or_else(|| {
                (received_at_ms != 0 && formatted_timestamp(received_at_ms).is_some())
                    .then_some(received_at_ms)
            })
            .unwrap_or_default();
        let output = self.entry(
            session_id,
            format!("opencode:command:{message_id}:{event_id}:h{}", digest(name)),
            ActivityEntryKind::Command,
            bounded_label(name),
            Some(bounded_detail(arguments)),
            ActivityEntryTone::Tool,
            at_ms,
        );
        if !output.mutations.is_empty() {
            self.seen_entries.insert(semantic);
        }
        output
    }

    fn flush_text_stream(
        &mut self,
        session_id: &str,
        part_id: &str,
        output: &mut OpenCodeActivityOutput,
        limit: usize,
    ) {
        let key = (session_id.to_owned(), part_id.to_owned());
        loop {
            if output.mutations.len() == limit {
                return;
            }
            let Some(pending) = self
                .message_text
                .get(&key)
                .and_then(|stream| stream.pending.front())
                .cloned()
            else {
                if let Some(stream) = self.message_text.get_mut(&key) {
                    stream.pending_at_ms = None;
                }
                return;
            };
            if self.seen_entries.contains(&pending.semantic) {
                self.pop_pending_text(&key);
                continue;
            }
            let Some(mutation) = self.entry_mutation(
                session_id,
                pending.id,
                ActivityEntryKind::Commentary,
                "Commentary".to_owned(),
                Some(pending.detail),
                ActivityEntryTone::Info,
                pending.at_ms,
                pending.created_at,
            ) else {
                self.pop_pending_text(&key);
                continue;
            };
            if output.push(mutation).is_err() {
                return;
            }
            self.seen_entries.insert(pending.semantic);
            self.pop_pending_text(&key);
        }
    }

    fn pop_pending_text(&mut self, key: &(String, String)) {
        let Some(stream) = self.message_text.get_mut(key) else {
            return;
        };
        if let Some(pending) = stream.pending.pop_front() {
            stream.pending_bytes = stream.pending_bytes.saturating_sub(pending.detail.len());
        }
        stream.pending_at_ms = stream.pending.front().map(|pending| pending.at_ms);
    }

    #[allow(clippy::too_many_arguments)]
    fn entry(
        &self,
        session_id: &str,
        id: String,
        kind: ActivityEntryKind,
        title: String,
        detail: Option<String>,
        tone: ActivityEntryTone,
        at_ms: u64,
    ) -> OpenCodeActivityOutput {
        let mut output = OpenCodeActivityOutput::default();
        if let Some(mutation) =
            self.entry_mutation(session_id, id, kind, title, detail, tone, at_ms, None)
        {
            let result = output.push(mutation);
            debug_assert!(result.is_ok());
        }
        output
    }

    #[allow(clippy::too_many_arguments)]
    fn entry_mutation(
        &self,
        session_id: &str,
        id: String,
        kind: ActivityEntryKind,
        title: String,
        detail: Option<String>,
        tone: ActivityEntryTone,
        at_ms: u64,
        created_at: Option<String>,
    ) -> Option<ProviderActivityMutation> {
        ActivityEntry::try_new(
            id,
            ActivityRecordKind::Actor,
            actor_id(session_id),
            kind,
            title,
            detail.as_deref(),
            tone,
            created_at.unwrap_or_else(|| timestamp(at_ms)),
        )
        .ok()
        .map(ProviderActivityMutation::AppendEntry)
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct OpenCodeActivityFixtureAdapter {
    tracker: OpenCodeActivityTracker,
}

#[doc(hidden)]
impl OpenCodeActivityFixtureAdapter {
    pub fn new(root_session_id: &str) -> Self {
        Self {
            tracker: OpenCodeActivityTracker::new(root_session_id),
        }
    }
    pub fn state_counts(&self) -> OpenCodeActivityStateCounts {
        self.tracker.state_counts()
    }
    pub fn reconcile_children(
        &mut self,
        parent_session_id: &str,
        response: &Value,
    ) -> OpenCodeActivityOutput {
        self.tracker.reconcile_children(parent_session_id, response)
    }
    pub fn handle_event(&mut self, event: &Value) -> OpenCodeActivityOutput {
        self.tracker.handle_event(event)
    }
    pub fn handle_event_at(
        &mut self,
        event: &Value,
        received_at_ms: u64,
    ) -> OpenCodeActivityOutput {
        self.tracker.handle_event_at(event, received_at_ms)
    }
    pub fn handle_history(&mut self, session_id: &str, messages: &Value) -> OpenCodeActivityOutput {
        self.tracker.handle_history(session_id, messages)
    }
    pub fn flush_text(&mut self) -> OpenCodeActivityOutput {
        self.tracker.flush_text()
    }
}

fn string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
fn raw_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}
pub(crate) fn valid_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NATIVE_ID_BYTES && !value.chars().any(char::is_control)
}
fn actor_id(session_id: &str) -> String {
    format!("opencode:session:{session_id}")
}
fn entry_id(message_id: &str, part_id: &str, state_or_hash: &str) -> String {
    format!("opencode:part:{message_id}:{part_id}:{state_or_hash}")
}

fn part_detail_identity(session_id: &str, message_id: &str, part_id: &str) -> String {
    format!(
        "{}:{session_id}{}:{message_id}{}:{part_id}",
        session_id.len(),
        message_id.len(),
        part_id.len(),
    )
}
fn actor_summary(child: &OpenCodeChildState, parent: Option<&str>) -> Option<ActivityActorSummary> {
    ActivityActorSummary::try_new(
        child.actor_id.clone(),
        parent,
        child.title.clone(),
        Some("child"),
        Some("opencode"),
        child.status,
        None,
        child.started_at.clone(),
        child.updated_at.clone(),
        child.terminal_at.as_deref(),
    )
    .ok()
}
fn bounded_label(value: &str) -> String {
    value.trim().chars().take(256).collect()
}
fn bounded_detail(value: &str) -> String {
    bounded_text(value.trim())
}
fn bounded_text(value: &str) -> String {
    if value.len() <= MAX_TEXT_BYTES {
        return value.to_owned();
    }
    utf8_prefix(value, MAX_TEXT_BYTES).to_owned()
}
fn bounded_text_with_marker(value: &str) -> String {
    if value.len() <= MAX_TEXT_BYTES {
        return value.to_owned();
    }
    mark_truncated(value)
}
fn mark_truncated(value: &str) -> String {
    let prefix = utf8_prefix(value, MAX_TEXT_BYTES - TRUNCATION_MARKER.len());
    let mut bounded = String::with_capacity(MAX_TEXT_BYTES);
    bounded.push_str(prefix);
    bounded.push_str(TRUNCATION_MARKER);
    bounded
}
fn utf8_prefix(value: &str, maximum: usize) -> &str {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
fn append_only_suffix<'a>(
    value: &'a str,
    previous_prefix: &str,
    previous_source_bytes: usize,
) -> Option<&'a str> {
    (value.len() > previous_source_bytes
        && value.starts_with(previous_prefix)
        && value.is_char_boundary(previous_source_bytes))
    .then(|| &value[previous_source_bytes..])
}
fn saturation_marker(
    session_id: &str,
    message_id: &str,
    part_id: &str,
    normalized: &str,
    source_bytes: usize,
    at_ms: u64,
) -> PendingTextEntry {
    let snapshot_digest = digest(normalized);
    PendingTextEntry {
        id: entry_id(
            message_id,
            part_id,
            &format!("coverage-saturated:{source_bytes}:{snapshot_digest}"),
        ),
        semantic: format!(
            "text-coverage-saturated:{session_id}:{message_id}:{part_id}:{source_bytes}:{snapshot_digest}"
        ),
        detail: TRUNCATION_MARKER.to_owned(),
        at_ms,
        created_at: None,
        snapshot_base: None,
    }
}
fn push_live_segment(stream: &mut BoundedTextAccumulator, value: &str) {
    let segment = bounded_text(value);
    if segment.is_empty() {
        return;
    }
    while !stream.live_segments.is_empty()
        && (stream.live_segments.len() == MAX_LIVE_TEXT_EVENTS
            || stream.live_bytes.saturating_add(segment.len()) > MAX_TEXT_BYTES)
    {
        if let Some(removed) = stream.live_segments.pop_front() {
            stream.live_bytes = stream.live_bytes.saturating_sub(removed.len());
            stream.coverage_saturated = true;
        }
    }
    stream.live_bytes = stream.live_bytes.saturating_add(segment.len());
    stream.live_segments.push_back(segment);
    debug_assert!(stream.live_bytes <= MAX_TEXT_BYTES);
    debug_assert!(stream.live_segments.len() <= MAX_LIVE_TEXT_EVENTS);
}
fn concatenate_live_segments(segments: &VecDeque<String>) -> String {
    let total = segments.iter().map(String::len).sum::<usize>();
    let mut concatenated = String::with_capacity(total.min(MAX_TEXT_BYTES));
    for segment in segments {
        concatenated.push_str(segment);
    }
    concatenated
}
fn match_newest_live_segments(
    segments: &VecDeque<String>,
    candidate_suffix: &str,
) -> Option<Vec<usize>> {
    if candidate_suffix.is_empty() {
        return Some(Vec::new());
    }
    let mut remaining = candidate_suffix;
    let mut matched = Vec::new();
    for (index, segment) in segments.iter().enumerate().rev() {
        if let Some(prefix) = remaining.strip_suffix(segment) {
            matched.push(index);
            remaining = prefix;
            if remaining.is_empty() {
                return Some(matched);
            }
        }
    }
    None
}
fn remove_live_segments(stream: &mut BoundedTextAccumulator, indices: &[usize]) {
    for index in indices {
        if let Some(removed) = stream.live_segments.remove(*index) {
            stream.live_bytes = stream.live_bytes.saturating_sub(removed.len());
        }
    }
}
fn clear_live_segments(stream: &mut BoundedTextAccumulator) {
    stream.live_segments.clear();
    stream.live_bytes = 0;
}
fn digest(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn first_valid_timestamp<'a>(values: impl IntoIterator<Item = Option<&'a Value>>) -> Option<u64> {
    values
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .filter(|milliseconds| *milliseconds != 0)
        .find(|milliseconds| formatted_timestamp(*milliseconds).is_some())
}
fn timestamp(milliseconds: u64) -> String {
    formatted_timestamp(milliseconds).unwrap_or_else(epoch)
}
fn formatted_timestamp(milliseconds: u64) -> Option<String> {
    formatted_timestamp_nanos(i128::from(milliseconds) * 1_000_000)
}
fn formatted_timestamp_nanos(nanoseconds: i128) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(nanoseconds)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
}
fn sequence_observation_timestamp(
    last_observed_event_at_ns: Option<i128>,
    candidate_at_ms: u64,
) -> Option<SequencedObservationTimestamp> {
    let candidate_at_ns = i128::from(candidate_at_ms).checked_mul(1_000_000)?;
    formatted_timestamp_nanos(candidate_at_ns)?;
    let previous = last_observed_event_at_ns
        .unwrap_or(candidate_at_ns.saturating_sub(1))
        .min(MAX_FORMATTABLE_UNIX_NANOSECONDS);
    let unix_nanos = if previous < candidate_at_ns {
        candidate_at_ns
    } else {
        // RFC3339's upper endpoint has no representable successor. Saturating
        // there keeps the head valid and deterministic; strict chronology is
        // preserved for every representable successor before this endpoint.
        previous
            .checked_add(1)
            .unwrap_or(MAX_FORMATTABLE_UNIX_NANOSECONDS)
            .min(MAX_FORMATTABLE_UNIX_NANOSECONDS)
    };
    Some(SequencedObservationTimestamp {
        unix_nanos,
        created_at: formatted_timestamp_nanos(unix_nanos)?,
    })
}
fn epoch() -> String {
    "1970-01-01T00:00:00Z".to_owned()
}
fn terminal_precedence(status: ActivityLifecycle) -> u8 {
    match status {
        ActivityLifecycle::Failed => 3,
        ActivityLifecycle::Cancelled => 2,
        ActivityLifecycle::Completed => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A history snapshot serializes a missing completion time as `null`. An assistant message
    /// still in progress must not mark its child completed, or a later `busy` cannot revive it.
    #[test]
    fn an_unfinished_history_message_leaves_a_running_child_running() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        tracker.reconcile_children(
            "root",
            &json!([{"id":"child","parentID":"root","title":"Child","time":{"created":1}}]),
        );
        tracker.handle_event(&json!({
            "type": "session.status",
            "properties": {"sessionID": "child", "status": {"type": "busy"}}
        }));
        assert_eq!(tracker.children["child"].status, ActivityLifecycle::Running);
        tracker.handle_history(
            "child",
            &json!([{
                "info": {
                    "id": "msg_1",
                    "sessionID": "child",
                    "role": "assistant",
                    "time": {"created": 2, "completed": null},
                    "finish": null,
                    "error": null
                },
                "parts": []
            }]),
        );
        assert_eq!(tracker.children["child"].status, ActivityLifecycle::Running);
    }

    #[test]
    fn limited_reconciliation_returns_promoted_child_within_remaining_capacity() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        let quarantined = tracker.handle_event(&json!({
            "type": "session.created",
            "properties": {
                "sessionID": "grandchild",
                "info": {
                    "id": "grandchild",
                    "parentID": "parent",
                    "title": "Grandchild",
                    "time": {"created": 2}
                }
            }
        }));
        assert!(quarantined.mutations.is_empty());
        assert_eq!(tracker.quarantined_children.len(), 1);

        let (output, accepted) = tracker.reconcile_children_limited(
            "root",
            &json!([{
                "id": "parent",
                "parentID": "root",
                "title": "Parent",
                "time": {"created": 1}
            }]),
            2,
        );

        assert_eq!(accepted, ["parent", "grandchild"]);
        assert_eq!(output.mutations.len(), 2);
        assert!(tracker.is_verified_child("parent"));
        assert!(tracker.is_verified_child("grandchild"));
        assert!(tracker.quarantined_children.is_empty());
    }

    #[test]
    fn limited_reconciliation_leaves_first_promoted_child_beyond_capacity_quarantined() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        for child_id in ["first", "beyond-capacity"] {
            let quarantined = tracker.handle_event(&json!({
                "type": "session.created",
                "properties": {
                    "sessionID": child_id,
                    "info": {
                        "id": child_id,
                        "parentID": "parent",
                        "title": child_id,
                        "time": {"created": 2}
                    }
                }
            }));
            assert!(quarantined.mutations.is_empty());
        }

        let (output, accepted) = tracker.reconcile_children_limited(
            "root",
            &json!([{
                "id": "parent",
                "parentID": "root",
                "title": "Parent",
                "time": {"created": 1}
            }]),
            2,
        );

        assert_eq!(accepted, ["parent", "first"]);
        assert_eq!(output.mutations.len(), 2);
        assert!(tracker.is_verified_child("first"));
        assert!(!tracker.is_verified_child("beyond-capacity"));
        assert!(matches!(
            tracker.quarantined_children.front(),
            Some(child) if child.id == "beyond-capacity"
        ));
        assert_eq!(tracker.quarantined_children.len(), 1);
    }

    #[test]
    fn supported_large_resume_baseline_keeps_new_post_baseline_identity_publishable_and_bounded() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        tracker.begin_detail_baseline();
        let child_count = 128;
        let parts_per_child = 200;

        for child_index in 0..child_count {
            let child_id = format!("child-{child_index}");
            let message_id = format!("message-{child_index}");
            let children = json!([{
                "id": child_id,
                "parentID": "root",
                "title": format!("Child {child_index}"),
                "time": {"created": 1}
            }]);
            assert_eq!(
                tracker
                    .reconcile_children("root", &children)
                    .mutations
                    .len(),
                1
            );
            let parts = (0..parts_per_child)
                .map(|part_index| {
                    json!({
                        "id": format!("part-{part_index}"),
                        "sessionID": child_id,
                        "messageID": message_id,
                        "type": "tool",
                        "tool": "dormant",
                        "callID": format!("call-{part_index}"),
                        "state": {"status": "completed", "time": {"start": 1, "end": 2}}
                    })
                })
                .collect::<Vec<_>>();
            let history = json!([{
                "info": {
                    "id": message_id,
                    "sessionID": child_id,
                    "role": "assistant",
                    "time": {"created": 1}
                },
                "parts": parts
            }]);
            assert!(
                tracker
                    .handle_history(&child_id, &history)
                    .mutations
                    .is_empty()
            );
        }

        tracker.finish_detail_baseline();
        let dormant_identity_count = child_count * parts_per_child;
        assert_eq!(
            tracker.detail_baseline_identities.len(),
            dormant_identity_count
        );
        assert!(!tracker.detail_baseline_saturated);
        assert!(
            tracker.detail_baseline_identities.len() <= MAX_DETAIL_BASELINE_IDENTITIES,
            "the supported baseline must remain strictly bounded"
        );

        let live = tracker.handle_event(&json!({
            "id": "fresh-event",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "child-0",
                "part": {
                    "id": "fresh-part",
                    "sessionID": "child-0",
                    "messageID": "message-0",
                    "type": "tool",
                    "tool": "fresh",
                    "callID": "fresh-call",
                    "state": {"status": "completed", "time": {"start": 3, "end": 4}}
                }
            }
        }));
        assert!(live.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::AppendEntry(entry)
                if entry.title == "fresh completed"
        )));
    }

    #[test]
    fn terminal_child_limit_rejects_the_first_over_bound_child_without_saturating_baseline() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        tracker.begin_detail_baseline();
        let children = (0..129)
            .map(|child_index| {
                json!({
                    "id": format!("child-{child_index}"),
                    "parentID": "root",
                    "title": format!("Child {child_index}"),
                    "time": {"created": 1}
                })
            })
            .collect::<Vec<_>>();

        let (output, accepted) =
            tracker.reconcile_children_limited("root", &Value::Array(children), 128);

        assert_eq!(output.mutations.len(), 128);
        assert_eq!(accepted.len(), 128);
        assert_eq!(accepted.first().map(String::as_str), Some("child-0"));
        assert_eq!(accepted.last().map(String::as_str), Some("child-127"));
        assert_eq!(tracker.state_counts().children, 128);
        assert!(!tracker.is_verified_child("child-128"));

        for child_index in 0..128 {
            let child_id = format!("child-{child_index}");
            let message_id = format!("message-{child_index}");
            let parts = (0..200)
                .map(|part_index| {
                    json!({
                        "id": format!("part-{part_index}"),
                        "sessionID": child_id,
                        "messageID": message_id,
                        "type": "tool",
                        "tool": "dormant",
                        "callID": format!("call-{part_index}"),
                        "state": {"status": "completed", "time": {"start": 1, "end": 2}}
                    })
                })
                .collect::<Vec<_>>();
            let history = json!([{
                "info": {
                    "id": message_id,
                    "sessionID": child_id,
                    "role": "assistant",
                    "time": {"created": 1}
                },
                "parts": parts
            }]);
            assert!(
                tracker
                    .handle_history(&child_id, &history)
                    .mutations
                    .is_empty()
            );
        }
        let rejected_history = json!([{
            "info": {
                "id": "message-128",
                "sessionID": "child-128",
                "role": "assistant",
                "time": {"created": 1}
            },
            "parts": [{
                "id": "part-0",
                "sessionID": "child-128",
                "messageID": "message-128",
                "type": "tool",
                "tool": "must-not-reconcile",
                "callID": "call-0",
                "state": {"status": "completed", "time": {"start": 1, "end": 2}}
            }]
        }]);
        assert!(
            tracker
                .handle_history("child-128", &rejected_history)
                .mutations
                .is_empty()
        );
        assert_eq!(tracker.detail_baseline_identities.len(), 25_600);
        assert!(!tracker.detail_baseline_saturated);

        tracker.finish_detail_baseline();
        let live = tracker.handle_event(&json!({
            "id": "fresh-event",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "child-0",
                "part": {
                    "id": "fresh-part",
                    "sessionID": "child-0",
                    "messageID": "message-0",
                    "type": "tool",
                    "tool": "fresh",
                    "callID": "fresh-call",
                    "state": {"status": "completed", "time": {"start": 3, "end": 4}}
                }
            }
        }));
        assert!(live.mutations.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::AppendEntry(entry)
                if entry.title == "fresh completed"
        )));
    }

    #[test]
    fn live_coverage_count_cannot_bind_before_the_byte_budget() {
        let mut stream = BoundedTextAccumulator::default();
        for _ in 0..MAX_TEXT_BYTES {
            push_live_segment(&mut stream, "x");
        }
        assert_eq!(stream.live_bytes, MAX_TEXT_BYTES);
        assert_eq!(stream.live_segments.len(), MAX_TEXT_BYTES);
        assert!(!stream.coverage_saturated);
    }

    #[test]
    fn live_coverage_marks_the_first_byte_over_budget() {
        let mut stream = BoundedTextAccumulator::default();
        for _ in 0..=MAX_TEXT_BYTES {
            push_live_segment(&mut stream, "x");
        }
        assert_eq!(stream.live_bytes, MAX_TEXT_BYTES);
        assert_eq!(stream.live_segments.len(), MAX_TEXT_BYTES);
        assert!(stream.coverage_saturated);
    }

    #[test]
    fn live_coverage_counts_utf8_bytes_at_the_budget_boundary() {
        let mut stream = BoundedTextAccumulator::default();
        for _ in 0..(MAX_TEXT_BYTES / "é".len()) {
            push_live_segment(&mut stream, "é");
        }
        assert_eq!(stream.live_bytes, MAX_TEXT_BYTES);
        assert_eq!(stream.live_segments.len(), MAX_TEXT_BYTES / "é".len());
        assert!(!stream.coverage_saturated);

        push_live_segment(&mut stream, "é");
        assert_eq!(stream.live_bytes, MAX_TEXT_BYTES);
        assert_eq!(stream.live_segments.len(), MAX_TEXT_BYTES / "é".len());
        assert!(stream.coverage_saturated);
    }

    #[test]
    fn empty_live_segments_consume_no_capacity() {
        let mut stream = BoundedTextAccumulator::default();
        push_live_segment(&mut stream, "");
        assert_eq!(stream.live_bytes, 0);
        assert!(stream.live_segments.is_empty());
        assert!(!stream.coverage_saturated);
    }

    #[test]
    fn saturation_marker_enqueue_rejection_preserves_authoritative_and_live_state() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        tracker.reconcile_children(
            "root",
            &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
        );
        tracker.handle_event(&json!({
            "id":"assistant",
            "type":"message.updated",
            "properties":{
                "sessionID":"child",
                "info":{"id":"message","sessionID":"child","role":"assistant"}
            }
        }));
        let key = ("child".to_owned(), "part".to_owned());
        let baseline = "b".repeat(MAX_TEXT_BYTES);
        let baseline_part = json!({
            "id":"part",
            "sessionID":"child",
            "messageID":"message",
            "type":"text",
            "text":baseline
        });
        assert!(
            tracker
                .handle_text_part("child", &baseline_part, 1)
                .mutations
                .is_empty()
        );
        assert_eq!(tracker.flush_text().mutations.len(), 1);

        let mut cumulative = baseline.clone();
        for index in 0..=MAX_TEXT_BYTES {
            cumulative.push('x');
            tracker.handle_event_at(
                &json!({
                    "id":format!("saturate-{index}"),
                    "type":"message.part.delta",
                    "properties":{
                        "sessionID":"child",
                        "messageID":"message",
                        "partID":"part",
                        "field":"text",
                        "delta":"x"
                    }
                }),
                u64::try_from(index).unwrap().saturating_mul(101),
            );
            tracker.flush_text();
        }
        for index in 0..MAX_PENDING_TEXT_EVENTS {
            cumulative.push('q');
            assert!(
                tracker
                    .handle_event_at(
                        &json!({
                            "id":format!("pending-{index}"),
                            "type":"message.part.delta",
                            "properties":{
                                "sessionID":"child",
                                "messageID":"message",
                                "partID":"part",
                                "field":"text",
                                "delta":"q"
                            }
                        }),
                        2_000_000,
                    )
                    .mutations
                    .is_empty()
            );
        }
        let changed_part = json!({
            "id":"part",
            "sessionID":"child",
            "messageID":"message",
            "type":"text",
            "text":cumulative
        });

        let before_rejection = tracker.message_text.get(&key).expect("text stream");
        assert_eq!(before_rejection.normalized, baseline);
        assert_eq!(before_rejection.source_bytes, MAX_TEXT_BYTES);
        assert_eq!(before_rejection.live_bytes, MAX_TEXT_BYTES);
        assert!(before_rejection.coverage_saturated);
        assert_eq!(before_rejection.pending.len(), MAX_PENDING_TEXT_EVENTS);
        assert!(
            tracker
                .handle_text_part("child", &changed_part, 2_000_001)
                .mutations
                .is_empty()
        );
        let rejected = tracker.message_text.get(&key).expect("text stream");
        assert_eq!(rejected.normalized, baseline);
        assert_eq!(rejected.source_bytes, MAX_TEXT_BYTES);
        assert_eq!(rejected.live_bytes, MAX_TEXT_BYTES);
        assert!(rejected.coverage_saturated);
        assert_eq!(rejected.pending.len(), MAX_PENDING_TEXT_EVENTS);

        assert_eq!(
            tracker.flush_text().mutations.len(),
            MAX_PENDING_TEXT_EVENTS
        );
        assert!(
            tracker
                .handle_text_part("child", &changed_part, 2_000_001)
                .mutations
                .is_empty()
        );
        let accepted = tracker.message_text.get(&key).expect("text stream");
        assert_eq!(accepted.normalized, baseline);
        assert_eq!(
            accepted.source_bytes,
            MAX_TEXT_BYTES + MAX_TEXT_BYTES + 1 + MAX_PENDING_TEXT_EVENTS,
        );
        assert!(accepted.live_segments.is_empty());
        assert_eq!(accepted.live_bytes, 0);
        assert!(!accepted.coverage_saturated);
        assert!(matches!(
            accepted.pending.as_slices(),
            ([PendingTextEntry { id, detail, .. }], [])
                if id == "opencode:part:message:part:coverage-saturated:33025:e33f24140499430b048f6600af4f41f3ccb0cb766d9f7661124cf8ba4b827523"
                    && detail == TRUNCATION_MARKER
        ));
    }

    #[test]
    fn append_only_snapshot_past_bounded_prefix_reconciles_utf8_live_suffix() {
        let mut tracker = OpenCodeActivityTracker::new("root");
        let key = ("child".to_owned(), "part".to_owned());
        let baseline = "b".repeat(MAX_TEXT_BYTES);
        let mut stream = BoundedTextAccumulator {
            normalized: baseline.clone(),
            source_bytes: MAX_TEXT_BYTES + 1,
            ..BoundedTextAccumulator::default()
        };
        push_live_segment(&mut stream, " é");
        tracker.message_text.insert(key.clone(), stream);
        let cumulative = format!("{baseline}x é");
        let part = json!({
            "id":"part",
            "sessionID":"child",
            "messageID":"message",
            "type":"text",
            "text":cumulative
        });

        assert!(
            tracker
                .handle_text_part("child", &part, 3)
                .mutations
                .is_empty()
        );
        let reconciled = tracker.message_text.get(&key).expect("text stream");
        assert_eq!(reconciled.source_bytes, MAX_TEXT_BYTES + 4);
        assert!(reconciled.live_segments.is_empty());
        assert_eq!(reconciled.live_bytes, 0);
        assert!(reconciled.pending.is_empty());
    }

    #[test]
    fn activity_output_accepts_exactly_256_mutations_and_returns_the_257th_for_retry() {
        let mut output = OpenCodeActivityOutput::default();
        for index in 0..MAX_MUTATIONS {
            assert!(
                output
                    .push(ProviderActivityMutation::RemoveActor {
                        actor_id: format!("actor-{index}"),
                    })
                    .is_ok()
            );
        }

        let deferred = output
            .push(ProviderActivityMutation::RemoveActor {
                actor_id: "actor-deferred".to_owned(),
            })
            .expect_err("the 257th mutation must be returned rather than discarded");
        assert_eq!(output.mutations.len(), MAX_MUTATIONS);

        let mut retry = OpenCodeActivityOutput::default();
        assert!(retry.push(deferred).is_ok());
        assert_eq!(retry.mutations.len(), 1);

        let mut extended = OpenCodeActivityOutput::default();
        for index in 0..(MAX_MUTATIONS - 1) {
            assert!(
                extended
                    .push(ProviderActivityMutation::RemoveActor {
                        actor_id: format!("extended-{index}"),
                    })
                    .is_ok()
            );
        }
        let deferred = extended.extend(vec![
            ProviderActivityMutation::RemoveActor {
                actor_id: "extended-accepted".to_owned(),
            },
            ProviderActivityMutation::RemoveActor {
                actor_id: "extended-deferred".to_owned(),
            },
        ]);
        assert_eq!(extended.mutations.len(), MAX_MUTATIONS);
        assert!(matches!(
            deferred.as_slice(),
            [ProviderActivityMutation::RemoveActor { actor_id }]
                if actor_id == "extended-deferred"
        ));
    }

    #[test]
    fn bounded_text_drain_cannot_starve_a_later_stream_under_sustained_replenishment() {
        fn pending(stream: &str, index: usize) -> PendingTextEntry {
            PendingTextEntry {
                id: format!("opencode:part:message:{stream}:{index}"),
                semantic: format!("pending:{stream}:{index}"),
                detail: format!("{stream}-{index}"),
                at_ms: u64::try_from(index + 1).expect("small fixture index"),
                created_at: None,
                snapshot_base: None,
            }
        }

        let mut tracker = OpenCodeActivityTracker::new("root");
        let a_key = ("child".to_owned(), "a".to_owned());
        let z_key = ("child".to_owned(), "z".to_owned());
        tracker.message_text.insert(
            a_key.clone(),
            BoundedTextAccumulator {
                pending: (0..4).map(|index| pending("a", index)).collect(),
                ..BoundedTextAccumulator::default()
            },
        );
        tracker.message_text.insert(
            z_key,
            BoundedTextAccumulator {
                pending: VecDeque::from([pending("z", 0)]),
                ..BoundedTextAccumulator::default()
            },
        );

        let first = tracker.flush_text_bounded(4);
        assert_eq!(first.mutations.len(), 4);
        tracker
            .message_text
            .get_mut(&a_key)
            .expect("a stream")
            .pending
            .extend((4..8).map(|index| pending("a", index)));

        let second = tracker.flush_text_bounded(4);
        assert!(
            second.mutations.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("z-0")
            )),
            "a retained stream replenished every slice must not starve z"
        );
    }

    #[test]
    fn observation_sequence_exhaustion_never_commits_a_timestamp_past_rfc3339() {
        let formatter_ceiling_ns = 253_402_300_799_999_999_999_i128;
        let formatter_ceiling_ms = 253_402_300_799_999_u64;

        let exhausted =
            sequence_observation_timestamp(Some(formatter_ceiling_ns), formatter_ceiling_ms)
                .expect("the valid formatter ceiling remains representable");
        assert_eq!(exhausted.unix_nanos, formatter_ceiling_ns);
        assert_eq!(exhausted.created_at, "9999-12-31T23:59:59.999999999Z");
        assert_ne!(exhausted.created_at, epoch());

        let repeated =
            sequence_observation_timestamp(Some(exhausted.unix_nanos), formatter_ceiling_ms)
                .expect("repeated post-boundary observations remain representable");
        assert_eq!(repeated.unix_nanos, formatter_ceiling_ns);
        assert_eq!(repeated.created_at, exhausted.created_at);
    }
}
