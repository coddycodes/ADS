//! OpenCode 2 input for the activity tracker. `OpenCodeActivityTracker` consumes OpenCode 1
//! shapes (`{type, properties}` events and the `/session/...` DTOs); this module maps OpenCode 2
//! events and REST responses onto those shapes, so both wires share one tracker. Shapes are those
//! of OpenCode 2.0.29, captured from a real server.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

/// Bounds the per-session and per-call state kept to complete V1 shapes.
const MAX_TRACKED: usize = 512;

/// Turns OpenCode 2 events into the OpenCode 1 events the activity tracker understands.
#[derive(Default)]
pub(crate) struct V2ActivityNormalizer {
    /// `(session, call id)` → tool name, from `session.tool.input.started`.
    tool_names: HashMap<(String, String), String>,
    /// `(session, call id)` → tool input, from `session.tool.called`.
    tool_inputs: HashMap<(String, String), Value>,
    /// session → its latest assistant message id and creation time.
    assistant: HashMap<String, (String, u64)>,
}

fn v1(event: &Value, kind: &str, properties: Value) -> Value {
    let mut out = json!({ "type": kind, "properties": properties });
    if let Some(id) = event.get("id") {
        out["id"] = id.clone();
    }
    out
}

fn bounded_insert<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, V>, key: K, value: V) {
    if map.len() >= MAX_TRACKED && !map.contains_key(&key) {
        map.clear();
    }
    map.insert(key, value);
}

