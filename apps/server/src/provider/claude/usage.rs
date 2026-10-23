use serde::Serialize;
use serde_json::{Map, Value};

const JAVASCRIPT_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeTokenUsageSnapshot {
    pub used_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_processed_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacts_automatically: Option<bool>,
    /// The context size at which Claude compacts automatically (`autoCompactThreshold`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_compact_tokens: Option<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct ClaudeTokenUsageState {
    last_good: Option<ClaudeTokenUsageSnapshot>,
    total_processed_tokens: Option<u64>,
    max_tokens: Option<u64>,
    auto_compact_tokens: Option<u64>,
    last_emitted: Option<ClaudeTokenUsageSnapshot>,
    active_observed_this_turn: bool,
}

impl ClaudeTokenUsageState {
    pub(crate) fn start_turn(&mut self) {
        self.last_emitted = None;
        self.active_observed_this_turn = false;
    }

    pub(crate) fn observe_stream_value(
        &mut self,
        value: &Value,
    ) -> Option<ClaudeTokenUsageSnapshot> {
        match value.get("type").and_then(Value::as_str) {
            Some("stream_event") => self.observe_stream_event(value),
            Some("system") => self.observe_system_value(value),
            Some("result") => self.observe_result_value(value),
            _ => None,
        }
    }

    pub(crate) fn observe_context_response(
        &mut self,
        value: &Value,
    ) -> Option<ClaudeTokenUsageSnapshot> {
        let used_tokens = positive_integer(value.get("totalTokens"))?;
        if let Some(max_tokens) = positive_integer(value.get("maxTokens")) {
            self.max_tokens = Some(max_tokens);
        }
        // Absent when Claude does not compact automatically, so its absence clears it.
        self.auto_compact_tokens = positive_integer(value.get("autoCompactThreshold"));
        let compacts_automatically = value
            .get("isAutoCompactEnabled")
            .and_then(Value::as_bool)
            .or_else(|| {
                self.last_good
                    .as_ref()
                    .and_then(|usage| usage.compacts_automatically)
            });
        self.replace_active(ActiveUsage {
            used_tokens,
            input_tokens: None,
            output_tokens: None,
            last_used_tokens: None,
            tool_uses: None,
            duration_ms: None,
            compacts_automatically,
        })
    }

    fn observe_stream_event(&mut self, value: &Value) -> Option<ClaudeTokenUsageSnapshot> {
        if value
            .get("parent_tool_use_id")
            .is_some_and(|parent| !parent.is_null())
        {
            return None;
        }
        let event = value.get("event")?.as_object()?;
        if event.get("type").and_then(Value::as_str) != Some("message_delta") {
            return None;
        }
        let usage = event.get("usage")?.as_object()?;
        let active = active_usage(usage)?;
        self.replace_active(active)
    }

    fn observe_system_value(&mut self, value: &Value) -> Option<ClaudeTokenUsageSnapshot> {
        match value.get("subtype").and_then(Value::as_str) {
            Some("task_progress" | "task_notification") => self.observe_task_usage(value),
            Some("compact_boundary") => self.observe_compact_boundary(value),
            _ => None,
        }
    }

    fn observe_task_usage(&mut self, value: &Value) -> Option<ClaudeTokenUsageSnapshot> {
        let usage = value.get("usage")?.as_object()?;
        let total_tokens = usage_total_tokens(usage)?;
        let current_tokens = self
            .last_good
            .as_ref()
            .map_or(0, |last_good| last_good.used_tokens);
        if self.active_observed_this_turn && total_tokens <= current_tokens {
            return None;
        }
        self.update_total_processed_tokens(total_tokens);
        self.replace_active(ActiveUsage {
            used_tokens: total_tokens.max(current_tokens),
            input_tokens: None,
            output_tokens: None,
            last_used_tokens: None,
            tool_uses: non_negative_integer(usage.get("tool_uses")),
            duration_ms: non_negative_integer(usage.get("duration_ms")),
            compacts_automatically: self
                .last_good
                .as_ref()
                .and_then(|last_good| last_good.compacts_automatically),
        })
    }

