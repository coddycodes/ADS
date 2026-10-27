use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::{
    process::{Child, Command},
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::sleep,
};
use uuid::Uuid;

use super::config::CoddyInstanceConfig;
use crate::production::provider_runtime::{
    BoxRuntimeFuture, ProviderActivityControls, ProviderDeliveryOutcome, ProviderDriver,
    ProviderEvent, ProviderLaunchRequest, ProviderRuntimeError, StartedSession,
};

const PROVIDER: &str = "coddy";
const STARTUP_DEADLINE: Duration = Duration::from_secs(20);
const FRAME_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct CoddyDriver {
    inner: Arc<Inner>,
}

struct Inner {
    thread_id: String,
    provider_label: String,
    client: Client,
    base_url: String,
    workspace_id: String,
    session_id: String,
    client_id: String,
    child: Mutex<Option<Child>>,
    owns_child: bool,
    events_tx: mpsc::UnboundedSender<ProviderEvent>,
    events_rx: Mutex<mpsc::UnboundedReceiver<ProviderEvent>>,
    pump: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
    reused_session: bool,
    state: Mutex<DriverState>,
}

#[derive(Default)]
struct DriverState {
    current_turn: Option<String>,
    completed_turns: HashSet<String>,
    messages: HashMap<String, MessageView>,
    pending_permissions: HashMap<String, Value>,
    pending_questions: HashMap<String, Value>,
    mcp: BTreeMap<String, String>,
    context_window: Option<i64>,
}

#[derive(Default)]
struct MessageView {
    text: String,
    reasoning: String,
    text_item: Option<String>,
    reasoning_item: Option<String>,
    tools: HashMap<String, ToolView>,
}

struct ToolView {
    item_type: String,
    completed: bool,
}

impl CoddyDriver {
    pub(crate) async fn spawn(
        request: ProviderLaunchRequest,
    ) -> Result<Self, ProviderRuntimeError> {
        ensure_device_agent().await?;
        let config = {
        let mut config = CoddyInstanceConfig::from_value(None, &request.binary_path);
        if let Some(endpoint) = request.endpoint.as_deref() {
            config.server_url = endpoint.trim().to_owned();
        }
        if let Some(data_dir) = request.environment.get("CODDY_CLI_DATA_DIR") {
            config.data_dir = data_dir.clone();
        }
        config
    };
        let (base_url, child, owns_child) = if !config.server_url.trim().is_empty() {
            (config.server_url.trim_end_matches('/').to_owned(), None, false)
        } else {
            let (url, child) = spawn_server(&request, &config).await?;
            (url, Some(child), true)
        };

        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| ProviderRuntimeError::Spawn {
                provider: PROVIDER.to_owned(),
                detail: format!("failed to build the Coddy HTTP client: {error}"),
            })?;

        check_version(&client, &base_url).await?;
        let workspace_id = ensure_workspace(&client, &base_url, &request.cwd).await?;
        let (session_id, reused_session) =
            ensure_session(&client, &base_url, &workspace_id, &request).await?;

        let (context_window, catalog_default) =
            model_catalog(&client, &base_url, &workspace_id, request.model.as_deref())
                .await
                .unwrap_or((None, None));
        if let Some(model) = request.model.as_deref().or(catalog_default.as_deref()) {
            set_cli_model(&client, &base_url, &workspace_id, model).await?;
        }

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        if let Some(window) = context_window {
            let mut state = DriverState::default();
            state.context_window = Some(window);
            let inner = Inner {
                thread_id: request.thread_id.clone(),
                provider_label: request.provider_label.clone(),
                client: client.clone(),
                base_url: base_url.clone(),
                workspace_id: workspace_id.clone(),
                session_id: session_id.clone(),
                client_id: Uuid::new_v4().to_string(),
                child: Mutex::new(child),
                owns_child,
                events_tx: events_tx.clone(),
                events_rx: Mutex::new(events_rx),
                pump: Mutex::new(None),
                stopped: AtomicBool::new(false),
                reused_session,
                state: Mutex::new(state),
            };
            return Ok(Self {
                inner: Arc::new(inner),
            });
        }

        let inner = Inner {
            thread_id: request.thread_id.clone(),
            provider_label: request.provider_label.clone(),
            client,
            base_url,
            workspace_id,
            session_id,
            client_id: Uuid::new_v4().to_string(),
            child: Mutex::new(child),
            owns_child,
            events_tx,
            events_rx: Mutex::new(events_rx),
            pump: Mutex::new(None),
            stopped: AtomicBool::new(false),
            reused_session,
            state: Mutex::new(DriverState::default()),
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    fn emit(
        &self,
        event_type: &str,
        turn_id: Option<String>,
        item_id: Option<String>,
        request_id: Option<String>,
        payload: Value,
    ) {
        self.inner.emit(event_type, turn_id, item_id, request_id, payload);
    }
}

impl Inner {
    fn emit(
        &self,
        event_type: &str,
        turn_id: Option<String>,
        item_id: Option<String>,
        request_id: Option<String>,
        payload: Value,
    ) {
        let _ = self.events_tx.send(ProviderEvent {
            native_event_id: None,
            event_type: event_type.to_owned(),
            thread_id: self.thread_id.clone(),
            turn_id,
            item_id,
            request_id,
            payload,
            activity: Vec::new(),
            activity_controls: ProviderActivityControls::default(),
        });
    }