impl V2ActivityNormalizer {
    pub(crate) fn normalize(&mut self, event: &Value) -> Vec<Value> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = event.get("data").unwrap_or(&Value::Null);
        let created = event.get("created").and_then(Value::as_u64);
        if kind == "server.connected" {
            return vec![json!({ "type": "server.connected", "properties": {} })];
        }
        let Some(session_id) = data.get("sessionID").and_then(Value::as_str) else {
            return Vec::new();
        };
        let session = session_id.to_owned();
        match kind {
            "session.created" => {
                let mut info = json!({ "id": session_id, "time": { "created": created } });
                for field in ["parentID", "title"] {
                    if let Some(value) = data.get(field) {
                        info[field] = value.clone();
                    }
                }
                vec![v1(
                    event,
                    "session.created",
                    json!({ "sessionID": session_id, "info": info }),
                )]
            }
            "session.execution.started" => {
                vec![status(event, session_id, json!({ "type": "busy" }))]
            }
            "session.status" => data
                .get("status")
                .map(|value| status(event, session_id, value.clone()))
                .into_iter()
                .collect(),
            "session.retry.scheduled" => vec![status(
                event,
                session_id,
                json!({
                    "type": "retry",
                    "attempt": data.get("attempt"),
                    "message": data.pointer("/error/message"),
                    "next": data.get("at"),
                }),
            )],
            "session.step.started" => {
                let Some(message_id) = data.get("assistantMessageID").and_then(Value::as_str)
                else {
                    return Vec::new();
                };
                let started = data
                    .get("started")
                    .and_then(Value::as_u64)
                    .or(created)
                    .unwrap_or_default();
                bounded_insert(
                    &mut self.assistant,
                    session.clone(),
                    (message_id.to_owned(), started),
                );
                vec![message_updated(
                    event,
                    session_id,
                    message_id,
                    json!({ "created": started }),
                    None,
                )]
            }
            "session.text.delta" => {
                let (Some(message_id), Some(delta)) = (
                    data.get("assistantMessageID").and_then(Value::as_str),
                    data.get("delta").and_then(Value::as_str),
                ) else {
                    return Vec::new();
                };
                vec![v1(
                    event,
                    "message.part.delta",
                    json!({
                        "sessionID": session_id,
                        "messageID": message_id,
                        "partID": text_part_id(message_id, data),
                        "field": "text",
                        "delta": delta,
                        "time": created,
                    }),
                )]
            }
            "session.text.ended" => {
                let (Some(message_id), Some(text)) = (
                    data.get("assistantMessageID").and_then(Value::as_str),
                    data.get("text").and_then(Value::as_str),
                ) else {
                    return Vec::new();
                };
                vec![part_updated(
                    event,
                    session_id,
                    json!({
                        "id": text_part_id(message_id, data),
                        "sessionID": session_id,
                        "messageID": message_id,
                        "type": "text",
                        "text": text,
                        "time": { "end": created },
                    }),
                )]
            }
            "session.tool.input.started" => {
                if let (Some(call_id), Some(name)) = (
                    data.get("id").and_then(Value::as_str),
                    data.get("name").and_then(Value::as_str),
                ) {
                    bounded_insert(
                        &mut self.tool_names,
                        (session.clone(), call_id.to_owned()),
                        name.to_owned(),
                    );
                }
                Vec::new()
            }
            "session.tool.called" | "session.tool.success" | "session.tool.failed" => {
                let (Some(message_id), Some(call_id)) = (
                    data.get("assistantMessageID").and_then(Value::as_str),
                    data.get("id").and_then(Value::as_str),
                ) else {
                    return Vec::new();
                };
                let key = (session.clone(), call_id.to_owned());
                if kind == "session.tool.called"
                    && let Some(input) = data.get("input")
                {
                    bounded_insert(&mut self.tool_inputs, key.clone(), input.clone());
                }
                let name = self
                    .tool_names
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| "tool".to_owned());
                let input = self.tool_inputs.get(&key).cloned().unwrap_or(json!({}));
                let state = match kind {
                    "session.tool.called" => json!({
                        "status": "running",
                        "input": input,
                        "time": { "start": created },
                    }),
                    "session.tool.success" => json!({
                        "status": "completed",
                        "input": input,
                        "time": { "end": created },
                    }),
                    _ => json!({
                        "status": "error",
                        "input": input,
                        "error": data.pointer("/error/message"),
                        "time": { "end": created },
                    }),
                };
                if kind != "session.tool.called" {
                    self.tool_names.remove(&key);
                    self.tool_inputs.remove(&key);
                }
                vec![part_updated(
                    event,
                    session_id,
                    json!({
                        "id": call_id,
                        "sessionID": session_id,
                        "messageID": message_id,
                        "type": "tool",
                        "callID": call_id,
                        "tool": name,
                        "state": state,
                    }),
                )]
            }
            // An execution's end is its terminal state: completed or failed on the latest
            // assistant message, or an abort.
            "session.execution.succeeded" | "session.execution.failed" => {
                let error = (kind == "session.execution.failed")
                    .then(|| data.get("error").cloned().unwrap_or(json!({})));
                match self.assistant.get(session_id).cloned() {
                    Some((message_id, started)) => vec![message_updated(
                        event,
                        session_id,
                        &message_id,
                        json!({ "created": started, "completed": created }),
                        Some(error.unwrap_or(Value::Null)),
                    )],
                    None => vec![status(event, session_id, json!({ "type": "idle" }))],
                }
            }
            "session.execution.interrupted" => vec![v1(
                event,
                "session.error",
                json!({
                    "sessionID": session_id,
                    "error": { "name": "MessageAbortedError" },
                    "time": created,
                }),
            )],
            _ => Vec::new(),
        }
    }
}

fn status(event: &Value, session_id: &str, status: Value) -> Value {
    v1(
        event,
        "session.status",
        json!({ "sessionID": session_id, "status": status }),
    )
}

fn message_updated(
    event: &Value,
    session_id: &str,
    message_id: &str,
    time: Value,
    error: Option<Value>,
) -> Value {
    let mut info = json!({
        "id": message_id,
        "sessionID": session_id,
        "role": "assistant",
        "time": time,
    });
    match error {
        Some(Value::Null) => info["finish"] = json!("stop"),
        Some(error) => info["error"] = error,
        None => {}
    }
    v1(
        event,
        "message.updated",
        json!({ "sessionID": session_id, "info": info }),
    )
}

