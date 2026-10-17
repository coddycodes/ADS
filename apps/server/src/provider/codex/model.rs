use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_MODEL: &str = "gpt-5.4";

const BIBCODE_BROWSER_TOOL_INSTRUCTIONS: &str = r#"

## BiBCode collaborative browser

You are running inside BiBCode. The `bibcode` MCP server is the product-native collaborative browser shared with the user. When it exposes `preview_*` tools, prefer those tools for browser navigation, inspection, interaction, screenshots, and recordings.

For browser work, first call `preview_status`. If no automation-capable preview is attached, call `preview_open` before concluding that the browser is unavailable. Then use `preview_navigate`, `preview_snapshot`, and the focused interaction tools. Prefer snapshot-provided locators over coordinates.

Do not switch to global browser skills, Chrome, Node REPL browser automation, standalone Playwright, or agent-browser merely because the preview is initially closed or a first call fails. Use an alternative browser system only when the BiBCode preview tools are absent, the user explicitly requests another browser, or `preview_open` returns an explicit unsupported/unavailable error. A failed BiBCode preview tool call should be inspected and retried with corrected arguments when the error is actionable.
"#;

const CODEX_PLAN_MODE_DEVELOPER_INSTRUCTIONS: &str = r#"<collaboration_mode># Plan Mode (Conversational)

You work in 3 phases, and you should *chat your way* to a great plan before finalizing it. A great plan is very detailed-intent- and implementation-wise-so that it can be handed to another engineer or agent to be implemented right away. It must be **decision complete**, where the implementer does not need to make any decisions.

## Mode rules (strict)

You are in **Plan Mode** until a developer message explicitly ends it.

Plan Mode is not changed by user intent, tone, or imperative language. If a user asks for execution while still in Plan Mode, treat it as a request to **plan the execution**, not perform it.

## Plan Mode vs update_plan tool

Plan Mode is a collaboration mode that can involve requesting user input and eventually issuing a `<proposed_plan>` block.

Separately, `update_plan` is a checklist/progress/TODOs tool; it does not enter or exit Plan Mode. Do not confuse it with Plan mode or try to use it while in Plan mode. If you try to use `update_plan` in Plan mode, it will return an error.

## Execution vs. mutation in Plan Mode

You may explore and execute **non-mutating** actions that improve the plan. You must not perform **mutating** actions.

### Allowed (non-mutating, plan-improving)

Actions that gather truth, reduce ambiguity, or validate feasibility without changing repo-tracked state. Examples:

* Reading or searching files, configs, schemas, types, manifests, and docs
* Static analysis, inspection, and repo exploration
* Dry-run style commands when they do not edit repo-tracked files
* Tests, builds, or checks that may write to caches or build artifacts (for example, `target/`, `.cache/`, or snapshots) so long as they do not edit repo-tracked files

### Not allowed (mutating, plan-executing)

Actions that implement the plan or change repo-tracked state. Examples:

* Editing or writing files
* Running formatters or linters that rewrite files
* Applying patches, migrations, or codegen that updates repo-tracked files
* Side-effectful commands whose purpose is to carry out the plan rather than refine it

When in doubt: if the action would reasonably be described as "doing the work" rather than "planning the work," do not do it.

## PHASE 1 - Ground in the environment (explore first, ask second)

Begin by grounding yourself in the actual environment. Eliminate unknowns in the prompt by discovering facts, not by asking the user. Resolve all questions that can be answered through exploration or inspection. Identify missing or ambiguous details only if they cannot be derived from the environment. Silent exploration between turns is allowed and encouraged.

Before asking the user any question, perform at least one targeted non-mutating exploration pass (for example: search relevant files, inspect likely entrypoints/configs, confirm current implementation shape), unless no local environment/repo is available.

Exception: you may ask clarifying questions about the user's prompt before exploring, ONLY if there are obvious ambiguities or contradictions in the prompt itself. However, if ambiguity might be resolved by exploring, always prefer exploring first.

Do not ask questions that can be answered from the repo or system (for example, "where is this struct?" or "which UI component should we use?" when exploration can make it clear). Only ask once you have exhausted reasonable non-mutating exploration.

## PHASE 2 - Intent chat (what they actually want)

* Keep asking until you can clearly state: goal + success criteria, audience, in/out of scope, constraints, current state, and the key preferences/tradeoffs.
* Bias toward questions over guessing: if any high-impact ambiguity remains, do NOT plan yet-ask.

## PHASE 3 - Implementation chat (what/how we'll build)

* Once intent is stable, keep asking until the spec is decision complete: approach, interfaces (APIs/schemas/I/O), data flow, edge cases/failure modes, testing + acceptance criteria, rollout/monitoring, and any migrations/compat constraints.

## Asking questions

Critical rules:

* Strongly prefer using the `request_user_input` tool to ask any questions.
* Offer only meaningful multiple-choice options; don't include filler choices that are obviously wrong or irrelevant.
* In rare cases where an unavoidable, important question can't be expressed with reasonable multiple-choice options (due to extreme ambiguity), you may ask it directly without the tool.

You SHOULD ask many questions, but each question must:

* materially change the spec/plan, OR
* confirm/lock an assumption, OR
* choose between meaningful tradeoffs.
* not be answerable by non-mutating commands.

Use the `request_user_input` tool only for decisions that materially change the plan, for confirming important assumptions, or for information that cannot be discovered via non-mutating exploration.