    async fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn post_agent(&self, run_id: &str, text: &str) -> Result<(), ProviderRuntimeError> {
        let response = self
            .client
            .post(self.url(&format!("/v1/workspaces/{}/agent", self.workspace_id)).await)
            .json(&json!({
                "session_id": self.session_id,
                "run_id": run_id,
                "prompt": text,
            }))
            .send()
            .await
            .map_err(|error| ProviderRuntimeError::Provider {
                provider: PROVIDER.to_owned(),
                detail: format!("failed to send the turn: {error}"),
            })?;

        let status = response.status();
        if status.as_u16() == 202 {
            return Ok(());
        }
        let detail = response.text().await.unwrap_or_default();
        Err(ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("the Coddy server refused the turn ({status}): {detail}"),
        })
    }
}

impl ProviderDriver for CoddyDriver {
    fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
        Box::pin(async move {
            let inner = self.inner.clone();
            inner.emit(
                "session.started",
                None,
                None,
                None,
                json!({ "message": inner.provider_label, "resume": { "sessionId": inner.session_id } }),
            );
            inner.emit(
                "thread.started",
                None,
                None,
                None,
                json!({ "providerThreadId": inner.session_id }),
            );
            inner.emit(
                "session.state.changed",
                None,
                None,
                None,
                json!({ "state": "ready" }),
            );

            let pump_inner = inner.clone();
            let handle = tokio::spawn(async move { run_pump(pump_inner).await });
            *inner.pump.lock().await = Some(handle);

            Ok(StartedSession {
                resume_cursor: Some(json!({
                    "sessionId": inner.session_id,
                    "workspaceId": inner.workspace_id,
                })),
                runtime_payload: None,
                activity_capabilities: Default::default(),
            })
        })
    }

    fn send(
        &self,
        text: String,
        _attachments: Vec<Value>,
        _interaction_mode: String,
    ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
        Box::pin(async move {
            let turn_id = format!("coddy-{}", Uuid::new_v4());
            self.inner.post_agent(&turn_id, &text).await?;
            {
                let mut state = self.inner.state.lock().await;
                state.current_turn = Some(turn_id.clone());
            }
            self.emit(
                "turn.started",
                Some(turn_id.clone()),
                None,
                None,
                json!({ "model": Value::Null }),
            );
            Ok(Some(turn_id))
        })
    }

    fn deliver(
        &self,
        text: String,
        _attachments: Vec<Value>,
        _interaction_mode: String,
        delivery_key: String,
    ) -> BoxRuntimeFuture<'_, ProviderDeliveryOutcome> {
        Box::pin(async move {
            let turn_id = if delivery_key.trim().is_empty() {
                format!("coddy-{}", Uuid::new_v4())
            } else {
                delivery_key
            };
            match self.inner.post_agent(&turn_id, &text).await {
                Ok(()) => {
                    {
                        let mut state = self.inner.state.lock().await;
                        state.current_turn = Some(turn_id.clone());
                    }
                    self.emit(
                        "turn.started",
                        Some(turn_id.clone()),
                        None,
                        None,
                        json!({ "model": Value::Null }),
                    );
                    if self.inner.reused_session {
                        ProviderDeliveryOutcome::Accepted {
                            turn_id: Some(turn_id),
                        }
                    } else {
                        ProviderDeliveryOutcome::AcceptedInNewConversation {
                            turn_id: Some(turn_id),
                        }
                    }
                }
                Err(ProviderRuntimeError::Provider { detail, .. }) => {
                    ProviderDeliveryOutcome::Rejected { detail }
                }
                Err(error) => ProviderDeliveryOutcome::Ambiguous {
                    detail: error.to_string(),
                },
            }
        })
    }

    fn interrupt(
        &self,
        _turn_id: Option<String>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let url = self
                .inner
                .url(&format!(
                    "/v1/workspaces/{}/agent/sessions/{}/cancel",
                    self.inner.workspace_id, self.inner.session_id
                ))
                .await;
            let response = self
                .inner
                .client
                .post(url)
                .send()
                .await
                .map_err(|error| ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("failed to cancel the turn: {error}"),
                })?;
            if response.status().is_success() {
                Ok(())
            } else {
                Err(ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("the Coddy server refused the cancel ({})", response.status()),
                })
            }
        })
    }

    fn approve(
        &self,
        request_id: String,
        decision: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let permission = {
                let mut state = self.inner.state.lock().await;
                state.pending_permissions.remove(&request_id)
            };
            let Some(permission) = permission else {
                return Err(ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("unknown permission request {request_id}"),
                });
            };
            let action = match decision.as_str() {
                "accept" => "allow",
                "acceptForSession" => "allow_session",
                "decline" | "cancel" => "deny",
                other => {
                    return Err(ProviderRuntimeError::Provider {
                        provider: PROVIDER.to_owned(),
                        detail: format!("unknown approval decision {other}"),
                    })
                }
            };
            let url = self
                .inner
                .url(&format!(
                    "/v1/workspaces/{}/permissions/grant",
                    self.inner.workspace_id
                ))
                .await;
            let response = self
                .inner
                .client
                .post(url)
                .json(&json!({ "permission": permission, "action": action }))
                .send()
                .await
                .map_err(|error| ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("failed to answer the permission request: {error}"),
                })?;
            if response.status().is_success() {
                self.inner.emit(
                    "request.resolved",
                    None,
                    None,
                    Some(request_id),
                    json!({ "requestType": "unknown", "decision": decision }),
                );
                Ok(())
            } else {
                Err(ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!(
                        "the Coddy server refused the permission answer ({})",
                        response.status()
                    ),
                })
            }
        })
    }

    fn answer(
        &self,
        request_id: String,
        answers: Value,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let batch = {
                let mut state = self.inner.state.lock().await;
                state.pending_questions.remove(&request_id)
            };
            let Some(batch) = batch else {
                return Err(ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("unknown question batch {request_id}"),
                });
            };
            let responses = question_responses(&batch, &answers);
            let url = self
                .inner
                .url(&format!(
                    "/v1/workspaces/{}/questions/answer",
                    self.inner.workspace_id
                ))
                .await;
            let response = self
                .inner
                .client
                .post(url)
                .json(&json!({ "batch_request_id": request_id, "responses": responses }))
                .send()
                .await
                .map_err(|error| ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("failed to answer the question batch: {error}"),
                })?;
            if response.status().is_success() {
                self.inner.emit(
                    "user-input.resolved",
                    None,
                    None,
                    Some(request_id),
                    json!({ "answers": answers }),
                );
                Ok(())
            } else {
                Err(ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!(
                        "the Coddy server refused the question answer ({})",
                        response.status()
                    ),
                })
            }
        })
    }

    fn set_mode(&self, _mode: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn set_model(&self, model: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            let (provider, model_id) = split_model(&model);
            let url = self
                .inner
                .url(&format!(
                    "/v1/workspaces/{}/config/model",
                    self.inner.workspace_id
                ))
                .await;
            let response = self
                .inner
                .client
                .post(url)
                .json(&json!({
                    "scope": "workspace",
                    "model_type": "large",
                    "model": { "provider": provider, "model": model_id },
                }))
                .send()
                .await
                .map_err(|error| ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!("failed to switch the model: {error}"),
                })?;
            if response.status().is_success() {
                Ok(())
            } else {
                Err(ProviderRuntimeError::Provider {
                    provider: PROVIDER.to_owned(),
                    detail: format!(
                        "the Coddy server refused the model switch ({})",
                        response.status()
                    ),
                })
            }
        })
    }

    fn set_options(
        &self,
        _options: Vec<Value>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn rollback(
        &self,
        _turn_count: i64,
        _rewind_key: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async {
            Err(ProviderRuntimeError::UnsupportedCapability {
                provider: PROVIDER.to_owned(),
                capability: "conversation rewind",
            })
        })
    }

    fn next_event(&self) -> BoxRuntimeFuture<'_, Option<ProviderEvent>> {
        Box::pin(async move { self.inner.events_rx.lock().await.recv().await })
    }

    fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async move {
            self.inner.stopped.store(true, Ordering::SeqCst);
            if let Some(handle) = self.inner.pump.lock().await.take() {
                handle.abort();
            }
            if let Some(mut child) = self.inner.child.lock().await.take() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            if self.inner.owns_child {
                let url = self
                    .inner
                    .url(&format!(
                        "/v1/workspaces/{}?client_id={}",
                        self.inner.workspace_id, self.inner.client_id
                    ))
                    .await;
                let _ = self.inner.client.delete(url).send().await;
            }
            Ok(())
        })
    }

    fn owns_native_work(&self) -> bool {
        self.inner.owns_child
    }
}