    fn observe_compact_boundary(&mut self, value: &Value) -> Option<ClaudeTokenUsageSnapshot> {
        let metadata = value.get("compact_metadata")?.as_object()?;
        let post_tokens = positive_integer(metadata.get("post_tokens"))?;
        self.replace_active(ActiveUsage {
            used_tokens: post_tokens,
            input_tokens: None,
            output_tokens: None,
            last_used_tokens: non_negative_integer(metadata.get("pre_tokens")),
            tool_uses: None,
            duration_ms: None,
            compacts_automatically: self
                .last_good
                .as_ref()
                .and_then(|last_good| last_good.compacts_automatically),
        })
    }

    fn observe_result_value(&mut self, value: &Value) -> Option<ClaudeTokenUsageSnapshot> {
        if let Some(max_tokens) = max_model_context_window(value) {
            self.max_tokens = Some(max_tokens);
        }

        let usage = value.get("usage").and_then(Value::as_object);
        if let Some(total_tokens) = usage.and_then(usage_total_tokens) {
            self.update_total_processed_tokens(total_tokens);
        }

        if let Some(active) = usage.and_then(result_active_usage) {
            return self.replace_active(active);
        }
        if self.active_observed_this_turn {
            self.refresh_last_good()
        } else {
            None
        }
    }

    fn replace_active(&mut self, active: ActiveUsage) -> Option<ClaudeTokenUsageSnapshot> {
        if active.used_tokens == 0 || active.used_tokens > JAVASCRIPT_MAX_SAFE_INTEGER {
            return None;
        }
        let used_tokens = self.max_tokens.map_or(active.used_tokens, |max_tokens| {
            active.used_tokens.min(max_tokens)
        });
        let last_used_tokens = active
            .last_used_tokens
            .filter(|tokens| *tokens <= JAVASCRIPT_MAX_SAFE_INTEGER)
            .or(Some(used_tokens));
        let compacts_automatically = active.compacts_automatically.or_else(|| {
            self.last_good
                .as_ref()
                .and_then(|usage| usage.compacts_automatically)
        });
        let snapshot = ClaudeTokenUsageSnapshot {
            used_tokens,
            total_processed_tokens: self
                .total_processed_tokens
                .filter(|total| *total > used_tokens),
            max_tokens: self.max_tokens,
            input_tokens: active.input_tokens.filter(|tokens| *tokens > 0),
            output_tokens: active.output_tokens.filter(|tokens| *tokens > 0),
            last_used_tokens,
            tool_uses: active.tool_uses,
            duration_ms: active.duration_ms,
            compacts_automatically,
            auto_compact_tokens: self.auto_compact_tokens,
        };
        self.last_good = Some(snapshot.clone());
        self.active_observed_this_turn = true;
        self.emit_if_changed(snapshot)
    }

    fn refresh_last_good(&mut self) -> Option<ClaudeTokenUsageSnapshot> {
        let mut snapshot = self.last_good.clone()?;
        if let Some(max_tokens) = self.max_tokens {
            snapshot.used_tokens = snapshot.used_tokens.min(max_tokens);
            snapshot.max_tokens = Some(max_tokens);
        }
        snapshot.auto_compact_tokens = self.auto_compact_tokens;
        snapshot.total_processed_tokens = self
            .total_processed_tokens
            .filter(|total| *total > snapshot.used_tokens);
        self.last_good = Some(snapshot.clone());
        self.emit_if_changed(snapshot)
    }

    fn update_total_processed_tokens(&mut self, total_tokens: u64) {
        self.total_processed_tokens = Some(
            self.total_processed_tokens
                .map_or(total_tokens, |current| current.max(total_tokens)),
        );
    }

    fn emit_if_changed(
        &mut self,
        snapshot: ClaudeTokenUsageSnapshot,
    ) -> Option<ClaudeTokenUsageSnapshot> {
        if self.last_emitted.as_ref() == Some(&snapshot) {
            return None;
        }
        self.last_emitted = Some(snapshot.clone());
        Some(snapshot)
    }
}

#[derive(Debug)]
struct ActiveUsage {
    used_tokens: u64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    last_used_tokens: Option<u64>,
    tool_uses: Option<u64>,
    duration_ms: Option<u64>,
    compacts_automatically: Option<bool>,
}

fn non_negative_integer(value: Option<&Value>) -> Option<u64> {
    value?
        .as_u64()
        .filter(|value| *value <= JAVASCRIPT_MAX_SAFE_INTEGER)
}

fn positive_integer(value: Option<&Value>) -> Option<u64> {
    non_negative_integer(value).filter(|value| *value > 0)
}