## Two kinds of unknowns (treat differently)

1. **Discoverable facts** (repo/system truth): explore first.

   * Before asking, run targeted searches and check likely sources of truth (configs/manifests/entrypoints/schemas/types/constants).
   * Ask only if: multiple plausible candidates; nothing found but you need a missing identifier/context; or ambiguity is actually product intent.
   * If asking, present concrete candidates (paths/service names) + recommend one.
   * Never ask questions you can answer from your environment (e.g., "where is this struct").

2. **Preferences/tradeoffs** (not discoverable): ask early.

   * These are intent or implementation preferences that cannot be derived from exploration.
   * Provide 2-4 mutually exclusive options + a recommended default.
   * If unanswered, proceed with the recommended option and record it as an assumption in the final plan.

## Finalization rule

Only output the final plan when it is decision complete and leaves no decisions to the implementer.

When you present the official plan, wrap it in a `<proposed_plan>` block so the client can render it specially:

1) The opening tag must be on its own line.
2) Start the plan content on the next line (no text on the same line as the tag).
3) The closing tag must be on its own line.
4) Use Markdown inside the block.
5) Keep the tags exactly as `<proposed_plan>` and `</proposed_plan>` (do not translate or rename them), even if the plan content is in another language.

Example:

<proposed_plan>
plan content
</proposed_plan>

plan content should be human and agent digestible. The final plan must be plan-only and include:

* A clear title
* A brief summary section
* Important changes or additions to public APIs/interfaces/types
* Test cases and scenarios
* Explicit assumptions and defaults chosen where needed

Do not ask "should I proceed?" in the final output. The user can easily switch out of Plan mode and request implementation if you have included a `<proposed_plan>` block in your response. Alternatively, they can decide to stay in Plan mode and continue refining the plan.

Only produce at most one `<proposed_plan>` block per turn, and only when you are presenting a complete spec.
"#;

const CODEX_DEFAULT_MODE_DEVELOPER_INSTRUCTIONS: &str = r#"<collaboration_mode># Collaboration Mode: Default

You are now in Default mode. Any previous instructions for other modes (e.g. Plan mode) are no longer active.

Your active mode changes only when new developer instructions with a different `<collaboration_mode>...</collaboration_mode>` change it; user requests or tool descriptions do not change mode by themselves. Known mode names are Default and Plan.

## request_user_input availability

The `request_user_input` tool is unavailable in Default mode. If you call it while in Default mode, it will return an error.

In Default mode, strongly prefer making reasonable assumptions and executing the user's request rather than stopping to ask questions. If you absolutely must ask a question because the answer cannot be discovered from local context and a reasonable assumption would be risky, ask the user directly with a concise plain-text question. Never write a multiple choice question as a textual assistant message.
"#;