/// Coddy Studio requires the Coddy Device Agent on the host that runs the
/// provider. The agent exposes an unauthenticated health endpoint; a missing
/// agent turns the refusal into one actionable message instead of a broken turn.
async fn ensure_device_agent() -> Result<(), ProviderRuntimeError> {
    let policy = crate::server_settings::policy::current();
    if !policy.device_agent.required {
        return Ok(());
    }
    let url = format!(
        "{}/api/v1/agent/health",
        policy.device_agent.url.trim_end_matches('/')
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to build the device-agent health client: {error}"),
        })?;
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => Ok(()),
        _ => Err(ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!(
                "Coddy Device Agent is required to use Coddy Studio. Install and start it so {url} answers, \
                 then retry. Advanced setups can set deviceAgent.required: false in \
                 ~/.coddy/studio/config.yaml."
            ),
        }),
    }
}

async fn spawn_server(
    request: &ProviderLaunchRequest,
    config: &CoddyInstanceConfig,
) -> Result<(String, Child), ProviderRuntimeError> {
    let port_file = std::env::temp_dir().join(format!("coddy-studio-{}.port", Uuid::new_v4()));
    let _ = tokio::fs::remove_file(&port_file).await;

    let mut command = Command::new(&config.binary_path);
    command
        .arg("cli")
        .arg("server")
        .arg("--host")
        .arg("tcp://127.0.0.1:0")
        .arg("--port-file")
        .arg(&port_file)
        .arg("--cwd")
        .arg(&request.cwd)
        .current_dir(&request.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if !config.data_dir.is_empty() {
        command.arg("-D").arg(&config.data_dir);
    }
    for (name, value) in &request.environment {
        if name == "CODDY_CLI_DATA_DIR" {
            continue;
        }
        command.env(name, value);
    }

    let child = command
        .spawn()
        .map_err(|error| ProviderRuntimeError::Spawn {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to start {}: {error}", config.binary_path),
        })?;

    let deadline = tokio::time::Instant::now() + STARTUP_DEADLINE;
    let base_url = loop {
        if tokio::time::Instant::now() > deadline {
            let _ = tokio::fs::remove_file(&port_file).await;
            return Err(ProviderRuntimeError::Spawn {
                provider: PROVIDER.to_owned(),
                detail: format!(
                    "{} did not publish its bound address within {} seconds",
                    config.binary_path,
                    STARTUP_DEADLINE.as_secs()
                ),
            });
        }
        match tokio::fs::read_to_string(&port_file).await {
            Ok(value) if !value.trim().is_empty() => break value.trim().to_owned(),
            _ => sleep(Duration::from_millis(100)).await,
        }
    };
    let _ = tokio::fs::remove_file(&port_file).await;

    let client = Client::new();
    let deadline = tokio::time::Instant::now() + STARTUP_DEADLINE;
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(ProviderRuntimeError::Provider {
                provider: PROVIDER.to_owned(),
                detail: format!("the Coddy server at {base_url} did not become healthy"),
            });
        }
        match client.get(format!("{base_url}/v1/health")).send().await {
            Ok(response) if response.status().is_success() => break,
            _ => sleep(Duration::from_millis(150)).await,
        }
    }

    Ok((base_url, child))
}

