use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use uuid::Uuid;

use super::acp::{
    AcpJsonRpcConnection, AcpProtocolError, IncomingEvent, JsonRpcErrorShape, acp_error_class,
    acp_turn_state,
};

const PROVIDER: &str = "grok";
const FIXED_EVENT_TIME: &str = "2026-07-10T00:00:00.000Z";

#[derive(Clone, Debug)]
pub struct GrokSessionOptions {
    pub thread_id: String,
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
    pub runtime_mode: String,
    pub interaction_mode: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokRuntimeEvent {
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
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokRuntimeEventStableView {
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

impl GrokRuntimeEvent {
    #[must_use]
    pub fn stable_view(&self) -> GrokRuntimeEventStableView {
        GrokRuntimeEventStableView {
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
pub enum GrokRuntimeError {
    #[error(transparent)]
    Protocol(#[from] AcpProtocolError),
    #[error("Grok runtime has not started a provider session")]
    MissingProviderSessionId,
    #[error("Unknown pending request id {request_id}")]
    PendingRequestNotFound { request_id: String },
}

#[derive(Clone)]
pub struct GrokSessionRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    options: GrokSessionOptions,
    auth_method_id: String,
    resume_session_id: Option<String>,
    connection: Mutex<AcpJsonRpcConnection>,
    provider_session_id: Mutex<Option<String>>,
    mode_state: Mutex<Option<Value>>,
    runtime_mode: Mutex<String>,
    interaction_mode: Mutex<String>,
    active_turn_id: Mutex<Option<String>>,
    events_tx: mpsc::UnboundedSender<GrokRuntimeEvent>,
    events_rx: Mutex<mpsc::UnboundedReceiver<GrokRuntimeEvent>>,
    event_counter: Mutex<u64>,
    pending_requests: Mutex<HashMap<String, PendingRequest>>,
    task: Mutex<Option<JoinHandle<()>>>,
    ignore_turn_output: Mutex<Option<String>>,
}

#[derive(Clone)]
struct PendingRequest {
    wire_id: Value,
    turn_id: Option<String>,
    accept_option_id: Option<String>,
    accept_for_session_option_id: Option<String>,
    reject_option_id: Option<String>,
}

impl GrokSessionRuntime {
    pub fn new(
        options: GrokSessionOptions,
        connection: AcpJsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
    ) -> Self {
        Self::new_with_auth_method(options, connection, incoming, "cached_token".to_owned())
    }

    pub fn new_with_auth_method(
        options: GrokSessionOptions,
        connection: AcpJsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
        auth_method_id: String,
    ) -> Self {
        Self::new_with_auth_and_resume(options, connection, incoming, auth_method_id, None)
    }

    pub fn new_with_auth_and_resume(
        options: GrokSessionOptions,
        connection: AcpJsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
        auth_method_id: String,
        resume_session_id: Option<String>,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let runtime_mode = options.runtime_mode.clone();
        let interaction_mode = options.interaction_mode.clone();
        let inner = Arc::new(RuntimeInner {
            options,
            auth_method_id,
            resume_session_id,
            connection: Mutex::new(connection.clone()),
            provider_session_id: Mutex::new(None),
            mode_state: Mutex::new(None),
            runtime_mode: Mutex::new(runtime_mode),
            interaction_mode: Mutex::new(interaction_mode),
            active_turn_id: Mutex::new(None),
            events_tx,
            events_rx: Mutex::new(events_rx),
            event_counter: Mutex::new(0),
            pending_requests: Mutex::new(HashMap::new()),
            task: Mutex::new(None),
            ignore_turn_output: Mutex::new(None),
        });
        let runtime = Self { inner };
        runtime.attach_incoming(connection, incoming);
        runtime
    }

    pub async fn start(&self) -> Result<String, GrokRuntimeError> {
        let connection = self.inner.connection.lock().await.clone();
        connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                    "clientInfo": { "name": "coddy-studio-rust", "version": "0.1.1" },
                }),
            )
            .await?;
        connection
            .request(
                "authenticate",
                json!({ "methodId": self.inner.auth_method_id }),
            )
            .await?;
        let response = match self.inner.resume_session_id.as_ref() {
            Some(session_id) => {
                connection
                    .request(
                        "session/load",
                        json!({ "sessionId": session_id, "cwd": self.inner.options.cwd }),
                    )
                    .await?
            }
            None => {
                connection
                    .request(
                        "session/create",
                        json!({
                            "cwd": self.inner.options.cwd,
                            "mcpServers": self.inner.options.mcp_servers,
                        }),
                    )
                    .await?
            }
        };
        let session_id = response
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.inner.resume_session_id.clone())
            .ok_or(GrokRuntimeError::MissingProviderSessionId)?;
        *self.inner.mode_state.lock().await = response.get("modes").cloned();
        *self.inner.provider_session_id.lock().await = Some(session_id.clone());
        self.apply_mode().await?;
        self.emit("session.started", None, None, json!({})).await;
        self.emit("thread.started", None, None, json!({})).await;
        Ok(session_id)
    }

    pub async fn send_turn(
        &self,
        input: Option<&str>,
        attachments: Vec<Value>,
    ) -> Result<String, GrokRuntimeError> {
        let session_id = self.provider_session_id().await?;
        let turn_id = format!("turn-{}", Uuid::new_v4());
        *self.inner.active_turn_id.lock().await = Some(turn_id.clone());
        *self.inner.ignore_turn_output.lock().await = None;
        self.emit("turn.started", Some(turn_id.clone()), None, json!({}))
            .await;
        let connection = self.inner.connection.lock().await.clone();
        let runtime = self.clone();
        let background_turn_id = turn_id.clone();
        let prompt = crate::provider::attachments::prompt_parts(input, attachments);
        tokio::spawn(async move {
            let result = connection
                .request(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": prompt,
                    }),
                )
                .await;
            if runtime.inner.active_turn_id.lock().await.as_deref()
                == Some(background_turn_id.as_str())
            {
                *runtime.inner.active_turn_id.lock().await = None;
            }
            match result {
                Ok(response) => {
                    let stop_reason = response
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("end_turn");
                    let (state, failure_message) = acp_turn_state(stop_reason);
                    let mut payload = json!({
                        "state": state,
                        "stopReason": stop_reason,
                    });
                    if let Some(message) = failure_message {
                        payload["errorMessage"] = json!(message);
                    }
                    runtime
                        .emit("turn.completed", Some(background_turn_id), None, payload)
                        .await;
                }
                Err(error) => {
                    let class = acp_error_class(&error);
                    runtime
                        .emit(
                            "turn.completed",
                            Some(background_turn_id),
                            None,
                            json!({
                                "state": "failed",
                                "stopReason": "error",
                                "errorMessage": error.to_string(),
                                "errorClass": class,
                            }),
                        )
                        .await;
                }
            }
        });
        Ok(turn_id)
    }

