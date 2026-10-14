use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::Deserialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    RpcRegistry, RpcRequest, RpcResult, RpcStreamChunk,
    crypto::sha256_hex,
    mcp::preview_automation::{
        PreviewAutomationBroker, PreviewAutomationHost, PreviewAutomationOperation,
        PreviewAutomationResponse, PreviewAutomationStreamEvent,
    },
    preview::{
        PreviewError, PreviewManager, PreviewNavStatus, PreviewViewportSetting,
        gateway::registry::{GatewayError, PreviewGateway},
        tunnel::{PreviewTunnels, TunnelError},
    },
    rpc::RpcSessionContext,
    workspace::WorkspaceRpc,
};

const STREAM_CAPACITY: usize = 8;
const MAX_AUTOMATION_CLIENTS: usize = 64;
const WORKSPACE_METHODS: &[&str] = &[
    "assets.createUrl",
    "filesystem.browse",
    "projects.createDownloadUrl",
    "projects.createEntry",
    "projects.createUploadUrl",
    "projects.deleteEntry",
    "projects.duplicateEntry",
    "projects.listEntries",
    "projects.readFile",
    "projects.renameEntry",
    "projects.searchEntries",
    "projects.writeFile",
    "review.getDiffPreview",
];
const PREVIEW_METHODS: &[&str] = &[
    "preview.claimOpenRequest",
    "preview.close",
    "preview.list",
    "preview.navigate",
    "preview.open",
    "preview.refresh",
    "preview.reportStatus",
    "preview.resize",
    "previewAutomation.focusHost",
    "previewAutomation.respond",
];

#[derive(Clone)]
pub struct WorkspacePreviewRpcServices {
    workspace: WorkspaceRpc,
    preview: PreviewManager,
    automation: PreviewAutomationBroker,
    automation_state: Arc<Mutex<AutomationRpcState>>,
    gateway: Option<PreviewGateway>,
    tunnels: Option<PreviewTunnels>,
}

#[derive(Default)]
struct AutomationRpcState {
    hosts: HashMap<String, RegisteredAutomationHost>,
    known_client_ids: HashSet<String>,
    pending: HashMap<String, PendingAutomationResponse>,
}

struct RegisteredAutomationHost {
    environment_id: String,
    connection_id: String,
}

struct PendingAutomationResponse {
    client_id: String,
    connection_id: String,
}

impl WorkspacePreviewRpcServices {
    #[must_use]
    pub fn new(
        workspace: WorkspaceRpc,
        preview: PreviewManager,
        automation: PreviewAutomationBroker,
    ) -> Self {
        Self {
            workspace,
            preview,
            automation,
            automation_state: Arc::new(Mutex::new(AutomationRpcState::default())),
            gateway: None,
            tunnels: None,
        }
    }

    /// Serves `preview.gatewayOpen` from `gateway`; without one the method is not registered.
    #[must_use]
    pub fn with_gateway(mut self, gateway: PreviewGateway) -> Self {
        self.gateway = Some(gateway);
        self
    }

    /// Serves `preview.tunnelOpen`, `preview.tunnelWrite` and `preview.tunnelClose` from
    /// `tunnels`; without them the methods are not registered.
    #[must_use]
    pub fn with_tunnels(mut self, tunnels: PreviewTunnels) -> Self {
        self.tunnels = Some(tunnels);
        self
    }
}

pub fn register_workspace_preview_rpc(
    registry: &mut RpcRegistry,
    services: WorkspacePreviewRpcServices,
) {
    for method in WORKSPACE_METHODS {
        let services = services.clone();
        registry.register_unary(*method, move |request, cancellation| {
            let services = services.clone();
            async move { services.handle_workspace(request, cancellation).await }
        });
    }
    for method in PREVIEW_METHODS {
        let services = services.clone();
        registry.register_unary(*method, move |request, cancellation| {
            let services = services.clone();
            async move { services.handle_preview(request, cancellation).await }
        });
    }
    if let Some(gateway) = services.gateway.clone() {
        registry.register_unary_with_context(
            "preview.gatewayOpen",
            move |request, context, _cancellation| gateway_open(gateway.clone(), request, context),
        );
    }
    if let Some(tunnels) = services.tunnels.clone() {
        register_tunnel_rpc(registry, tunnels);
    }
    let preview = services.preview.clone();
    let workspace = services.workspace.clone();
    registry.register_stream("previewAutomation.connect", move |request, cancellation| {
        services.automation_connect(request, cancellation)
    });

    registry.register_stream("subscribePreviewEvents", move |_request, cancellation| {
        preview_event_stream(preview.clone(), cancellation)
    });

    registry.register_stream("subscribeProjectEntries", move |request, cancellation| {
        project_entries_stream(workspace.clone(), request, cancellation)
    });
}

