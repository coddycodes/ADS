//! The OpenCode 2 (`/api/*`) wire layer: request bodies, the permission ruleset, forms, error
//! classification and the root-turn event translator. `runtime.rs` owns the I/O and turns these
//! values into BiBCode provider events. Shapes are those of OpenCode 2.0.29, captured from a real
//! server; see `docs/superpowers/specs/2026-10-15-opencode-v2-client-design.md`.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::runtime::OpenCodeFailure;

/// Bounds the sessions tracked as descendants of the root, as the V1 activity tracker does.
const MAX_TRACKED_DESCENDANTS: usize = 256;
/// Bounds the per-message text kept to complete a missed delta from `session.text.ended`.
const MAX_TRACKED_TEXT_STREAMS: usize = 256;

/// BiBCode's permission modes as an OpenCode 2 ruleset. The last matching rule wins, so the
/// broad rule comes first. V2 renamed `bash` to `shell` and `task` to `subagent`; `codesearch` and
/// `doom_loop` are not V2 actions.
pub(crate) fn permission_rules(runtime_mode: &str) -> Vec<Value> {
    if runtime_mode == "full-access" {
        return vec![json!({ "action": "*", "resource": "*", "effect": "allow" })];
    }
    [
        "*",
        "shell",
        "edit",
        "webfetch",
        "websearch",
        "external_directory",
    ]
    .into_iter()
    .map(|action| json!({ "action": action, "resource": "*", "effect": "ask" }))
    .chain(std::iter::once(
        json!({ "action": "question", "resource": "*", "effect": "allow" }),
    ))
    .collect()
}

/// The OpenCode message id for a durable delivery key, on both wires. OpenCode 2 requires `msg_`,
/// and OpenCode 1 (1.18 and later) rejects an id that does not start with `msg`. Both order
/// messages by creation time, so a deterministic id lets reconciliation look the message up
/// again after a restart.
pub(crate) fn message_id(delivery_key: &str) -> String {
    let key = delivery_key.strip_prefix("msg_").unwrap_or(delivery_key);
    let key = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    format!("msg_{key}")
}

/// An OpenCode 2 `Model.Ref`.
pub(crate) fn model_ref(provider_id: &str, model_id: &str, variant: Option<&str>) -> Value {
    let mut model = json!({ "providerID": provider_id, "id": model_id });
    if let Some(variant) = variant {
        model["variant"] = json!(variant);
    }
    model
}

pub(crate) fn session_create_body(
    title: &str,
    directory: &str,
    runtime_mode: &str,
    agent: Option<&str>,
    model: Option<Value>,
) -> Value {
    let mut body = json!({
        "title": title,
        "location": { "directory": directory },
        "permissions": permission_rules(runtime_mode),
    });
    if let Some(agent) = agent {
        body["agent"] = json!(agent);
    }
    if let Some(model) = model {
        body["model"] = model;
    }
    body
}

/// A prompt body. `attachments` are the V1 file parts BiBCode materializes
/// (`{type:"file", mime, url, filename}`); OpenCode 2 takes `{uri, name}`.
pub(crate) fn prompt_body(text: Option<&str>, attachments: &[Value], id: Option<&str>) -> Value {
    let files = attachments
        .iter()
        .filter_map(|part| {
            let uri = part.get("url").and_then(Value::as_str)?;
            let mut file = json!({ "uri": uri });
            if let Some(name) = part.get("filename").and_then(Value::as_str) {
                file["name"] = json!(name);
            }
            Some(file)
        })
        .collect::<Vec<_>>();
    let mut body = json!({ "text": text.unwrap_or_default(), "delivery": "steer" });
    if !files.is_empty() {
        body["files"] = Value::Array(files);
    }
    if let Some(id) = id {
        body["id"] = json!(id);
    }
    body
}

pub(crate) fn command_body(name: &str, arguments: &str) -> Value {
    json!({ "name": name, "text": arguments, "delivery": "steer" })
}

pub(crate) fn permission_decision(decision: &str) -> &'static str {
    match decision {
        "acceptForSession" => "always",
        "accept" => "once",
        _ => "reject",
    }
}