fn part_updated(event: &Value, session_id: &str, part: Value) -> Value {
    v1(
        event,
        "message.part.updated",
        json!({ "sessionID": session_id, "part": part }),
    )
}

fn text_part_id(message_id: &str, data: &Value) -> String {
    let ordinal = data
        .get("ordinal")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    format!("{message_id}:text:{ordinal}")
}

/// `GET /api/session?parentID=…` → the V1 children response (`[{id, parentID, title, time}]`).
pub(crate) fn children(response: &Value) -> Value {
    Value::Array(
        response
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|session| {
                let mut child = Map::new();
                for field in ["id", "parentID", "title"] {
                    if let Some(value) = session.get(field) {
                        child.insert(field.to_owned(), value.clone());
                    }
                }
                child.insert(
                    "time".to_owned(),
                    json!({
                        "created": session.pointer("/time/created"),
                        "updated": session.pointer("/time/updated"),
                    }),
                );
                Value::Object(child)
            })
            .collect(),
    )
}

/// `GET /api/session/active` → the V1 status map: an active session is busy, any other idle.
pub(crate) fn statuses(response: &Value) -> Value {
    Value::Object(
        response
            .get("data")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(session_id, _)| (session_id.clone(), json!({ "type": "busy" })))
            .collect(),
    )
}