fn usage_input_tokens(usage: &Map<String, Value>) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .into_iter()
    .filter_map(|field| non_negative_integer(usage.get(field)))
    .try_fold(0_u64, |total, value| total.checked_add(value))
    .filter(|total| *total <= JAVASCRIPT_MAX_SAFE_INTEGER)
    .unwrap_or(JAVASCRIPT_MAX_SAFE_INTEGER + 1)
}

fn usage_output_tokens(usage: &Map<String, Value>) -> u64 {
    non_negative_integer(usage.get("output_tokens")).unwrap_or(0)
}

fn usage_total_tokens(usage: &Map<String, Value>) -> Option<u64> {
    if !usage_token_fields_are_valid(usage) {
        return None;
    }
    if let Some(total_tokens) = positive_integer(usage.get("total_tokens")) {
        return Some(total_tokens);
    }
    let input_tokens = usage_input_tokens(usage);
    let output_tokens = usage_output_tokens(usage);
    input_tokens
        .checked_add(output_tokens)
        .filter(|total| *total > 0 && *total <= JAVASCRIPT_MAX_SAFE_INTEGER)
}

fn usage_token_fields_are_valid(usage: &Map<String, Value>) -> bool {
    [
        "total_tokens",
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ]
    .into_iter()
    .all(|field| !usage.contains_key(field) || non_negative_integer(usage.get(field)).is_some())
}

fn max_model_context_window(value: &Value) -> Option<u64> {
    value
        .get("modelUsage")?
        .as_object()?
        .values()
        .filter_map(|usage| positive_integer(usage.get("contextWindow")))
        .max()
}

fn active_usage(usage: &Map<String, Value>) -> Option<ActiveUsage> {
    let input_tokens = usage_input_tokens(usage);
    let output_tokens = usage_output_tokens(usage);
    let used_tokens = usage_total_tokens(usage)?;
    Some(ActiveUsage {
        used_tokens,
        input_tokens: (input_tokens <= JAVASCRIPT_MAX_SAFE_INTEGER).then_some(input_tokens),
        output_tokens: Some(output_tokens),
        last_used_tokens: None,
        tool_uses: None,
        duration_ms: None,
        compacts_automatically: None,
    })
}

fn result_active_usage(usage: &Map<String, Value>) -> Option<ActiveUsage> {
    if !usage_token_fields_are_valid(usage) {
        return None;
    }
    if let Some(iteration) = usage
        .get("iterations")
        .and_then(Value::as_array)
        .and_then(|iterations| iterations.iter().rev().find_map(Value::as_object))
    {
        return active_usage(iteration);
    }

    let input_tokens = usage_input_tokens(usage);
    let output_tokens = usage_output_tokens(usage);
    let used_tokens = input_tokens
        .checked_add(output_tokens)
        .filter(|total| *total > 0 && *total <= JAVASCRIPT_MAX_SAFE_INTEGER)?;
    Some(ActiveUsage {
        used_tokens,
        input_tokens: Some(input_tokens),
        output_tokens: Some(output_tokens),
        last_used_tokens: None,
        tool_uses: None,
        duration_ms: None,
        compacts_automatically: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_context_response_auto_compact_threshold_reaches_the_usage_snapshot() {
        let mut state = ClaudeTokenUsageState::default();
        let usage = state
            .observe_context_response(&json!({
                "totalTokens": 961_230, "maxTokens": 1_000_000,
                "autoCompactThreshold": 967_000, "isAutoCompactEnabled": true
            }))
            .expect("usage");
        assert_eq!(usage.auto_compact_tokens, Some(967_000));
        let serialized = serde_json::to_value(&usage).expect("snapshot");
        assert_eq!(serialized["autoCompactTokens"], 967_000);

        // Later stream usage keeps the threshold, like the window size.
        state.start_turn();
        let streamed = state
            .observe_stream_value(&json!({
                "type": "stream_event", "parent_tool_use_id": null,
                "event": { "type": "message_delta", "usage": { "input_tokens": 962_000, "output_tokens": 10 } }
            }))
            .expect("usage");
        assert_eq!(streamed.auto_compact_tokens, Some(967_000));

        // A response without one (automatic compaction off) drops it.
        let off = state
            .observe_context_response(&json!({
                "totalTokens": 962_010, "maxTokens": 1_000_000, "isAutoCompactEnabled": false
            }))
            .expect("usage");
        assert_eq!(off.auto_compact_tokens, None);
    }
}
