use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Deserialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    json_size::encoded_json_len,
    orchestration::{
        CommandAdmission, NewProviderTurnDelivery, OrchestrationCommand, OrchestrationEngine,
        OrchestrationError, canonical_command_digest,
        engine::{CommandLifetimeGuard, OptionalNullable, TurnDeliveryResolutionAction},
        load_shell_snapshot, load_thread_snapshot,
    },
    persistence::{
        EVENT_PAGE_SIZE, OrchestrationEvent, PersistenceError, ProjectionThread, Repositories,
    },
    provider::attachments::{
        AttachmentMaterializationError, AttachmentMaterializer, PreparedAttachmentBatch,
        ReusableAttachments, id_only_attachment_ids,
    },
    rpc::{
        MAX_RECORDED_MESSAGE_BYTES, RpcRegistry, RpcRequest, RpcResult, RpcStreamChunk,
        response_too_large_failure,
    },
    server_settings::ProviderSettingsStore,
    transfer::staging::{UploadOwner, UploadRegistry},
    worktree_catalog::WorkspaceAvailabilityRegistry,
};

use super::orchestration_effects::install_project_command_effects;
use super::provider_runtime::{
    ProviderRuntimeError, ProviderRuntimeSupervisor, canonical_provider_kind,
    freeze_delivery_route, route_orchestration_command,
};
use super::turn_delivery::TurnDeliveryService;
use super::workspace_availability::{WorkspaceAdmissionController, WorkspaceAdmissionError};

const STREAM_CAPACITY: usize = 16;
/// Use the History page target (`COMMIT_PAGE_TARGET_BYTES`): 1 MiB of event JSON.
const REPLAY_PAGE_TARGET_BYTES: usize = 1024 * 1024;

pub fn register_orchestration_rpc(registry: &mut RpcRegistry, engine: OrchestrationEngine) {
    register_orchestration_rpc_inner(registry, engine, None, None);
}

pub fn register_orchestration_rpc_with_availability(
    registry: &mut RpcRegistry,
    engine: OrchestrationEngine,
    availability: WorkspaceAvailabilityRegistry,
) {
    register_orchestration_rpc_inner(registry, engine, None, Some(availability));
}

pub fn register_orchestration_rpc_with_delivery(
    registry: &mut RpcRegistry,
    engine: OrchestrationEngine,
    provider: Arc<ProviderRuntimeSupervisor>,
    settings_root: PathBuf,
    turn_delivery: Arc<TurnDeliveryService>,
    uploads: UploadRegistry,
) {
    let attachments = AttachmentMaterializer::new(settings_root.join("attachments"));
    register_orchestration_rpc_inner(
        registry,
        engine,
        Some(ProviderRegistration {
            provider,
            settings_root,
            attachments,
            turn_delivery,
            uploads,
        }),
        None,
    );
}

pub fn register_orchestration_rpc_with_delivery_and_availability(
    registry: &mut RpcRegistry,
    engine: OrchestrationEngine,
    provider: Arc<ProviderRuntimeSupervisor>,
    settings_root: PathBuf,
    turn_delivery: Arc<TurnDeliveryService>,
    availability: WorkspaceAvailabilityRegistry,
    uploads: UploadRegistry,
) {
    let attachments = AttachmentMaterializer::new(settings_root.join("attachments"));
    register_orchestration_rpc_inner(
        registry,
        engine,
        Some(ProviderRegistration {
            provider,
            settings_root,
            attachments,
            turn_delivery,
            uploads,
        }),
        Some(availability),
    );
}

#[derive(Clone)]
struct ProviderRegistration {
    provider: Arc<ProviderRuntimeSupervisor>,
    settings_root: PathBuf,
    attachments: AttachmentMaterializer,
    turn_delivery: Arc<TurnDeliveryService>,
    uploads: UploadRegistry,
}

fn register_orchestration_rpc_inner(
    registry: &mut RpcRegistry,
    engine: OrchestrationEngine,
    provider: Option<ProviderRegistration>,
    availability: Option<WorkspaceAvailabilityRegistry>,
) {
    install_project_command_effects(&engine);
    let availability = availability
        .map(|availability| WorkspaceAdmissionController::new(availability, engine.repositories()));
    let dispatch = engine.clone();
    registry.register_unary_with_context(
        "orchestration.dispatchCommand",
        move |request, context, _| {
            let dispatch = dispatch.clone();
            let owner = UploadOwner::from_context(&context);
            let provider = provider.clone();
            let availability = availability.clone();
            async move {
                let payload_digest = canonical_command_digest(&request.payload)
                    .map_err(|error| invalid_request(&request.tag, error))?;
                let command = decode_public_orchestration_command(&dispatch, request.payload)
                    .await
                    .map_err(|error| invalid_request(&request.tag, error))?;
                let command_claim = dispatch
                    .acquire_command_admission(command.command_id())
                    .await
                    .map_err(|error| {
                        orchestration_error("OrchestrationDispatchCommandError", error)
                    })?;
                if let OrchestrationCommand::ThreadTurnStart { thread_id, .. } = &command {
                    let workspace_admission = if let Some(availability) = &availability {
                        Some(
                            availability
                                .acquire_thread(thread_id, std::iter::empty())
                                .await
                                .map_err(workspace_admission_error)?,
                        )
                    } else {
                        None
                    };
                    let provider = provider.ok_or_else(|| {
                        invalid_request(
                            &request.tag,
                            "thread.turn.start requires durable provider delivery",
                        )
                    })?;
                    return dispatch_turn_command(
                        dispatch,
                        provider,
                        command,
                        payload_digest,
                        request.tag,
                        workspace_admission,
                        command_claim,
                        &owner,
                    )
                    .await;
                }
                dispatch_prepared_command(
                    dispatch,
                    provider,
                    command,
                    payload_digest,
                    request.tag,
                    command_claim,
                )
                .await
            }
        },
    );

    let replay = engine.clone();
    registry.register_unary("orchestration.replayEvents", move |request, _| {
        let replay = replay.clone();
        async move {
            let tag = request.tag.clone();
            let input = decode::<ReplayInput>(request)?;
            if input.from_sequence_exclusive < 0 {
                return Err(invalid_request(
                    &tag,
                    "fromSequenceExclusive must be a non-negative integer",
                ));
            }
            let budget = if input.paged {
                ReplayBudget::Page {
                    target_bytes: REPLAY_PAGE_TARGET_BYTES,
                }
            } else {
                ReplayBudget::Whole {
                    limit_bytes: MAX_RECORDED_MESSAGE_BYTES,
                }
            };
            let read = read_replay(
                &replay.repositories(),
                input.from_sequence_exclusive,
                budget,
            )
            .await
            .map_err(|error| match error {
                ReplayReadError::TooLarge { bytes, limit_bytes } => {
                    response_too_large_failure(&tag, bytes, limit_bytes)
                }
                error => orchestration_error("OrchestrationReplayEventsError", error),
            })?;
            let events = Value::Array(read.events);
            if input.paged {
                let mut page = json!({ "exhausted": read.exhausted });
                // json! uses to_value(&value); move by index to avoid deep-copying the whole page.
                page["events"] = events;
                Ok(page)
            } else {
                Ok(events)
            }
        }
    });

    for method in [
        "orchestration.getArchivedShellSnapshot",
        "orchestration.getTurnDiff",
        "orchestration.getFullThreadDiff",
    ] {
        let engine = engine.clone();
        registry.register_unary(method, move |request, _| {
            let engine = engine.clone();
            async move { handle_query(&engine, request).await }
        });
    }

    let shell = engine.clone();
    registry.register_stream(
        "orchestration.subscribeShell",
        move |_request, cancellation| shell_stream(shell.clone(), cancellation),
    );
    registry.register_stream(
        "orchestration.subscribeThread",
        move |request, cancellation| thread_stream(engine.clone(), request, cancellation),
    );
}

/// Decodes the public orchestration wire shape and resolves the small amount of
/// server-owned context that ordinary commands need. Worktree policy, identity,
/// lifecycle kind, and adopted-owner deletion are deliberately not part of this
/// generic ingress.
pub(crate) async fn decode_public_orchestration_command(
    engine: &OrchestrationEngine,
    payload: Value,
) -> Result<OrchestrationCommand, String> {
    let supplied_prepare_cwd = payload
        .get("bootstrap")
        .and_then(|bootstrap| bootstrap.get("prepareWorktree"))
        .and_then(Value::as_object)
        .is_some_and(|prepare| prepare.contains_key("projectCwd"));
    let mut command = serde_json::from_value::<OrchestrationCommand>(payload)
        .map_err(|error| error.to_string())?;
    if command.is_server_internal() {
        return Err(
            "server-internal orchestration commands cannot be dispatched by clients".to_owned(),
        );
    }

    const DEDICATED_AUTHORITY: &str = "worktree authority requires a dedicated server-resolved RPC";
    match &command {
        OrchestrationCommand::ProjectMetaUpdate {
            worktree_discovery: Some(_),
            ..
        }
        | OrchestrationCommand::ThreadCreate { kind: Some(_), .. }
        | OrchestrationCommand::ThreadCreate {
            worktree_path: Some(_),
            ..
        }
        | OrchestrationCommand::ThreadCreate {
            host_thread_id: Some(_),
            ..
        }
        | OrchestrationCommand::ThreadMetaUpdate {
            worktree_path: OptionalNullable::Present(_),
            ..
        } => return Err(DEDICATED_AUTHORITY.to_owned()),
        OrchestrationCommand::ThreadTurnStart {
            bootstrap: Some(bootstrap),
            ..
        } if supplied_prepare_cwd
            || bootstrap
                .create_thread
                .as_ref()
                .is_some_and(|create| create.worktree_path.is_some()) =>
        {
            return Err(DEDICATED_AUTHORITY.to_owned());
        }
        OrchestrationCommand::ThreadDelete { thread_id, .. } => {
            if engine
                .repositories()
                .get_thread(thread_id.clone())
                .await
                .map_err(|error| error.to_string())?
                .is_some_and(|thread| {
                    thread.deleted_at.is_none()
                        && thread.kind == "workspace"
                        && thread.worktree_path.is_some()
                })
            {
                return Err(DEDICATED_AUTHORITY.to_owned());
            }
        }
        OrchestrationCommand::ProjectDelete { project_id, .. }
            if engine
                .repositories()
                .list_threads_by_project(project_id.clone())
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .any(|thread| {
                    thread.deleted_at.is_none()
                        && thread.kind == "workspace"
                        && thread.worktree_path.is_some()
                }) =>
        {
            return Err(DEDICATED_AUTHORITY.to_owned());
        }
        _ => {}
    }

    if let OrchestrationCommand::ThreadCreate {
        project_id, kind, ..
    } = &mut command
    {
        let has_default = engine
            .repositories()
            .list_threads_by_project(project_id.clone())
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .any(|thread| thread.deleted_at.is_none() && thread.kind == "default");
        *kind = Some(if has_default { "workspace" } else { "default" }.to_owned());
    }

    if let OrchestrationCommand::ThreadTurnStart {
        thread_id,
        bootstrap: Some(bootstrap),
        ..
    } = &mut command
        && let Some(prepare) = bootstrap.prepare_worktree.as_mut()
    {
        let project_id = if let Some(create) = bootstrap.create_thread.as_ref() {
            create.project_id.clone()
        } else {
            engine
                .repositories()
                .get_thread(thread_id.clone())
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("thread '{thread_id}' does not exist"))?
                .project_id
        };
        prepare.project_cwd = engine
            .repositories()
            .get_project(project_id.clone())
            .await
            .map_err(|error| error.to_string())?
            .filter(|project| project.deleted_at.is_none())
            .ok_or_else(|| format!("project '{project_id}' does not exist"))?
            .workspace_root;
    }
    Ok(command)
}

async fn dispatch_prepared_command(
    dispatch: OrchestrationEngine,
    provider: Option<ProviderRegistration>,
    command: OrchestrationCommand,
    payload_digest: String,
    request_tag: String,
    command_claim: crate::orchestration::engine::CommandAdmissionClaim,
) -> RpcResult {
    let delivery_cancellation = match &command {
        OrchestrationCommand::ThreadTurnDeliveryResolve {
            command_id,
            thread_id,
            action: TurnDeliveryResolutionAction::Dismiss,
            created_at,
            ..
        } => Some((command_id.clone(), thread_id.clone(), created_at.clone())),
        // Closing a chat panel archives it. The server stops its work, including a delivery
        // claimed but not yet visible to clients as a running session.
        OrchestrationCommand::ThreadArchive {
            command_id,
            thread_id,
        } => dispatch
            .repositories()
            .get_thread(thread_id.clone())
            .await
            .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?
            .filter(|thread| thread.kind == "panel")
            .map(|_| (command_id.clone(), thread_id.clone(), now_iso())),
        _ => None,
    };
    let is_delivery_resolution = matches!(
        command,
        OrchestrationCommand::ThreadTurnDeliveryResolve { .. }
    );
    let existing_receipt = dispatch
        .repositories()
        .get_command_receipt(command.command_id().to_owned())
        .await
        .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
    let legacy_replay = existing_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.payload_digest.is_none());
    let route_before_admission = if !legacy_replay
        && matches!(
            &command,
            OrchestrationCommand::ThreadMetaUpdate {
                model_selection: Some(_),
                ..
            }
        ) {
        dispatch
            .reserve_generic_command_admission(&command_claim, &command, &payload_digest)
            .await
            .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?
    } else {
        false
    };
    if route_before_admission && let Some(provider) = provider.as_ref() {
        #[cfg(test)]
        dispatch
            .test_hooks()
            .note_generic_external_preparation_attempt();
        route_orchestration_command(
            &provider.provider,
            &dispatch,
            &provider.settings_root,
            command.clone(),
        )
        .await
        .map_err(provider_command_error)?;
    }
    let accepted_new = Arc::new(AtomicBool::new(false));
    let result = if legacy_replay {
        dispatch
            .dispatch_with_command_claim(command.clone(), command_claim)
            .await
    } else {
        let turn_delivery = is_delivery_resolution
            .then(|| {
                provider
                    .as_ref()
                    .map(|provider| provider.turn_delivery.clone())
            })
            .flatten();
        let committed = accepted_new.clone();
        dispatch
            .dispatch_with_admission_and_command_claim(
                command.clone(),
                CommandAdmission {
                    payload_digest,
                    attachment_refs: Vec::new(),
                    provider_turn: None,
                },
                command_claim,
                move || {
                    committed.store(true, Ordering::Release);
                    if let Some(turn_delivery) = turn_delivery {
                        turn_delivery.wake();
                    }
                },
            )
            .await
    }
    .map_err(|error| match error {
        OrchestrationError::ProjectPreparation { detail } => invalid_request(&request_tag, detail),
        error => orchestration_error("OrchestrationDispatchCommandError", error),
    })?;
    let should_route = match (&command, &result.project_id) {
        (OrchestrationCommand::ProjectCreate { project_id, .. }, Some(resolved_project_id)) => {
            project_id == resolved_project_id
        }
        _ => true,
    };
    if let Some(provider) = provider
        && accepted_new.load(Ordering::Acquire)
        && should_route
    {
        if let Some((command_id, thread_id, created_at)) = delivery_cancellation {
            let result = provider
                .provider
                .handle_orchestration(OrchestrationCommand::ThreadTurnInterrupt {
                    command_id: format!("{command_id}:provider-interrupt"),
                    thread_id,
                    turn_id: None,
                    created_at,
                })
                .await;
            if let Err(error) = result
                && !matches!(error, ProviderRuntimeError::SessionNotFound { .. })
            {
                tracing::warn!(%error, "provider interrupt failed after cancelling provider work");
            }
        } else if !is_delivery_resolution && !route_before_admission {
            route_orchestration_command(
                &provider.provider,
                &dispatch,
                &provider.settings_root,
                command,
            )
            .await
            .map_err(provider_command_error)?;
        }
    }
    serde_json::to_value(result).map_err(|error| invalid_request(&request_tag, error.to_string()))
}

#[expect(
    clippy::too_many_arguments,
    reason = "Durable admission explicitly carries the workspace lease, command claim, and authenticated upload owner."
)]
async fn dispatch_turn_command(
    dispatch: OrchestrationEngine,
    provider: ProviderRegistration,
    command: OrchestrationCommand,
    payload_digest: String,
    request_tag: String,
    workspace_admission: Option<crate::worktree_catalog::WorkspaceAdmissionLease>,
    command_claim: crate::orchestration::engine::CommandAdmissionClaim,
    owner: &UploadOwner,
) -> RpcResult {
    let existing_receipt = dispatch
        .repositories()
        .get_command_receipt(command.command_id().to_owned())
        .await
        .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
    if existing_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.payload_digest.is_none())
    {
        let result = dispatch
            .dispatch_with_command_claim(command, command_claim)
            .await
            .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
        return serde_json::to_value(result)
            .map_err(|error| invalid_request(&request_tag, error.to_string()));
    }

    let prepare_external = dispatch
        .reserve_generic_command_admission(&command_claim, &command, &payload_digest)
        .await
        .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
    if !prepare_external {
        let result = dispatch
            .dispatch_with_admission_and_command_claim(
                command,
                CommandAdmission {
                    payload_digest,
                    attachment_refs: Vec::new(),
                    provider_turn: None,
                },
                command_claim,
                || {},
            )
            .await
            .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
        return serde_json::to_value(result)
            .map_err(|error| invalid_request(&request_tag, error.to_string()));
    }

    let reserved_command = command.clone();
    let reserved_digest = payload_digest.clone();
    let result = dispatch_reserved_turn_command(
        dispatch.clone(),
        provider,
        command,
        payload_digest,
        request_tag.clone(),
        workspace_admission,
        command_claim.clone(),
        owner,
    )
    .await;
    if result.is_err() {
        dispatch
            .release_generic_command_admission(&command_claim, &reserved_command, &reserved_digest)
            .await
            .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
    }
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "Durable admission explicitly carries the workspace lease, command claim, and authenticated upload owner."
)]
async fn dispatch_reserved_turn_command(
    dispatch: OrchestrationEngine,
    provider: ProviderRegistration,
    command: OrchestrationCommand,
    payload_digest: String,
    request_tag: String,
    workspace_admission: Option<crate::worktree_catalog::WorkspaceAdmissionLease>,
    command_claim: crate::orchestration::engine::CommandAdmissionClaim,
    owner: &UploadOwner,
) -> RpcResult {
    let reusable = reusable_thread_attachments(&dispatch, &command, &request_tag).await?;
    let (mut command, prepared_batch) = prepare_attachments(
        &provider.attachments,
        command,
        &reusable,
        owner,
        &provider.uploads,
    )
    .await
    .map_err(|error| match error {
        AttachmentMaterializationError::Upload(upload) => {
            serde_json::to_value(upload).expect("upload error serializes")
        }
        other => invalid_request(&request_tag, other.to_string()),
    })?;
    let attachment_refs = prepared_batch
        .as_ref()
        .map(|batch| batch.references().to_vec())
        .unwrap_or_default();
    let mut delivery_state = crate::orchestration::TurnDeliveryState::Pending;
    if let OrchestrationCommand::ThreadTurnStart {
        thread_id,
        queued,
        model_selection,
        bootstrap,
        ..
    } = &mut command
    {
        if *queued == Some(true) {
            let session = dispatch
                .repositories()
                .get_thread_session(thread_id.clone())
                .await
                .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
            let resolved = session
                .is_some_and(|session| matches!(session.status.as_str(), "running" | "starting"));
            *queued = Some(resolved);
            if resolved {
                delivery_state = crate::orchestration::TurnDeliveryState::Queued;
            }
        }
        // Freeze even an omitted selection before a later thread configuration can change it.
        if model_selection.is_none() {
            *model_selection = bootstrap
                .as_deref()
                .and_then(|bootstrap| bootstrap.create_thread.as_ref())
                .map(|create| create.model_selection.clone());
            if model_selection.is_none() {
                *model_selection = dispatch
                    .repositories()
                    .get_thread(thread_id.clone())
                    .await
                    .map_err(|error| {
                        orchestration_error("OrchestrationDispatchCommandError", error)
                    })?
                    .map(|thread| thread.model_selection);
            }
        }
    }
    let (thread_id, message_id, instance_id, provider_kind, created_at) =
        turn_identity(&dispatch, &provider.settings_root, &command)
            .await
            .map_err(|error| invalid_request(&request_tag, error))?;
    let mut delivery_payload = serde_json::to_value(&command)
        .map_err(|error| invalid_request(&request_tag, error.to_string()))?;
    freeze_delivery_route(
        &dispatch,
        &provider.settings_root,
        &command,
        &mut delivery_payload,
    )
    .await
    .map_err(provider_command_error)?;
    let admission = CommandAdmission {
        payload_digest,
        attachment_refs,
        provider_turn: Some(NewProviderTurnDelivery {
            state: delivery_state,
            mode: crate::orchestration::TurnDeliveryMode::Start,
            command_id: command.command_id().to_owned(),
            thread_id,
            message_id,
            provider_instance_id: instance_id,
            provider_kind,
            provider_session_id: None,
            delivery_key: uuid::Uuid::new_v4().to_string(),
            payload: delivery_payload,
            created_at,
        }),
    };
    let turn_delivery = provider.turn_delivery.clone();
    let on_commit = move || {
        if let Some(batch) = prepared_batch {
            batch.commit();
        }
        turn_delivery.wake();
    };
    let result = if let Some(workspace_admission) = workspace_admission {
        let loss = workspace_admission.loss_cancellation();
        let commit_fence = workspace_admission.commit_fence();
        let lifetime =
            CommandLifetimeGuard::new(workspace_admission, loss.cancellation_token(), commit_fence);
        let dispatch = dispatch.dispatch_with_admission_lifetime_and_command_claim(
            command,
            admission,
            lifetime,
            command_claim,
            on_commit,
        );
        tokio::pin!(dispatch);
        tokio::select! {
            biased;
            () = loss.cancelled() => {
                let unavailable = loss
                    .unavailable()
                    .expect("workspace loss cancellation retains its error");
                return Err(workspace_admission_error(
                    WorkspaceAdmissionError::Unavailable(unavailable),
                ));
            }
            result = &mut dispatch => {
                if matches!(result, Err(OrchestrationError::Cancelled))
                    && let Some(unavailable) = loss.unavailable()
                {
                    return Err(workspace_admission_error(
                        WorkspaceAdmissionError::Unavailable(unavailable),
                    ));
                }
                result
            },
        }
    } else {
        dispatch
            .dispatch_with_admission_and_command_claim(command, admission, command_claim, on_commit)
            .await
    }
    .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
    serde_json::to_value(result).map_err(|error| invalid_request(&request_tag, error.to_string()))
}

async fn turn_identity(
    engine: &OrchestrationEngine,
    settings_root: &PathBuf,
    command: &OrchestrationCommand,
) -> Result<(String, String, String, String, String), String> {
    let OrchestrationCommand::ThreadTurnStart {
        thread_id,
        message,
        model_selection,
        bootstrap,
        created_at,
        ..
    } = command
    else {
        return Err("only turn starts have durable delivery identity".to_owned());
    };
    let thread = engine
        .repositories()
        .get_thread(thread_id.clone())
        .await
        .map_err(|error| error.to_string())?;
    let bootstrap_selection = bootstrap
        .as_deref()
        .and_then(|bootstrap| bootstrap.create_thread.as_ref())
        .map(|create| &create.model_selection);
    let selection = model_selection
        .as_ref()
        .or(bootstrap_selection)
        .or_else(|| thread.as_ref().map(|thread| &thread.model_selection))
        .ok_or_else(|| format!("turn for thread {thread_id} has no provider identity"))?;
    let instance_id = selection
        .get("instanceId")
        .and_then(Value::as_str)
        .filter(|instance_id| !instance_id.trim().is_empty())
        .ok_or_else(|| format!("turn for thread {thread_id} has no provider instanceId"))?
        .to_owned();
    let settings = ProviderSettingsStore::new(settings_root)
        .get()
        .await
        .map_err(|error| error.to_string())?;
    let driver = settings
        .provider_instances
        .get(&instance_id)
        .map(|instance| instance.driver.as_str())
        .unwrap_or(instance_id.as_str());
    let provider_kind = canonical_provider_kind(driver).map_err(|error| error.to_string())?;
    Ok((
        thread_id.clone(),
        message.message_id.clone(),
        instance_id,
        provider_kind.to_owned(),
        created_at.clone(),
    ))
}

/// The attachments a turn start may send again by id alone: those an accepted command already
/// attached in the same thread. Nothing is looked up when every attachment carries its bytes.
async fn reusable_thread_attachments(
    engine: &OrchestrationEngine,
    command: &OrchestrationCommand,
    request_tag: &str,
) -> Result<ReusableAttachments, Value> {
    let OrchestrationCommand::ThreadTurnStart {
        thread_id, message, ..
    } = command
    else {
        return Ok(ReusableAttachments::new());
    };
    let ids = id_only_attachment_ids(&message.attachments)
        .map_err(|error| invalid_request(request_tag, error.to_string()))?;
    if ids.is_empty() {
        return Ok(ReusableAttachments::new());
    }
    engine
        .repositories()
        .thread_attachment_digests(thread_id.clone(), ids)
        .await
        .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))
}

async fn prepare_attachments(
    attachments: &AttachmentMaterializer,
    mut command: OrchestrationCommand,
    reusable: &ReusableAttachments,
    owner: &UploadOwner,
    uploads: &UploadRegistry,
) -> Result<(OrchestrationCommand, Option<PreparedAttachmentBatch>), AttachmentMaterializationError>
{
    if let OrchestrationCommand::ThreadTurnStart { message, .. } = &mut command {
        if message.attachments.is_empty() {
            return Ok((command, None));
        }
        let prepared = attachments
            .prepare(
                std::mem::take(&mut message.attachments),
                reusable,
                owner,
                uploads,
            )
            .await?;
        message.attachments = prepared.attachments().to_vec();
        return Ok((command, Some(prepared)));
    }
    Ok((command, None))
}