/// Streams a signal whenever the workspace path set under `cwd` changes outside the application.
///
/// The signal carries only `cwd`: the server's entry index stays the single source of truth, so the
/// subscriber re-lists rather than applying a diff. The index is already invalidated by the time the
/// signal arrives, so that re-list sees the new state.
fn project_entries_stream(
    workspace: WorkspaceRpc,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> mpsc::Receiver<RpcStreamChunk> {
    let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
    tokio::spawn(async move {
        let input: SubscribeEntriesInput = match serde_json::from_value(request.payload) {
            Ok(input) => input,
            Err(error) => {
                let _ = sender
                    .send(Err(json!({
                        "_tag": "Defect",
                        "message": error.to_string(),
                    })))
                    .await;
                return;
            }
        };
        let mut changes = match workspace
            .subscribe_entry_changes(std::path::Path::new(&input.cwd))
            .await
        {
            Ok(changes) => changes,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        };
        // The sweep must not outlive the availability guard: when the workspace is fenced for
        // removal this token fires, and dropping the subscription releases the lease that removal
        // is waiting on.
        let loss = changes.loss_cancellation();
        let loss_token = loss
            .as_ref()
            .map(|loss| loss.cancellation_token())
            .unwrap_or_default();
        let event = json!({ "cwd": input.cwd });
        if sender.send(Ok(vec![event.clone()])).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                () = loss_token.cancelled() => break,
                change = changes.recv() => match change {
                    Ok(()) => {
                        if sender.send(Ok(vec![event.clone()])).await.is_err() {
                            break;
                        }
                    }
                    // Lagging is not a loss here: the signal is idempotent, so one delivery after
                    // any number of skipped ones still tells the subscriber to re-list.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if sender.send(Ok(vec![event.clone()])).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
    });
    receiver
}

#[derive(Deserialize)]
struct SubscribeEntriesInput {
    cwd: String,
}

fn preview_event_stream(
    preview: PreviewManager,
    cancellation: CancellationToken,
) -> mpsc::Receiver<RpcStreamChunk> {
    let mut events = preview.subscribe_events();
    let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                event = events.recv() => match event {
                    Ok(event) => {
                        let value = match serde_json::to_value(event) {
                            Ok(mut value) => {
                                normalize_camel_case_fields(&mut value);
                                value
                            }
                            Err(error) => {
                                let _ = sender.send(Err(json!({
                                    "_tag": "PreviewEventEncodeError",
                                    "message": error.to_string(),
                                }))).await;
                                break;
                            }
                        };
                        if sender.send(Ok(vec![value])).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        let _ = sender.send(Err(json!({
                            "_tag": "PreviewEventLaggedError",
                            "skipped": skipped,
                        }))).await;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    });
    receiver
}

impl WorkspacePreviewRpcServices {
    async fn handle_workspace(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        let method = request.tag;
        let payload = request.payload;
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(interrupted_error(&method)),
            result = self.workspace.handle_with_cancellation(&method, payload, cancellation.clone()) => {
                if method == "review.getDiffPreview" {
                    result.and_then(normalize_review_result)
                } else {
                    result
                }
            }
        }
    }

    async fn handle_preview(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        let tag = request.tag.clone();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(interrupted_error(&tag)),
            result = self.dispatch_preview(&tag, request.payload) => result,
        }
    }

    async fn dispatch_preview(&self, method: &str, payload: Value) -> RpcResult {
        match method {
            "preview.open" => {
                let input: PreviewOpenInput = decode(payload, method)?;
                encode(
                    self.preview
                        .open(&input.thread_id, input.url.as_deref())
                        .await
                        .map_err(preview_error)?,
                    method,
                )
            }
            "preview.navigate" => {
                let input: PreviewNavigateInput = decode(payload, method)?;
                encode(
                    self.preview
                        .navigate(
                            &input.thread_id,
                            &input.tab_id,
                            &input.url,
                            input.resolved_title.as_deref(),
                        )
                        .await
                        .map_err(preview_error)?,
                    method,
                )
            }
            "preview.resize" => {
                let input: PreviewResizeInput = decode(payload, method)?;
                let viewport = input
                    .viewport
                    .try_into()
                    .map_err(|message| invalid_request(method, message))?;
                encode(
                    self.preview
                        .resize(&input.thread_id, &input.tab_id, viewport)
                        .await
                        .map_err(preview_error)?,
                    method,
                )
            }
            "preview.refresh" => {
                let input: PreviewTabInput = decode(payload, method)?;
                self.preview
                    .refresh(&input.thread_id, &input.tab_id)
                    .await
                    .map_err(preview_error)?;
                Ok(Value::Null)
            }
            "preview.close" => {
                let input: PreviewCloseInput = decode(payload, method)?;
                self.preview
                    .close(&input.thread_id, input.tab_id.as_deref())
                    .await
                    .map_err(preview_error)?;
                Ok(Value::Null)
            }
            "preview.claimOpenRequest" => {
                let input: PreviewClaimOpenRequestInput = decode(payload, method)?;
                let claimed = self.preview.claim_open_request(&input.request_id).await;
                Ok(json!({ "claimed": claimed }))
            }
            "preview.list" => {
                let input: PreviewThreadInput = decode(payload, method)?;
                encode(self.preview.list(&input.thread_id).await, method)
            }
            "preview.reportStatus" => {
                let input: PreviewReportStatusInput = decode(payload, method)?;
                self.preview
                    .report_status(
                        &input.thread_id,
                        &input.tab_id,
                        input.nav_status,
                        input.can_go_back,
                        input.can_go_forward,
                    )
                    .await
                    .map_err(preview_error)?;
                Ok(Value::Null)
            }
            "previewAutomation.focusHost" => {
                let input: PreviewAutomationFocusInput = decode(payload, method)?;
                let matches_connection = self
                    .automation_state
                    .lock()
                    .await
                    .hosts
                    .get(&input.client_id)
                    .is_some_and(|host| {
                        host.environment_id == input.environment_id
                            && host.connection_id == input.connection_id
                    });
                if matches_connection && input.focused {
                    self.automation
                        .focus_host(&input.client_id, &input.environment_id)
                        .await
                        .map_err(automation_error)?;
                }
                Ok(Value::Null)
            }
            "previewAutomation.respond" => {
                let input: PreviewAutomationResponseInput = decode(payload, method)?;
                let valid_response = {
                    let mut state = self.automation_state.lock().await;
                    let valid = state.pending.get(&input.request_id).is_some_and(|pending| {
                        pending.client_id == input.client_id
                            && pending.connection_id == input.connection_id
                    });
                    if valid {
                        state.pending.remove(&input.request_id);
                    }
                    valid
                };
                if !valid_response {
                    return Ok(Value::Null);
                }
                self.automation
                    .respond(PreviewAutomationResponse {
                        client_id: input.client_id,
                        connection_id: input.connection_id,
                        request_id: input.request_id,
                        ok: input.ok,
                        result: input.result,
                        error: input.error,
                    })
                    .await
                    .map_err(automation_error)?;
                Ok(Value::Null)
            }
            _ => Err(invalid_request(method, "unsupported method")),
        }
    }

    fn automation_connect(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
        let input = decode::<PreviewAutomationHostInput>(request.payload, &request.tag);
        let automation = self.automation.clone();
        let automation_state = Arc::clone(&self.automation_state);
        tokio::spawn(async move {
            let input = match input {
                Ok(input) => input,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let supported_operations = input
                .supported_operations
                .unwrap_or_else(default_automation_operations)
                .into_iter()
                .filter_map(|operation| PreviewAutomationOperation::from_wire(&operation))
                .collect();
            let client_id = input.client_id;
            let environment_id = input.environment_id;
            let capacity_reached = {
                let mut state = automation_state.lock().await;
                let reached = !state.known_client_ids.contains(&client_id)
                    && state.known_client_ids.len() >= MAX_AUTOMATION_CLIENTS;
                if !reached {
                    state.known_client_ids.insert(client_id.clone());
                }
                reached
            };
            if capacity_reached {
                let _ = sender
                    .send(Err(json!({
                        "_tag": "PreviewAutomationNoAvailableHostError",
                        "message": "Preview automation host capacity was reached.",
                    })))
                    .await;
                return;
            }
            let mut events = automation.connect(PreviewAutomationHost {
                client_id: client_id.clone(),
                environment_id: environment_id.clone(),
                supported_operations,
            });
            let mut registered_connection_id = None;
            loop {
                let event = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => break,
                    event = events.recv() => event,
                };
                let Some(event) = event else { break };
                match &event {
                    PreviewAutomationStreamEvent::Connected { connection_id } => {
                        registered_connection_id = Some(connection_id.clone());
                        automation_state.lock().await.hosts.insert(
                            client_id.clone(),
                            RegisteredAutomationHost {
                                environment_id: environment_id.clone(),
                                connection_id: connection_id.clone(),
                            },
                        );
                    }
                    PreviewAutomationStreamEvent::Request {
                        connection_id,
                        request,
                    } => {
                        automation_state.lock().await.pending.insert(
                            request.request_id.clone(),
                            PendingAutomationResponse {
                                client_id: client_id.clone(),
                                connection_id: connection_id.clone(),
                            },
                        );
                    }
                }
                let value = automation_event(event);
                if sender.send(Ok(vec![value])).await.is_err() {
                    break;
                }
            }
            let mut state = automation_state.lock().await;
            if state.hosts.get(&client_id).is_some_and(|host| {
                registered_connection_id
                    .as_ref()
                    .is_some_and(|connection_id| host.connection_id == *connection_id)
            }) {
                state.hosts.remove(&client_id);
            }
            state.pending.retain(|_, pending| {
                pending.client_id != client_id
                    || registered_connection_id
                        .as_ref()
                        .is_some_and(|connection_id| pending.connection_id != *connection_id)
            });
            drop(state);
            if let Some(connection_id) = registered_connection_id {
                automation.disconnect(&client_id, &connection_id);
            }
        });
        receiver
    }
}