async fn check_version(client: &Client, base_url: &str) -> Result<(), ProviderRuntimeError> {
    let response = client
        .get(format!("{base_url}/v1/version"))
        .send()
        .await
        .map_err(|error| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to read the Coddy server version: {error}"),
        })?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    let version = body
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let major = version
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(1);
    if major >= 2 {
        return Err(ProviderRuntimeError::IncompatibleProvider {
            provider: PROVIDER.to_owned(),
            detail: format!(
                "Coddy CLI {version} speaks a newer server API than Coddy Studio drives; \
                 install a 1.x release or set the binary path to one"
            ),
        });
    }
    if !status.is_success() {
        return Err(ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("the Coddy server version check failed ({status})"),
        });
    }
    Ok(())
}

async fn ensure_workspace(
    client: &Client,
    base_url: &str,
    cwd: &Path,
) -> Result<String, ProviderRuntimeError> {
    let path = cwd.to_str().ok_or_else(|| ProviderRuntimeError::Provider {
        provider: PROVIDER.to_owned(),
        detail: format!("the workspace path is not valid UTF-8: {}", cwd.display()),
    })?;
    let response = client
        .post(format!("{base_url}/v1/workspaces"))
        .json(&json!({ "path": path }))
        .send()
        .await
        .map_err(|error| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to create the Coddy workspace: {error}"),
        })?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    body.get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("the Coddy server did not return a workspace id ({status})"),
        })
}

async fn ensure_session(
    client: &Client,
    base_url: &str,
    workspace_id: &str,
    request: &ProviderLaunchRequest,
) -> Result<(String, bool), ProviderRuntimeError> {
    let resume = request
        .resume_cursor
        .as_ref()
        .and_then(|cursor| cursor.get("sessionId").or_else(|| cursor.get("session_id")))
        .and_then(Value::as_str)
        .map(str::to_owned);

    if let Some(session_id) = resume {
        let response = client
            .get(format!(
                "{base_url}/v1/workspaces/{workspace_id}/sessions/{session_id}"
            ))
            .send()
            .await
            .map_err(|error| ProviderRuntimeError::Provider {
                provider: PROVIDER.to_owned(),
                detail: format!("failed to read the resumed Coddy session: {error}"),
            })?;
        if response.status().is_success() {
            return Ok((session_id, true));
        }
        if response.status().as_u16() != 404 {
            return Err(ProviderRuntimeError::Provider {
                provider: PROVIDER.to_owned(),
                detail: format!(
                    "the Coddy server could not resume session {session_id} ({})",
                    response.status()
                ),
            });
        }
    }

    let response = client
        .post(format!("{base_url}/v1/workspaces/{workspace_id}/sessions"))
        .json(&json!({ "title": "" }))
        .send()
        .await
        .map_err(|error| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to create a Coddy session: {error}"),
        })?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    body.get("id")
        .and_then(Value::as_str)
        .map(|id| (id.to_owned(), false))
        .ok_or_else(|| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("the Coddy server did not return a session id ({status})"),
        })
}

async fn model_catalog(
    client: &Client,
    base_url: &str,
    workspace_id: &str,
    selected: Option<&str>,
) -> Result<(Option<i64>, Option<String>), ProviderRuntimeError> {
    let response = client
        .get(format!("{base_url}/v1/workspaces/{workspace_id}/models"))
        .send()
        .await
        .map_err(|error| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to read the Coddy model catalog: {error}"),
        })?;
    if !response.status().is_success() {
        return Ok((None, None));
    }
    let models: Vec<Value> = response.json().await.unwrap_or_default();
    let context_window = selected
        .and_then(|model| {
            models.iter().find(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == model || id.ends_with(&format!("/{model}")))
            })
        })
        .and_then(|entry| entry.get("context_window").and_then(Value::as_i64))
        .filter(|window| *window > 0);
    // Studio's product default is the Coddy API; a launch without an explicit
    // selection still pins the CLI to the first Coddy model instead of whatever
    // a previous TUI session left configured.
    let catalog_default = models
        .iter()
        .find(|entry| {
            entry
                .get("provider_id")
                .and_then(Value::as_str)
                .is_some_and(|provider| provider == "coddy")
        })
        .and_then(|entry| entry.get("id").and_then(Value::as_str))
        .map(str::to_owned);
    Ok((context_window, catalog_default))
}