async fn handle_query(engine: &OrchestrationEngine, request: RpcRequest) -> RpcResult {
    match request.tag.as_str() {
        "orchestration.getArchivedShellSnapshot" => shell_snapshot(engine, true).await,
        "orchestration.getTurnDiff" => {
            let input = decode::<TurnDiffInput>(request)?;
            diff(
                engine,
                input.thread_id,
                input.from_turn_count,
                input.to_turn_count,
            )
            .await
        }
        "orchestration.getFullThreadDiff" => {
            let input = decode::<FullDiffInput>(request)?;
            diff(engine, input.thread_id, 0, input.to_turn_count).await
        }
        _ => Err(invalid_request(
            &request.tag,
            "unsupported orchestration query",
        )),
    }
}

async fn diff(
    engine: &OrchestrationEngine,
    thread_id: String,
    from_turn_count: i64,
    to_turn_count: i64,
) -> RpcResult {
    if from_turn_count < 0 || to_turn_count < from_turn_count {
        return Err(invalid_request(
            "orchestration.diff",
            "turn counts must be non-negative and ordered",
        ));
    }
    let blobs = engine
        .repositories()
        .list_checkpoint_diff_blobs_by_thread(thread_id.clone())
        .await
        .map_err(|error| orchestration_error("OrchestrationGetTurnDiffError", error))?;
    let diff = blobs
        .into_iter()
        .find(|blob| blob.from_turn_count == from_turn_count && blob.to_turn_count == to_turn_count)
        .map(|blob| blob.diff)
        .unwrap_or_default();
    Ok(json!({
        "threadId": thread_id,
        "fromTurnCount": from_turn_count,
        "toTurnCount": to_turn_count,
        "diff": diff,
    }))
}

