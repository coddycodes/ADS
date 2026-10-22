//! How a failed Claude turn is described and classified.
//!
//! Claude Code reports a failed run in up to three places: the `result` frame's `errors`,
//! `terminal_reason` and `api_error_status`; a synthetic assistant message whose `error` names
//! the failure class and whose text is Claude's own description of it; and, while it retried,
//! `system`/`api_retry` frames. Shapes are those of the Agent SDK (`SDKResultMessage`,
//! `SDKAssistantMessage.error`), checked against Claude Code 2.1.306.

use super::protocol::{AssistantContent, AssistantMessage, ResultMessage};

/// What a documented `terminal_reason` means for the turn that ended with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaudeTerminalReasonClass {
    /// The conversation no longer fits Claude's context window.
    ContextLimit,
    /// Any other documented reason. Its name is still the failure's description.
    Other,
}

/// Every `terminal_reason` Claude Code 2.1.306 can report, with its class. Claude's own
/// recovery table treats the first three as `{ kind: "stop", cause: "context_limit" }`.
/// A value missing here is a newer one: it is classified [`ClaudeTerminalReasonClass::Other`]
/// and keeps being shown by name.
const TERMINAL_REASONS: &[(&str, ClaudeTerminalReasonClass)] = &[
    ("blocking_limit", ClaudeTerminalReasonClass::ContextLimit),
    ("prompt_too_long", ClaudeTerminalReasonClass::ContextLimit),
    (
        "rapid_refill_breaker",
        ClaudeTerminalReasonClass::ContextLimit,
    ),
    ("image_error", ClaudeTerminalReasonClass::Other),
    ("model_error", ClaudeTerminalReasonClass::Other),
    ("api_error", ClaudeTerminalReasonClass::Other),
    (
        "malformed_tool_use_exhausted",
        ClaudeTerminalReasonClass::Other,
    ),
    ("aborted_streaming", ClaudeTerminalReasonClass::Other),
    ("aborted_tools", ClaudeTerminalReasonClass::Other),
    ("stop_hook_prevented", ClaudeTerminalReasonClass::Other),
    ("hook_stopped", ClaudeTerminalReasonClass::Other),
    ("tool_deferred", ClaudeTerminalReasonClass::Other),
    ("max_turns", ClaudeTerminalReasonClass::Other),
    ("background_requested", ClaudeTerminalReasonClass::Other),
    ("completed", ClaudeTerminalReasonClass::Other),
    ("budget_exhausted", ClaudeTerminalReasonClass::Other),
    (
        "structured_output_retry_exhausted",
        ClaudeTerminalReasonClass::Other,
    ),
    (
        "tool_deferred_unavailable",
        ClaudeTerminalReasonClass::Other,
    ),
    ("turn_setup_failed", ClaudeTerminalReasonClass::Other),
];

pub(crate) fn terminal_reason_class(reason: &str) -> ClaudeTerminalReasonClass {
    TERMINAL_REASONS
        .iter()
        .find(|(name, _)| *name == reason)
        .map_or(ClaudeTerminalReasonClass::Other, |(_, class)| *class)
}

/// The failure a synthetic assistant message reported during the turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClaudeAssistantError {
    /// `SDKAssistantMessage.error`, such as `invalid_request` or `rate_limit`.
    pub(crate) kind: String,
    /// The message's text: Claude's description of the failure.
    pub(crate) text: Option<String>,
}

impl ClaudeAssistantError {
    pub(crate) fn from_message(message: &AssistantMessage) -> Option<Self> {
        let kind = message
            .error
            .as_deref()
            .map(str::trim)
            .filter(|kind| !kind.is_empty())?
            .to_owned();
        let text = message
            .message
            .content
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text { text } => Some(text.trim()),
                AssistantContent::ToolUse { .. } => None,
            })
            .find(|text| !text.is_empty())
            .map(str::to_owned);
        Some(Self { kind, text })
    }

    /// The canonical error class for this failure. Credential failures read as a rejection of
    /// the credentials; everything else is a provider-reported error.
    fn error_class(&self) -> &'static str {
        match self.kind.as_str() {
            "authentication_failed"
            | "oauth_org_not_allowed"
            | "verification_required"
            | "cloud_credential_error" => "permission_error",
            _ => "provider_error",
        }
    }
}

/// The error class of a turn that failed because the conversation filled Claude's context
/// window. The thread offers to compact the conversation.
pub(crate) const CONTEXT_LIMIT_ERROR_CLASS: &str = "context_limit";

/// What a context-limit failure tells the user to do.
const CONTEXT_LIMIT_MESSAGE: &str =
    "Claude's context window is full. Compact the conversation, then send your message again.";