    pub async fn set_model(&self, model: &str) -> Result<(), GrokRuntimeError> {
        self.inner
            .connection
            .lock()
            .await
            .request(
                "session/set_model",
                json!({
                    "sessionId": self.provider_session_id().await?,
                    "modelId": model,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn set_runtime_mode(&self, mode: &str) -> Result<(), GrokRuntimeError> {
        *self.inner.runtime_mode.lock().await = mode.to_owned();
        self.apply_mode().await
    }

    pub async fn set_interaction_mode(&self, mode: &str) -> Result<(), GrokRuntimeError> {
        *self.inner.interaction_mode.lock().await = mode.to_owned();
        self.apply_mode().await
    }

    async fn apply_mode(&self) -> Result<(), GrokRuntimeError> {
        let mode_state = self.inner.mode_state.lock().await.clone();
        let runtime_mode = self.inner.runtime_mode.lock().await.clone();
        let interaction_mode = self.inner.interaction_mode.lock().await.clone();
        let Some(mode_id) = crate::provider::acp_mode::resolve_requested_mode_id(
            mode_state.as_ref(),
            &runtime_mode,
            &interaction_mode,
        ) else {
            return Ok(());
        };
        self.inner
            .connection
            .lock()
            .await
            .request(
                "session/set_mode",
                json!({
                    "sessionId": self.provider_session_id().await?,
                    "modeId": mode_id,
                }),
            )
            .await?;
        if let Some(state) = self.inner.mode_state.lock().await.as_mut() {
            state["currentModeId"] = Value::String(mode_id);
        }
        Ok(())
    }

    pub async fn interrupt_turn(&self, turn_id: &str) -> Result<(), GrokRuntimeError> {
        *self.inner.ignore_turn_output.lock().await = Some(turn_id.to_owned());
        let pending_ids = self
            .inner
            .pending_requests
            .lock()
            .await
            .iter()
            .filter_map(|(request_id, pending)| {
                (pending.turn_id.as_deref() == Some(turn_id)).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in pending_ids {
            let _ = self.respond_to_request(&request_id, "cancel").await;
        }
        let session_id = self.provider_session_id().await?;
        let connection = self.inner.connection.lock().await.clone();
        connection
            .notify("session/cancel", json!({ "sessionId": session_id }))
            .await?;
        Ok(())
    }

    pub async fn respond_to_request(
        &self,
        request_id: &str,
        decision: &str,
    ) -> Result<(), GrokRuntimeError> {
        let pending = self
            .inner
            .pending_requests
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| GrokRuntimeError::PendingRequestNotFound {
                request_id: request_id.to_owned(),
            })?;
        let outcome = match decision {
            "acceptForSession" => pending
                .accept_for_session_option_id
                .or(pending.accept_option_id)
                .map(|option_id| json!({ "outcome": "selected", "optionId": option_id }))
                .unwrap_or_else(|| json!({ "outcome": "cancelled" })),
            "accept" => pending
                .accept_option_id
                .map(|option_id| json!({ "outcome": "selected", "optionId": option_id }))
                .unwrap_or_else(|| json!({ "outcome": "cancelled" })),
            "decline" => pending
                .reject_option_id
                .map(|option_id| json!({ "outcome": "selected", "optionId": option_id }))
                .unwrap_or_else(|| json!({ "outcome": "cancelled" })),
            _ => json!({ "outcome": "cancelled" }),
        };
        let connection = self.inner.connection.lock().await.clone();
        connection
            .respond(pending.wire_id, json!({ "outcome": outcome }))
            .await?;
        self.emit(
            "request.resolved",
            pending.turn_id,
            Some(request_id.to_owned()),
            json!({
                "requestType": "exec_command_approval",
                "decision": decision,
            }),
        )
        .await;
        Ok(())
    }

    pub async fn respond_to_user_input(
        &self,
        request_id: &str,
        answers: Value,
    ) -> Result<(), GrokRuntimeError> {
        let pending = self
            .inner
            .pending_requests
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| GrokRuntimeError::PendingRequestNotFound {
                request_id: request_id.to_owned(),
            })?;
        let answer_map = answers.as_object().cloned().unwrap_or_default();
        let normalized = answer_map
            .into_iter()
            .map(|(key, value)| {
                let values = match value {
                    Value::String(string) => vec![Value::String(string)],
                    Value::Array(array) => array,
                    _ => Vec::new(),
                };
                (key, Value::Array(values))
            })
            .collect::<serde_json::Map<String, Value>>();
        let connection = self.inner.connection.lock().await.clone();
        connection
            .respond(
                pending.wire_id,
                json!({
                    "outcome": "accepted",
                    "answers": normalized,
                }),
            )
            .await?;
        self.emit(
            "user-input.resolved",
            pending.turn_id,
            Some(request_id.to_owned()),
            json!({ "answers": answers }),
        )
        .await;
        Ok(())
    }

    pub async fn next_event(&self) -> Option<GrokRuntimeEvent> {
        self.inner.events_rx.lock().await.recv().await
    }

    pub async fn collect_events(&self, expected: usize) -> Vec<GrokRuntimeEventStableView> {
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
        connection: AcpJsonRpcConnection,
        mut incoming: mpsc::UnboundedReceiver<IncomingEvent>,
    ) {
        let runtime = self.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = incoming.recv().await {
                runtime.handle_incoming(connection.clone(), event).await;
            }
        });
        tokio::spawn({
            let inner = self.inner.clone();
            async move {
                if let Some(previous) = inner.task.lock().await.replace(task) {
                    previous.abort();
                }
            }
        });
    }

    async fn handle_incoming(&self, connection: AcpJsonRpcConnection, event: IncomingEvent) {
        match event {
            IncomingEvent::Notification { method, params } => {
                self.handle_notification(method, params).await
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
                self.emit("runtime.warning", None, None, json!({ "message": message }))
                    .await;
            }
            IncomingEvent::Closed { reason } => {
                self.emit("session.exited", None, None, json!({ "reason": reason }))
                    .await;
            }
        }
    }

    async fn handle_notification(&self, method: String, params: Value) {
        if method == "_x.ai/session/prompt_complete" {
            return;
        }
        if method != "session/update" {
            return;
        }
        let active_turn_id = self.inner.active_turn_id.lock().await.clone();
        if let Some(ignored_turn_id) = self.inner.ignore_turn_output.lock().await.clone()
            && active_turn_id.as_deref() == Some(ignored_turn_id.as_str())
        {
            return;
        }
        match params
            .get("update")
            .and_then(|value| value.get("sessionUpdate"))
            .and_then(Value::as_str)
        {
            Some("agent_message_chunk") => {
                let delta = params
                    .get("update")
                    .and_then(|value| value.get("content"))
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.emit(
                    "content.delta",
                    active_turn_id,
                    None,
                    json!({ "streamKind": "assistant_text", "delta": delta }),
                )
                .await;
            }
            Some("plan") => {
                self.emit(
                    "turn.plan.updated",
                    active_turn_id,
                    None,
                    json!({
                        "entries": params.get("update").and_then(|value| value.get("entries")).cloned().unwrap_or(Value::Array(Vec::new())),
                    }),
                )
                .await;
            }
            unhandled => {
                // The last silent drop in the ACP path: unhandled
                // `sessionUpdate` variants. Variant name only — the update body
                // carries conversation content.
                tracing::debug!(
                    provider = PROVIDER,
                    session_update = unhandled.unwrap_or("<absent>"),
                    "dropped an unhandled ACP session update"
                );
            }
        }
    }

    async fn handle_request(
        &self,
        connection: AcpJsonRpcConnection,
        correlation_id: String,
        wire_id: Value,
        method: String,
        params: Value,
    ) -> Result<(), GrokRuntimeError> {
        match method.as_str() {
            "session/request_permission" => {
                let request_id = format!("approval:{correlation_id}");
                let turn_id = self.inner.active_turn_id.lock().await.clone();
                let options = params
                    .get("options")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if self.inner.runtime_mode.lock().await.as_str() == "full-access"
                    && let Some(option_id) =
                        crate::provider::acp_mode::auto_approved_option_id(&options)
                {
                    connection
                        .respond(
                            wire_id,
                            json!({
                                "outcome": {
                                    "outcome": "selected",
                                    "optionId": option_id,
                                }
                            }),
                        )
                        .await?;
                    return Ok(());
                }
                self.inner.pending_requests.lock().await.insert(
                    request_id.clone(),
                    PendingRequest {
                        wire_id,
                        turn_id: turn_id.clone(),
                        accept_option_id: find_option_id(&options, "allow_once"),
                        accept_for_session_option_id: find_option_id(&options, "allow_always"),
                        reject_option_id: find_option_id(&options, "reject_once"),
                    },
                );
                let detail = params
                    .get("toolCall")
                    .and_then(|value| value.get("title"))
                    .and_then(Value::as_str)
                    .map(|value| value.trim_matches('`').to_owned())
                    .unwrap_or_default();
                self.emit(
                    "request.opened",
                    turn_id,
                    Some(request_id),
                    json!({ "requestType": "exec_command_approval", "detail": detail }),
                )
                .await;
                Ok(())
            }
            "_x.ai/ask_user_question" | "x.ai/ask_user_question" => {
                let request_id = format!("user-input:{correlation_id}");
                let turn_id = self.inner.active_turn_id.lock().await.clone();
                let questions = params
                    .get("params")
                    .and_then(|value| value.get("questions"))
                    .or_else(|| params.get("questions"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                self.inner.pending_requests.lock().await.insert(
                    request_id.clone(),
                    PendingRequest {
                        wire_id,
                        turn_id: turn_id.clone(),
                        accept_option_id: None,
                        accept_for_session_option_id: None,
                        reject_option_id: None,
                    },
                );
                self.emit(
                    "user-input.requested",
                    turn_id,
                    Some(request_id),
                    json!({
                        "questions": questions.into_iter().map(|question| {
                            let prompt = question.get("question").cloned().unwrap_or(Value::String(String::new()));
                            json!({
                                "id": question.get("id").cloned().unwrap_or_else(|| prompt.clone()),
                                "header": prompt,
                                "question": question.get("question").cloned().unwrap_or(Value::String(String::new())),
                                "options": question.get("options").cloned().unwrap_or(Value::Array(Vec::new())),
                            })
                        }).collect::<Vec<_>>(),
                    }),
                )
                .await;
                Ok(())
            }
            _ => {
                connection
                    .respond_error(
                        wire_id,
                        JsonRpcErrorShape {
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
        let mut counter = self.inner.event_counter.lock().await;
        *counter += 1;
        let _ = self.inner.events_tx.send(GrokRuntimeEvent {
            event_id: format!("evt-{}", *counter),
            provider: PROVIDER.to_owned(),
            created_at: FIXED_EVENT_TIME.to_owned(),
            event_type: event_type.to_owned(),
            thread_id: self.inner.options.thread_id.clone(),
            turn_id,
            item_id: None,
            request_id,
            payload,
        });
    }

    async fn provider_session_id(&self) -> Result<String, GrokRuntimeError> {
        self.inner
            .provider_session_id
            .lock()
            .await
            .clone()
            .ok_or(GrokRuntimeError::MissingProviderSessionId)
    }
}

fn find_option_id(options: &[Value], kind: &str) -> Option<String> {
    options.iter().find_map(|option| {
        (option.get("kind").and_then(Value::as_str) == Some(kind))
            .then(|| {
                option
                    .get("optionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_lookup_requires_matching_kind_and_string_identifier() {
        let options = json!([
            {"kind":"mode","optionId":42},
            {"kind":"model","optionId":"model-option"},
            {"kind":"mode","optionId":"mode-option"}
        ]);
        let options = options.as_array().unwrap();

        assert_eq!(
            find_option_id(options, "model").as_deref(),
            Some("model-option")
        );
        assert_eq!(
            find_option_id(options, "mode").as_deref(),
            Some("mode-option")
        );
        assert_eq!(find_option_id(options, "missing"), None);
    }
}