fn shell_stream(
    engine: OrchestrationEngine,
    cancellation: CancellationToken,
) -> mpsc::Receiver<RpcStreamChunk> {
    let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
    // Subscribe before loading the initial snapshot so an event committed in
    // between is queued for the stream instead of disappearing in that gap.
    let mut events = engine.subscribe_events();
    tokio::spawn(async move {
        if send_snapshot(&sender, shell_snapshot(&engine, false).await)
            .await
            .is_err()
        {
            return;
        }
        // A burst of events yields one trailing snapshot: every event changed the
        // shell, but re-sending it per event saturated slow remote links (#89).
        let mut due: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                () = async { tokio::time::sleep_until(due.expect("guarded")).await }, if due.is_some() => {
                    due = None;
                    if send_snapshot(&sender, shell_snapshot(&engine, false).await).await.is_err() {
                        return;
                    }
                }
                event = events.recv() => match event {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                        due.get_or_insert_with(|| tokio::time::Instant::now() + SHELL_SNAPSHOT_COALESCE);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    receiver
}

/// How long a thread stream gathers its events before one update; short enough
/// for streamed assistant text to read smoothly.
const THREAD_UPDATE_COALESCE: std::time::Duration = std::time::Duration::from_millis(100);

/// What a thread subscriber keeps of the last snapshot it sent, to compute the next patch:
/// a fingerprint of each field of `thread`, and of each entry of its arrays, instead of the
/// thread itself (#101). A long session's thread is megabytes of JSON, and every open
/// subscription held its own copy for as long as it stayed open.
struct HeldThread {
    /// This subscription's own random key: thread content comes from agents and tools, and a
    /// fixed key would let crafted content collide on purpose to hide an update.
    keys: std::hash::RandomState,
    fields: HashMap<String, HeldField>,
}

enum HeldField {
    Value(u64),
    Array(Vec<u64>),
}

impl HeldThread {
    /// `None` for a snapshot without a thread, after which the next update is a snapshot.
    fn of(snapshot: &Value, keys: std::hash::RandomState) -> Option<Self> {
        let thread = snapshot.get("thread")?.as_object()?;
        let fields = thread
            .iter()
            .map(|(key, value)| {
                let held = match value {
                    Value::Array(entries) => HeldField::Array(
                        entries
                            .iter()
                            .map(|entry| fingerprint(&keys, entry))
                            .collect(),
                    ),
                    other => HeldField::Value(fingerprint(&keys, other)),
                };
                (key.clone(), held)
            })
            .collect();
        Some(Self { keys, fields })
    }
}

/// A 64-bit fingerprint of `value` under `keys`, walked like an equality check: no
/// serializing.
// ponytail: a chance collision (about 1 in 2^64 per entry) leaves that entry out of one
// patch, so the client shows it stale until it changes again; keep entries if that matters.
fn fingerprint(keys: &std::hash::RandomState, value: &Value) -> u64 {
    use std::hash::{BuildHasher, Hasher};

    fn walk(value: &Value, hasher: &mut impl Hasher) {
        match value {
            Value::Null => hasher.write_u8(0),
            Value::Bool(flag) => hasher.write_u8(1 + u8::from(*flag)),
            // Each representation its own tag: an integer and a float with the same bits
            // are different values.
            Value::Number(number) => {
                if let Some(integer) = number.as_i64() {
                    hasher.write_u8(3);
                    hasher.write_i64(integer);
                } else if let Some(integer) = number.as_u64() {
                    hasher.write_u8(4);
                    hasher.write_u64(integer);
                } else {
                    hasher.write_u8(5);
                    hasher.write_u64(number.as_f64().unwrap_or(f64::NAN).to_bits());
                }
            }
            Value::String(text) => {
                hasher.write_u8(6);
                hasher.write_usize(text.len());
                hasher.write(text.as_bytes());
            }
            Value::Array(entries) => {
                hasher.write_u8(7);
                hasher.write_usize(entries.len());
                for entry in entries {
                    walk(entry, hasher);
                }
            }
            Value::Object(fields) => {
                hasher.write_u8(8);
                hasher.write_usize(fields.len());
                for (key, field) in fields {
                    hasher.write_usize(key.len());
                    hasher.write(key.as_bytes());
                    walk(field, hasher);
                }
            }
        }
    }
    let mut hasher = keys.build_hasher();
    walk(value, &mut hasher);
    hasher.finish()
}

/// The change from the thread snapshot a subscriber holds to `next`
/// (`{snapshotSequence, thread}`), as a `patch` stream item, or `None` when nothing changed.
/// Changed fields are replaced whole, except arrays: they send their new length and only the
/// entries that differ, by index. The client rebuilds exactly the server's next snapshot.
fn thread_patch(held: &HeldThread, next: &Value) -> Option<Value> {
    let Some(new) = next.get("thread").and_then(Value::as_object) else {
        return Some(json!({ "kind": "snapshot", "snapshot": next }));
    };
    let mut set = serde_json::Map::new();
    let mut arrays = serde_json::Map::new();
    for (key, value) in new {
        match (held.fields.get(key), value) {
            (Some(HeldField::Array(before)), Value::Array(after)) => {
                // Only the entries that differ: an appended activity or a message
                // updated in place costs one entry, wherever it sorts.
                let changes = after
                    .iter()
                    .enumerate()
                    .filter(|(index, entry)| {
                        before.get(*index) != Some(&fingerprint(&held.keys, entry))
                    })
                    .map(|(index, entry)| json!([index, entry]))
                    .collect::<Vec<_>>();
                if changes.is_empty() && before.len() == after.len() {
                    continue;
                }
                arrays.insert(
                    key.clone(),
                    json!({ "length": after.len(), "changes": changes }),
                );
            }
            (Some(HeldField::Value(before)), value)
                if !value.is_array() && *before == fingerprint(&held.keys, value) => {}
            _ => {
                set.insert(key.clone(), value.clone());
            }
        }
    }
    let unset = held
        .fields
        .keys()
        .filter(|key| !new.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if set.is_empty() && arrays.is_empty() && unset.is_empty() {
        return None;
    }
    Some(json!({
        "kind": "patch",
        "snapshotSequence": next.get("snapshotSequence"),
        "patch": { "set": set, "unset": unset, "arrays": arrays },
    }))
}

/// How long the shell stream gathers events before sending one snapshot.
const SHELL_SNAPSHOT_COALESCE: std::time::Duration = std::time::Duration::from_millis(500);

fn thread_stream(
    engine: OrchestrationEngine,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> mpsc::Receiver<RpcStreamChunk> {
    let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
    let input = match decode::<SubscribeThreadInput>(request) {
        Ok(input) => input,
        Err(error) => {
            tokio::spawn(async move {
                let _ = sender.send(Err(error)).await;
            });
            return receiver;
        }
    };
    // Match the shell stream's subscribe-before-snapshot ordering. Provider
    // startup can publish a session update while this snapshot is loading.
    let mut events = engine.subscribe_events();
    tokio::spawn(async move {
        let first = thread_snapshot(&engine, &input.thread_id).await;
        // What the subscriber holds of the last snapshot, for a client that applies patches.
        let keys = std::hash::RandomState::new();
        let hold = |snapshot: &RpcResult| {
            snapshot
                .as_ref()
                .ok()
                .filter(|_| input.patches)
                .and_then(|snapshot| HeldThread::of(snapshot, keys.clone()))
        };
        let mut held = hold(&first);
        // An error ends the stream; a deleted thread has no events to end the loop by (#113).
        let failed = first.is_err();
        if send_snapshot(&sender, first).await.is_err() || failed {
            return;
        }
        let mut due: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                () = async { tokio::time::sleep_until(due.expect("guarded")).await }, if due.is_some() => {
                    due = None;
                    let next = thread_snapshot(&engine, &input.thread_id).await;
                    // A whole thread per update re-sent megabytes for long threads and
                    // saturated slow remote links (#89): a client that applies patches
                    // gets only what changed since the snapshot it holds.
                    let patch = match (&next, &held) {
                        (Ok(next), Some(held)) if input.patches => Some(thread_patch(held, next)),
                        _ => None,
                    };
                    let sent = match patch {
                        Some(None) => Ok(()),
                        Some(Some(item)) => sender.send(Ok(vec![item])).await.map_err(|_| ()),
                        None => send_snapshot(&sender, next.clone()).await,
                    };
                    if sent.is_err() || next.is_err() {
                        return;
                    }
                    held = hold(&next);
                }
                event = events.recv() => match event {
                    // Lost events need no special handling: every update re-reads the
                    // projection, so the next one is exact either way.
                    Ok(event) if event.event.aggregate_kind == "thread" && event.event.aggregate_id == input.thread_id => {
                        due.get_or_insert_with(|| tokio::time::Instant::now() + THREAD_UPDATE_COALESCE);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        due.get_or_insert_with(|| tokio::time::Instant::now() + THREAD_UPDATE_COALESCE);
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    });
    receiver
}

async fn send_snapshot(
    sender: &mpsc::Sender<RpcStreamChunk>,
    snapshot: RpcResult,
) -> Result<(), ()> {
    sender
        .send(snapshot.map(|snapshot| vec![json!({ "kind": "snapshot", "snapshot": snapshot })]))
        .await
        .map_err(|_| ())
}

pub async fn shell_snapshot(engine: &OrchestrationEngine, archived: bool) -> RpcResult {
    let snapshot = load_shell_snapshot(&engine.repositories(), archived)
        .await
        .map_err(|error| orchestration_error("OrchestrationGetSnapshotError", error))?;
    Ok(shell_snapshot_value(&snapshot, archived))
}

/// Builds the shell wire value from any snapshot that holds the shown threads' rows.
fn shell_snapshot_value(snapshot: &crate::orchestration::Snapshot, archived: bool) -> Value {
    let sequence = snapshot
        .states
        .iter()
        .map(|state| state.last_applied_sequence)
        .max()
        .unwrap_or(0);
    let projects = snapshot
        .projects
        .iter()
        .filter(|project| project.deleted_at.is_none())
        .map(project_shell)
        .collect::<Vec<_>>();
    let previews = build_conversation_previews(snapshot);
    let threads = snapshot
        .threads
        .iter()
        .filter(|thread| thread.deleted_at.is_none() && (thread.archived_at.is_some()) == archived)
        .map(|thread| thread_shell(thread, snapshot, previews.get(thread.thread_id.as_str())))
        .collect::<Vec<_>>();
    json!({
        "snapshotSequence": sequence,
        "projects": projects,
        "threads": threads,
        "updatedAt": now_iso(),
    })
}

async fn thread_snapshot(engine: &OrchestrationEngine, thread_id: &str) -> RpcResult {
    let snapshot = load_thread_snapshot(&engine.repositories(), thread_id)
        .await
        .map_err(|error| orchestration_error("OrchestrationGetSnapshotError", error))?;
    thread_snapshot_value(&snapshot, thread_id)
}

/// Builds the thread wire value from any snapshot that holds the thread's rows.
fn thread_snapshot_value(snapshot: &crate::orchestration::Snapshot, thread_id: &str) -> RpcResult {
    let thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.thread_id == thread_id && thread.deleted_at.is_none())
        .ok_or_else(|| {
            json!({
                "_tag": "OrchestrationGetSnapshotError",
                "message": format!("Thread {thread_id} was not found"),
            })
        })?;
    let sequence = snapshot
        .states
        .iter()
        .map(|state| state.last_applied_sequence)
        .max()
        .unwrap_or(0);
    let rows = thread_detail_rows(snapshot, thread);
    let detail = thread_detail(thread, &rows);
    Ok(json!({ "snapshotSequence": sequence, "thread": detail }))
}

fn thread_detail(thread: &ProjectionThread, rows: &ThreadDetailRows<'_>) -> Value {
    let mut detail = thread_shell_with_records(thread, rows.latest_turn, rows.session, None);
    let object = detail.as_object_mut().expect("thread shell is an object");
    object.insert("deletedAt".to_owned(), json!(thread.deleted_at));
    object.insert(
        "messages".to_owned(),
        Value::Array(
            rows.messages
                .iter()
                .map(|row| {
                    let mut message = json!({
                        "id": row.message_id,
                        "turnId": row.turn_id,
                        "role": row.role,
                        "text": row.text,
                        "attachments": row.attachments.clone().unwrap_or_else(|| json!([])),
                        "streaming": row.is_streaming,
                        "createdAt": row.created_at,
                        "updatedAt": row.updated_at,
                    });
                    if let (Some(state), Some(provider)) =
                        (&row.delivery_state, &row.delivery_provider)
                    {
                        let mut delivery = json!({"state": state, "provider": provider});
                        if let Some(provider_instance_id) = &row.delivery_provider_instance_id {
                            delivery["providerInstanceId"] = json!(provider_instance_id);
                        }
                        if let Some(mode) = &row.delivery_mode {
                            delivery["mode"] = json!(mode);
                        }
                        if let Some(held) = row.delivery_held {
                            delivery["held"] = json!(held);
                        }
                        if let Some(detail) = &row.delivery_detail {
                            delivery["detail"] = json!(detail);
                        }
                        if let Some(reason) = &row.delivery_reason {
                            delivery["reason"] = json!(reason);
                        }
                        message["delivery"] = delivery;
                    }
                    message
                })
                .collect(),
        ),
    );
    object.insert(
        "activities".to_owned(),
        Value::Array(
            rows.activities
                .iter()
                .map(|row| {
                    json!({
                        "id": row.activity_id,
                        "turnId": row.turn_id,
                        "tone": thread_activity_tone(&row.tone),
                        "kind": row.kind,
                        "summary": row.summary,
                        "payload": row.payload,
                        "sequence": row.sequence,
                        "createdAt": row.created_at,
                    })
                })
                .collect(),
        ),
    );
    object.insert(
        "proposedPlans".to_owned(),
        Value::Array(
            rows.proposed_plans
                .iter()
                .map(|row| {
                    json!({
                        "id": row.plan_id,
                        "turnId": row.turn_id,
                        "planMarkdown": row.plan_markdown,
                        "implementedAt": row.implemented_at,
                        "implementationThreadId": row.implementation_thread_id,
                        "createdAt": row.created_at,
                        "updatedAt": row.updated_at,
                    })
                })
                .collect(),
        ),
    );
    object.insert(
        "checkpoints".to_owned(),
        Value::Array(
            rows.checkpoints
                .iter()
                .map(|row| {
                    json!({
                        "turnId": row.turn_id,
                        "checkpointTurnCount": row.checkpoint_turn_count,
                        "checkpointRef": row.checkpoint_ref,
                        "status": row.status,
                        "files": row.files,
                        "assistantMessageId": row.assistant_message_id,
                        "completedAt": row.completed_at,
                    })
                })
                .collect(),
        ),
    );
    detail
}

#[derive(Default)]
struct ThreadDetailRows<'a> {
    messages: Vec<&'a crate::persistence::ProjectionThreadMessage>,
    activities: Vec<&'a crate::persistence::ProjectionThreadActivity>,
    proposed_plans: Vec<&'a crate::persistence::ProjectionThreadProposedPlan>,
    checkpoints: Vec<&'a crate::orchestration::engine::ProjectionCheckpointRow>,
    session: Option<&'a crate::persistence::ProjectionThreadSession>,
    latest_turn: Option<&'a crate::persistence::ProjectionTurn>,
}

fn thread_detail_rows<'a>(
    snapshot: &'a crate::orchestration::Snapshot,
    thread: &ProjectionThread,
) -> ThreadDetailRows<'a> {
    ThreadDetailRows {
        messages: snapshot
            .messages
            .iter()
            .filter(|row| row.thread_id == thread.thread_id)
            .collect(),
        activities: snapshot
            .activities
            .iter()
            .filter(|row| row.thread_id == thread.thread_id)
            .collect(),
        proposed_plans: snapshot
            .proposed_plans
            .iter()
            .filter(|row| row.thread_id == thread.thread_id)
            .collect(),
        checkpoints: snapshot
            .checkpoints
            .iter()
            .filter(|row| row.thread_id == thread.thread_id)
            .collect(),
        session: snapshot
            .sessions
            .iter()
            .find(|row| row.thread_id == thread.thread_id),
        latest_turn: thread.latest_turn_id.as_ref().and_then(|latest_id| {
            snapshot.turns.iter().find(|row| {
                row.thread_id == thread.thread_id && row.turn_id.as_ref() == Some(latest_id)
            })
        }),
    }
}

/// Borrow each row once; an HTTP snapshot must not reload or rescan the full history per thread.
fn all_thread_detail_rows(
    snapshot: &crate::orchestration::Snapshot,
) -> HashMap<&str, ThreadDetailRows<'_>> {
    let mut rows: HashMap<&str, ThreadDetailRows<'_>> = HashMap::new();
    for row in &snapshot.messages {
        rows.entry(&row.thread_id).or_default().messages.push(row);
    }
    for row in &snapshot.activities {
        rows.entry(&row.thread_id).or_default().activities.push(row);
    }
    for row in &snapshot.proposed_plans {
        rows.entry(&row.thread_id)
            .or_default()
            .proposed_plans
            .push(row);
    }
    for row in &snapshot.checkpoints {
        rows.entry(&row.thread_id)
            .or_default()
            .checkpoints
            .push(row);
    }
    for row in &snapshot.sessions {
        let group = rows.entry(&row.thread_id).or_default();
        if group.session.is_none() {
            group.session = Some(row);
        }
    }
    let latest_ids: HashMap<&str, &str> = snapshot
        .threads
        .iter()
        .filter_map(|thread| {
            thread
                .latest_turn_id
                .as_deref()
                .map(|id| (thread.thread_id.as_str(), id))
        })
        .collect();
    for row in &snapshot.turns {
        if let Some(id) = latest_ids.get(row.thread_id.as_str())
            && row.turn_id.as_deref() == Some(*id)
        {
            let group = rows.entry(&row.thread_id).or_default();
            if group.latest_turn.is_none() {
                group.latest_turn = Some(row);
            }
        }
    }
    rows
}

fn project_shell(project: &crate::persistence::ProjectionProject) -> Value {
    json!({
        "id": project.project_id,
        "title": project.title,
        "workspaceRoot": project.workspace_root,
        "defaultModelSelection": project.default_model_selection,
        "scripts": project.scripts,
        "worktreeDiscovery": project.worktree_discovery,
        "repositoryIdentity": project.repository_identity,
        "createdAt": project.created_at,
        "updatedAt": project.updated_at,
    })
}

/// The already-declared HTTP read model includes archived/deleted records and full thread details.
/// Legacy WS null optionals stay unchanged; HTTP omits only fields declared optional, not nullable.
pub(crate) fn read_model_snapshot(
    snapshot: &crate::orchestration::Snapshot,
    updated_at: &str,
) -> Value {
    let mut grouped = all_thread_detail_rows(snapshot);
    let projects = snapshot
        .projects
        .iter()
        .map(|project| {
            let mut wire = project_shell(project);
            wire["deletedAt"] = json!(project.deleted_at);
            wire
        })
        .collect::<Vec<_>>();
    let threads = snapshot
        .threads
        .iter()
        .map(|thread| {
            let rows = grouped
                .remove(thread.thread_id.as_str())
                .unwrap_or_default();
            let mut wire = thread_detail(thread, &rows);
            if let Some(turn) = wire.get_mut("latestTurn").and_then(Value::as_object_mut)
                && turn.get("sourceProposedPlan").is_some_and(Value::is_null)
            {
                turn.remove("sourceProposedPlan");
            }
            if let Some(session) = wire.get_mut("session").and_then(Value::as_object_mut)
                && session
                    .get("providerInstanceId")
                    .is_some_and(Value::is_null)
            {
                session.remove("providerInstanceId");
            }
            wire
        })
        .collect::<Vec<_>>();
    json!({
        "snapshotSequence": snapshot.states.iter().map(|state| state.last_applied_sequence).max().unwrap_or(0),
        "projects": projects,
        "threads": threads,
        "updatedAt": updated_at,
    })
}

fn thread_activity_tone(tone: &str) -> &str {
    match tone {
        // `warning` must be listed: this whitelist is the last hop before the
        // client, so omitting it silently downgraded provider warnings to
        // `info` and the new tone never arrived.
        "info" | "tool" | "approval" | "warning" | "error" => tone,
        _ => "info",
    }
}

const PREVIEW_PROMPT_MAX_CHARS: usize = 200;
const PREVIEW_TOOL_MAX_CHARS: usize = 160;
const PREVIEW_ASSISTANT_MAX_CHARS: usize = 320;

#[derive(Debug, Default)]
struct ConversationPreview {
    prompt: Option<String>,
    tool: Option<String>,
    assistant_message: Option<String>,
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// One pass over messages and activities so shell building stays
/// O(messages + activities + threads) even though every engine event
/// re-sends the full shell snapshot.
fn build_conversation_previews(
    snapshot: &crate::orchestration::Snapshot,
) -> HashMap<&str, ConversationPreview> {
    let mut previews: HashMap<&str, ConversationPreview> = HashMap::new();
    let mut newest_user: HashMap<&str, &crate::persistence::ProjectionThreadMessage> =
        HashMap::new();
    let mut newest_assistant: HashMap<&str, &crate::persistence::ProjectionThreadMessage> =
        HashMap::new();
    for message in &snapshot.messages {
        let bucket = match message.role.as_str() {
            "user" => &mut newest_user,
            "assistant" => &mut newest_assistant,
            _ => continue,
        };
        match bucket.get(message.thread_id.as_str()) {
            Some(existing) if existing.created_at > message.created_at => {}
            _ => {
                bucket.insert(message.thread_id.as_str(), message);
            }
        }
    }
    let turn_states: HashMap<(&str, &str), &str> = snapshot
        .turns
        .iter()
        .filter_map(|turn| {
            Some((
                (turn.thread_id.as_str(), turn.turn_id.as_deref()?),
                turn.state.as_str(),
            ))
        })
        .collect();
    let running_latest_turn: HashMap<&str, &str> = snapshot
        .threads
        .iter()
        .filter_map(|thread| {
            let latest_id = thread.latest_turn_id.as_deref()?;
            (turn_states.get(&(thread.thread_id.as_str(), latest_id)) == Some(&"running"))
                .then_some((thread.thread_id.as_str(), latest_id))
        })
        .collect();
    let mut newest_tool: HashMap<&str, &crate::persistence::ProjectionThreadActivity> =
        HashMap::new();
    for activity in &snapshot.activities {
        if activity.tone != "tool" {
            continue;
        }
        let Some(latest_id) = running_latest_turn.get(activity.thread_id.as_str()) else {
            continue;
        };
        if activity.turn_id.as_deref() != Some(latest_id) {
            continue;
        }
        match newest_tool.get(activity.thread_id.as_str()) {
            Some(existing) if existing.created_at > activity.created_at => {}
            _ => {
                newest_tool.insert(activity.thread_id.as_str(), activity);
            }
        }
    }
    for (thread_id, message) in newest_user {
        let text = message.text.trim();
        if text.is_empty() {
            continue;
        }
        previews.entry(thread_id).or_default().prompt =
            Some(truncate_preview(text, PREVIEW_PROMPT_MAX_CHARS));
    }
    for (thread_id, message) in newest_assistant {
        let text = message.text.trim();
        if text.is_empty() {
            continue;
        }
        previews.entry(thread_id).or_default().assistant_message =
            Some(truncate_preview(text, PREVIEW_ASSISTANT_MAX_CHARS));
    }
    for (thread_id, activity) in newest_tool {
        let summary = activity.summary.trim();
        if summary.is_empty() {
            continue;
        }
        previews.entry(thread_id).or_default().tool =
            Some(truncate_preview(summary, PREVIEW_TOOL_MAX_CHARS));
    }
    previews
}

fn thread_shell(
    thread: &ProjectionThread,
    snapshot: &crate::orchestration::Snapshot,
    preview: Option<&ConversationPreview>,
) -> Value {
    let latest_turn = thread.latest_turn_id.as_ref().and_then(|latest_id| {
        snapshot.turns.iter().find(|turn| {
            turn.thread_id == thread.thread_id && turn.turn_id.as_ref() == Some(latest_id)
        })
    });
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.thread_id == thread.thread_id);
    thread_shell_with_records(thread, latest_turn, session, preview)
}

fn thread_shell_with_records(
    thread: &ProjectionThread,
    latest_turn: Option<&crate::persistence::ProjectionTurn>,
    session: Option<&crate::persistence::ProjectionThreadSession>,
    preview: Option<&ConversationPreview>,
) -> Value {
    let latest_turn = latest_turn.map(|turn| json!({
                "turnId": turn.turn_id,
                "state": turn.state,
                "requestedAt": turn.requested_at,
                "startedAt": turn.started_at,
                "completedAt": turn.completed_at,
                "assistantMessageId": turn.assistant_message_id,
                "sourceProposedPlan": match (&turn.source_proposed_plan_thread_id, &turn.source_proposed_plan_id) {
                    (Some(thread_id), Some(plan_id)) => Some(json!({ "threadId": thread_id, "planId": plan_id })),
                    _ => None,
                },
            }))
    ;
    let session = session.map(|session| {
        json!({
            "threadId": session.thread_id,
            "status": session.status,
            "providerName": session.provider_name,
            "providerInstanceId": session.provider_instance_id,
            "runtimeMode": session.runtime_mode,
            "activeTurnId": session.active_turn_id,
            "lastError": session.last_error,
            "lastErrorClass": session.last_error_class,
            "updatedAt": session.updated_at,
        })
    });
    let mut shell = json!({
        "id": thread.thread_id,
        "projectId": thread.project_id,
        "title": thread.title,
        "modelSelection": thread.model_selection,
        "runtimeMode": thread.runtime_mode,
        "interactionMode": thread.interaction_mode,
        "kind": thread.kind,
        "branch": thread.branch,
        "worktreePath": thread.worktree_path,
        "latestTurn": latest_turn,
        "createdAt": thread.created_at,
        "updatedAt": thread.updated_at,
        "archivedAt": thread.archived_at,
        "session": session,
        "latestUserMessageAt": thread.latest_user_message_at,
        "hasPendingApprovals": thread.pending_approval_count > 0,
        "hasPendingUserInput": thread.pending_user_input_count > 0,
        "hasActionableProposedPlan": thread.has_actionable_proposed_plan != 0,
        "unresolvedDelivery": thread.unresolved_delivery_state.as_ref().map(|state| {
            json!({ "state": state, "detail": thread.unresolved_delivery_detail })
        }),
    });
    if let Some(host_thread_id) = &thread.host_thread_id {
        shell["hostThreadId"] = json!(host_thread_id);
    }
    if let Some(preview) = preview {
        shell["conversationPreview"] = json!({
            "prompt": preview.prompt,
            "tool": preview.tool,
            "assistantMessage": preview.assistant_message,
        });
    }
    shell
}

pub fn wire_event(row: &OrchestrationEvent) -> Value {
    json!({
        "sequence": row.sequence,
        "eventId": row.event.event_id,
        "type": row.event.event_type,
        "aggregateKind": row.event.aggregate_kind,
        "aggregateId": row.event.aggregate_id,
        "occurredAt": row.event.occurred_at,
        "commandId": row.event.command_id,
        "causationEventId": row.event.causation_event_id,
        "correlationId": row.event.correlation_id,
        "payload": row.event.payload,
        "metadata": row.event.metadata,
    })
}

/// Controls byte limits for paged and whole-tail replay reads.
enum ReplayBudget {
    /// Stops before an event would exceed `target_bytes`, always keeping the first event.
    Page { target_bytes: usize },
    /// Returns the whole tail or fails as soon as counted bytes exceed `limit_bytes`.
    Whole { limit_bytes: usize },
}

#[derive(Debug)]
struct ReplayRead {
    events: Vec<Value>,
    exhausted: bool,
}

#[derive(Debug, thiserror::Error)]
enum ReplayReadError {
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Count(#[from] serde_json::Error),
    #[error("encoded replay JSON is too large to count")]
    CountOverflow,
    #[error("replay events exceed {limit_bytes} bytes ({bytes} bytes read)")]
    TooLarge { bytes: usize, limit_bytes: usize },
}

/// Reads one batch at a time and counts only event JSON, array brackets and commas.
/// Unpaged oversize bytes are the running total at the crossing, a lower bound;
/// the session's size check remains the backstop for the uncounted Exit envelope.
/// Paged reads always retain their first event, even over the target. A single
/// event over the connection's message limit is left to `send_server_message`'s
/// own size check and `RpcResponseTooLargeError`.
async fn read_replay(
    repositories: &Repositories,
    from_sequence_exclusive: i64,
    budget: ReplayBudget,
) -> Result<ReplayRead, ReplayReadError> {
    let mut pages = repositories.event_pages(from_sequence_exclusive);
    let mut events = Vec::new();
    let mut total = 2_usize; // JSON array brackets
    while let Some(batch) = pages.next_page().await? {
        let last_batch = batch.len() < EVENT_PAGE_SIZE;
        for row in batch {
            let event = wire_event(&row);
            let event_bytes = encoded_json_len(&event)?;
            let next_total = total
                .checked_add(event_bytes)
                .and_then(|bytes| bytes.checked_add(usize::from(!events.is_empty())))
                .ok_or(ReplayReadError::CountOverflow)?;
            match budget {
                ReplayBudget::Page { target_bytes }
                    if !events.is_empty() && next_total > target_bytes =>
                {
                    return Ok(ReplayRead {
                        events,
                        exhausted: false,
                    });
                }
                ReplayBudget::Whole { limit_bytes } if next_total > limit_bytes => {
                    return Err(ReplayReadError::TooLarge {
                        bytes: next_total,
                        limit_bytes,
                    });
                }
                _ => {}
            }
            total = next_total;
            events.push(event);
        }
        if last_batch {
            break;
        }
    }
    Ok(ReplayRead {
        events,
        exhausted: true,
    })
}

fn decode<T: for<'de> Deserialize<'de>>(request: RpcRequest) -> Result<T, Value> {
    serde_json::from_value(request.payload)
        .map_err(|error| invalid_request(&request.tag, error.to_string()))
}

fn invalid_request(method: &str, message: impl Into<String>) -> Value {
    json!({ "_tag": "InvalidRequest", "method": method, "message": message.into() })
}

fn orchestration_error(tag: &str, error: impl std::fmt::Display) -> Value {
    json!({ "_tag": tag, "message": error.to_string() })
}

fn workspace_admission_error(error: WorkspaceAdmissionError) -> Value {
    match error {
        WorkspaceAdmissionError::Unavailable(error) => {
            serde_json::to_value(error).expect("workspace unavailable error serializes")
        }
        WorkspaceAdmissionError::Identity(error) => {
            serde_json::to_value(error).expect("workspace identity error serializes")
        }
        WorkspaceAdmissionError::Resolution(error) => {
            orchestration_error("OrchestrationDispatchCommandError", error)
        }
    }
}

fn provider_command_error(error: impl std::fmt::Display) -> Value {
    orchestration_error("OrchestrationDispatchCommandError", error)
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayInput {
    from_sequence_exclusive: i64,
    #[serde(default)]
    paged: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscribeThreadInput {
    thread_id: String,
    /// The client applies `patch` items; without it every update is a snapshot.
    #[serde(default)]
    patches: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnDiffInput {
    thread_id: String,
    from_turn_count: i64,
    to_turn_count: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FullDiffInput {
    thread_id: String,
    to_turn_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #101: a subscriber keeps fingerprints; a number that changes representation, but not
    /// its bits, is still a change.
    #[test]
    fn a_patch_sees_a_number_change_its_representation() {
        let held = json!({ "snapshotSequence": 1, "thread": { "activities": [
            { "id": "a", "payload": { "n": 4_609_434_218_613_702_656_i64 } },
        ] } });
        let next = json!({ "snapshotSequence": 2, "thread": { "activities": [
            { "id": "a", "payload": { "n": 1.5 } },
        ] } });
        let patch = thread_patch(
            &HeldThread::of(&held, std::hash::RandomState::new()).expect("a thread"),
            &next,
        )
        .expect("the number changed");
        assert_eq!(
            patch["patch"]["arrays"]["activities"]["changes"],
            json!([[0, next["thread"]["activities"][0]]])
        );
        assert_eq!(
            thread_patch(
                &HeldThread::of(&next, std::hash::RandomState::new()).expect("a thread"),
                &next
            ),
            None
        );
    }
    fn test_upload_registry() -> UploadRegistry {
        UploadRegistry::new(
            std::env::temp_dir().join(format!("bibcode-test-uploads-{}", uuid::Uuid::new_v4())),
            crate::transfer::staging::UploadLimits::default(),
            Arc::new(tokio::time::Instant::now),
        )
    }

    use crate::{
        RequestId, RpcExit, ServerConfig, ServerMessage, ServerRuntime,
        activity::{ActivityProjection, ActivityRepository},
        orchestration::{
            AttachmentReference, NewProviderTurnDelivery, Snapshot, TurnDeliveryState,
            TurnDeliveryTransition,
            engine::{EngineOptions, TestHooks},
            load_snapshot,
        },
        persistence::{
            Database, NewOrchestrationEvent, ProjectionThreadActivity, ProjectionThreadMessage,
            ProjectionTurn, run_migrations,
        },
        production::{
            provider_runtime::{
                BoxRuntimeFuture, ProviderDriver, ProviderDriverFactory, ProviderEvent,
                ProviderLaunchRequest, ProviderRuntimeError, StartedSession, SupervisorOptions,
            },
            turn_delivery::DeliveryRouter,
        },
        provider::attachments::AttachmentPrepareTestPause,
        worktree_catalog::{
            AdoptedWorktreeAvailability, WorkspaceAvailabilityRegistry, WorkspaceLossTransition,
        },
    };
    use futures_util::SinkExt;
    use std::sync::atomic::AtomicUsize;
    use tokio_tungstenite::tungstenite::Message;

    use crate::test_support::websocket_frames::next_frame_past_heartbeat;

    const CREATED_AT: &str = "2026-07-11T00:00:00.000Z";

    fn preview_snapshot_fixture(latest_turn_state: &str) -> Snapshot {
        Snapshot {
            projects: Vec::new(),
            threads: vec![ProjectionThread {
                thread_id: "thread-1".to_owned(),
                project_id: "project-1".to_owned(),
                title: "Preview thread".to_owned(),
                kind: "default".to_owned(),
                model_selection: json!({"instanceId": "codex", "model": "gpt-5"}),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                branch: None,
                worktree_path: None,
                latest_turn_id: Some("turn-2".to_owned()),
                created_at: "2026-07-11T00:00:00.000Z".to_owned(),
                updated_at: "2026-07-11T00:00:00.007Z".to_owned(),
                archived_at: None,
                latest_user_message_at: Some("2026-07-11T00:00:00.004Z".to_owned()),
                pending_approval_count: 0,
                pending_user_input_count: 0,
                has_actionable_proposed_plan: 0,
                unresolved_delivery_state: None,
                unresolved_delivery_detail: None,
                deleted_at: None,
                host_thread_id: None,
            }],
            messages: vec![
                ProjectionThreadMessage {
                    message_id: "message-user-older".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    turn_id: Some("turn-1".to_owned()),
                    role: "user".to_owned(),
                    text: "older prompt".to_owned(),
                    attachments: None,
                    is_streaming: false,
                    delivery_state: None,
                    delivery_provider: None,
                    delivery_provider_instance_id: None,
                    delivery_detail: None,
                    delivery_reason: None,
                    delivery_mode: None,
                    delivery_held: None,
                    created_at: "2026-07-11T00:00:00.000Z".to_owned(),
                    updated_at: "2026-07-11T00:00:00.000Z".to_owned(),
                },
                ProjectionThreadMessage {
                    message_id: "message-assistant-older".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    turn_id: Some("turn-1".to_owned()),
                    role: "assistant".to_owned(),
                    text: "older assistant".to_owned(),
                    attachments: None,
                    is_streaming: false,
                    delivery_state: None,
                    delivery_provider: None,
                    delivery_provider_instance_id: None,
                    delivery_detail: None,
                    delivery_reason: None,
                    delivery_mode: None,
                    delivery_held: None,
                    created_at: "2026-07-11T00:00:00.001Z".to_owned(),
                    updated_at: "2026-07-11T00:00:00.001Z".to_owned(),
                },
                ProjectionThreadMessage {
                    message_id: "message-user-newer".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    turn_id: Some("turn-2".to_owned()),
                    role: "user".to_owned(),
                    text: "newest user prompt".to_owned(),
                    attachments: None,
                    is_streaming: false,
                    delivery_state: None,
                    delivery_provider: None,
                    delivery_provider_instance_id: None,
                    delivery_detail: None,
                    delivery_reason: None,
                    delivery_mode: None,
                    delivery_held: None,
                    created_at: "2026-07-11T00:00:00.002Z".to_owned(),
                    updated_at: "2026-07-11T00:00:00.002Z".to_owned(),
                },
                ProjectionThreadMessage {
                    message_id: "message-assistant-newer".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    turn_id: Some("turn-2".to_owned()),
                    role: "assistant".to_owned(),
                    text: "newest assistant text".to_owned(),
                    attachments: None,
                    is_streaming: false,
                    delivery_state: None,
                    delivery_provider: None,
                    delivery_provider_instance_id: None,
                    delivery_detail: None,
                    delivery_reason: None,
                    delivery_mode: None,
                    delivery_held: None,
                    created_at: "2026-07-11T00:00:00.003Z".to_owned(),
                    updated_at: "2026-07-11T00:00:00.003Z".to_owned(),
                },
                ProjectionThreadMessage {
                    message_id: "message-user-newest".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    turn_id: Some("turn-2".to_owned()),
                    role: "user".to_owned(),
                    text: "  padded prompt \n".to_owned(),
                    attachments: None,
                    is_streaming: false,
                    delivery_state: None,
                    delivery_provider: None,
                    delivery_provider_instance_id: None,
                    delivery_detail: None,
                    delivery_reason: None,
                    delivery_mode: None,
                    delivery_held: None,
                    created_at: "2026-07-11T00:00:00.004Z".to_owned(),
                    updated_at: "2026-07-11T00:00:00.004Z".to_owned(),
                },
                ProjectionThreadMessage {
                    message_id: "message-assistant-newest".to_owned(),
                    thread_id: "thread-1".to_owned(),
                    turn_id: Some("turn-2".to_owned()),
                    role: "assistant".to_owned(),
                    text: " \n\t".to_owned(),
                    attachments: None,
                    is_streaming: true,
                    delivery_state: None,
                    delivery_provider: None,
                    delivery_provider_instance_id: None,
                    delivery_detail: None,
                    delivery_reason: None,
                    delivery_mode: None,
                    delivery_held: None,
                    created_at: "2026-07-11T00:00:00.005Z".to_owned(),
                    updated_at: "2026-07-11T00:00:00.005Z".to_owned(),
                },
            ],
            activities: vec![ProjectionThreadActivity {
                activity_id: "activity-tool".to_owned(),
                thread_id: "thread-1".to_owned(),
                turn_id: Some("turn-2".to_owned()),
                tone: "tool".to_owned(),
                kind: "tool".to_owned(),
                summary: "Edit: src/main.rs".to_owned(),
                payload: json!({}),
                sequence: Some(1),
                created_at: "2026-07-11T00:00:00.006Z".to_owned(),
            }],
            sessions: Vec::new(),
            approvals: Vec::new(),
            proposed_plans: Vec::new(),
            turns: vec![ProjectionTurn {
                thread_id: "thread-1".to_owned(),
                turn_id: Some("turn-2".to_owned()),
                pending_message_id: None,
                source_proposed_plan_thread_id: None,
                source_proposed_plan_id: None,
                assistant_message_id: None,
                state: latest_turn_state.to_owned(),
                requested_at: "2026-07-11T00:00:00.002Z".to_owned(),
                started_at: Some("2026-07-11T00:00:00.003Z".to_owned()),
                completed_at: None,
                checkpoint_turn_count: None,
                checkpoint_ref: None,
                checkpoint_status: None,
                checkpoint_files: json!([]),
            }],
            checkpoints: Vec::new(),
            states: Vec::new(),
            receipts: Vec::new(),
            diffs: Vec::new(),
        }
    }

    #[test]
    fn conversation_preview_truncates_on_char_boundary() {
        let text = "é".repeat(300);
        let truncated = truncate_preview(&text, 200);
        assert_eq!(truncated.chars().count(), 201);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate_preview("short", 200), "short");
    }

    #[test]
    fn conversation_preview_picks_newest_rows_and_gates_tool_on_running_turn() {
        let snapshot = preview_snapshot_fixture("running");
        let previews = build_conversation_previews(&snapshot);
        let preview = previews.get("thread-1").expect("preview for thread-1");
        assert_eq!(preview.prompt.as_deref(), Some("padded prompt"));
        assert_eq!(preview.assistant_message, None);
        assert_eq!(preview.tool.as_deref(), Some("Edit: src/main.rs"));

        let done = preview_snapshot_fixture("completed");
        let previews = build_conversation_previews(&done);
        assert_eq!(previews.get("thread-1").expect("preview").tool, None);
    }

    #[test]
    fn thread_shell_embeds_preview_and_detail_omits_it() {
        let snapshot = preview_snapshot_fixture("running");
        let previews = build_conversation_previews(&snapshot);
        let thread = &snapshot.threads[0];
        let with = thread_shell(thread, &snapshot, previews.get(thread.thread_id.as_str()));
        assert_eq!(
            with["conversationPreview"]["prompt"],
            serde_json::json!("padded prompt")
        );
        let without = thread_shell(thread, &snapshot, None);
        assert!(without.get("conversationPreview").is_none());
    }

    fn store_thread(
        thread_id: &str,
        project_id: &str,
        latest_turn_id: Option<&str>,
    ) -> ProjectionThread {
        let mut thread = preview_snapshot_fixture("running").threads.remove(0);
        thread.thread_id = thread_id.to_owned();
        thread.project_id = project_id.to_owned();
        thread.title = thread_id.to_owned();
        thread.latest_turn_id = latest_turn_id.map(str::to_owned);
        thread
    }

    fn store_message(
        message_id: &str,
        thread_id: &str,
        role: &str,
        text: &str,
        created_at: &str,
    ) -> ProjectionThreadMessage {
        ProjectionThreadMessage {
            message_id: message_id.to_owned(),
            thread_id: thread_id.to_owned(),
            turn_id: None,
            role: role.to_owned(),
            text: text.to_owned(),
            attachments: None,
            is_streaming: false,
            delivery_state: None,
            delivery_provider: None,
            delivery_provider_instance_id: None,
            delivery_detail: None,
            delivery_reason: None,
            delivery_mode: None,
            delivery_held: None,
            created_at: created_at.to_owned(),
            updated_at: created_at.to_owned(),
        }
    }

    fn store_activity(
        activity_id: &str,
        thread_id: &str,
        turn_id: &str,
        tone: &str,
        sequence: Option<i64>,
        created_at: &str,
    ) -> ProjectionThreadActivity {
        ProjectionThreadActivity {
            activity_id: activity_id.to_owned(),
            thread_id: thread_id.to_owned(),
            turn_id: Some(turn_id.to_owned()),
            tone: tone.to_owned(),
            kind: "tool.completed".to_owned(),
            summary: format!("summary {activity_id}"),
            payload: json!({ "activity": activity_id }),
            sequence,
            created_at: created_at.to_owned(),
        }
    }

    fn store_turn(
        thread_id: &str,
        turn_id: &str,
        state: &str,
    ) -> crate::persistence::ProjectionTurnById {
        crate::persistence::ProjectionTurnById {
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            pending_message_id: None,
            source_proposed_plan_thread_id: None,
            source_proposed_plan_id: None,
            assistant_message_id: None,
            state: state.to_owned(),
            requested_at: "2026-07-11T00:00:01.000Z".to_owned(),
            started_at: Some("2026-07-11T00:00:01.000Z".to_owned()),
            completed_at: None,
            checkpoint_turn_count: None,
            checkpoint_ref: None,
            checkpoint_status: None,
            checkpoint_files: json!([]),
        }
    }

    /// A store with every shape the shell and thread snapshots distinguish: live, archived,
    /// deleted, panel and project-less threads, a deleted project, tied timestamps, queued
    /// messages, a running turn with tool activities, and history the shell must not read.
    async fn seeded_snapshot_store() -> Repositories {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let repositories = Repositories::new(database);
        let mut fixture = http_read_model_fixture();
        for project in fixture.projects.drain(..) {
            repositories.upsert_project(project).await.unwrap();
        }
        let mut archived = store_thread("t-archived", "project-live", Some("turn-archived"));
        archived.archived_at = Some(CREATED_AT.to_owned());
        let mut deleted = store_thread("t-deleted", "project-live", None);
        deleted.deleted_at = Some(CREATED_AT.to_owned());
        let mut panel = store_thread("t-panel", "project-live", None);
        panel.kind = "panel".to_owned();
        panel.host_thread_id = Some("t-running".to_owned());
        for thread in [
            store_thread("t-running", "project-live", Some("turn-run")),
            store_thread("t-ties", "project-live", Some("turn-done")),
            store_thread("t-under-deleted-project", "project-deleted", None),
            store_thread("t-orphan", "project-missing", None),
            archived,
            deleted,
            panel,
        ] {
            repositories.upsert_thread(thread).await.unwrap();
        }
        let mut queued = store_message(
            "m-queued",
            "t-running",
            "user",
            "queued prompt",
            "2026-07-11T00:00:05.000Z",
        );
        queued.delivery_state = Some("queued".to_owned());
        let messages = [
            store_message(
                "m-old",
                "t-running",
                "user",
                "older prompt",
                "2026-07-11T00:00:02.000Z",
            ),
            store_message(
                "m-tie-a",
                "t-running",
                "user",
                "tied prompt",
                "2026-07-11T00:00:05.000Z",
            ),
            queued,
            store_message(
                "m-reply",
                "t-running",
                "assistant",
                "an answer",
                "2026-07-11T00:00:03.000Z",
            ),
            store_message(
                "m-blank",
                "t-running",
                "assistant",
                "   ",
                "2026-07-11T00:00:04.000Z",
            ),
            store_message("m-b", "t-ties", "user", "bravo", "2026-07-11T00:00:05.000Z"),
            store_message("m-a", "t-ties", "user", "alpha", "2026-07-11T00:00:05.000Z"),
            store_message(
                "m-system",
                "t-ties",
                "system",
                "ignored",
                "2026-07-11T00:00:09.000Z",
            ),
            store_message(
                "m-archived",
                "t-archived",
                "assistant",
                "archived answer",
                "2026-07-11T00:00:05.000Z",
            ),
            store_message(
                "m-deleted",
                "t-deleted",
                "user",
                "deleted prompt",
                "2026-07-11T00:00:05.000Z",
            ),
        ];
        for message in messages {
            repositories.upsert_message(message).await.unwrap();
        }
        let mut activities = vec![
            // Tied newest tool rows: the one later in sequence order wins.
            store_activity(
                "act-y",
                "t-running",
                "turn-run",
                "tool",
                Some(5),
                "2026-07-11T00:00:09.000Z",
            ),
            store_activity(
                "act-x",
                "t-running",
                "turn-run",
                "tool",
                Some(3),
                "2026-07-11T00:00:09.000Z",
            ),
            store_activity(
                "act-null",
                "t-running",
                "turn-run",
                "tool",
                None,
                "2026-07-11T00:00:09.000Z",
            ),
            store_activity(
                "act-info",
                "t-running",
                "turn-run",
                "info",
                Some(6),
                "2026-07-11T00:00:10.000Z",
            ),
            store_activity(
                "act-old-turn",
                "t-running",
                "turn-old",
                "tool",
                Some(7),
                "2026-07-11T00:00:11.000Z",
            ),
            store_activity(
                "act-done",
                "t-ties",
                "turn-done",
                "tool",
                Some(1),
                "2026-07-11T00:00:09.000Z",
            ),
        ];
        activities.extend((0..40).map(|index| {
            store_activity(
                &format!("history-{index:02}"),
                if index % 2 == 0 {
                    "t-running"
                } else {
                    "t-archived"
                },
                "turn-history",
                "tool",
                Some(100 + index),
                "2026-07-11T00:00:01.000Z",
            )
        }));
        for activity in activities {
            repositories.upsert_activity(activity).await.unwrap();
        }
        for (thread_id, turn_id, state) in [
            ("t-running", "turn-run", "running"),
            ("t-running", "turn-old", "completed"),
            ("t-ties", "turn-done", "completed"),
            ("t-archived", "turn-archived", "running"),
        ] {
            repositories
                .upsert_turn_by_id(store_turn(thread_id, turn_id, state))
                .await
                .unwrap();
        }
        repositories
            .upsert_checkpoint(crate::persistence::ProjectionCheckpoint {
                thread_id: "t-ties".to_owned(),
                turn_id: "turn-done".to_owned(),
                checkpoint_turn_count: 1,
                checkpoint_ref: "refs/checkpoint/1".to_owned(),
                status: "ready".to_owned(),
                files: json!([]),
                assistant_message_id: None,
                completed_at: "2026-07-11T00:00:06.000Z".to_owned(),
            })
            .await
            .unwrap();
        repositories
            .upsert_proposed_plan(crate::persistence::ProjectionThreadProposedPlan {
                plan_id: "plan-1".to_owned(),
                thread_id: "t-running".to_owned(),
                turn_id: Some("turn-old".to_owned()),
                plan_markdown: "# Plan".to_owned(),
                implemented_at: None,
                implementation_thread_id: None,
                created_at: CREATED_AT.to_owned(),
                updated_at: CREATED_AT.to_owned(),
            })
            .await
            .unwrap();
        for thread_id in ["t-running", "t-archived"] {
            repositories
                .upsert_thread_session(crate::persistence::ProjectionThreadSession {
                    thread_id: thread_id.to_owned(),
                    status: "running".to_owned(),
                    provider_name: Some("codex".to_owned()),
                    provider_instance_id: Some("codex".to_owned()),
                    runtime_mode: "full-access".to_owned(),
                    active_turn_id: Some("turn-run".to_owned()),
                    last_error: None,
                    last_error_class: None,
                    updated_at: CREATED_AT.to_owned(),
                })
                .await
                .unwrap();
        }
        for (projector, sequence) in [("projector-a", 7), ("projector-b", 9)] {
            repositories
                .upsert_projection_state(crate::persistence::ProjectionState {
                    projector: projector.to_owned(),
                    last_applied_sequence: sequence,
                    updated_at: CREATED_AT.to_owned(),
                })
                .await
                .unwrap();
        }
        repositories
    }

    fn without_updated_at(mut value: Value) -> Value {
        value
            .as_object_mut()
            .expect("snapshot object")
            .remove("updatedAt");
        value
    }

    #[tokio::test]
    async fn targeted_snapshots_match_the_full_store_load_without_reading_history() {
        let repositories = seeded_snapshot_store().await;
        let full = load_snapshot(&repositories).await.unwrap();
        assert_eq!(full.activities.len(), 46, "fixture history is in the store");

        for archived in [false, true] {
            let targeted = load_shell_snapshot(&repositories, archived).await.unwrap();
            assert_eq!(
                without_updated_at(shell_snapshot_value(&targeted, archived)),
                without_updated_at(shell_snapshot_value(&full, archived)),
                "archived = {archived}"
            );
            // Only preview candidates are read: every row tied for the newest user and
            // assistant message of a returned thread, and for the newest tool activity of
            // a running latest turn. History, plans and checkpoints stay unread.
            let mut message_ids = targeted
                .messages
                .iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>();
            message_ids.sort_unstable();
            let mut activity_ids = targeted
                .activities
                .iter()
                .map(|row| row.activity_id.as_str())
                .collect::<Vec<_>>();
            activity_ids.sort_unstable();
            let (expected_messages, expected_activities): (&[&str], &[&str]) = if archived {
                (&["m-archived"], &[])
            } else {
                (
                    &["m-a", "m-b", "m-blank", "m-queued", "m-tie-a"],
                    &["act-null", "act-x", "act-y"],
                )
            };
            assert_eq!(message_ids, expected_messages, "archived = {archived}");
            assert_eq!(activity_ids, expected_activities, "archived = {archived}");
            assert!(targeted.proposed_plans.is_empty() && targeted.checkpoints.is_empty());
            assert!(targeted.approvals.is_empty() && targeted.receipts.is_empty());
            assert!(targeted.diffs.is_empty());
        }
        let live = shell_snapshot_value(
            &load_shell_snapshot(&repositories, false).await.unwrap(),
            false,
        );
        let running = live["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|thread| thread["id"] == "t-running")
            .unwrap();
        assert_eq!(running["conversationPreview"]["prompt"], "queued prompt");
        assert_eq!(running["conversationPreview"]["tool"], "summary act-y");
        assert_eq!(
            running["conversationPreview"]["assistantMessage"],
            Value::Null
        );
        assert_eq!(live["snapshotSequence"], 9);

        for thread_id in [
            "t-running",
            "t-ties",
            "t-archived",
            "t-deleted",
            "t-panel",
            "t-under-deleted-project",
            "t-orphan",
            "t-missing",
        ] {
            let targeted = load_thread_snapshot(&repositories, thread_id)
                .await
                .unwrap();
            assert_eq!(
                thread_snapshot_value(&targeted, thread_id),
                thread_snapshot_value(&full, thread_id),
                "thread {thread_id}"
            );
        }
        // A view of a deleted thread keeps asking for it; its history stays unread (#113).
        let deleted = load_thread_snapshot(&repositories, "t-deleted")
            .await
            .unwrap();
        assert!(deleted.threads.is_empty() && deleted.messages.is_empty());
    }

    /// Bytes this process has passed to `write(2)` so far: SQLite temporary files included.
    fn written_bytes() -> u64 {
        std::fs::read_to_string("/proc/self/io")
            .ok()
            .and_then(|io| {
                io.lines()
                    .find_map(|line| line.strip_prefix("wchar: ")?.trim().parse().ok())
            })
            .unwrap_or(0)
    }

    async fn timed<F: std::future::Future<Output = Value>>(
        label: &str,
        run: impl Fn() -> F,
    ) -> Value {
        let mut samples = Vec::new();
        let mut written = 0;
        let mut value = Value::Null;
        for _ in 0..3 {
            let before = written_bytes();
            let started = std::time::Instant::now();
            value = run().await;
            samples.push(started.elapsed());
            written = written.max(written_bytes().saturating_sub(before));
        }
        samples.sort();
        println!(
            "{label}: median {:?} (min {:?}, max {:?}), up to {:.1} MB written",
            samples[1],
            samples[0],
            samples[2],
            written as f64 / 1e6
        );
        value
    }

    /// #78 timings on a synthetic store shaped like the reporting machine's: 15 projects,
    /// 50 threads (2 archived), 1,300 messages and 170,000 activities of about 1.5 KB.
    /// `BIBCODE_ISSUE78_BENCH_DIR` sets where the database file goes; run with
    /// `cargo test -p bibcode-server --lib shell_snapshot_timing -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "builds a store of about 300 MB; run by hand for timings"]
    async fn shell_snapshot_timing_on_a_large_store() {
        let directory = std::env::var_os("BIBCODE_ISSUE78_BENCH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("issue-78-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database = Database::create_new(directory.join("state.sqlite"))
            .await
            .unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                connection.execute_batch("PRAGMA synchronous = OFF; BEGIN")?;
                Ok(())
            })
            .await
            .unwrap();
        let repositories = Repositories::new(database.clone());
        let mut fixture = http_read_model_fixture();
        let template = fixture.projects.remove(0);
        for project in 0..15 {
            let mut row = template.clone();
            row.project_id = format!("project-{project:02}");
            row.workspace_root = format!("/bench/{project:02}");
            repositories.upsert_project(row).await.unwrap();
        }
        let payload = "x".repeat(1_500);
        let mut activity = 0_i64;
        for thread in 0..50 {
            let thread_id = format!("thread-{thread:02}");
            let mut row = store_thread(
                &thread_id,
                &format!("project-{:02}", thread % 15),
                Some("turn-12"),
            );
            if thread >= 48 {
                row.archived_at = Some(CREATED_AT.to_owned());
            }
            repositories.upsert_thread(row).await.unwrap();
            for turn in 0..13 {
                let state = if thread == 0 && turn == 12 {
                    "running"
                } else {
                    "completed"
                };
                repositories
                    .upsert_turn_by_id(store_turn(&thread_id, &format!("turn-{turn:02}"), state))
                    .await
                    .unwrap();
            }
            for message in 0..26 {
                let role = if message % 2 == 0 {
                    "user"
                } else {
                    "assistant"
                };
                let created_at =
                    format!("2026-07-11T00:{:02}:{:02}.000Z", message / 60, message % 60);
                repositories
                    .upsert_message(store_message(
                        &format!("{thread_id}-message-{message:02}"),
                        &thread_id,
                        role,
                        &format!("message {message} of {thread_id}"),
                        &created_at,
                    ))
                    .await
                    .unwrap();
            }
            for index in 0..3_400 {
                activity += 1;
                let mut row = store_activity(
                    &format!("{thread_id}-activity-{index:04}"),
                    &thread_id,
                    &format!("turn-{:02}", index * 13 / 3_400),
                    if index % 3 == 0 { "tool" } else { "info" },
                    Some(activity),
                    &format!(
                        "2026-07-11T01:{:02}:{:02}.{:03}Z",
                        index / 3_600,
                        (index / 60) % 60,
                        index % 1_000
                    ),
                );
                row.payload = json!({ "detail": payload, "index": index });
                repositories.upsert_activity(row).await.unwrap();
            }
            repositories
                .upsert_thread_session(crate::persistence::ProjectionThreadSession {
                    thread_id: thread_id.clone(),
                    status: "ready".to_owned(),
                    provider_name: Some("codex".to_owned()),
                    provider_instance_id: Some("codex".to_owned()),
                    runtime_mode: "full-access".to_owned(),
                    active_turn_id: None,
                    last_error: None,
                    last_error_class: None,
                    updated_at: CREATED_AT.to_owned(),
                })
                .await
                .unwrap();
        }
        repositories
            .upsert_projection_state(crate::persistence::ProjectionState {
                projector: "bench".to_owned(),
                last_applied_sequence: activity,
                updated_at: CREATED_AT.to_owned(),
            })
            .await
            .unwrap();
        database
            .call(|connection| {
                connection.execute_batch(
                    "COMMIT; PRAGMA synchronous = FULL; PRAGMA wal_checkpoint(TRUNCATE)",
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let size = std::fs::metadata(directory.join("state.sqlite"))
            .unwrap()
            .len();
        println!(
            "store: 50 threads, {activity} activities, {:.0} MB",
            size as f64 / 1e6
        );

        for archived in [true, false] {
            let label = if archived {
                "getArchivedShellSnapshot"
            } else {
                "subscribeShell snapshot"
            };
            let legacy = timed(&format!("{label}, full load"), || async {
                shell_snapshot_value(&load_snapshot(&repositories).await.unwrap(), archived)
            })
            .await;
            let targeted = timed(&format!("{label}, targeted load"), || async {
                shell_snapshot_value(
                    &load_shell_snapshot(&repositories, archived).await.unwrap(),
                    archived,
                )
            })
            .await;
            assert_eq!(without_updated_at(targeted), without_updated_at(legacy));
        }
        let legacy = timed("subscribeThread snapshot, full load", || async {
            thread_snapshot_value(&load_snapshot(&repositories).await.unwrap(), "thread-00")
                .unwrap()
        })
        .await;
        let targeted = timed("subscribeThread snapshot, targeted load", || async {
            thread_snapshot_value(
                &load_thread_snapshot(&repositories, "thread-00")
                    .await
                    .unwrap(),
                "thread-00",
            )
            .unwrap()
        })
        .await;
        assert_eq!(targeted, legacy);
        database.close().await;
        std::fs::remove_dir_all(&directory).unwrap();
    }

    fn http_read_model_fixture() -> Snapshot {
        let mut snapshot = preview_snapshot_fixture("completed");
        let mut live = snapshot.threads.remove(0);
        live.thread_id = "thread-live".to_owned();
        live.project_id = "project-live".to_owned();
        live.title = "thread-live".to_owned();
        live.latest_turn_id = Some("turn-live".to_owned());
        live.created_at = CREATED_AT.to_owned();
        live.updated_at = CREATED_AT.to_owned();
        live.latest_user_message_at = Some(CREATED_AT.to_owned());
        let mut archived = live.clone();
        archived.thread_id = "thread-archived".to_owned();
        archived.title = "thread-archived".to_owned();
        archived.kind = "workspace".to_owned();
        archived.archived_at = Some(CREATED_AT.to_owned());
        archived.latest_turn_id = None;
        archived.latest_user_message_at = None;
        let mut deleted = archived.clone();
        deleted.thread_id = "thread-deleted".to_owned();
        deleted.project_id = "project-deleted".to_owned();
        deleted.title = "thread-deleted".to_owned();
        deleted.archived_at = None;
        deleted.deleted_at = Some(CREATED_AT.to_owned());
        snapshot.threads = vec![live, archived, deleted];
        let project = crate::persistence::ProjectionProject {
            project_id: "project-live".to_owned(),
            title: "Owned live".to_owned(),
            workspace_root: "/owned/live".to_owned(),
            default_model_selection: None,
            scripts: json!([]),
            worktree_discovery: json!({ "visibility": "hidden", "initialPromptDismissedAt": null, "baselinePaths": [] }),
            worktree_repository_key: None,
            created_at: CREATED_AT.to_owned(),
            updated_at: CREATED_AT.to_owned(),
            deleted_at: None,
            repository_identity: None,
        };
        let mut deleted_project = project.clone();
        deleted_project.project_id = "project-deleted".to_owned();
        deleted_project.title = "Owned deleted".to_owned();
        deleted_project.workspace_root = "/owned/deleted".to_owned();
        deleted_project.deleted_at = Some(CREATED_AT.to_owned());
        snapshot.projects = vec![project, deleted_project];
        let mut message = snapshot.messages.remove(0);
        message.message_id = "message-live".to_owned();
        message.thread_id = "thread-live".to_owned();
        message.turn_id = Some("turn-live".to_owned());
        message.text = "Owned message".to_owned();
        let mut deleted_message = message.clone();
        deleted_message.message_id = "message-deleted".to_owned();
        deleted_message.thread_id = "thread-deleted".to_owned();
        deleted_message.turn_id = None;
        deleted_message.role = "assistant".to_owned();
        deleted_message.text = "Owned deleted message".to_owned();
        snapshot.messages = vec![message, deleted_message];
        let mut activity = snapshot.activities.remove(0);
        activity.activity_id = "activity-live".to_owned();
        activity.thread_id = "thread-live".to_owned();
        activity.turn_id = Some("turn-live".to_owned());
        activity.tone = "warning".to_owned();
        activity.kind = "status".to_owned();
        activity.summary = "Owned status".to_owned();
        activity.created_at = CREATED_AT.to_owned();
        snapshot.activities = vec![activity];
        let mut turn = snapshot.turns.remove(0);
        turn.thread_id = "thread-live".to_owned();
        turn.turn_id = Some("turn-live".to_owned());
        turn.requested_at = CREATED_AT.to_owned();
        turn.started_at = Some(CREATED_AT.to_owned());
        turn.completed_at = Some(CREATED_AT.to_owned());
        snapshot.turns = vec![turn];
        snapshot.sessions = vec![crate::persistence::ProjectionThreadSession {
            thread_id: "thread-live".to_owned(),
            status: "ready".to_owned(),
            provider_name: Some("codex".to_owned()),
            provider_instance_id: None,
            runtime_mode: "full-access".to_owned(),
            active_turn_id: None,
            last_error: None,
            last_error_class: None,
            updated_at: CREATED_AT.to_owned(),
        }];
        snapshot.proposed_plans = vec![crate::persistence::ProjectionThreadProposedPlan {
            plan_id: "plan-live".to_owned(),
            thread_id: "thread-live".to_owned(),
            turn_id: Some("turn-live".to_owned()),
            plan_markdown: "Owned plan".to_owned(),
            implemented_at: None,
            implementation_thread_id: None,
            created_at: CREATED_AT.to_owned(),
            updated_at: CREATED_AT.to_owned(),
        }];
        snapshot.checkpoints = vec![crate::orchestration::engine::ProjectionCheckpointRow {
            thread_id: "thread-live".to_owned(),
            turn_id: "turn-live".to_owned(),
            checkpoint_turn_count: 1,
            checkpoint_ref: "refs/checkpoints/owned".to_owned(),
            status: "ready".to_owned(),
            files: json!([]),
            assistant_message_id: None,
            completed_at: CREATED_AT.to_owned(),
        }];
        snapshot.states = vec![crate::persistence::ProjectionState {
            projector: "owned".to_owned(),
            last_applied_sequence: 42,
            updated_at: CREATED_AT.to_owned(),
        }];
        snapshot
    }

    #[test]
    fn http_read_model_matches_public_fixture_and_preserves_ws_detail() {
        let snapshot = http_read_model_fixture();
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../packages/contracts/fixtures/http-orchestration/full-read-model.json"
        ))
        .expect("public full-read-model fixture");
        let model = read_model_snapshot(&snapshot, CREATED_AT);
        assert_eq!(model, expected);
        assert!(model.get("states").is_none());
        assert!(model.get("receipts").is_none());
        let thread = &snapshot.threads[0];
        let ws_detail = thread_detail(thread, &thread_detail_rows(&snapshot, thread));
        let mut expected_ws = expected["threads"][0].clone();
        expected_ws["latestTurn"]["sourceProposedPlan"] = Value::Null;
        expected_ws["session"]["providerInstanceId"] = Value::Null;
        assert_eq!(ws_detail, expected_ws);
        let shell = thread_shell(thread, &snapshot, None);
        let mut detail_shell = ws_detail;
        for field in [
            "deletedAt",
            "messages",
            "activities",
            "proposedPlans",
            "checkpoints",
        ] {
            detail_shell
                .as_object_mut()
                .expect("detail object")
                .remove(field);
        }
        assert_eq!(shell, detail_shell);
    }

    #[test]
    fn http_read_model_keeps_present_optionals_and_first_matching_ws_rows() {
        let mut snapshot = http_read_model_fixture();
        snapshot.sessions[0].provider_instance_id = Some("codex-custom".to_owned());
        snapshot.turns[0].source_proposed_plan_thread_id = Some("thread-source".to_owned());
        snapshot.turns[0].source_proposed_plan_id = Some("plan-source".to_owned());
        let mut later_session = snapshot.sessions[0].clone();
        later_session.status = "error".to_owned();
        snapshot.sessions.push(later_session);
        let mut later_turn = snapshot.turns[0].clone();
        later_turn.state = "error".to_owned();
        snapshot.turns.push(later_turn);
        let model = read_model_snapshot(&snapshot, CREATED_AT);
        let thread = &snapshot.threads[0];
        let ws = thread_detail(thread, &thread_detail_rows(&snapshot, thread));
        assert_eq!(model["threads"][0], ws);
        assert_eq!(ws["session"]["providerInstanceId"], "codex-custom");
        assert_eq!(ws["session"]["status"], "ready");
        assert_eq!(ws["latestTurn"]["state"], "completed");
        assert_eq!(
            ws["latestTurn"]["sourceProposedPlan"],
            json!({ "threadId": "thread-source", "planId": "plan-source" })
        );
    }

    struct NeverFactory;

    impl ProviderDriverFactory for NeverFactory {
        fn create(
            &self,
            request: ProviderLaunchRequest,
        ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
            Box::pin(async move {
                Err(ProviderRuntimeError::UnsupportedProvider {
                    provider: request.provider,
                })
            })
        }
    }

    #[derive(Default)]
    struct ModelMutationProbe {
        set_model_calls: AtomicUsize,
        interrupt_calls: AtomicUsize,
        fail_set_model_calls: AtomicUsize,
        pause_next_set_model: std::sync::Mutex<Option<Arc<ModelMutationPause>>>,
    }

    #[derive(Default)]
    struct ModelMutationPause {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    impl ModelMutationProbe {
        fn pause_next_set_model(&self) -> Arc<ModelMutationPause> {
            let pause = Arc::new(ModelMutationPause::default());
            *self
                .pause_next_set_model
                .lock()
                .expect("model mutation pause mutex") = Some(pause.clone());
            pause
        }
    }

    impl ProviderDriver for ModelMutationProbe {
        fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
            Box::pin(async { Ok(StartedSession::default()) })
        }

        fn send(
            &self,
            _text: String,
            _attachments: Vec<Value>,
            _interaction_mode: String,
        ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
            Box::pin(async { Ok(None) })
        }

        fn interrupt(
            &self,
            _turn_id: Option<String>,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            self.interrupt_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn approve(
            &self,
            _request_id: String,
            _decision: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }

        fn answer(
            &self,
            _request_id: String,
            _answers: Value,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }

        fn set_mode(
            &self,
            _mode: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }

        fn set_model(
            &self,
            _model: String,
        ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            self.set_model_calls.fetch_add(1, Ordering::SeqCst);
            let pause = self
                .pause_next_set_model
                .lock()
                .expect("model mutation pause mutex")
                .take();
            let fail = self
                .fail_set_model_calls
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok();
            Box::pin(async move {
                if let Some(pause) = pause {
                    pause.entered.notify_one();
                    pause.release.notified().await;
                }
                if fail {
                    Err(ProviderRuntimeError::Provider {
                        provider: "probe".to_owned(),
                        detail: "injected model mutation failure".to_owned(),
                    })
                } else {
                    Ok(())
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
            Box::pin(async { Ok(()) })
        }

        fn next_event(&self) -> BoxRuntimeFuture<'_, Option<ProviderEvent>> {
            Box::pin(std::future::pending())
        }

        fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct ModelMutationProbeFactory(Arc<ModelMutationProbe>);

    impl ProviderDriverFactory for ModelMutationProbeFactory {
        fn create(
            &self,
            _request: ProviderLaunchRequest,
        ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
            let probe: Arc<dyn ProviderDriver> = self.0.clone();
            Box::pin(async move { Ok(probe) })
        }
    }

    async fn migrated_engine() -> OrchestrationEngine {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts")
    }

    async fn append_replay_events(
        engine: &OrchestrationEngine,
        count: usize,
        text: &str,
    ) -> Vec<Value> {
        let repositories = engine.repositories();
        let mut events = Vec::with_capacity(count);
        for index in 0..count {
            let row = repositories
                .append_event(NewOrchestrationEvent {
                    event_id: format!("replay-event-{index:04}"),
                    event_type: "thread.message-sent".to_owned(),
                    aggregate_kind: "thread".to_owned(),
                    aggregate_id: "replay-thread".to_owned(),
                    occurred_at: CREATED_AT.to_owned(),
                    command_id: None,
                    causation_event_id: None,
                    correlation_id: None,
                    payload: json!({
                        "threadId": "replay-thread",
                        "messageId": format!("replay-message-{index:04}"),
                        "role": "assistant",
                        "text": text,
                        "turnId": null,
                        "streaming": false,
                        "createdAt": CREATED_AT,
                        "updatedAt": CREATED_AT,
                    }),
                    metadata: json!({}),
                })
                .await
                .expect("append replay event");
            events.push(wire_event(&row));
        }
        events
    }

    fn replay_array_bytes(events: &[Value]) -> usize {
        serde_json::to_vec(events)
            .expect("encode event array")
            .len()
    }

    /// Clients cannot set the server-authored identity: serde drops the unknown field, so the
    /// command decodes, applies its other fields, and leaves the stored identity untouched.
    #[tokio::test]
    async fn a_client_project_meta_update_cannot_change_the_repository_identity() {
        let engine = migrated_engine().await;
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"project.create","commandId":"create","projectId":"p","title":"P",
                    "workspaceRoot":"/work/p","defaultModelSelection":null,"createdAt":CREATED_AT
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let identity = json!({"canonicalKey":"github.com/acme/repo","locator":{"source":"git-remote","remoteName":"origin","remoteUrl":"git@github.com:acme/repo.git"},"name":"repo"});
        engine
            .dispatch(OrchestrationCommand::ProjectRepositoryIdentitySet {
                command_id: "identity".to_owned(),
                project_id: "p".to_owned(),
                repository_identity: Some(identity.clone()),
            })
            .await
            .unwrap();
        let before = engine.repositories().max_event_sequence().await.unwrap();

        let command = decode_public_orchestration_command(
            &engine,
            json!({
                "type":"project.meta.update","commandId":"client-meta","projectId":"p",
                "title":"Renamed",
                "repositoryIdentity":{"canonicalKey":"evil.example/x","locator":{"source":"git-remote","remoteName":"origin","remoteUrl":"https://evil.example/x.git"},"name":"x"}
            }),
        )
        .await
        .expect("the unknown field is ignored, not an error");
        engine.dispatch(command).await.unwrap();

        let project = engine
            .repositories()
            .get_project("p".to_owned())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(project.title, "Renamed");
        assert_eq!(project.repository_identity, Some(identity));
        let page = read_replay(
            &engine.repositories(),
            before,
            ReplayBudget::Page {
                target_bytes: 100_000,
            },
        )
        .await
        .unwrap();
        assert!(!page.events.is_empty());
        for event in &page.events {
            assert!(
                event["payload"].get("repositoryIdentity").is_none(),
                "{event}"
            );
        }
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_stops_before_large_event_exceeds_target() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 9, &"x".repeat(1_500)).await;
        let target_bytes = replay_array_bytes(&events[..3]);
        let page = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page { target_bytes },
        )
        .await
        .expect("replay page");
        assert_eq!(page.events, events[..3]);
        assert!(!page.exhausted);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_keeps_one_oversized_event() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 2, &"x".repeat(2_000)).await;
        let page = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page { target_bytes: 100 },
        )
        .await
        .expect("oversized first event");
        assert_eq!(page.events, events[..1]);
        assert!(!page.exhausted);
        let final_page = read_replay(
            &engine.repositories(),
            events[0]["sequence"].as_i64().unwrap(),
            ReplayBudget::Page { target_bytes: 100 },
        )
        .await
        .expect("oversized final event");
        assert_eq!(final_page.events, events[1..]);
        assert!(final_page.exhausted);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_returns_all_small_events_at_tail() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 20, "small").await;
        let page = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page {
                target_bytes: 100_000,
            },
        )
        .await
        .expect("small events");
        assert_eq!(page.events, events);
        assert!(page.exhausted);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_counts_escaped_json_bytes_exactly() {
        let engine = migrated_engine().await;
        let text = "\"\\\u{1}é🙂".repeat(20);
        let events = append_replay_events(&engine, 4, &text).await;
        let encoded_text_bytes = serde_json::to_vec(&text).expect("encode text").len() - 2;
        let escape_overhead = encoded_text_bytes - text.len();
        assert!(escape_overhead > 0);
        let escaped_pair_bytes = replay_array_bytes(&events[..2]);
        let unescaped_pair_bytes = escaped_pair_bytes - 2 * escape_overhead;
        let target_bytes = unescaped_pair_bytes + escape_overhead;
        assert!(unescaped_pair_bytes < target_bytes && target_bytes < escaped_pair_bytes);
        let page = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page { target_bytes },
        )
        .await
        .expect("escaped page");
        assert_eq!(page.events, events[..1]);
        assert!(!page.exhausted);
        let exact_page = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page {
                target_bytes: escaped_pair_bytes,
            },
        )
        .await
        .expect("exact escaped boundary");
        assert_eq!(exact_page.events, events[..2]);
        assert_eq!(replay_array_bytes(&exact_page.events), escaped_pair_bytes);
        assert!(!exact_page.exhausted);
        for page in [page, exact_page] {
            let accounted_bytes =
                page.events
                    .iter()
                    .enumerate()
                    .fold(2, |total, (index, event)| {
                        total
                            + encoded_json_len(event).expect("count event")
                            + usize::from(index > 0)
                    });
            assert_eq!(replay_array_bytes(&page.events), accounted_bytes);
        }
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_probes_tail_at_full_batch() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 128, "boundary").await;
        let page = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page {
                target_bytes: 1_000_000,
            },
        )
        .await
        .expect("full final batch");
        assert_eq!(page.events, events);
        assert!(page.exhausted);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_resumes_into_short_final_batch() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 129, "boundary").await;
        let target_bytes = replay_array_bytes(&events[..128]);
        let first = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page { target_bytes },
        )
        .await
        .expect("first page");
        assert_eq!(first.events, events[..128]);
        assert!(!first.exhausted);
        let last = read_replay(
            &engine.repositories(),
            first.events.last().unwrap()["sequence"].as_i64().unwrap(),
            ReplayBudget::Page { target_bytes },
        )
        .await
        .expect("last page");
        assert_eq!(last.events, events[128..]);
        assert!(last.exhausted);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_page_at_full_batch_resumes_without_skipping() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 256, "boundary").await;
        // Later sequences use more digits; this budget fits either batch but
        // cannot fit the first event of the next batch alongside the first 128.
        let target_bytes = replay_array_bytes(&events[128..]);
        assert!(target_bytes < replay_array_bytes(&events[..129]));
        let first = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Page { target_bytes },
        )
        .await
        .expect("exact first batch");
        assert_eq!(first.events, events[..128]);
        assert!(!first.exhausted);
        let last = read_replay(
            &engine.repositories(),
            first.events.last().unwrap()["sequence"].as_i64().unwrap(),
            ReplayBudget::Page { target_bytes },
        )
        .await
        .expect("exact final batch");
        assert_eq!(last.events, events[128..]);
        assert!(last.exhausted);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_pages_equal_whole_replay_and_engine_events() {
        let engine = migrated_engine().await;
        let seeded = append_replay_events(&engine, 270, "page \"text\" é").await;
        let mut cursor = 0;
        let mut events = Vec::new();
        let target_bytes = replay_array_bytes(&seeded[..7]);
        for _ in 0..=seeded.len() {
            let page = read_replay(
                &engine.repositories(),
                cursor,
                ReplayBudget::Page { target_bytes },
            )
            .await
            .expect("next replay page");
            assert!(!page.events.is_empty());
            assert!(replay_array_bytes(&page.events) <= target_bytes);
            for event in &page.events {
                let sequence = event["sequence"].as_i64().unwrap();
                assert!(sequence > cursor);
                cursor = sequence;
            }
            events.extend(page.events);
            assert_eq!(page.exhausted, events.len() == seeded.len());
            if page.exhausted {
                break;
            }
        }
        let whole = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Whole {
                limit_bytes: replay_array_bytes(&seeded),
            },
        )
        .await
        .expect("whole replay at exact ceiling");
        assert!(whole.exhausted);
        assert_eq!(events, whole.events);
        assert_eq!(events, seeded);
        assert_eq!(
            events,
            engine
                .read_events(0)
                .await
                .expect("engine events")
                .iter()
                .map(wire_event)
                .collect::<Vec<_>>()
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_whole_fails_when_limit_is_crossed_in_first_batch() {
        let engine = migrated_engine().await;
        let events = append_replay_events(&engine, 300, &"x".repeat(200)).await;
        let limit_bytes = replay_array_bytes(&events[..10]);
        let error = read_replay(
            &engine.repositories(),
            0,
            ReplayBudget::Whole { limit_bytes },
        )
        .await
        .expect_err("whole replay exceeds ceiling");
        let ReplayReadError::TooLarge {
            bytes,
            limit_bytes: actual_limit,
        } = error
        else {
            panic!("expected size error, got {error:?}");
        };
        assert_eq!(actual_limit, limit_bytes);
        assert!(limit_bytes < bytes && bytes <= replay_array_bytes(&events[..128]));
        assert_eq!(bytes, replay_array_bytes(&events[..11]));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn replay_past_tail_is_empty_in_both_modes() {
        let engine = migrated_engine().await;
        append_replay_events(&engine, 2, "tail").await;
        for cursor in [2, 100] {
            for budget in [
                ReplayBudget::Page {
                    target_bytes: 1_000,
                },
                ReplayBudget::Whole { limit_bytes: 1_000 },
            ] {
                let replay = read_replay(&engine.repositories(), cursor, budget)
                    .await
                    .expect("empty tail");
                assert!(replay.events.is_empty());
                assert!(replay.exhausted);
            }
        }
        engine.shutdown().await;
    }

    async fn delivery_engine(hooks: TestHooks) -> (Database, OrchestrationEngine, String) {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                test_hooks: hooks,
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine starts");
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "delivery-project-create",
                "projectId": "delivery-project",
                "title": "Delivery project",
                "workspaceRoot": "C:/delivery-project",
                "defaultModelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        let thread_id = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        (database, engine, thread_id)
    }

    fn decode_command(value: Value) -> OrchestrationCommand {
        serde_json::from_value(value).expect("command decodes")
    }

    fn delivery_resolution(
        command_id: &str,
        thread_id: &str,
        message_id: &str,
        action: &str,
    ) -> OrchestrationCommand {
        decode_command(json!({
            "type": "thread.turn-delivery.resolve",
            "commandId": command_id,
            "threadId": thread_id,
            "messageId": message_id,
            "action": action,
            "createdAt": CREATED_AT,
        }))
    }

    async fn seed_delivery(
        engine: &OrchestrationEngine,
        command_id: &str,
        thread_id: &str,
        message_id: &str,
        state: TurnDeliveryState,
    ) {
        let queued = state == TurnDeliveryState::Queued;
        let command = decode_command(json!({
            "type": "thread.turn.start",
            "commandId": command_id,
            "threadId": thread_id,
            "message": {
                "messageId": message_id,
                "role": "user",
                "text": command_id,
                "attachments": [],
            },
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "queued": queued,
            "createdAt": CREATED_AT,
        }));
        engine
            .dispatch_with_admission(
                command.clone(),
                CommandAdmission {
                    payload_digest: canonical_command_digest(&command).expect("command digest"),
                    attachment_refs: Vec::<AttachmentReference>::new(),
                    provider_turn: Some(NewProviderTurnDelivery {
                        state: if queued {
                            TurnDeliveryState::Queued
                        } else {
                            TurnDeliveryState::Pending
                        },
                        mode: crate::orchestration::TurnDeliveryMode::Start,
                        command_id: command_id.to_owned(),
                        thread_id: thread_id.to_owned(),
                        message_id: message_id.to_owned(),
                        provider_instance_id: "codex".to_owned(),
                        provider_kind: "codex".to_owned(),
                        provider_session_id: None,
                        delivery_key: format!("delivery-key-{command_id}"),
                        payload: serde_json::to_value(&command).expect("command payload"),
                        created_at: CREATED_AT.to_owned(),
                    }),
                },
                || {},
            )
            .await
            .expect("delivery admitted");
        // Queued starts are admitted directly; requeue transitions are reserved for steers.
        if !queued && state != TurnDeliveryState::Pending {
            assert!(
                engine
                    .transition_turn_delivery(TurnDeliveryTransition {
                        turn_id: None,
                        command_id: command_id.to_owned(),
                        expected_states: vec![TurnDeliveryState::Pending],
                        expected_attempt: 0,
                        next_state: state,
                        detail: Some("delivery outcome is uncertain".to_owned()),
                        updated_at: CREATED_AT.to_owned(),
                    })
                    .await
                    .expect("delivery transition")
            );
        }
    }

    async fn dispatch_prepared_for_test(
        engine: &OrchestrationEngine,
        provider: Option<ProviderRegistration>,
        command: OrchestrationCommand,
    ) -> RpcResult {
        let payload_digest = canonical_command_digest(&command).expect("command digest");
        let command_claim = engine
            .acquire_command_admission(command.command_id())
            .await
            .expect("test command claim");
        dispatch_prepared_command(
            engine.clone(),
            provider,
            command,
            payload_digest,
            "orchestration.dispatchCommand".to_owned(),
            command_claim,
        )
        .await
    }

    async fn dispatch_turn_for_test(
        engine: OrchestrationEngine,
        provider: ProviderRegistration,
        command: OrchestrationCommand,
        payload_digest: String,
        request_tag: String,
        workspace_admission: Option<crate::worktree_catalog::WorkspaceAdmissionLease>,
        owner: &UploadOwner,
    ) -> RpcResult {
        let command_claim = engine
            .acquire_command_admission(command.command_id())
            .await
            .map_err(|error| orchestration_error("OrchestrationDispatchCommandError", error))?;
        dispatch_turn_command(
            engine,
            provider,
            command,
            payload_digest,
            request_tag,
            workspace_admission,
            command_claim,
            owner,
        )
        .await
    }

    fn provider_registration(
        database: Database,
        engine: &OrchestrationEngine,
        settings_root: PathBuf,
        turn_delivery: Arc<TurnDeliveryService>,
    ) -> (ProviderRegistration, Arc<ProviderRuntimeSupervisor>) {
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(NeverFactory),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        (
            ProviderRegistration {
                provider: provider.clone(),
                settings_root: settings_root.clone(),
                attachments: AttachmentMaterializer::new(settings_root.join("attachments")),
                turn_delivery,
                uploads: test_upload_registry(),
            },
            provider,
        )
    }

    #[tokio::test]
    async fn queued_admission_resolves_status_and_preserves_original_receipt_digest() {
        for (status, requested, expected) in [
            ("running", Some(true), TurnDeliveryState::Queued),
            ("starting", Some(true), TurnDeliveryState::Queued),
            ("ready", Some(true), TurnDeliveryState::Pending),
            ("idle", Some(true), TurnDeliveryState::Pending),
            ("stopped", Some(true), TurnDeliveryState::Pending),
            ("error", Some(true), TurnDeliveryState::Pending),
            ("running", None, TurnDeliveryState::Pending),
            ("running", Some(false), TurnDeliveryState::Pending),
        ] {
            let (database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
            engine.dispatch(decode_command(json!({"type":"thread.session.set", "commandId":"session", "threadId":thread_id, "session":{"threadId":thread_id, "status":status, "providerName":"codex", "activeTurnId":null, "lastError":null, "updatedAt":CREATED_AT}, "createdAt":CREATED_AT}))).await.unwrap();
            let service = Arc::new(TurnDeliveryService::start_with_router(
                engine.clone(),
                1,
                Arc::new(|_| Box::pin(async { Ok(()) })),
            ));
            service.shutdown().await;
            let state = tempfile::tempdir().unwrap();
            let (registration, provider) =
                provider_registration(database, &engine, state.path().to_path_buf(), service);
            let mut payload = json!({"type":"thread.turn.start", "commandId":"queued-admission", "threadId":thread_id, "message":{"messageId":"queued-admission-message", "role":"user", "text":"saved", "attachments":[]}, "createdAt":CREATED_AT});
            if let Some(queued) = requested {
                payload["queued"] = json!(queued);
            }
            let command = decode_command(payload);
            let digest = canonical_command_digest(&command).unwrap();
            let before = engine
                .read_events(0)
                .await
                .unwrap()
                .last()
                .unwrap()
                .sequence;
            dispatch_turn_for_test(
                engine.clone(),
                registration.clone(),
                command.clone(),
                digest.clone(),
                "orchestration.dispatchCommand".into(),
                None,
                &UploadOwner::Unauthenticated,
            )
            .await
            .unwrap();
            let row = engine
                .repositories()
                .get_provider_turn_delivery("queued-admission".into())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.state, expected,
                "status {status}, requested {requested:?}"
            );
            assert_eq!(
                row.payload["queued"],
                requested
                    .map(|_| json!(expected == TurnDeliveryState::Queued))
                    .unwrap_or(Value::Null)
            );
            assert_eq!(
                row.payload["modelSelection"],
                json!({"instanceId":"codex", "model":"gpt-5"})
            );
            let events = engine.read_events(before).await.unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event.event_type == "thread.turn-start-requested")
                    .count(),
                usize::from(expected == TurnDeliveryState::Pending)
            );
            let snapshot = thread_snapshot(&engine, &thread_id).await.unwrap();
            assert_eq!(
                snapshot["thread"]["messages"][0]["delivery"]["state"],
                if expected == TurnDeliveryState::Queued {
                    "queued"
                } else {
                    "pending"
                }
            );
            assert_eq!(
                snapshot["thread"]["messages"][0]["delivery"]["mode"],
                "start"
            );
            assert_eq!(snapshot["thread"]["messages"][0]["delivery"]["held"], false);
            assert_eq!(
                engine
                    .repositories()
                    .get_command_receipt("queued-admission".into())
                    .await
                    .unwrap()
                    .unwrap()
                    .payload_digest,
                Some(digest.clone())
            );
            dispatch_turn_for_test(
                engine.clone(),
                registration,
                command,
                digest,
                "orchestration.dispatchCommand".into(),
                None,
                &UploadOwner::Unauthenticated,
            )
            .await
            .unwrap();
            assert_eq!(
                engine.read_events(before).await.unwrap().len(),
                events.len()
            );
            provider.shutdown().await.unwrap();
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn thread_snapshot_keeps_queued_messages_in_admission_order() {
        let (_, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        seed_delivery(
            &engine,
            "z-first",
            &thread_id,
            "z-message",
            TurnDeliveryState::Queued,
        )
        .await;
        seed_delivery(
            &engine,
            "a-second",
            &thread_id,
            "a-message",
            TurnDeliveryState::Queued,
        )
        .await;
        let snapshot = thread_snapshot(&engine, &thread_id).await.unwrap();
        let messages = snapshot["thread"]["messages"].as_array().unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["z-message", "a-message"]
        );
        engine.shutdown().await;
    }

    async fn wait_for_delivery_state(
        engine: &OrchestrationEngine,
        command_id: &str,
        expected: TurnDeliveryState,
    ) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let delivery = engine
                    .repositories()
                    .get_provider_turn_delivery(command_id.to_owned())
                    .await
                    .expect("delivery read")
                    .expect("delivery row");
                if delivery.state == expected {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("delivery reaches expected state");
    }

    fn request(tag: &str, payload: Value) -> RpcRequest {
        RpcRequest {
            id: RequestId::try_from("1").unwrap(),
            tag: tag.to_owned(),
            payload,
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        }
    }

    fn assert_empty_thread_contract(thread: &Value, expected_kind: &str) {
        let object = thread.as_object().expect("thread object");
        assert!(object.contains_key("deletedAt"));
        assert!(object.contains_key("latestTurn"));
        assert!(object.contains_key("session"));
        assert_eq!(thread["deletedAt"], Value::Null);
        assert_eq!(thread["latestTurn"], Value::Null);
        assert_eq!(thread["session"], Value::Null);
        assert_eq!(thread["kind"], expected_kind);
        for field in ["messages", "activities", "proposedPlans", "checkpoints"] {
            assert_eq!(thread[field], json!([]), "{field} is empty");
        }
    }

    async fn dispatch_registered_command(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        id: &str,
        payload: Value,
    ) -> Result<Value, Value> {
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": id,
                    "tag": "orchestration.dispatchCommand",
                    "payload": payload,
                    "headers": [],
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send registered orchestration request");
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            next_frame_past_heartbeat(socket),
        )
        .await
        .expect("registered orchestration response timeout")
        .expect("registered orchestration socket remains open")
        .expect("registered orchestration frame");
        let Message::Text(text) = frame else {
            panic!("expected registered orchestration text frame, got {frame:?}");
        };
        match serde_json::from_str::<ServerMessage>(&text).expect("registered RPC response") {
            ServerMessage::Exit {
                request_id,
                exit: RpcExit::Success { value },
            } if request_id == RequestId::try_from(id).unwrap() => Ok(value.unwrap_or(Value::Null)),
            ServerMessage::Exit {
                request_id,
                exit: RpcExit::Failure { cause },
            } if request_id == RequestId::try_from(id).unwrap() => {
                Err(serde_json::to_value(cause).unwrap())
            }
            message => panic!("unexpected registered orchestration response: {message:?}"),
        }
    }

    #[tokio::test]
    async fn prepared_rpc_replays_legacy_accepted_receipt_without_digest() {
        let engine = migrated_engine().await;
        let command = decode_command(json!({
            "type": "project.create",
            "commandId": "legacy-project-create",
            "projectId": "legacy-project",
            "title": "Legacy project",
            "workspaceRoot": "C:/legacy-project",
            "defaultModelSelection": null,
            "createdAt": CREATED_AT,
        }));
        let original = engine
            .dispatch(command.clone())
            .await
            .expect("historical command accepted");
        let historical_receipt = engine
            .repositories()
            .get_command_receipt("legacy-project-create".to_owned())
            .await
            .expect("historical receipt read")
            .expect("historical receipt");
        assert_eq!(historical_receipt.status, "accepted");
        assert_eq!(historical_receipt.payload_digest, None);
        let event_count = engine
            .read_events(0)
            .await
            .expect("events before replay")
            .len();

        let replay = dispatch_prepared_for_test(&engine, None, command)
            .await
            .expect("identical historical command replays its original receipt");

        assert_eq!(
            replay,
            serde_json::to_value(original).expect("original result")
        );
        assert_eq!(
            engine
                .read_events(0)
                .await
                .expect("events after replay")
                .len(),
            event_count,
            "an accepted replay cannot append events"
        );
        assert_eq!(
            engine
                .repositories()
                .get_command_receipt("legacy-project-create".to_owned())
                .await
                .expect("replayed receipt read")
                .expect("replayed receipt")
                .payload_digest,
            None,
            "legacy replay must not rewrite the historical receipt"
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn prepared_rpc_replays_legacy_rejected_receipt_without_digest() {
        let engine = migrated_engine().await;
        let command = decode_command(json!({
            "type": "project.delete",
            "commandId": "legacy-project-delete-rejected",
            "projectId": "missing-project",
            "force": true,
        }));
        engine
            .dispatch(command.clone())
            .await
            .expect_err("historical command rejected");
        let historical_receipt = engine
            .repositories()
            .get_command_receipt("legacy-project-delete-rejected".to_owned())
            .await
            .expect("historical rejected receipt read")
            .expect("historical rejected receipt");
        assert_eq!(historical_receipt.status, "rejected");
        assert_eq!(historical_receipt.payload_digest, None);
        let historical_detail = historical_receipt.error.expect("rejection detail");
        let event_count = engine
            .read_events(0)
            .await
            .expect("events before rejected replay")
            .len();

        let replay = dispatch_prepared_for_test(&engine, None, command)
            .await
            .expect_err("historical rejection remains rejected");

        assert_eq!(replay["_tag"], "OrchestrationDispatchCommandError");
        let message = replay["message"].as_str().expect("replay error message");
        assert!(message.to_ascii_lowercase().contains("previously rejected"));
        assert!(message.contains(&historical_detail));
        assert!(!message.to_ascii_lowercase().contains("conflict"));
        assert_eq!(
            engine
                .read_events(0)
                .await
                .expect("events after rejected replay")
                .len(),
            event_count,
            "a rejected replay cannot append events"
        );
        assert_eq!(
            engine
                .repositories()
                .get_command_receipt("legacy-project-delete-rejected".to_owned())
                .await
                .expect("replayed rejected receipt read")
                .expect("replayed rejected receipt")
                .payload_digest,
            None,
            "legacy rejected replay must not rewrite the historical receipt"
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn turn_rpc_replays_legacy_receipts_before_attachment_or_delivery_work() {
        let (database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let accepted = decode_command(json!({
            "type": "thread.turn.start",
            "commandId": "legacy-turn-accepted",
            "threadId": thread_id,
            "message": {
                "messageId": "legacy-turn-message",
                "role": "user",
                "text": "historical turn",
                "attachments": [{
                    "type": "file",
                    "id": "legacy-file",
                    "name": "legacy.txt",
                    "mimeType": "text/plain",
                    "sizeBytes": 6,
                    "dataUrl": "data:text/plain;base64,bGVnYWN5"
                }]
            },
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "createdAt": CREATED_AT,
        }));
        let original = engine
            .dispatch(accepted.clone())
            .await
            .expect("historical turn accepted");
        let rejected = decode_command(json!({
            "type": "thread.turn.start",
            "commandId": "legacy-turn-rejected",
            "threadId": "missing-legacy-thread",
            "message": {
                "messageId": "legacy-rejected-message",
                "role": "user",
                "text": "historical rejected turn",
                "attachments": []
            },
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "createdAt": CREATED_AT,
        }));
        engine
            .dispatch(rejected.clone())
            .await
            .expect_err("historical missing-thread turn rejected");
        for command_id in ["legacy-turn-accepted", "legacy-turn-rejected"] {
            assert_eq!(
                engine
                    .repositories()
                    .get_command_receipt(command_id.to_owned())
                    .await
                    .expect("legacy turn receipt read")
                    .expect("legacy turn receipt")
                    .payload_digest,
                None
            );
        }

        let state = tempfile::tempdir().expect("provider state");
        std::fs::write(state.path().join("attachments"), b"blocked attachment root")
            .expect("block attachment materialization");
        let router: DeliveryRouter =
            Arc::new(|_| Box::pin(async { panic!("legacy replay must not route provider work") }));
        let service = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            router,
        ));
        let (registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            service.clone(),
        );
        let event_count = engine
            .read_events(0)
            .await
            .expect("events before replay")
            .len();

        let replay = dispatch_turn_for_test(
            engine.clone(),
            registration.clone(),
            accepted,
            "ignored legacy digest".to_owned(),
            "orchestration.dispatchCommand".to_owned(),
            None,
            &UploadOwner::Unauthenticated,
        )
        .await
        .expect("legacy accepted turn replays its stored result");
        assert_eq!(
            replay,
            serde_json::to_value(original).expect("original result")
        );
        let rejection = dispatch_turn_for_test(
            engine.clone(),
            registration,
            rejected,
            "different ignored legacy digest".to_owned(),
            "orchestration.dispatchCommand".to_owned(),
            None,
            &UploadOwner::Unauthenticated,
        )
        .await
        .expect_err("legacy rejected turn remains rejected");
        assert!(
            rejection["message"].as_str().is_some_and(|message| message
                .to_ascii_lowercase()
                .contains("previously rejected"))
        );
        assert_eq!(
            engine
                .read_events(0)
                .await
                .expect("events after replay")
                .len(),
            event_count
        );
        for command_id in ["legacy-turn-accepted", "legacy-turn-rejected"] {
            assert!(
                engine
                    .repositories()
                    .get_provider_turn_delivery(command_id.to_owned())
                    .await
                    .expect("legacy outbox read")
                    .is_none(),
                "legacy replay cannot synthesize delivery work"
            );
        }

        service.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn losing_turn_admission_never_prepares_external_attachments() {
        let hooks = TestHooks::default();
        let (database, engine, thread_id) = delivery_engine(hooks.clone()).await;
        let state = tempfile::tempdir().expect("provider state");
        let delivery = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        ));
        let (registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            delivery.clone(),
        );
        let command = decode_command(json!({
            "type":"thread.turn.start",
            "commandId":"attachment-admission-race",
            "threadId":thread_id,
            "message":{
                "messageId":"attachment-admission-message",
                "role":"user",
                "text":"review",
                "attachments":[{
                    "type":"file",
                    "id":"attachment-admission-file",
                    "name":"notes.txt",
                    "mimeType":"text/plain",
                    "sizeBytes":5,
                    "dataUrl":"data:text/plain;base64,bm90ZXM="
                }]
            },
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "createdAt":CREATED_AT
        }));
        let payload_digest = canonical_command_digest(&command).expect("turn digest");
        let removal_claim = engine
            .acquire_command_admission("attachment-admission-race")
            .await
            .expect("removal owns absent turn identity");
        engine
            .reserve_worktree_removal_admission(
                &removal_claim,
                "attachment-admission-race",
                "removal-project",
                "removal-payload",
            )
            .await
            .expect("removal reserves the absent turn identity");
        let dispatch_engine = engine.clone();
        let mut dispatch = tokio::spawn(async move {
            dispatch_turn_for_test(
                dispatch_engine,
                registration,
                command,
                payload_digest,
                "orchestration.dispatchCommand".to_owned(),
                None,
                &UploadOwner::Unauthenticated,
            )
            .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut dispatch)
                .await
                .is_err(),
            "turn must wait while removal owns the command ID"
        );
        drop(removal_claim);

        let error = dispatch
            .await
            .expect("turn dispatch joins")
            .expect_err("losing turn conflicts");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.to_ascii_lowercase().contains("conflict"))
        );
        assert!(
            !state.path().join("attachments").exists(),
            "losing admission must not initialize or publish the attachment store"
        );

        delivery.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_matching_turn_replays_without_repreparing_attachments() {
        let (database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let state = tempfile::tempdir().expect("provider state");
        let delivery = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        ));
        let (mut registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            delivery.clone(),
        );
        let publication = Arc::new(AttachmentPrepareTestPause::default());
        registration.attachments = AttachmentMaterializer::new(state.path().join("attachments"))
            .with_pause_after_final_publication(publication.clone());
        let command = decode_command(json!({
            "type":"thread.turn.start",
            "commandId":"concurrent-attachment-command",
            "threadId":thread_id,
            "message":{
                "messageId":"concurrent-attachment-message",
                "role":"user",
                "text":"review",
                "attachments":[{
                    "type":"file",
                    "id":"concurrent-attachment-file",
                    "name":"notes.txt",
                    "mimeType":"text/plain",
                    "sizeBytes":5,
                    "dataUrl":"data:text/plain;base64,bm90ZXM="
                }]
            },
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "createdAt":CREATED_AT
        }));
        let digest = canonical_command_digest(&command).expect("turn digest");
        let first_engine = engine.clone();
        let first_registration = registration.clone();
        let first_command = command.clone();
        let first_digest = digest.clone();
        let first = tokio::spawn(async move {
            dispatch_turn_for_test(
                first_engine,
                first_registration,
                first_command,
                first_digest,
                "orchestration.dispatchCommand".to_owned(),
                None,
                &UploadOwner::Unauthenticated,
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            publication.wait_until_reached(),
        )
        .await
        .expect("first claimant publishes attachment");
        let second_engine = engine.clone();
        let mut second = tokio::spawn(async move {
            dispatch_turn_for_test(
                second_engine,
                registration,
                command,
                digest,
                "orchestration.dispatchCommand".to_owned(),
                None,
                &UploadOwner::Unauthenticated,
            )
            .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "matching turn must wait for the live claimant"
        );
        std::fs::write(
            state
                .path()
                .join("attachments")
                .join("concurrent-attachment-file"),
            b"other",
        )
        .expect("corrupt the published file to expose duplicate preparation");
        publication.release();
        let first_result = first
            .await
            .expect("first turn joins")
            .expect("first turn accepts");
        let second_result = second
            .await
            .expect("second turn joins")
            .expect("matching turn replays without attachment preparation");
        assert_eq!(second_result, first_result);

        delivery.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn ordinary_rpc_receipts_store_and_validate_the_canonical_digest() {
        let engine = migrated_engine().await;
        let accepted = decode_command(json!({
            "type": "project.create",
            "commandId": "digested-project-create",
            "projectId": "digested-project",
            "title": "Digested project",
            "workspaceRoot": "C:/digested-project",
            "defaultModelSelection": null,
            "createdAt": CREATED_AT,
        }));
        let accepted_digest = canonical_command_digest(&accepted).expect("accepted digest");
        dispatch_prepared_for_test(&engine, None, accepted)
            .await
            .expect("ordinary accepted command");
        assert_eq!(
            engine
                .repositories()
                .get_command_receipt("digested-project-create".to_owned())
                .await
                .expect("accepted receipt read")
                .expect("accepted receipt")
                .payload_digest,
            Some(accepted_digest)
        );

        let rejected = decode_command(json!({
            "type": "project.delete",
            "commandId": "digested-project-delete-rejected",
            "projectId": "missing-digested-project",
            "force": true,
        }));
        let rejected_digest = canonical_command_digest(&rejected).expect("rejected digest");
        dispatch_prepared_for_test(&engine, None, rejected)
            .await
            .expect_err("ordinary rejected command");
        assert_eq!(
            engine
                .repositories()
                .get_command_receipt("digested-project-delete-rejected".to_owned())
                .await
                .expect("rejected receipt read")
                .expect("rejected receipt")
                .payload_digest,
            Some(rejected_digest)
        );

        let conflict = dispatch_prepared_for_test(
            &engine,
            None,
            decode_command(json!({
                "type": "project.delete",
                "commandId": "digested-project-delete-rejected",
                "projectId": "different-project",
                "force": true,
            })),
        )
        .await
        .expect_err("changed ordinary replay conflicts");
        assert!(
            conflict["message"]
                .as_str()
                .is_some_and(|message| message.to_ascii_lowercase().contains("conflict"))
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn archiving_a_chat_panel_interrupts_its_provider_work_on_the_server() {
        let engine = migrated_engine().await;
        let state = tempfile::tempdir().expect("provider state");
        let mut commands = vec![json!({
            "type":"project.create",
            "commandId":"panel-close-project",
            "projectId":"panel-close-project",
            "title":"Panel Close Project",
            "workspaceRoot":state.path(),
            "defaultModelSelection":null,
            "createdAt":CREATED_AT
        })];
        for (thread_id, kind, host) in [
            ("host", "workspace", None),
            ("side", "workspace", None),
            ("panel", "panel", Some("host")),
        ] {
            commands.push(json!({
                "type":"thread.create",
                "commandId":format!("create-{thread_id}"),
                "threadId":thread_id,
                "projectId":"panel-close-project",
                "title":thread_id,
                "kind":kind,
                "hostThreadId":host,
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default",
                "branch":null,
                "worktreePath":null,
                "createdAt":CREATED_AT
            }));
        }
        for command in commands {
            engine
                .dispatch(decode_command(command))
                .await
                .expect("fixture command");
        }
        let probe = Arc::new(ModelMutationProbe::default());
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(ModelMutationProbeFactory(probe.clone())),
            ActivityProjection::new(ActivityRepository::new(
                engine.repositories().database().clone(),
            )),
            SupervisorOptions::default(),
        ));
        for thread_id in ["side", "panel"] {
            provider
                .launch(ProviderLaunchRequest {
                    thread_id: thread_id.to_owned(),
                    activity_causal_revision: 0,
                    provider: "codex".to_owned(),
                    provider_label: "Codex".to_owned(),
                    provider_instance_id: Some("codex".to_owned()),
                    binary_path: "probe".to_owned(),
                    cwd: state.path().to_path_buf(),
                    runtime_mode: "full-access".to_owned(),
                    interaction_mode: "default".to_owned(),
                    model: Some("gpt-5".to_owned()),
                    options: Vec::new(),
                    custom_models: Vec::new(),
                    service_tier: None,
                    effort: None,
                    agent: None,
                    resume_cursor: None,
                    environment: Default::default(),
                    endpoint: None,
                    server_password: None,
                    mcp: None,
                    codex_home: None,
                    open_url: None,
                })
                .await
                .expect("provider launches");
        }
        let registration = ProviderRegistration {
            provider: provider.clone(),
            settings_root: state.path().to_path_buf(),
            attachments: AttachmentMaterializer::new(state.path().join("attachments")),
            turn_delivery: Arc::new(TurnDeliveryService::start_with_router(
                engine.clone(),
                1,
                Arc::new(|_| Box::pin(async { Ok(()) })),
            )),
            uploads: test_upload_registry(),
        };

        // Archiving an ordinary thread leaves its provider work alone.
        dispatch_prepared_for_test(
            &engine,
            Some(registration.clone()),
            decode_command(
                json!({"type":"thread.archive","commandId":"archive-side","threadId":"side"}),
            ),
        )
        .await
        .expect("side thread archives");
        assert_eq!(probe.interrupt_calls.load(Ordering::SeqCst), 0);

        // Closing a chat panel stops its work even when no client saw a running session.
        dispatch_prepared_for_test(
            &engine,
            Some(registration),
            decode_command(
                json!({"type":"thread.archive","commandId":"archive-panel","threadId":"panel"}),
            ),
        )
        .await
        .expect("panel archives");
        assert_eq!(probe.interrupt_calls.load(Ordering::SeqCst), 1);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn losing_generic_admission_never_reaches_the_provider() {
        let hooks = TestHooks::default();
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine starts");
        engine
            .dispatch(decode_command(json!({
                "type":"project.create",
                "commandId":"provider-race-project-create",
                "projectId":"provider-race-project",
                "title":"Provider Race Project",
                "workspaceRoot":"C:/provider-race-project",
                "defaultModelSelection":null,
                "createdAt":CREATED_AT
            })))
            .await
            .expect("project created");
        engine
            .dispatch(decode_command(json!({
                "type":"thread.create",
                "commandId":"provider-race-thread-create",
                "threadId":"provider-race-thread",
                "projectId":"provider-race-project",
                "title":"Provider Race Thread",
                "kind":"workspace",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default",
                "branch":null,
                "worktreePath":null,
                "createdAt":CREATED_AT
            })))
            .await
            .expect("thread created");

        let probe = Arc::new(ModelMutationProbe::default());
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(ModelMutationProbeFactory(probe.clone())),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let state = tempfile::tempdir().expect("provider state");
        provider
            .launch(ProviderLaunchRequest {
                thread_id: "provider-race-thread".to_owned(),
                activity_causal_revision: 0,
                provider: "codex".to_owned(),
                provider_label: "Codex".to_owned(),
                provider_instance_id: Some("codex".to_owned()),
                binary_path: "probe".to_owned(),
                cwd: state.path().to_path_buf(),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                model: Some("gpt-5".to_owned()),
                options: Vec::new(),
                custom_models: Vec::new(),
                service_tier: None,
                effort: None,
                agent: None,
                resume_cursor: None,
                environment: Default::default(),
                endpoint: None,
                server_password: None,
                mcp: None,
                codex_home: None,
                open_url: None,
            })
            .await
            .expect("provider launches");
        let delivery = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        ));
        let registration = ProviderRegistration {
            provider: provider.clone(),
            settings_root: state.path().to_path_buf(),
            attachments: AttachmentMaterializer::new(state.path().join("attachments")),
            turn_delivery: delivery.clone(),
            uploads: test_upload_registry(),
        };
        let command = decode_command(json!({
            "type":"thread.meta.update",
            "commandId":"provider-race-command",
            "threadId":"provider-race-thread",
            "modelSelection":{"instanceId":"codex","model":"gpt-5.1"}
        }));
        let removal_claim = engine
            .acquire_command_admission("provider-race-command")
            .await
            .expect("removal owns absent provider command identity");
        let reserved = engine
            .reserve_worktree_removal_admission(
                &removal_claim,
                "provider-race-command",
                "provider-race-project",
                "removal-payload",
            )
            .await
            .expect("removal reserves the absent command identity");
        assert!(reserved.0.is_none());
        let generic_engine = engine.clone();
        let racing_registration = registration.clone();
        let mut generic = tokio::spawn(async move {
            dispatch_prepared_for_test(&generic_engine, Some(racing_registration), command).await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut generic)
                .await
                .is_err(),
            "generic provider command must wait while removal owns the command ID"
        );
        drop(removal_claim);

        let error = generic
            .await
            .expect("generic dispatch joins")
            .expect_err("losing generic admission conflicts");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.to_ascii_lowercase().contains("conflict"))
        );
        assert_eq!(
            probe.set_model_calls.load(Ordering::SeqCst),
            0,
            "a losing generic command must not mutate the provider"
        );
        let receipt = engine
            .repositories()
            .get_command_receipt("provider-race-command".to_owned())
            .await
            .expect("receipt read")
            .expect("removal receipt");
        assert_eq!(receipt.status, "reserved");
        assert_eq!(receipt.payload_digest.as_deref(), Some("removal-payload"));

        probe.fail_set_model_calls.store(1, Ordering::SeqCst);
        let resumable = decode_command(json!({
            "type":"thread.meta.update",
            "commandId":"provider-resume-command",
            "threadId":"provider-race-thread",
            "modelSelection":{"instanceId":"codex","model":"gpt-5.1"}
        }));
        dispatch_prepared_for_test(&engine, Some(registration.clone()), resumable.clone())
            .await
            .expect_err("injected provider failure leaves exact admission resumable");
        let calls_after_failure = probe.set_model_calls.load(Ordering::SeqCst);
        assert!(calls_after_failure > 0);
        let reserved = engine
            .repositories()
            .get_command_receipt("provider-resume-command".to_owned())
            .await
            .expect("resumable receipt read")
            .expect("resumable receipt");
        assert_eq!(reserved.status, "reserved");
        assert_eq!(
            reserved.payload_digest.as_deref(),
            Some(
                canonical_command_digest(&resumable)
                    .expect("resume digest")
                    .as_str()
            )
        );

        let accepted =
            dispatch_prepared_for_test(&engine, Some(registration.clone()), resumable.clone())
                .await
                .expect("same-digest retry resumes provider mutation and durable admission");
        let calls_after_retry = probe.set_model_calls.load(Ordering::SeqCst);
        assert!(calls_after_retry > calls_after_failure);
        assert_eq!(
            dispatch_prepared_for_test(&engine, Some(registration.clone()), resumable)
                .await
                .expect("accepted retry replays"),
            accepted
        );
        assert_eq!(
            probe.set_model_calls.load(Ordering::SeqCst),
            calls_after_retry,
            "accepted replay must not repeat provider mutation"
        );
        let changed = decode_command(json!({
            "type":"thread.meta.update",
            "commandId":"provider-resume-command",
            "threadId":"provider-race-thread",
            "modelSelection":{"instanceId":"codex","model":"gpt-5.2"}
        }));
        dispatch_prepared_for_test(&engine, Some(registration.clone()), changed)
            .await
            .expect_err("changed payload conflicts before provider mutation");
        assert_eq!(
            probe.set_model_calls.load(Ordering::SeqCst),
            calls_after_retry
        );

        let concurrent = decode_command(json!({
            "type":"thread.meta.update",
            "commandId":"provider-concurrent-command",
            "threadId":"provider-race-thread",
            "modelSelection":{"instanceId":"codex","model":"gpt-5.3"}
        }));
        let calls_before_concurrent = probe.set_model_calls.load(Ordering::SeqCst);
        let preparations_before_concurrent = hooks.generic_external_preparation_attempts();
        let provider_pause = probe.pause_next_set_model();
        let first_engine = engine.clone();
        let first_registration = registration.clone();
        let first_command = concurrent.clone();
        let first = tokio::spawn(async move {
            dispatch_prepared_for_test(&first_engine, Some(first_registration), first_command).await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider_pause.entered.notified(),
        )
        .await
        .expect("first matching claimant reaches provider mutation");
        let second_engine = engine.clone();
        let second_registration = registration.clone();
        let mut second = tokio::spawn(async move {
            dispatch_prepared_for_test(&second_engine, Some(second_registration), concurrent).await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut second)
                .await
                .is_err(),
            "the matching waiter must not complete while the live claimant owns preparation"
        );
        assert_eq!(
            probe.set_model_calls.load(Ordering::SeqCst),
            calls_before_concurrent + 1,
            "matching live claimants must coalesce to one provider mutation"
        );
        assert_eq!(
            hooks.generic_external_preparation_attempts(),
            preparations_before_concurrent + 1,
            "the matching waiter must replay without entering provider preparation"
        );
        provider_pause.release.notify_one();
        let first_result = first
            .await
            .expect("first matching claimant joins")
            .expect("first matching claimant succeeds");
        let second_result = second
            .await
            .expect("second matching claimant joins")
            .expect("second matching claimant replays");
        assert_eq!(second_result, first_result);
        assert_eq!(
            probe.set_model_calls.load(Ordering::SeqCst),
            calls_before_concurrent + 1,
            "accepted replay must not reroute after waiting"
        );

        delivery.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn registered_turn_rpc_atomically_admits_the_first_local_draft_turn() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("engine starts");
        let state = tempfile::tempdir().expect("provider state");
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "local-draft-project-create",
                "projectId": "local-draft-project",
                "title": "Local draft project",
                "workspaceRoot": state.path(),
                "defaultModelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(NeverFactory),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let router: DeliveryRouter = Arc::new(|_| Box::pin(async { Ok(()) }));
        let delivery = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            router,
        ));
        let mut registry = RpcRegistry::empty();
        register_orchestration_rpc_with_delivery(
            &mut registry,
            engine.clone(),
            provider.clone(),
            state.path().to_path_buf(),
            delivery.clone(),
            test_upload_registry(),
        );
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(state.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            registry,
        )
        .await
        .expect("registered RPC server starts");
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/ws", handle.local_addr()))
                .await
                .expect("registered RPC socket connects");
        let selection = json!({"instanceId": "codex", "model": "gpt-5"});
        dispatch_registered_command(
            &mut socket,
            "1",
            json!({
                "type": "thread.turn.start",
                "commandId": "local-draft-turn",
                "threadId": "local-draft-thread",
                "message": {
                    "messageId": "local-draft-message",
                    "role": "user",
                    "text": "first local draft message",
                    "attachments": []
                },
                "modelSelection": selection,
                "titleSeed": "First local draft",
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "bootstrap": {
                    "createThread": {
                        "projectId": "local-draft-project",
                        "title": "First local draft",
                        "modelSelection": selection,
                        "runtimeMode": "full-access",
                        "interactionMode": "default",
                        "branch": null,
                        "worktreePath": null,
                        "createdAt": CREATED_AT
                    }
                },
                "createdAt": CREATED_AT
            }),
        )
        .await
        .expect("ChatView local draft shape is admitted");
        assert!(
            engine
                .repositories()
                .get_thread("local-draft-thread".to_owned())
                .await
                .expect("thread read")
                .is_some(),
            "the composite admission creates the draft thread"
        );
        let outbox = engine
            .repositories()
            .get_provider_turn_delivery("local-draft-turn".to_owned())
            .await
            .expect("outbox read")
            .expect("outbox row");
        assert_eq!(outbox.thread_id, "local-draft-thread");
        assert_eq!(outbox.message_id, "local-draft-message");
        assert_eq!(outbox.provider_instance_id, "codex");
        let frozen_fingerprint = outbox.payload["_bibcodeProviderRouteFingerprint"]
            .as_str()
            .expect("admission persists a provider route fingerprint");
        assert_eq!(frozen_fingerprint.len(), 64);
        let frozen_command = serde_json::from_value::<OrchestrationCommand>(outbox.payload.clone())
            .expect("the internal route field does not alter provider command decoding");
        let mut repeated_payload =
            serde_json::to_value(&frozen_command).expect("repeat route payload");
        freeze_delivery_route(
            &engine,
            &state.path().to_path_buf(),
            &frozen_command,
            &mut repeated_payload,
        )
        .await
        .expect("unchanged settings refreeze deterministically");
        assert_eq!(
            repeated_payload["_bibcodeProviderRouteFingerprint"],
            outbox.payload["_bibcodeProviderRouteFingerprint"]
        );

        socket
            .close(None)
            .await
            .expect("close registered RPC socket");
        handle.shutdown();
        handle.join().await.expect("registered RPC server joins");
        delivery.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn interrupted_turn_cannot_commit_after_authoritative_workspace_loss() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let hooks = TestHooks::default();
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine starts");
        let state = tempfile::tempdir().expect("provider state");
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "loss-project-create",
                "projectId": "loss-project",
                "title": "Loss project",
                "workspaceRoot": state.path(),
                "defaultModelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        let thread_id = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            Arc::new(NeverFactory),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let router: DeliveryRouter = Arc::new(|_| Box::pin(async { Ok(()) }));
        let delivery = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            router,
        ));
        let availability = WorkspaceAvailabilityRegistry::new();
        let mut registry = RpcRegistry::empty();
        register_orchestration_rpc_with_delivery_and_availability(
            &mut registry,
            engine.clone(),
            provider.clone(),
            state.path().to_path_buf(),
            delivery.clone(),
            availability.clone(),
            test_upload_registry(),
        );
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(state.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            registry,
        )
        .await
        .expect("registered RPC server starts");
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/ws", handle.local_addr()))
                .await
                .expect("registered RPC socket connects");
        let durable_after_disconnect = json!({
            "type": "thread.turn.start",
            "commandId": "disconnect-only-turn",
            "threadId": thread_id,
            "message": {
                "messageId": "disconnect-only-message",
                "role": "user",
                "text": "must retain durable handoff",
                "attachments": []
            },
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "createdAt": CREATED_AT
        });
        let disconnect_pause = hooks.pause_before_next_command_persist();
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": "6",
                    "tag": "orchestration.dispatchCommand",
                    "payload": durable_after_disconnect,
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send disconnect-only turn request");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            disconnect_pause.wait_until_entered(),
        )
        .await
        .expect("disconnect-only turn reaches persistence");
        socket
            .send(Message::Text(
                json!({"_tag": "Interrupt", "requestId": "6"})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("interrupt disconnect-only request");
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        .expect("disconnect-only interrupt response timeout")
        .expect("disconnect-only interrupt response")
        .expect("disconnect-only interrupt frame");
        disconnect_pause.release();
        let disconnect_scope = WorkspaceLossTransition {
            thread_id: thread_id.clone(),
            repository_key: "loss-repository".to_owned(),
            generation: 0,
            path: state.path().to_path_buf(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            availability.wait_for_transition_admissions(&disconnect_scope),
        )
        .await
        .expect("queued command retains and then releases its admission lease");
        assert!(
            engine
                .repositories()
                .get_command_receipt("disconnect-only-turn".to_owned())
                .await
                .expect("disconnect-only receipt")
                .is_some(),
            "RPC interruption alone preserves durable command handoff"
        );
        dispatch_registered_command(&mut socket, "9", durable_after_disconnect)
            .await
            .expect("interrupted durable command replays exactly");
        let event_count_before_loss_turn = engine
            .read_events(0)
            .await
            .expect("events before loss turn")
            .len();

        let pause = hooks.pause_before_next_command_finalization();
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": "7",
                    "tag": "orchestration.dispatchCommand",
                    "payload": {
                        "type": "thread.turn.start",
                        "commandId": "loss-turn",
                        "threadId": thread_id,
                        "message": {
                            "messageId": "loss-message",
                            "role": "user",
                            "text": "must not commit after loss",
                            "attachments": []
                        },
                        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                        "createdAt": CREATED_AT
                    },
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send turn request");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                () = pause.wait_until_entered() => {}
                frame = next_frame_past_heartbeat(&mut socket) => {
                    panic!("turn exited before persistence: {frame:?}")
                }
            }
        })
        .await
        .expect("turn reaches the SQLite pre-finalization barrier");
        socket
            .send(Message::Text(
                json!({"_tag": "Interrupt", "requestId": "7"})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("interrupt turn request");
        let interrupted = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        .expect("interrupt response timeout")
        .expect("interrupt response")
        .expect("interrupt frame");
        assert!(matches!(interrupted, Message::Text(_)));

        let loss = WorkspaceLossTransition {
            thread_id: thread_id.clone(),
            repository_key: "loss-repository".to_owned(),
            generation: 1,
            path: state.path().to_path_buf(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        assert!(
            availability
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        pause.release();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            availability.wait_for_transition_admissions(&loss),
        )
        .await
        .expect("workspace admission drains after the SQLite barrier releases");

        let receipt = engine
            .repositories()
            .get_command_receipt("loss-turn".to_owned())
            .await
            .expect("receipt lookup");
        let outbox = engine
            .repositories()
            .get_provider_turn_delivery("loss-turn".to_owned())
            .await
            .expect("outbox lookup");
        let event_count_after_loss_turn = engine
            .read_events(0)
            .await
            .expect("events after loss turn")
            .len();
        let messages = thread_snapshot(&engine, &thread_id)
            .await
            .expect("thread snapshot")["thread"]["messages"]
            .as_array()
            .expect("messages")
            .clone();
        assert!(
            messages
                .iter()
                .any(|message| { message["id"] == "disconnect-only-message" })
        );
        assert!(
            receipt.is_none()
                && outbox.is_none()
                && event_count_after_loss_turn == event_count_before_loss_turn
                && !messages
                    .iter()
                    .any(|message| message["id"] == "loss-message"),
            "loss-before-finalization must roll back every artifact; receipt={}, outbox={}, events_before={event_count_before_loss_turn}, events_after={event_count_after_loss_turn}, message={} ",
            receipt.is_some(),
            outbox.is_some(),
            messages
                .iter()
                .any(|message| message["id"] == "loss-message"),
        );

        availability
            .clear_recovered_in_repository(&thread_id, state.path(), "loss-repository")
            .await
            .expect("physical identity resolves");
        for (suffix, generation, source_plan) in [
            ("accepted", 2_u64, None),
            ("rejected", 3_u64, Some("missing-forced-order-plan")),
        ] {
            let finalization_pause = hooks.pause_before_next_command_finalization();
            let rejection_pause = availability.pause_after_next_finalization_rejection();
            let mut payload = json!({
                "type": "thread.turn.start",
                "commandId": format!("forced-order-{suffix}-turn"),
                "threadId": thread_id,
                "message": {
                    "messageId": format!("forced-order-{suffix}-message"),
                    "role": "user",
                    "text": "loss must retain its exact public error",
                    "attachments": []
                },
                "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "createdAt": CREATED_AT
            });
            if let Some(plan_id) = source_plan {
                payload["sourceProposedPlan"] = json!({
                    "threadId": thread_id,
                    "planId": plan_id,
                });
            }
            let request = dispatch_registered_command(&mut socket, "12", payload);
            tokio::pin!(request);
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    () = finalization_pause.wait_until_entered() => {}
                    result = &mut request => panic!("forced-order turn exited before finalization: {result:?}"),
                }
            })
            .await
            .expect("forced-order turn reaches pre-finalization barrier");

            let forced_loss = WorkspaceLossTransition {
                thread_id: thread_id.clone(),
                repository_key: "loss-repository".to_owned(),
                generation,
                path: state.path().to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            };
            let loss_availability = availability.clone();
            let loss_transition = forced_loss.clone();
            let runtime = tokio::runtime::Handle::current();
            let loss_task = tokio::task::spawn_blocking(move || {
                runtime.block_on(loss_availability.mark_unavailable(loss_transition))
            });
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                rejection_pause.wait_until_entered(),
            )
            .await
            .expect("loss rejects finalization before publishing cancellation");
            finalization_pause.release();

            let error = request
                .await
                .expect_err("loss-wins RPC returns a typed failure");
            rejection_pause.release();
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(5), loss_task)
                    .await
                    .expect("forced-order loss completes")
                    .expect("forced-order loss joins")
                    .expect("physical identity resolves")
            );
            assert_eq!(
                error[0]["error"]["_tag"], "WorkspaceUnavailableError",
                "accepted and rejected persistence must not expose generic cancellation"
            );
            assert_eq!(error[0]["error"]["threadId"], thread_id);
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                availability.wait_for_transition_admissions(&forced_loss),
            )
            .await
            .expect("forced-order admission drains");
            assert!(
                engine
                    .repositories()
                    .get_command_receipt(format!("forced-order-{suffix}-turn"))
                    .await
                    .expect("forced-order receipt lookup")
                    .is_none()
            );
            availability
                .clear_recovered_in_repository(&thread_id, state.path(), "loss-repository")
                .await
                .expect("physical identity resolves");
        }
        let commit_wins_pause = hooks.pause_after_next_command_finalization();
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": "8",
                    "tag": "orchestration.dispatchCommand",
                    "payload": {
                        "type": "thread.turn.start",
                        "commandId": "commit-wins-turn",
                        "threadId": thread_id,
                        "message": {
                            "messageId": "commit-wins-message",
                            "role": "user",
                            "text": "commit finalization wins before loss",
                            "attachments": []
                        },
                        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                        "createdAt": CREATED_AT
                    },
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send commit-wins turn request");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            commit_wins_pause.wait_until_entered(),
        )
        .await
        .expect("turn acquires finalization fence before SQLite commit");
        let commit_wins_loss = WorkspaceLossTransition {
            thread_id: thread_id.clone(),
            repository_key: "loss-repository".to_owned(),
            generation: 4,
            path: state.path().to_path_buf(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        let (loss_started_tx, loss_started_rx) = tokio::sync::oneshot::channel();
        let commit_wins_availability = availability.clone();
        let commit_wins_transition = commit_wins_loss.clone();
        let runtime = tokio::runtime::Handle::current();
        let loss_task = tokio::task::spawn_blocking(move || {
            let _ = loss_started_tx.send(());
            runtime.block_on(commit_wins_availability.mark_unavailable(commit_wins_transition))
        });
        loss_started_rx.await.expect("loss task starts");
        tokio::task::yield_now().await;
        assert!(
            !loss_task.is_finished(),
            "loss cannot linearize while the SQLite commit permit is held"
        );
        commit_wins_pause.release();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), loss_task)
                .await
                .expect("loss completes after commit")
                .expect("loss task joins")
                .expect("physical identity resolves")
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            availability.wait_for_transition_admissions(&commit_wins_loss),
        )
        .await
        .expect("commit-wins admission drains");
        assert!(
            engine
                .repositories()
                .get_command_receipt("commit-wins-turn".to_owned())
                .await
                .expect("commit-wins receipt lookup")
                .is_some(),
            "a command that owns finalization must commit before loss"
        );
        assert!(
            engine
                .repositories()
                .get_provider_turn_delivery("commit-wins-turn".to_owned())
                .await
                .expect("commit-wins outbox lookup")
                .is_some(),
            "the provider outbox commits in the same transaction"
        );

        availability
            .clear_recovered_in_repository(&thread_id, state.path(), "loss-repository")
            .await
            .expect("physical identity resolves");
        let rejected_loss_wins_pause = hooks.pause_before_next_command_finalization();
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": "10",
                    "tag": "orchestration.dispatchCommand",
                    "payload": {
                        "type": "thread.turn.start",
                        "commandId": "rejected-loss-wins-turn",
                        "threadId": thread_id,
                        "message": {
                            "messageId": "rejected-loss-wins-message",
                            "role": "user",
                            "text": "missing source plan must reject",
                            "attachments": []
                        },
                        "sourceProposedPlan": {
                            "threadId": thread_id,
                            "planId": "missing-plan"
                        },
                        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                        "createdAt": CREATED_AT
                    },
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send rejected loss-wins request");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rejected_loss_wins_pause.wait_until_entered(),
        )
        .await
        .expect("rejection reaches the SQLite pre-finalization barrier");
        let rejected_loss = WorkspaceLossTransition {
            thread_id: thread_id.clone(),
            repository_key: "loss-repository".to_owned(),
            generation: 5,
            path: state.path().to_path_buf(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        assert!(
            availability
                .mark_unavailable(rejected_loss.clone())
                .await
                .expect("physical identity resolves")
        );
        rejected_loss_wins_pause.release();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            availability.wait_for_transition_admissions(&rejected_loss),
        )
        .await
        .expect("rejected loss-wins admission drains");
        assert!(
            engine
                .repositories()
                .get_command_receipt("rejected-loss-wins-turn".to_owned())
                .await
                .expect("rejected loss-wins receipt lookup")
                .is_none(),
            "loss-before-finalization must roll back a rejected receipt"
        );

        availability
            .clear_recovered_in_repository(&thread_id, state.path(), "loss-repository")
            .await
            .expect("physical identity resolves");
        let rejected_commit_wins_pause = hooks.pause_after_next_command_finalization();
        socket
            .send(Message::Text(
                json!({
                    "_tag": "Request",
                    "id": "11",
                    "tag": "orchestration.dispatchCommand",
                    "payload": {
                        "type": "thread.turn.start",
                        "commandId": "rejected-commit-wins-turn",
                        "threadId": thread_id,
                        "message": {
                            "messageId": "rejected-commit-wins-message",
                            "role": "user",
                            "text": "rejection finalization wins before loss",
                            "attachments": []
                        },
                        "sourceProposedPlan": {
                            "threadId": thread_id,
                            "planId": "still-missing-plan"
                        },
                        "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                        "createdAt": CREATED_AT
                    },
                    "headers": []
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("send rejected commit-wins request");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rejected_commit_wins_pause.wait_until_entered(),
        )
        .await
        .expect("rejection owns finalization before SQLite commit");
        let rejected_commit_wins_loss = WorkspaceLossTransition {
            thread_id: thread_id.clone(),
            repository_key: "loss-repository".to_owned(),
            generation: 6,
            path: state.path().to_path_buf(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        let (rejected_loss_started_tx, rejected_loss_started_rx) = tokio::sync::oneshot::channel();
        let rejected_commit_wins_availability = availability.clone();
        let rejected_commit_wins_transition = rejected_commit_wins_loss.clone();
        let runtime = tokio::runtime::Handle::current();
        let rejected_loss_task = tokio::task::spawn_blocking(move || {
            let _ = rejected_loss_started_tx.send(());
            runtime.block_on(
                rejected_commit_wins_availability.mark_unavailable(rejected_commit_wins_transition),
            )
        });
        rejected_loss_started_rx
            .await
            .expect("rejected loss task starts");
        tokio::task::yield_now().await;
        assert!(
            !rejected_loss_task.is_finished(),
            "loss cannot linearize while rejected-receipt finalization is held"
        );
        rejected_commit_wins_pause.release();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), rejected_loss_task,)
                .await
                .expect("rejected loss completes after commit")
                .expect("rejected loss task joins")
                .expect("physical identity resolves")
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            availability.wait_for_transition_admissions(&rejected_commit_wins_loss),
        )
        .await
        .expect("rejected commit-wins admission drains");
        let rejected_receipt = engine
            .repositories()
            .get_command_receipt("rejected-commit-wins-turn".to_owned())
            .await
            .expect("rejected commit-wins receipt lookup")
            .expect("rejected receipt commits before loss");
        assert_eq!(rejected_receipt.status, "rejected");
        assert!(
            engine
                .repositories()
                .get_provider_turn_delivery("rejected-commit-wins-turn".to_owned())
                .await
                .expect("rejected commit-wins outbox lookup")
                .is_none(),
            "a rejected turn cannot create provider delivery"
        );

        socket.close(None).await.expect("close socket");
        handle.shutdown();
        handle.join().await.expect("server joins");
        delivery.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_resolution_command_id_conflicts_on_changed_payload() {
        let (_, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        seed_delivery(
            &engine,
            "delivery-first",
            &thread_id,
            "message-first",
            TurnDeliveryState::Uncertain,
        )
        .await;
        seed_delivery(
            &engine,
            "delivery-second",
            &thread_id,
            "message-second",
            TurnDeliveryState::Uncertain,
        )
        .await;
        let command_id = "resolve-delivery-once";
        dispatch_prepared_for_test(
            &engine,
            None,
            delivery_resolution(command_id, &thread_id, "message-first", "retry"),
        )
        .await
        .expect("first resolution commits");
        let event_count = engine
            .read_events(0)
            .await
            .expect("events before conflict")
            .len();

        let conflict = dispatch_prepared_for_test(
            &engine,
            None,
            delivery_resolution(command_id, &thread_id, "message-second", "dismiss"),
        )
        .await
        .expect_err("changed resolution payload must conflict");

        assert_eq!(conflict["_tag"], "OrchestrationDispatchCommandError");
        assert!(conflict["message"].as_str().is_some_and(|message| {
            message.contains(command_id) && message.to_ascii_lowercase().contains("conflict")
        }));
        assert_eq!(
            engine
                .read_events(0)
                .await
                .expect("events after conflict")
                .len(),
            event_count,
            "a conflicting replay cannot append a second transition"
        );
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("delivery-first".to_owned())
                .await
                .expect("first row")
                .expect("first delivery")
                .state,
            TurnDeliveryState::Pending
        );
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("delivery-second".to_owned())
                .await
                .expect("second row")
                .expect("second delivery")
                .state,
            TurnDeliveryState::Uncertain
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_resolution_receipts_keep_canonical_digest_when_accepted_or_rejected() {
        let (_database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        seed_delivery(
            &engine,
            "receipt-target",
            &thread_id,
            "receipt-target-message",
            TurnDeliveryState::Uncertain,
        )
        .await;

        let accepted = delivery_resolution(
            "accepted-resolution-receipt",
            &thread_id,
            "receipt-target-message",
            "retry",
        );
        let accepted_digest = canonical_command_digest(&accepted).expect("accepted digest");
        dispatch_prepared_for_test(&engine, None, accepted)
            .await
            .expect("resolution accepted");
        let accepted_receipt = engine
            .repositories()
            .get_command_receipt("accepted-resolution-receipt".to_owned())
            .await
            .expect("accepted receipt read")
            .expect("accepted receipt");
        assert_eq!(accepted_receipt.status, "accepted");
        assert_eq!(accepted_receipt.payload_digest, Some(accepted_digest));

        let rejected = delivery_resolution(
            "rejected-resolution-receipt",
            &thread_id,
            "missing-message",
            "dismiss",
        );
        let rejected_digest = canonical_command_digest(&rejected).expect("rejected digest");
        dispatch_prepared_for_test(&engine, None, rejected)
            .await
            .expect_err("resolution without a delivery is rejected");
        let rejected_receipt = engine
            .repositories()
            .get_command_receipt("rejected-resolution-receipt".to_owned())
            .await
            .expect("rejected receipt read")
            .expect("rejected receipt");
        assert_eq!(rejected_receipt.status, "rejected");
        assert_eq!(rejected_receipt.payload_digest, Some(rejected_digest));

        let conflict = dispatch_prepared_for_test(
            &engine,
            None,
            delivery_resolution(
                "rejected-resolution-receipt",
                &thread_id,
                "receipt-target-message",
                "retry",
            ),
        )
        .await
        .expect_err("changed replay of a rejected command conflicts");
        assert!(
            conflict["message"]
                .as_str()
                .is_some_and(|message| message.to_ascii_lowercase().contains("conflict"))
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn committed_resolution_wakes_idle_delivery_once_and_conflict_does_not_rewake() {
        let (database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        seed_delivery(
            &engine,
            "wake-target",
            &thread_id,
            "wake-target-message",
            TurnDeliveryState::Uncertain,
        )
        .await;
        let initial_read = engine
            .repositories()
            .pause_after_next_provider_turn_read_for_test();
        let (route_sender, mut routes) = mpsc::unbounded_channel();
        let route_release = Arc::new(tokio::sync::Notify::new());
        let router: DeliveryRouter = Arc::new({
            let route_release = route_release.clone();
            move |command| {
                let route_sender = route_sender.clone();
                let route_release = route_release.clone();
                Box::pin(async move {
                    let _ = route_sender.send(command);
                    route_release.notified().await;
                    Ok(())
                })
            }
        });
        let service = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            router,
        ));
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            initial_read.wait_until_entered(),
        )
        .await
        .expect("dispatcher captures uncertain idle snapshot");

        let state = tempfile::tempdir().expect("provider state");
        let (registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            service.clone(),
        );
        let resolution = delivery_resolution(
            "wake-resolution",
            &thread_id,
            "wake-target-message",
            "retry",
        );
        dispatch_prepared_for_test(&engine, Some(registration.clone()), resolution.clone())
            .await
            .expect("retry resolution commits");
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("wake-target".to_owned())
                .await
                .expect("target delivery")
                .expect("target row")
                .state,
            TurnDeliveryState::Pending,
            "retry must commit before the wake is observable"
        );
        initial_read.release();
        let routed = tokio::time::timeout(std::time::Duration::from_secs(5), routes.recv())
            .await
            .expect("committed retry wakes dispatcher")
            .expect("routed command");
        assert!(matches!(
            routed,
            OrchestrationCommand::ThreadTurnStart { ref command_id, .. }
                if command_id == "wake-target"
        ));
        let post_delivery_read = engine
            .repositories()
            .pause_after_next_provider_turn_read_for_test();
        route_release.notify_one();
        wait_for_delivery_state(&engine, "wake-target", TurnDeliveryState::Delivered).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            post_delivery_read.wait_until_entered(),
        )
        .await
        .expect("dispatcher captures empty post-delivery snapshot");

        seed_delivery(
            &engine,
            "must-stay-idle",
            &thread_id,
            "must-stay-idle-message",
            TurnDeliveryState::Pending,
        )
        .await;
        post_delivery_read.release();
        let event_count = engine
            .read_events(0)
            .await
            .expect("events before conflict")
            .len();
        let conflict = dispatch_prepared_for_test(
            &engine,
            Some(registration),
            delivery_resolution(
                "wake-resolution",
                &thread_id,
                "must-stay-idle-message",
                "dismiss",
            ),
        )
        .await;
        let unexpected_route =
            tokio::time::timeout(std::time::Duration::from_millis(250), routes.recv()).await;
        if unexpected_route.is_ok() {
            route_release.notify_one();
        }
        service.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");

        assert!(
            unexpected_route.is_err(),
            "a conflicting resolution must not wake or forward work to the provider router: {unexpected_route:?}"
        );
        let conflict = conflict.expect_err("changed resolution payload must conflict");
        assert!(
            conflict["message"]
                .as_str()
                .is_some_and(|message| message.to_ascii_lowercase().contains("conflict"))
        );
        assert_eq!(
            engine
                .read_events(0)
                .await
                .expect("events after conflict")
                .len(),
            event_count
        );
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("must-stay-idle".to_owned())
                .await
                .expect("idle delivery")
                .expect("idle row")
                .state,
            TurnDeliveryState::Pending
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn rolled_back_resolution_does_not_wake_idle_delivery() {
        let hooks = TestHooks::default();
        let (database, engine, thread_id) = delivery_engine(hooks.clone()).await;
        seed_delivery(
            &engine,
            "rollback-target",
            &thread_id,
            "rollback-target-message",
            TurnDeliveryState::Uncertain,
        )
        .await;
        let initial_read = engine
            .repositories()
            .pause_after_next_provider_turn_read_for_test();
        let router: DeliveryRouter = Arc::new(|_| Box::pin(async { Ok(()) }));
        let service = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            router,
        ));
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            initial_read.wait_until_entered(),
        )
        .await
        .expect("dispatcher captures uncertain idle snapshot");

        let state = tempfile::tempdir().expect("provider state");
        let (registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            service.clone(),
        );
        hooks.fail_next_projector(
            "projection.thread-messages",
            Some("thread.turn-delivery-updated"),
        );
        let result = dispatch_prepared_for_test(
            &engine,
            Some(registration),
            delivery_resolution(
                "rollback-resolution",
                &thread_id,
                "rollback-target-message",
                "retry",
            ),
        )
        .await;
        assert!(result.is_err(), "projector failure rejects the resolution");
        let post_failure_read = engine
            .repositories()
            .pause_after_next_provider_turn_read_for_test();
        initial_read.release();
        let unexpected_read = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            post_failure_read.wait_until_entered(),
        )
        .await;
        service.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");

        assert!(
            unexpected_read.is_err(),
            "a rolled-back resolution must not notify the idle dispatcher"
        );
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("rollback-target".to_owned())
                .await
                .expect("rollback delivery")
                .expect("rollback row")
                .state,
            TurnDeliveryState::Uncertain
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn attachment_preparation_sanitizes_before_dispatch_and_rejects_before_events() {
        let engine = migrated_engine().await;
        engine
            .dispatch(decode_command(json!({
                "type": "project.create", "commandId": "create-project", "projectId": "project-1",
                "title": "Project", "workspaceRoot": "C:/repo", "defaultModelSelection": null,
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        let thread_id = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        let state = tempfile::tempdir().expect("state directory");
        let attachments = AttachmentMaterializer::new(state.path().join("attachments"));
        let command = decode_command(json!({
            "type": "thread.turn.start", "commandId": "start-with-upload", "threadId": thread_id.clone(),
            "message": {"messageId":"message-1", "role":"user", "text":"review", "attachments":[{
                "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
                "sizeBytes":5, "dataUrl":"data:text/plain;base64,bm90ZXM="
            }]}, "createdAt": CREATED_AT,
        }));
        let (command, prepared) = prepare_attachments(
            &attachments,
            command,
            &ReusableAttachments::new(),
            &UploadOwner::Unauthenticated,
            &test_upload_registry(),
        )
        .await
        .expect("upload prepares");
        engine.dispatch(command).await.expect("turn dispatches");
        prepared.expect("attachment batch").commit();
        let snapshot = thread_snapshot(&engine, &thread_id)
            .await
            .expect("thread snapshot");
        let message = snapshot["thread"]["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .find(|message| message["id"] == "message-1")
            .expect("user message");
        assert_eq!(
            message["attachments"],
            json!([{
                "type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain", "sizeBytes":5
            }])
        );
        let event_count = engine.read_events(0).await.expect("events").len();
        let rejected = prepare_attachments(
            &attachments,
            decode_command(json!({
                "type": "thread.turn.start", "commandId": "reject-upload", "threadId": thread_id.clone(),
                "message": {"messageId":"message-2", "role":"user", "text":"review", "attachments":[{
                    "type":"file", "id":"notes-2", "name":"notes.txt", "mimeType":"text/plain",
                    "sizeBytes":5, "dataUrl":"data:text/plain,notes"
                }]}, "createdAt": CREATED_AT,
            })),
            &ReusableAttachments::new(), &UploadOwner::Unauthenticated, &test_upload_registry())
        .await
        .expect_err("malformed upload rejects before dispatch");
        assert_eq!(
            invalid_request("orchestration.dispatchCommand", rejected.to_string())["_tag"],
            "InvalidRequest"
        );
        assert_eq!(
            engine.read_events(0).await.expect("events").len(),
            event_count,
            "preparation failure cannot persist a message or turn"
        );
        engine.shutdown().await;
    }

    #[test]
    fn provider_failures_use_the_declared_dispatch_error_contract() {
        assert_eq!(
            provider_command_error("provider failed"),
            json!({
                "_tag": "OrchestrationDispatchCommandError",
                "message": "provider failed",
            })
        );
    }

    #[tokio::test]
    async fn empty_default_and_workspace_snapshots_match_the_thread_contract() {
        let engine = migrated_engine().await;
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "create-project",
                "projectId": "project-1",
                "title": "Project",
                "workspaceRoot": "C:/repo",
                "defaultModelSelection": null,
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");

        let projection = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot");
        let default_id = projection
            .threads
            .iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id
            .clone();
        let default_snapshot = thread_snapshot(&engine, &default_id)
            .await
            .expect("default snapshot");
        assert_empty_thread_contract(&default_snapshot["thread"], "default");

        engine
            .dispatch(decode_command(json!({
                "type": "thread.create",
                "commandId": "create-workspace",
                "threadId": "workspace-1",
                "projectId": "project-1",
                "title": "Workspace",
                "kind": "workspace",
                "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "branch": "feature",
                "worktreePath": "C:/repo-worktrees/feature",
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("workspace thread created");
        let workspace_snapshot = thread_snapshot(&engine, "workspace-1")
            .await
            .expect("workspace snapshot");
        assert_empty_thread_contract(&workspace_snapshot["thread"], "workspace");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn populated_thread_snapshot_uses_the_message_wire_contract() {
        let engine = migrated_engine().await;
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "create-project",
                "projectId": "project-1",
                "title": "Project",
                "workspaceRoot": "C:/repo",
                "defaultModelSelection": null,
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        let projection = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot");
        let default_id = projection
            .threads
            .iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id
            .clone();
        engine
            .dispatch(decode_command(json!({
                "type": "thread.turn.start",
                "commandId": "start-turn",
                "threadId": default_id,
                "message": {
                    "messageId": "message-1",
                    "role": "user",
                    "text": "hello",
                    "attachments": []
                },
                "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
                "runtimeMode": "full-access",
                "interactionMode": "default",
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("turn started");
        engine
            .dispatch(decode_command(json!({
                "type": "thread.activity.append",
                "commandId": "legacy-activity",
                "threadId": default_id,
                "activity": {
                    "id": "activity-1",
                    "tone": "status",
                    "kind": "provider.session",
                    "summary": "session.ready",
                    "payload": {},
                    "turnId": null,
                    "createdAt": CREATED_AT
                },
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("legacy activity stored");
        for command in [
            json!({"type":"thread.session.set","commandId":"session","threadId":default_id,"session":{"threadId":default_id,"status":"running","providerName":"codex","providerInstanceId":"codex","runtimeMode":"full-access","activeTurnId":"turn-1","lastError":null,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.message.assistant.delta","commandId":"assistant-delta","threadId":default_id,"messageId":"assistant-1","delta":"working","turnId":"turn-1","createdAt":CREATED_AT}),
            json!({"type":"thread.proposed-plan.upsert","commandId":"plan","threadId":default_id,"proposedPlan":{"id":"plan-1","turnId":"turn-1","planMarkdown":"# Plan","createdAt":CREATED_AT,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.turn.diff.complete","commandId":"checkpoint","threadId":default_id,"turnId":"turn-1","completedAt":CREATED_AT,"checkpointRef":"checkpoint-1","status":"ready","files":[],"assistantMessageId":"assistant-1","checkpointTurnCount":1,"createdAt":CREATED_AT}),
        ] {
            engine
                .dispatch(decode_command(command))
                .await
                .expect("populated snapshot fixture command");
        }

        let mut persisted = engine
            .repositories()
            .get_message("message-1".to_owned())
            .await
            .expect("message lookup")
            .expect("message exists");
        persisted.delivery_state = Some("uncertain".to_owned());
        persisted.delivery_provider = Some("claudeAgent".to_owned());
        persisted.delivery_detail = Some("connection lost after write".to_owned());
        engine
            .repositories()
            .upsert_message(persisted)
            .await
            .expect("delivery projection stored");

        let snapshot = thread_snapshot(&engine, &default_id)
            .await
            .expect("thread snapshot");
        let message = snapshot["thread"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "user")
            .unwrap();
        assert_eq!(message["streaming"], json!(false));
        assert!(message.get("isStreaming").is_none());
        assert_eq!(
            message["delivery"],
            json!({
                "state": "uncertain",
                "provider": "claudeAgent",
                "detail": "connection lost after write",
            })
        );
        assert_eq!(snapshot["thread"]["activities"][0]["tone"], json!("info"));
        assert_eq!(
            snapshot["thread"]["proposedPlans"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            snapshot["thread"]["checkpoints"].as_array().unwrap().len(),
            1
        );
        assert_eq!(snapshot["thread"]["session"]["status"], "running");
        assert_eq!(snapshot["thread"]["latestTurn"]["turnId"], "turn-1");
        for provider_instance_id in [Some("claude-personal"), None] {
            let mut message = engine
                .repositories()
                .get_message("message-1".to_owned())
                .await
                .expect("message lookup")
                .expect("message");
            message.delivery_provider_instance_id = provider_instance_id.map(str::to_owned);
            engine
                .repositories()
                .upsert_message(message)
                .await
                .expect("store delivery instance");
            let snapshot = thread_snapshot(&engine, &default_id)
                .await
                .expect("snapshot with delivery instance");
            let message = snapshot["thread"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["id"] == "message-1")
                .unwrap();
            assert_eq!(
                message["delivery"].get("providerInstanceId"),
                provider_instance_id.map(|value| json!(value)).as_ref()
            );
        }
        for (state, reason) in [
            ("failed", Some("modelSelectionRefused")),
            ("delivered", Some("startedNewConversation")),
            ("delivered", None),
        ] {
            let mut message = engine
                .repositories()
                .get_message("message-1".to_owned())
                .await
                .expect("message lookup")
                .expect("message");
            message.delivery_state = Some(state.to_owned());
            message.delivery_reason = reason.map(str::to_owned);
            engine
                .repositories()
                .upsert_message(message)
                .await
                .expect("store reason");
            let snapshot = thread_snapshot(&engine, &default_id)
                .await
                .expect("snapshot with reason");
            let message = snapshot["thread"]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["id"] == "message-1")
                .unwrap();
            assert_eq!(
                message["delivery"].get("reason"),
                reason.map(|value| json!(value)).as_ref()
            );
        }
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn query_and_stream_boundaries_cover_registered_orchestration_adapters() {
        let engine = migrated_engine().await;
        let mut registry = RpcRegistry::empty();
        register_orchestration_rpc(&mut registry, engine.clone());
        assert!(
            registry
                .validate_complete()
                .expect_err("focused registry is incomplete")
                .contains("server.getConfig")
        );

        assert!(
            handle_query(&engine, request("orchestration.unknown", json!({})))
                .await
                .is_err()
        );
        assert!(
            handle_query(
                &engine,
                request("orchestration.getTurnDiff", json!({"threadId":"t1"})),
            )
            .await
            .is_err()
        );
        assert!(
            diff(&engine, "t1".to_owned(), 2, 1).await.is_err(),
            "reversed turn bounds must fail"
        );
        assert_eq!(
            handle_query(
                &engine,
                request(
                    "orchestration.getFullThreadDiff",
                    json!({"threadId":"t1","toTurnCount":0}),
                ),
            )
            .await
            .unwrap()["diff"],
            json!("")
        );

        let cancellation = CancellationToken::new();
        let mut shell = shell_stream(engine.clone(), cancellation.clone());
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), shell.recv())
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
        cancellation.cancel();

        let mut malformed = thread_stream(
            engine.clone(),
            request("orchestration.subscribeThread", json!({})),
            CancellationToken::new(),
        );
        assert!(malformed.recv().await.unwrap().is_err());

        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        assert!(send_snapshot(&sender, Ok(json!({}))).await.is_err());
        engine.shutdown().await;
    }

    async fn created_project_thread(engine: &OrchestrationEngine) -> String {
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "project",
                "projectId": "project",
                "title": "Project",
                "workspaceRoot": "C:/repo",
                "defaultModelSelection": null,
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        load_snapshot(&engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id
    }

    async fn rename(engine: &OrchestrationEngine, thread_id: &str, index: usize) {
        engine
            .dispatch(decode_command(json!({
                "type": "thread.meta.update",
                "commandId": format!("rename-{index}"),
                "threadId": thread_id,
                "title": format!("Title {index}"),
            })))
            .await
            .expect("thread renamed");
    }

    async fn next_items(stream: &mut mpsc::Receiver<RpcStreamChunk>) -> Vec<Value> {
        tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("a stream chunk")
            .expect("an open stream")
            .expect("a successful chunk")
    }

    /// A client that applies patches gets only what changed: re-sending the whole
    /// thread per update saturated slow links (#89). Older clients keep snapshots.
    #[tokio::test]
    async fn thread_stream_sends_patches_to_clients_that_apply_them() {
        let engine = migrated_engine().await;
        let thread_id = created_project_thread(&engine).await;
        let cancellation = CancellationToken::new();
        let subscribe = |patches: bool| {
            thread_stream(
                engine.clone(),
                request(
                    "orchestration.subscribeThread",
                    json!({ "threadId": thread_id, "patches": patches }),
                ),
                cancellation.clone(),
            )
        };
        let mut patched = subscribe(true);
        let mut legacy = subscribe(false);
        assert_eq!(next_items(&mut patched).await[0]["kind"], "snapshot");
        assert_eq!(next_items(&mut legacy).await[0]["kind"], "snapshot");

        rename(&engine, &thread_id, 1).await;
        let items = next_items(&mut patched).await;
        assert_eq!(items[0]["kind"], "patch", "{items:?}");
        assert_eq!(items[0]["patch"]["set"]["title"], "Title 1");
        assert!(items[0]["patch"]["set"].get("messages").is_none());
        assert_eq!(next_items(&mut legacy).await[0]["kind"], "snapshot");
        cancellation.cancel();
        engine.shutdown().await;
    }

    /// The thread stream's wire contract: a client that applies these patches to
    /// the first snapshot must reach the server's final snapshot. The TypeScript
    /// side decodes and applies this fixture (`threadStreamParity.test.ts`).
    /// Regenerate with `BIBCODE_UPDATE_THREAD_STREAM_FIXTURE=1`.
    #[tokio::test]
    async fn thread_stream_lifecycle_fixture_matches_the_wire() {
        const AT: &str = "2026-07-10T10:00:00.000Z";
        let engine = migrated_engine().await;
        for command in [
            json!({"type":"project.create","commandId":"c01","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","defaultModelSelection":null,"createdAt":AT}),
            json!({"type":"thread.create","commandId":"c02","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","branch":null,"worktreePath":null,"createdAt":AT}),
        ] {
            engine
                .dispatch(decode_command(command))
                .await
                .expect("setup");
        }
        let initial = thread_snapshot(&engine, "t1")
            .await
            .expect("initial snapshot");
        let mut held = initial.clone();
        let mut patches = Vec::new();
        let at = |second: u32| format!("2026-07-10T10:01:{second:02}.000Z");
        for command in [
            json!({"type":"thread.meta.update","commandId":"c03","threadId":"t1","title":"Renamed","branch":"main","worktreePath":null}),
            json!({"type":"thread.runtime-mode.set","commandId":"c04","threadId":"t1","runtimeMode":"approval-required","createdAt":at(1)}),
            json!({"type":"thread.interaction-mode.set","commandId":"c05","threadId":"t1","interactionMode":"plan","createdAt":at(2)}),
            json!({"type":"thread.turn.start","commandId":"c06","threadId":"t1","message":{"messageId":"m-user","role":"user","text":"hello","attachments":[]},"createdAt":at(3)}),
            json!({"type":"thread.session.set","commandId":"c07","threadId":"t1","session":{"threadId":"t1","status":"running","providerName":"codex","providerInstanceId":"codex","runtimeMode":"approval-required","activeTurnId":"turn-1","lastError":null,"updatedAt":at(4)},"createdAt":at(4)}),
            json!({"type":"thread.activity.append","commandId":"c08","threadId":"t1","activity":{"id":"a1","tone":"tool","kind":"command","summary":"ran","payload":{"step":1},"turnId":"turn-1","createdAt":at(5)},"createdAt":at(5)}),
            json!({"type":"thread.message.assistant.delta","commandId":"c09","threadId":"t1","messageId":"m-assistant","delta":"hel","turnId":"turn-1","createdAt":at(6)}),
            json!({"type":"thread.message.assistant.delta","commandId":"c10","threadId":"t1","messageId":"m-assistant","delta":"lo","turnId":"turn-1","createdAt":at(7)}),
            json!({"type":"thread.activity.append","commandId":"c11","threadId":"t1","activity":{"id":"a2","tone":"info","kind":"status","summary":"working","payload":{"step":2},"turnId":"turn-1","createdAt":at(8)},"createdAt":at(8)}),
            json!({"type":"thread.message.assistant.complete","commandId":"c12","threadId":"t1","messageId":"m-assistant","turnId":"turn-1","createdAt":at(9)}),
            json!({"type":"thread.proposed-plan.upsert","commandId":"c13","threadId":"t1","proposedPlan":{"id":"plan-1","turnId":"turn-1","planMarkdown":"Do it","createdAt":at(10),"updatedAt":at(10)},"createdAt":at(10)}),
            json!({"type":"thread.turn.diff.complete","commandId":"c14","threadId":"t1","turnId":"turn-1","completedAt":at(11),"checkpointRef":"ref-1","status":"ready","files":[{"path":"a.rs","kind":"modified","additions":2,"deletions":1}],"assistantMessageId":"m-assistant","checkpointTurnCount":1,"createdAt":at(11)}),
            json!({"type":"thread.turn.start","commandId":"c15","threadId":"t1","message":{"messageId":"m-queued","role":"user","text":"next","attachments":[]},"runtimeMode":"approval-required","interactionMode":"plan","queued":true,"createdAt":at(12)}),
            json!({"type":"thread.turn.steer","commandId":"c16","threadId":"t1","messageId":"m-queued","createdAt":at(13)}),
            json!({"type":"thread.session.set","commandId":"c17","threadId":"t1","session":{"threadId":"t1","status":"ready","providerName":"codex","providerInstanceId":"codex","runtimeMode":"approval-required","activeTurnId":null,"lastError":null,"updatedAt":at(14)},"createdAt":at(14)}),
            json!({"type":"thread.archive","commandId":"c18","threadId":"t1"}),
            json!({"type":"thread.unarchive","commandId":"c19","threadId":"t1"}),
            json!({"type":"thread.session.stop","commandId":"c20","threadId":"t1","createdAt":at(15)}),
        ] {
            // A command the thread's state refuses still exercises the stream.
            let _ = engine.dispatch(decode_command(command)).await;
            let next = thread_snapshot(&engine, "t1").await.expect("snapshot");
            if let Some(patch) = thread_patch(
                &HeldThread::of(&held, std::hash::RandomState::new()).expect("a thread"),
                &next,
            ) {
                patches.push(patch);
            }
            held = next;
        }
        let fixture = stable_ids_and_clock_times(
            json!({ "initial": initial, "patches": patches, "final": held }),
            AT,
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/contracts/fixtures/thread-stream/lifecycle.json");
        if std::env::var_os("BIBCODE_UPDATE_THREAD_STREAM_FIXTURE").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&fixture).unwrap() + "\n",
            )
            .unwrap();
        }
        let committed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("committed fixture"))
                .expect("fixture JSON");
        assert_eq!(fixture, committed, "regenerate the thread stream fixture");
        engine.shutdown().await;
    }

    /// Event ids and times the engine takes from the clock (rather than from a
    /// command) differ per run; they become stable placeholders in first-seen order,
    /// so references between them and their order survive.
    fn stable_ids_and_clock_times(fixture: Value, day_of_commands: &str) -> Value {
        fn collect(value: &Value, day: &str, seen: &mut Vec<String>) {
            match value {
                Value::String(text) => {
                    let event_id = uuid::Uuid::parse_str(text).is_ok();
                    // An ISO-8601 instant such as 2026-10-13T12:00:00.000Z.
                    let bytes = text.as_bytes();
                    let clock_time = bytes.len() >= 20
                        && bytes[4] == b'-'
                        && bytes[10] == b'T'
                        && text.ends_with('Z')
                        && bytes[..4].iter().all(u8::is_ascii_digit)
                        && !text.starts_with(&day[..10]);
                    if (event_id || clock_time) && !seen.contains(text) {
                        seen.push(text.clone());
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| collect(item, day, seen)),
                Value::Object(map) => map.values().for_each(|item| collect(item, day, seen)),
                _ => {}
            }
        }
        fn replace(value: Value, stable: &HashMap<String, String>) -> Value {
            match value {
                Value::String(text) => Value::String(stable.get(&text).cloned().unwrap_or(text)),
                Value::Array(items) => Value::Array(
                    items
                        .into_iter()
                        .map(|item| replace(item, stable))
                        .collect(),
                ),
                Value::Object(map) => Value::Object(
                    map.into_iter()
                        .map(|(key, item)| (key, replace(item, stable)))
                        .collect(),
                ),
                other => other,
            }
        }
        let mut seen = Vec::new();
        collect(&fixture, day_of_commands, &mut seen);
        let stable = seen
            .into_iter()
            .enumerate()
            .map(|(index, original)| {
                let placeholder = if uuid::Uuid::parse_str(&original).is_ok() {
                    format!("event-{index:02}")
                } else {
                    format!("2026-07-11T00:00:{index:02}.000Z")
                };
                (original, placeholder)
            })
            .collect::<HashMap<_, _>>();
        replace(fixture, &stable)
    }

    /// A burst of events yields one shell snapshot, not one per event.
    #[tokio::test]
    async fn shell_stream_coalesces_event_bursts() {
        let engine = migrated_engine().await;
        let thread_id = created_project_thread(&engine).await;
        let cancellation = CancellationToken::new();
        let mut stream = shell_stream(engine.clone(), cancellation.clone());
        assert_eq!(next_items(&mut stream).await[0]["kind"], "snapshot");

        for index in 0..10 {
            rename(&engine, &thread_id, index).await;
        }
        let latest = next_items(&mut stream).await;
        assert_eq!(latest[0]["kind"], "snapshot");
        tokio::time::sleep(SHELL_SNAPSHOT_COALESCE * 3).await;
        let mut snapshots = 1;
        while let Ok(Some(_)) = stream.try_recv().map(Some) {
            snapshots += 1;
        }
        assert!(snapshots <= 2, "{snapshots} shell snapshots for one burst");
        cancellation.cancel();
        engine.shutdown().await;
    }

    /// Creates a thread and deletes it: a view that still shows it keeps subscribing.
    async fn deleted_thread(engine: &OrchestrationEngine) -> &'static str {
        created_project_thread(engine).await;
        for command in [
            json!({"type":"thread.create","commandId":"create","threadId":"gone","projectId":"project","title":"Gone","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","branch":null,"worktreePath":null,"createdAt":CREATED_AT}),
            json!({"type":"thread.delete","commandId":"delete","threadId":"gone"}),
        ] {
            engine
                .dispatch(decode_command(command))
                .await
                .expect("setup");
        }
        "gone"
    }

    /// The producer ends with its error instead of waiting for a cancellation or for an
    /// event of its thread, which a deleted thread never gets (#113).
    #[tokio::test]
    async fn thread_stream_ends_after_its_error() {
        let engine = migrated_engine().await;
        let thread_id = deleted_thread(&engine).await;
        let mut stream = thread_stream(
            engine.clone(),
            request(
                "orchestration.subscribeThread",
                json!({ "threadId": thread_id, "patches": true }),
            ),
            CancellationToken::new(),
        );
        assert!(stream.recv().await.expect("an item").is_err());
        assert!(
            tokio::time::timeout(Duration::from_secs(1), stream.recv())
                .await
                .expect("the stream ends")
                .is_none()
        );
        assert_eq!(engine.event_subscriber_count_for_test(), 0);
        engine.shutdown().await;
    }

    /// A failed subscription must end with its request (#113). A client retried a deleted
    /// thread every 250 ms; each failed attempt left a task holding an engine event receiver,
    /// so every later event was cloned into tens of thousands of receivers until restart.
    #[tokio::test]
    async fn failed_thread_subscriptions_release_their_event_receivers() {
        const ATTEMPTS: usize = 20;
        let engine = migrated_engine().await;
        let thread_id = deleted_thread(&engine).await;
        let state = tempfile::tempdir().expect("state directory");
        let mut registry = RpcRegistry::empty();
        register_orchestration_rpc(&mut registry, engine.clone());
        let handle = ServerRuntime::start_with_registry(
            ServerConfig::new(state.path())
                .with_bind("127.0.0.1", 0)
                .with_unsafe_no_auth(),
            registry,
        )
        .await
        .expect("server starts");
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{}/ws", handle.local_addr()))
                .await
                .expect("socket connects");
        let baseline = engine.event_subscriber_count_for_test();

        for attempt in 1..=ATTEMPTS {
            let id = attempt.to_string();
            let request = json!({
                "_tag": "Request",
                "id": id,
                "tag": "orchestration.subscribeThread",
                "payload": { "threadId": thread_id, "patches": true },
                "headers": [],
            });
            socket
                .send(Message::Text(request.to_string().into()))
                .await
                .expect("send subscription");
            let frame = tokio::time::timeout(
                Duration::from_secs(5),
                next_frame_past_heartbeat(&mut socket),
            )
            .await
            .expect("subscription answers")
            .expect("socket open")
            .expect("frame");
            let Message::Text(text) = frame else {
                panic!("expected a text frame, got {frame:?}");
            };
            assert!(
                matches!(
                    serde_json::from_str::<ServerMessage>(&text).expect("server message"),
                    ServerMessage::Exit { request_id, exit: RpcExit::Failure { .. } }
                        if request_id == RequestId::try_from(id.as_str()).unwrap()
                ),
                "a deleted thread's subscription fails: {text}"
            );
        }

        // The connection stays open, as the client's does between retries.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while engine.event_subscriber_count_for_test() > baseline
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let leaked = engine.event_subscriber_count_for_test() - baseline;
        assert_eq!(
            leaked, 0,
            "{leaked} of {ATTEMPTS} failed subscriptions still hold an event receiver"
        );
        drop(socket);
        handle.shutdown();
        handle.join().await.expect("server joins");
        engine.shutdown().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn thread_stream_subscribes_before_returning_to_concurrent_dispatchers() {
        let engine = migrated_engine().await;
        engine
            .dispatch(decode_command(json!({
                "type": "project.create",
                "commandId": "project",
                "projectId": "project",
                "title": "Project",
                "workspaceRoot": "C:/repo",
                "defaultModelSelection": null,
                "createdAt": CREATED_AT,
            })))
            .await
            .expect("project created");
        let thread_id = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot")
            .threads
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        let cancellation = CancellationToken::new();

        let stream = thread_stream(
            engine.clone(),
            request(
                "orchestration.subscribeThread",
                json!({ "threadId": thread_id }),
            ),
            cancellation.clone(),
        );

        assert_eq!(
            engine.event_subscriber_count_for_test(),
            1,
            "the event receiver must exist before a concurrent post-snapshot update can publish",
        );
        cancellation.cancel();
        drop(stream);
        engine.shutdown().await;
    }
    use crate::transfer::staging::{
        UploadAppendInput, UploadBeginInput, UploadErrorReason, UploadLimits, UploadTarget,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::time::Duration;
    async fn stage_notes(uploads: &UploadRegistry, owner: &UploadOwner, complete: bool) -> Value {
        let begun = uploads
            .begin(
                owner,
                UploadBeginInput {
                    target: UploadTarget::ChatAttachment {
                        attachment_type: "file".into(),
                        name: "notes.txt".into(),
                        mime_type: "text/plain".into(),
                    },
                    size_bytes: 5,
                    sha256: None,
                },
            )
            .await
            .unwrap();
        if complete {
            uploads
                .append(
                    owner,
                    UploadAppendInput {
                        upload_id: begun.upload_id.clone(),
                        offset: 0,
                        data: STANDARD.encode(b"notes"),
                        sha256: Some(crate::crypto::sha256_hex(b"notes")),
                    },
                )
                .await
                .unwrap();
        }
        json!({"type":"file", "id":"notes-1", "name":"notes.txt", "mimeType":"text/plain",
        "sizeBytes":5, "uploadId":begun.upload_id})
    }
    #[tokio::test]
    async fn failed_staged_turn_retries_the_same_command_without_appending_again() {
        let hooks = TestHooks::default();
        let (database, engine, thread_id) = delivery_engine(hooks.clone()).await;
        let state = tempfile::tempdir().unwrap();
        let uploads = UploadRegistry::new(
            state.path().join("attachment-uploads"),
            UploadLimits::default(),
            Arc::new(tokio::time::Instant::now),
        );
        let owner = UploadOwner::Session("sender".into());
        let input = stage_notes(&uploads, &owner, true).await;
        let upload_id = input["uploadId"].as_str().unwrap().to_owned();
        let (sent, mut received) = tokio::sync::mpsc::channel(1);
        let root = state.path().join("attachments");
        let delivery = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            Arc::new(move |command| {
                let sent = sent.clone();
                let root = root.clone();
                Box::pin(async move {
                    let OrchestrationCommand::ThreadTurnStart { message, .. } = command else {
                        panic!("expected turn start");
                    };
                    let images = AttachmentMaterializer::new(root)
                        .materialize(message.attachments)
                        .await
                        .unwrap();
                    sent.send(images[0].base64_data.clone()).await.unwrap();
                    Ok(())
                })
            }),
        ));
        let (mut registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            delivery.clone(),
        );
        registration.uploads = uploads.clone();
        let command = decode_command(
            json!({"type":"thread.turn.start", "commandId":"staged-retry",
        "threadId":thread_id, "message":{"messageId":"m-staged", "role":"user", "text":"review",
        "attachments":[input]}, "modelSelection":{"instanceId":"codex","model":"gpt-5"},
        "createdAt":CREATED_AT}),
        );
        let digest = canonical_command_digest(&command).unwrap();
        hooks.fail_next_projector("projection.thread-messages", Some("thread.message-sent"));
        let failed = dispatch_turn_for_test(
            engine.clone(),
            registration.clone(),
            command.clone(),
            digest.clone(),
            "orchestration.dispatchCommand".into(),
            None,
            &owner,
        )
        .await;
        assert!(failed.is_err());
        assert!(uploads.get(&owner, &upload_id).await.unwrap().complete);
        assert!(!state.path().join("attachments/notes-1").exists());
        assert!(received.try_recv().is_err());
        dispatch_turn_for_test(
            engine.clone(),
            registration,
            command,
            digest,
            "orchestration.dispatchCommand".into(),
            None,
            &owner,
        )
        .await
        .unwrap();
        let encoded = tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(STANDARD.decode(encoded).unwrap(), b"notes");
        assert_eq!(
            uploads.get(&owner, &upload_id).await.unwrap_err().reason,
            UploadErrorReason::NotFound
        );
        delivery.shutdown().await;
        provider.shutdown().await.unwrap();
        engine.shutdown().await;
    }
    #[tokio::test]
    async fn queued_staged_turn_binds_before_enqueue_and_keeps_only_durable_metadata() {
        let (database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        engine
            .dispatch(decode_command(
                json!({"type":"thread.session.set", "commandId":"session",
        "threadId":thread_id, "session":{"threadId":thread_id,"status":"running",
        "providerName":"codex","activeTurnId":null,"lastError":null,"updatedAt":CREATED_AT},
        "createdAt":CREATED_AT}),
            ))
            .await
            .unwrap();
        let state = tempfile::tempdir().unwrap();
        let uploads = UploadRegistry::new(
            state.path().join("attachment-uploads"),
            UploadLimits::default(),
            Arc::new(tokio::time::Instant::now),
        );
        let owner = UploadOwner::Session("sender".into());
        let input = stage_notes(&uploads, &owner, true).await;
        let id = input["uploadId"].as_str().unwrap().to_owned();
        let service = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        ));
        service.shutdown().await; // inspect durable admission before any delivery worker
        let (mut registration, provider) =
            provider_registration(database, &engine, state.path().to_path_buf(), service);
        registration.uploads = uploads.clone();
        let command = decode_command(
            json!({"type":"thread.turn.start","commandId":"queued-stage",
        "threadId":thread_id,"queued":true,"message":{"messageId":"queued-stage-message",
        "role":"user","text":"later","attachments":[input]},"createdAt":CREATED_AT}),
        );
        let digest = canonical_command_digest(&command).unwrap();
        dispatch_turn_for_test(
            engine.clone(),
            registration,
            command,
            digest,
            "orchestration.dispatchCommand".into(),
            None,
            &owner,
        )
        .await
        .unwrap();
        let row = engine
            .repositories()
            .get_provider_turn_delivery("queued-stage".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, TurnDeliveryState::Queued);
        let attachments = row.payload["message"]["attachments"].as_array().unwrap();
        assert!(attachments[0].get("uploadId").is_none());
        assert!(attachments[0].get("dataUrl").is_none());
        assert_eq!(
            uploads.get(&owner, &id).await.unwrap_err().reason,
            UploadErrorReason::NotFound
        );
        let ready = AttachmentMaterializer::new(state.path().join("attachments"))
            .materialize(attachments.clone())
            .await
            .unwrap();
        assert_eq!(STANDARD.decode(&ready[0].base64_data).unwrap(), b"notes");
        provider.shutdown().await.unwrap();
        engine.shutdown().await;
    }
    #[tokio::test]
    async fn vanished_staged_dispatch_returns_typed_not_found_and_releases_the_command_claim() {
        let (database, engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let state = tempfile::tempdir().unwrap();
        let uploads = UploadRegistry::new(
            state.path().join("attachment-uploads"),
            UploadLimits::default(),
            Arc::new(tokio::time::Instant::now),
        );
        let owner = UploadOwner::Session("sender".into());
        let input = stage_notes(&uploads, &owner, true).await;
        uploads
            .cancel(&owner, input["uploadId"].as_str().unwrap())
            .await
            .unwrap();
        let service = Arc::new(TurnDeliveryService::start_with_router(
            engine.clone(),
            1,
            Arc::new(|_| Box::pin(async { Ok(()) })),
        ));
        let (mut registration, provider) = provider_registration(
            database,
            &engine,
            state.path().to_path_buf(),
            service.clone(),
        );
        registration.uploads = uploads;
        let command = decode_command(
            json!({"type":"thread.turn.start","commandId":"missing-stage","threadId":thread_id,"message":{"messageId":"missing-stage-message","role":"user","text":"review","attachments":[input]},"modelSelection":{"instanceId":"codex","model":"gpt-5"},"createdAt":CREATED_AT}),
        );
        let digest = canonical_command_digest(&command).unwrap();
        let error = dispatch_turn_for_test(
            engine.clone(),
            registration,
            command,
            digest,
            "orchestration.dispatchCommand".into(),
            None,
            &owner,
        )
        .await
        .unwrap_err();
        assert_eq!(error["_tag"], "UploadError");
        assert_eq!(error["reason"], "not_found");
        assert!(
            engine
                .repositories()
                .get_command_receipt("missing-stage".into())
                .await
                .unwrap()
                .is_none()
        );
        service.shutdown().await;
        provider.shutdown().await.unwrap();
        engine.shutdown().await;
    }
}