/// Claude Code's text for a request it refused as too long, also the start of its
/// `Prompt is too long · automatic compaction failed: <cause>` form.
const PROMPT_TOO_LONG: &str = "Prompt is too long";

/// The description and class of a failed turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClaudeTurnFailure {
    pub(crate) message: String,
    pub(crate) error_class: &'static str,
}

/// Describe a failed (not interrupted) turn from everything Claude reported about it, most
/// specific first: the result's `errors`, the assistant error's text, the upstream API error it
/// was retrying, then the result's `terminal_reason` (or `subtype`) by name.
pub(crate) fn describe_failed_turn(
    result: &ResultMessage,
    assistant_error: Option<&ClaudeAssistantError>,
    api_error: Option<&str>,
) -> ClaudeTurnFailure {
    if is_context_limit(result, assistant_error) {
        return context_limit_failure(result, assistant_error);
    }
    let error_class = assistant_error.map_or("provider_error", ClaudeAssistantError::error_class);
    if let Some(error) = result
        .errors
        .iter()
        .map(|error| error.trim())
        .find(|error| !error.is_empty())
    {
        return ClaudeTurnFailure {
            message: error.to_owned(),
            error_class,
        };
    }
    if let Some(text) = assistant_error.and_then(|error| error.text.as_deref()) {
        return ClaudeTurnFailure {
            message: sentence(text),
            error_class,
        };
    }
    if let Some(api_error) = api_error {
        return ClaudeTurnFailure {
            message: sentence(api_error),
            error_class,
        };
    }
    ClaudeTurnFailure {
        message: named_failure("Claude turn failed", result),
        error_class,
    }
}

/// Claude stopped because the conversation no longer fits its context window: a context-limit
/// `terminal_reason`, or (for a version that ends such a run with another reason) the synthetic
/// `invalid_request` message Claude writes for a prompt that is too long.
fn is_context_limit(
    result: &ResultMessage,
    assistant_error: Option<&ClaudeAssistantError>,
) -> bool {
    let reason_is_context_limit = result
        .terminal_reason
        .as_deref()
        .map(|reason| terminal_reason_class(reason.trim()))
        == Some(ClaudeTerminalReasonClass::ContextLimit);
    reason_is_context_limit
        || assistant_error.is_some_and(|error| {
            error.kind == "invalid_request"
                && error
                    .text
                    .as_deref()
                    .is_some_and(|text| text.starts_with(PROMPT_TOO_LONG))
        })
}

/// The actionable context-limit failure, keeping Claude's own description of it (which names
/// the cause when Claude's automatic compaction failed).
fn context_limit_failure(
    result: &ResultMessage,
    assistant_error: Option<&ClaudeAssistantError>,
) -> ClaudeTurnFailure {
    let claude_text = result
        .errors
        .iter()
        .map(|error| error.trim())
        .find(|error| !error.is_empty())
        .or_else(|| assistant_error.and_then(|error| error.text.as_deref()));
    let message = match claude_text {
        Some(text) => format!(
            "{CONTEXT_LIMIT_MESSAGE} Claude reported: {}",
            sentence(text)
        ),
        None => CONTEXT_LIMIT_MESSAGE.to_owned(),
    };
    ClaudeTurnFailure {
        message,
        error_class: CONTEXT_LIMIT_ERROR_CLASS,
    }
}

/// `"<reason> (<terminal_reason or subtype>[, HTTP <status>])."`
pub(crate) fn named_failure(reason: &str, result: &ResultMessage) -> String {
    let name = result
        .terminal_reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(result.subtype.trim());
    let detail = match (name.is_empty(), result.api_error_status) {
        (true, None) => return format!("{reason}."),
        (true, Some(status)) => format!("HTTP {status}"),
        (false, None) => name.to_owned(),
        (false, Some(status)) => format!("{name}, HTTP {status}"),
    };
    format!("{reason} ({detail}).")
}

fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn result(value: serde_json::Value) -> ResultMessage {
        let mut frame = json!({
            "subtype": "success", "is_error": true, "stop_reason": null,
            "session_id": "session", "uuid": "result"
        });
        frame
            .as_object_mut()
            .expect("object")
            .extend(value.as_object().expect("object").clone());
        serde_json::from_value(frame).expect("result frame")
    }

    #[test]
    fn every_context_limit_reason_is_classified_and_new_reasons_fall_back() {
        for reason in ["blocking_limit", "prompt_too_long", "rapid_refill_breaker"] {
            assert_eq!(
                terminal_reason_class(reason),
                ClaudeTerminalReasonClass::ContextLimit,
                "{reason}"
            );
        }
        assert_eq!(TERMINAL_REASONS.len(), 19);
        assert_eq!(
            terminal_reason_class("max_turns"),
            ClaudeTerminalReasonClass::Other
        );
        assert_eq!(
            terminal_reason_class("a_reason_from_a_newer_claude"),
            ClaudeTerminalReasonClass::Other
        );
    }

    #[test]
    fn an_api_error_status_is_named_when_nothing_else_describes_the_failure() {
        let failure = describe_failed_turn(
            &result(json!({ "terminal_reason": "api_error", "api_error_status": 500 })),
            None,
            None,
        );
        assert_eq!(failure.message, "Claude turn failed (api_error, HTTP 500).");
        assert_eq!(failure.error_class, "provider_error");
    }

    #[test]
    fn credential_failures_are_classified_as_rejected_credentials() {
        let error = ClaudeAssistantError {
            kind: "authentication_failed".to_owned(),
            text: Some("Invalid API key · Please run /login".to_owned()),
        };
        let failure = describe_failed_turn(&result(json!({})), Some(&error), None);
        assert_eq!(failure.message, "Invalid API key · Please run /login.");
        assert_eq!(failure.error_class, "permission_error");
    }

    /// Claude Code 2.1.306's `blocking_limit` stop, synthesized from its builder: a synthetic
    /// assistant message (`error: "invalid_request"`, text `Prompt is too long`), then an error
    /// result with no `errors` whose `terminal_reason` is `blocking_limit`.
    pub(crate) fn blocking_limit_frames(text: &str) -> [serde_json::Value; 2] {
        [
            json!({
                "type": "assistant", "session_id": "session", "uuid": "assistant-error",
                "parent_tool_use_id": null, "error": "invalid_request",
                "message": {
                    "id": "msg-error", "type": "message", "role": "assistant",
                    "content": [{ "type": "text", "text": text }]
                }
            }),
            json!({
                "type": "result", "subtype": "success", "is_error": true,
                "terminal_reason": "blocking_limit", "stop_reason": "stop_sequence",
                "num_turns": 1, "result": text,
                "session_id": "session", "uuid": "result-blocking"
            }),
        ]
    }

    pub(crate) fn runtime_with_turn() -> crate::provider::claude::ClaudeProviderRuntime {
        let mut runtime = crate::provider::claude::ClaudeProviderRuntime::new(
            "thread".to_owned(),
            "session".to_owned(),
        );
        runtime.start_turn(crate::provider::claude::TurnInput {
            turn_id: "turn".to_owned(),
            input: "hello".to_owned(),
        });
        runtime
    }

    pub(crate) fn completion(
        runtime: &mut crate::provider::claude::ClaudeProviderRuntime,
        frames: &[serde_json::Value],
    ) -> serde_json::Value {
        let mut completed = None;
        for frame in frames {
            for event in runtime.handle_raw_value(frame, 1_000).events {
                if event.event_type == "turn.completed" {
                    completed = Some(event.payload);
                }
            }
        }
        completed.expect("the turn completes")
    }

    fn max_turns_result() -> serde_json::Value {
        json!({
            "type": "result", "subtype": "error_max_turns", "is_error": true,
            "terminal_reason": "max_turns", "stop_reason": null,
            "session_id": "session", "uuid": "result-2"
        })
    }

    fn status(status: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
        let mut frame = json!({
            "type": "system", "subtype": "status", "status": status,
            "session_id": "session", "uuid": "status"
        });
        frame
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        frame
    }

    #[test]
    fn a_failed_turn_carries_the_assistant_error_text_instead_of_the_terminal_reason() {
        let mut runtime = runtime_with_turn();
        let [assistant, mut result] = blocking_limit_frames("API Error: 400 bad request");
        result["terminal_reason"] = json!("api_error");
        let payload = completion(&mut runtime, &[assistant, result]);
        assert_eq!(payload["state"], "failed");
        assert_eq!(payload["errorMessage"], "API Error: 400 bad request.");
        assert_eq!(payload["errorClass"], "provider_error");
    }

    #[test]
    fn a_context_limit_stop_is_an_actionable_context_limit_failure() {
        for reason in ["blocking_limit", "prompt_too_long", "rapid_refill_breaker"] {
            let mut runtime = runtime_with_turn();
            let [assistant, mut result] = blocking_limit_frames("Prompt is too long");
            result["terminal_reason"] = json!(reason);
            let payload = completion(&mut runtime, &[assistant, result]);
            assert_eq!(payload["state"], "failed", "{reason}");
            assert_eq!(payload["errorClass"], "context_limit", "{reason}");
            assert_eq!(
                payload["errorMessage"],
                "Claude's context window is full. Compact the conversation, then send your \
                 message again. Claude reported: Prompt is too long.",
                "{reason}"
            );
        }
    }

    #[test]
    fn a_context_limit_failure_keeps_why_automatic_compaction_failed() {
        let mut runtime = runtime_with_turn();
        let payload = completion(
            &mut runtime,
            &blocking_limit_frames(
                "Prompt is too long · automatic compaction failed: the summary request timed out",
            ),
        );
        assert_eq!(payload["errorClass"], "context_limit");
        let message = payload["errorMessage"].as_str().expect("error message");
        assert!(
            message.ends_with("automatic compaction failed: the summary request timed out."),
            "{message:?}"
        );
    }

    #[test]
    fn a_prompt_too_long_error_is_a_context_limit_under_another_terminal_reason() {
        let mut runtime = runtime_with_turn();
        let [assistant, mut result] = blocking_limit_frames("Prompt is too long");
        result
            .as_object_mut()
            .expect("object")
            .remove("terminal_reason");
        let payload = completion(&mut runtime, &[assistant, result]);
        assert_eq!(payload["errorClass"], "context_limit");
    }

    #[test]
    fn an_assistant_error_does_not_leak_into_a_later_turn() {
        let mut runtime = runtime_with_turn();
        let [assistant, _] = blocking_limit_frames("Prompt is too long");
        runtime.handle_raw_value(&assistant, 1_000);
        runtime.start_turn(crate::provider::claude::TurnInput {
            turn_id: "turn-2".to_owned(),
            input: "again".to_owned(),
        });
        let payload = completion(&mut runtime, &[max_turns_result()]);
        assert_eq!(payload["errorMessage"], "Claude turn failed (max_turns).");
    }

    #[test]
    fn a_subagent_error_message_is_not_the_turn_failure() {
        let mut runtime = runtime_with_turn();
        let [mut assistant, _] = blocking_limit_frames("Prompt is too long");
        assistant["parent_tool_use_id"] = json!("tool-agent");
        runtime.handle_raw_value(&assistant, 1_000);
        let payload = completion(&mut runtime, &[max_turns_result()]);
        assert_eq!(payload["errorMessage"], "Claude turn failed (max_turns).");
    }

    #[test]
    fn compaction_status_is_shown_as_a_conversation_compaction_item() {
        let mut runtime = runtime_with_turn();
        let started = runtime.handle_raw_value(&status(json!("compacting"), json!({})), 1_000);
        let [started] = started.events.as_slice() else {
            panic!("one event: {:?}", started.events);
        };
        assert_eq!(started.event_type, "item.started");
        assert_eq!(started.turn_id.as_deref(), Some("turn"));
        assert_eq!(started.payload["itemType"], "context_compaction");
        assert_eq!(started.payload["status"], "inProgress");
        assert_eq!(started.payload["title"], "Conversation compaction");
        let item = started.provider_refs.clone().expect("item reference");

        // A repeated `compacting` (an automatic compaction's progress) opens no second item.
        let repeated = runtime.handle_raw_value(&status(json!("compacting"), json!({})), 1_000);
        assert!(repeated.events.is_empty(), "{:?}", repeated.events);
        // `requesting` is not a compaction status and opens nothing either.
        let mut idle = runtime_with_turn();
        let requesting = idle.handle_raw_value(&status(json!("requesting"), json!({})), 1_000);
        assert!(requesting.events.is_empty(), "{:?}", requesting.events);

        let ended = runtime.handle_raw_value(
            &status(json!(null), json!({ "compact_result": "success" })),
            1_000,
        );
        let [ended] = ended.events.as_slice() else {
            panic!("one event: {:?}", ended.events);
        };
        assert_eq!(ended.event_type, "item.completed");
        assert_eq!(ended.provider_refs.as_ref(), Some(&item));
        assert_eq!(ended.payload["status"], "completed");
    }

    #[test]
    fn a_failed_compaction_is_shown_failed_with_claude_reason() {
        let mut runtime = runtime_with_turn();
        runtime.handle_raw_value(&status(json!("compacting"), json!({})), 1_000);
        let ended = runtime.handle_raw_value(
            &status(
                json!(null),
                json!({
                    "compact_result": "failed",
                    "compact_error": "Not enough messages to compact."
                }),
            ),
            1_000,
        );
        let [ended] = ended.events.as_slice() else {
            panic!("one event: {:?}", ended.events);
        };
        assert_eq!(ended.payload["status"], "failed");
        assert_eq!(ended.payload["detail"], "Not enough messages to compact.");
    }

    #[test]
    fn a_compaction_still_open_when_the_turn_ends_is_completed_with_it() {
        let mut runtime = runtime_with_turn();
        runtime.handle_raw_value(&status(json!("compacting"), json!({})), 1_000);
        let output = runtime.handle_raw_value(
            &json!({
                "type": "result", "subtype": "success", "is_error": false,
                "stop_reason": null, "session_id": "session", "uuid": "result"
            }),
            1_000,
        );
        let types = output
            .events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>();
        assert_eq!(types, ["item.completed", "turn.completed"]);
    }
}