const RECOVERABLE_THREAD_RESUME_ERROR_SNIPPETS: &[&str] = &[
    "not found",
    "no rollout",
    "missing thread",
    "no such thread",
    "unknown thread",
    "does not exist",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodexRuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    FullAccess,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexServiceTier {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexProviderModel {
    pub slug: String,
    pub name: String,
    pub is_custom: bool,
    pub capabilities: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexProviderSkill {
    pub name: String,
    pub path: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexProviderSnapshot {
    pub account: Value,
    pub version: Option<String>,
    pub models: Vec<CodexProviderModel>,
    pub skills: Vec<CodexProviderSkill>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexThreadTurnSnapshot {
    pub id: String,
    pub items: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexThreadSnapshot {
    pub thread_id: String,
    pub turns: Vec<CodexThreadTurnSnapshot>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadListParams<'a> {
    pub ancestor_thread_id: &'a str,
    pub limit: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadListResponse {
    pub data: Vec<ReconciliationThread>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadReadParams<'a> {
    pub thread_id: &'a str,
    pub include_turns: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ThreadReadResponse {
    pub thread: ReconciliationThread,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReconciliationThread {
    pub id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub name: Option<String>,
    pub created_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub status: Option<ReconciliationThreadStatus>,
    #[serde(default)]
    pub turns: Vec<ReconciliationTurn>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum ReconciliationThreadStatus {
    NotLoaded,
    Idle,
    SystemError,
    Active {
        #[serde(default)]
        active_flags: Vec<String>,
    },
    #[serde(other)]
    Unknown,
}

impl ReconciliationThreadStatus {
    pub(crate) fn activity_status(&self) -> &'static str {
        match self {
            Self::NotLoaded | Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::SystemError => "systemError",
            Self::Active { active_flags } if active_flags.is_empty() => "active",
            Self::Active { .. } => "waiting",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReconciliationTurn {
    pub id: Option<String>,
    pub status: Option<String>,
    pub error: Option<ReconciliationTurnError>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub items: Vec<ReconciliationThreadItem>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ReconciliationTurnError {
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type")]
pub(crate) enum ReconciliationThreadItem {
    #[serde(rename = "agentMessage")]
    AgentMessage {
        id: Option<String>,
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        id: Option<String>,
        #[serde(default)]
        summary: Vec<String>,
    },
    #[serde(rename = "commandExecution")]
    CommandExecution {
        id: Option<String>,
        #[serde(default)]
        command: String,
        status: Option<String>,
        #[serde(rename = "aggregatedOutput")]
        aggregated_output: Option<String>,
    },
    #[serde(rename = "dynamicToolCall")]
    DynamicToolCall {
        id: Option<String>,
        #[serde(default)]
        tool: String,
        status: Option<String>,
    },
    #[serde(rename = "mcpToolCall")]
    McpToolCall {
        id: Option<String>,
        #[serde(default)]
        tool: String,
        status: Option<String>,
    },
    #[serde(rename = "subAgentActivity")]
    SubAgentActivity {
        id: Option<String>,
        #[serde(rename = "agentThreadId")]
        agent_thread_id: Option<String>,
        #[serde(rename = "agentPath")]
        agent_path: Option<String>,
        kind: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadBackgroundTerminalsListParams<'a> {
    pub thread_id: &'a str,
    pub limit: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadBackgroundTerminalsListResponse {
    pub data: Vec<ReconciliationBackgroundTerminal>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReconciliationBackgroundTerminal {
    pub item_id: Option<String>,
    pub process_id: Option<String>,
    pub command: Option<String>,
}

pub(crate) const CODEX_REMOTE_MESSAGE_MAX_BYTES: usize = 512 * 1024;
/// The decode error for a payload over `CODEX_REMOTE_MESSAGE_MAX_BYTES`.
pub(crate) const CODEX_REMOTE_JSON_TOO_LARGE: &str = "Codex remote JSON exceeded its fixed bound";
pub(crate) const CODEX_RECONCILIATION_THREAD_LIMIT: usize = 50;
pub(crate) const CODEX_RECONCILIATION_BACKGROUND_LIMIT: usize = 128;
pub(crate) const CODEX_RECONCILIATION_TURN_LIMIT: usize = 20;
pub(crate) const CODEX_RECONCILIATION_ITEM_LIMIT: usize = 200;

struct BoundedJsonWriter {
    remaining: usize,
}

impl std::io::Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if buffer.len() > self.remaining {
            return Err(std::io::Error::other(
                "Codex remote JSON exceeded the fixed message limit",
            ));
        }
        self.remaining -= buffer.len();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn ensure_bounded_remote_value(value: &Value) -> Result<(), String> {
    serde_json::to_writer(
        BoundedJsonWriter {
            remaining: CODEX_REMOTE_MESSAGE_MAX_BYTES,
        },
        value,
    )
    .map_err(|_| CODEX_REMOTE_JSON_TOO_LARGE.to_owned())
}

pub(crate) fn decode_thread_list_response(mut value: Value) -> Result<ThreadListResponse, String> {
    ensure_bounded_remote_value(&value)?;
    if let Some(data) = value.get_mut("data").and_then(Value::as_array_mut) {
        data.truncate(CODEX_RECONCILIATION_THREAD_LIMIT);
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// The transport's line limit bounds what Codex sends; the fixed bound applies to what is kept.
/// A long thread is trimmed to its newest turns and items, and command output, which activity
/// shows only as present (`safe_command_output`), is reduced to a marker before the check.
pub(crate) fn decode_thread_read_response(mut value: Value) -> Result<ThreadReadResponse, String> {
    if let Some(turns) = value
        .pointer_mut("/thread/turns")
        .and_then(Value::as_array_mut)
    {
        if turns.len() > CODEX_RECONCILIATION_TURN_LIMIT {
            turns.drain(..turns.len() - CODEX_RECONCILIATION_TURN_LIMIT);
        }
        let mut remaining_items = CODEX_RECONCILIATION_ITEM_LIMIT;
        for turn in turns.iter_mut().rev() {
            let Some(items) = turn.get_mut("items").and_then(Value::as_array_mut) else {
                continue;
            };
            if remaining_items == 0 {
                items.clear();
            } else if items.len() > remaining_items {
                items.drain(..items.len() - remaining_items);
                remaining_items = 0;
            } else {
                remaining_items -= items.len();
            }
            for output in items
                .iter_mut()
                .filter_map(|item| item.get_mut("aggregatedOutput"))
            {
                if output.as_str().is_some_and(|text| !text.is_empty()) {
                    *output = Value::String("…".to_owned());
                }
            }
        }
    }
    ensure_bounded_remote_value(&value)?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}

pub(crate) fn delivery_key_exists(
    value: &Value,
    thread_id: &str,
    delivery_key: &str,
) -> Result<bool, String> {
    ensure_bounded_remote_value(value)?;
    let thread = value
        .get("thread")
        .and_then(Value::as_object)
        .ok_or_else(|| "thread/read response missing thread".to_owned())?;
    if thread.get("id").and_then(Value::as_str) != Some(thread_id) {
        return Err("thread/read response has the wrong thread.id".to_owned());
    }
    let turns = thread
        .get("turns")
        .and_then(Value::as_array)
        .ok_or_else(|| "thread/read response missing complete thread.turns".to_owned())?;
    for turn in turns {
        let turn = turn
            .as_object()
            .ok_or_else(|| "thread/read response contains an invalid turn".to_owned())?;
        if turn.get("id").and_then(Value::as_str).is_none() {
            return Err("thread/read response turn is missing id".to_owned());
        }
        let items = turn
            .get("items")
            .and_then(Value::as_array)
            .ok_or_else(|| "thread/read response turn is missing items".to_owned())?;
        for item in items {
            let item = item
                .as_object()
                .ok_or_else(|| "thread/read response contains an invalid item".to_owned())?;
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| "thread/read response item is missing type".to_owned())?;
            if item_type != "userMessage" {
                continue;
            }
            if item.get("id").and_then(Value::as_str).is_none()
                || item.get("content").and_then(Value::as_array).is_none()
            {
                return Err("thread/read response contains an incomplete userMessage".to_owned());
            }
            match item.get("clientId") {
                Some(Value::String(client_id)) if client_id == delivery_key => return Ok(true),
                Some(Value::String(_) | Value::Null) | None => {}
                Some(_) => {
                    return Err("thread/read response userMessage.clientId is invalid".to_owned());
                }
            }
        }
    }
    Ok(false)
}

pub(crate) fn decode_background_terminals_list_response(
    mut value: Value,
) -> Result<ThreadBackgroundTerminalsListResponse, String> {
    ensure_bounded_remote_value(&value)?;
    if let Some(data) = value.get_mut("data").and_then(Value::as_array_mut) {
        data.truncate(CODEX_RECONCILIATION_BACKGROUND_LIMIT);
    }
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTurnStartInput {
    pub thread_id: String,
    pub runtime_mode: CodexRuntimeMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_mode: Option<String>,
}

#[must_use]
pub fn build_initialize_params(version: &str) -> Value {
    json!({
        "clientInfo": {
            "name": "bibcode_desktop",
            "title": "BiBCode Desktop",
            "version": version,
        },
        "capabilities": {
            "experimentalApi": true,
        },
    })
}

fn turn_input_parts(prompt: Option<&str>, attachments: &[Value]) -> Vec<Value> {
    let mut input = Vec::new();
    if let Some(prompt) = prompt.filter(|value| !value.is_empty()) {
        input.push(json!({ "type": "text", "text": prompt }));
    }
    input.extend_from_slice(attachments);
    input
}

#[must_use]
pub fn build_turn_steer_params(
    thread_id: &str,
    prompt: Option<&str>,
    attachments: &[Value],
    expected_turn_id: &str,
    client_user_message_id: Option<&str>,
) -> Value {
    let mut payload = json!({
        "threadId": thread_id,
        "input": turn_input_parts(prompt, attachments),
        "expectedTurnId": expected_turn_id,
    });
    if let Some(id) = client_user_message_id {
        payload["clientUserMessageId"] = json!(id);
    }
    payload
}

#[must_use]
pub fn build_turn_start_params(input: &BuildTurnStartInput) -> Value {
    let (approval_policy, sandbox_policy) = match input.runtime_mode {
        CodexRuntimeMode::ApprovalRequired => ("untrusted", json!({ "type": "readOnly" })),
        CodexRuntimeMode::AutoAcceptEdits => ("on-request", json!({ "type": "workspaceWrite" })),
        CodexRuntimeMode::FullAccess => ("never", json!({ "type": "dangerFullAccess" })),
    };

    let mut payload = json!({
        "threadId": input.thread_id,
        "approvalPolicy": approval_policy,
        "sandboxPolicy": sandbox_policy,
        "input": turn_input_parts(input.prompt.as_deref(), &input.attachments),
    });

    if let Some(client_user_message_id) = input.client_user_message_id.as_ref() {
        payload["clientUserMessageId"] = json!(client_user_message_id);
    }

    if let Some(model) = input.model.as_ref().map(|value| resolve_turn_model(value)) {
        payload["model"] = json!(model);
    }
    if let Some(service_tier) = input.service_tier.as_ref() {
        payload["serviceTier"] = json!(service_tier);
    }
    if let Some(effort) = input.effort.as_ref() {
        payload["effort"] = json!(effort);
    }
    if let Some(mode) = input.interaction_mode.as_ref() {
        let model = input
            .model
            .as_ref()
            .map(|value| resolve_turn_model(value))
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        payload["collaborationMode"] = json!({
            "mode": mode,
            "settings": {
                "model": model,
                "reasoning_effort": input.effort.clone().unwrap_or_else(|| "medium".to_owned()),
                "developer_instructions": if mode == "plan" {
                    format!(
                        "{}{}{}",
                        CODEX_PLAN_MODE_DEVELOPER_INSTRUCTIONS,
                        BIBCODE_BROWSER_TOOL_INSTRUCTIONS,
                        "\n</collaboration_mode>"
                    )
                } else {
                    format!(
                        "{}{}{}",
                        CODEX_DEFAULT_MODE_DEVELOPER_INSTRUCTIONS,
                        BIBCODE_BROWSER_TOOL_INSTRUCTIONS,
                        "\n</collaboration_mode>"
                    )
                },
            }
        });
    }

    payload
}

#[must_use]
pub fn is_recoverable_thread_resume_error(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("thread")
        && RECOVERABLE_THREAD_RESUME_ERROR_SNIPPETS
            .iter()
            .any(|snippet| lowered.contains(snippet))
}

pub fn parse_model_list_response(
    response: &Value,
    custom_models: &[String],
) -> Result<Vec<CodexProviderModel>, String> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "model/list response missing data array".to_owned())?;
    let mut models = Vec::with_capacity(data.len() + custom_models.len());
    for model in data {
        let slug = model
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| "model entry missing model".to_owned())?;
        let display_name = model
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(slug);
        models.push(CodexProviderModel {
            slug: slug.to_owned(),
            name: to_display_name(display_name),
            is_custom: false,
            capabilities: map_model_capabilities(model),
        });
    }

    let mut seen = models
        .iter()
        .map(|model| model.slug.clone())
        .collect::<std::collections::HashSet<_>>();
    let fallback_capabilities = models
        .iter()
        .find(|model| !model.capabilities.is_null())
        .map(|model| model.capabilities.clone())
        .unwrap_or(Value::Null);
    for custom in custom_models {
        let slug = custom.trim();
        if slug.is_empty() || seen.contains(slug) {
            continue;
        }
        seen.insert(slug.to_owned());
        models.push(CodexProviderModel {
            slug: slug.to_owned(),
            name: slug.to_owned(),
            is_custom: true,
            capabilities: fallback_capabilities.clone(),
        });
    }

    Ok(models)
}

pub fn fallback_models(
    configured_model: Option<&str>,
    configured_effort: Option<&str>,
    _configured_service_tier: Option<&str>,
    custom_models: &[String],
) -> Vec<Value> {
    const EFFORTS: [&str; 8] = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ];
    let model = configured_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MODEL);
    let selected_effort = configured_effort
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("medium");
    let mut efforts = EFFORTS.into_iter().collect::<Vec<_>>();
    if !efforts.contains(&selected_effort) {
        efforts.push(selected_effort);
    }
    let effort_options = efforts
        .into_iter()
        .map(|effort| {
            let mut option = json!({
                "id": effort,
                "label": reasoning_effort_label(effort),
            });
            if effort == selected_effort {
                option["isDefault"] = json!(true);
            }
            option
        })
        .collect::<Vec<_>>();
    let mut models = vec![json!({
        "slug": model,
        "name": to_display_name(model),
        "isCustom": false,
        "capabilities": {
            "optionDescriptors": [
                {
                    "id": "reasoningEffort",
                    "label": REASONING_EFFORT_LABEL,
                    "type": "select",
                    "options": effort_options,
                    "currentValue": selected_effort,
                }
            ]
        }
    })];
    let mut seen = std::collections::HashSet::from([model.to_owned()]);
    models.extend(custom_models.iter().filter_map(|custom| {
        let slug = custom.trim();
        (!slug.is_empty() && seen.insert(slug.to_owned())).then(|| {
            json!({
                "slug": slug,
                "name": slug,
                "isCustom": true,
                "capabilities": null,
            })
        })
    }));
    models
}

pub fn parse_skills_list_response(
    response: &Value,
    cwd: &str,
) -> Result<Vec<CodexProviderSkill>, String> {
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "skills/list response missing data array".to_owned())?;

    let selected = data
        .iter()
        .find(|entry| entry.get("cwd").and_then(Value::as_str) == Some(cwd))
        .or_else(|| data.first());

    let Some(skills) = selected
        .and_then(|entry| entry.get("skills"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };

    Ok(skills
        .iter()
        .map(|skill| {
            let interface = skill.get("interface");
            let short_description = skill
                .get("shortDescription")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    interface
                        .and_then(|value| value.get("shortDescription"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            CodexProviderSkill {
                name: skill
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                path: skill
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                enabled: skill
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                description: skill
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                scope: skill
                    .get("scope")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                display_name: interface
                    .and_then(|value| value.get("displayName"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                short_description,
            }
        })
        .collect())
}

pub fn parse_thread_snapshot(response: &Value) -> Result<CodexThreadSnapshot, String> {
    let thread = response
        .get("thread")
        .and_then(Value::as_object)
        .ok_or_else(|| "thread response missing thread object".to_owned())?;
    let turns = thread
        .get("turns")
        .and_then(Value::as_array)
        .ok_or_else(|| "thread response missing turns array".to_owned())?;
    Ok(CodexThreadSnapshot {
        thread_id: thread
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        turns: turns
            .iter()
            .map(|turn| CodexThreadTurnSnapshot {
                id: turn
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                items: turn
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect(),
    })
}

fn map_model_capabilities(model: &Value) -> Value {
    let supported_reasoning_efforts = model
        .get("supportedReasoningEfforts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let default_reasoning_effort = model
        .get("defaultReasoningEffort")
        .and_then(Value::as_str)
        .unwrap_or("medium");

    let reasoning_options = supported_reasoning_efforts
        .iter()
        .filter_map(|entry| entry.get("reasoningEffort").and_then(Value::as_str))
        .map(|effort| {
            if effort == default_reasoning_effort {
                json!({
                    "id": effort,
                    "label": reasoning_effort_label(effort),
                    "isDefault": true,
                })
            } else {
                json!({
                    "id": effort,
                    "label": reasoning_effort_label(effort),
                })
            }
        })
        .collect::<Vec<_>>();

    let service_tiers = model
        .get("serviceTiers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut seen_service_tiers = std::collections::HashSet::new();
    let service_tiers = service_tiers
        .iter()
        .filter(|tier| {
            tier.get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .is_some_and(|id| seen_service_tiers.insert(id.to_owned()))
        })
        .collect::<Vec<_>>();

    let default_service_tier = model
        .get("defaultServiceTier")
        .and_then(Value::as_str)
        .filter(|default_id| {
            service_tiers
                .iter()
                .any(|tier| tier.get("id").and_then(Value::as_str) == Some(*default_id))
        });

    let mut option_descriptors = Vec::new();
    if !reasoning_options.is_empty() {
        option_descriptors.push(json!({
            "id": "reasoningEffort",
            "label": REASONING_EFFORT_LABEL,
            "type": "select",
            "options": reasoning_options,
            "currentValue": default_reasoning_effort,
        }));
    }
    let effective_default_service_tier = default_service_tier.unwrap_or("default");
    let mut options = Vec::new();
    if !service_tiers.is_empty()
        && !service_tiers
            .iter()
            .any(|tier| tier.get("id").and_then(Value::as_str) == Some("default"))
    {
        options.push(json!({
            "id": "default",
            "label": "Standard",
            "isDefault": effective_default_service_tier == "default",
        }));
    }
    options.extend(service_tiers.iter().map(|tier| {
            let id = tier.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tier.get("name").and_then(Value::as_str).unwrap_or(id);
            let description = tier
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            json!({
                "id": id,
                "label": name,
                "description": if description.is_empty() { Value::Null } else { json!(description) },
                "isDefault": effective_default_service_tier == id,
            })
        })
        .map(|value| {
            let mut object = value.as_object().cloned().unwrap_or_default();
            if object.get("description").is_some_and(Value::is_null) {
                object.remove("description");
            }
            if object.get("isDefault") == Some(&Value::Bool(false)) {
                object.remove("isDefault");
            }
            Value::Object(object)
        }));
    if !options.is_empty() {
        option_descriptors.push(json!({
            "id": "serviceTier",
            "label": SERVICE_TIER_LABEL,
            "type": "select",
            "options": options,
            "currentValue": effective_default_service_tier,
        }));
    }

    json!({
        "optionDescriptors": option_descriptors,
    })
}

/// The labels BiBCode gives Codex's turn options. The model catalog shows them, and the refusal
/// for an option shows the same label.
pub(crate) const REASONING_EFFORT_LABEL: &str = "Reasoning";
pub(crate) const SERVICE_TIER_LABEL: &str = "Service Tier";

/// The label for a Codex turn option id, or the id itself for an option Codex does not take.
pub(crate) fn turn_option_label(id: &str) -> &str {
    match id {
        "reasoningEffort" => REASONING_EFFORT_LABEL,
        "serviceTier" => SERVICE_TIER_LABEL,
        other => other,
    }
}

pub(crate) fn reasoning_effort_label(value: &str) -> &str {
    match value {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        "ultra" => "Ultra",
        _ => value,
    }
}

fn normalize_model_slug(model: &str, preferred_id: Option<&str>) -> String {
    let normalized = model.trim().to_ascii_lowercase();
    if let Some(preferred_id) = preferred_id
        && preferred_id.ends_with("-codex")
        && preferred_id != normalized
    {
        return preferred_id.to_owned();
    }
    normalized
}

fn resolve_turn_model(model: &str) -> String {
    if model.trim().eq_ignore_ascii_case("auto") {
        DEFAULT_MODEL.to_owned()
    } else {
        normalize_model_slug(model, None)
    }
}

fn to_display_name(display_name: &str) -> String {
    let mut result = display_name.replacen("gpt", "GPT", 1);
    let mut chars = result.chars().peekable();
    let mut rebuilt = String::with_capacity(result.len());
    while let Some(character) = chars.next() {
        rebuilt.push(character);
        if character == '-'
            && let Some(next) = chars.next()
        {
            rebuilt.push(next.to_ascii_uppercase());
        }
    }
    result.clear();
    rebuilt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_decoders_bound_collection_allocations() {
        let listed = decode_thread_list_response(json!({
            "data": (0..=CODEX_RECONCILIATION_THREAD_LIMIT)
                .map(|index| json!({"id": format!("thread-{index}")}))
                .collect::<Vec<_>>(),
            "nextCursor": null
        }))
        .expect("bounded thread/list response");
        assert_eq!(
            listed.data.len(),
            CODEX_RECONCILIATION_THREAD_LIMIT,
            "thread/list must be capped before typed Vec allocation"
        );

        let background = decode_background_terminals_list_response(json!({
            "data": (0..=CODEX_RECONCILIATION_BACKGROUND_LIMIT)
                .map(|index| json!({
                    "itemId": format!("background-{index}"),
                    "command": "sleep"
                }))
                .collect::<Vec<_>>(),
            "nextCursor": null
        }))
        .expect("bounded background-terminal response");
        assert_eq!(
            background.data.len(),
            CODEX_RECONCILIATION_BACKGROUND_LIMIT,
            "background terminals must be capped before typed Vec allocation"
        );

        let read = decode_thread_read_response(json!({
            "thread": {
                "id": "bounded-history",
                "turns": (0..=CODEX_RECONCILIATION_TURN_LIMIT)
                    .map(|turn| json!({
                        "id": format!("turn-{turn}"),
                        "items": (0..11)
                            .map(|item| json!({
                                "id": format!("item-{turn}-{item}"),
                                "type": "agentMessage",
                                "text": "bounded"
                            }))
                            .collect::<Vec<_>>()
                    }))
                    .collect::<Vec<_>>()
            }
        }))
        .expect("bounded thread/read response");
        assert_eq!(
            read.thread.turns.len(),
            CODEX_RECONCILIATION_TURN_LIMIT,
            "thread/read turns must be capped before typed Vec allocation"
        );
        assert!(
            read.thread
                .turns
                .iter()
                .map(|turn| turn.items.len())
                .sum::<usize>()
                <= CODEX_RECONCILIATION_ITEM_LIMIT,
            "thread/read items must be globally capped before typed Vec allocation"
        );
    }

    #[test]
    fn reconciliation_decoders_reject_payloads_over_the_transport_limit() {
        let response = decode_thread_read_response(json!({
            "thread": {
                "id": "oversized-history",
                "turns": [{
                    "id": "turn-1",
                    "items": [{
                        "id": "item-1",
                        "type": "agentMessage",
                        "text": "x".repeat(CODEX_REMOTE_MESSAGE_MAX_BYTES)
                    }]
                }]
            }
        }));

        assert!(
            response.is_err(),
            "the history kept after trimming stays within the fixed bound"
        );
    }

    #[test]
    fn thread_read_bounds_the_trimmed_history_not_the_reply() {
        let message = "x".repeat(20 * 1024);
        let turns = (0..CODEX_RECONCILIATION_TURN_LIMIT + 10)
            .map(|index| {
                json!({
                    "id": format!("turn-{index}"),
                    "items": [{
                        "id": format!("message-{index}"),
                        "type": "agentMessage",
                        "text": message
                    }]
                })
            })
            .collect::<Vec<_>>();
        let long_thread = json!({"thread": {"id": "long-thread", "turns": turns}});
        assert!(
            serde_json::to_vec(&long_thread).expect("long thread").len()
                > CODEX_REMOTE_MESSAGE_MAX_BYTES
        );
        let response = decode_thread_read_response(long_thread).expect("trimmed long thread");
        assert_eq!(response.thread.turns.len(), CODEX_RECONCILIATION_TURN_LIMIT);

        let response = decode_thread_read_response(json!({
            "thread": {
                "id": "large-output",
                "turns": [{
                    "id": "turn-1",
                    "items": [
                        {
                            "id": "command-1",
                            "type": "commandExecution",
                            "aggregatedOutput": "x".repeat(CODEX_REMOTE_MESSAGE_MAX_BYTES)
                        },
                        {"id": "command-2", "type": "commandExecution", "aggregatedOutput": ""}
                    ]
                }]
            }
        }))
        .expect("command output is not retained");
        let outputs = response.thread.turns[0]
            .items
            .iter()
            .map(|item| match item {
                ReconciliationThreadItem::CommandExecution {
                    aggregated_output, ..
                } => aggregated_output.as_deref(),
                _ => panic!("command item"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            outputs,
            [Some("…"), Some("")],
            "output presence survives, its text does not"
        );
    }

    #[test]
    fn historical_sub_agent_activity_decodes_as_a_known_thread_item() {
        let response = decode_thread_read_response(json!({
            "thread": {
                "id": "child-1",
                "turns": [{
                    "id": "turn-1",
                    "items": [{
                        "id": "sub-agent-1",
                        "type": "subAgentActivity",
                        "agentThreadId": "nested-child-1",
                        "agentPath": "/root/child-1/nested-child-1",
                        "kind": "started"
                    }]
                }]
            }
        }))
        .expect("historical thread/read response");

        assert!(matches!(
            response.thread.turns[0].items[0],
            ReconciliationThreadItem::SubAgentActivity {
                ref id,
                ref agent_thread_id,
                ref agent_path,
                ref kind,
            } if id.as_deref() == Some("sub-agent-1")
                && agent_thread_id.as_deref() == Some("nested-child-1")
                && agent_path.as_deref() == Some("/root/child-1/nested-child-1")
                && kind.as_deref() == Some("started")
        ));
    }

    #[test]
    fn fallback_models_do_not_advertise_unverified_fast() {
        let models = fallback_models(Some("gpt-private"), Some("max"), Some("fast"), &[]);
        let model = &models[0];
        let descriptors = model["capabilities"]["optionDescriptors"]
            .as_array()
            .expect("fallback option descriptors");
        let effort = descriptors
            .iter()
            .find(|descriptor| descriptor["id"] == "reasoningEffort")
            .expect("reasoning effort descriptor");
        assert_eq!(model["slug"], "gpt-private");
        assert!(
            effort["options"]
                .as_array()
                .unwrap()
                .iter()
                .any(|option| { option["id"] == "max" && option["isDefault"] == true })
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| descriptor["id"] != "serviceTier")
        );
    }

    #[test]
    fn fallback_models_keep_custom_models_unique_and_label_all_known_efforts() {
        let models = fallback_models(
            None,
            Some("ultra"),
            None,
            &["gpt-custom".to_owned(), "gpt-custom".to_owned()],
        );

        assert_eq!(
            models
                .iter()
                .filter(|model| model["slug"] == "gpt-custom")
                .count(),
            1
        );
        assert_eq!(reasoning_effort_label("max"), "Max");
        assert_eq!(reasoning_effort_label("ultra"), "Ultra");
    }

    #[test]
    fn live_model_without_service_tiers_does_not_advertise_fast() {
        let models = parse_model_list_response(
            &json!({ "data": [{ "model": "gpt-5.6", "serviceTiers": [] }]}),
            &[],
        )
        .expect("live model should parse");
        let descriptors = models[0].capabilities["optionDescriptors"]
            .as_array()
            .expect("live option descriptors");

        assert!(
            descriptors
                .iter()
                .all(|descriptor| descriptor["id"] != "serviceTier")
        );
    }

    #[test]
    fn live_priority_service_tier_includes_the_standard_off_value() {
        let models = parse_model_list_response(
            &json!({
                "data": [{
                    "model": "gpt-5.6-sol",
                    "additionalSpeedTiers": ["fast"],
                    "serviceTiers": [{ "id": "priority", "name": "Fast" }],
                    "defaultServiceTier": null
                }]
            }),
            &[],
        )
        .expect("live model should parse");
        let service_tier = models[0].capabilities["optionDescriptors"]
            .as_array()
            .expect("live option descriptors")
            .iter()
            .find(|descriptor| descriptor["id"] == "serviceTier")
            .expect("advertised service tier descriptor");

        assert_eq!(service_tier["currentValue"], "default");
        assert_eq!(
            service_tier["options"]
                .as_array()
                .expect("service tier options")
                .iter()
                .filter_map(|option| option["id"].as_str())
                .collect::<Vec<_>>(),
            vec!["default", "priority"]
        );
    }

    #[test]
    fn live_model_preserves_only_advertised_service_tiers() {
        let models = parse_model_list_response(
            &json!({
                "data": [{
                    "model": "gpt-5.6",
                    "serviceTiers": [{ "id": "default", "name": "Standard" }]
                }]
            }),
            &[],
        )
        .expect("live model should parse");
        let service_tier = models[0].capabilities["optionDescriptors"]
            .as_array()
            .expect("live option descriptors")
            .iter()
            .find(|descriptor| descriptor["id"] == "serviceTier")
            .expect("advertised service tier descriptor");

        assert_eq!(
            service_tier["options"]
                .as_array()
                .expect("service tier options")
                .iter()
                .filter_map(|option| option["id"].as_str())
                .collect::<Vec<_>>(),
            vec!["default"]
        );
    }

    #[test]
    fn malformed_and_fallback_payloads_cover_codex_model_boundaries() {
        assert!(parse_model_list_response(&json!({}), &[]).is_err());
        assert!(parse_model_list_response(&json!({"data":[{}]}), &[]).is_err());
        let models = parse_model_list_response(
            &json!({
                "data":[{
                    "model":"gpt-test",
                    "supportedReasoningEfforts":[{}, {"reasoningEffort":"high"}],
                    "serviceTiers":[{"id":"fast"}, {"name":"missing-id"}],
                    "defaultServiceTier":"missing"
                }]
            }),
            &[" ".to_owned(), "gpt-test".to_owned(), "custom".to_owned()],
        )
        .expect("fallback model payload should parse");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].name, "GPT-Test");
        assert!(models[1].is_custom);

        assert!(parse_skills_list_response(&json!({}), "/workspace").is_err());
        assert!(
            parse_skills_list_response(&json!({"data":[]}), "/workspace")
                .unwrap()
                .is_empty()
        );
        let skills = parse_skills_list_response(
            &json!({"data":[{"cwd":"/fallback","skills":[{
                "interface":{"shortDescription":"nested"}
            }]}]}),
            "/missing",
        )
        .unwrap();
        assert_eq!(skills[0].short_description.as_deref(), Some("nested"));

        assert!(parse_thread_snapshot(&json!({})).is_err());
        assert!(parse_thread_snapshot(&json!({"thread":{}})).is_err());
        assert_eq!(
            parse_thread_snapshot(&json!({"thread":{"turns":[{}]}}))
                .unwrap()
                .turns[0],
            CodexThreadTurnSnapshot {
                id: String::new(),
                items: Vec::new(),
            }
        );

        let turn = build_turn_start_params(&BuildTurnStartInput {
            thread_id: "thread".to_owned(),
            runtime_mode: CodexRuntimeMode::ApprovalRequired,
            client_user_message_id: None,
            prompt: Some(String::new()),
            attachments: Vec::new(),
            model: None,
            service_tier: None,
            effort: None,
            interaction_mode: Some("plan".to_owned()),
        });
        assert_eq!(
            turn["collaborationMode"]["settings"]["model"],
            DEFAULT_MODEL
        );
    }

    #[test]
    fn turn_start_serializes_the_delivery_key_as_client_user_message_id() {
        let input: BuildTurnStartInput = serde_json::from_value(json!({
            "threadId": "provider-thread-1",
            "runtimeMode": "full-access",
            "prompt": "hello",
            "clientUserMessageId": "delivery-1"
        }))
        .expect("turn input");

        assert_eq!(
            build_turn_start_params(&input)["clientUserMessageId"],
            "delivery-1"
        );
    }

    #[test]
    fn delivery_readback_requires_a_complete_exact_client_id_match() {
        let found = json!({
            "thread": {
                "id": "provider-thread-1",
                "turns": [{
                    "id": "turn-1",
                    "items": [{
                        "type": "userMessage",
                        "id": "user-1",
                        "clientId": "delivery-1",
                        "content": [{"type": "text", "text": "hello"}]
                    }]
                }]
            }
        });
        let absent = json!({
            "thread": {
                "id": "provider-thread-1",
                "turns": [{"id": "turn-1", "items": []}]
            }
        });

        assert_eq!(
            delivery_key_exists(&found, "provider-thread-1", "delivery-1"),
            Ok(true)
        );
        assert_eq!(
            delivery_key_exists(&absent, "provider-thread-1", "delivery-1"),
            Ok(false)
        );
        assert!(delivery_key_exists(&json!({}), "provider-thread-1", "delivery-1").is_err());
        assert!(
            delivery_key_exists(
                &json!({"thread":{"id":"provider-thread-1","turns":[{}]}}),
                "provider-thread-1",
                "delivery-1"
            )
            .is_err()
        );
    }
}