/// The variants `GET /api/model` advertises for one model, or `None` when it is not listed.
pub(crate) fn model_variants(
    models: &Value,
    provider_id: &str,
    model_id: &str,
) -> Option<Vec<String>> {
    models
        .get("data")?
        .as_array()?
        .iter()
        .find(|model| {
            model.get("providerID").and_then(Value::as_str) == Some(provider_id)
                && model.get("id").and_then(Value::as_str) == Some(model_id)
        })
        .map(|model| {
            model
                .get("variants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|variant| variant.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
}

/// Classify an OpenCode 2 structured error (`{type, message, status?}`).
pub(crate) fn classify_error(error: &Value) -> OpenCodeFailure {
    let kind = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (state, stop_reason, class, fallback) = match kind {
        "aborted" => ("cancelled", "cancelled", "unknown", "The turn was stopped."),
        "provider.auth" => (
            "failed",
            "error",
            "permission_error",
            "The upstream provider rejected the credentials.",
        ),
        "provider.content-filter" => (
            "failed",
            "refusal",
            "provider_error",
            "The provider's content filter stopped this turn.",
        ),
        kind if kind.starts_with("provider.") => (
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
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or(fallback)
        .to_owned();
    if let Some(status) = error.get("status").and_then(Value::as_i64) {
        message.push_str(&format!(" (HTTP {status})"));
    }
    OpenCodeFailure {
        state,
        stop_reason,
        class,
        message,
    }
}

fn interrupted() -> OpenCodeFailure {
    OpenCodeFailure {
        state: "cancelled",
        stop_reason: "cancelled",
        class: "unknown",
        message: "The turn was stopped.".to_owned(),
    }
}

/// One field of a pending OpenCode 2 form, kept to coerce BiBCode's answer to its type and to
/// turn a chosen option's label back into its value.
#[derive(Clone, Debug)]
pub(crate) struct FormField {
    key: String,
    kind: String,
    options: Vec<(String, String)>,
}

/// A form as BiBCode questions, or `None` when a field cannot be answered here: BiBCode's question
/// UI offers choices, so a field without options (free text, numbers) or an `external` field
/// (such as an OAuth link) makes the form unanswerable.
pub(crate) fn form_questions(form: &Value) -> Option<(Vec<Value>, Vec<FormField>)> {
    let title = non_empty(form.get("title")).unwrap_or("OpenCode needs an answer");
    let mut questions = Vec::new();
    let mut fields = Vec::new();
    for field in form.get("fields")?.as_array()? {
        let key = non_empty(field.get("key"))?;
        let kind = field.get("type").and_then(Value::as_str)?;
        if field.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        // BiBCode asks every question it shows, so a field that is active only for some answers
        // could not be submitted.
        if field
            .get("when")
            .and_then(Value::as_array)
            .is_some_and(|conditions| !conditions.is_empty())
        {
            return None;
        }
        let mut values = Vec::new();
        let options = match kind {
            "string" | "multiselect" => field
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    let value = non_empty(option.get("value"));
                    let label = non_empty(option.get("label")).or(value)?;
                    values.push((label.to_owned(), value.unwrap_or(label).to_owned()));
                    let description = non_empty(option.get("description")).unwrap_or(label);
                    Some(json!({ "label": label, "description": description }))
                })
                .collect::<Vec<_>>(),
            "boolean" => vec![
                json!({ "label": "Yes", "description": "Yes" }),
                json!({ "label": "No", "description": "No" }),
            ],
            _ => return None,
        };
        if options.is_empty() {
            return None;
        }
        questions.push(json!({
            "id": key,
            "header": non_empty(field.get("title")).unwrap_or(key),
            "question": non_empty(field.get("description")).unwrap_or(title),
            "options": options,
            "multiSelect": kind == "multiselect",
        }));
        fields.push(FormField {
            key: key.to_owned(),
            kind: kind.to_owned(),
            options: values,
        });
    }
    (!questions.is_empty()).then_some((questions, fields))
}

/// BiBCode's answers (question id → string or list) as an OpenCode 2 form answer, each value
/// coerced to its field's type. Unanswered fields are left out.
pub(crate) fn form_answer(fields: &[FormField], answers: &Value) -> Value {
    let mut answer = serde_json::Map::new();
    for field in fields {
        let values = match answers.get(&field.key) {
            Some(Value::String(value)) => vec![value.clone()],
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(Value::Bool(value)) => vec![value.to_string()],
            Some(Value::Number(value)) => vec![value.to_string()],
            _ => Vec::new(),
        };
        // A chosen option answers with its value; anything else is the user's own answer.
        let values = values
            .into_iter()
            .map(|answer| {
                let answer = answer.trim().to_owned();
                field
                    .options
                    .iter()
                    .find(|(label, _)| *label == answer)
                    .map_or(answer, |(_, value)| value.clone())
            })
            .collect::<Vec<_>>();
        let Some(first) = values.first().cloned() else {
            continue;
        };
        let value = match field.kind.as_str() {
            "multiselect" => json!(values),
            "boolean" => json!(matches!(
                first.to_ascii_lowercase().as_str(),
                "yes" | "true" | "y" | "1"
            )),
            _ => json!(first),
        };
        answer.insert(field.key.clone(), value);
    }
    Value::Object(answer)
}

fn non_empty(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// What the root-turn translator asks the runtime to do for one OpenCode 2 event.
#[derive(Debug)]
pub(crate) enum V2Output {
    /// The root session started executing (BiBCode's prompt, or OpenCode delivering a
    /// background result on its own).
    ExecutionStarted,
    TextDelta {
        message_id: String,
        delta: String,
    },
    AssistantCompleted {
        message_id: String,
    },
    /// The root execution ended; `None` is a success.
    TurnEnded(Option<OpenCodeFailure>),
    PermissionAsked {
        request_id: String,
        session_id: String,
        detail: String,
    },
    PermissionSettled {
        request_id: String,
    },
    FormAsked {
        form_id: String,
        session_id: String,
        questions: Vec<Value>,
        fields: Vec<FormField>,
    },
    /// A form BiBCode cannot show, to be cancelled so the agent is told nobody answered.
    FormUnanswerable {
        form_id: String,
        session_id: String,
    },
    FormSettled {
        form_id: String,
    },
}

/// Folds the global OpenCode 2 event stream into the root session's turn lifecycle. Events of
/// other sessions are ignored, except that permission asks and forms of the root's descendants
/// are surfaced: a foreground subagent blocks its parent until they are answered.
pub(crate) struct V2TurnTranslator {
    root: String,
    descendants: HashSet<String>,
    text: HashMap<String, String>,
    error: Option<OpenCodeFailure>,
}

impl V2TurnTranslator {
    pub(crate) fn new(root: &str) -> Self {
        Self {
            root: root.to_owned(),
            descendants: HashSet::new(),
            text: HashMap::new(),
            error: None,
        }
    }

    /// Records a descendant found by reading the session tree, such as on resume.
    pub(crate) fn track_descendant(&mut self, session_id: &str) {
        if session_id != self.root && self.descendants.len() < MAX_TRACKED_DESCENDANTS {
            self.descendants.insert(session_id.to_owned());
        }
    }

    fn tracks(&self, session_id: &str) -> bool {
        session_id == self.root || self.descendants.contains(session_id)
    }

    pub(crate) fn handle(&mut self, event: &Value) -> Vec<V2Output> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = event.get("data").unwrap_or(&Value::Null);
        let session_id = data.get("sessionID").and_then(Value::as_str);
        match kind {
            "session.created" => {
                if let (Some(child), Some(parent)) =
                    (session_id, data.get("parentID").and_then(Value::as_str))
                    && self.tracks(parent)
                    && child != self.root
                    && self.descendants.len() < MAX_TRACKED_DESCENDANTS
                {
                    self.descendants.insert(child.to_owned());
                }
                Vec::new()
            }
            "permission.asked" => {
                let (Some(session_id), Some(request_id)) =
                    (session_id, data.get("id").and_then(Value::as_str))
                else {
                    return Vec::new();
                };
                if !self.tracks(session_id) {
                    return Vec::new();
                }
                let action = non_empty(data.get("action")).unwrap_or("tool");
                let resources = data
                    .get("resources")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let detail = if resources.is_empty() {
                    action.to_owned()
                } else {
                    format!("{action}: {resources}")
                };
                vec![V2Output::PermissionAsked {
                    request_id: request_id.to_owned(),
                    session_id: session_id.to_owned(),
                    detail,
                }]
            }
            "permission.replied" => data
                .get("requestID")
                .and_then(Value::as_str)
                .map(|request_id| V2Output::PermissionSettled {
                    request_id: request_id.to_owned(),
                })
                .into_iter()
                .collect(),
            "form.created" => {
                let form = data.get("form").unwrap_or(&Value::Null);
                let (Some(form_id), Some(session_id)) = (
                    form.get("id").and_then(Value::as_str),
                    form.get("sessionID").and_then(Value::as_str),
                ) else {
                    return Vec::new();
                };
                if !self.tracks(session_id) {
                    return Vec::new();
                }
                vec![match form_questions(form) {
                    Some((questions, fields)) => V2Output::FormAsked {
                        form_id: form_id.to_owned(),
                        session_id: session_id.to_owned(),
                        questions,
                        fields,
                    },
                    None => V2Output::FormUnanswerable {
                        form_id: form_id.to_owned(),
                        session_id: session_id.to_owned(),
                    },
                }]
            }
            "form.replied" | "form.cancelled" => data
                .get("id")
                .and_then(Value::as_str)
                .map(|form_id| V2Output::FormSettled {
                    form_id: form_id.to_owned(),
                })
                .into_iter()
                .collect(),
            _ if session_id != Some(self.root.as_str()) => Vec::new(),
            "session.execution.started" => {
                self.text.clear();
                self.error = None;
                vec![V2Output::ExecutionStarted]
            }
            "session.text.delta" => {
                let (Some(message_id), Some(delta)) = (
                    data.get("assistantMessageID").and_then(Value::as_str),
                    data.get("delta").and_then(Value::as_str),
                ) else {
                    return Vec::new();
                };
                if delta.is_empty() {
                    return Vec::new();
                }
                let key = text_key(message_id, data);
                if let Some(text) = self.text.get_mut(&key) {
                    text.push_str(delta);
                } else if self.text.len() < MAX_TRACKED_TEXT_STREAMS {
                    self.text.insert(key, delta.to_owned());
                }
                vec![V2Output::TextDelta {
                    message_id: message_id.to_owned(),
                    delta: delta.to_owned(),
                }]
            }
            "session.text.ended" => {
                let (Some(message_id), Some(text)) = (
                    data.get("assistantMessageID").and_then(Value::as_str),
                    data.get("text").and_then(Value::as_str),
                ) else {
                    return Vec::new();
                };
                let seen = self
                    .text
                    .remove(&text_key(message_id, data))
                    .unwrap_or_default();
                // The full text completes deltas the stream missed; text that disagrees with what
                // was already shown is left alone rather than duplicated.
                match text.strip_prefix(seen.as_str()) {
                    Some(rest) if !rest.is_empty() => vec![V2Output::TextDelta {
                        message_id: message_id.to_owned(),
                        delta: rest.to_owned(),
                    }],
                    _ => Vec::new(),
                }
            }
            "session.step.ended" => data
                .get("assistantMessageID")
                .and_then(Value::as_str)
                .map(|message_id| V2Output::AssistantCompleted {
                    message_id: message_id.to_owned(),
                })
                .into_iter()
                .collect(),
            "session.step.failed" => {
                if let Some(error) = data.get("error") {
                    self.error = Some(classify_error(error));
                }
                Vec::new()
            }
            "session.execution.succeeded" => {
                self.text.clear();
                self.error = None;
                vec![V2Output::TurnEnded(None)]
            }
            "session.execution.interrupted" => {
                self.text.clear();
                self.error = None;
                vec![V2Output::TurnEnded(Some(interrupted()))]
            }
            "session.execution.failed" => {
                self.text.clear();
                let failure = data
                    .get("error")
                    .map(classify_error)
                    .or_else(|| self.error.take())
                    .unwrap_or_else(|| classify_error(&Value::Null));
                self.error = None;
                vec![V2Output::TurnEnded(Some(failure))]
            }
            _ => Vec::new(),
        }
    }
}

fn text_key(message_id: &str, data: &Value) -> String {
    let ordinal = data
        .get("ordinal")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    format!("{message_id}:{ordinal}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_events(scenario: &str) -> Vec<Value> {
        let path = format!(
            "{}/tests/fixtures/opencode-v2/{scenario}.events.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{path}: {error}"))
            .lines()
            .map(|line| serde_json::from_str(line).expect("fixture event"))
            .collect()
    }

    fn root_of(events: &[Value]) -> String {
        events
            .iter()
            .find(|event| {
                event["type"] == "session.created" && event["data"].get("parentID").is_none()
            })
            .and_then(|event| event["data"]["sessionID"].as_str())
            .expect("root session")
            .to_owned()
    }

    fn translate(scenario: &str) -> Vec<V2Output> {
        let events = fixture_events(scenario);
        let mut translator = V2TurnTranslator::new(&root_of(&events));
        events
            .iter()
            .flat_map(|event| translator.handle(event))
            .collect()
    }

    fn text_of(outputs: &[V2Output]) -> String {
        outputs
            .iter()
            .filter_map(|output| match output {
                V2Output::TextDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_text_reply_starts_streams_completes_and_ends_the_turn() {
        let outputs = translate("hello");
        assert!(matches!(outputs.first(), Some(V2Output::ExecutionStarted)));
        assert_eq!(text_of(&outputs), "Hello from mock.");
        assert!(
            outputs
                .iter()
                .any(|output| matches!(output, V2Output::AssistantCompleted { .. }))
        );
        assert!(matches!(outputs.last(), Some(V2Output::TurnEnded(None))));
    }

    #[test]
    fn a_shell_permission_is_asked_with_its_action_and_command() {
        let outputs = translate("shell");
        let asked = outputs
            .iter()
            .find_map(|output| match output {
                V2Output::PermissionAsked {
                    request_id, detail, ..
                } => Some((request_id.clone(), detail.clone())),
                _ => None,
            })
            .expect("permission asked");
        assert!(asked.0.starts_with("per_"));
        assert_eq!(asked.1, "shell: echo bibcode-v2");
        assert!(outputs.iter().any(|output| matches!(
            output,
            V2Output::PermissionSettled { request_id } if *request_id == asked.0
        )));
        assert_eq!(text_of(&outputs), "Done after tool.");
        assert!(matches!(outputs.last(), Some(V2Output::TurnEnded(None))));
    }

    #[test]
    fn a_question_form_becomes_one_question_per_field() {
        let outputs = translate("question");
        let (questions, fields) = outputs
            .iter()
            .find_map(|output| match output {
                V2Output::FormAsked {
                    questions, fields, ..
                } => Some((questions.clone(), fields.clone())),
                _ => None,
            })
            .expect("form asked");
        assert_eq!(
            questions,
            vec![json!({
                "id": "q0",
                "header": "Color",
                "question": "Pick a color",
                "options": [
                    {"label": "Red", "description": "Warm"},
                    {"label": "Blue", "description": "Cool"},
                ],
                "multiSelect": false,
            })]
        );
        assert_eq!(
            form_answer(&fields, &json!({ "q0": ["Blue"] })),
            json!({ "q0": "Blue" })
        );
        assert!(
            outputs
                .iter()
                .any(|output| matches!(output, V2Output::FormSettled { .. }))
        );
    }

    #[test]
    fn a_foreground_child_session_does_not_end_the_root_turn() {
        let outputs = translate("child");
        let ended = outputs
            .iter()
            .filter(|output| matches!(output, V2Output::TurnEnded(_)))
            .count();
        assert_eq!(ended, 1, "only the root execution ends the turn");
        assert_eq!(
            text_of(&outputs),
            "Done after tool.",
            "the child's text is not root text"
        );
    }

    #[test]
    fn a_background_result_arrives_in_an_execution_opencode_starts_itself() {
        let outputs = translate("background");
        let starts = outputs
            .iter()
            .filter(|output| matches!(output, V2Output::ExecutionStarted))
            .count();
        let ends = outputs
            .iter()
            .filter(|output| matches!(output, V2Output::TurnEnded(None)))
            .count();
        assert_eq!((starts, ends), (2, 2));
        assert!(text_of(&outputs).ends_with("Background result noted."));
    }

    #[test]
    fn provider_failures_and_interrupts_end_the_turn_with_their_state() {
        let failed = translate("fail");
        let Some(V2Output::TurnEnded(Some(failure))) = failed.last() else {
            panic!("failed turn: {failed:?}");
        };
        assert_eq!(
            (failure.state, failure.stop_reason, failure.class),
            ("failed", "error", "provider_error")
        );
        assert_eq!(
            failure.message,
            "mock provider rejected the request (HTTP 400)"
        );
        let interrupted = translate("interrupt");
        let Some(V2Output::TurnEnded(Some(failure))) = interrupted.last() else {
            panic!("interrupted turn: {interrupted:?}");
        };
        assert_eq!(
            (failure.state, failure.stop_reason),
            ("cancelled", "cancelled")
        );
    }

    #[test]
    fn missed_text_deltas_are_completed_from_the_ended_text() {
        let mut translator = V2TurnTranslator::new("ses_root");
        let data = |extra: Value| {
            let mut data = json!({"sessionID":"ses_root","assistantMessageID":"msg_a","ordinal":0});
            data.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            data
        };
        translator
            .handle(&json!({"type":"session.execution.started","data":{"sessionID":"ses_root"}}));
        translator
            .handle(&json!({"type":"session.text.delta","data":data(json!({"delta":"Hel"}))}));
        let outputs = translator
            .handle(&json!({"type":"session.text.ended","data":data(json!({"text":"Hello"}))}));
        assert_eq!(text_of(&outputs), "lo");
        let outputs = translator
            .handle(&json!({"type":"session.text.ended","data":data(json!({"text":"Hello"}))}));
        assert_eq!(
            text_of(&outputs),
            "Hello",
            "an unseen stream is shown whole"
        );
    }

    #[test]
    fn foreign_sessions_are_ignored() {
        let mut translator = V2TurnTranslator::new("ses_root");
        for event in [
            json!({"type":"session.execution.started","data":{"sessionID":"ses_other"}}),
            json!({"type":"permission.asked","data":{"id":"per_1","sessionID":"ses_other","action":"shell","resources":["rm"]}}),
            json!({"type":"form.created","data":{"form":{"id":"frm_1","sessionID":"ses_other","title":"Q","fields":[{"key":"q0","type":"string"}]}}}),
        ] {
            assert!(translator.handle(&event).is_empty(), "{event}");
        }
    }

    #[test]
    fn rulesets_and_ids_use_the_v2_names() {
        assert_eq!(
            permission_rules("full-access"),
            vec![json!({"action":"*","resource":"*","effect":"allow"})]
        );
        let ask = permission_rules("approval-required");
        assert!(ask.contains(&json!({"action":"shell","resource":"*","effect":"ask"})));
        assert!(ask.contains(&json!({"action":"question","resource":"*","effect":"allow"})));
        assert!(!ask.iter().any(|rule| rule["action"] == "bash"));
        assert_eq!(
            message_id("3f2a1b9c-0000-4000-8000-000000000000"),
            "msg_3f2a1b9c000040008000000000000000"
        );
        assert_eq!(message_id("msg_abc"), "msg_abc");
    }

    #[test]
    fn choice_forms_coerce_answers_and_forms_without_choices_are_unanswerable() {
        let (questions, fields) = form_questions(&json!({
            "title": "Configure",
            "fields": [
                {"key":"enabled","type":"boolean","title":"Enabled"},
                {"key":"tags","type":"multiselect","options":[{"value":"a","label":"A"}]},
            ]
        }))
        .expect("answerable form");
        assert_eq!(questions[0]["question"], "Configure");
        assert_eq!(questions[1]["multiSelect"], true);
        assert_eq!(
            form_answer(&fields, &json!({"enabled":"Yes","tags":["A"]})),
            json!({"enabled":true,"tags":["a"]}),
            "a chosen option answers with its value, not its label"
        );
        for field in [
            json!({"key":"oauth","type":"external","url":"https://example.test"}),
            json!({"key":"count","type":"integer"}),
            json!({"key":"name","type":"string"}),
            json!({"key":"tags","type":"multiselect","options":[{"value":"a","label":"A"}],"when":[{"key":"enabled","op":"eq","value":true}]}),
        ] {
            assert!(
                form_questions(&json!({"title":"Form","fields":[field]})).is_none(),
                "{field}"
            );
        }
    }
}

/// Walks an OpenCode 2 history newest first to find where the n-th most recent turn starts. A
/// turn is one execution, which OpenCode 2 ends with an `idle` entry. Executions OpenCode starts
/// on its own (a background subagent's result, opened by a synthetic message) are turns too, so
/// turns are not user messages. Model, agent and location switches belong to the execution they
/// precede; switches that precede none (a prompt that was never admitted) are not a turn.
#[derive(Default)]
pub(crate) struct TurnStartFinder {
    turns: usize,
    /// The oldest message seen of the turn being walked.
    start: Option<String>,
    /// Whether the turn being walked has more than switches.
    executed: bool,
    oldest_start: Option<String>,
}

impl TurnStartFinder {
    /// Takes the next message, newest first; returns the n-th turn's first message once it is
    /// known.
    pub(crate) fn push(&mut self, message: &Value, n: usize) -> Option<String> {
        let kind = message.get("type").and_then(Value::as_str);
        if kind == Some("idle") {
            return self.close(n);
        }
        self.start = Some(message.get("id").and_then(Value::as_str)?.to_owned());
        self.executed |= !matches!(
            kind,
            Some("agent-switched" | "model-switched" | "location-switched")
        );
        None
    }

    /// The history ended: the n-th turn's first message, or the oldest turn's when there are
    /// fewer.
    pub(crate) fn finish(mut self, n: usize) -> Option<String> {
        self.close(n);
        self.oldest_start
    }

    fn close(&mut self, n: usize) -> Option<String> {
        let start = self.start.take()?;
        if !std::mem::take(&mut self.executed) {
            return None;
        }
        self.turns += 1;
        self.oldest_start = Some(start.clone());
        (self.turns == n).then_some(start)
    }
}

#[cfg(test)]
mod turn_tests {
    use serde_json::json;

    use super::TurnStartFinder;

    /// The captured background scenario, newest first: a prompt's turn, then the turn OpenCode
    /// started to deliver the subagent's result.
    #[test]
    fn turns_start_at_their_first_message_whoever_started_them() {
        let history = [
            json!({ "id": "idle-2", "type": "idle" }),
            json!({ "id": "reply-2", "type": "assistant" }),
            json!({ "id": "result", "type": "synthetic" }),
            json!({ "id": "idle-1", "type": "idle" }),
            json!({ "id": "reply-1b", "type": "assistant" }),
            json!({ "id": "reply-1a", "type": "assistant" }),
            json!({ "id": "prompt", "type": "user" }),
            json!({ "id": "switch", "type": "model-switched" }),
        ];
        let start = |n: usize| {
            let mut finder = TurnStartFinder::default();
            history
                .iter()
                .find_map(|message| finder.push(message, n))
                .or_else(|| finder.finish(n))
        };
        assert_eq!(start(1).as_deref(), Some("result"));
        assert_eq!(
            start(2).as_deref(),
            Some("switch"),
            "a turn's setup goes with it"
        );
        assert_eq!(
            start(5).as_deref(),
            Some("switch"),
            "fewer turns: all of them"
        );
        let unadmitted = [
            json!({ "id": "switch-3", "type": "model-switched" }),
            json!({ "id": "idle-2", "type": "idle" }),
            json!({ "id": "reply-2", "type": "assistant" }),
            json!({ "id": "prompt-2", "type": "user" }),
            json!({ "id": "idle-1", "type": "idle" }),
        ];
        let mut finder = TurnStartFinder::default();
        assert_eq!(
            unadmitted
                .iter()
                .find_map(|message| finder.push(message, 1))
                .as_deref(),
            Some("prompt-2"),
            "switches no prompt followed are not a turn"
        );
        let running = [
            json!({ "id": "reply-3", "type": "assistant" }),
            json!({ "id": "prompt-3", "type": "user" }),
            json!({ "id": "idle-2", "type": "idle" }),
        ];
        let mut finder = TurnStartFinder::default();
        assert_eq!(
            running
                .iter()
                .find_map(|message| finder.push(message, 1))
                .as_deref(),
            Some("prompt-3"),
            "a turn still running counts"
        );
    }
}

/// OpenCode 2 catalogs (`/api/provider`, `/api/model`, `/api/agent`, `/api/command`,
/// `/api/skill`) as the OpenCode 1 shapes `build_inventory_snapshot` reads.
pub(crate) mod catalog {
    use serde_json::{Map, Value, json};

    fn data(response: &Value) -> impl Iterator<Item = &Value> {
        response
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    }

    /// `{all:[{id, models:{id:{name, variants:{id:{}}}}}], connected:[ids]}` from the usable
    /// models (OpenCode 2 lists only those its policies allow) of providers that are not
    /// disabled; an `auto` provider is active once it has credentials.
    pub(crate) fn provider_list(providers: &Value, models: &Value) -> Value {
        let disabled = data(providers)
            .filter(|provider| {
                provider.get("activation").and_then(Value::as_str) == Some("disabled")
            })
            .filter_map(|provider| provider.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        let mut grouped: Vec<(String, Map<String, Value>)> = Vec::new();
        for model in data(models) {
            let (Some(provider_id), Some(model_id)) = (
                model.get("providerID").and_then(Value::as_str),
                model.get("id").and_then(Value::as_str),
            ) else {
                continue;
            };
            if model.get("enabled").and_then(Value::as_bool) == Some(false)
                || model.get("status").and_then(Value::as_str) == Some("deprecated")
                || disabled.contains(&provider_id)
            {
                continue;
            }
            let variants = model
                .get("variants")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|variant| variant.get("id").and_then(Value::as_str))
                .map(|id| (id.to_owned(), json!({})))
                .collect::<Map<_, _>>();
            let entry = json!({
                "name": model.get("name").and_then(Value::as_str).unwrap_or(model_id),
                "variants": variants,
            });
            match grouped.iter_mut().find(|(id, _)| id == provider_id) {
                Some((_, models)) => {
                    models.insert(model_id.to_owned(), entry);
                }
                None => grouped.push((
                    provider_id.to_owned(),
                    Map::from_iter([(model_id.to_owned(), entry)]),
                )),
            }
        }
        json!({
            "connected": grouped.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            "all": grouped
                .into_iter()
                .map(|(id, models)| json!({ "id": id, "models": models }))
                .collect::<Vec<_>>(),
        })
    }

    /// Agents keyed by their id, which is what a session selects.
    pub(crate) fn agents(agents: &Value) -> Value {
        Value::Array(
            data(agents)
                .filter_map(|agent| {
                    let id = agent.get("id").and_then(Value::as_str)?;
                    let mut out = json!({ "name": id });
                    for key in ["description", "mode", "hidden"] {
                        if let Some(value) = agent.get(key) {
                            out[key] = value.clone();
                        }
                    }
                    match agent.get("model") {
                        Some(Value::String(model)) => out["model"] = json!(model),
                        Some(model @ Value::Object(_)) => {
                            if let (Some(provider), Some(id)) = (
                                model.get("providerID").and_then(Value::as_str),
                                model.get("id").and_then(Value::as_str),
                            ) {
                                out["model"] = json!(format!("{provider}/{id}"));
                            }
                        }
                        _ => {}
                    }
                    Some(out)
                })
                .collect(),
        )
    }

    /// Commands, then skills as `source: "skill"` entries (BiBCode offers both under `/`).
    pub(crate) fn commands(commands: &Value, skills: &Value) -> Value {
        Value::Array(
            data(commands)
                .filter_map(|command| {
                    let name = command.get("name").and_then(Value::as_str)?;
                    let mut out = json!({ "name": name, "source": "command" });
                    for key in ["description", "template"] {
                        if let Some(value) = command.get(key) {
                            out[key] = value.clone();
                        }
                    }
                    Some(out)
                })
                .chain(data(skills).filter_map(|skill| {
                    let id = skill.get("id").and_then(Value::as_str)?;
                    let mut out = json!({ "name": id, "source": "skill" });
                    if let Some(description) = skill.get("description") {
                        out["description"] = description.clone();
                    }
                    Some(out)
                }))
                .collect(),
        )
    }

    /// Whether `name` is one of the skills `GET /api/skill` lists.
    pub(crate) fn is_skill(skills: &Value, name: &str) -> bool {
        data(skills).any(|skill| skill.get("id").and_then(Value::as_str) == Some(name))
    }

    /// Whether `name` is one of the commands `GET /api/command` lists.
    pub(crate) fn is_command(commands: &Value, name: &str) -> bool {
        data(commands).any(|command| command.get("name").and_then(Value::as_str) == Some(name))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::provider::opencode::model::build_inventory_snapshot;

        fn captured(route: &str) -> Value {
            let path = format!(
                "{}/tests/fixtures/opencode-v2/catalog.exchanges.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let exchanges: Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            exchanges["exchanges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|exchange| {
                    exchange["method"] == "GET"
                        && exchange["path"].as_str().unwrap().split('?').next() == Some(route)
                })
                .unwrap()["response"]
                .clone()
        }

        /// The OpenCode 2.0.29 catalogs produce the same inventory a 1.x server would: models
        /// with their variants, selectable agents, commands, and skills under `/`.
        #[test]
        fn captured_v2_catalogs_build_the_inventory() {
            let snapshot = build_inventory_snapshot(
                &provider_list(&captured("/api/provider"), &captured("/api/model")),
                &agents(&captured("/api/agent")),
                &commands(&captured("/api/command"), &captured("/api/skill")),
                &["custom/model".to_owned()],
            );
            assert_eq!(snapshot.auth["status"], "authenticated");
            let slugs = snapshot
                .models
                .iter()
                .map(|model| model.slug.as_str())
                .collect::<Vec<_>>();
            assert_eq!(slugs, ["mock/mock-model", "custom/model"]);
            let variant = snapshot.models[0].capabilities["optionDescriptors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|descriptor| descriptor["id"] == "variant")
                .expect("variant option")
                .clone();
            assert_eq!(
                variant["options"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|option| option["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["high", "low", "medium"]
            );
            let agent_names = snapshot
                .agents
                .iter()
                .map(|agent| agent["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert!(agent_names.contains(&"build"));
            assert!(agent_names.contains(&"plan"));
            assert!(!agent_names.contains(&"title"), "hidden agents stay hidden");
            let command_names = snapshot
                .commands
                .iter()
                .map(|command| command["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(command_names, ["init", "review"]);
            let skill_names = snapshot
                .skills
                .iter()
                .map(|skill| skill["name"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(skill_names, ["opencode", "report"]);
            assert!(is_skill(&captured("/api/skill"), "report"));
            assert!(!is_skill(&captured("/api/skill"), "review"));
        }

        /// An `auto` provider (active once it has credentials) keeps its models; only a disabled
        /// provider's models are dropped.
        #[test]
        fn only_disabled_providers_lose_their_models() {
            let providers = json!({ "data": [
                { "id": "anthropic", "activation": "auto" },
                { "id": "mock", "activation": "enabled" },
                { "id": "old", "activation": "disabled" },
            ]});
            let models = json!({ "data": [
                { "id": "sonnet", "providerID": "anthropic" },
                { "id": "mock-model", "providerID": "mock" },
                { "id": "legacy", "providerID": "old" },
            ]});
            assert_eq!(
                provider_list(&providers, &models)["connected"],
                json!(["anthropic", "mock"])
            );
        }
    }
}