/// `GET /api/session/{id}/message` → the V1 history (`[{info, parts}]`, oldest first). Only user
/// and assistant messages carry activity. A step's own completion is not the session's: OpenCode
/// 2 records an execution's end as an `idle` entry with its outcome, which completes (succeeded),
/// fails (failed) or aborts (interrupted, read as a cancellation) the assistant message before it.
pub(crate) fn history(session_id: &str, response: &Value) -> Value {
    let mut entries = history_entries(session_id, response);
    let mut last_assistant = None;
    let mut messages = response
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    messages.sort_by_key(|message| message.pointer("/time/created").and_then(Value::as_u64));
    let mut index = 0;
    for message in &messages {
        match message.get("type").and_then(Value::as_str) {
            Some("assistant") if message.get("id").is_some() => {
                last_assistant = Some(index);
                index += 1;
            }
            Some("user") if message.get("id").is_some() => index += 1,
            Some("idle") => {
                let Some(assistant) = last_assistant.take() else {
                    continue;
                };
                let info = &mut entries[assistant]["info"];
                match message.get("outcome").and_then(Value::as_str) {
                    Some("succeeded") => {
                        info["time"]["completed"] = message
                            .pointer("/time/created")
                            .cloned()
                            .unwrap_or(Value::Null);
                        info["finish"] = json!("stop");
                    }
                    Some(outcome @ ("failed" | "interrupted")) => {
                        let error = messages
                            .iter()
                            .find(|candidate| {
                                candidate.get("id") == info.get("id")
                                    && candidate.get("error").is_some()
                            })
                            .and_then(|candidate| candidate.get("error").cloned());
                        info["error"] = match outcome {
                            "interrupted" => json!({ "type": "aborted" }),
                            _ => error.unwrap_or_else(|| json!({ "type": "failed" })),
                        };
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Value::Array(entries)
}

fn history_entries(session_id: &str, response: &Value) -> Vec<Value> {
    let mut messages = response
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // OpenCode 2 lists newest first.
    messages.sort_by_key(|message| message.pointer("/time/created").and_then(Value::as_u64));
    messages
        .iter()
        .filter_map(|message| {
            let id = message.get("id").and_then(Value::as_str)?;
            let role = message.get("type").and_then(Value::as_str)?;
            if !matches!(role, "user" | "assistant") {
                return None;
            }
            let info = json!({
                "id": id,
                "sessionID": session_id,
                "role": role,
                "time": { "created": message.pointer("/time/created") },
            });
            let parts = message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, content)| match content.get("type")?.as_str()? {
                    "text" => Some(json!({
                        "id": format!("{id}:text:{index}"),
                        "sessionID": session_id,
                        "messageID": id,
                        "type": "text",
                        "text": content.get("text"),
                    })),
                    "tool" => {
                        let call_id = content.get("id")?.as_str()?;
                        let status = match content.pointer("/state/status")?.as_str()? {
                            "streaming" => "pending",
                            status => status,
                        };
                        Some(json!({
                            "id": call_id,
                            "sessionID": session_id,
                            "messageID": id,
                            "type": "tool",
                            "callID": call_id,
                            "tool": content.get("name"),
                            "state": {
                                "status": status,
                                "input": content.pointer("/state/input"),
                                "time": {
                                    "start": content.pointer("/time/ran")
                                        .or_else(|| content.pointer("/time/created")),
                                    "end": content.pointer("/time/completed"),
                                },
                            },
                        }))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            Some(json!({ "info": info, "parts": parts }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        activity::{ActivityLifecycle, ProviderActivityMutation},
        provider::opencode::{
            activity::OpenCodeActivityTracker,
            model::{OpenCodeChildSessionDto, OpenCodeMessageDto, OpenCodeStatusMapDto},
        },
    };

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/opencode-v2/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn children_capture() -> (Vec<Value>, Value) {
        let events = fixture("children.events.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).expect("event"))
            .collect();
        let exchanges =
            serde_json::from_str(&fixture("children.exchanges.json")).expect("exchanges");
        (events, exchanges)
    }

    fn captured<'a>(exchanges: &'a Value, path_prefix: &str, nth: usize) -> &'a Value {
        &exchanges["exchanges"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|exchange| {
                exchange["method"] == "GET"
                    && exchange["path"].as_str().unwrap().starts_with(path_prefix)
            })
            .nth(nth)
            .unwrap_or_else(|| panic!("no GET {path_prefix} #{nth}"))["response"]
    }

    fn lifecycles(mutations: &[ProviderActivityMutation]) -> Vec<ActivityLifecycle> {
        mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertActor(actor) => Some(actor.status),
                _ => None,
            })
            .collect()
    }

    /// A background subagent captured from OpenCode 2.0.29, fed through the normaliser, becomes a
    /// child actor that runs its tool and completes, while the root's own events are not activity.
    #[test]
    fn a_background_child_is_tracked_from_creation_to_completion() {
        let (events, _) = children_capture();
        let root = events
            .iter()
            .find(|event| {
                event["type"] == "session.created" && event["data"].get("parentID").is_none()
            })
            .and_then(|event| event["data"]["sessionID"].as_str())
            .unwrap()
            .to_owned();
        let mut normalizer = V2ActivityNormalizer::default();
        let mut tracker = OpenCodeActivityTracker::new(&root);
        let mut mutations = Vec::new();
        for event in &events {
            for normalized in normalizer.normalize(event) {
                mutations.extend(tracker.handle_event_at(&normalized, 1).mutations);
            }
        }
        mutations.extend(tracker.flush_text().mutations);

        let states = lifecycles(&mutations);
        assert!(states.contains(&ActivityLifecycle::Running), "{states:?}");
        assert_eq!(states.last(), Some(&ActivityLifecycle::Completed));
        let entries = mutations
            .iter()
            .filter_map(|mutation| match mutation {
                ProviderActivityMutation::AppendEntry(entry) => Some(format!(
                    "{} {}",
                    entry.title,
                    entry.detail.as_deref().unwrap_or_default()
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            entries
                .iter()
                .any(|entry| entry.starts_with("shell running")),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.starts_with("shell completed")),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.contains("Child finished.")),
            "{entries:?}"
        );
    }

    fn root_history(scenario: &str) -> Vec<Value> {
        let exchanges: Value =
            serde_json::from_str(&fixture(&format!("{scenario}.exchanges.json"))).unwrap();
        let session = exchanges["session"].as_str().unwrap().to_owned();
        let response = &exchanges["exchanges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|exchange| {
                exchange["method"] == "GET"
                    && exchange["path"] == format!("/api/session/{session}/message")
            })
            .unwrap()["response"];
        history(&session, response).as_array().unwrap().clone()
    }

    /// A step's own completion is not the session's: only the execution outcome completes it.
    #[test]
    fn history_completes_only_the_message_an_execution_ended_on() {
        let entries = root_history("shell");
        let assistants = entries
            .iter()
            .filter(|entry| entry["info"]["role"] == "assistant")
            .collect::<Vec<_>>();
        assert_eq!(assistants.len(), 2, "the tool step and the reply");
        assert!(assistants[0]["info"]["time"].get("completed").is_none());
        assert!(assistants[0]["info"].get("finish").is_none());
        assert!(assistants[1]["info"]["time"]["completed"].is_u64());
        assert_eq!(assistants[1]["info"]["finish"], "stop");
    }

    /// An interrupted execution is recovered as an abort, which the tracker reads as a
    /// cancellation, not a failure.
    #[test]
    fn history_does_not_report_an_interruption_as_a_failure() {
        let entries = root_history("interrupt");
        let assistant = entries
            .iter()
            .find(|entry| entry["info"]["role"] == "assistant")
            .expect("assistant message");
        assert_eq!(assistant["info"]["error"], json!({ "type": "aborted" }));
        assert!(assistant["info"]["time"].get("completed").is_none());
        let mut tracker = OpenCodeActivityTracker::new("root");
        tracker.reconcile_children(
            "root",
            &json!([{"id":"child","parentID":"root","title":"Child","time":{"created":1}}]),
        );
        let mut info = assistant["info"].clone();
        info["sessionID"] = json!("child");
        let mutations = tracker
            .handle_history("child", &json!([{ "info": info, "parts": [] }]))
            .mutations;
        assert_eq!(lifecycles(&mutations), vec![ActivityLifecycle::Cancelled]);
        let failed = root_history("fail");
        let assistant = failed
            .iter()
            .find(|entry| entry["info"]["role"] == "assistant")
            .expect("assistant message");
        assert_eq!(
            assistant["info"]["error"]["type"],
            "provider.invalid-request"
        );
    }

    #[test]
    fn reconciliation_reads_map_onto_the_opencode_1_shapes() {
        let (events, exchanges) = children_capture();
        let child = events
            .iter()
            .find(|event| {
                event["type"] == "session.created" && event["data"].get("parentID").is_some()
            })
            .and_then(|event| event["data"]["sessionID"].as_str())
            .unwrap()
            .to_owned();

        let children: Vec<OpenCodeChildSessionDto> =
            serde_json::from_value(children(captured(&exchanges, "/api/session?parentID=", 0)))
                .expect("children DTOs");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child);
        assert_eq!(children[0].title.as_deref(), Some("Background task"));

        let running: OpenCodeStatusMapDto =
            serde_json::from_value(statuses(captured(&exchanges, "/api/session/active", 0)))
                .expect("status map");
        assert!(running.contains_key(&child));
        let idle: OpenCodeStatusMapDto =
            serde_json::from_value(statuses(captured(&exchanges, "/api/session/active", 1)))
                .expect("status map");
        assert!(idle.is_empty());

        let mid_run: Vec<OpenCodeMessageDto> = serde_json::from_value(history(
            &child,
            captured(&exchanges, &format!("/api/session/{child}/message"), 0),
        ))
        .expect("history DTOs");
        assert_eq!(mid_run[0].info.role, "user", "oldest first");
        let tool = &mid_run[1].parts[0];
        assert_eq!(tool.part_type, "tool");
        assert_eq!(tool.tool.as_deref(), Some("shell"));
        assert_eq!(tool.state.as_ref().unwrap()["status"], "running");

        let finished: Vec<OpenCodeMessageDto> = serde_json::from_value(history(
            &child,
            captured(&exchanges, &format!("/api/session/{child}/message"), 1),
        ))
        .expect("history DTOs");
        let last = finished.last().expect("assistant reply");
        assert_eq!(last.parts[0].text.as_deref(), Some("Child finished."));
        assert_eq!(last.info.finish.as_deref(), Some("stop"));
    }
}