async fn set_cli_model(
    client: &Client,
    base_url: &str,
    workspace_id: &str,
    model: &str,
) -> Result<(), ProviderRuntimeError> {
    let (provider, model_id) = split_model(model);
    let response = client
        .post(format!(
            "{base_url}/v1/workspaces/{workspace_id}/config/model"
        ))
        .json(&json!({
            "scope": "workspace",
            "model_type": "large",
            "model": { "provider": provider, "model": model_id },
        }))
        .send()
        .await
        .map_err(|error| ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!("failed to select the Coddy model: {error}"),
        })?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(ProviderRuntimeError::Provider {
            provider: PROVIDER.to_owned(),
            detail: format!(
                "the Coddy server refused the model selection ({})",
                response.status()
            ),
        })
    }
}

async fn run_pump(inner: Arc<Inner>) {
    let mut attempt = 0u32;
    while !inner.stopped.load(Ordering::SeqCst) {
        let result = read_stream(&inner).await;
        if inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        if let Err(detail) = result {
            if let Some(child) = inner.child.lock().await.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    inner.emit(
                        "session.exited",
                        None,
                        None,
                        None,
                        json!({
                            "reason": format!("the Coddy server exited: {status}"),
                            "recoverable": true,
                            "exitKind": "error",
                        }),
                    );
                    return;
                }
            }
            let _ = detail;
        }
        let delay = Duration::from_millis((250u64 << attempt.min(5)).min(10_000));
        attempt = attempt.saturating_add(1);
        sleep(delay).await;
    }
}

async fn read_stream(inner: &Arc<Inner>) -> Result<(), String> {
    let url = format!(
        "{}/v1/workspaces/{}/events?client_id={}",
        inner.base_url, inner.workspace_id, inner.client_id
    );
    let response = inner
        .client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("event stream answered {}", response.status()));
    }

    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        if inner.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let chunk = chunk.map_err(|error| error.to_string())?;
        buffer.extend_from_slice(&chunk);
        while let Some(position) = find_frame_end(&buffer) {
            let frame: Vec<u8> = buffer.drain(..position + 2).collect();
            let text = String::from_utf8_lossy(&frame[..position]);
            if let Some(data) = sse_data(&text) {
                handle_frame(inner, &data).await;
            }
        }
        if buffer.len() > FRAME_LIMIT {
            return Err("event frame exceeded the 32 MiB bound".to_owned());
        }
    }
    if inner.stopped.load(Ordering::SeqCst) {
        Ok(())
    } else {
        Err("event stream ended".to_owned())
    }
}

fn find_frame_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == &b"\n\n"[..])
}

