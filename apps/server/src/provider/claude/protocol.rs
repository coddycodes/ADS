use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeMessage {
    StreamEvent(StreamEventMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    Result(ResultMessage),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamEventMessage {
    pub session_id: String,
    pub uuid: String,
    pub parent_tool_use_id: Option<String>,
    pub event: StreamEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: StreamMessageStart,
    },
    ContentBlockStart {
        index: u64,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u64,
        delta: ContentBlockDelta,
    },
    ContentBlockStop {
        index: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamMessageStart {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    ThinkingDelta { thinking: String },
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub session_id: String,
    pub uuid: String,
    pub parent_tool_use_id: Option<String>,
    pub message: UserPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserPayload {
    pub role: String,
    #[serde(default)]
    pub content: Vec<UserContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    ToolResult { tool_use_id: String, content: Value },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub session_id: String,
    pub uuid: String,
    pub parent_tool_use_id: Option<String>,
    pub message: AssistantPayload,
    /// Set on the synthetic message Claude writes when a request fails (`SDKAssistantMessage.error`:
    /// `invalid_request`, `rate_limit`, `billing_error`, ...). The message's text is then Claude's
    /// own description of the failure, such as `Prompt is too long`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantPayload {
    pub id: Option<String>,
    #[serde(default)]
    pub content: Vec<AssistantContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultMessage {
    pub subtype: String,
    pub is_error: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    /// Claude's own name for why the run ended (e.g. `max_turns`). Present on
    /// error results that carry no `errors`, where it is the only description
    /// of the failure.
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// HTTP status of the API error that ended the run, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<i64>,
    pub stop_reason: Option<String>,
    pub session_id: String,
    pub uuid: String,
}

#[derive(Clone, PartialEq, Deserialize)]
pub(crate) struct ClaudeControlResponseFrame {
    pub response: ClaudeControlResponse,
}

#[derive(Clone, PartialEq, Deserialize)]
pub(crate) struct ClaudeControlResponse {
    pub subtype: String,
    pub request_id: String,
    #[serde(default)]
    pub response: Value,
    pub error: Option<String>,
}

impl std::fmt::Debug for ClaudeControlResponseFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaudeControlResponseFrame { .. }")
    }
}

impl std::fmt::Debug for ClaudeControlResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaudeControlResponse { .. }")
    }
}

#[derive(Deserialize)]
pub(crate) struct ClaudeTaskStartedMessage {
    pub(crate) session_id: String,
    pub(crate) task_id: String,
    pub(crate) tool_use_id: String,
    pub(crate) task_type: Option<String>,
}

impl std::fmt::Debug for ClaudeTaskStartedMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaudeTaskStartedMessage { .. }")
    }
}

#[derive(Deserialize)]
pub(crate) struct ClaudeTaskNotificationMessage {
    pub(crate) session_id: String,
    pub(crate) task_id: String,
    pub(crate) status: String,
}

impl std::fmt::Debug for ClaudeTaskNotificationMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaudeTaskNotificationMessage { .. }")
    }
}

#[cfg(test)]
mod task_lifecycle_privacy_tests {
    use super::*;

    #[test]
    fn internal_task_lifecycle_debug_never_exposes_native_identifiers() {
        let started: ClaudeTaskStartedMessage = serde_json::from_value(serde_json::json!({
            "session_id":"session-secret","task_id":"task-secret",
            "tool_use_id":"tool-secret","task_type":"local_agent"
        }))
        .expect("typed task-started message");
        let notification: ClaudeTaskNotificationMessage =
            serde_json::from_value(serde_json::json!({
                "session_id":"session-secret","task_id":"task-secret","status":"failed"
            }))
            .expect("typed task notification");
        let debug = format!("{started:?} {notification:?}");
        assert!(!debug.contains("secret"));
        assert_eq!(
            debug,
            "ClaudeTaskStartedMessage { .. } ClaudeTaskNotificationMessage { .. }"
        );
    }
}