fn decode<T: for<'de> Deserialize<'de>>(payload: Value, method: &str) -> Result<T, Value> {
    serde_json::from_value(payload).map_err(|error| invalid_request(method, error.to_string()))
}

fn encode<T: serde::Serialize>(value: T, method: &str) -> RpcResult {
    let mut value =
        serde_json::to_value(value).map_err(|error| invalid_request(method, error.to_string()))?;
    normalize_camel_case_fields(&mut value);
    Ok(value)
}

fn normalize_camel_case_fields(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_camel_case_fields(value);
            }
        }
        Value::Object(object) => {
            if let Some(preset_id) = object.remove("preset_id") {
                object.insert("presetId".to_owned(), preset_id);
            }
            for value in object.values_mut() {
                normalize_camel_case_fields(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn invalid_request(method: &str, message: impl Into<String>) -> Value {
    json!({
        "_tag": "InvalidRequest",
        "method": method,
        "message": message.into(),
    })
}

fn interrupted_error(method: &str) -> Value {
    json!({
        "_tag": "Interrupted",
        "method": method,
        "message": "RPC request was interrupted.",
    })
}

fn preview_error(error: PreviewError) -> Value {
    match error {
        PreviewError::SessionLookup { thread_id, tab_id } => {
            let message = format!("Unknown preview session: thread={thread_id}, tab={tab_id}");
            json!({
                "_tag": "PreviewSessionLookupError",
                "threadId": thread_id,
                "tabId": tab_id,
                "message": message,
            })
        }
        PreviewError::InvalidUrl {
            input_length,
            reason,
            protocol,
        } => {
            let message = format!("Invalid preview URL ({reason}; input length {input_length}).");
            let mut value = json!({
                "_tag": "PreviewInvalidUrlError",
                "inputLength": input_length,
                "reason": reason,
                "cause": message,
                "message": message,
            });
            if let Some(protocol) = protocol {
                value
                    .as_object_mut()
                    .expect("preview error object")
                    .insert("protocol".to_owned(), json!(protocol));
            }
            value
        }
    }
}

fn automation_error(error: crate::mcp::preview_automation::PreviewAutomationError) -> Value {
    json!({
        "_tag": error.tag(),
        "message": error.message(),
    })
}

fn automation_event(event: PreviewAutomationStreamEvent) -> Value {
    match event {
        PreviewAutomationStreamEvent::Connected { connection_id } => json!({
            "type": "connected",
            "connectionId": connection_id,
        }),
        PreviewAutomationStreamEvent::Request {
            connection_id,
            request,
        } => {
            let mut request_value = json!({
                "requestId": request.request_id,
                "threadId": request.thread_id,
                "tabIdExplicit": request.tab_id_explicit,
                "operation": request.operation.as_str(),
                "input": request.input,
                "timeoutMs": request.timeout_ms,
            });
            if let Some(tab_id) = request.tab_id {
                request_value
                    .as_object_mut()
                    .expect("automation request object")
                    .insert("tabId".to_owned(), json!(tab_id));
            }
            json!({
                "type": "request",
                "connectionId": connection_id,
                "request": request_value,
            })
        }
    }
}

fn default_automation_operations() -> Vec<String> {
    [
        "status",
        "open",
        "navigate",
        "snapshot",
        "click",
        "type",
        "press",
        "scroll",
        "evaluate",
        "waitFor",
        "recordingStart",
        "recordingStop",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn normalize_review_result(mut value: Value) -> RpcResult {
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid_request("review.getDiffPreview", "invalid review result"))?;
    let generated_at = object
        .remove("generatedAt")
        .and_then(|value| value.as_u64())
        .unwrap_or_default();
    let timestamp = OffsetDateTime::from_unix_timestamp_nanos(i128::from(generated_at) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    object.insert("generatedAt".to_owned(), json!(timestamp));
    let sources = object
        .get_mut("sources")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid_request("review.getDiffPreview", "invalid review sources"))?;
    for (index, source) in sources.iter_mut().enumerate() {
        if source.get("kind").is_some() {
            continue;
        }
        let path = source
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("working-tree")
            .to_owned();
        let diff = source
            .get("diff")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let hash = sha256_hex(diff.as_bytes());
        *source = json!({
            "id": format!("source-{index}-{path}"),
            "kind": "working-tree",
            "title": path,
            "baseRef": null,
            "headRef": null,
            "diff": diff,
            "diffHash": hash,
            "truncated": false,
        });
    }
    Ok(value)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewOpenInput {
    thread_id: String,
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewNavigateInput {
    thread_id: String,
    tab_id: String,
    url: String,
    resolved_title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewTabInput {
    thread_id: String,
    tab_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewThreadInput {
    thread_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewGatewayOpenInput {
    thread_id: String,
    url: String,
}

/// Opens a gateway target for the caller's own session, which the capability is bound to,
/// on the address the caller's connection reached the server on.
async fn gateway_open(
    gateway: PreviewGateway,
    request: RpcRequest,
    context: RpcSessionContext,
) -> RpcResult {
    let input: PreviewGatewayOpenInput = decode(request.payload, "preview.gatewayOpen")?;
    let Some(principal) = context.principal() else {
        return Err(gateway_error(&GatewayError::Unavailable(
            "the preview gateway needs an authenticated session".to_owned(),
        )));
    };
    let opened = gateway
        .open(
            &input.thread_id,
            &input.url,
            &principal.session_id,
            principal.expires_at_ms,
            context.reach(),
        )
        .await
        .map_err(|error| gateway_error(&error))?;
    let mut result = json!({
        "gatewayPort": opened.gateway_port,
        "capability": opened.capability,
        "expiresAtMs": opened.expires_at_ms,
    });
    if let Some(origin) = opened.origin {
        result["origin"] = json!(origin);
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewTunnelOpenInput {
    port: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewTunnelWriteInput {
    tunnel_id: String,
    data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewTunnelCloseInput {
    tunnel_id: String,
}

/// The preview tunnel methods (#69). Each is bound to the caller's own BiBCode session: a
/// tunnel is written to and closed only by the session that opened it.
fn register_tunnel_rpc(registry: &mut RpcRegistry, tunnels: PreviewTunnels) {
    let open = tunnels.clone();
    registry.register_stream_with_context(
        "preview.tunnelOpen",
        move |request, context, cancellation| {
            let input = decode::<PreviewTunnelOpenInput>(request.payload, "preview.tunnelOpen");
            match (input, context.principal()) {
                (Ok(input), Some(principal)) => {
                    open.open(input.port, principal.session_id.clone(), cancellation)
                }
                (Err(error), _) => failed_stream(error),
                (_, None) => failed_stream(TunnelError::Revoked.to_value()),
            }
        },
    );
    let write = tunnels.clone();
    registry.register_unary_with_context(
        "preview.tunnelWrite",
        move |request, context, _cancellation| {
            let tunnels = write.clone();
            async move {
                let input: PreviewTunnelWriteInput =
                    decode(request.payload, "preview.tunnelWrite")?;
                let principal = context
                    .principal()
                    .ok_or_else(|| TunnelError::Revoked.to_value())?;
                tunnels
                    .write(&input.tunnel_id, &principal.session_id, &input.data)
                    .await
                    .map_err(TunnelError::to_value)?;
                Ok(Value::Null)
            }
        },
    );
    registry.register_unary_with_context(
        "preview.tunnelClose",
        move |request, context, _cancellation| {
            let tunnels = tunnels.clone();
            async move {
                let input: PreviewTunnelCloseInput =
                    decode(request.payload, "preview.tunnelClose")?;
                if let Some(principal) = context.principal() {
                    tunnels.close(&input.tunnel_id, &principal.session_id);
                }
                Ok(Value::Null)
            }
        },
    );
}

/// A stream that fails at once with `error`.
fn failed_stream(error: Value) -> mpsc::Receiver<RpcStreamChunk> {
    let (sender, receiver) = mpsc::channel(1);
    let _ = sender.try_send(Err(error));
    receiver
}

fn gateway_error(error: &GatewayError) -> Value {
    let reason = match error {
        GatewayError::NotAdmitted => "not-admitted",
        GatewayError::HttpsUnsupported => "https-unsupported",
        GatewayError::NoUpstream => "no-upstream",
        GatewayError::NotReachable(_) => "not-reachable",
        GatewayError::RelayNotProvisioned => "relay-not-provisioned",
        GatewayError::RelayBusy => "relay-busy",
        GatewayError::Unavailable(_) => "unavailable",
    };
    json!({
        "_tag": "PreviewGatewayError",
        "reason": reason,
        "message": error.to_string(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewClaimOpenRequestInput {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewCloseInput {
    thread_id: String,
    tab_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewResizeInput {
    thread_id: String,
    tab_id: String,
    viewport: WireViewport,
}

#[derive(Deserialize)]
#[serde(tag = "_tag")]
enum WireViewport {
    #[serde(rename = "fill")]
    Fill,
    #[serde(rename = "freeform")]
    Freeform { width: u32, height: u32 },
    #[serde(rename = "preset", rename_all = "camelCase")]
    Preset {
        preset_id: String,
        width: u32,
        height: u32,
    },
}

impl TryFrom<WireViewport> for PreviewViewportSetting {
    type Error = &'static str;

    fn try_from(value: WireViewport) -> Result<Self, Self::Error> {
        let dimensions = match &value {
            WireViewport::Fill => None,
            WireViewport::Freeform { width, height }
            | WireViewport::Preset { width, height, .. } => Some((*width, *height)),
        };
        if let Some((width, height)) = dimensions
            && (!(240..=3840).contains(&width)
                || !(240..=3840).contains(&height)
                || u64::from(width) * u64::from(height) > 3840 * 2160)
        {
            return Err("preview viewport dimensions are out of bounds");
        }
        Ok(match value {
            WireViewport::Fill => Self::Fill,
            WireViewport::Freeform { width, height } => Self::Freeform { width, height },
            WireViewport::Preset {
                preset_id,
                width,
                height,
            } => Self::Preset {
                preset_id,
                width,
                height,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewReportStatusInput {
    thread_id: String,
    tab_id: String,
    nav_status: PreviewNavStatus,
    can_go_back: bool,
    can_go_forward: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewAutomationHostInput {
    client_id: String,
    environment_id: String,
    supported_operations: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewAutomationFocusInput {
    client_id: String,
    environment_id: String,
    connection_id: String,
    focused: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewAutomationResponseInput {
    client_id: String,
    connection_id: String,
    request_id: String,
    ok: bool,
    result: Option<Value>,
    error: Option<Value>,
}

#[cfg(test)]
mod tests {
    use crate::{
        RequestId,
        mcp::preview_automation::{PreviewAutomationInvokeInput, PreviewAutomationOperation},
        workspace::WorkspaceService,
    };

    use super::*;

    #[tokio::test]
    async fn unit_build_covers_workspace_preview_and_automation_rpc_paths() {
        let temp = tempfile::tempdir().expect("workspace");
        let automation = PreviewAutomationBroker::new();
        let services = WorkspacePreviewRpcServices::new(
            WorkspaceRpc::new(WorkspaceService::default()),
            PreviewManager::new(),
            automation.clone(),
        );
        let request = |tag: &str, payload: Value| RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: tag.to_owned(),
            payload,
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        };

        let preview_event_cancellation = CancellationToken::new();
        let mut preview_events =
            preview_event_stream(services.preview.clone(), preview_event_cancellation.clone());

        let cwd = temp.path().to_string_lossy().into_owned();
        assert_eq!(
            services
                .handle_workspace(
                    request(
                        "projects.createEntry",
                        json!({"cwd":cwd,"relativePath":"created.txt","kind":"file"}),
                    ),
                    CancellationToken::new(),
                )
                .await
                .expect("workspace entry creates"),
            json!({"relativePath":"created.txt"}),
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            services
                .handle_workspace(
                    request("projects.listEntries", json!({"cwd":cwd})),
                    cancelled,
                )
                .await
                .expect_err("cancelled workspace request"),
            interrupted_error("projects.listEntries"),
        );
        let review = normalize_review_result(json!({
            "generatedAt":0,
            "sources":[{"path":"src/lib.rs","diff":"+line"},{"kind":"commit","id":"existing"}]
        }))
        .expect("review result normalizes");
        assert_eq!(review["sources"][0]["kind"], "working-tree");
        assert_eq!(review["sources"][1]["id"], "existing");
        assert!(normalize_review_result(Value::Null).is_err());

        let opened = services
            .dispatch_preview(
                "preview.open",
                json!({"threadId":"thread-1","url":"localhost:4173"}),
            )
            .await
            .expect("preview opens");
        let opened_event = preview_events
            .recv()
            .await
            .expect("preview event chunk")
            .expect("preview event encodes");
        assert_eq!(opened_event[0]["type"], "opened");
        preview_event_cancellation.cancel();
        while preview_events.recv().await.is_some() {}
        let tab_id = opened["tabId"].as_str().expect("tab id").to_owned();
        let navigated = services
            .dispatch_preview(
                "preview.navigate",
                json!({
                    "threadId":"thread-1",
                    "tabId":tab_id,
                    "url":"http://localhost:4174/path",
                    "resolvedTitle":"Next"
                }),
            )
            .await
            .expect("preview navigates");
        assert_eq!(navigated["navStatus"]["title"], "Next");
        let resized = services
            .dispatch_preview(
                "preview.resize",
                json!({
                    "threadId":"thread-1",
                    "tabId":tab_id,
                    "viewport":{"_tag":"preset","presetId":"phone","width":375,"height":667}
                }),
            )
            .await
            .expect("preview resizes");
        assert_eq!(resized["viewport"]["presetId"], "phone");
        assert!(
            services
                .dispatch_preview(
                    "preview.resize",
                    json!({
                        "threadId":"thread-1",
                        "tabId":tab_id,
                        "viewport":{"_tag":"freeform","width":10,"height":10}
                    }),
                )
                .await
                .is_err()
        );
        services
            .dispatch_preview(
                "preview.reportStatus",
                json!({
                    "threadId":"thread-1",
                    "tabId":tab_id,
                    "navStatus":{"_tag":"Success","url":"http://localhost:4174/path","title":"Ready"},
                    "canGoBack":true,
                    "canGoForward":false
                }),
            )
            .await
            .expect("preview status reports");
        services
            .dispatch_preview(
                "preview.refresh",
                json!({"threadId":"thread-1","tabId":tab_id}),
            )
            .await
            .expect("preview refreshes");
        assert_eq!(
            services
                .dispatch_preview("preview.list", json!({"threadId":"thread-1"}))
                .await
                .expect("previews list")["sessions"]
                .as_array()
                .expect("sessions")
                .len(),
            1,
        );
        assert!(
            services
                .dispatch_preview("preview.unknown", json!({}))
                .await
                .is_err()
        );

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            services
                .handle_preview(
                    request("preview.list", json!({"threadId":"thread-1"})),
                    cancelled,
                )
                .await
                .expect_err("cancelled preview request"),
            interrupted_error("preview.list"),
        );

        let stream_cancellation = CancellationToken::new();
        let mut stream = services.automation_connect(
            request(
                "previewAutomation.connect",
                json!({"clientId":"desktop-1","environmentId":"local"}),
            ),
            stream_cancellation.clone(),
        );
        let connected = stream
            .recv()
            .await
            .expect("connected chunk")
            .expect("connected event");
        let connection_id = connected[0]["connectionId"]
            .as_str()
            .expect("connection id")
            .to_owned();
        services
            .handle_preview(
                request(
                    "previewAutomation.focusHost",
                    json!({
                        "clientId":"desktop-1",
                        "environmentId":"local",
                        "connectionId":connection_id,
                        "focused":true
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("host focuses");

        let invoke_automation = automation.clone();
        let invoke = tokio::spawn(async move {
            invoke_automation
                .invoke(PreviewAutomationInvokeInput {
                    environment_id: "local".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    provider_session_id: "provider-session".to_owned(),
                    provider_instance_id: "codex".to_owned(),
                    operation: PreviewAutomationOperation::Snapshot,
                    input: json!({}),
                    tab_id: Some("tab-explicit".to_owned()),
                    timeout_ms: Some(2_000),
                })
                .await
        });
        let automation_request = stream
            .recv()
            .await
            .expect("request chunk")
            .expect("request event");
        let request_id = automation_request[0]["request"]["requestId"]
            .as_str()
            .expect("automation request id")
            .to_owned();
        services
            .handle_preview(
                request(
                    "previewAutomation.respond",
                    json!({
                        "clientId":"desktop-1",
                        "connectionId":connection_id,
                        "requestId":request_id,
                        "ok":true,
                        "result":{"tabId":"tab-result"},
                        "error":null
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("automation responds");
        assert_eq!(
            invoke.await.expect("invoke task").expect("invoke result")["tabId"],
            "tab-result"
        );
        stream_cancellation.cancel();
        while stream.recv().await.is_some() {}

        services
            .dispatch_preview(
                "preview.close",
                json!({"threadId":"thread-1","tabId":tab_id}),
            )
            .await
            .expect("preview closes");
        assert_eq!(
            preview_error(PreviewError::SessionLookup {
                thread_id: "missing".to_owned(),
                tab_id: "tab".to_owned(),
            })["_tag"],
            "PreviewSessionLookupError"
        );
        assert_eq!(
            preview_error(PreviewError::InvalidUrl {
                input_length: 4,
                reason: "unsupported protocol",
                protocol: Some("file:".to_owned()),
            })["protocol"],
            "file:"
        );
        let focus_error = automation
            .focus_host("missing", "local")
            .await
            .expect_err("missing automation host");
        assert_eq!(
            automation_error(focus_error)["_tag"],
            "PreviewAutomationNoAvailableHostError"
        );
    }
}