fn sse_data(frame: &str) -> Option<String> {
    let mut data = Vec::new();
    for line in frame.lines() {
        if line.starts_with(':') || line.starts_with("retry:") || line.starts_with("event:") {
            continue;
        }
        let value = line.strip_prefix("data:").unwrap_or(line);
        data.push(value.trim_start());
    }
    let joined = data.join("\n");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

async fn handle_frame(inner: &Arc<Inner>, data: &str) {
    let Ok(outer) = serde_json::from_str::<Value>(data) else {
        return;
    };
    let Some(kind) = outer.get("type").and_then(Value::as_str) else {
        return;
    };
    let envelope = outer.get("payload").unwrap_or(&Value::Null);
    let payload = envelope.get("payload").unwrap_or(envelope);

    match kind {
        "message" => {
            handle_message(inner, payload).await;
        }
        "run_complete" => handle_run_complete(inner, payload).await,
        "permission_request" => handle_permission(inner, payload).await,
        "permission_notification" => handle_permission_resolved(inner, payload).await,
        "question_batch_request" => handle_question(inner, payload).await,
        "question_batch_notification" => handle_question_resolved(inner, payload).await,
        "session" => handle_session(inner, payload).await,
        "mcp_event" => handle_mcp(inner, payload).await,
        "agent_event" => handle_agent_event(inner, payload).await,
        _ => {}
    }
}

async fn handle_message(inner: &Arc<Inner>, message: &Value) {
    let Some(message_id) = message.get("id").and_then(Value::as_str) else {
        return;
    };
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let parts = message
        .get("parts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut state = inner.state.lock().await;
    let turn_id = state.current_turn.clone();

    if role == "assistant" {
        let text = collect_text(&parts);
        let reasoning = collect_reasoning(&parts);
        let view = state
            .messages
            .entry(message_id.to_owned())
            .or_insert_with(MessageView::default);

        if !text.is_empty() {
            if let Some(item) = view.text_item.clone() {
                if text == view.text {
                } else if text.starts_with(&view.text) {
                    let delta = &text[view.text.len()..];
                    if !delta.is_empty() {
                        inner.emit(
                            "content.delta",
                            turn_id.clone(),
                            Some(item),
                            None,
                            json!({ "streamKind": "assistant_text", "delta": delta }),
                        );
                    }
                } else {
                    inner.emit(
                        "item.completed",
                        turn_id.clone(),
                        Some(item.clone()),
                        None,
                        json!({ "itemType": "assistant_message" }),
                    );
                    let new_item = format!("msg-{message_id}-text-{}", Uuid::new_v4());
                    inner.emit(
                        "item.started",
                        turn_id.clone(),
                        Some(new_item.clone()),
                        None,
                        json!({ "itemType": "assistant_message" }),
                    );
                    inner.emit(
                        "content.delta",
                        turn_id.clone(),
                        Some(new_item.clone()),
                        None,
                        json!({ "streamKind": "assistant_text", "delta": text }),
                    );
                    view.text_item = Some(new_item);
                }
                view.text = text;
            } else {
                let item = format!("msg-{message_id}-text");
                inner.emit(
                    "item.started",
                    turn_id.clone(),
                    Some(item.clone()),
                    None,
                    json!({ "itemType": "assistant_message" }),
                );
                inner.emit(
                    "content.delta",
                    turn_id.clone(),
                    Some(item.clone()),
                    None,
                    json!({ "streamKind": "assistant_text", "delta": text }),
                );
                view.text_item = Some(item);
                view.text = text;
            }
        }

        if !reasoning.is_empty() {
            if let Some(item) = view.reasoning_item.clone() {
                if reasoning.starts_with(&view.reasoning) {
                    let delta = &reasoning[view.reasoning.len()..];
                    if !delta.is_empty() {
                        inner.emit(
                            "content.delta",
                            turn_id.clone(),
                            Some(item),
                            None,
                            json!({ "streamKind": "reasoning_text", "delta": delta }),
                        );
                    }
                } else {
                    inner.emit(
                        "item.completed",
                        turn_id.clone(),
                        Some(item.clone()),
                        None,
                        json!({ "itemType": "reasoning" }),
                    );
                    let new_item = format!("msg-{message_id}-reasoning-{}", Uuid::new_v4());
                    inner.emit(
                        "item.started",
                        turn_id.clone(),
                        Some(new_item.clone()),
                        None,
                        json!({ "itemType": "reasoning" }),
                    );
                    inner.emit(
                        "content.delta",
                        turn_id.clone(),
                        Some(new_item.clone()),
                        None,
                        json!({ "streamKind": "reasoning_text", "delta": reasoning }),
                    );
                    view.reasoning_item = Some(new_item);
                }
                view.reasoning = reasoning;
            } else {
                let item = format!("msg-{message_id}-reasoning");
                inner.emit(
                    "item.started",
                    turn_id.clone(),
                    Some(item.clone()),
                    None,
                    json!({ "itemType": "reasoning" }),
                );
                inner.emit(
                    "content.delta",
                    turn_id.clone(),
                    Some(item.clone()),
                    None,
                    json!({ "streamKind": "reasoning_text", "delta": reasoning }),
                );
                view.reasoning_item = Some(item);
                view.reasoning = reasoning;
            }
        }
    }

    for part in &parts {
        if let Some(tool_call) = part.get("id").and_then(Value::as_str).filter(|_| {
            part.get("input").is_some() && part.get("name").is_some()
        }) {
            let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
            let item_type = tool_item_type(name);
            let item_id = format!("tool-{tool_call}");
            let view = state
                .messages
                .entry(message_id.to_owned())
                .or_insert_with(MessageView::default);
            let tool = view
                .tools
                .entry(tool_call.to_owned())
                .or_insert_with(|| ToolView {
                    item_type: item_type.to_owned(),
                    completed: false,
                });
            if !tool.completed {
                if part.get("finished").and_then(Value::as_bool) == Some(true) {
                    inner.emit(
                        "item.completed",
                        turn_id.clone(),
                        Some(item_id),
                        None,
                        json!({
                            "itemType": tool.item_type,
                            "status": "completed",
                            "title": name,
                            "data": { "input": part.get("input") },
                        }),
                    );
                    tool.completed = true;
                } else {
                    inner.emit(
                        "item.started",
                        turn_id.clone(),
                        Some(item_id),
                        None,
                        json!({
                            "itemType": tool.item_type,
                            "status": "inProgress",
                            "title": name,
                            "data": { "input": part.get("input") },
                        }),
                    );
                }
            }
        }

        if let Some(tool_call_id) = part.get("tool_call_id").and_then(Value::as_str) {
            let item_id = format!("tool-{tool_call_id}");
            let is_error = part
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
            let item_type = tool_item_type(name);
            let view = state
                .messages
                .entry(message_id.to_owned())
                .or_insert_with(MessageView::default);
            let tool = view
                .tools
                .entry(tool_call_id.to_owned())
                .or_insert_with(|| ToolView {
                    item_type: item_type.to_owned(),
                    completed: false,
                });
            if !tool.completed {
                inner.emit(
                    "item.completed",
                    turn_id.clone(),
                    Some(item_id),
                    None,
                    json!({
                        "itemType": tool.item_type,
                        "status": if is_error { "failed" } else { "completed" },
                        "title": name,
                        "data": {
                            "content": part.get("content"),
                            "is_error": is_error,
                        },
                    }),
                );
                tool.completed = true;
            }
        }

        if let Some(finish) = part.get("reason").and_then(Value::as_str).filter(|reason| {
            matches!(
                *reason,
                "end_turn" | "tool_use" | "canceled" | "max_tokens" | "content_filter" | "error"
            )
        }) {
            if let Some(turn) = turn_id.clone() {
                if state.completed_turns.insert(turn.clone()) {
                    let (terminal, message) = finish_state(finish);
                    let mut payload = json!({ "state": terminal });
                    if let Some(message) = message {
                        payload["errorMessage"] = json!(message);
                        payload["errorClass"] = json!("provider_error");
                    }
                    inner.emit(
                        "turn.completed",
                        Some(turn),
                        None,
                        None,
                        payload,
                    );
                    state.current_turn = None;
                }
            }
        }
    }
}

async fn handle_run_complete(inner: &Arc<Inner>, payload: &Value) {
    let run_id = payload
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let mut state = inner.state.lock().await;
    let turn_id = if run_id.is_empty() {
        state.current_turn.clone()
    } else {
        Some(run_id)
    };
    let Some(turn_id) = turn_id else {
        return;
    };
    if !state.completed_turns.insert(turn_id.clone()) {
        return;
    }
    state.current_turn = None;

    let error = payload.get("error").and_then(Value::as_str).unwrap_or("");
    let cancelled = payload
        .get("cancelled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut completion = if cancelled {
        json!({ "state": "cancelled" })
    } else if !error.is_empty() {
        json!({
            "state": "failed",
            "errorMessage": error,
            "errorClass": "provider_error",
        })
    } else {
        json!({ "state": "completed" })
    };
    if let Some(window) = state.context_window {
        completion["usage"] = json!({ "usedTokens": 0, "maxTokens": window });
    }
    inner.emit(
        "turn.completed",
        Some(turn_id),
        None,
        None,
        completion,
    );
}

async fn handle_permission(inner: &Arc<Inner>, permission: &Value) {
    let Some(id) = permission.get("id").and_then(Value::as_str) else {
        return;
    };
    let tool_name = permission
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let detail = permission
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| permission.get("path").and_then(Value::as_str))
        .unwrap_or("The provider asked for permission.");
    let request_type = permission_request_type(tool_name);

    {
        let mut state = inner.state.lock().await;
        state
            .pending_permissions
            .insert(id.to_owned(), permission.clone());
    }

    inner.emit(
        "request.opened",
        None,
        None,
        Some(id.to_owned()),
        json!({
            "requestType": request_type,
            "detail": detail,
            "args": permission,
        }),
    );
}

async fn handle_permission_resolved(inner: &Arc<Inner>, notification: &Value) {
    let tool_call_id = notification
        .get("tool_call_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let denied = notification
        .get("denied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut state = inner.state.lock().await;
    let id = state
        .pending_permissions
        .iter()
        .find(|(_, permission)| {
            permission
                .get("tool_call_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == tool_call_id)
        })
        .map(|(id, _)| id.clone());
    if let Some(id) = id {
        state.pending_permissions.remove(&id);
        inner.emit(
            "request.resolved",
            None,
            None,
            Some(id),
            json!({
                "requestType": "unknown",
                "decision": if denied { "decline" } else { "accept" },
            }),
        );
    }
}

async fn handle_question(inner: &Arc<Inner>, batch: &Value) {
    let Some(id) = batch.get("id").and_then(Value::as_str) else {
        return;
    };
    let questions: Vec<Value> = batch
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| {
            questions
                .iter()
                .map(|question| {
                    let question_type = question
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("select");
                    let options = question
                        .get("choices")
                        .and_then(Value::as_array)
                        .map(|choices| {
                            choices
                                .iter()
                                .map(|choice| {
                                    json!({
                                        "label": choice.get("label").and_then(Value::as_str).unwrap_or("Choice"),
                                        "description": choice.get("description").and_then(Value::as_str).unwrap_or(""),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_else(Vec::new);
                    json!({
                        "id": question.get("id").and_then(Value::as_str).unwrap_or(""),
                        "header": question.get("label").and_then(Value::as_str).unwrap_or(question_type),
                        "question": question.get("question").and_then(Value::as_str).unwrap_or(""),
                        "options": options,
                        "multiSelect": question_type == "multi_select",
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    {
        let mut state = inner.state.lock().await;
        state.pending_questions.insert(id.to_owned(), batch.clone());
    }

    inner.emit(
        "user-input.requested",
        None,
        None,
        Some(id.to_owned()),
        json!({ "questions": questions }),
    );
}

async fn handle_question_resolved(inner: &Arc<Inner>, notification: &Value) {
    let id = notification
        .get("batch_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut state = inner.state.lock().await;
    if state.pending_questions.remove(id).is_some() {
        inner.emit(
            "user-input.resolved",
            None,
            None,
            Some(id.to_owned()),
            json!({ "answers": { "cancelled": true } }),
        );
    }
}

async fn handle_session(inner: &Arc<Inner>, session: &Value) {
    let state = inner.state.lock().await;
    let prompt_tokens = session
        .get("prompt_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion_tokens = session
        .get("completion_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if prompt_tokens <= 0 && completion_tokens <= 0 {
        return;
    }
    let mut usage = json!({
        "usedTokens": prompt_tokens.max(0),
        "inputTokens": prompt_tokens.max(0),
        "outputTokens": completion_tokens.max(0),
        "totalProcessedTokens": prompt_tokens.max(0) + completion_tokens.max(0),
    });
    if let Some(window) = state.context_window {
        usage["maxTokens"] = json!(window);
    }
    inner.emit(
        "thread.token-usage.updated",
        None,
        None,
        None,
        json!({ "usage": usage }),
    );
}

async fn handle_mcp(inner: &Arc<Inner>, event: &Value) {
    let Some(name) = event.get("name").and_then(Value::as_str) else {
        return;
    };
    let raw_state = event
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("disconnected");
    let state = match raw_state {
        "connected" | "starting" | "needs-auth" | "disconnected" | "error" => raw_state,
        _ => "error",
    };
    let mut driver_state = inner.state.lock().await;
    driver_state
        .mcp
        .insert(name.to_owned(), state.to_owned());
    let servers: Vec<Value> = driver_state
        .mcp
        .iter()
        .map(|(name, state)| json!({ "name": name, "state": state }))
        .collect();
    inner.emit(
        "mcp.status.updated",
        None,
        None,
        None,
        json!({ "servers": servers }),
    );
}

async fn handle_agent_event(inner: &Arc<Inner>, event: &Value) {
    if event.get("type").and_then(Value::as_str) != Some("error") {
        return;
    }
    let message = event
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("The Coddy agent reported an error.");
    inner.emit(
        "runtime.error",
        None,
        None,
        None,
        json!({ "message": message, "class": "provider_error" }),
    );

    let mut state = inner.state.lock().await;
    if let Some(turn) = state.current_turn.clone() {
        if state.completed_turns.insert(turn.clone()) {
            state.current_turn = None;
            inner.emit(
                "turn.completed",
                Some(turn),
                None,
                None,
                json!({
                    "state": "failed",
                    "errorMessage": message,
                    "errorClass": "provider_error",
                }),
            );
        }
    }
}

fn question_responses(batch: &Value, answers: &Value) -> Vec<Value> {
    let Some(answers) = answers.as_object() else {
        return Vec::new();
    };
    let question_ids: Vec<String> = batch
        .get("questions")
        .and_then(Value::as_array)
        .map(|questions| {
            questions
                .iter()
                .filter_map(|question| {
                    question
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();

    answers
        .iter()
        .filter(|(id, _)| question_ids.is_empty() || question_ids.contains(*id))
        .map(|(id, value)| {
            if let Some(values) = value.as_array() {
                let selected: Vec<Value> = values
                    .iter()
                    .filter_map(|value| value.as_str().map(|value| json!(value)))
                    .collect();
                json!({ "request_id": id, "selected_ids": selected })
            } else if let Some(value) = value.as_bool() {
                json!({ "request_id": id, "yes": value })
            } else {
                json!({
                    "request_id": id,
                    "fill_in_text": value.as_str().unwrap_or(""),
                })
            }
        })
        .collect()
}

fn collect_text(parts: &[Value]) -> String {
    parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn collect_reasoning(parts: &[Value]) -> String {
    parts
        .iter()
        .filter_map(|part| part.get("thinking").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn tool_item_type(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    if name.contains("bash") || name.contains("shell") || name.starts_with("job_") {
        "command_execution"
    } else if name.contains("edit")
        || name.contains("write")
        || name.contains("patch")
        || name.contains("multiedit")
    {
        "file_change"
    } else if name.starts_with("mcp") {
        "mcp_tool_call"
    } else if name.contains("search") || name.contains("fetch") || name.contains("download") {
        "web_search"
    } else if name.contains("view") || name.contains("image") {
        "image_view"
    } else {
        "dynamic_tool_call"
    }
}

fn permission_request_type(tool_name: &str) -> &'static str {
    let tool_name = tool_name.to_ascii_lowercase();
    if tool_name.contains("bash") || tool_name.contains("shell") || tool_name.starts_with("job_") {
        "command_execution_approval"
    } else if tool_name.contains("edit") || tool_name.contains("write") || tool_name.contains("patch")
    {
        "file_change_approval"
    } else if tool_name.contains("view") || tool_name.contains("read") {
        "file_read_approval"
    } else {
        "dynamic_tool_call"
    }
}

fn finish_state(reason: &str) -> (&'static str, Option<&'static str>) {
    match reason {
        "canceled" => ("cancelled", None),
        "max_tokens" => (
            "failed",
            Some("The response reached the model's token limit."),
        ),
        "content_filter" => ("failed", Some("The provider filtered the response.")),
        "error" => ("failed", Some("The provider reported an error.")),
        _ => ("completed", None),
    }
}

fn split_model(model: &str) -> (String, String) {
    match model.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            (provider.to_owned(), model.to_owned())
        }
        _ => ("coddy".to_owned(), model.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_map_to_canonical_item_types() {
        assert_eq!(tool_item_type("bash"), "command_execution");
        assert_eq!(tool_item_type("edit"), "file_change");
        assert_eq!(tool_item_type("mcp_search"), "mcp_tool_call");
        assert_eq!(tool_item_type("web_search"), "web_search");
        assert_eq!(tool_item_type("view"), "image_view");
        assert_eq!(tool_item_type("question"), "dynamic_tool_call");
    }

    #[test]
    fn finish_reasons_map_to_terminal_states() {
        assert_eq!(finish_state("end_turn"), ("completed", None));
        assert_eq!(finish_state("canceled"), ("cancelled", None));
        assert_eq!(finish_state("max_tokens").0, "failed");
        assert_eq!(finish_state("content_filter").0, "failed");
    }

    #[test]
    fn model_ids_split_into_provider_and_model() {
        assert_eq!(
            split_model("openai/gpt-5.4"),
            ("openai".to_owned(), "gpt-5.4".to_owned())
        );
        assert_eq!(
            split_model("gpt-5.4"),
            ("coddy".to_owned(), "gpt-5.4".to_owned())
        );
    }

    #[test]
    fn sse_frames_reassemble_across_chunks() {
        let mut buffer = b"retry: 3000\n\ndata: {\"type\":\"session\"".to_vec();
        let first = find_frame_end(&buffer).unwrap();
        assert_eq!(first, 11);
        buffer.drain(..first + 2);
        buffer.extend_from_slice(b"}\n\n");
        let position = find_frame_end(&buffer).unwrap();
        assert!(sse_data(&String::from_utf8_lossy(&buffer[..position])).is_some());
    }

    #[test]
    fn question_answers_round_trip_three_shapes() {
        let batch = json!({ "questions": [{ "id": "q1" }, { "id": "q2" }, { "id": "q3" }] });
        let responses = question_responses(
            &batch,
            &json!({ "q1": ["a"], "q2": true, "q3": "typed" }),
        );

        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["selected_ids"], json!(["a"]));
        assert_eq!(responses[1]["yes"], json!(true));
        assert_eq!(responses[2]["fill_in_text"], json!("typed"));
    }
}
