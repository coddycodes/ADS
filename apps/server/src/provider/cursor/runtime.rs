#![cfg_attr(test, allow(dead_code))]

use std::{collections::HashMap, future::Future, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::provider::{OptionRefusal, RefusedOption, default_model_unavailable_refusal};

use super::{
    acp::{
        AcpJsonRpcConnection, AcpProtocolError, IncomingEvent, JsonRpcErrorShape, acp_error_class,
        acp_turn_state,
    },
    model::{
        acp_config_option_current_value, resolve_acp_config_updates_with_baseline,
        resolve_acp_default_model_config,
    },
};

const PROVIDER: &str = "cursor";
const FIXED_EVENT_TIME: &str = "2026-07-10T00:00:00.000Z";

/// Phrases that mean the saved Cursor session itself is gone. A `session/load`
/// error that does not say this can be transient, so the saved id must be kept.
const GONE_CURSOR_SESSION_SNIPPETS: &[&str] = &[
    "not found",
    "does not exist",
    "doesn't exist",
    "no such session",
    "unknown session",
    "missing session",
];

/// `true` only when Cursor reported that this session cannot be loaded again.
fn session_load_is_permanently_gone(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    let names_session = lowered.contains("session") || lowered.contains("conversation");
    names_session
        && GONE_CURSOR_SESSION_SNIPPETS
            .iter()
            .any(|snippet| lowered.contains(snippet))
}

#[derive(Clone, Debug)]
pub struct CursorSessionOptions {
    pub thread_id: String,
    pub cwd: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub model: String,
    pub resume_session_id: Option<String>,
    pub mcp_servers: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorRuntimeEvent {
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

#[derive(Debug)]
pub struct CursorPromptReceipt {
    pub turn_id: String,
    completion: oneshot::Receiver<Result<(), CursorRuntimeError>>,
}

impl CursorPromptReceipt {
    pub async fn completion(self) -> Result<(), CursorRuntimeError> {
        self.completion.await.unwrap_or_else(|_| {
            Err(CursorRuntimeError::Protocol(AcpProtocolError::Closed {
                reason: "Cursor prompt receipt task ended without an outcome".to_owned(),
            }))
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorRuntimeEventStableView {
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

impl CursorRuntimeEvent {
    #[must_use]
    pub fn stable_view(&self) -> CursorRuntimeEventStableView {
        CursorRuntimeEventStableView {
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
pub enum CursorRuntimeError {
    #[error(transparent)]
    Protocol(#[from] AcpProtocolError),
    #[error("Cursor runtime has not started a provider session")]
    MissingProviderSessionId,
    #[error("Unknown pending request id {request_id}")]
    PendingRequestNotFound { request_id: String },
    /// The session refuses a turn's options, checked against what it advertises: an option or
    /// value it does not advertise, a configuration Coddy Studio can't work with, or a switch to the
    /// default model when the session advertises none. The same request is refused again on
    /// every retry.
    #[error("Cursor option update is unsupported: {0}")]
    UnsupportedOption(RefusedOption),
    /// A config update failed and every update applied before it was rolled back. The failure may
    /// be transient, so a retry may succeed.
    #[error("Cursor option update failed and was rolled back: {application}")]
    ConfigUpdateRolledBack { application: String },
    #[error("Cursor config update acknowledgement omitted configOptions")]
    MalformedConfigAcknowledgement,
    #[error("Cursor config update failed: {application}; compensation failed: {compensation}")]
    ConfigCompensationFailed {
        application: String,
        compensation: String,
    },
    #[error("Cursor session configuration is uncertain: {detail}")]
    ConfigurationUncertain { detail: String },
}

impl OptionRefusal for CursorRuntimeError {
    fn refused_option(&self) -> Option<&RefusedOption> {
        match self {
            Self::UnsupportedOption(refused) => Some(refused),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct CursorSessionRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    options: CursorSessionOptions,
    connection: Mutex<AcpJsonRpcConnection>,
    provider_session_id: Mutex<Option<String>>,
    config_options: Mutex<Vec<Value>>,
    baseline_config_options: Mutex<Vec<Value>>,
    default_model_config: Mutex<Option<(String, Value)>>,
    configuration_uncertain: Mutex<Option<String>>,
    mode_state: Mutex<Option<Value>>,
    runtime_mode: Mutex<String>,
    interaction_mode: Mutex<String>,
    active_turn_id: Mutex<Option<String>>,
    events_tx: mpsc::UnboundedSender<CursorRuntimeEvent>,
    events_rx: Mutex<mpsc::UnboundedReceiver<CursorRuntimeEvent>>,
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
    kind: PendingRequestKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PendingRequestKind {
    Permission,
    UserInput,
    CursorQuestion,
}

impl CursorSessionRuntime {
    pub fn new(
        options: CursorSessionOptions,
        connection: AcpJsonRpcConnection,
        incoming: mpsc::UnboundedReceiver<IncomingEvent>,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let runtime_mode = options.runtime_mode.clone();
        let interaction_mode = options.interaction_mode.clone();
        let inner = Arc::new(RuntimeInner {
            options,
            connection: Mutex::new(connection.clone()),
            provider_session_id: Mutex::new(None),
            config_options: Mutex::new(Vec::new()),
            baseline_config_options: Mutex::new(Vec::new()),
            default_model_config: Mutex::new(None),
            configuration_uncertain: Mutex::new(None),
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

    pub async fn start(&self) -> Result<String, CursorRuntimeError> {
        self.emit("session.started", None, None, json!({})).await;
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
            .request("authenticate", json!({ "methodId": "cursor_login" }))
            .await?;
        let new_session = json!({
            "cwd": self.inner.options.cwd,
            "mcpServers": self.inner.options.mcp_servers,
        });
        let mut loaded_session_id = self.inner.options.resume_session_id.clone();
        let response = match loaded_session_id.clone() {
            Some(session_id) => {
                match connection
                    .request("session/load", json!({ "sessionId": session_id }))
                    .await
                {
                    Ok(response) => response,
                    // The saved session is gone, so loading it again cannot succeed. Any other
                    // provider error may be transient and must keep the saved id for the next try.
                    Err(AcpProtocolError::RemoteRequest { message, .. })
                        if session_load_is_permanently_gone(&message) =>
                    {
                        loaded_session_id = None;
                        connection.request("session/new", new_session).await?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            None => connection.request("session/new", new_session).await?,
        };
        let session_id = response
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(loaded_session_id);
        let session_id = session_id.ok_or(CursorRuntimeError::MissingProviderSessionId)?;
        *self.inner.provider_session_id.lock().await = Some(session_id.clone());
        let config_options = response
            .get("configOptions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        *self.inner.default_model_config.lock().await =
            resolve_acp_default_model_config(&Value::Array(config_options.clone())).ok();
        *self.inner.config_options.lock().await = config_options.clone();
        *self.inner.baseline_config_options.lock().await = config_options;
        *self.inner.mode_state.lock().await = response.get("modes").cloned();
        if !self.inner.options.model.trim().is_empty()
            && !self
                .inner
                .options
                .model
                .trim()
                .eq_ignore_ascii_case("default")
        {
            self.set_model(&self.inner.options.model).await?;
        }
        self.apply_mode().await?;
        self.emit("thread.started", None, None, json!({})).await;
        Ok(session_id)
    }

    pub async fn set_model(&self, model: &str) -> Result<(), CursorRuntimeError> {
        self.ensure_configuration_certain().await?;
        let (config_id, value) = if model.trim().eq_ignore_ascii_case("default") {
            self.inner
                .default_model_config
                .lock()
                .await
                .clone()
                .ok_or_else(|| {
                    CursorRuntimeError::UnsupportedOption(RefusedOption::new(
                        "Cursor session did not advertise a reversible default model selection",
                        default_model_unavailable_refusal(),
                    ))
                })?
        } else {
            let config_id = self
                .inner
                .config_options
                .lock()
                .await
                .iter()
                .find(|option| option.get("category").and_then(Value::as_str) == Some("model"))
                .and_then(|option| option.get("id").and_then(Value::as_str))
                .map(str::to_owned);
            let Some(config_id) = config_id else {
                return Ok(());
            };
            (config_id, json!(model))
        };
        let options = self
            .set_config_option(&self.provider_session_id().await?, &config_id, value)
            .await?;
        *self.inner.config_options.lock().await = options.clone();
        *self.inner.baseline_config_options.lock().await = options;
        Ok(())
    }

    pub async fn set_options(&self, options: Vec<Value>) -> Result<(), CursorRuntimeError> {
        self.ensure_configuration_certain().await?;
        let mut current = self.inner.config_options.lock().await.clone();
        let baseline = self.inner.baseline_config_options.lock().await.clone();
        let updates = resolve_acp_config_updates_with_baseline(
            &Value::Array(current.clone()),
            &Value::Array(baseline),
            &Value::Array(options),
        )
        .map_err(CursorRuntimeError::UnsupportedOption)?;
        let session_id = self.provider_session_id().await?;
        let mut applied = Vec::new();
        for update in updates {
            let config_id = update["configId"]
                .as_str()
                .expect("resolved ACP config updates have a string config id");
            let previous_value =
                acp_config_option_current_value(&Value::Array(current.clone()), config_id)
                    .map_err(CursorRuntimeError::UnsupportedOption)?;
            match self
                .set_config_option(&session_id, config_id, update["value"].clone())
                .await
            {
                Ok(next) => {
                    applied.push((config_id.to_owned(), previous_value));
                    current = next;
                    *self.inner.config_options.lock().await = current.clone();
                }
                Err(error @ CursorRuntimeError::MalformedConfigAcknowledgement) => {
                    applied.push((config_id.to_owned(), previous_value));
                    let error = self
                        .compensate_config_updates(&session_id, &mut current, applied, error)
                        .await;
                    self.latch_failed_compensation(&error).await;
                    return Err(error);
                }
                Err(error) => {
                    let error = self
                        .compensate_config_updates(&session_id, &mut current, applied, error)
                        .await;
                    self.latch_failed_compensation(&error).await;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    async fn set_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: Value,
    ) -> Result<Vec<Value>, CursorRuntimeError> {
        let connection = self.inner.connection.lock().await.clone();
        let response = connection
            .request(
                "session/set_config_option",
                json!({
                    "sessionId": session_id,
                    "configId": config_id,
                    "value": value,
                }),
            )
            .await?;
        response
            .get("configOptions")
            .and_then(Value::as_array)
            .cloned()
            .ok_or(CursorRuntimeError::MalformedConfigAcknowledgement)
    }

    async fn compensate_config_updates(
        &self,
        session_id: &str,
        current: &mut Vec<Value>,
        applied: Vec<(String, Value)>,
        application: CursorRuntimeError,
    ) -> CursorRuntimeError {
        let application = application.to_string();
        for (config_id, value) in applied.into_iter().rev() {
            match self.set_config_option(session_id, &config_id, value).await {
                Ok(next) => {
                    *current = next;
                    *self.inner.config_options.lock().await = current.clone();
                }
                Err(compensation) => {
                    return CursorRuntimeError::ConfigCompensationFailed {
                        application,
                        compensation: compensation.to_string(),
                    };
                }
            }
        }
        CursorRuntimeError::ConfigUpdateRolledBack { application }
    }

    pub async fn set_runtime_mode(&self, mode: &str) -> Result<(), CursorRuntimeError> {
        self.ensure_configuration_certain().await?;
        *self.inner.runtime_mode.lock().await = mode.to_owned();
        self.apply_mode().await
    }

    pub async fn set_interaction_mode(&self, mode: &str) -> Result<(), CursorRuntimeError> {
        self.ensure_configuration_certain().await?;
        *self.inner.interaction_mode.lock().await = mode.to_owned();
        self.apply_mode().await
    }

    async fn apply_mode(&self) -> Result<(), CursorRuntimeError> {
        self.ensure_configuration_certain().await?;
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

    pub async fn send_turn(
        &self,
        input: Option<&str>,
        attachments: Vec<Value>,
    ) -> Result<String, CursorRuntimeError> {
        let (session_id, turn_id, connection, prompt) =
            self.prepare_prompt(input, attachments).await?;
        let completion = self.track_prompt_response(turn_id.clone(), async move {
            connection
                .request(
                    "session/prompt",
                    json!({
                        "sessionId": session_id,
                        "prompt": prompt,
                    }),
                )
                .await
        });
        drop(completion);
        Ok(turn_id)
    }

    pub async fn send_turn_with_receipt(
        &self,
        input: Option<&str>,
        attachments: Vec<Value>,
    ) -> Result<CursorPromptReceipt, CursorRuntimeError> {
        let (session_id, turn_id, connection, prompt) =
            self.prepare_prompt(input, attachments).await?;
        let mut request = connection
            .start_request(
                "session/prompt",
                json!({
                    "sessionId": session_id,
                    "prompt": prompt,
                }),
            )
            .await?;
        let completion = match request.written().await {
            Ok(()) => self.track_prompt_response(turn_id.clone(), request.response()),
            Err(failure) if failure.is_definitely_not_sent() => {
                return Err(failure.into_error().into());
            }
            Err(failure) => self.track_prompt_response(
                turn_id.clone(),
                std::future::ready(Err(failure.into_error())),
            ),
        };
        Ok(CursorPromptReceipt {
            turn_id,
            completion,
        })
    }

    async fn prepare_prompt(
        &self,
        input: Option<&str>,
        attachments: Vec<Value>,
    ) -> Result<(String, String, AcpJsonRpcConnection, Vec<Value>), CursorRuntimeError> {
        self.ensure_configuration_certain().await?;
        let session_id = self.provider_session_id().await?;
        let turn_id = format!("turn-{}", Uuid::new_v4());
        *self.inner.active_turn_id.lock().await = Some(turn_id.clone());
        *self.inner.ignore_turn_output.lock().await = None;
        self.emit(
            "turn.started",
            Some(turn_id.clone()),
            None,
            json!({ "model": self.inner.options.model }),
        )
        .await;
        let connection = self.inner.connection.lock().await.clone();
        let prompt = crate::provider::attachments::prompt_parts(input, attachments);
        Ok((session_id, turn_id, connection, prompt))
    }

    async fn ensure_configuration_certain(&self) -> Result<(), CursorRuntimeError> {
        self.inner
            .configuration_uncertain
            .lock()
            .await
            .clone()
            .map_or(Ok(()), |detail| {
                Err(CursorRuntimeError::ConfigurationUncertain { detail })
            })
    }

    async fn latch_failed_compensation(&self, error: &CursorRuntimeError) {
        if matches!(error, CursorRuntimeError::ConfigCompensationFailed { .. }) {
            *self.inner.configuration_uncertain.lock().await = Some(error.to_string());
        }
    }

    fn track_prompt_response<F>(
        &self,
        background_turn_id: String,
        response: F,
    ) -> oneshot::Receiver<Result<(), CursorRuntimeError>>
    where
        F: Future<Output = Result<Value, AcpProtocolError>> + Send + 'static,
    {
        let runtime = self.clone();
        let (completion_tx, completion) = oneshot::channel();
        tokio::spawn(async move {
            let result = response.await;
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
                    let _ = completion_tx.send(Ok(()));
                }
                Err(error) => {
                    let detail = error.to_string();
                    // Attribute from the variant, not the flattened string: a
                    // JSON-RPC error is the agent's, a broken pipe is ours.
                    let class = acp_error_class(&error);
                    runtime
                        .emit(
                            "turn.completed",
                            Some(background_turn_id),
                            None,
                            json!({
                                "state": "failed",
                                "stopReason": "error",
                                "errorMessage": detail,
                                "errorClass": class,
                            }),
                        )
                        .await;
                    let _ = completion_tx.send(Err(error.into()));
                }
            }
        });
        completion
    }

    pub async fn interrupt_turn(&self) -> Result<(), CursorRuntimeError> {
        let Some(turn_id) = self.inner.active_turn_id.lock().await.clone() else {
            return Ok(());
        };
        *self.inner.ignore_turn_output.lock().await = Some(turn_id.clone());
        let pending_ids = self
            .inner
            .pending_requests
            .lock()
            .await
            .iter()
            .filter_map(|(request_id, pending)| {
                (pending.turn_id.as_deref() == Some(turn_id.as_str())).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in pending_ids {
            let _ = self.respond_to_request(&request_id, "cancel").await;
        }
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .notify(
                "session/cancel",
                json!({ "sessionId": self.provider_session_id().await? }),
            )
            .await?;
        Ok(())
    }

    pub async fn respond_to_request(
        &self,
        request_id: &str,
        decision: &str,
    ) -> Result<(), CursorRuntimeError> {
        let pending = self
            .inner
            .pending_requests
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| CursorRuntimeError::PendingRequestNotFound {
                request_id: request_id.to_owned(),
            })?;
        let result = match pending.kind {
            PendingRequestKind::Permission => {
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
                json!({ "outcome": outcome })
            }
            PendingRequestKind::UserInput | PendingRequestKind::CursorQuestion => {
                json!({ "outcome": "cancelled" })
            }
        };
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .respond(pending.wire_id, result)
            .await?;
        self.emit(
            "request.resolved",
            pending.turn_id.clone(),
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
    ) -> Result<(), CursorRuntimeError> {
        let pending = self
            .inner
            .pending_requests
            .lock()
            .await
            .remove(request_id)
            .ok_or_else(|| CursorRuntimeError::PendingRequestNotFound {
                request_id: request_id.to_owned(),
            })?;
        let response = if pending.kind == PendingRequestKind::CursorQuestion {
            json!({ "answers": answers })
        } else {
            let mut normalized = serde_json::Map::new();
            let answer_map = answers.as_object().cloned().unwrap_or_default();
            for (key, value) in answer_map {
                let values = match value {
                    Value::String(string) => vec![Value::String(string)],
                    Value::Array(array) => array,
                    _ => Vec::new(),
                };
                normalized.insert(key, Value::Array(values));
            }
            json!({
                "outcome": "accepted",
                "answers": normalized,
            })
        };
        self.inner
            .connection
            .lock()
            .await
            .clone()
            .respond(pending.wire_id, response)
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

    pub async fn next_event(&self) -> Option<CursorRuntimeEvent> {
        self.inner.events_rx.lock().await.recv().await
    }

    pub async fn collect_events(&self, expected: usize) -> Vec<CursorRuntimeEventStableView> {
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
                self.handle_notification(method, params).await;
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
        if method == "cursor/update_todos" {
            let entries = params
                .get("todos")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|todo| {
                    let step = todo
                        .get("content")
                        .or_else(|| todo.get("title"))
                        .and_then(Value::as_str)?
                        .trim();
                    if step.is_empty() {
                        return None;
                    }
                    let status = match todo.get("status").and_then(Value::as_str) {
                        Some("completed") => "completed",
                        Some("in_progress" | "inProgress") => "inProgress",
                        _ => "pending",
                    };
                    Some(json!({ "step": step, "status": status }))
                })
                .collect::<Vec<_>>();
            self.emit(
                "turn.plan.updated",
                self.inner.active_turn_id.lock().await.clone(),
                None,
                json!({ "entries": entries }),
            )
            .await;
            return;
        }
        if method != "session/update" {
            tracing::debug!(
                provider = PROVIDER,
                %method,
                "dropped an unhandled ACP notification"
            );
            return;
        }
        let turn_id = self.inner.active_turn_id.lock().await.clone();
        if let Some(ignored_turn_id) = self.inner.ignore_turn_output.lock().await.clone()
            && turn_id.as_deref() == Some(ignored_turn_id.as_str())
        {
            return;
        }
        match params
            .get("update")
            .and_then(|value| value.get("sessionUpdate"))
            .and_then(Value::as_str)
        {
            Some("tool_call") | Some("tool_call_update") => {
                let update = params.get("update").cloned().unwrap_or(Value::Null);
                let tool_call_id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call");
                let detail = update
                    .get("rawInput")
                    .and_then(|value| value.get("command"))
                    .and_then(Value::as_array)
                    .map(|command| {
                        command
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|detail| !detail.is_empty())
                    .or_else(|| {
                        update
                            .get("title")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                let status = update
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                let event_type = if status == "completed" {
                    "item.completed"
                } else {
                    "item.updated"
                };
                self.emit(
                    event_type,
                    turn_id.clone(),
                    None,
                    json!({
                        "itemType": "command_execution",
                        "itemId": tool_call_id,
                        "status": if status == "completed" { "completed" } else { "inProgress" },
                        "detail": detail,
                    }),
                )
                .await;
            }
            Some("agent_message_chunk") => {
                let delta = params
                    .get("update")
                    .and_then(|value| value.get("content"))
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.emit(
                    "content.delta",
                    turn_id,
                    None,
                    json!({
                        "streamKind": "assistant_text",
                        "delta": delta,
                    }),
                )
                .await;
            }
            Some("plan") => {
                self.emit(
                    "turn.plan.updated",
                    turn_id,
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
    ) -> Result<(), CursorRuntimeError> {
        match method.as_str() {
            "cursor/ask_question" => {
                let request_id = format!("user-input:{correlation_id}");
                let turn_id = self.inner.active_turn_id.lock().await.clone();
                let questions = params
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|question| {
                        let options = question
                            .get("options")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|option| {
                                let label = option
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default();
                                json!({ "label": label, "description": label })
                            })
                            .collect::<Vec<_>>();
                        json!({
                            "id": question.get("id").cloned().unwrap_or(Value::String(String::new())),
                            "header": "Question",
                            "question": question.get("prompt").cloned().unwrap_or(Value::String(String::new())),
                            "multiSelect": question.get("allowMultiple").and_then(Value::as_bool).unwrap_or(false),
                            "options": if options.is_empty() {
                                vec![json!({ "label": "OK", "description": "Continue" })]
                            } else {
                                options
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                self.inner.pending_requests.lock().await.insert(
                    request_id.clone(),
                    PendingRequest {
                        wire_id,
                        turn_id: turn_id.clone(),
                        accept_option_id: None,
                        accept_for_session_option_id: None,
                        reject_option_id: None,
                        kind: PendingRequestKind::CursorQuestion,
                    },
                );
                self.emit(
                    "user-input.requested",
                    turn_id,
                    Some(request_id),
                    json!({ "questions": questions }),
                )
                .await;
                Ok(())
            }
            "cursor/create_plan" => {
                let turn_id = self.inner.active_turn_id.lock().await.clone();
                let plan = params
                    .get("plan")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("# Plan\n\n(Cursor did not supply plan text.)");
                self.emit(
                    "turn.proposed.completed",
                    turn_id,
                    None,
                    json!({ "planMarkdown": plan }),
                )
                .await;
                connection
                    .respond(wire_id, json!({ "accepted": true }))
                    .await?;
                Ok(())
            }
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
                        kind: PendingRequestKind::Permission,
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
                    json!({
                        "requestType": "exec_command_approval",
                        "detail": detail,
                    }),
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
                        kind: PendingRequestKind::UserInput,
                    },
                );
                self.emit(
                    "user-input.requested",
                    turn_id,
                    Some(request_id),
                    json!({
                        "questions": questions.into_iter().map(|question| {
                            json!({
                                "id": question.get("id").cloned().unwrap_or_else(|| question.get("question").cloned().unwrap_or(Value::String(String::new()))),
                                "header": question.get("question").cloned().unwrap_or(Value::String(String::new())),
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
        let _ = self.inner.events_tx.send(CursorRuntimeEvent {
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

    async fn provider_session_id(&self) -> Result<String, CursorRuntimeError> {
        self.inner
            .provider_session_id
            .lock()
            .await
            .clone()
            .ok_or(CursorRuntimeError::MissingProviderSessionId)
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
    use crate::provider::cursor::{AcpConnectionConfig, AcpJsonRpcConnection};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, duplex};
    use tokio::time::{Duration, timeout};

    fn delivery_runtime(
        provider_session_id: Option<&str>,
    ) -> (
        CursorSessionRuntime,
        Lines<BufReader<DuplexStream>>,
        DuplexStream,
    ) {
        let (stdout, stdout_peer) = duplex(4096);
        let (stdin_peer, stdin) = duplex(4096);
        let (stderr, _stderr_peer) = duplex(4096);
        let (connection, incoming) =
            AcpJsonRpcConnection::spawn(stdout, stdin, stderr, AcpConnectionConfig::default());
        let runtime = CursorSessionRuntime::new(
            CursorSessionOptions {
                thread_id: "cursor-delivery-thread".to_owned(),
                cwd: "/workspace".to_owned(),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                model: "default".to_owned(),
                resume_session_id: None,
                mcp_servers: Vec::new(),
            },
            connection,
            incoming,
        );
        if let Some(provider_session_id) = provider_session_id {
            runtime
                .inner
                .provider_session_id
                .try_lock()
                .expect("fresh provider session lock")
                .replace(provider_session_id.to_owned());
        }
        (runtime, BufReader::new(stdin_peer).lines(), stdout_peer)
    }

    #[tokio::test]
    async fn delivery_without_a_provider_session_fails_before_writing_a_request() {
        let (runtime, mut requests, _responses) = delivery_runtime(None);

        let error = runtime
            .send_turn_with_receipt(Some("hello"), Vec::new())
            .await
            .expect_err("a missing session must fail before request creation");

        assert!(matches!(
            error,
            CursorRuntimeError::MissingProviderSessionId
        ));
        assert!(
            timeout(Duration::from_millis(100), requests.next_line())
                .await
                .is_err(),
            "a definite pre-write failure must emit no provider bytes"
        );
    }

    #[tokio::test]
    async fn delivery_receipt_accepts_only_after_the_session_prompt_response() {
        let (runtime, mut requests, mut responses) = delivery_runtime(Some("cursor-session"));
        let receipt = runtime
            .send_turn_with_receipt(Some("hello"), Vec::new())
            .await
            .expect("session prompt should write");
        let request: Value = serde_json::from_str(
            &requests
                .next_line()
                .await
                .expect("request read")
                .expect("session prompt request"),
        )
        .expect("valid JSON-RPC request");
        assert_eq!(request["method"], "session/prompt");
        let mut completion = Box::pin(receipt.completion());
        assert!(
            timeout(Duration::from_millis(100), completion.as_mut())
                .await
                .is_err(),
            "request write alone must not accept delivery"
        );
        responses
            .write_all(
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"stopReason\":\"end_turn\"}}}}\n",
                    request["id"]
                )
                .as_bytes(),
            )
            .await
            .expect("prompt response write");
        assert!(
            timeout(Duration::from_secs(2), completion)
                .await
                .expect("prompt response should resolve receipt")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn delivery_receipt_reports_disconnect_after_request_write() {
        let (runtime, mut requests, responses) = delivery_runtime(Some("cursor-session"));
        let receipt = runtime
            .send_turn_with_receipt(Some("hello"), Vec::new())
            .await
            .expect("session prompt should write");
        let request = requests
            .next_line()
            .await
            .expect("request read")
            .expect("session prompt request");
        assert!(request.contains("\"method\":\"session/prompt\""));

        drop(responses);

        assert!(
            timeout(Duration::from_secs(2), receipt.completion())
                .await
                .expect("connection loss should resolve receipt")
                .is_err()
        );
    }

    #[test]
    fn only_a_missing_session_load_error_abandons_the_saved_session() {
        assert!(session_load_is_permanently_gone("Session not found"));
        assert!(session_load_is_permanently_gone(
            "The conversation does not exist"
        ));
        assert!(!session_load_is_permanently_gone("Internal error"));
        assert!(!session_load_is_permanently_gone("rate limit exceeded"));
        assert!(!session_load_is_permanently_gone(
            "Unknown error while loading session"
        ));
        assert!(!session_load_is_permanently_gone("model not found"));
    }
}
